//! editor — tabbed code editor: syntect highlighting + line-number gutter.

use crate::{icons, theme::*};
use egui::{
    pos2, vec2, Id, Key, Modifiers, Rect, Rounding, ScrollArea, Sense, Stroke, TextEdit, Ui,
};
use egui_extras::syntax_highlighting::{highlight, CodeTheme};
use std::path::{Path, PathBuf};

pub const LARGE_FILE_THRESHOLD: u64 = 4 * 1024 * 1024;
pub const LARGE_FILE_NOTICE: &str = "file is over 4mb, so this may take a while";

pub struct OpenFile {
    pub path: PathBuf,
    pub display_name: String,
    pub text: String,
    pub dirty: bool,
    read_only: bool,
    large: bool,
    hex_data: Option<Vec<u8>>,
    lang: String,
    remote: Option<RemoteFile>,
}

#[derive(Clone)]
struct RemoteFile {
    session_id: u64,
    path: String,
}

pub struct RemoteSave {
    pub key: PathBuf,
    pub session_id: u64,
    pub path: String,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct Editor {
    pub files: Vec<OpenFile>,
    pub active: Option<usize>,
    remote_save: Option<RemoteSave>,
}

pub type Flash = Option<(String, bool)>;

impl Editor {
    pub fn open_bytes(&mut self, path: &Path, bytes: Vec<u8>) -> Result<(), String> {
        if let Some(i) = self.files.iter().position(|f| f.path == path) {
            self.active = Some(i);
            return Ok(());
        }
        let large = bytes.len() as u64 > LARGE_FILE_THRESHOLD;
        let text = String::from_utf8(bytes).map_err(|_| "not a text file".to_string())?;
        self.files.push(OpenFile {
            path: path.to_path_buf(),
            display_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            text,
            dirty: false,
            read_only: false,
            large,
            hex_data: None,
            lang: lang_for(path),
            remote: None,
        });
        self.active = Some(self.files.len() - 1);
        Ok(())
    }

    pub fn open_remote(
        &mut self,
        session_id: u64,
        remote_path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        let large = bytes.len() as u64 > LARGE_FILE_THRESHOLD;
        let key = PathBuf::from(format!("remote-{session_id}")).join(
            remote_path
                .trim_start_matches(['/', '\\'])
                .replace(':', "_"),
        );
        if let Some(i) = self.files.iter().position(|f| f.path == key) {
            self.active = Some(i);
            return Ok(());
        }
        let text = String::from_utf8(bytes).map_err(|_| "not a text file".to_string())?;
        let display_name = remote_path
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(remote_path)
            .to_string();
        self.files.push(OpenFile {
            path: key,
            display_name,
            text,
            dirty: false,
            read_only: false,
            large,
            hex_data: None,
            lang: lang_for(Path::new(remote_path)),
            remote: Some(RemoteFile {
                session_id,
                path: remote_path.to_string(),
            }),
        });
        self.active = Some(self.files.len() - 1);
        Ok(())
    }

