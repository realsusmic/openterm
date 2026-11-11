//! Device list and local and remote file trees.

use super::widgets::{elide, icon_btn, row, section, ui_f};
use super::{Act, DeviceKind, OpenTerm};
use crate::fs_tree::{Node, RemoteNode};
use crate::icons;
use crate::theme::*;
use egui::{pos2, vec2, Color32, Frame, Id, Rect, Sense, Stroke, Ui};

pub(super) fn sidebar(ctx: &egui::Context, app: &mut OpenTerm, acts: &mut Vec<Act>) {
    egui::SidePanel::left("ot_side_v2")
        .resizable(true)
        .default_width(240.0)
        .width_range(200.0..=380.0)
        .show_separator_line(true)
        .frame(Frame::none().fill(bg2()))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add_space(8.0);
            section(ui, "DEVICES", |ui| {
                if icon_btn(ui, "add device", |p, c, col| icons::plus(p, c, 4.5, col)).clicked() {
                    acts.push(Act::OpenModal);
                }
            });

            for d in &app.devices {
                let selected = app.selected == d.id;
                let live = app.device_live(&d.id);
                let pending = app.pending.iter().any(|p| p.device == d.id);
                let removable = !matches!(d.kind, DeviceKind::Local);
                let popup_id = Id::new(("device_menu", d.id.as_str()));
                let menu_open = ui.memory(|memory| memory.is_popup_open(popup_id));
                let resp = row(ui, 32.0, selected, |p, r, hovered| {
                    let ic = pos2(r.left() + 16.0, r.center().y);
                    let failed = app.connection_errors.contains_key(&d.id);
                    let col = if selected || hovered { fg() } else { fg_dim() };
                    match &d.kind {
                        DeviceKind::Local => icons::laptop(p, ic, col),
                        _ => icons::server(p, ic, col),
                    }
                    let gal = elide(p, &d.label, ui_f(), col, r.width() - 56.0);
                    p.galley(
                        pos2(r.left() + 32.0, r.center().y - gal.size().y / 2.0),
                        gal,
                        col,
                    );
                    let dc = pos2(r.right() - 14.0, r.center().y);
                    if removable && (hovered || menu_open) {
                        icons::more(p, dc, if selected { fg() } else { fg_dim() });
                    } else if pending {
                        let t = p.ctx().input(|i| i.time) as f32;
                        icons::dot(
                            p,
                            dc,
                            3.5,
                            fg_dim().gamma_multiply(0.5 + 0.5 * (t * 4.0).sin().abs()),
                        );
                        p.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(60));
                    } else {
                        icons::dot(
                            p,
                            dc,
                            3.5,
                            if failed {
                                red()
                            } else if live {
                                signal()
                            } else {
                                fg_muted()
                            },
                        );
                    }
                });
                let menu_response = removable.then(|| {
                    let menu_rect = Rect::from_center_size(
                        pos2(resp.rect.right() - 14.0, resp.rect.center().y),
                        vec2(26.0, 28.0),
                    );
                    ui.interact(
                        menu_rect,
                        Id::new(("device_menu_button", d.id.as_str())),
                        Sense::click(),
                    )
                    .on_hover_text("Connection actions")
                });
                let menu_clicked = menu_response
                    .as_ref()
                    .is_some_and(|response| response.clicked());
                if menu_clicked {
                    ui.memory_mut(|memory| memory.toggle_popup(popup_id));
                }
                let mut menu_action = None;
                if let Some(menu_response) = menu_response.as_ref() {
                    let _ = egui::popup::popup_above_or_below_widget(
                        ui,
                        popup_id,
                        menu_response,
                        egui::AboveOrBelow::Below,
                        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(150.0);
                            if ui.button("Edit connection").clicked() {
                                menu_action = Some(Act::EditDevice(d.id.clone()));
                            }
                            if ui.button("Rename").clicked() {
                                menu_action = Some(Act::RenameDevice(d.id.clone()));
                            }
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("Delete").color(red()),
                                ))
                                .clicked()
                            {
                                menu_action = Some(Act::DeleteDevice(d.id.clone()));
                            }
                        },
                    );
                }
                if let Some(action) = menu_action {
                    ui.memory_mut(|memory| memory.close_popup());
                    acts.push(action);
                } else if resp.double_clicked() {
                    acts.push(Act::NewSession(d.id.clone()));
                } else if resp.clicked() && !menu_clicked {
                    acts.push(Act::SelectDevice(d.id.clone()));
                }
                if !menu_clicked {
                    if let Some(error) = app.connection_errors.get(&d.id) {
                        resp.on_hover_text(format!(
                            "Connection failed: {error}\n\nDouble-click to retry"
                        ));
                    } else {
                        resp.on_hover_text("double-click for a new session");
                    }
                }
            }
            let add = row(ui, 30.0, false, |p, r, hovered| {
                let col = if hovered { fg_dim() } else { fg_muted() };
                icons::plus(p, pos2(r.left() + 16.0, r.center().y), 4.0, col);
                p.text(
                    pos2(r.left() + 32.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Add device",
                    ui_f(),
                    col,
                );
            });
            if add.clicked() {
                acts.push(Act::OpenModal);
            }

            ui.add_space(10.0);
            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().line_segment(
                [line.left_center(), line.right_center()],
                Stroke::new(1.0_f32, border()),
            );
            ui.add_space(8.0);

            let remote_session = app.active.and_then(|id| {
                app.sessions
                    .iter()
                    .position(|s| s.id == id && s.device == app.selected && s.remote_tree.is_some())
            });
            if let Some(index) = remote_session {
                let session_id = app.sessions[index].id;
                section(ui, "REMOTE FILES", |ui| {
                    if icon_btn(ui, "refresh remote files", icons::refresh).clicked() {
                        acts.push(Act::RemoteRefresh(session_id));
                    }
                    if icon_btn(ui, "parent folder", icons::up).clicked() {
                        acts.push(Act::RemoteUp(session_id));
                    }
                });
                let tree = app.sessions[index].remote_tree.as_mut().unwrap();
                let root_label = tree.path.clone();
                let r = row(ui, 26.0, false, |p, r, _| {
                    let gal = elide(p, &root_label, mono(11.5), fg_muted(), r.width() - 24.0);
                    p.galley(
                        pos2(r.left() + 12.0, r.center().y - gal.size().y / 2.0),
                        gal,
                        fg_muted(),
                    );
                });
                let r = r.on_hover_text(format!("Remote: {}", tree.path));
                let root_path = tree.path.clone();
                r.context_menu(|ui| {
                    if ui.button("Download folder…").clicked() {
                        acts.push(Act::RemoteDownload(session_id, root_path.clone(), true));
                        ui.close_menu();
                    }
                    if ui.button("Upload file here…").clicked() {
                        acts.push(Act::RemoteUpload(session_id, root_path.clone(), false));
                        ui.close_menu();
                    }
                    if ui.button("Upload folder here…").clicked() {
                        acts.push(Act::RemoteUpload(session_id, root_path.clone(), true));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Sync folder…").clicked() {
                        acts.push(Act::RemoteSync(session_id, root_path.clone()));
                        ui.close_menu();
                    }
                });

                egui::ScrollArea::vertical()
                    .id_salt(("ot_remote_tree", session_id))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if tree.loading {
                            tree_status(ui, "Loading remote files…", fg_muted());
                        } else if let Some(error) = &tree.error {
                            tree_status(ui, error, red());
                        } else if let Some(children) = tree.children.as_mut() {
                            if children.is_empty() {
                                tree_status(ui, "Empty folder", fg_muted());
                            }
                            for child in children.iter_mut() {
                                remote_tree_node(ui, child, 0, session_id, acts);
                            }
                        }
                        ui.add_space(12.0);
                    });
            } else {
                section(ui, "FILES", |ui| {
                    if icon_btn(ui, "refresh", icons::refresh).clicked() {
                        acts.push(Act::RefreshTree);
                    }
                    if icon_btn(ui, "parent folder", icons::up).clicked() {
                        acts.push(Act::RootUp);
                    }
                });

                // current root as a breadcrumb-ish header
                let root_label = contract_home(&app.tree.path);
                let r = row(ui, 26.0, false, |p, r, _| {
                    let gal = elide(p, &root_label, mono(11.5), fg_muted(), r.width() - 24.0);
                    p.galley(
                        pos2(r.left() + 12.0, r.center().y - gal.size().y / 2.0),
                        gal,
                        fg_muted(),
                    );
                });
                r.on_hover_text(app.tree.path.display().to_string());

                egui::ScrollArea::vertical()
                    .id_salt("ot_tree")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        app.tree.ensure_loaded();
                        if let Some(children) = app.tree.children.as_mut() {
                            if children.is_empty() {
                                tree_status(ui, "Empty folder", fg_muted());
                            }
                            for c in children.iter_mut() {
                                tree_node(ui, c, 0, acts);
                            }
                        }
                        ui.add_space(12.0);
                    });
            }
        });
}

