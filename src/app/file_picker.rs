use super::*;

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
    let attempts = [
        (
            "zenity",
            vec!["--file-selection", "--directory", &zenity_start],
        ),
        ("kdialog", vec!["--getexistingdirectory", start]),
    ];
    for (program, args) in attempts {
        match Command::new(program).args(args).output() {
            Ok(output) if output.status.success() => {
                let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return Ok((!selected.is_empty()).then_some(selected));
            }
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("couldn't open the folder picker"),
        }
    }
    Err(anyhow!("No folder picker is installed (tried zenity and kdialog). You can still enter the path manually."))
}
