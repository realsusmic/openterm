//! File transfer and binary download dialogs.

use super::widgets::field;
use super::{Act, BinaryFileDialog, SyncDirection, TransferDialog, TransferMode};
use crate::theme::*;
use egui::{vec2, Id, Key};

pub(super) fn transfer_dialog(
    ctx: &egui::Context,
    dialog: &mut TransferDialog,
    acts: &mut Vec<Act>,
) {
    let title = match dialog.mode {
        TransferMode::DownloadFile => "Download remote file",
        TransferMode::DownloadFolder => "Download remote folder",
        TransferMode::UploadFile => "Upload local file",
        TransferMode::UploadFolder => "Upload local folder",
        TransferMode::Sync => "Sync project folder",
    };
    egui::Window::new(title)
        .id(Id::new("ot_transfer_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -10.0))
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    "Transfers use the existing SSH/SFTP connection. Nothing is installed on the remote host.",
                )
                .color(fg_muted()),
            );
            ui.add_space(8.0);
            let remote_changed = field(ui, "Remote path", &mut dialog.remote, "", false).changed();
            let local_label = match dialog.mode {
                TransferMode::DownloadFile => "Save as local file",
                TransferMode::UploadFile => "Local file",
                _ => "Local directory",
            };
            let local_changed = field(ui, local_label, &mut dialog.local, "", false).changed();
            if matches!(
                dialog.mode,
                TransferMode::DownloadFolder | TransferMode::UploadFolder | TransferMode::Sync
            ) && ui.button("Browse local folder…").clicked()
            {
                acts.push(Act::TransferPickLocalFolder);
            }

            if dialog.mode == TransferMode::Sync {
                if remote_changed || local_changed {
                    dialog.previewed = false;
                    dialog.changes.clear();
                    dialog.error = None;
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Sync direction").size(11.0).color(fg_muted()));
                ui.horizontal(|ui| {
                    let left = ui
                        .selectable_label(
                            dialog.direction == SyncDirection::LocalToRemote,
                            "LOCAL  ─────▶  REMOTE",
                        )
                        .on_hover_text("Upload added and changed files");
                    let right = ui
                        .selectable_label(
                            dialog.direction == SyncDirection::RemoteToLocal,
                            "LOCAL  ◀─────  REMOTE",
                        )
                        .on_hover_text("Download added and changed files");
                    if left.clicked() {
                        dialog.direction = SyncDirection::LocalToRemote;
                        dialog.previewed = false;
                        dialog.changes.clear();
                    }
                    if right.clicked() {
                        dialog.direction = SyncDirection::RemoteToLocal;
                        dialog.previewed = false;
                        dialog.changes.clear();
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Preview compares file sizes and modified times without downloading every file. Sync adds and updates files; it never deletes destination files.",
                    )
                    .size(11.0)
                    .color(fg_muted()),
                );
                if dialog.busy {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new("Scanning or syncing folders over SSH…")
                                .size(11.0)
                                .color(fg_dim()),
                        );
                    });
                }
                if dialog.previewed {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} changed or added file{}",
                            dialog.changes.len(),
                            if dialog.changes.len() == 1 { "" } else { "s" }
                        ))
                        .color(fg()),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for change in &dialog.changes {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(change.kind.to_ascii_uppercase())
                                            .size(10.0)
                                            .color(if change.kind == "add" { signal() } else { fg_dim() }),
                                    );
                                    ui.label(egui::RichText::new(&change.path).font(mono(11.0)));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("{} B", change.size))
                                                    .size(10.0)
                                                    .color(fg_muted()),
                                            );
                                        },
                                    );
                                });
                            }
                        });
                }
            }

            if let Some(error) = &dialog.error {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(error).color(red()));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!dialog.busy, egui::Button::new("Cancel"))
                    .clicked()
                {
                    acts.push(Act::TransferClose);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if dialog.mode == TransferMode::Sync {
                        let apply_label = if dialog.busy {
                            "Working…".to_string()
                        } else {
                            format!("Sync {} files", dialog.changes.len())
                        };
                        if ui
                            .add_enabled(
                                dialog.previewed && !dialog.busy && !dialog.changes.is_empty(),
                                egui::Button::new(apply_label),
                            )
                            .clicked()
                        {
                            acts.push(Act::TransferApply);
                        }
                        if ui
                            .add_enabled(!dialog.busy, egui::Button::new("Preview changes"))
                            .clicked()
                        {
                            acts.push(Act::TransferPreview);
                        }
                    } else if ui
                        .add_enabled(
                            !dialog.busy,
                            egui::Button::new(if dialog.busy { "Working…" } else { "Start" }),
                        )
                        .clicked()
                    {
                        acts.push(Act::TransferApply);
                    }
                });
            });
        });
    if ctx.input(|input| input.key_pressed(Key::Escape)) && !dialog.busy {
        acts.push(Act::TransferClose);
    }
}

pub(super) fn binary_file_dialog(
    ctx: &egui::Context,
    dialog: &mut BinaryFileDialog,
    acts: &mut Vec<Act>,
) {
    egui::Window::new("This file isn't text")
        .id(Id::new("ot_binary_file_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -10.0))
        .collapsible(false)
        .resizable(false)
        .default_width(540.0)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    "OpenTerm can't display this file as text. You can download it, cancel, or download it and inspect the bytes as hex.",
                )
                .color(fg_muted()),
            );
            ui.add_space(8.0);
            ui.label(egui::RichText::new(&dialog.remote).font(mono(11.0)).color(fg()));
            ui.add_space(6.0);
            field(
                ui,
                "Save as local file",
                &mut dialog.local,
                "Local destination",
                false,
            );
            if let Some(error) = &dialog.error {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(error).color(red()));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    acts.push(Act::BinaryCancel);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Download and view as hex").clicked() {
                        acts.push(Act::BinaryDownload(true));
                    }
                    if ui.button("Download").clicked() {
                        acts.push(Act::BinaryDownload(false));
                    }
                });
            });
        });
    if ctx.input(|input| input.key_pressed(Key::Escape)) {
        acts.push(Act::BinaryCancel);
    }
}