fn tree_node(ui: &mut Ui, n: &mut Node, depth: usize, acts: &mut Vec<Act>) {
    let indent = 10.0 + depth as f32 * 14.0;
    let resp = row(ui, 26.0, false, |p, r, hovered| {
        let y = r.center().y;
        let x = r.left() + indent;
        if n.is_dir {
            icons::chevron(p, pos2(x + 4.0, y), n.expanded, fg_muted());
        }
        let col = if hovered {
            fg()
        } else if n.is_dir {
            fg()
        } else {
            fg_dim()
        };
        if n.is_dir {
            icons::folder(
                p,
                pos2(x + 18.0, y),
                if hovered { fg_dim() } else { fg_muted() },
            );
        } else {
            icons::file(p, pos2(x + 18.0, y), fg_muted());
        }
        let gal = elide(p, &n.name, ui_f(), col, r.right() - (x + 30.0) - 8.0);
        p.galley(pos2(x + 30.0, y - gal.size().y / 2.0), gal, col);
    });
    if resp.double_clicked() && n.is_dir {
        acts.push(Act::SetRoot(n.path.clone()));
    } else if resp.clicked() {
        if n.is_dir {
            n.expanded = !n.expanded;
        } else {
            acts.push(Act::OpenFile(n.path.clone()));
        }
    }
    if n.is_dir && n.expanded {
        n.ensure_loaded();
        if let Some(ch) = n.children.as_mut() {
            for c in ch.iter_mut() {
                tree_node(ui, c, depth + 1, acts);
            }
        }
    }
}

