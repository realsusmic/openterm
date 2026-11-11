//! Terminal tabs and the editor pane.

use super::widgets::{elide, icon_at, ui_f};
use super::{resolved_terminal, Act, OpenTerm};
use crate::icons;
use crate::theme::*;
use egui::{pos2, vec2, Frame, Id, Pos2, Rect, Response, Rounding, Sense, Stroke, Ui};

pub(super) fn workspace(ctx: &egui::Context, app: &mut OpenTerm, acts: &mut Vec<Act>) {
    egui::CentralPanel::default()
        .frame(Frame::none().fill(bg()))
        .show(ctx, |ui| {
            // tab bar
            egui::TopBottomPanel::top("ot_tabs")
                .exact_height(38.0)
                .show_separator_line(false)
                .frame(Frame::none().fill(bg2()))
                .show_inside(ui, |ui| {
                    let rect = ui.max_rect();
                    ui.painter().line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.0_f32, border()),
                    );
                    let mut x = rect.left() + 8.0;
                    let show_tools = rect.width() >= 390.0;
                    let tabs_right = if show_tools {
                        rect.right() - 140.0
                    } else {
                        rect.right() - 8.0
                    };
                    for s in app.sessions.iter().filter(|s| s.device == app.selected) {
                        let remaining = tabs_right - x;
                        if remaining < 70.0 {
                            break;
                        }
                        let active = app.active == Some(s.id);
                        let live = s.backend.is_some();
                        let reserve_new_tab = if remaining >= 104.0 { 34.0 } else { 0.0 };
                        let (resp, closed, w) = tab(
                            ui,
                            pos2(x, rect.top() + 6.0),
                            s.id,
                            &s.label,
                            active,
                            live,
                            remaining - reserve_new_tab,
                        );
                        if closed {
                            acts.push(Act::Close(s.id));
                        } else if resp.clicked() {
                            acts.push(Act::Activate(s.id));
                        }
                        if resp.middle_clicked() {
                            acts.push(Act::Close(s.id));
                        }
                        x += w + 4.0;
                    }
                    if x + 28.0 <= tabs_right {
                        let plus = Rect::from_center_size(
                            pos2(x + 14.0, rect.center().y),
                            vec2(26.0, 26.0),
                        );
                        let plus_response = icon_at(
                            ui,
                            plus,
                            Id::new("ot_newtab"),
                            if app.selected == "local" {
                                "choose shell  (ctrl+t uses default)"
                            } else {
                                "new tab  (ctrl+t)"
                            },
                            |p, c, col| icons::plus(p, c, 4.5, col),
                        );
                        let popup_id = Id::new("ot_newtab_shell_menu");
                        if plus_response.clicked() {
                            if app.selected == "local" {
                                ui.memory_mut(|memory| memory.toggle_popup(popup_id));
                            } else {
                                acts.push(Act::NewTab);
                            }
                        }
                        if app.selected == "local" {
                            let selected = resolved_terminal(
                                &app.settings.default_shell,
                                &app.available_terminals,
                            );
                            let mut picked = None;
                            let _ = egui::popup::popup_above_or_below_widget(
                                ui,
                                popup_id,
                                &plus_response,
                                egui::AboveOrBelow::Below,
                                egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    ui.set_min_width(178.0);
                                    if app.available_terminals.is_empty() {
                                        ui.label(
                                            egui::RichText::new("No shell detected").color(red()),
                                        );
                                    }
                                    for &terminal in &app.available_terminals {
                                        let label = if terminal == selected {
                                            format!("{}  ·  default", terminal.label())
                                        } else {
                                            terminal.label().to_string()
                                        };
                                        if ui.button(label).clicked() {
                                            picked = Some(terminal);
                                        }
                                    }
                                },
                            );
                            if let Some(terminal) = picked {
                                ui.memory_mut(|memory| memory.close_popup());
                                acts.push(Act::NewTerminal(terminal));
                            }
                        }
                    }

                    // right side: Info pill + editor toggle
                    if show_tools {
                        let er = Rect::from_center_size(
                            pos2(rect.right() - 22.0, rect.center().y),
                            vec2(28.0, 26.0),
                        );
                        let tip = if app.show_editor {
                            "hide editor  (ctrl+shift+e)"
                        } else {
                            "show editor  (ctrl+shift+e)"
                        };
                        let on = app.show_editor;
                        if icon_at(
                            ui,
                            er,
                            Id::new("ot_editor_toggle"),
                            tip,
                            move |p, c, col| icons::split(p, c, if on { fg() } else { col }),
                        )
                        .clicked()
                        {
                            acts.push(Act::ToggleEditor);
                        }
                        let gal = ui
                            .painter()
                            .layout_no_wrap("Info".into(), sans(11.5), fg_dim());
                        let nr = Rect::from_min_size(
                            pos2(
                                er.left() - 10.0 - gal.size().x - 38.0,
                                rect.center().y - 12.0,
                            ),
                            vec2(gal.size().x + 38.0, 24.0),
                        );
                        let nresp = ui
                            .interact(nr, Id::new("ot_sysinfo"), Sense::click())
                            .on_hover_text("show system information");
                        let p = ui.painter();
                        p.rect(
                            nr,
                            Rounding::same(6.0),
                            if nresp.hovered() { bg4() } else { bg3() },
                            Stroke::new(1.0_f32, border()),
                        );
                        let control_color = if nresp.hovered() { fg() } else { fg_dim() };
                        icons::info(p, pos2(nr.left() + 13.0, nr.center().y), control_color);
                        p.galley(
                            pos2(nr.left() + 25.0, nr.center().y - gal.size().y / 2.0),
                            gal,
                            control_color,
                        );
                        if nresp.clicked() {
                            acts.push(Act::Info);
                        }
                    }
                });

            // breadcrumb
            egui::TopBottomPanel::top("ot_crumb")
                .exact_height(32.0)
                .show_separator_line(false)
                .frame(Frame::none().fill(bg()))
                .show_inside(ui, |ui| {
                    let rect = ui.max_rect();
                    let p = ui.painter();
                    p.line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.0_f32, border()),
                    );
                    let y = rect.center().y;
                    icons::prompt(p, pos2(rect.left() + 20.0, y), fg_muted());
                    let dev = app
                        .devices
                        .iter()
                        .find(|d| d.id == app.selected)
                        .map(|d| d.label.clone())
                        .unwrap_or_default();
                    let r1 = p.text(
                        pos2(rect.left() + 34.0, y),
                        egui::Align2::LEFT_CENTER,
                        dev,
                        ui_f(),
                        fg(),
                    );
                    icons::chevron(p, pos2(r1.right() + 10.0, y), false, fg_muted());
                    let tail = app
                        .active_session()
                        .map(|s| {
                            let t = s.term.title();
                            if t.is_empty() {
                                s.label.clone()
                            } else {
                                t
                            }
                        })
                        .unwrap_or_else(|| "no session".into());
                    let gal = elide(
                        p,
                        &tail,
                        mono(11.5),
                        fg_dim(),
                        rect.right() - r1.right() - 40.0,
                    );
                    p.galley(
                        pos2(r1.right() + 20.0, y - gal.size().y / 2.0),
                        gal,
                        fg_dim(),
                    );
                });

            // editor (resizable, right)
            if app.show_editor {
                egui::SidePanel::right("ot_editor")
                    .resizable(true)
                    .default_width(ui.available_width() * 0.45)
                    .width_range(260.0..=ui.available_width() * 0.8)
                    .show_separator_line(true)
                    .frame(Frame::none().fill(bg()))
                    .show_inside(ui, |ui| {
                        if let Some((m, err)) = app.editor.ui(ui) {
                            let until = ui.input(|i| i.time) + 4.0;
                            app.flash = Some((m, err, until));
                        }
                    });
            }

            // terminal
            egui::CentralPanel::default()
                .frame(Frame::none().fill(bg()))
                .show_inside(ui, |ui| {
                    let area = ui.max_rect();
                    let modal = app.modal || app.settings_open || app.binary_file_dialog.is_some();
                    let connection_error = app.connection_errors.get(&app.selected).cloned();
                    match app.active_session() {
                        Some(s) => {
                            if let Some(error) = &s.connection_error {
                                let reconnecting =
                                    app.pending.iter().any(|pending| pending.device == s.device);
                                let error = connection_error.as_ref().unwrap_or(error);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("SSH session ended").color(red()))
                                        .on_hover_text(error);
                                    let label = if reconnecting {
                                        "Reconnecting…"
                                    } else {
                                        "Reconnect"
                                    };
                                    if ui
                                        .add_enabled(!reconnecting, egui::Button::new(label))
                                        .clicked()
                                    {
                                        acts.push(Act::NewSession(s.device.clone()));
                                    }
                                });
                            }
                            let r = Rect::from_min_max(
                                pos2(area.left() + 14.0, ui.next_widget_position().y + 10.0),
                                area.max - vec2(10.0, 6.0),
                            );
                            s.term.ui(ui, r, !modal && s.connection_error.is_none());
                        }
                        None => {
                            let c = area.center();
                            let p = ui.painter();
                            let heading = if connection_error.is_some() {
                                "Connection failed"
                            } else {
                                "No session on this device"
                            };
                            p.text(
                                c - vec2(0.0, 38.0),
                                egui::Align2::CENTER_CENTER,
                                heading,
                                medium(13.0),
                                if connection_error.is_some() {
                                    red()
                                } else {
                                    fg_dim()
                                },
                            );
                            if let Some(error) = &connection_error {
                                let galley = elide(
                                    p,
                                    error,
                                    mono(11.5),
                                    fg_dim(),
                                    (area.width() - 80.0).max(120.0),
                                );
                                let error_rect = Rect::from_center_size(
                                    c - vec2(0.0, 12.0),
                                    galley.size() + vec2(12.0, 8.0),
                                );
                                p.galley(
                                    error_rect.center() - galley.size() / 2.0,
                                    galley,
                                    fg_dim(),
                                );
                                ui.interact(
                                    error_rect,
                                    Id::new(("ot_connection_error", app.selected.as_str())),
                                    Sense::hover(),
                                )
                                .on_hover_text(error);
                            }
                            let btn =
                                Rect::from_center_size(c + vec2(0.0, 28.0), vec2(120.0, 30.0));
                            let resp = ui.interact(btn, Id::new("ot_empty_open"), Sense::click());
                            let p = ui.painter();
                            p.rect(
                                btn,
                                Rounding::same(7.0),
                                if resp.hovered() { bg4() } else { bg3() },
                                Stroke::new(1.0_f32, border_d()),
                            );
                            p.text(
                                btn.center(),
                                egui::Align2::CENTER_CENTER,
                                if connection_error.is_some() {
                                    "Retry"
                                } else {
                                    "Open session"
                                },
                                ui_f(),
                                fg(),
                            );
                            if resp.clicked() {
                                acts.push(Act::NewSession(app.selected.clone()));
                            }
                        }
                    }
                });
        });
}

