//! Vault setup, unlock, and reset dialogs.

use super::widgets::{field, primary_button};
use super::{Act, VaultState};
use crate::theme::*;
use crate::vault::Mode as VaultMode;
use egui::{pos2, vec2, Color32, Frame, Id, Key, Margin, Response, Rounding, Sense, Stroke, Ui};

pub(super) fn vault_dialog(ctx: &egui::Context, state: &mut VaultState, acts: &mut Vec<Act>) {
    let is_setup = matches!(state, VaultState::Setup { .. });
    let is_unlock = matches!(state, VaultState::Unlock { .. });
    let is_keystore_error = matches!(state, VaultState::KeystoreError { .. });
    let is_reset = matches!(state, VaultState::ResetConfirm { .. });
    if !is_setup && !is_unlock && !is_keystore_error && !is_reset {
        return;
    }

    let screen = ctx.screen_rect();
    egui::Area::new(Id::new("ot_vault_back"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::hover());
            ui.painter()
                .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(170));
        });

    egui::Area::new(Id::new("ot_vault"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -20.0))
        .show(ctx, |ui| {
            Frame::none()
                .fill(bg2())
                .stroke(Stroke::new(1.0_f32, border_d()))
                .rounding(Rounding::same(12.0))
                .inner_margin(Margin::same(22.0))
                .show(ui, |ui| {
                    ui.set_width(420.0);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    match state {
                        VaultState::Setup {
                            choice,
                            pw,
                            pw2,
                            err,
                        } => render_setup(ui, choice, pw, pw2, err, acts),
                        VaultState::Unlock { pw, err, focus } => {
                            render_unlock(ui, pw, err, focus, acts)
                        }
                        VaultState::KeystoreError { err } => render_keystore_error(ui, err, acts),
                        VaultState::ResetConfirm {
                            confirm,
                            err,
                            focus,
                        } => render_vault_reset(ui, confirm, err, focus, acts),
                        _ => {}
                    }
                });
        });

    ctx.input(|i| {
        if i.key_pressed(Key::Enter) {
            if is_setup {
                acts.push(Act::VaultCreate);
            }
            if is_unlock {
                acts.push(Act::VaultUnlock);
            }
            if is_keystore_error {
                acts.push(Act::VaultOsRetry);
            }
            if is_reset {
                acts.push(Act::VaultReset);
            }
        }
    });
}

fn render_keystore_error(ui: &mut Ui, err: &str, acts: &mut Vec<Act>) {
    ui.label(
        egui::RichText::new("OS sign-in unavailable")
            .font(medium(15.0))
            .color(fg()),
    );
    ui.label(
        egui::RichText::new(
            "This vault uses the operating system's credential store and does not have a master password.",
        )
        .color(fg_muted()),
    );
    ui.add_space(10.0);
    ui.label(egui::RichText::new(err).color(red()));
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary_button(ui, "Retry OS sign-in").clicked() {
                acts.push(Act::VaultOsRetry);
            }
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Reset vault").color(fg_dim()))
                        .frame(false),
                )
                .clicked()
            {
                acts.push(Act::VaultResetBegin);
            }
        });
    });
}

fn render_setup(
    ui: &mut Ui,
    choice: &mut VaultMode,
    pw: &mut String,
    pw2: &mut String,
    err: &mut Option<String>,
    acts: &mut Vec<Act>,
) {
    ui.label(
        egui::RichText::new("Set up credential vault")
            .font(medium(15.0))
            .color(fg()),
    );
    ui.label(egui::RichText::new("OpenTerm stores SSH credentials locally, encrypted with XChaCha20-Poly1305. The derived key is never written to disk.").color(fg_muted()));
    ui.add_space(10.0);

    ui.label(
        egui::RichText::new("How should the vault be unlocked?")
            .size(11.0)
            .color(fg_muted()),
    );
    ui.add_space(4.0);
    ui.vertical(|ui| {
        if radio_card(ui, "Use the OS keystore", "Automatic. A random key is stored in Windows DPAPI / macOS Keychain / Linux Secret Service. Anyone with access to your logged-in session can read the vault.", *choice == VaultMode::Os).clicked() {
            acts.push(Act::VaultChooseMode(VaultMode::Os));
        }
        ui.add_space(4.0);
        if radio_card(ui, "Set a master password", "Argon2id-derived. Nothing on disk unlocks the vault without the password — losing it means losing the saved credentials.", *choice == VaultMode::Password).clicked() {
            acts.push(Act::VaultChooseMode(VaultMode::Password));
        }
    });

    if *choice == VaultMode::Password {
        ui.add_space(10.0);
        let _ = field(ui, "Master password", pw, "min 8 characters", true);
        let _ = field(ui, "Confirm", pw2, "", true);
    }
    if let Some(e) = err {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(e.as_str()).color(red()));
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary_button(ui, "Create vault").clicked() {
                acts.push(Act::VaultCreate);
            }
        });
    });
}

