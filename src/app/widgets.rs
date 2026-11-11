//! Shared controls and drawing helpers for the app.

use super::TerminalKind;
use crate::icons;
use crate::theme::*;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    pos2, vec2, Color32, CursorIcon, Galley, Id, Margin, Painter, Pos2, Rect, Response, Rounding,
    Sense, Stroke, TextEdit, Ui, Vec2,
};
use std::sync::Arc;

pub(super) fn primary_button(ui: &mut Ui, label: &str) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(bg()).font(medium(12.5)))
            .fill(fg())
            .stroke(Stroke::NONE)
            .rounding(7.0)
            .min_size(vec2(100.0, 30.0)),
    )
}

pub(super) fn field(
    ui: &mut Ui,
    label: &str,
    val: &mut String,
    hint: &str,
    password: bool,
) -> Response {
    ui.label(egui::RichText::new(label).size(11.0).color(fg_muted()));
    let eye_id = ui.make_persistent_id(("password_eye", label, hint));
    let mut revealed = password && ui.data(|d| d.get_temp::<bool>(eye_id).unwrap_or(false));
    let margin = if password {
        Margin {
            left: 9.0,
            right: 39.0,
            top: 7.0,
            bottom: 7.0,
        }
    } else {
        Margin::symmetric(9.0, 7.0)
    };
    let r = ui.add(
        TextEdit::singleline(val)
            .hint_text(egui::RichText::new(hint).color(fg_muted()))
            .password(password && !revealed)
            .margin(margin)
            .desired_width(f32::INFINITY),
    );
    if password {
        // A field reopened with an empty value starts safely hidden again.
        if revealed && val.is_empty() && !r.has_focus() {
            revealed = false;
            ui.data_mut(|d| d.insert_temp(eye_id, false));
        }
        let button_rect = Rect::from_center_size(
            pos2(r.rect.right() - 17.0, r.rect.center().y),
            vec2(28.0, 28.0),
        );
        let eye = ui.interact(button_rect, eye_id.with("button"), Sense::click());
        if eye.clicked() {
            revealed = !revealed;
            let now = ui.input(|i| i.time);
            ui.data_mut(|d| {
                d.insert_temp(eye_id, revealed);
                d.insert_temp(eye_id.with("next_blink"), now + blink_delay(now, eye_id));
            });
            // Clicking the eye should not make the user reacquire the field.
            r.request_focus();
        }
        let eye = eye.on_hover_text(if revealed {
            "Hide password"
        } else {
            "Show password"
        });
        paint_password_eye(ui, &eye, eye_id, revealed, val, &r);
    }
    ui.add_space(4.0);
    r
}

