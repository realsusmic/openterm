//! wake — lets background reader threads poke the UI so we only repaint when
//! bytes actually arrive (no 60fps idle burn).
use std::sync::OnceLock;

static CTX: OnceLock<egui::Context> = OnceLock::new();

pub fn set(ctx: egui::Context) {
    let _ = CTX.set(ctx);
}
pub fn poke() {
    if let Some(c) = CTX.get() {
        c.request_repaint();
    }
}