fn tab(
    ui: &mut Ui,
    at: Pos2,
    id: u64,
    label: &str,
    active: bool,
    live: bool,
    max_width: f32,
) -> (Response, bool, f32) {
    let gal = ui.painter().layout_no_wrap(
        label.to_string(),
        ui_f(),
        if active { fg() } else { fg_dim() },
    );
    let w = (gal.size().x + 56.0).min(220.0).min(max_width.max(70.0));
    let rect = Rect::from_min_size(at, vec2(w, 32.0));
    let resp = ui.interact(rect, Id::new(("ot_tab", id)), Sense::click());
    let xc = pos2(rect.right() - 14.0, rect.center().y - 1.0);
    let xresp = ui.interact(
        Rect::from_center_size(xc, vec2(18.0, 18.0)),
        Id::new(("ot_tabx", id)),
        Sense::click(),
    );
    let p = ui.painter();
    if active {
        p.rect(
            rect,
            Rounding {
                nw: 7.0,
                ne: 7.0,
                sw: 0.0,
                se: 0.0,
            },
            bg(),
            Stroke::NONE,
        );
        p.add(egui::Shape::line(
            vec![rect.left_bottom(), rect.left_top() + vec2(0.0, 7.0)],
            Stroke::new(1.0_f32, border()),
        ));
        p.add(egui::Shape::line(
            vec![rect.right_bottom(), rect.right_top() + vec2(0.0, 7.0)],
            Stroke::new(1.0_f32, border()),
        ));
        p.line_segment(
            [
                rect.left_top() + vec2(7.0, 0.0),
                rect.right_top() - vec2(7.0, 0.0),
            ],
            Stroke::new(1.0_f32, border()),
        );
    } else if resp.hovered() {
        p.rect_filled(
            rect.shrink2(vec2(0.0, 3.0)).translate(vec2(0.0, -2.0)),
            Rounding::same(6.0),
            bg3(),
        );
    }
    let y = rect.center().y - 1.0;
    icons::dot(
        p,
        pos2(rect.left() + 14.0, y),
        3.2,
        if live { signal() } else { fg_muted() },
    );
    let gal = elide(
        p,
        label,
        ui_f(),
        if active { fg() } else { fg_dim() },
        w - 56.0,
    );
    p.galley(pos2(rect.left() + 26.0, y - gal.size().y / 2.0), gal, fg());
    if xresp.hovered() {
        p.rect_filled(
            Rect::from_center_size(xc, vec2(18.0, 18.0)),
            Rounding::same(4.0),
            bg4(),
        );
    }
    if active || resp.hovered() || xresp.hovered() {
        icons::cross(p, xc, 3.2, if xresp.hovered() { fg() } else { fg_muted() });
    }
    (resp, xresp.clicked(), w)
}
