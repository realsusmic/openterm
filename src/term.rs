//! term — VT parser (C) → TermState → Grid (Zig) → egui.
//!
//! Rendering is one text galley per row with color runs (not one draw call per
//! cell), and only rows that exist get laid out. egui caches identical galleys
//! across frames, so a static screen is basically free.

use crate::native::{
    Cell, Grid, ATTR_BLINK, ATTR_BOLD, ATTR_DIM, ATTR_HIDDEN, ATTR_ITALIC, ATTR_REVERSE,
    ATTR_STRIKE, ATTR_UNDERLINE, ATTR_UNDERLINE_STYLE_MASK, ATTR_UNDERLINE_STYLE_SHIFT, DEF_BG,
    DEF_FG,
};
use crate::theme;
use egui::{
    text::{LayoutJob, TextFormat},
    Color32, Event, EventFilter, Id, Key, Modifiers, Pos2, Rect, Rounding, Sense, Stroke, Vec2,
};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

const SCROLLBACK: usize = 5000;

type CbPrint = extern "C" fn(*mut c_void, u32);
type CbExec = extern "C" fn(*mut c_void, u8);
type CbCsi = extern "C" fn(*mut c_void, u8, *const u32, u8, *const u8, *const u8, u8);
type CbEsc = extern "C" fn(*mut c_void, u8, *const u8, u8);
type CbOsc = extern "C" fn(*mut c_void, *const u8, u32);
type CbDcs =
    extern "C" fn(*mut c_void, u8, *const u32, u8, *const u8, *const u8, u8, *const u8, u32);

extern "C" {
    fn vt_sizeof() -> usize;
    fn vt_init(p: *mut c_void);
    fn vt_set_callbacks(
        p: *mut c_void,
        a: CbPrint,
        b: CbExec,
        c: CbCsi,
        d: CbEsc,
        e: CbOsc,
        f: CbDcs,
        ud: *mut c_void,
    );
    fn vt_feed(p: *mut c_void, data: *const u8, len: usize);
}

// ───────────────────────── state machine semantics ─────────────────────────

#[derive(Clone)]
struct GridRow {
    cells: Vec<Cell>,
    links: Vec<u32>,
}

pub struct DcsSequence {
    pub final_byte: u8,
    pub params: Vec<u32>,
    pub subparams: Vec<u8>,
    pub intermediates: Vec<u8>,
    pub data: Vec<u8>,
}

struct TermState {
    grid: Grid,
    alt: Option<(Grid, u32, u32)>, // saved primary screen while in alt screen
    cx: u32,
    cy: u32,
    saved: (u32, u32),
    fg: u16,
    bg: u16,
    attrs: u16,
    link_id: u32,
    links: Vec<String>,
    link_ids: HashMap<String, u32>,
    top: u32,
    bot: u32, // exclusive
    wrap_pending: bool,
    cursor_visible: bool,
    app_cursor: bool,
    responses: Vec<u8>,
    scrollback: VecDeque<GridRow>,
    title: String,
    dcs: VecDeque<DcsSequence>,
}

impl TermState {
    fn new(cols: u32, rows: u32) -> Self {
        Self {
            grid: Grid::new(cols, rows),
            alt: None,
            cx: 0,
            cy: 0,
            saved: (0, 0),
            fg: DEF_FG,
            bg: DEF_BG,
            attrs: 0,
            link_id: 0,
            links: vec![String::new()],
            link_ids: HashMap::new(),
            top: 0,
            bot: rows,
            wrap_pending: false,
            cursor_visible: true,
            app_cursor: false,
            responses: Vec::new(),
            scrollback: VecDeque::new(),
            title: String::new(),
            dcs: VecDeque::new(),
        }
    }

    fn push_scrollback(&mut self, y: u32) {
        if self.alt.is_some() {
            return;
        }
        let (cells, links) = self.grid.row_with_links(y);
        self.scrollback.push_back(GridRow { cells, links });
        if self.scrollback.len() > SCROLLBACK {
            self.scrollback.pop_front();
        }
    }

    fn resize(&mut self, cols: u32, rows: u32) {
        let shift = if self.cy >= rows {
            self.cy - rows + 1
        } else {
            0
        };
        for y in 0..shift.min(self.grid.rows) {
            self.push_scrollback(y);
        }
        self.grid.resize(cols, rows, shift);
        if let Some((g, _, _)) = &mut self.alt {
            g.resize(cols, rows, 0);
        }
        self.cy -= shift;
        self.cx = self.cx.min(cols.saturating_sub(1));
        self.cy = self.cy.min(rows.saturating_sub(1));
        self.top = 0;
        self.bot = rows;
        self.wrap_pending = false;
    }

    fn linefeed(&mut self) {
        if self.cy + 1 == self.bot {
            if self.top == 0 {
                self.push_scrollback(0);
            }
            self.grid.scroll_up(self.top, self.bot, 1);
        } else if self.cy + 1 < self.grid.rows {
            self.cy += 1;
        }
        self.wrap_pending = false;
    }

    fn put(&mut self, ch: u32) {
        if self.wrap_pending {
            self.cx = 0;
            self.linefeed();
        }
        let mut width = if is_wide(ch) { 2 } else { 1 };
        if width == 2 && self.grid.cols < 2 {
            width = 1;
        }
        if width == 2 && self.cx + 1 >= self.grid.cols {
            self.cx = 0;
            self.linefeed();
        }

        self.clear_wide_occupant(self.cx, self.cy);
        if width == 2 {
            self.clear_wide_occupant(self.cx + 1, self.cy);
        }
        let cell = Cell {
            glyph: ch,
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs,
            width,
            _pad: 0,
        };
        self.grid
            .set_with_link(self.cx, self.cy, cell, self.link_id);
        if width == 2 {
            self.grid.set_with_link(
                self.cx + 1,
                self.cy,
                Cell {
                    glyph: 0,
                    width: 0,
                    ..cell
                },
                self.link_id,
            );
        }
        if self.cx + width as u32 >= self.grid.cols {
            self.cx = self.grid.cols.saturating_sub(width as u32);
            self.wrap_pending = true;
        } else {
            self.cx += width as u32;
        }
    }