fn render_unlock(
    ui: &mut Ui,
    pw: &mut String,
    err: &mut Option<String>,
    focus: &mut bool,
    acts: &mut Vec<Act>,
) {
    ui.label(
        egui::RichText::new("Unlock credential vault")
            .font(medium(15.0))
            .color(fg()),
    );
    ui.label(
        egui::RichText::new("Enter your master password to decrypt saved SSH credentials.")
            .color(fg_muted()),
    );
    ui.add_space(10.0);
    let r = field(ui, "Master password", pw, "", true);
    if *focus {
        r.request_focus();
        *focus = false;
    }
    if let Some(e) = err {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(e.as_str()).color(red()));
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary_button(ui, "Unlock").clicked() {
                acts.push(Act::VaultUnlock);
            }
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Forgot password?").color(fg_dim()))
                        .frame(false),
                )
                .clicked()
            {
                acts.push(Act::VaultResetBegin);
            }
        });
    });
}

fn render_vault_reset(
    ui: &mut Ui,
    confirm: &mut String,
    err: &mut Option<String>,
    focus: &mut bool,
    acts: &mut Vec<Act>,
) {
    ui.label(
        egui::RichText::new("Reset credential vault")
            .font(medium(15.0))
            .color(fg()),
    );
    ui.label(egui::RichText::new("A forgotten master password cannot be recovered. Resetting deletes all saved SSH credentials, but does not affect active connections or files on any device.").color(fg_muted()));
    ui.add_space(10.0);
    ui.label(egui::RichText::new("Type RESET to permanently delete the vault.").color(red()));
    let r = field(ui, "Confirmation", confirm, "RESET", false);
    if *focus {
        r.request_focus();
        *focus = false;
    }
    if let Some(e) = err {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(e.as_str()).color(red()));
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Delete vault").color(fg()))
                        .fill(bg4())
                        .stroke(Stroke::new(1.0_f32, red()))
                        .rounding(7.0)
                        .min_size(vec2(100.0, 30.0)),
                )
                .clicked()
            {
                acts.push(Act::VaultReset);
            }
            if ui.button("Cancel").clicked() {
                acts.push(Act::VaultResetCancel);
            }
        });
    });
}

fn radio_card(ui: &mut Ui, title: &str, body: &str, selected: bool) -> Response {
    let body_job = {
        let mut job = egui::text::LayoutJob::single_section(
            body.into(),
            egui::text::TextFormat {
                font_id: sans(11.5),
                color: fg_muted(),
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping {
            max_width: (ui.available_width() - 54.0).max(80.0),
            max_rows: usize::MAX,
            break_anywhere: false,
            overflow_character: None,
        };
        ui.painter().layout_job(job)
    };
    // Measure the wrapped copy instead of clipping it into a fixed-height card.
    let card_h = (body_job.size().y + 43.0).max(72.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), card_h), Sense::click());
    let p = ui.painter();
    let bg = if selected {
        bg3()
    } else if resp.hovered() {
        bg4()
    } else {
        bg2()
    };
    let stroke = Stroke::new(1.0_f32, if selected { fg_dim() } else { border() });
    p.rect(rect, Rounding::same(8.0), bg, stroke);
    let dot_c = pos2(rect.left() + 18.0, rect.top() + 18.0);
    p.circle_stroke(
        dot_c,
        6.0,
        Stroke::new(1.2_f32, if selected { fg() } else { fg_muted() }),
    );
    if selected {
        p.circle_filled(dot_c, 3.0, fg());
    }
    p.text(
        pos2(rect.left() + 36.0, rect.top() + 11.0),
        egui::Align2::LEFT_TOP,
        title,
        medium(13.0),
        fg(),
    );
    p.galley(
        pos2(rect.left() + 36.0, rect.top() + 30.0),
        body_job,
        fg_muted(),
    );
    resp
}
