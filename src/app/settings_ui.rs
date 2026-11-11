//! Appearance, terminal, and vault preferences.

use super::widgets::{primary_button, seg, terminal_seg};
use super::{resolved_terminal, Act, TerminalKind};
use crate::settings::{Settings, ThemeMode, VaultGraceUnit};
use crate::theme::*;
use egui::{vec2, Color32, Frame, Id, Key, Margin, Rounding, Sense, Stroke};

pub(super) fn settings_dialog(
    ctx: &egui::Context,
    settings: &Settings,
    available_terminals: &[TerminalKind],
    acts: &mut Vec<Act>,
) {
    let screen = ctx.screen_rect();
    egui::Area::new(Id::new("ot_settings_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let response = ui.allocate_rect(screen, Sense::click());
            ui.painter()
                .rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(150));
            if response.clicked() {
                acts.push(Act::CloseSettings);
            }
        });

    egui::Area::new(Id::new("ot_settings_dialog"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -20.0))
        .show(ctx, |ui| {
            Frame::none()
                .fill(bg2())
                .stroke(Stroke::new(1.0_f32, border_d()))
                .rounding(Rounding::same(12.0))
                .inner_margin(Margin::same(22.0))
                .show(ui, |ui| {
                    ui.set_width(430.0);
                    ui.label(
                        egui::RichText::new("Settings")
                            .font(medium(15.0))
                            .color(fg()),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Appearance, terminal defaults, and vault security.",
                        )
                        .color(fg_muted()),
                    );
                    ui.add_space(14.0);

                    ui.label(
                        egui::RichText::new("Appearance")
                            .size(11.0)
                            .color(fg_muted()),
                    );
                    ui.horizontal(|ui| {
                        if seg(ui, "Dark", settings.theme == ThemeMode::Dark).clicked() {
                            acts.push(Act::SetTheme(ThemeMode::Dark));
                        }
                        if seg(ui, "Light", settings.theme == ThemeMode::Light).clicked() {
                            acts.push(Act::SetTheme(ThemeMode::Light));
                        }
                    });

                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("Default shell for Ctrl+T")
                            .size(11.0)
                            .color(fg_muted()),
                    );
                    let selected = resolved_terminal(&settings.default_shell, available_terminals);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                        for &terminal in available_terminals {
                            if terminal_seg(ui, terminal, selected == terminal).clicked() {
                                acts.push(Act::SetDefaultShell(terminal));
                            }
                        }
                    });
                    if available_terminals.is_empty() {
                        ui.label(
                            egui::RichText::new("No supported shell was detected on PATH.")
                                .size(10.5)
                                .color(red()),
                        );
                    }

                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("Remember vault unlock")
                            .size(11.0)
                            .color(fg_muted()),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Keep the derived key in the OS credential store for this long. The master password is never saved.",
                        )
                        .size(10.5)
                        .color(fg_dim()),
                    );
                    ui.add_space(5.0);
                    let original_unit = settings.vault_grace_unit;
                    let original_value = settings.vault_grace_value;
                    let mut next_unit = original_unit;
                    let mut next_value = original_value;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                        for (unit, label) in [
                            (VaultGraceUnit::Day, "Days"),
                            (VaultGraceUnit::Week, "Weeks"),
                            (VaultGraceUnit::Month, "Months"),
                            (VaultGraceUnit::Year, "Years"),
                        ] {
                            if seg(ui, label, next_unit == unit).clicked() {
                                next_unit = unit;
                                next_value = next_value.max(1).min(unit.max_value());
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Amount").size(11.0).color(fg_muted()));
                        ui.add(
                            egui::DragValue::new(&mut next_value)
                                .range(1..=next_unit.max_value())
                                .speed(1.0),
                        );
                        let limit = match next_unit {
                            VaultGraceUnit::Day => "1–7 days",
                            VaultGraceUnit::Week => "1–4 weeks",
                            VaultGraceUnit::Month => "1–12 months",
                            VaultGraceUnit::Year => "1+ years",
                        };
                        ui.label(egui::RichText::new(limit).size(10.5).color(fg_dim()));
                    });
                    next_value = next_value.max(1).min(next_unit.max_value());
                    if next_unit != original_unit || next_value != original_value {
                        acts.push(Act::SetVaultGrace(next_value, next_unit));
                    }

                    ui.add_space(12.0);
                    let location = Settings::path()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| "settings path unavailable".into());
                    ui.label(
                        egui::RichText::new(if cfg!(feature = "portable") {
                            format!("Portable settings: {location}")
                        } else {
                            format!("Settings: {location}")
                        })
                        .size(10.5)
                        .color(fg_muted()),
                    );

                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Done").clicked() {
                            acts.push(Act::CloseSettings);
                        }
                    });
                });
        });

    if ctx.input(|input| input.key_pressed(Key::Escape)) {
        acts.push(Act::CloseSettings);
    }
}
