//! Installed monospace families for the terminal.

use anyhow::{Context, Result};
use fontdb::{Database, FaceInfo, Style, Weight};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};

impl super::OpenTerm {
    pub(super) fn load_terminal_font(&self, family: &str) -> Result<egui::FontData> {
        self.terminal_fonts
            .catalog()
            .context("installed fonts are unavailable")?
            .load(family)
    }
}

pub(super) struct FontCatalog {
    db: Database,
    families: BTreeMap<String, fontdb::ID>,
}

fn face_rank(face: &FaceInfo) -> (bool, u16) {
    (
        face.style != Style::Normal,
        face.weight.0.abs_diff(Weight::NORMAL.0),
    )
}

impl FontCatalog {
    fn scan() -> Self {
        let mut db = Database::new();
        db.load_system_fonts();
        Self::from_database(db)
    }

    fn from_database(db: Database) -> Self {
        let mut families = BTreeMap::new();
        for face in db.faces().filter(|face| face.monospaced) {
            let Some((family, _)) = face.families.first() else {
                continue;
            };
            let family = family.trim();
            if family.is_empty() || family.chars().any(char::is_control) {
                continue;
            }
            let best = families.entry(family.to_owned()).or_insert(face.id);
            if db
                .face(*best)
                .is_some_and(|previous| face_rank(face) < face_rank(previous))
            {
                *best = face.id;
            }
        }
        Self { db, families }
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.families.keys().map(String::as_str)
    }

    pub(super) fn load(&self, family: &str) -> Result<egui::FontData> {
        let id = *self
            .families
            .get(family)
            .context("font family is unavailable")?;
        self.db
            .with_face_data(id, |bytes, index| {
                ab_glyph::FontRef::try_from_slice_and_index(bytes, index)
                    .context("unsupported font data")?;
                let mut data = egui::FontData::from_owned(bytes.to_vec());
                data.index = index;
                Ok(data)
            })
            .context("couldn't read the font file")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_font_renders_in_the_terminal_family() {
        let mut db = Database::new();
        db.load_font_data(include_bytes!("../../assets/fonts/Inter-Regular.ttf").to_vec());
        db.load_font_data(include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf").to_vec());
        let catalog = FontCatalog::from_database(db);
        assert_eq!(catalog.names().collect::<Vec<_>>(), ["JetBrains Mono"]);
        let chosen = catalog.load("JetBrains Mono").unwrap();

        let ctx = egui::Context::default();
        crate::theme::set_terminal_font(&ctx, Some(chosen));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let font = crate::theme::terminal_font();
            ctx.fonts(|fonts| {
                let width = fonts.glyph_width(&font, 'M');
                assert!(width > 0.0);
                assert_eq!(width, fonts.glyph_width(&font, 'i'));
            });
        });
    }
}

pub(super) enum TerminalFonts {
    Scanning(Receiver<FontCatalog>),
    Ready(FontCatalog),
    Unavailable,
}

impl TerminalFonts {
    pub(super) fn scan(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(FontCatalog::scan());
            ctx.request_repaint();
        });
        Self::Scanning(rx)
    }

    pub(super) fn poll(&mut self) -> bool {
        let Self::Scanning(rx) = self else {
            return false;
        };
        *self = match rx.try_recv() {
            Ok(catalog) => Self::Ready(catalog),
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Self::Unavailable,
        };
        true
    }

    pub(super) fn catalog(&self) -> Option<&FontCatalog> {
        match self {
            Self::Ready(catalog) => Some(catalog),
            _ => None,
        }
    }
}
