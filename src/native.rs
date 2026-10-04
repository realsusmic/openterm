//! native — FFI to the Zig grid primitives. All unsafe grid access lives here.

/// palette sentinels meaning "terminal default color" (not palette index 0/7)
pub const DEF_FG: u16 = 256;
pub const DEF_BG: u16 = 257;

pub const ATTR_BOLD: u16 = 1 << 0;
pub const ATTR_ITALIC: u16 = 1 << 1;
pub const ATTR_UNDERLINE: u16 = 1 << 2;
pub const ATTR_STRIKE: u16 = 1 << 3;
pub const ATTR_REVERSE: u16 = 1 << 4;
pub const ATTR_DIM: u16 = 1 << 5;
pub const ATTR_BLINK: u16 = 1 << 6;
pub const ATTR_HIDDEN: u16 = 1 << 7;
pub const ATTR_UNDERLINE_STYLE_SHIFT: u16 = 8;
pub const ATTR_UNDERLINE_STYLE_MASK: u16 = 0b11 << ATTR_UNDERLINE_STYLE_SHIFT;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub glyph: u32,
    pub fg: u16,
    pub bg: u16,
    pub attrs: u16,
    /// Display columns occupied by this scalar: 1 or 2; 0 marks a continuation.
    pub width: u8,
    pub _pad: u8,
}
impl Default for Cell {
    fn default() -> Self {
        Self {
            glyph: ' ' as u32,
            fg: DEF_FG,
            bg: DEF_BG,
            attrs: 0,
            width: 1,
            _pad: 0,
        }
    }
}

extern "C" {
    fn fg_cell_size() -> usize;
    fn fg_scroll_up(
        cells: *mut Cell,
        cols: u32,
        rows: u32,
        top: u32,
        bottom: u32,
        n: u32,
        blank: Cell,
    );
    fn fg_scroll_down(
        cells: *mut Cell,
        cols: u32,
        rows: u32,
        top: u32,
        bottom: u32,
        n: u32,
        blank: Cell,
    );
    fn fg_clear_rect(cells: *mut Cell, cols: u32, x: u32, y: u32, w: u32, h: u32, blank: Cell);
    fn fg_insert_cells(cells: *mut Cell, cols: u32, x: u32, y: u32, n: u32, blank: Cell);
    fn fg_delete_cells(cells: *mut Cell, cols: u32, x: u32, y: u32, n: u32, blank: Cell);
}

#[derive(Clone)]
pub struct Grid {
    pub cols: u32,
    pub rows: u32,
    cells: Vec<Cell>,
    // OSC 8 hyperlink ids parallel to `cells`. Keeping these out of Cell makes
    // the hot C ABI cell exactly 12 bytes and avoids an unaligned packed u32.
    links: Vec<u32>,
    blank: Cell,
}

impl Grid {
    pub fn new(cols: u32, rows: u32) -> Self {
        assert_eq!(std::mem::size_of::<Cell>(), unsafe { fg_cell_size() });
        assert_eq!(std::mem::size_of::<Cell>(), 12);
        Self {
            cols,
            rows,
            cells: vec![Cell::default(); (cols * rows) as usize],
            links: vec![0; (cols * rows) as usize],
            blank: Cell::default(),
        }
    }

    /// resize keeping content. `shift` rows are dropped off the top first
    /// (used to keep the cursor on screen when shrinking).
    pub fn resize(&mut self, cols: u32, rows: u32, shift: u32) {
        if cols == self.cols && rows == self.rows && shift == 0 {
            return;
        }
        let mut next = vec![self.blank; (cols * rows) as usize];
        let mut next_links = vec![0; (cols * rows) as usize];
        let copy_rows = self.rows.saturating_sub(shift).min(rows);
        let copy_cols = self.cols.min(cols) as usize;
        for y in 0..copy_rows {
            let src = ((y + shift) * self.cols) as usize;
            let dst = (y * cols) as usize;
            next[dst..dst + copy_cols].copy_from_slice(&self.cells[src..src + copy_cols]);
            next_links[dst..dst + copy_cols].copy_from_slice(&self.links[src..src + copy_cols]);
        }
        self.cells = next;
        self.links = next_links;
        self.cols = cols;
        self.rows = rows;
        for y in 0..rows {
            self.repair_wide_row(y);
        }
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, c: Cell) {
        self.set_with_link(x, y, c, 0);
    }
    #[inline]
    pub fn set_with_link(&mut self, x: u32, y: u32, c: Cell, link_id: u32) {
        if x < self.cols && y < self.rows {
            let index = (y * self.cols + x) as usize;
            self.cells[index] = c;
            self.links[index] = link_id;
        }
    }
    #[inline]
    pub fn row_slice(&self, y: u32) -> &[Cell] {
        let s = (y * self.cols) as usize;
        &self.cells[s..s + self.cols as usize]
    }
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Option<Cell> {
        (x < self.cols && y < self.rows).then(|| self.cells[(y * self.cols + x) as usize])
    }
    pub fn row(&self, y: u32) -> Vec<Cell> {
        self.row_slice(y).to_vec()
    }
    #[inline]
    pub fn row_links(&self, y: u32) -> &[u32] {
        let s = (y * self.cols) as usize;
        &self.links[s..s + self.cols as usize]
    }
    pub fn row_with_links(&self, y: u32) -> (Vec<Cell>, Vec<u32>) {
        (self.row(y), self.row_links(y).to_vec())
    }