    pub fn open_hex_bytes(&mut self, path: &Path, bytes: Vec<u8>) {
        if let Some(i) = self.files.iter().position(|file| file.path == path) {
            self.files[i].hex_data = Some(bytes);
            self.files[i].text.clear();
            self.files[i].read_only = true;
            self.files[i].dirty = false;
            self.active = Some(i);
            return;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.files.push(OpenFile {
            path: path.to_path_buf(),
            display_name: format!("{name} [hex]"),
            text: String::new(),
            dirty: false,
            read_only: true,
            large: bytes.len() as u64 > LARGE_FILE_THRESHOLD,
            hex_data: Some(bytes),
            lang: "txt".into(),
            remote: None,
        });
        self.active = Some(self.files.len() - 1);
    }

    fn close(&mut self, i: usize) {
        self.files.remove(i);
        self.active = if self.files.is_empty() {
            None
        } else {
            Some(i.min(self.files.len() - 1))
        };
    }

    fn save(&mut self, i: usize) -> Flash {
        let f = &mut self.files[i];
        let name = f.display_name.clone();
        if let Some(remote) = &f.remote {
            self.remote_save = Some(RemoteSave {
                key: f.path.clone(),
                session_id: remote.session_id,
                path: remote.path.clone(),
                data: f.text.as_bytes().to_vec(),
            });
            return Some((format!("saving {name} to remote host…"), false));
        }
        Some(match std::fs::write(&f.path, &f.text) {
            Ok(()) => {
                f.dirty = false;
                (format!("saved {name}"), false)
            }
            Err(e) => (format!("couldn't save {name}: {e}"), true),
        })
    }

    pub fn take_remote_save(&mut self) -> Option<RemoteSave> {
        self.remote_save.take()
    }

    pub fn requeue_remote_save(&mut self, save: RemoteSave) {
        self.remote_save = Some(save);
    }

    pub fn finish_remote_save(
        &mut self,
        key: &Path,
        saved_data: &[u8],
        result: Result<(), String>,
    ) -> Flash {
        let Some(file) = self.files.iter_mut().find(|file| file.path == key) else {
            return None;
        };
        let name = file.display_name.clone();
        Some(match result {
            Ok(()) => {
                // The user may keep typing while the SSH write is in flight. Only
                // clear the marker when the current buffer is exactly what landed.
                if file.text.as_bytes() == saved_data {
                    file.dirty = false;
                }
                (format!("saved {name} to remote host"), false)
            }
            Err(error) => (format!("couldn't save {name}: {error}"), true),
        })
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Flash {
        let mut flash = None;

        // ── tab strip ──
        let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
        let p = ui.painter().clone();
        p.rect_filled(strip, Rounding::ZERO, bg2());
        p.line_segment(
            [strip.left_bottom(), strip.right_bottom()],
            Stroke::new(1.0_f32, border()),
        );

        let mut x = strip.left() + 6.0;
        let (mut close, mut activate) = (None, None);
        for (i, f) in self.files.iter().enumerate() {
            let active = self.active == Some(i);
            let name = f.display_name.clone();
            let gal = p.layout_no_wrap(name, ui_font(), if active { fg() } else { fg_dim() });
            let rect =
                Rect::from_min_size(pos2(x, strip.top() + 5.0), vec2(gal.size().x + 42.0, 29.0));
            if rect.left() > strip.right() {
                break;
            }
            let resp = ui.interact(rect, Id::new(("etab", i)), Sense::click());
            if active {
                p.rect(
                    rect,
                    Rounding {
                        nw: 6.0,
                        ne: 6.0,
                        sw: 0.0,
                        se: 0.0,
                    },
                    bg(),
                    Stroke::NONE,
                );
                p.line_segment(
                    [
                        rect.left_top() + vec2(6.0, 0.0),
                        rect.right_top() - vec2(6.0, 0.0),
                    ],
                    Stroke::new(1.0_f32, border_d()),
                );
            } else if resp.hovered() {
                p.rect_filled(rect.shrink2(vec2(0.0, 3.0)), Rounding::same(6.0), bg3());
            }
            p.galley(
                pos2(rect.left() + 12.0, rect.center().y - gal.size().y / 2.0),
                gal,
                fg(),
            );
            let xc = pos2(rect.right() - 14.0, rect.center().y);
            let xr = ui.interact(
                Rect::from_center_size(xc, vec2(16.0, 16.0)),
                Id::new(("etabx", i)),
                Sense::click(),
            );
            if xr.hovered() {
                p.rect_filled(
                    Rect::from_center_size(xc, vec2(16.0, 16.0)),
                    Rounding::same(4.0),
                    bg4(),
                );
            }
            if f.dirty && !xr.hovered() {
                icons::dot(&p, xc, 3.0, fg_dim());
            } else if active || resp.hovered() || xr.hovered() {
                icons::cross(&p, xc, 3.2, if xr.hovered() { fg() } else { fg_muted() });
            }
            if xr.clicked() || resp.middle_clicked() {
                close = Some(i);
            } else if resp.clicked() {
                activate = Some(i);
            }
            x = rect.right() + 2.0;
        }
        if let Some(i) = activate {
            self.active = Some(i);
        }
        if let Some(i) = close {
            self.close(i);
        }

        // ── body ──
        let Some(idx) = self.active else {
            ui.add_space(ui.available_height() * 0.38);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No file open")
                        .font(medium(13.0))
                        .color(fg_dim()),
                );
                ui.add_space(2.0);
                ui.label(egui::RichText::new("Pick a file in the sidebar").color(fg_muted()));
            });
            return flash;
        };

        let id = Id::new(("editor_text", &self.files[idx].path));
        if ui.memory(|m| m.has_focus(id))
            && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S))
        {
            flash = self.save(idx);
        }

        let f = &mut self.files[idx];
        if let Some(bytes) = f.hex_data.as_deref() {
            hex_view(ui, idx, bytes);
            return flash;
        }
        let lines = f.text.lines().count().max(1) + usize::from(f.text.ends_with('\n'));
        let width = lines.to_string().len().max(3);
        let gutter: String = (1..=lines).map(|n| format!("{n:>width$}\n")).collect();
        let lang = f.lang.clone();
        let large = f.large;

        ScrollArea::both()
            .id_salt(("editor_scroll", idx))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(10.0);
                    ui.spacing_mut().item_spacing.x = 16.0;
                    ui.label(
                        egui::RichText::new(gutter.trim_end())
                            .font(mono(MONO_SIZE))
                            .color(fg_muted()),
                    );
                    let theme = if ui.visuals().dark_mode {
                        CodeTheme::dark(MONO_SIZE)
                    } else {
                        CodeTheme::light(MONO_SIZE)
                    };
                    let mut layouter = |ui: &Ui, text: &str, _w: f32| {
                        let mut job = if large {
                            egui::text::LayoutJob::simple(
                                text.to_owned(),
                                mono(MONO_SIZE),
                                fg(),
                                f32::INFINITY,
                            )
                        } else {
                            highlight(ui.ctx(), ui.style(), &theme, text, &lang)
                        };
                        job.wrap.max_width = f32::INFINITY;
                        ui.fonts(|f| f.layout_job(job))
                    };
                    let out = TextEdit::multiline(&mut f.text)
                        .id(id)
                        .code_editor()
                        .frame(false)
                        .margin(egui::Margin::ZERO)
                        .desired_width(f32::INFINITY)
                        .lock_focus(true)
                        .interactive(!f.read_only)
                        .layouter(&mut layouter)
                        .show(ui);
                    if out.response.changed() {
                        f.dirty = true;
                    }
                });
            });
        flash
    }
}

