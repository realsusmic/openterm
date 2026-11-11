//! Appearance, terminal, and vault preferences.

use super::terminal_fonts::TerminalFonts;
use super::widgets::{primary_button, seg, terminal_seg};
use super::{resolved_terminal, Act, TerminalKind};
use crate::settings::{Settings, ThemeMode, VaultGraceUnit};
use crate::theme::*;
use egui::{vec2, Color32, Frame, Id, Key, Margin, Rounding, Sense, Stroke};

pub(super) fn settings_dialog(
    ctx: &egui::Context,
    settings: &Settings,
    available_terminals: &[TerminalKind],
    terminal_fonts: &TerminalFonts,
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
                        egui::RichText::new("Appearance, terminal defaults, and vault security.")
                            .color(fg_muted()),
                    );
                    ui.add_space(14.0);

                    egui::ScrollArea::vertical()
                        .id_salt("settings_body")
                        .auto_shrink([false, true])
                        .max_height((screen.height() - 160.0).max(100.0))
                        .show(ui, |ui| {
                            settings_body(ui, settings, available_terminals, terminal_fonts, acts);
                        });

                    ui.add_space(14.0);
                    settings_footer(ui, acts);
                });
        });

    if ctx.input(|input| input.key_pressed(Key::Escape)) {
        acts.push(Act::CloseSettings);
    }
}

fn settings_footer(ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    ui.horizontal(|ui| {
        let icon = egui::Image::new(egui::include_image!("../../assets/icons/github.svg"))
            .fit_to_exact_size(vec2(18.0, 18.0))
            .tint(fg());
        if ui
            .add(egui::Button::image(icon))
            .on_hover_text("Open OpenTerm on GitHub")
            .clicked()
        {
            ui.ctx().open_url(egui::OpenUrl::new_tab(
                "https://github.com/realsusmic/openterm",
            ));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary_button(ui, "Done").clicked() {
                acts.push(Act::CloseSettings);
            }
        });
    });
}

fn settings_body(
    ui: &mut egui::Ui,
    settings: &Settings,
    available_terminals: &[TerminalKind],
    terminal_fonts: &TerminalFonts,
    acts: &mut Vec<Act>,
) {
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
    terminal_font_picker(ui, settings.terminal_font.as_deref(), terminal_fonts, acts);

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
}

fn terminal_font_picker(
    ui: &mut egui::Ui,
    selected: Option<&str>,
    fonts: &TerminalFonts,
    acts: &mut Vec<Act>,
) {
    const DEFAULT: &str = "JetBrains Mono (bundled)";
    ui.label(
        egui::RichText::new("Terminal font")
            .size(11.0)
            .color(fg_muted()),
    );
    let label = match selected {
        Some(name)
            if !matches!(fonts, TerminalFonts::Scanning(_))
                && !fonts
                    .catalog()
                    .is_some_and(|catalog| catalog.names().any(|family| family == name)) =>
        {
            format!("{name} (unavailable)")
        }
        Some(name) => name.to_owned(),
        None => DEFAULT.to_owned(),
    };
    let button = ui.add(egui::Button::new(format!("{label}  ▾")).min_size(vec2(250.0, 26.0)));
    let popup_id = ui.make_persistent_id("terminal_font_popup");
    if button.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    }
    egui::popup::popup_above_or_below_widget(
        ui,
        popup_id,
        &button,
        egui::AboveOrBelow::Below,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            let choose = |ui: &egui::Ui, acts: &mut Vec<Act>, family| {
                acts.push(Act::SetTerminalFont(family));
                ui.memory_mut(|memory| memory.close_popup());
            };
            if ui.selectable_label(selected.is_none(), DEFAULT).clicked() {
                choose(ui, acts, None);
            }
            match fonts {
                TerminalFonts::Scanning(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Scanning installed fonts…");
                    });
                }
                TerminalFonts::Ready(catalog) => {
                    let id = popup_id.with("search");
                    let mut search = ui
                        .data_mut(|data| data.get_temp::<String>(id))
                        .unwrap_or_default();
                    ui.add(
                        egui::TextEdit::singleline(&mut search).hint_text("Search installed fonts"),
                    );
                    let query = search.trim().to_lowercase();
                    ui.data_mut(|data| data.insert_temp(id, search));
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            let mut found = false;
                            for name in catalog
                                .names()
                                .filter(|name| name.to_lowercase().contains(&query))
                            {
                                found = true;
                                if ui.selectable_label(selected == Some(name), name).clicked() {
                                    choose(ui, acts, Some(name.to_owned()));
                                }
                            }
                            if !found {
                                ui.label("No matching monospace fonts");
                            }
                        });
                }
                TerminalFonts::Unavailable => {
                    ui.label("Installed fonts are unavailable");
                }
            }
        },
    );
}