/// Native version of the password-eye easter egg: it wakes when the secret is
/// revealed, follows the pointer (or the typing caret), and blinks at irregular
/// intervals. Hidden passwords get a sleepy closed eye with lashes.
fn paint_password_eye(
    ui: &Ui,
    response: &Response,
    id: Id,
    revealed: bool,
    value: &str,
    input: &Response,
) {
    let now = ui.input(|i| i.time);
    let next_id = id.with("next_blink");
    let blink_id = id.with("blink_until");
    let caret_id = id.with("caret_until");
    let stored_next = ui.data(|d| d.get_temp::<f64>(next_id));
    let mut next = stored_next.unwrap_or_else(|| now + blink_delay(now, id));
    if stored_next.is_none() {
        ui.data_mut(|d| d.insert_temp(next_id, next));
    }
    let mut blink_until = ui.data(|d| d.get_temp::<f64>(blink_id).unwrap_or(0.0));
    let mut caret_until = ui.data(|d| d.get_temp::<f64>(caret_id).unwrap_or(0.0));

    if input.changed() || (input.has_focus() && ui.input(|i| i.pointer.primary_clicked())) {
        caret_until = now + 0.85;
        ui.data_mut(|d| d.insert_temp(caret_id, caret_until));
    }
    if revealed && now >= next {
        blink_until = now + 0.17;
        next = blink_until + blink_delay(blink_until, id);
        ui.data_mut(|d| {
            d.insert_temp(blink_id, blink_until);
            d.insert_temp(next_id, next);
        });
    } else if !revealed && next < now + 1.0 {
        next = now + blink_delay(now, id);
        ui.data_mut(|d| d.insert_temp(next_id, next));
    }
    let blinking = revealed && now < blink_until;
    if blinking {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    } else if revealed {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(
                (next - now).clamp(0.016, 7.0),
            ));
    }

    let open = ui
        .ctx()
        .animate_bool_with_time(id.with("lid"), revealed && !blinking, 0.2);
    let center = response.rect.center();
    let mut target = Vec2::ZERO;
    if revealed {
        if input.has_focus() && now < caret_until {
            let text_w = ui.fonts(|f| f.layout_no_wrap(value.to_owned(), ui_f(), fg()).size().x);
            let caret = pos2(
                (input.rect.left() + 9.0 + text_w).min(input.rect.right() - 41.0),
                input.rect.center().y,
            );
            target = caret - center;
        } else if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
            target = pointer - center;
        }
    }
    let distance = target.length();
    let reach = (distance / 160.0).min(1.0);
    let desired = if distance > 0.5 {
        vec2(
            target.x / distance * 3.2 * reach,
            target.y / distance * 1.7 * reach,
        )
    } else {
        Vec2::ZERO
    };
    let gaze_x = ui
        .ctx()
        .animate_value_with_time(id.with("gaze_x"), desired.x, 0.12);
    let gaze_y = ui
        .ctx()
        .animate_value_with_time(id.with("gaze_y"), desired.y, 0.12);

    let color = if revealed || response.hovered() {
        fg()
    } else {
        fg_dim()
    };
    if response.hovered() {
        ui.painter()
            .rect_filled(response.rect, Rounding::same(6.0), bg4());
    }
    let map = |x: f32, y: f32| pos2(center.x + (x - 12.0) * 0.72, center.y + (y - 12.0) * 0.72);
    let lid_y = 19.0 - 14.0 * open;
    if open > 0.08 {
        let iris = map(12.0 + gaze_x, 12.0 + gaze_y);
        ui.painter()
            .circle_stroke(iris, 2.45 * open.max(0.35), Stroke::new(1.15_f32, color));
        ui.painter()
            .circle_filled(iris, 0.95 * open.max(0.4), color);
    }
    paint_cubic(
        ui.painter(),
        map(2.0, 12.0),
        map(6.0, lid_y),
        map(18.0, lid_y),
        map(22.0, 12.0),
        color,
    );
    paint_cubic(
        ui.painter(),
        map(2.0, 12.0),
        map(6.0, 19.0),
        map(18.0, 19.0),
        map(22.0, 12.0),
        color,
    );
    if !revealed {
        for ((x1, y1), (x2, y2)) in [
            ((5.2, 15.4), (3.9, 17.4)),
            ((12.0, 17.3), (12.0, 19.8)),
            ((18.8, 15.4), (20.1, 17.4)),
        ] {
            ui.painter()
                .line_segment([map(x1, y1), map(x2, y2)], Stroke::new(1.15_f32, color));
        }
    }
}

fn paint_cubic(p: &Painter, a: Pos2, b: Pos2, c: Pos2, d: Pos2, color: Color32) {
    let mut points = Vec::with_capacity(13);
    for i in 0..=12 {
        let t = i as f32 / 12.0;
        let u = 1.0 - t;
        points.push(pos2(
            u * u * u * a.x + 3.0 * u * u * t * b.x + 3.0 * u * t * t * c.x + t * t * t * d.x,
            u * u * u * a.y + 3.0 * u * u * t * b.y + 3.0 * u * t * t * c.y + t * t * t * d.y,
        ));
    }
    p.add(egui::Shape::line(points, Stroke::new(1.15_f32, color)));
}

fn blink_delay(now: f64, id: Id) -> f64 {
    // Organic 3–7s spacing without an RNG or another UI-thread dependency.
    let seed = (now.to_bits() ^ id.value()).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    3.0 + (seed % 4000) as f64 / 1000.0
}

