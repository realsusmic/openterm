use super::AppResult;
#[cfg(any(windows, test, all(unix, not(target_os = "macos"))))]
use anyhow::anyhow;
#[cfg(any(windows, target_os = "macos"))]
use anyhow::Context;
use std::{path::Path, process::Command};

pub(super) fn sync_root_too_broad(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let supplied = Path::new(value);
    let normalized = std::fs::canonicalize(supplied).unwrap_or_else(|_| supplied.to_path_buf());
    if normalized.parent().is_none() {
        return true;
    }
    dirs::home_dir()
        .map(|home| std::fs::canonicalize(&home).unwrap_or(home) == normalized)
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
pub(super) fn pick_local_folder(start: &str) -> AppResult<Option<String>> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"Add-Type -AssemblyName System.Windows.Forms; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $d=New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description='Select local project folder'; if(Test-Path -LiteralPath $env:OPENTERM_PICKER_START){$d.SelectedPath=$env:OPENTERM_PICKER_START}; if($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK){[Console]::Out.Write($d.SelectedPath)}"#;
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-STA", "-Command", SCRIPT])
        .env("OPENTERM_PICKER_START", start)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("couldn't open the folder picker")?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(if message.is_empty() {
            "the folder picker failed".to_string()
        } else {
            message
        }));
    }
    let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!selected.is_empty()).then_some(selected))
}

#[cfg(target_os = "macos")]
pub(super) fn pick_local_folder(_start: &str) -> AppResult<Option<String>> {
    let output = Command::new("osascript")
        .args([
            "-e",
            "POSIX path of (choose folder with prompt \"Select local project folder\")",
        ])
        .output()
        .context("couldn't open the folder picker")?;
    if !output.status.success() {
        return Ok(None);
    }
    let selected = String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_end_matches('/')
        .to_string();
    Ok((!selected.is_empty()).then_some(selected))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn pick_local_folder(start: &str) -> AppResult<Option<String>> {
    let zenity_start = format!("--filename={}/", start.trim_end_matches('/'));
    try_folder_pickers(
        &[
            (
                "zenity",
                &["--file-selection", "--directory", &zenity_start],
            ),
            ("kdialog", &["--getexistingdirectory", start]),
        ],
        |program, args| Command::new(program).args(args).output(),
    )
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn try_folder_pickers(
    attempts: &[(&str, &[&str])],
    mut run: impl FnMut(&str, &[&str]) -> std::io::Result<std::process::Output>,
) -> AppResult<Option<String>> {
    let mut failures = Vec::new();
    for &(program, args) in attempts {
        match run(program, args) {
            Ok(output) if output.status.success() => {
                let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return Ok((!selected.is_empty()).then_some(selected));
            }
            Ok(output)
                if output.status.code() == Some(1)
                    && String::from_utf8_lossy(&output.stderr).trim().is_empty() =>
            {
                return Ok(None);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                failures.push(format!("{program} ({}) {}", output.status, stderr.trim()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => failures.push(format!("{program}: {error}")),
        }
    }
    if failures.is_empty() {
        Err(anyhow!("No folder picker is installed (tried zenity and kdialog). You can still enter the path manually."))
    } else {
        Err(anyhow!(
            "Couldn't open a folder picker. {}. You can enter the path manually.",
            failures.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::try_folder_pickers;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn output(code: u8, stdout: &str, stderr: &str) -> Output {
        #[cfg(unix)]
        let status = ExitStatus::from_raw(i32::from(code) << 8);
        #[cfg(windows)]
        let status = ExitStatus::from_raw(u32::from(code));
        Output {
            status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn retries_with_second_picker_after_a_launch_error() {
        let picked = try_folder_pickers(&[("zenity", &[]), ("kdialog", &[])], |program, _| {
            Ok(if program == "zenity" {
                output(1, "", "cannot open display")
            } else {
                output(0, "/home/user/project\n", "")
            })
        })
        .unwrap();
        assert_eq!(picked.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn cancellation_does_not_open_a_second_picker() {
        let picked = try_folder_pickers(&[("zenity", &[]), ("kdialog", &[])], |program, _| {
            assert_eq!(program, "zenity");
            Ok(output(1, "", ""))
        })
        .unwrap();
        assert_eq!(picked, None);
    }
}
