//! Runtime dark/light palette + embedded fonts.

use crate::settings::ThemeMode;
use egui::{
    epaint::Shadow, style::WidgetVisuals, Color32, FontData, FontDefinitions, FontFamily, FontId,
    Rounding, Stroke, TextStyle, Visuals,
};
use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(true);

fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn bg() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x0b, 0x0c, 0x0d)
    } else {
        Color32::from_rgb(0xf8, 0xf9, 0xfb)
    }
}
pub fn bg2() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x11, 0x12, 0x14)
    } else {
        Color32::from_rgb(0xf0, 0xf2, 0xf5)
    }
}
pub fn bg3() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x18, 0x19, 0x1c)
    } else {
        Color32::from_rgb(0xe6, 0xe9, 0xee)
    }
}
pub fn bg4() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x1e, 0x1f, 0x23)
    } else {
        Color32::from_rgb(0xdc, 0xe0, 0xe6)
    }
}
pub fn border() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x1f, 0x21, 0x24)
    } else {
        Color32::from_rgb(0xd4, 0xd8, 0xdf)
    }
}
pub fn border_d() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x2b, 0x2d, 0x32)
    } else {
        Color32::from_rgb(0xb9, 0xbf, 0xc9)
    }
}
pub fn fg() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0xd8, 0xdb, 0xdf)
    } else {
        Color32::from_rgb(0x1d, 0x22, 0x2a)
    }
}
pub fn fg_dim() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x8a, 0x8f, 0x97)
    } else {
        Color32::from_rgb(0x55, 0x5d, 0x69)
    }
}
pub fn fg_muted() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x55, 0x5a, 0x61)
    } else {
        Color32::from_rgb(0x82, 0x89, 0x94)
    }
}
pub fn signal() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0x6d, 0xd4, 0x00)
    } else {
        Color32::from_rgb(0x38, 0x91, 0x00)
    }
}
pub fn red() -> Color32 {
    if is_dark() {
        Color32::from_rgb(0xd6, 0x5c, 0x5c)
    } else {
        Color32::from_rgb(0xb4, 0x2f, 0x38)
    }
}
pub fn close_hover() -> Color32 {
    Color32::from_rgb(0xc4, 0x2b, 0x1c)
}

pub const UI_SIZE: f32 = 12.5;
pub const MONO_SIZE: f32 = 13.0;

pub fn sans(size: f32) -> FontId {
    FontId::proportional(size)
}
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn install(ctx: &egui::Context, mode: ThemeMode) {
    // ---- fonts (both OFL) ----
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.ttf")),
    );
    fonts.font_data.insert(
        "inter-medium".into(),
        FontData::from_static(include_bytes!("../assets/fonts/Inter-Medium.ttf")),
    );
    fonts.font_data.insert(
        "jbmono".into(),
        FontData::from_static(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
    );

    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "inter".into());
    prop.insert(1, "jbmono".into()); // glyph fallback (arrows, box drawing)
    let monof = fonts.families.entry(FontFamily::Monospace).or_default();
    monof.insert(0, "jbmono".into());
    let mut med = vec!["inter-medium".to_string(), "jbmono".to_string()];
    med.extend(
        fonts.families[&FontFamily::Proportional]
            .iter()
            .skip(2)
            .cloned(),
    );
    fonts
        .families
        .insert(FontFamily::Name("medium".into()), med);
    ctx.set_fonts(fonts);

    apply(ctx, mode);

    // ---- style ----
    let mut s = (*ctx.style()).clone();
    s.spacing.item_spacing = egui::vec2(6.0, 4.0);
    s.spacing.button_padding = egui::vec2(10.0, 5.0);
    s.spacing.interact_size.y = 26.0;
    s.spacing.scroll = egui::style::ScrollStyle::thin();
    s.interaction.selectable_labels = false;
    s.text_styles = [
        (TextStyle::Small, sans(11.0)),
        (TextStyle::Body, sans(UI_SIZE)),
        (TextStyle::Button, sans(UI_SIZE)),
        (TextStyle::Heading, medium(15.0)),
        (TextStyle::Monospace, mono(MONO_SIZE)),
    ]
    .into();
    ctx.set_style(s);
}

pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    DARK.store(mode == ThemeMode::Dark, Ordering::Relaxed);
    let mut v = if is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.override_text_color = Some(fg());
    v.extreme_bg_color = bg();
    v.faint_bg_color = bg2();
    v.panel_fill = bg();
    v.window_fill = bg2();
    v.window_stroke = Stroke::new(1.0_f32, border_d());
    v.window_rounding = Rounding::same(10.0);
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.menu_rounding = Rounding::same(6.0);
    v.selection.bg_fill = fg_muted().gamma_multiply(0.25);
    v.selection.stroke = Stroke::new(1.0_f32, fg());
    v.hyperlink_color = fg();
    v.text_cursor.stroke = Stroke::new(1.5_f32, fg());

    let r = Rounding::same(6.0);
    let wv = |bg: Color32, stroke: Stroke, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: stroke,
        fg_stroke: Stroke::new(1.0_f32, fg),
        rounding: r,
        expansion: 0.0,
    };
    v.widgets.noninteractive = wv(bg2(), Stroke::new(1.0_f32, border()), fg());
    v.widgets.inactive = wv(bg4(), Stroke::new(1.0_f32, border()), fg_dim());
    v.widgets.hovered = wv(bg4(), Stroke::new(1.0_f32, border_d()), fg());
    v.widgets.active = wv(bg3(), Stroke::new(1.0_f32, fg_muted()), fg());
    v.widgets.open = wv(bg3(), Stroke::new(1.0_f32, border_d()), fg());
    ctx.set_visuals(v);
}