fn tree_status(ui: &mut Ui, text: &str, color: Color32) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.add(egui::Label::new(egui::RichText::new(text).color(color)).wrap());
    });
}

fn remote_tree_node(
    ui: &mut Ui,
    n: &mut RemoteNode,
    depth: usize,
    session_id: u64,
    acts: &mut Vec<Act>,
) {
    let indent = 10.0 + depth as f32 * 14.0;
    let resp = row(ui, 26.0, false, |p, r, hovered| {
        let y = r.center().y;
        let x = r.left() + indent;
        if n.is_dir {
            icons::chevron(p, pos2(x + 4.0, y), n.expanded, fg_muted());
        }
        let col = if hovered {
            fg()
        } else if n.is_dir {
            fg()
        } else {
            fg_dim()
        };
        if n.is_dir {
            icons::folder(
                p,
                pos2(x + 18.0, y),
                if hovered { fg_dim() } else { fg_muted() },
            );
        } else {
            icons::file(p, pos2(x + 18.0, y), fg_muted());
        }
        let gal = elide(p, &n.name, ui_f(), col, r.right() - (x + 30.0) - 8.0);
        p.galley(pos2(x + 30.0, y - gal.size().y / 2.0), gal, col);
    });
    let resp = resp.on_hover_text(format!("Remote: {}", n.path));
    resp.context_menu(|ui| {
        if n.is_dir {
            if ui.button("Download folder…").clicked() {
                acts.push(Act::RemoteDownload(session_id, n.path.clone(), true));
                ui.close_menu();
            }
            if ui.button("Upload file here…").clicked() {
                acts.push(Act::RemoteUpload(session_id, n.path.clone(), false));
                ui.close_menu();
            }
            if ui.button("Upload folder here…").clicked() {
                acts.push(Act::RemoteUpload(session_id, n.path.clone(), true));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Sync folder…").clicked() {
                acts.push(Act::RemoteSync(session_id, n.path.clone()));
                ui.close_menu();
            }
        } else if ui.button("Download file…").clicked() {
            acts.push(Act::RemoteDownload(session_id, n.path.clone(), false));
            ui.close_menu();
        }
    });
    if resp.double_clicked() && n.is_dir {
        acts.push(Act::RemoteSetRoot(session_id, n.path.clone()));
    } else if resp.clicked() {
        if n.is_dir {
            n.expanded = !n.expanded;
            if n.expanded && n.children.is_none() && !n.loading {
                acts.push(Act::RemoteLoad(session_id, n.path.clone()));
            }
        } else {
            acts.push(Act::RemoteOpen(session_id, n.path.clone(), n.size));
        }
    }
    if n.is_dir && n.expanded {
        if n.loading {
            tree_status(ui, "Loading…", fg_muted());
        } else if let Some(error) = &n.error {
            tree_status(ui, error, red());
        } else if let Some(children) = n.children.as_mut() {
            for child in children.iter_mut() {
                remote_tree_node(ui, child, depth + 1, session_id, acts);
            }
        }
    }
}

fn contract_home(p: &std::path::Path) -> String {
    if let Some(h) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&h) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display()).replace('\\', "/")
            };
        }
    }
    p.display().to_string()
}
