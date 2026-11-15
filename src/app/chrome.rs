//! Window title bar, status bar, and resizing.

use super::commands::platform_label;
use super::widgets::{icon_at, ui_f};
use super::{Act, OpenTerm, SettingsTab};
use crate::icons;
use crate::theme::*;
#[cfg(not(target_os = "macos"))]
use egui::CursorIcon;
use egui::{pos2, vec2, Color32, Frame, Id, Rect, Rounding, Sense, Stroke, ViewportCommand};

const TITLE_H: f32 = 38.0;

pub(super) fn titlebar(ctx: &egui::Context, app: &mut OpenTerm, acts: &mut Vec<Act>) {
    egui::TopBottomPanel::top("ot_titlebar")
        .exact_height(TITLE_H)
        .show_separator_line(false)
        .frame(Frame::none().fill(bg2()))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let p = ui.painter().clone();
            p.line_segment(
                [rect.left_bottom(), rect.right_bottom()],
                Stroke::new(1.0_f32, border()),
            );

            if let Some(maximized) = ctx.input(|i| i.viewport().maximized) {
                app.window_maximized = maximized;
            }
            let native_controls_left = if cfg!(target_os = "macos") {
                rect.right()
            } else {
                rect.right() - 138.0
            };
            let settings_rect = (cfg!(target_os = "macos") || rect.width() >= 310.0).then(|| {
                let x = if cfg!(target_os = "macos") {
                    rect.right() - 20.0
                } else {
                    native_controls_left - 18.0
                };
                Rect::from_center_size(pos2(x, rect.center().y), vec2(30.0, 28.0))
            });

            // Keep the drag hit target completely clear of settings and native
            // window controls. A full-width drag interaction swallowed their
            // clicks on undecorated Windows/Linux windows.
            let drag_rect = Rect::from_min_max(
                rect.min,
                pos2(
                    settings_rect
                        .map(|settings| settings.left() - 4.0)
                        .unwrap_or(native_controls_left)
                        .max(rect.left()),
                    rect.bottom(),
                ),
            );
            let drag = ui.interact(drag_rect, Id::new("ot_drag"), Sense::click_and_drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                toggle_max(ctx, &mut app.window_maximized);
            }

            let left = rect.left()
                + if cfg!(target_os = "macos") {
                    78.0
                } else {
                    12.0
                };
            if cfg!(target_os = "macos") || rect.width() >= 300.0 {
                let badge =
                    Rect::from_min_size(pos2(left, rect.center().y - 11.0), vec2(22.0, 22.0));
                p.rect(
                    badge,
                    Rounding::same(6.0),
                    bg4(),
                    Stroke::new(1.0_f32, border_d()),
                );
                icons::prompt(&p, badge.center(), fg());
                p.text(
                    pos2(badge.right() + 9.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "OpenTerm",
                    medium(12.5),
                    fg(),
                );
            }
            if rect.width() >= 620.0 {
                p.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Workspace",
                    ui_f(),
                    fg_dim(),
                );
            }

            if let Some(settings_rect) = settings_rect {
                let active = app.settings_tab == SettingsTab::Active;
                if icon_at(
                    ui,
                    settings_rect,
                    Id::new("ot_settings"),
                    "settings",
                    |p, c, col| icons::gear(p, c, if active { fg() } else { col }),
                )
                .clicked()
                {
                    acts.push(Act::OpenSettings);
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                let max = app.window_maximized;
                let w = 46.0;
                let mut x = rect.right() - w;
                for kind in [2u8, 1, 0] {
                    let r = Rect::from_min_size(pos2(x, rect.top()), vec2(w, TITLE_H - 1.0));
                    let resp = ui.interact(r, Id::new(("ot_wc", kind)), Sense::click());
                    let col = if resp.hovered() { fg() } else { fg_dim() };
                    if resp.hovered() {
                        p.rect_filled(
                            r,
                            Rounding::ZERO,
                            if kind == 2 { close_hover() } else { bg4() },
                        );
                    }
                    match kind {
                        0 => icons::minimize(&p, r.center(), col),
                        1 => icons::maximize(&p, r.center(), !max, col),
                        _ => icons::cross(
                            &p,
                            r.center(),
                            4.6,
                            if resp.hovered() {
                                Color32::WHITE
                            } else {
                                fg_dim()
                            },
                        ),
                    }
                    if resp.clicked() {
                        match kind {
                            0 => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                            1 => toggle_max(ctx, &mut app.window_maximized),
                            _ => ctx.send_viewport_cmd(ViewportCommand::Close),
                        }
                    }
                    x -= w;
                }
            }
            let _ = (app, acts);
        });
}

fn toggle_max(ctx: &egui::Context, maximized: &mut bool) {
    *maximized = !*maximized;
    ctx.send_viewport_cmd(ViewportCommand::Maximized(*maximized));
}