    pub fn scroll_up(&mut self, top: u32, bottom: u32, n: u32) {
        unsafe {
            fg_scroll_up(
                self.cells.as_mut_ptr(),
                self.cols,
                self.rows,
                top,
                bottom,
                n,
                self.blank,
            )
        }
        scroll_up_slice(&mut self.links, self.cols, self.rows, top, bottom, n, 0);
    }
    pub fn scroll_down(&mut self, top: u32, bottom: u32, n: u32) {
        unsafe {
            fg_scroll_down(
                self.cells.as_mut_ptr(),
                self.cols,
                self.rows,
                top,
                bottom,
                n,
                self.blank,
            )
        }
        scroll_down_slice(&mut self.links, self.cols, self.rows, top, bottom, n, 0);
    }
    pub fn clear_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        if x >= self.cols || y >= self.rows {
            return;
        }
        let w = w.min(self.cols - x);
        let h = h.min(self.rows - y);
        for row in y..y + h {
            if x > 0 && self.get(x, row).is_some_and(|cell| cell.width == 0) {
                self.set(x - 1, row, self.blank);
            }
            let right = x + w;
            if right < self.cols
                && self
                    .get(right.saturating_sub(1), row)
                    .is_some_and(|cell| cell.width == 2)
            {
                self.set(right, row, self.blank);
            }
        }
        unsafe { fg_clear_rect(self.cells.as_mut_ptr(), self.cols, x, y, w, h, self.blank) }
        for row in y..y + h {
            let start = (row * self.cols + x) as usize;
            self.links[start..start + w as usize].fill(0);
        }
    }
    pub fn insert_cells(&mut self, x: u32, y: u32, n: u32) {
        if y >= self.rows {
            return;
        }
        unsafe { fg_insert_cells(self.cells.as_mut_ptr(), self.cols, x, y, n, self.blank) }
        let row = (y * self.cols) as usize;
        insert_slice(
            &mut self.links[row..row + self.cols as usize],
            x as usize,
            n as usize,
            0,
        );
        self.repair_wide_row(y);
    }
    pub fn delete_cells(&mut self, x: u32, y: u32, n: u32) {
        if y >= self.rows {
            return;
        }
        unsafe { fg_delete_cells(self.cells.as_mut_ptr(), self.cols, x, y, n, self.blank) }
        let row = (y * self.cols) as usize;
        delete_slice(
            &mut self.links[row..row + self.cols as usize],
            x as usize,
            n as usize,
            0,
        );
        self.repair_wide_row(y);
    }

    fn repair_wide_row(&mut self, y: u32) {
        if y >= self.rows {
            return;
        }
        let mut x = 0;
        while x < self.cols {
            let cell = self.get(x, y).unwrap_or_default();
            if cell.width == 2 {
                if x + 1 >= self.cols || self.get(x + 1, y).map_or(true, |next| next.width != 0) {
                    self.set(x, y, self.blank);
                } else {
                    x += 1;
                }
            } else if cell.width == 0
                && (x == 0
                    || self
                        .get(x - 1, y)
                        .map_or(true, |previous| previous.width != 2))
            {
                self.set(x, y, self.blank);
            }
            x += 1;
        }
    }
}

fn scroll_up_slice<T: Copy>(
    cells: &mut [T],
    cols: u32,
    rows: u32,
    top: u32,
    bottom: u32,
    n: u32,
    blank: T,
) {
    if n == 0 || bottom <= top || bottom > rows {
        return;
    }
    let steps = n.min(bottom - top);
    let src = ((top + steps) * cols) as usize;
    let dst = (top * cols) as usize;
    let count = ((bottom - top - steps) * cols) as usize;
    cells.copy_within(src..src + count, dst);
    let fill = ((bottom - steps) * cols) as usize;
    cells[fill..(bottom * cols) as usize].fill(blank);
}

fn scroll_down_slice<T: Copy>(
    cells: &mut [T],
    cols: u32,
    rows: u32,
    top: u32,
    bottom: u32,
    n: u32,
    blank: T,
) {
    if n == 0 || bottom <= top || bottom > rows {
        return;
    }
    let steps = n.min(bottom - top);
    let src = (top * cols) as usize;
    let dst = ((top + steps) * cols) as usize;
    let count = ((bottom - top - steps) * cols) as usize;
    cells.copy_within(src..src + count, dst);
    cells[src..dst].fill(blank);
}

fn insert_slice<T: Copy>(row: &mut [T], x: usize, n: usize, blank: T) {
    if x >= row.len() || n == 0 {
        return;
    }
    let shift = n.min(row.len() - x);
    row.copy_within(x..row.len() - shift, x + shift);
    row[x..x + shift].fill(blank);
}

fn delete_slice<T: Copy>(row: &mut [T], x: usize, n: usize, blank: T) {
    if x >= row.len() || n == 0 {
        return;
    }
    let shift = n.min(row.len() - x);
    row.copy_within(x + shift.., x);
    let fill = row.len() - shift;
    row[fill..].fill(blank);
}
