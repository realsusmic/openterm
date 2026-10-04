//! Persistent user settings. Portable builds keep this INI beside the exe;
//! regular builds use the operating system's configuration directory.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultGraceUnit {
    Day,
    Week,
    Month,
    Year,
}

impl VaultGraceUnit {
    pub fn max_value(self) -> u64 {
        match self {
            Self::Day => 7,
            Self::Week => 4,
            Self::Month => 12,
            Self::Year => u64::MAX,
        }
    }

    pub fn seconds(self, value: u64) -> u64 {
        let days = match self {
            Self::Day => 1,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        };
        value.max(1).saturating_mul(days).saturating_mul(86_400)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeMode,
    pub default_shell: String,
    pub vault_grace_value: u64,
    pub vault_grace_unit: VaultGraceUnit,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            default_shell: if cfg!(windows) {
                "powershell".into()
            } else {
                "bash".into()
            },
            vault_grace_value: 1,
            vault_grace_unit: VaultGraceUnit::Day,
        }
    }
}

impl Settings {
    pub fn path() -> Result<PathBuf> {
        if cfg!(feature = "portable") {
            let exe = std::env::current_exe().context("locate OpenTerm executable")?;
            let dir = exe.parent().context("OpenTerm executable has no parent")?;
            Ok(dir.join("openterm.ini"))
        } else {
            let dir = dirs::config_dir()
                .context("no operating-system config directory")?
                .join("openterm");
            fs::create_dir_all(&dir)
                .with_context(|| format!("create settings directory {}", dir.display()))?;
            Ok(dir.join("settings.ini"))
        }
    }

    pub fn load_or_default() -> Self {
        let Ok(path) = Self::path() else {
            return Self::default();
        };
        if let Ok(data) = fs::read_to_string(&path) {
            return Self::from_ini(&data);
        }

        // One-time compatibility with builds that wrote JSON. `main` saves the
        // loaded value immediately, so the next launch uses INI exclusively.
        Self::legacy_json_path()
            .ok()
            .and_then(|legacy| fs::read(legacy).ok())
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create settings directory {}", parent.display()))?;
        }
        fs::write(&path, self.to_ini())
            .with_context(|| format!("write settings {}", path.display()))
    }

    fn legacy_json_path() -> Result<PathBuf> {
        Ok(Self::path()?.with_file_name(if cfg!(feature = "portable") {
            "openterm.json"
        } else {
            "settings.json"
        }))
    }

    fn from_ini(data: &str) -> Self {
        let mut settings = Self::default();
        let mut section = String::new();

        for raw in data.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = name.trim().to_ascii_lowercase();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().trim_matches(['"', '\'']).to_ascii_lowercase();

            match (section.as_str(), key.as_str(), value.as_str()) {
                ("appearance" | "", "theme", "dark") => settings.theme = ThemeMode::Dark,
                ("appearance" | "", "theme", "light") => settings.theme = ThemeMode::Light,
                ("terminal" | "", "default_shell", shell)
                    if matches!(
                        shell,
                        "powershell" | "pwsh" | "wsl" | "bash" | "cmd" | "cmd.exe"
                    ) =>
                {
                    settings.default_shell = shell.into();
                }
                ("vault", "unlock_grace_unit", "day") => {
                    settings.vault_grace_unit = VaultGraceUnit::Day
                }
                ("vault", "unlock_grace_unit", "week") => {
                    settings.vault_grace_unit = VaultGraceUnit::Week
                }
                ("vault", "unlock_grace_unit", "month") => {
                    settings.vault_grace_unit = VaultGraceUnit::Month
                }
                ("vault", "unlock_grace_unit", "year") => {
                    settings.vault_grace_unit = VaultGraceUnit::Year
                }
                ("vault", "unlock_grace_value", value) => {
                    if let Ok(value) = value.parse::<u64>() {
                        settings.vault_grace_value = value.max(1);
                    }
                }
                _ => {}
            }
        }
        settings.vault_grace_value = settings
            .vault_grace_value
            .min(settings.vault_grace_unit.max_value());
        settings
    }

    fn to_ini(&self) -> String {
        let theme = match self.theme {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        };
        let grace_unit = match self.vault_grace_unit {
            VaultGraceUnit::Day => "day",
            VaultGraceUnit::Week => "week",
            VaultGraceUnit::Month => "month",
            VaultGraceUnit::Year => "year",
        };
        let grace_value = self
            .vault_grace_value
            .max(1)
            .min(self.vault_grace_unit.max_value());
        format!(
            "; OpenTerm settings\n[appearance]\ntheme={theme}\n\n[terminal]\ndefault_shell={}\n\n[vault]\nunlock_grace_value={grace_value}\nunlock_grace_unit={grace_unit}\n",
            self.default_shell,
        )
    }

    pub fn vault_grace_seconds(&self) -> u64 {
        self.vault_grace_unit.seconds(
            self.vault_grace_value
                .min(self.vault_grace_unit.max_value()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_round_trip() {
        let expected = Settings {
            theme: ThemeMode::Light,
            default_shell: "wsl".into(),
            vault_grace_value: 3,
            vault_grace_unit: VaultGraceUnit::Week,
        };
        let loaded = Settings::from_ini(&expected.to_ini());
        assert_eq!(loaded.theme, expected.theme);
        assert_eq!(loaded.default_shell, expected.default_shell);
        assert_eq!(loaded.vault_grace_value, expected.vault_grace_value);
        assert_eq!(loaded.vault_grace_unit, expected.vault_grace_unit);
    }

    #[test]
    fn invalid_ini_values_fall_back_safely() {
        let loaded = Settings::from_ini(
            "[appearance]\ntheme=ultraviolet\n[terminal]\ndefault_shell=malware.exe\n",
        );
        assert_eq!(loaded.theme, ThemeMode::Dark);
        assert_eq!(loaded.default_shell, Settings::default().default_shell);
    }

    #[test]
    fn vault_grace_ranges_are_clamped_by_unit() {
        let days = Settings::from_ini("[vault]\nunlock_grace_unit=day\nunlock_grace_value=99\n");
        assert_eq!(days.vault_grace_value, 7);
        assert_eq!(days.vault_grace_seconds(), 7 * 86_400);

        let months =
            Settings::from_ini("[vault]\nunlock_grace_unit=month\nunlock_grace_value=99\n");
        assert_eq!(months.vault_grace_value, 12);

        let years = Settings::from_ini("[vault]\nunlock_grace_unit=year\nunlock_grace_value=500\n");
        assert_eq!(years.vault_grace_value, 500);
    }
}