pub(super) fn seg(ui: &mut Ui, label: &str, on: bool) -> Response {
    let gal =
        ui.painter()
            .layout_no_wrap(label.into(), sans(12.0), if on { fg() } else { fg_dim() });
    let (r, resp) = ui.allocate_exact_size(vec2(gal.size().x + 22.0, 28.0), Sense::click());
    let fill = if on {
        bg4()
    } else if resp.hovered() {
        bg3()
    } else {
        bg2()
    };
    ui.painter().rect(
        r,
        Rounding::same(6.0),
        fill,
        Stroke::new(1.0_f32, if on { border_d() } else { border() }),
    );
    ui.painter()
        .galley(r.center() - gal.size() / 2.0, gal, fg());
    resp
}

pub(super) fn terminal_seg(ui: &mut Ui, terminal: TerminalKind, on: bool) -> Response {
    let label = terminal.label();
    let color = if on { fg() } else { fg_dim() };
    let gal = ui.painter().layout_no_wrap(label.into(), sans(12.0), color);
    let (r, resp) = ui.allocate_exact_size(vec2(gal.size().x + 42.0, 30.0), Sense::click());
    let fill = if on {
        bg4()
    } else if resp.hovered() {
        bg3()
    } else {
        bg2()
    };
    let painter = ui.painter();
    painter.rect(
        r,
        Rounding::same(6.0),
        fill,
        Stroke::new(1.0_f32, if on { border_d() } else { border() }),
    );
    let icon_center = pos2(r.left() + 15.0, r.center().y);
    match terminal {
        TerminalKind::PowerShell => icons::powershell(painter, icon_center, color),
        TerminalKind::Wsl => icons::wsl(painter, icon_center, color),
        TerminalKind::Bash => icons::bash(painter, icon_center, color),
        TerminalKind::Cmd => icons::cmd(painter, icon_center, color),
    }
    painter.galley(
        pos2(r.left() + 28.0, r.center().y - gal.size().y / 2.0),
        gal,
        color,
    );
    resp
}

// ───────────────────────────── widgets ─────────────────────────────

pub(super) fn ui_f() -> egui::FontId {
    sans(UI_SIZE)
}

pub(super) fn section(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_height(26.0);
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new(title)
                .font(medium(10.5))
                .color(fg_muted()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            right(ui);
        });
    });
    ui.add_space(2.0);
}

/// full-width, inset, rounded row. `paint(painter, rect, hovered)`
pub(super) fn row(
    ui: &mut Ui,
    h: f32,
    selected: bool,
    paint: impl FnOnce(&Painter, Rect, bool),
) -> Response {
    let (outer, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let r = outer.shrink2(vec2(6.0, 0.0));
    let hovered = resp.hovered();
    if selected {
        ui.painter().rect_filled(r, Rounding::same(6.0), bg4());
    } else if hovered {
        ui.painter().rect_filled(r, Rounding::same(6.0), bg3());
    }
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    paint(ui.painter(), r, hovered);
    resp
}

pub(super) fn icon_btn(
    ui: &mut Ui,
    tip: &str,
    paint: impl FnOnce(&Painter, Pos2, Color32),
) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    paint_icon_btn(ui, r, &resp, paint);
    resp.on_hover_text(tip)
}

pub(super) fn icon_at(
    ui: &mut Ui,
    r: Rect,
    id: Id,
    tip: &str,
    paint: impl FnOnce(&Painter, Pos2, Color32),
) -> Response {
    let resp = ui.interact(r, id, Sense::click());
    paint_icon_btn(ui, r, &resp, paint);
    resp.on_hover_text(tip)
}

fn paint_icon_btn(ui: &Ui, r: Rect, resp: &Response, paint: impl FnOnce(&Painter, Pos2, Color32)) {
    let col = if resp.hovered() { fg() } else { fg_dim() };
    if resp.hovered() {
        ui.painter().rect_filled(r, Rounding::same(6.0), bg4());
    }
    paint(ui.painter(), r.center(), col);
}

pub(super) fn elide(
    p: &Painter,
    text: &str,
    font: egui::FontId,
    color: Color32,
    max_w: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: max_w.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    p.layout_job(job)
}