    fn clear_wide_occupant(&mut self, x: u32, y: u32) {
        let Some(cell) = self.grid.get(x, y) else {
            return;
        };
        if cell.width == 0 && x > 0 {
            self.grid.set(x - 1, y, Cell::default());
        } else if cell.width == 2 && x + 1 < self.grid.cols {
            self.grid.set(x + 1, y, Cell::default());
        }
    }

    fn exec(&mut self, c: u8) {
        match c {
            0x08 => {
                self.cx = self.cx.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => {
                self.cx = (((self.cx / 8) + 1) * 8).min(self.grid.cols.saturating_sub(1));
            }
            0x0A | 0x0B | 0x0C => self.linefeed(),
            0x0D => {
                self.cx = 0;
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn dec_mode(&mut self, m: u32, on: bool) {
        match m {
            1 => self.app_cursor = on,
            25 => self.cursor_visible = on,
            47 | 1047 | 1049 => {
                if on && self.alt.is_none() {
                    let (c, r) = (self.grid.cols, self.grid.rows);
                    let primary = std::mem::replace(&mut self.grid, Grid::new(c, r));
                    self.alt = Some((primary, self.cx, self.cy));
                } else if !on {
                    if let Some((g, x, y)) = self.alt.take() {
                        let (c, r) = (self.grid.cols, self.grid.rows);
                        self.grid = g;
                        self.grid.resize(c, r, 0);
                        if m == 1049 {
                            self.cx = x.min(c - 1);
                            self.cy = y.min(r - 1);
                        }
                    }
                }
                self.top = 0;
                self.bot = self.grid.rows;
            }
            _ => {}
        }
    }

    fn csi(&mut self, f: u8, params: &[u32], subparams: &[u8], inter: &[u8]) {
        match inter.first() {
            Some(b'?') => {
                if f == b'h' || f == b'l' {
                    for &m in params {
                        self.dec_mode(m, f == b'h');
                    }
                }
                return;
            }
            Some(b'>') => {
                if f == b'c' {
                    self.responses.extend_from_slice(b"\x1b[>0;10;1c");
                }
                return;
            }
            Some(_) => return, // DECSCUSR, DECSTR, etc. — ignored
            None => {}
        }
        let p = |i: usize, d: u32| params.get(i).copied().filter(|v| *v != 0).unwrap_or(d);
        let raw0 = params.first().copied().unwrap_or(0);
        let (cols, rows) = (self.grid.cols, self.grid.rows);
        let maxx = cols.saturating_sub(1);
        let maxy = rows.saturating_sub(1);
        match f {
            b'A' => self.cy = self.cy.saturating_sub(p(0, 1)),
            b'B' | b'e' => self.cy = (self.cy + p(0, 1)).min(maxy),
            b'C' | b'a' => self.cx = (self.cx + p(0, 1)).min(maxx),
            b'D' => self.cx = self.cx.saturating_sub(p(0, 1)),
            b'E' => {
                self.cy = (self.cy + p(0, 1)).min(maxy);
                self.cx = 0;
            }
            b'F' => {
                self.cy = self.cy.saturating_sub(p(0, 1));
                self.cx = 0;
            }
            b'G' | b'`' => self.cx = (p(0, 1) - 1).min(maxx),
            b'd' => self.cy = (p(0, 1) - 1).min(maxy),
            b'H' | b'f' => {
                self.cy = (p(0, 1) - 1).min(maxy);
                self.cx = (p(1, 1) - 1).min(maxx);
            }
            b'J' => match raw0 {
                0 => {
                    self.grid.clear_rect(self.cx, self.cy, cols - self.cx, 1);
                    self.grid.clear_rect(0, self.cy + 1, cols, rows);
                }
                1 => {
                    self.grid.clear_rect(0, 0, cols, self.cy);
                    self.grid.clear_rect(0, self.cy, self.cx + 1, 1);
                }
                3 => self.scrollback.clear(),
                _ => self.grid.clear_rect(0, 0, cols, rows),
            },
            b'K' => match raw0 {
                0 => self.grid.clear_rect(self.cx, self.cy, cols - self.cx, 1),
                1 => self.grid.clear_rect(0, self.cy, self.cx + 1, 1),
                _ => self.grid.clear_rect(0, self.cy, cols, 1),
            },
            b'X' => self.grid.clear_rect(self.cx, self.cy, p(0, 1), 1),
            b'L' => {
                if self.cy >= self.top && self.cy < self.bot {
                    self.grid.scroll_down(self.cy, self.bot, p(0, 1));
                }
            }
            b'M' => {
                if self.cy >= self.top && self.cy < self.bot {
                    self.grid.scroll_up(self.cy, self.bot, p(0, 1));
                }
            }
            b'S' => {
                let n = p(0, 1);
                if self.top == 0 {
                    for y in 0..n.min(rows) {
                        self.push_scrollback(y);
                    }
                }
                self.grid.scroll_up(self.top, self.bot, n);
            }
            b'T' => self.grid.scroll_down(self.top, self.bot, p(0, 1)),
            b'P' => self.grid.delete_cells(self.cx, self.cy, p(0, 1)),
            b'@' => self.grid.insert_cells(self.cx, self.cy, p(0, 1)),
            b'r' => {
                let t = p(0, 1) - 1;
                let b = params
                    .get(1)
                    .copied()
                    .filter(|v| *v != 0)
                    .unwrap_or(rows)
                    .min(rows);
                if t < b {
                    self.top = t;
                    self.bot = b;
                } else {
                    self.top = 0;
                    self.bot = rows;
                }
                self.cx = 0;
                self.cy = 0;
            }
            b's' => self.saved = (self.cx, self.cy),
            b'u' => {
                self.cx = self.saved.0.min(maxx);
                self.cy = self.saved.1.min(maxy);
            }
            b'm' => self.sgr(params, subparams),
            // device status / attributes — ConPTY blocks on 6n until we answer
            b'n' => match raw0 {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => self
                    .responses
                    .extend_from_slice(format!("\x1b[{};{}R", self.cy + 1, self.cx + 1).as_bytes()),
                _ => {}
            },
            b'c' => {
                if raw0 == 0 {
                    self.responses.extend_from_slice(b"\x1b[?1;2c");
                }
            }
            _ => {}
        }
        if f != b'm' && f != b'n' && f != b'c' {
            self.wrap_pending = false;
        }
    }

    fn sgr(&mut self, params: &[u32], subparams: &[u8]) {
        let p: &[u32] = if params.is_empty() { &[0] } else { params };
        let mut i = 0;
        while i < p.len() {
            match p[i] {
                0 => {
                    self.fg = DEF_FG;
                    self.bg = DEF_BG;
                    self.attrs = 0;
                }
                1 => self.attrs |= ATTR_BOLD,
                2 => self.attrs |= ATTR_DIM,
                3 => self.attrs |= ATTR_ITALIC,
                4 => {
                    let style = if subparams.get(i + 1).copied() == Some(1) {
                        i += 1;
                        p[i].min(3) as u16
                    } else {
                        1
                    };
                    self.attrs &= !ATTR_UNDERLINE_STYLE_MASK;
                    if style == 0 {
                        self.attrs &= !ATTR_UNDERLINE;
                    } else {
                        self.attrs |= ATTR_UNDERLINE;
                        self.attrs |= style << ATTR_UNDERLINE_STYLE_SHIFT;
                    }
                }
                5 | 6 => self.attrs |= ATTR_BLINK,
                7 => self.attrs |= ATTR_REVERSE,
                8 => self.attrs |= ATTR_HIDDEN,
                9 => self.attrs |= ATTR_STRIKE,
                21 => {
                    self.attrs |= ATTR_UNDERLINE;
                    self.attrs &= !ATTR_UNDERLINE_STYLE_MASK;
                    self.attrs |= 2 << ATTR_UNDERLINE_STYLE_SHIFT;
                }
                22 => self.attrs &= !(ATTR_BOLD | ATTR_DIM),
                23 => self.attrs &= !ATTR_ITALIC,
                24 => self.attrs &= !(ATTR_UNDERLINE | ATTR_UNDERLINE_STYLE_MASK),
                25 => self.attrs &= !ATTR_BLINK,
                27 => self.attrs &= !ATTR_REVERSE,
                28 => self.attrs &= !ATTR_HIDDEN,
                29 => self.attrs &= !ATTR_STRIKE,
                30..=37 => self.fg = (p[i] - 30) as u16,
                39 => self.fg = DEF_FG,
                40..=47 => self.bg = (p[i] - 40) as u16,
                49 => self.bg = DEF_BG,
                90..=97 => self.fg = (p[i] - 90 + 8) as u16,
                100..=107 => self.bg = (p[i] - 100 + 8) as u16,
                38 | 48 => {
                    let is_fg = p[i] == 38;
                    let colon = subparams.get(i + 1).copied() == Some(1);
                    let (color, consumed) = parse_sgr_color(p, subparams, i, colon);
                    i += consumed;
                    if let Some(c) = color {
                        if is_fg {
                            self.fg = c
                        } else {
                            self.bg = c
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn esc(&mut self, f: u8, inter: &[u8]) {
        if !inter.is_empty() {
            return;
        } // charset designations etc.
        match f {
            b'c' => {
                let (c, r) = (self.grid.cols, self.grid.rows);
                *self = TermState::new(c, r);
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.cx = 0;
                self.linefeed();
            }
            b'M' => {
                if self.cy == self.top {
                    self.grid.scroll_down(self.top, self.bot, 1);
                } else {
                    self.cy = self.cy.saturating_sub(1);
                }
            }
            b'7' => self.saved = (self.cx, self.cy),
            b'8' => {
                self.cx = self.saved.0.min(self.grid.cols - 1);
                self.cy = self.saved.1.min(self.grid.rows - 1);
            }
            _ => {}
        }
    }

    fn osc(&mut self, data: &[u8]) {
        let s = String::from_utf8_lossy(data);
        if let Some(rest) = s.strip_prefix("0;").or_else(|| s.strip_prefix("2;")) {
            self.title = rest.to_string();
        } else if let Some(rest) = s.strip_prefix("8;") {
            let uri = rest.split_once(';').map(|(_, uri)| uri).unwrap_or("");
            self.link_id = if uri.is_empty() {
                0
            } else {
                self.intern_link(uri)
            };
        }
    }

    fn intern_link(&mut self, uri: &str) -> u32 {
        if let Some(id) = self.link_ids.get(uri) {
            return *id;
        }
        let Ok(id) = u32::try_from(self.links.len()) else {
            return 0;
        };
        let uri = uri.to_owned();
        self.links.push(uri.clone());
        self.link_ids.insert(uri, id);
        id
    }

    fn dcs(
        &mut self,
        final_byte: u8,
        params: &[u32],
        subparams: &[u8],
        intermediates: &[u8],
        data: &[u8],
    ) {
        // Keep bounded payloads available on the Rust side. Protocol-specific
        // renderers (sixel/DECRQSS/etc.) can consume them through `take_dcs`.
        if self.dcs.len() == 64 {
            self.dcs.pop_front();
        }
        self.dcs.push_back(DcsSequence {
            final_byte,
            params: params.to_vec(),
            subparams: subparams.to_vec(),
            intermediates: intermediates.to_vec(),
            data: data.to_vec(),
        });
    }
}

fn parse_sgr_color(
    params: &[u32],
    subparams: &[u8],
    start: usize,
    colon: bool,
) -> (Option<u16>, usize) {
    let Some(&mode) = params.get(start + 1) else {
        return (None, 0);
    };
    if !colon {
        return match mode {
            5 => (
                Some(params.get(start + 2).copied().unwrap_or(0).min(255) as u16),
                2,
            ),
            2 => {
                let get = |offset| params.get(start + offset).copied().unwrap_or(0).min(255) as u8;
                (Some(rgb_to_256(get(2), get(3), get(4))), 4)
            }
            _ => (None, 0),
        };
    }

    let end = (start + 1..params.len())
        .find(|&index| subparams.get(index).copied().unwrap_or(0) == 0)
        .unwrap_or(params.len());
    match mode {
        5 => (
            Some(params.get(start + 2).copied().unwrap_or(0).min(255) as u16),
            end.saturating_sub(start + 1),
        ),
        2 => {
            // ITU-T form includes an optional color-space slot:
            // 38:2:<colorspace>:r:g:b. Accept the common abbreviated form too.
            let first_rgb = if end.saturating_sub(start) >= 6 {
                start + 3
            } else {
                start + 2
            };
            let get = |index| params.get(index).copied().unwrap_or(0).min(255) as u8;
            (
                Some(rgb_to_256(
                    get(first_rgb),
                    get(first_rgb + 1),
                    get(first_rgb + 2),
                )),
                end.saturating_sub(start + 1),
            )
        }
        _ => (None, end.saturating_sub(start + 1)),
    }
}

fn is_wide(cp: u32) -> bool {
    matches!(
        cp,
        0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1f64f
            | 0x1f900..=0x1f9ff
            | 0x20000..=0x3fffd
    )
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u16 {
    // grayscale ramp if it's a gray, otherwise the 6x6x6 cube
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((r as u16 - 8) * 24 / 247);
    }
    let q = |v: u8| {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u16
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

// ─────────────────────────── C trampolines ───────────────────────────

fn st<'a>(ud: *mut c_void) -> &'a mut TermState {
    unsafe { &mut *(ud as *mut TermState) }
}

extern "C" fn cb_print(ud: *mut c_void, cp: u32) {
    st(ud).put(cp)
}
extern "C" fn cb_exec(ud: *mut c_void, c: u8) {
    st(ud).exec(c)
}
extern "C" fn cb_csi(
    ud: *mut c_void,
    f: u8,
    p: *const u32,
    np: u8,
    sub: *const u8,
    i: *const u8,
    ni: u8,
) {
    let params = unsafe { std::slice::from_raw_parts(p, np as usize) };
    let subparams = unsafe { std::slice::from_raw_parts(sub, np as usize) };
    let inter = unsafe { std::slice::from_raw_parts(i, ni as usize) };
    st(ud).csi(f, params, subparams, inter)
}
extern "C" fn cb_esc(ud: *mut c_void, f: u8, i: *const u8, ni: u8) {
    let inter = unsafe { std::slice::from_raw_parts(i, ni as usize) };
    st(ud).esc(f, inter)
}
extern "C" fn cb_osc(ud: *mut c_void, d: *const u8, len: u32) {
    let data = unsafe { std::slice::from_raw_parts(d, len as usize) };
    st(ud).osc(data)
}
extern "C" fn cb_dcs(
    ud: *mut c_void,
    final_byte: u8,
    p: *const u32,
    np: u8,
    sub: *const u8,
    i: *const u8,
    ni: u8,
    d: *const u8,
    len: u32,
) {
    let params = unsafe { std::slice::from_raw_parts(p, np as usize) };
    let subparams = unsafe { std::slice::from_raw_parts(sub, np as usize) };
    let intermediates = unsafe { std::slice::from_raw_parts(i, ni as usize) };
    let data = unsafe { std::slice::from_raw_parts(d, len as usize) };
    st(ud).dcs(final_byte, params, subparams, intermediates, data)
}

// ─────────────────────────── public view ───────────────────────────

struct Inner {
    state: Box<TermState>, // boxed: its address is the C userdata pointer
    parser: Vec<u64>,      // u64 backing = correctly aligned for the C struct
    input: Vec<u8>,
    size: (u16, u16),
    resized: Option<(u16, u16)>,
    view_offset: usize, // lines scrolled back into history
}

pub struct TermView {
    id: Id,
    inner: Mutex<Inner>,
}

// Safety: the raw TermState pointer is only dereferenced inside vt_feed, which
// is only ever called while holding `inner`'s lock.
unsafe impl Send for TermView {}
unsafe impl Sync for TermView {}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl TermView {
    pub fn new() -> Self {
        let (cols, rows) = (100u16, 30u16);
        let mut state = Box::new(TermState::new(cols as u32, rows as u32));
        let words = (unsafe { vt_sizeof() } + 7) / 8;
        let mut parser = vec![0u64; words];
        unsafe {
            let pp = parser.as_mut_ptr() as *mut c_void;
            vt_init(pp);
            let ud = state.as_mut() as *mut TermState as *mut c_void;
            vt_set_callbacks(pp, cb_print, cb_exec, cb_csi, cb_esc, cb_osc, cb_dcs, ud);
        }
        Self {
            id: Id::new(("term", NEXT_ID.fetch_add(1, Ordering::Relaxed))),
            inner: Mutex::new(Inner {
                state,
                parser,
                input: Vec::new(),
                size: (cols, rows),
                resized: None,
                view_offset: 0,
            }),
        }
    }

    pub fn feed(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut g = self.inner.lock();
        let pp = g.parser.as_mut_ptr() as *mut c_void;
        unsafe { vt_feed(pp, data.as_ptr(), data.len()) };
        let resp = std::mem::take(&mut g.state.responses);
        g.input.extend_from_slice(&resp);
    }

    pub fn take_input(&self) -> Vec<u8> {
        std::mem::take(&mut self.inner.lock().input)
    }
    pub fn take_resize(&self) -> Option<(u16, u16)> {
        self.inner.lock().resized.take()
    }
    pub fn size(&self) -> (u16, u16) {
        self.inner.lock().size
    }
    pub fn title(&self) -> String {
        self.inner.lock().state.title.clone()
    }
    pub fn take_dcs(&self) -> Option<DcsSequence> {
        self.inner.lock().state.dcs.pop_front()
    }

    /// draw + handle input. `interactive = false` (e.g. a modal is open) makes the
    /// terminal give up keyboard focus and ignore keys.
    pub fn ui(&self, ui: &mut egui::Ui, rect: Rect, interactive: bool) {
        let font = theme::mono(theme::MONO_SIZE);
        let cw = ui.fonts(|f| f.glyph_width(&font, 'M'));
        let ch = ui.fonts(|f| f.row_height(&font)).ceil();
        let cols = ((rect.width() / cw).floor() as u16).max(10);
        let rows = ((rect.height() / ch).floor() as u16).max(3);

        let resp = ui.interact(rect, self.id, Sense::click());
        if interactive {
            if resp.clicked() {
                resp.request_focus();
            }
            if ui.memory(|m| m.focused().is_none()) {
                resp.request_focus();
            }
        } else if resp.has_focus() {
            resp.surrender_focus();
        }
        let focused = interactive && resp.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        let mut g = self.inner.lock();
        if g.size != (cols, rows) {
            g.size = (cols, rows);
            g.resized = Some((cols, rows));
            g.state.resize(cols as u32, rows as u32);
        }

        // ---- input ----
        if focused {
            let app_cursor = g.state.app_cursor;
            let mut out = Vec::new();
            ui.input(|i| {
                for ev in &i.events {
                    match ev {
                        Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                        Event::Paste(s) => out.extend_from_slice(
                            s.replace("\r\n", "\r").replace('\n', "\r").as_bytes(),
                        ),
                        // egui eats ctrl+c / ctrl+x as clipboard events — send them to the shell
                        Event::Copy if !i.modifiers.mac_cmd => out.push(0x03),
                        Event::Cut if !i.modifiers.mac_cmd => out.push(0x18),
                        Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => {
                            if let Some(seq) = key_bytes(*key, *modifiers, app_cursor) {
                                out.extend_from_slice(&seq);
                            }
                        }
                        _ => {}
                    }
                }
            });
            if !out.is_empty() {
                g.view_offset = 0;
                g.input.extend_from_slice(&out);
            }
        }
        if resp.hovered() {
            let dy = ui.input(|i| i.raw_scroll_delta.y);
            if dy != 0.0 {
                let lines = (dy / ch).round() as isize;
                let max = g.state.scrollback.len() as isize;
                g.view_offset = (g.view_offset as isize + lines).clamp(0, max) as usize;
            }
        }

        // ---- paint ----
        let painter = ui.painter_at(rect);
        let st = &g.state;
        let sb = st.scrollback.len();
        let off = g.view_offset.min(sb);
        for r in 0..rows as usize {
            let logical = sb - off + r;
            let (line, links): (&[Cell], &[u32]) = if logical < sb {
                let row = &st.scrollback[logical];
                (&row.cells, &row.links)
            } else {
                let y = (logical - sb) as u32;
                if y >= st.grid.rows {
                    continue;
                }
                (st.grid.row_slice(y), st.grid.row_links(y))
            };
            paint_row(
                &painter,
                ui,
                line,
                links,
                rect.min + Vec2::new(0.0, r as f32 * ch),
                cw,
                ch,
                &font,
            );
        }

        if let Some(pos) = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| rect.contains(*p))
        {
            let col = ((pos.x - rect.left()) / cw).floor() as usize;
            let row = ((pos.y - rect.top()) / ch).floor() as usize;
            let logical = sb - off + row;
            let link_id = if logical < sb {
                st.scrollback
                    .get(logical)
                    .and_then(|line| line.links.get(col))
                    .copied()
            } else {
                let y = logical - sb;
                (y < st.grid.rows as usize)
                    .then(|| st.grid.row_links(y as u32).get(col).copied())
                    .flatten()
            }
            .unwrap_or(0);
            if link_id != 0 {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    if let Some(url) = st.links.get(link_id as usize) {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
                    }
                }
            }
        }

        if off == 0 && st.cursor_visible {
            let cursor_line = st.grid.row_slice(st.cy);
            // The text galley may accumulate sub-pixel glyph advances slightly
            // differently from `cx * glyph_width` (especially at fractional
            // display scaling). Anchor the cursor to the laid-out prefix so it
            // cannot drift left and cover a previously typed character.
            let cursor_x = visual_prefix_width(ui, cursor_line, st.cx as usize, &font);
            let cr = Rect::from_min_size(
                rect.min + Vec2::new(cursor_x, st.cy as f32 * ch),
                Vec2::new(cw, ch),
            );
            if focused {
                painter.rect_filled(cr, Rounding::same(1.0), theme::fg());
                let glyph = cursor_line
                    .get(st.cx as usize)
                    .map(|c| c.glyph)
                    .unwrap_or(32);
                if let Some(c) = char::from_u32(glyph).filter(|c| !c.is_whitespace()) {
                    painter.text(cr.min, egui::Align2::LEFT_TOP, c, font.clone(), theme::bg());
                }
            } else {
                painter.rect_stroke(
                    cr.shrink(0.5),
                    Rounding::same(1.0),
                    Stroke::new(1.0_f32, theme::fg_muted()),
                );
            }
        }
        if off > 0 {
            let txt = format!("{off} lines up");
            let gal = painter.layout_no_wrap(txt, theme::sans(11.0), theme::fg_dim());
            let pill = Rect::from_min_size(
                Pos2::new(rect.right() - gal.size().x - 22.0, rect.top() + 4.0),
                gal.size() + Vec2::new(16.0, 8.0),
            );
            painter.rect(
                pill,
                Rounding::same(10.0),
                theme::bg4(),
                Stroke::new(1.0_f32, theme::border_d()),
            );
            painter.galley(pill.min + Vec2::new(8.0, 4.0), gal, theme::fg_dim());
        }
    }
}

fn visual_prefix_width(ui: &egui::Ui, line: &[Cell], cells: usize, font: &egui::FontId) -> f32 {
    let text: String = line
        .iter()
        .take(cells)
        .filter(|cell| cell.width != 0)
        .map(|cell| {
            char::from_u32(cell.glyph)
                .filter(|c| *c != '\0')
                .unwrap_or(' ')
        })
        .collect();
    ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(text, font.clone(), Color32::WHITE)
            .size()
            .x
    })
}

fn paint_row(
    painter: &egui::Painter,
    ui: &egui::Ui,
    line: &[Cell],
    links: &[u32],
    origin: Pos2,
    cw: f32,
    ch: f32,
    font: &egui::FontId,
) {
    // trim trailing default blanks so empty rows cost nothing
    let end = line
        .iter()
        .enumerate()
        .rposition(|(index, c)| {
            !(c.bg == DEF_BG
                && c.attrs == 0
                && links.get(index).copied().unwrap_or(0) == 0
                && (c.glyph == 32 || c.glyph == 0))
        })
        .map(|i| i + 1)
        .unwrap_or(0);
    if end == 0 {
        return;
    }
    let line = &line[..end];

    // backgrounds: merged runs
    let mut x = 0;
    while x < line.len() {
        let bg = effective_colors(&line[x]).1;
        let start = x;
        while x < line.len() && effective_colors(&line[x]).1 == bg {
            x += 1;
        }
        if bg != DEF_BG {
            painter.rect_filled(
                Rect::from_min_size(
                    origin + Vec2::new(start as f32 * cw, 0.0),
                    Vec2::new((x - start) as f32 * cw, ch),
                ),
                Rounding::ZERO,
                palette(bg),
            );
        }
    }

    // text: one galley per row, one section per fg run
    let mut job = LayoutJob::default();
    let mut x = 0;
    let mut buf = String::new();
    while x < line.len() {
        let cell = line[x];
        let fg = effective_colors(&cell).0;
        let attrs = cell.attrs;
        let hidden = attrs & ATTR_HIDDEN != 0
            || (attrs & ATTR_BLINK != 0 && ui.input(|i| (i.time * 2.0) as u64 % 2 != 0));
        if attrs & ATTR_BLINK != 0 {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        let linked = links.get(x).copied().unwrap_or(0) != 0;
        buf.clear();
        while x < line.len() {
            let current = line[x];
            let current_fg = effective_colors(&current).0;
            let current_hidden = current.attrs & ATTR_HIDDEN != 0
                || (current.attrs & ATTR_BLINK != 0
                    && ui.input(|i| (i.time * 2.0) as u64 % 2 != 0));
            let current_linked = links.get(x).copied().unwrap_or(0) != 0;
            if current_fg != fg
                || current.attrs != attrs
                || current_hidden != hidden
                || current_linked != linked
            {
                break;
            }
            if current.width != 0 {
                let c = char::from_u32(current.glyph)
                    .filter(|c| *c != '\0')
                    .unwrap_or(' ');
                buf.push(c);
            }
            x += 1;
        }
        let mut color = palette(fg);
        if attrs & ATTR_DIM != 0 {
            color = color.gamma_multiply(0.6);
        }
        if attrs & ATTR_BOLD != 0 && fg < 8 {
            color = palette(fg + 8);
        }
        if hidden {
            color = Color32::TRANSPARENT;
        }
        let underline_style = (attrs & ATTR_UNDERLINE_STYLE_MASK) >> ATTR_UNDERLINE_STYLE_SHIFT;
        job.append(
            &buf,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                italics: attrs & ATTR_ITALIC != 0,
                underline: if (attrs & ATTR_UNDERLINE != 0 || linked) && underline_style <= 1 {
                    Stroke::new(1.0_f32, color)
                } else {
                    Stroke::NONE
                },
                strikethrough: if attrs & ATTR_STRIKE != 0 {
                    Stroke::new(1.0_f32, color)
                } else {
                    Stroke::NONE
                },
                ..Default::default()
            },
        );
    }
    let galley = ui.fonts(|f| f.layout_job(job));
    painter.galley(origin, galley, theme::fg());

    // Egui has a native single underline only. Draw double and curly variants
    // against cell coordinates so their style remains stable for wide glyphs.
    paint_styled_underlines(painter, line, origin, cw, ch);
}

fn effective_colors(cell: &Cell) -> (u16, u16) {
    if cell.attrs & ATTR_REVERSE != 0 {
        (cell.bg, cell.fg)
    } else {
        (cell.fg, cell.bg)
    }
}

fn paint_styled_underlines(painter: &egui::Painter, line: &[Cell], origin: Pos2, cw: f32, ch: f32) {
    let mut x = 0;
    while x < line.len() {
        let style = (line[x].attrs & ATTR_UNDERLINE_STYLE_MASK) >> ATTR_UNDERLINE_STYLE_SHIFT;
        if line[x].attrs & ATTR_UNDERLINE == 0 || style < 2 {
            x += 1;
            continue;
        }
        let start = x;
        while x < line.len()
            && line[x].attrs & ATTR_UNDERLINE != 0
            && (line[x].attrs & ATTR_UNDERLINE_STYLE_MASK) >> ATTR_UNDERLINE_STYLE_SHIFT == style
        {
            x += 1;
        }
        let color = palette(effective_colors(&line[start]).0);
        let x0 = origin.x + start as f32 * cw;
        let x1 = origin.x + x as f32 * cw;
        let y = origin.y + ch - 2.0;
        if style == 2 {
            painter.line_segment(
                [Pos2::new(x0, y - 2.0), Pos2::new(x1, y - 2.0)],
                Stroke::new(1.0_f32, color),
            );
            painter.line_segment(
                [Pos2::new(x0, y), Pos2::new(x1, y)],
                Stroke::new(1.0_f32, color),
            );
        } else {
            let mut points = Vec::new();
            let mut px = x0;
            let mut up = false;
            while px < x1 {
                points.push(Pos2::new(px, y + if up { -1.5 } else { 0.0 }));
                px += 2.0;
                up = !up;
            }
            points.push(Pos2::new(x1, y + if up { -1.5 } else { 0.0 }));
            painter.add(egui::Shape::line(points, Stroke::new(1.0_f32, color)));
        }
    }
}

pub fn palette(i: u16) -> Color32 {
    const BASE: [Color32; 16] = [
        Color32::from_rgb(0x2a, 0x2c, 0x30),
        Color32::from_rgb(0xd6, 0x5c, 0x5c),
        Color32::from_rgb(0x8c, 0xc2, 0x5a),
        Color32::from_rgb(0xc9, 0xa8, 0x5e),
        Color32::from_rgb(0x7c, 0x9c, 0xc4),
        Color32::from_rgb(0xb0, 0x8c, 0xc4),
        Color32::from_rgb(0x7a, 0xb3, 0xa8),
        Color32::from_rgb(0xc8, 0xcb, 0xd0),
        Color32::from_rgb(0x5c, 0x61, 0x68),
        Color32::from_rgb(0xe8, 0x78, 0x78),
        Color32::from_rgb(0xa6, 0xd9, 0x72),
        Color32::from_rgb(0xe0, 0xc0, 0x7c),
        Color32::from_rgb(0x9a, 0xb8, 0xd8),
        Color32::from_rgb(0xc8, 0xa8, 0xd8),
        Color32::from_rgb(0x98, 0xc8, 0xc0),
        Color32::from_rgb(0xf2, 0xf3, 0xf5),
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let v = i - 16;
            let s = |x: u16| if x == 0 { 0u8 } else { (55 + x * 40) as u8 };
            Color32::from_rgb(s(v / 36), s((v / 6) % 6), s(v % 6))
        }
        232..=255 => {
            let v = (8 + (i - 232) * 10) as u8;
            Color32::from_rgb(v, v, v)
        }
        DEF_BG => theme::bg(),
        _ => theme::fg(),
    }
}

fn key_bytes(k: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use Key::*;
    if m.ctrl && !m.alt {
        let letter = match k {
            A => 1,
            B => 2,
            C => 3,
            D => 4,
            E => 5,
            F => 6,
            G => 7,
            H => 8,
            I => 9,
            J => 10,
            K => 11,
            L => 12,
            M => 13,
            N => 14,
            O => 15,
            P => 16,
            Q => 17,
            R => 18,
            S => 19,
            T => 20,
            U => 21,
            V => 22,
            W => 23,
            X => 24,
            Y => 25,
            Z => 26,
            _ => 0,
        };
        if letter != 0 {
            return Some(vec![letter]);
        }
    }
    let arrow = |c: char| {
        Some(
            if app_cursor {
                format!("\x1bO{c}")
            } else {
                format!("\x1b[{c}")
            }
            .into_bytes(),
        )
    };
    let csi = |s: &str| Some(format!("\x1b[{s}").into_bytes());
    match k {
        Enter => Some(b"\r".to_vec()),
        Tab => Some(if m.shift {
            b"\x1b[Z".to_vec()
        } else {
            b"\t".to_vec()
        }),
        Backspace => Some(if m.ctrl {
            b"\x17".to_vec()
        } else {
            b"\x7f".to_vec()
        }),
        Escape => Some(b"\x1b".to_vec()),
        ArrowUp => arrow('A'),
        ArrowDown => arrow('B'),
        ArrowRight => {
            if m.ctrl {
                csi("1;5C")
            } else {
                arrow('C')
            }
        }
        ArrowLeft => {
            if m.ctrl {
                csi("1;5D")
            } else {
                arrow('D')
            }
        }
        Home => arrow('H'),
        End => arrow('F'),
        PageUp => csi("5~"),
        PageDown => csi("6~"),
        Insert => csi("2~"),
        Delete => csi("3~"),
        F1 => Some(b"\x1bOP".to_vec()),
        F2 => Some(b"\x1bOQ".to_vec()),
        F3 => Some(b"\x1bOR".to_vec()),
        F4 => Some(b"\x1bOS".to_vec()),
        F5 => csi("15~"),
        F6 => csi("17~"),
        F7 => csi("18~"),
        F8 => csi("19~"),
        F9 => csi("20~"),
        F10 => csi("21~"),
        F11 => csi("23~"),
        F12 => csi("24~"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ParserEvents {
        printed: Vec<u32>,
        csi: Vec<(u8, Vec<u32>, Vec<u8>)>,
        osc: Vec<Vec<u8>>,
        dcs: Vec<(u8, Vec<u32>, Vec<u8>, Vec<u8>)>,
    }

    extern "C" fn test_print(ud: *mut c_void, codepoint: u32) {
        let events = unsafe { &mut *(ud as *mut ParserEvents) };
        events.printed.push(codepoint);
    }
    extern "C" fn test_exec(_: *mut c_void, _: u8) {}
    extern "C" fn test_esc(_: *mut c_void, _: u8, _: *const u8, _: u8) {}
    extern "C" fn test_csi(
        ud: *mut c_void,
        final_byte: u8,
        params: *const u32,
        n_params: u8,
        subparams: *const u8,
        _: *const u8,
        _: u8,
    ) {
        let events = unsafe { &mut *(ud as *mut ParserEvents) };
        let params = unsafe { std::slice::from_raw_parts(params, n_params as usize) };
        let subparams = unsafe { std::slice::from_raw_parts(subparams, n_params as usize) };
        events
            .csi
            .push((final_byte, params.to_vec(), subparams.to_vec()));
    }
    extern "C" fn test_osc(ud: *mut c_void, data: *const u8, len: u32) {
        let events = unsafe { &mut *(ud as *mut ParserEvents) };
        let data = unsafe { std::slice::from_raw_parts(data, len as usize) };
        events.osc.push(data.to_vec());
    }
    extern "C" fn test_dcs(
        ud: *mut c_void,
        final_byte: u8,
        params: *const u32,
        n_params: u8,
        _: *const u8,
        intermediates: *const u8,
        n_intermediates: u8,
        data: *const u8,
        len: u32,
    ) {
        let events = unsafe { &mut *(ud as *mut ParserEvents) };
        let params = unsafe { std::slice::from_raw_parts(params, n_params as usize) };
        let intermediates =
            unsafe { std::slice::from_raw_parts(intermediates, n_intermediates as usize) };
        let data = unsafe { std::slice::from_raw_parts(data, len as usize) };
        events.dcs.push((
            final_byte,
            params.to_vec(),
            intermediates.to_vec(),
            data.to_vec(),
        ));
    }

    fn parser() -> (Vec<u64>, Box<ParserEvents>) {
        let words = (unsafe { vt_sizeof() } + 7) / 8;
        let mut storage = vec![0u64; words];
        let mut events = Box::<ParserEvents>::default();
        unsafe {
            let parser = storage.as_mut_ptr() as *mut c_void;
            vt_init(parser);
            vt_set_callbacks(
                parser,
                test_print,
                test_exec,
                test_csi,
                test_esc,
                test_osc,
                test_dcs,
                events.as_mut() as *mut ParserEvents as *mut c_void,
            );
        }
        (storage, events)
    }

    fn feed(parser: &mut [u64], bytes: &[u8]) {
        unsafe {
            vt_feed(
                parser.as_mut_ptr() as *mut c_void,
                bytes.as_ptr(),
                bytes.len(),
            )
        }
    }

    #[test]
    fn parser_accepts_colon_sgr_and_st_terminated_strings() {
        let (mut parser, mut events) = parser();
        feed(&mut parser, b"\x1b[38:2::255:128:0m");
        feed(&mut parser, b"\x1b]8;;https://example.com\x1b");
        feed(&mut parser, b"\\");
        feed(&mut parser, b"\x1bP1;2$qabc\x07def\x1b\\");

        assert_eq!(events.csi[0].0, b'm');
        assert_eq!(events.csi[0].1, [38, 2, 0, 255, 128, 0]);
        assert_eq!(events.csi[0].2, [0, 1, 1, 1, 1, 1]);
        assert_eq!(events.osc.pop().unwrap(), b"8;;https://example.com");
        assert_eq!(events.dcs[0].0, b'q');
        assert_eq!(events.dcs[0].1, [1, 2]);
        assert_eq!(events.dcs[0].2, [b'$']);
        assert_eq!(events.dcs[0].3, b"abc\x07def");
    }

    #[test]
    fn osc_payload_is_not_limited_to_one_kibibyte() {
        let (mut parser, events) = parser();
        let mut sequence = b"\x1b]52;c;".to_vec();
        sequence.extend(std::iter::repeat(b'A').take(4096));
        sequence.extend_from_slice(b"\x1b\\");
        feed(&mut parser, &sequence);
        assert_eq!(events.osc[0].len(), 52usize.to_string().len() + 3 + 4096);
    }

    #[test]
    fn parser_rejects_non_canonical_utf8_scalars() {
        let (mut parser, events) = parser();
        feed(&mut parser, b"\xC2\x80");
        feed(&mut parser, b"\xF4\x8F\xBF\xBF");
        feed(&mut parser, b"\xE0\x80\xAF");
        feed(&mut parser, b"\xED\xA0\x80");
        feed(&mut parser, b"\xF4\x90\x80\x80");
        feed(&mut parser, b"\xC0\xAF");

        assert_eq!(
            events.printed,
            [0x80, 0x10FFFF, 0xFFFD, 0xFFFD, 0xFFFD, 0xFFFD, 0xFFFD]
        );
        assert!(!events.printed.contains(&('/' as u32)));
    }

    #[test]
    fn invalid_csi_and_dcs_sequences_are_ignored_until_their_terminator() {
        let (mut parser, events) = parser();

        let mut overflowing_csi = b"\x1b[".to_vec();
        overflowing_csi.extend_from_slice(b"1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1mA");
        feed(&mut parser, &overflowing_csi);
        feed(&mut parser, b"\x1b[1$2mB");
        feed(&mut parser, b"\x1bP?ignored\x1b\\C");

        assert!(events.csi.is_empty());
        assert!(events.dcs.is_empty());
        assert_eq!(events.printed, ['A' as u32, 'B' as u32, 'C' as u32]);
    }

    #[test]
    fn color_and_cell_width_helpers_cover_new_forms() {
        assert_eq!(std::mem::size_of::<Cell>(), 12);
        assert!(is_wide('界' as u32));
        assert!(!is_wide('A' as u32));
        assert_eq!(
            parse_sgr_color(&[38, 2, 0, 255, 128, 0], &[0, 1, 1, 1, 1, 1], 0, true).0,
            Some(rgb_to_256(255, 128, 0))
        );
    }

    #[test]
    fn bold_attribute_is_limited_to_its_sgr_run() {
        let terminal = TermView::new();
        terminal.feed(b"plain\x1b[1mbold\x1b[22mnormal");
        let inner = terminal.inner.lock();
        let row = inner.state.grid.row_slice(0);

        assert!(row[..5].iter().all(|cell| cell.attrs & ATTR_BOLD == 0));
        assert!(row[5..9].iter().all(|cell| cell.attrs & ATTR_BOLD != 0));
        assert!(row[9..15].iter().all(|cell| cell.attrs & ATTR_BOLD == 0));
    }
}
