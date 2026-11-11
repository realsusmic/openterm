//! Connection setup, editing, and host key prompts.

use super::widgets::{field, primary_button, seg, terminal_seg};
use super::{
    Act, AuthMode, ConnectionType, HostKeyDialog, RenameDeviceDialog, SshForm, TerminalKind,
};
use crate::icons;
use crate::theme::*;
use egui::{
    pos2, vec2, Color32, Frame, Id, Key, Margin, Response, Rounding, Sense, Stroke, Ui, Vec2,
};

fn connection_card(ui: &mut Ui, connection: ConnectionType, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(76.0, 52.0), Sense::click());
    let painter = ui.painter();
    let hovered = response.hovered();
    painter.rect(
        rect,
        Rounding::same(5.0),
        if selected {
            bg4()
        } else if hovered {
            bg3()
        } else {
            bg()
        },
        Stroke::new(1.0_f32, if selected { fg_muted() } else { border() }),
    );
    let color = if selected || hovered { fg() } else { fg_dim() };
    let center = pos2(rect.center().x, rect.top() + 17.0);
    match connection {
        ConnectionType::File => icons::folder(painter, center, color),
        ConnectionType::Shell | ConnectionType::Serial => icons::prompt(painter, center, color),
        ConnectionType::Wsl => icons::wsl(painter, center, color),
        ConnectionType::Browser => icons::info(painter, center, color),
        _ => icons::server(painter, center, color),
    }
    painter.text(
        pos2(rect.center().x, rect.bottom() - 11.0),
        egui::Align2::CENTER_CENTER,
        connection.label(),
        sans(10.5),
        color,
    );
    response
}

pub(super) fn host_key_dialog(ctx: &egui::Context, prompt: &HostKeyDialog, acts: &mut Vec<Act>) {
    let screen = ctx.screen_rect();
    egui::Area::new(Id::new("ot_host_key_backdrop"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::hover());
            ui.painter()
                .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(180));
        });
    let title = if prompt.changed {
        "SSH host key changed"
    } else {
        "Verify SSH host key"
    };
    egui::Window::new(title)
        .id(Id::new("ot_host_key_dialog"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(480.0);
            if prompt.changed {
                ui.label(
                    egui::RichText::new(format!(
                        "The host key for {} no longer matches the key stored on this computer.",
                        prompt.cfg.host
                    ))
                    .color(red()),
                );
                ui.label(
                    egui::RichText::new(
                        "This can be legitimate after a server reinstall, but it can also indicate a man-in-the-middle attack. Compare both fingerprints through a trusted channel before replacing the stored key.",
                    )
                    .size(11.0)
                    .color(fg_dim()),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Stored SHA-256 fingerprint")
                        .size(11.0)
                        .color(fg_muted()),
                );
                for fingerprint in &prompt.known_fingerprints {
                    ui.label(
                        egui::RichText::new(fingerprint)
                            .monospace()
                            .color(fg()),
                    );
                }
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Presented SHA-256 fingerprint")
                        .size(11.0)
                        .color(fg_muted()),
                );
            } else {
                ui.label(format!(
                    "This is the first connection to {}. Verify the fingerprint before trusting it.",
                    prompt.cfg.host
                ));
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("SHA-256 fingerprint")
                        .size(11.0)
                        .color(fg_muted()),
                );
            }
            ui.label(
                egui::RichText::new(&prompt.fingerprint)
                    .monospace()
                    .color(fg()),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "The approved key will be saved to {}",
                    prompt.known_hosts
                ))
                .size(10.5)
                .color(fg_dim()),
            );
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let accept_label = if prompt.changed {
                    "Replace key and connect"
                } else {
                    "Trust and connect"
                };
                if primary_button(ui, accept_label).clicked() {
                    acts.push(Act::TrustHostKey);
                }
                let decline_label = if prompt.changed { "Decline" } else { "Cancel" };
                if ui.button(decline_label).clicked() {
                    acts.push(Act::RejectHostKey);
                }
            });
        });
    if ctx.input(|input| input.key_pressed(Key::Escape)) {
        acts.push(Act::RejectHostKey);
    }
}