pub(super) fn statusbar(ctx: &egui::Context, app: &OpenTerm) {
    egui::TopBottomPanel::bottom("ot_status")
        .exact_height(24.0)
        .show_separator_line(false)
        .frame(Frame::none().fill(bg2()))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let p = ui.painter();
            p.line_segment(
                [rect.left_top(), rect.right_top()],
                Stroke::new(1.0_f32, border()),
            );
            let y = rect.center().y;
            let mut x = rect.left() + 12.0;
            let live = app.device_live(&app.selected);
            icons::dot(
                p,
                pos2(x + 3.0, y),
                3.0,
                if live { signal() } else { fg_muted() },
            );
            x += 12.0;
            let dev = app
                .devices
                .iter()
                .find(|d| d.id == app.selected)
                .map(|d| d.label.clone())
                .unwrap_or_default();
            let mut parts = vec![(dev, false)];
            if let Some(s) = app.active_session() {
                if !s.hostname.trim().is_empty() {
                    parts.push((s.hostname.clone(), false));
                }
                parts.push((platform_label(&s.platform).to_string(), true));
                let (c, r) = s.term.size();
                parts.push((format!("{c}×{r}"), false));
            }
            for (i, (part, is_platform)) in parts.iter().enumerate() {
                if i > 0 {
                    let r = p.text(
                        pos2(x, y),
                        egui::Align2::LEFT_CENTER,
                        "·",
                        sans(11.0),
                        fg_muted(),
                    );
                    x = r.right() + 7.0;
                }
                if *is_platform {
                    icons::platform(p, pos2(x + 6.0, y), part, fg_muted());
                    x += 15.0;
                }
                let r = p.text(
                    pos2(x, y),
                    egui::Align2::LEFT_CENTER,
                    part,
                    mono(11.0),
                    fg_muted(),
                );
                x = r.right() + 7.0;
            }
            let (msg, col) = match &app.flash {
                Some((m, err, _)) => (m.clone(), if *err { red() } else { fg_dim() }),
                None => ("openterm 0.1".to_string(), fg_muted()),
            };
            p.text(
                pos2(rect.right() - 12.0, y),
                egui::Align2::RIGHT_CENTER,
                msg,
                sans(11.0),
                col,
            );
        });
}

#[derive(Default)]
pub(super) struct ResizeOverlay {
    size: Option<(u32, u32)>,
    changed_at: Option<f64>,
}

impl ResizeOverlay {
    pub(super) fn show(&mut self, ctx: &egui::Context) {
        const HOLD: f64 = 0.75;
        const FADE: f64 = 0.20;

        let (size, now) = ctx.input(|input| {
            let rect = input
                .viewport()
                .inner_rect
                .unwrap_or_else(|| input.screen_rect());
            let pixels = rect.size() * input.pixels_per_point();
            (
                (pixels.x.round() as u32, pixels.y.round() as u32),
                input.time,
            )
        });
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        if self
            .size
            .replace(size)
            .is_some_and(|previous| previous != size)
        {
            self.changed_at = Some(now);
        }
        let Some(changed_at) = self.changed_at else {
            return;
        };
        let elapsed = now - changed_at;
        if elapsed >= HOLD + FADE {
            self.changed_at = None;
            return;
        }
        let opacity = if elapsed < HOLD {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(HOLD - elapsed));
            1.0
        } else {
            ctx.request_repaint();
            (1.0 - (elapsed - HOLD) / FADE) as f32
        };

        let (width, height) = size;
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            Id::new("ot_resize_size"),
        ));
        let text = painter.layout_no_wrap(
            format!("{width} × {height} px"),
            mono(17.0),
            fg().gamma_multiply(opacity),
        );
        let rect =
            Rect::from_center_size(ctx.screen_rect().center(), text.size() + vec2(36.0, 24.0));
        painter.rect(
            rect,
            Rounding::same(8.0),
            bg3().gamma_multiply(opacity),
            Stroke::new(1.0_f32, border_d().gamma_multiply(opacity)),
        );
        painter.galley(rect.center() - text.size() / 2.0, text, fg());
    }
}

// borderless window: resize from the edges
#[cfg(not(target_os = "macos"))]
pub(super) fn window_edges(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as D;
    let screen = ctx.screen_rect();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if !maximized {
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::new("ot_border"),
        ))
        .rect_stroke(
            screen.shrink(0.5),
            Rounding::ZERO,
            Stroke::new(1.0_f32, border_d()),
        );
    } else {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let m = 5.0;
    let (l, r, t, b) = (
        pos.x < screen.left() + m,
        pos.x > screen.right() - m,
        pos.y < screen.top() + m,
        pos.y > screen.bottom() - m,
    );
    let hit = match (l, r, t, b) {
        (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNwSe)),
        (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNeSw)),
        (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeNeSw)),
        (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeNwSe)),
        (true, ..) => Some((D::West, CursorIcon::ResizeHorizontal)),
        (_, true, ..) => Some((D::East, CursorIcon::ResizeHorizontal)),
        (_, _, true, _) => Some((D::North, CursorIcon::ResizeVertical)),
        (.., true) => Some((D::South, CursorIcon::ResizeVertical)),
        _ => None,
    };
    if let Some((dir, icon)) = hit {
        ctx.set_cursor_icon(icon);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_overlay_reports_physical_size_at_center_and_expires() {
        let ctx = egui::Context::default();
        let mut overlay = ResizeOverlay::default();
        let screen = |width| Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 480.0));
        let mut frame = |width, time| {
            let mut input = egui::RawInput {
                screen_rect: Some(screen(width)),
                time: Some(time),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&input.viewport_id)
                .unwrap()
                .native_pixels_per_point = Some(1.25);
            ctx.run(input, |ctx| overlay.show(ctx)).shapes
        };

        assert!(frame(800.0, 0.0).is_empty());
        let shapes = frame(840.0, 0.2);
        let text = shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text),
                _ => None,
            })
            .expect("resizing should show the window size");
        assert_eq!(text.galley.job.text, "1050 × 600 px");
        assert_eq!(text.pos + text.galley.size() / 2.0, screen(840.0).center());

        assert!(!frame(840.0, 1.05).is_empty());
        assert!(frame(840.0, 1.2).is_empty());
    }
}