pub fn is_text_bytes(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0)
}

fn hex_view(ui: &mut Ui, index: usize, bytes: &[u8]) {
    const BYTES_PER_ROW: usize = 16;
    let rows = bytes.len().div_ceil(BYTES_PER_ROW).max(1);
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    ScrollArea::both()
        .id_salt(("editor_hex_scroll", index))
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows, |ui, range| {
            ui.set_min_width(640.0);
            for row in range {
                let start = row * BYTES_PER_ROW;
                let end = (start + BYTES_PER_ROW).min(bytes.len());
                let chunk = &bytes[start..end];
                let mut line = format!("{start:08X}  ");
                for column in 0..BYTES_PER_ROW {
                    if let Some(byte) = chunk.get(column) {
                        line.push_str(&format!("{byte:02X} "));
                    } else {
                        line.push_str("   ");
                    }
                    if column == 7 {
                        line.push(' ');
                    }
                }
                line.push_str(" | ");
                for byte in chunk {
                    line.push(if byte.is_ascii_graphic() || *byte == b' ' {
                        char::from(*byte)
                    } else {
                        '.'
                    });
                }
                ui.label(egui::RichText::new(line).font(mono(MONO_SIZE)).color(fg()));
            }
        });
}

fn ui_font() -> egui::FontId {
    sans(UI_SIZE)
}

fn lang_for(p: &Path) -> String {
    match p.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "rs" => "rs",
        "js" | "mjs" | "cjs" | "jsx" => "js",
        "ts" | "tsx" => "ts",
        "py" => "py",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "rb",
        "sh" | "bash" | "zsh" => "sh",
        "ps1" => "ps1",
        "html" | "htm" => "html",
        "css" => "css",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" => "md",
        "lua" => "lua",
        "zig" => "zig",
        _ => "txt",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_file_is_editable_and_generates_save_request() {
        let mut editor = Editor::default();
        editor
            .open_remote(42, "/srv/project/main.rs", b"old".to_vec())
            .unwrap();
        let index = editor.active.unwrap();
        assert!(!editor.files[index].read_only);
        editor.files[index].text = "new".into();
        editor.files[index].dirty = true;
        let flash = editor.save(index).unwrap();
        assert!(!flash.1);
        let request = editor.take_remote_save().unwrap();
        assert_eq!(request.session_id, 42);
        assert_eq!(request.path, "/srv/project/main.rs");
        assert_eq!(request.data, b"new");
        editor
            .finish_remote_save(&request.key, &request.data, Ok(()))
            .expect("save completion flash");
        assert!(!editor.files[index].dirty);
    }

    #[test]
    fn completing_an_older_remote_save_keeps_newer_edits_dirty() {
        let mut editor = Editor::default();
        editor
            .open_remote(7, "/srv/project/main.rs", b"old".to_vec())
            .unwrap();
        editor.files[0].text = "first save".into();
        editor.files[0].dirty = true;
        editor.save(0);
        let first = editor.take_remote_save().unwrap();

        editor.files[0].text = "newer edit".into();
        editor.files[0].dirty = true;
        editor.finish_remote_save(&first.key, &first.data, Ok(()));

        assert!(editor.files[0].dirty);
    }

    #[test]
    fn files_over_four_megabytes_use_large_file_mode() {
        let mut editor = Editor::default();
        let bytes = vec![b'a'; LARGE_FILE_THRESHOLD as usize + 1];

        editor
            .open_bytes(Path::new("large.txt"), bytes.clone())
            .unwrap();
        editor.open_remote(9, "/srv/large.txt", bytes).unwrap();

        assert!(editor.files[0].large);
        assert!(editor.files[1].large);
    }

    #[test]
    fn binary_detection_and_hex_view_preserve_original_bytes() {
        assert!(is_text_bytes(b"hello\nworld"));
        assert!(!is_text_bytes(b"hello\0world"));
        assert!(!is_text_bytes(&[0xff, 0xfe]));

        let mut editor = Editor::default();
        editor.open_hex_bytes(Path::new("image.bin"), vec![0, 1, 254, 255]);
        assert_eq!(
            editor.files[0].hex_data.as_deref(),
            Some(&[0, 1, 254, 255][..])
        );
        assert!(editor.files[0].read_only);
    }
}