pub(super) fn rename_device_dialog(
    ctx: &egui::Context,
    dialog: &mut RenameDeviceDialog,
    acts: &mut Vec<Act>,
) {
    let screen = ctx.screen_rect();
    egui::Area::new(Id::new("ot_rename_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let response = ui.allocate_rect(screen, Sense::click());
            ui.painter()
                .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(150));
            if response.clicked() {
                acts.push(Act::CancelRenameDevice);
            }
        });
    egui::Area::new(Id::new("ot_rename_dialog"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -20.0))
        .show(ctx, |ui| {
            Frame::none()
                .fill(bg2())
                .stroke(Stroke::new(1.0_f32, border_d()))
                .rounding(Rounding::same(12.0))
                .inner_margin(Margin::same(22.0))
                .show(ui, |ui| {
                    ui.set_width(360.0);
                    ui.label(
                        egui::RichText::new("Rename connection")
                            .font(medium(15.0))
                            .color(fg()),
                    );
                    ui.add_space(8.0);
                    let response = field(ui, "Name", &mut dialog.value, "home server", false);
                    if dialog.focus {
                        response.request_focus();
                        dialog.focus = false;
                    }
                    if let Some(error) = dialog.error.as_ref() {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(error).color(red()));
                    }
                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Rename").clicked() {
                            acts.push(Act::ConfirmRenameDevice);
                        }
                        if ui.button("Cancel").clicked() {
                            acts.push(Act::CancelRenameDevice);
                        }
                    });
                });
        });
    ctx.input(|input| {
        if input.key_pressed(Key::Escape) {
            acts.push(Act::CancelRenameDevice);
        } else if input.key_pressed(Key::Enter) {
            acts.push(Act::ConfirmRenameDevice);
        }
    });
}

