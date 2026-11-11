use super::*;

pub(super) fn platform_label(platform: &str) -> &str {
    let lower = platform.to_ascii_lowercase();
    if lower.contains("windows") || lower.contains("mingw") || lower.contains("cygwin") {
        "Windows"
    } else if lower.contains("darwin") || lower.contains("macos") || lower.contains("mac os") {
        "macOS"
    } else if lower.contains("linux") {
        "Linux"
    } else {
        "Unknown"
    }
}

pub(super) fn os_unlock_error(error: &UnlockError) -> String {
    match error {
        UnlockError::BadPassword => {
            "The key in the OS keystore does not match this vault. No master password can unlock an OS-sign-in vault."
                .into()
        }
        UnlockError::NeedPassword => {
            "This vault was marked for OS sign-in but requested a password.".into()
        }
        UnlockError::Other(error) => format!("OS keystore: {error}"),
    }
}

pub(super) fn command_for_profile(
    profile: &CommandProfile,
) -> AppResult<(String, Vec<String>, String)> {
    let host = profile.host.trim();
    let port = profile.port.trim();
    let user_host = if profile.user.trim().is_empty() {
        host.to_string()
    } else {
        format!("{}@{host}", profile.user.trim())
    };
    let mut label = if host.is_empty() {
        profile.kind.label().to_string()
    } else {
        format!("{} · {host}", profile.kind.label())
    };
    let command = match profile.kind {
        ConnectionType::Telnet => (
            "telnet".into(),
            [host, port]
                .into_iter()
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        ConnectionType::Rsh => {
            let mut args = vec![host.to_string()];
            if !profile.user.trim().is_empty() {
                args.extend(["-l".into(), profile.user.trim().into()]);
            }
            ("rsh".into(), args)
        }
        ConnectionType::Ftp => ("ftp".into(), vec![host.into()]),
        ConnectionType::Mosh => {
            let mut args = Vec::new();
            if !port.is_empty() && port != "22" {
                args.extend(["--ssh".into(), format!("ssh -p {port}")]);
            }
            args.push(user_host);
            ("mosh".into(), args)
        }
        ConnectionType::AwsS3 => {
            let target = if host.is_empty() {
                "s3://".to_string()
            } else if host.starts_with("s3://") {
                host.to_string()
            } else {
                format!("s3://{host}")
            };
            let mut args = vec!["s3".into(), "ls".into(), target];
            if !profile.user.trim().is_empty() {
                args.extend(["--profile".into(), profile.user.trim().into()]);
            }
            ("aws".into(), args)
        }
        ConnectionType::Wsl => terminal_command(TerminalKind::Wsl, host)?,
        ConnectionType::Serial => {
            if cfg!(windows) {
                (
                    "plink.exe".into(),
                    vec![
                        "-serial".into(),
                        host.into(),
                        "-sercfg".into(),
                        format!("{},8,n,1,N", if port.is_empty() { "115200" } else { port }),
                    ],
                )
            } else {
                (
                    "screen".into(),
                    vec![
                        host.into(),
                        if port.is_empty() { "115200" } else { port }.into(),
                    ],
                )
            }
        }
        ConnectionType::Shell => {
            label = profile.terminal.label().into();
            terminal_command(profile.terminal, host)?
        }
        _ => {
            return Err(anyhow!(
                "{} opens outside the terminal",
                profile.kind.label()
            ))
        }
    };
    Ok((command.0, command.1, label))
}

pub(super) fn terminal_command(
    kind: TerminalKind,
    wsl_distribution: &str,
) -> AppResult<(String, Vec<String>)> {
    match kind {
        TerminalKind::PowerShell => Ok((
            pty::powershell_shell().ok_or_else(|| anyhow!("PowerShell is not installed"))?,
            vec!["-NoLogo".into()],
        )),
        TerminalKind::Wsl => {
            let program = pty::wsl_shell().ok_or_else(|| anyhow!("WSL is not installed"))?;
            let mut args = Vec::new();
            if !wsl_distribution.is_empty() {
                args.extend(["-d".into(), wsl_distribution.into()]);
            }
            Ok((program, args))
        }
        TerminalKind::Bash => Ok((
            pty::posix_shell().ok_or_else(|| anyhow!("No POSIX shell is installed"))?,
            vec!["-l".into()],
        )),
        TerminalKind::Cmd => Ok((
            pty::cmd_shell().ok_or_else(|| anyhow!("CMD is not installed"))?,
            Vec::new(),
        )),
    }
}

pub(super) fn profile_shell(profile: &CommandProfile) -> &str {
    match profile.kind {
        ConnectionType::Shell => profile.terminal.shell_name(),
        ConnectionType::Wsl => "bash",
        _ => "unknown",
    }
}

pub(super) fn profile_platform(profile: &CommandProfile) -> &str {
    match profile.kind {
        ConnectionType::Wsl => "linux",
        ConnectionType::Shell if profile.terminal == TerminalKind::Wsl => "linux",
        ConnectionType::Shell if profile.terminal == TerminalKind::Bash && cfg!(windows) => "linux",
        _ => std::env::consts::OS,
    }
}

pub(super) fn launch_external(profile: &CommandProfile) -> AppResult<()> {
    let host = profile.host.trim();
    let port = profile.port.trim();
    let (program, args): (String, Vec<String>) = match profile.kind {
        ConnectionType::Browser => {
            let url = if host.contains("://") {
                host.to_string()
            } else {
                format!("https://{host}")
            };
            if cfg!(windows) {
                ("explorer.exe".into(), vec![url])
            } else if cfg!(target_os = "macos") {
                ("open".into(), vec![url])
            } else {
                ("xdg-open".into(), vec![url])
            }
        }
        ConnectionType::Rdp => {
            let target = if port.is_empty() || port == "3389" {
                host.to_string()
            } else {
                format!("{host}:{port}")
            };
            if cfg!(windows) {
                ("mstsc.exe".into(), vec![format!("/v:{target}")])
            } else {
                ("xfreerdp".into(), vec![format!("/v:{target}")])
            }
        }
        ConnectionType::Vnc => {
            let target = if port.is_empty() {
                host.to_string()
            } else {
                format!("{host}:{port}")
            };
            if cfg!(target_os = "macos") {
                ("open".into(), vec![format!("vnc://{target}")])
            } else {
                ("vncviewer".into(), vec![target])
            }
        }
        ConnectionType::Xdmcp => (
            "Xephyr".into(),
            vec!["-query".into(), host.into(), ":1".into()],
        ),
        _ => {
            return Err(anyhow!(
                "{} is not an external connection",
                profile.kind.label()
            ))
        }
    };
    Command::new(&program)
        .args(&args)
        .spawn()
        .with_context(|| format!("couldn't launch {program}"))?;
    Ok(())
}