pub(super) fn ssh_modal(
    ctx: &egui::Context,
    f: &mut SshForm,
    acts: &mut Vec<Act>,
    vault_ready: bool,
    editing: bool,
) {
    f.port.retain(|c| c.is_ascii_digit());
    f.port.truncate(7);
    if !vault_ready || !f.connection.uses_ssh_auth() {
        f.save_to_vault = false;
    }
    // dim the app behind the dialog
    let screen = ctx.screen_rect();
    egui::Area::new(Id::new("ot_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen, Sense::click());
            ui.painter()
                .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(150));
            if resp.clicked() {
                acts.push(Act::CloseModal);
            }
        });

    egui::Area::new(Id::new("ot_modal"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -20.0))
        .show(ctx, |ui| {
            Frame::none()
                .fill(bg2())
                .stroke(Stroke::new(1.0_f32, border_d()))
                .rounding(Rounding::same(12.0))
                .inner_margin(Margin::same(22.0))
                .show(ui, |ui| {
                    ui.set_width(620.0);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(
                        egui::RichText::new(if editing {
                            "Edit connection"
                        } else {
                            "New connection"
                        })
                        .font(medium(15.0))
                        .color(fg()),
                    );
                    ui.label(
                        egui::RichText::new(if editing {
                            "Update the settings used for future sessions."
                        } else {
                            "Choose a protocol or local session type."
                        })
                        .color(fg_muted()),
                    );
                    ui.add_space(8.0);

                    if editing {
                        ui.label(
                            egui::RichText::new(format!("Protocol · {}", f.connection.label()))
                                .font(medium(12.0))
                                .color(fg_dim()),
                        );
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
                            for connection in ConnectionType::ALL {
                                if connection_card(ui, connection, f.connection == connection)
                                    .clicked()
                                {
                                    let changed = f.connection != connection;
                                    f.connection = connection;
                                    f.port = connection.default_port().into();
                                    f.error = None;
                                    f.save_to_vault = vault_ready && connection.uses_ssh_auth();
                                    if changed
                                        && matches!(
                                            connection,
                                            ConnectionType::Shell | ConnectionType::Wsl
                                        )
                                    {
                                        f.host.clear();
                                        f.user.clear();
                                    }
                                }
                            }
                        });
                    }
                    ui.add_space(10.0);

                    if f.connection == ConnectionType::Shell {
                        ui.label(egui::RichText::new("Terminal").size(11.0).color(fg_muted()));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for terminal in [
                                TerminalKind::PowerShell,
                                TerminalKind::Wsl,
                                TerminalKind::Bash,
                                TerminalKind::Cmd,
                            ] {
                                if terminal_seg(ui, terminal, f.terminal == terminal).clicked() {
                                    f.terminal = terminal;
                                    f.error = None;
                                    if terminal != TerminalKind::Wsl {
                                        f.host.clear();
                                    }
                                }
                            }
                        });
                        ui.add_space(4.0);
                        if f.terminal == TerminalKind::Wsl {
                            let r =
                                field(ui, "Distribution (optional)", &mut f.host, "Ubuntu", false);
                            if f.focus_host {
                                r.request_focus();
                                f.focus_host = false;
                            }
                        } else {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Opens a local {} terminal.",
                                    f.terminal.label()
                                ))
                                .color(fg_dim()),
                            );
                        }
                    } else if f.connection == ConnectionType::Wsl {
                        let r = field(ui, "Distribution (optional)", &mut f.host, "Ubuntu", false);
                        if f.focus_host {
                            r.request_focus();
                            f.focus_host = false;
                        }
                    } else if f.connection == ConnectionType::File {
                        let r = field(ui, "Directory", &mut f.host, "C:\\projects", false);
                        if f.focus_host {
                            r.request_focus();
                            f.focus_host = false;
                        }
                    } else if f.connection == ConnectionType::Browser {
                        let r = field(ui, "URL", &mut f.host, "https://example.com", false);
                        if f.focus_host {
                            r.request_focus();
                            f.focus_host = false;
                        }
                    } else {
                        let host_label = match f.connection {
                            ConnectionType::Serial => "Device",
                            ConnectionType::AwsS3 => "Bucket",
                            _ => "Host",
                        };
                        let host_hint = match f.connection {
                            ConnectionType::Serial if cfg!(windows) => "COM3",
                            ConnectionType::Serial => "/dev/ttyUSB0",
                            ConnectionType::AwsS3 => "my-bucket/path",
                            _ => "192.168.1.20",
                        };
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.vertical(|ui| {
                                ui.set_width(500.0);
                                let r = field(ui, host_label, &mut f.host, host_hint, false);
                                if f.focus_host {
                                    r.request_focus();
                                    f.focus_host = false;
                                }
                            });
                            if !matches!(f.connection, ConnectionType::AwsS3) {
                                ui.vertical(|ui| {
                                    let port_label = if f.connection == ConnectionType::Serial {
                                        "Baud"
                                    } else {
                                        "Port"
                                    };
                                    let _ = field(
                                        ui,
                                        port_label,
                                        &mut f.port,
                                        f.connection.default_port(),
                                        false,
                                    );
                                });
                            }
                        });
                        if matches!(
                            f.connection,
                            ConnectionType::Ssh
                                | ConnectionType::Sftp
                                | ConnectionType::Rsh
                                | ConnectionType::Mosh
                                | ConnectionType::AwsS3
                        ) {
                            let user_label = if f.connection == ConnectionType::AwsS3 {
                                "AWS profile (optional)"
                            } else {
                                "User"
                            };
                            let _ = field(ui, user_label, &mut f.user, "pi", false);
                        }
                    }

                    if f.connection.uses_ssh_auth() {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Authentication")
                                .size(11.0)
                                .color(fg_muted()),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (m, name) in [
                                (AuthMode::Password, "Password"),
                                (AuthMode::Key, "Key file"),
                                (AuthMode::Agent, "Agent"),
                            ] {
                                if seg(ui, name, f.mode == m).clicked() {
                                    f.mode = m;
                                }
                            }
                        });
                        ui.add_space(4.0);
                        match f.mode {
                            AuthMode::Password => {
                                let _ = field(ui, "Password", &mut f.password, "", true);
                            }
                            AuthMode::Key => {
                                let _ = field(
                                    ui,
                                    "Private key path",
                                    &mut f.key_path,
                                    "~/.ssh/id_ed25519",
                                    false,
                                );
                            }
                            AuthMode::Agent => {
                                ui.label(
                                    egui::RichText::new("Uses keys from your running ssh-agent.")
                                        .color(fg_dim()),
                                );
                            }
                        }
                        if !editing {
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.add_enabled_ui(vault_ready, |ui| {
                                    let _ = ui.add(egui::Checkbox::new(
                                        &mut f.save_to_vault,
                                        if vault_ready {
                                            "Keep this machine in the vault"
                                        } else {
                                            "Keep this machine (unlock the vault first)"
                                        },
                                    ));
                                });
                            });
                        }
                        if f.save_to_vault && !editing {
                            ui.add_space(2.0);
                            let _ =
                                field(ui, "Label (optional)", &mut f.label, "home server", false);
                        }
                    }
                    if editing {
                        ui.add_space(6.0);
                        let _ = field(ui, "Name", &mut f.label, "home server", false);
                    }
                    if let Some(e) = &f.error {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(e).color(red()));
                    }

                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let connect = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(if editing {
                                        "Save changes"
                                    } else {
                                        "Connect"
                                    })
                                    .color(bg())
                                    .font(medium(12.5)),
                                )
                                .fill(fg())
                                .stroke(Stroke::NONE)
                                .rounding(7.0)
                                .min_size(vec2(88.0, 30.0)),
                            );
                            let cancel = ui.add(
                                egui::Button::new(egui::RichText::new("Cancel").color(fg_dim()))
                                    .fill(bg3())
                                    .stroke(Stroke::new(1.0_f32, border()))
                                    .rounding(7.0)
                                    .min_size(vec2(76.0, 30.0)),
                            );
                            if connect.clicked() {
                                acts.push(Act::Connect);
                            }
                            if cancel.clicked() {
                                acts.push(Act::CloseModal);
                            }
                        });
                    });
                });
        });

    ctx.input(|i| {
        if i.key_pressed(Key::Escape) {
            acts.push(Act::CloseModal);
        }
        if i.key_pressed(Key::Enter) {
            acts.push(Act::Connect);
        }
    });
}
