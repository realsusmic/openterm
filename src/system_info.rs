//! SysInfo — persistent system summaries for local and SSH terminal sessions.
//!
//! susmic@susmic.dev was here, 56 hours after i started making this
//! why does cmd have to be so stubborn...

use sysinfo::System;

/// Return the visible one-liner for the OS and shell of the active session.
///
/// Commands are sent as plain text exactly as shown here; none are encoded.
///
/// Every script prints the same seven keys in the same order:
///
/// ```text
/// os      <name>
/// kernel  <version>
/// shell   <shell>
/// uptime  <uptime>
/// cpu     <brand> - <logical cores>c
/// memory  <memory>
/// term    <terminal>
/// ```
pub fn command(platform: &str, shell: &str) -> String {
    let platform = platform.to_ascii_lowercase();

    if platform.contains("windows") || platform.contains("mingw") || platform.contains("cygwin") {
        let shell = shell.to_ascii_lowercase();
        if shell == "cmd" || shell.ends_with("cmd.exe") {
            WINDOWS_CMD_SCRIPT.into()
        } else {
            WINDOWS_POWERSHELL_SCRIPT.into()
        }
    } else if platform.contains("darwin")
        || platform.contains("macos")
        || platform.contains("mac os")
    {
        MACOS_SCRIPT.into()
    } else {
        LINUX_SCRIPT.into()
    }
}

// Shell is read from the live process (`ps -p $$`), not $SHELL, because $SHELL
// is the passwd entry and can differ from the shell actually running over SSH.
//
// Memory falls back to MemFree when MemAvailable is absent and only prints when
// the parse produced usable numbers.
const LINUX_SCRIPT: &str = r#"os="$(if [ -r /etc/os-release ]; then . /etc/os-release; printf '%s' "${PRETTY_NAME:-Linux}"; elif [ -r /usr/lib/os-release ]; then . /usr/lib/os-release; printf '%s' "${PRETTY_NAME:-Linux}"; else uname -s; fi)"; kernel="$(uname -r)"; shell="$(ps -p $$ -o comm= 2>/dev/null | sed 's|^-||; s|.*/||')"; [ -n "$shell" ] || shell="${SHELL##*/}"; uptime="$(awk '{s=int($1);d=int(s/86400);h=int((s%86400)/3600);m=int((s%3600)/60);if(d)printf "%dd %dh %dm",d,h,m;else if(h)printf "%dh %dm",h,m;else printf "%dm",m}' /proc/uptime 2>/dev/null)"; cpu="$(awk -F: '/model name|Model|Hardware|Processor/{gsub(/^[ \t]+/,"",$2);if($2!=""){print $2;exit}}' /proc/cpuinfo 2>/dev/null)"; [ -n "$cpu" ] || cpu="$(uname -m)"; cores="$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null)"; [ -n "$cores" ] || cores=1; mem="$(awk '/^MemTotal:/{t=$2}/^MemAvailable:/{a=$2}/^MemFree:/{f=$2}END{if(!a)a=f;if(t>0&&a>0)printf "%.1f GiB / %.1f GiB",(t-a)/1048576,t/1048576}' /proc/meminfo 2>/dev/null)"; printf 'os      %s\nkernel  %s\nshell   %s\nuptime  %s\ncpu     %s - %sc\nmemory  %s\nterm    %s\n' "$os" "$kernel" "${shell:-unknown}" "${uptime:-unknown}" "$cpu" "$cores" "${mem:-unknown}" "${TERM:-unknown}""#;
// vm_stat: free + inactive + speculative + purgeable counted as available,
// matching Activity Monitor's "Memory Used" within a few hundred MiB. Not
// identical to macOS's own calc (which also factors compressed + wired),
// but close enough for a one-liner summary.
const MACOS_SCRIPT: &str = r#"shell="$(ps -p $$ -o comm= 2>/dev/null | sed 's|^-||; s|.*/||')"; [ -n "$shell" ] || shell="${SHELL##*/}"; boot="$(sysctl -n kern.boottime 2>/dev/null | sed -E 's/.*sec *= *([0-9]+).*/\1/')"; up="$(awk -v b="$boot" -v n="$(date +%s)" 'BEGIN{if(b==""){print "unknown";exit}s=n-b;d=int(s/86400);h=int((s%86400)/3600);m=int((s%3600)/60);if(d)printf "%dd %dh %dm",d,h,m;else if(h)printf "%dh %dm",h,m;else printf "%dm",m}')"; mem="$(vm_stat 2>/dev/null | awk -v t="$(sysctl -n hw\.memsize)" -v p="$(sysctl -n hw\.pagesize)" '/Pages free/{f=$3}/Pages inactive/{i=$3}/Pages speculative/{s=$3}/Pages purgeable/{g=$3}END{gsub(/\./,"",f);gsub(/\./,"",i);gsub(/\./,"",s);gsub(/\./,"",g);a=(f+i+s+g)*p;if(t>0)printf "%.1f GiB / %.1f GiB",(t-a)/1073741824,t/1073741824}')"; printf 'os      %s %s\nkernel  %s\nshell   %s\nuptime  %s\ncpu     %s - %sc\nmemory  %s\nterm    %s\n' "$(sw_vers -productName)" "$(sw_vers -productVersion)" "$(uname -r)" "${shell:-unknown}" "${up:-unknown}" "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || sysctl -n hw\.model)" "$(sysctl -n hw\.ncpu)" "${mem:-unknown}" "${TERM:-unknown}""#;
const WINDOWS_POWERSHELL_SCRIPT: &str = r#"$os=Get-CimInstance Win32_OperatingSystem;$cpu=Get-CimInstance Win32_Processor|Select-Object -First 1;$u=(Get-Date)-$os.LastBootUpTime;$term=if($env:WT_SESSION){'Generic'}elseif($env:TERM_PROGRAM){$env:TERM_PROGRAM}else{'conhost'};Write-Host ("os      {0} {1}`nkernel  {2}`nshell   powershell {3}`nuptime  {4}d {5}h {6}m`ncpu     {7} - {8}c`nmemory  {9:N1} GiB / {10:N1} GiB`nterm    {11}" -f $os.Caption,$os.Version,[Environment]::OSVersion.Version,$PSVersionTable.PSVersion,$u.Days,$u.Hours,$u.Minutes,$cpu.Name,$cpu.NumberOfLogicalProcessors,(($os.TotalVisibleMemorySize-$os.FreePhysicalMemory)/1MB),($os.TotalVisibleMemorySize/1MB),$term)"#;
// NOTE: cmd uptime stays `since <boot time>` instead of `Xd Yh Zm`. Pure-cmd
// date arithmetic on a locale-dependent string is cursed, and every clean
// shortcut (wmic, powershell shellout, cmd /v:on /c) is either dead or
// banned by the one-liner test below. The seven-key contract still holds —
// only the uptime value shape differs on cmd.
const WINDOWS_CMD_SCRIPT: &str = r#"@systeminfo>"%TEMP%\sys.info" 2>nul & for /f "tokens=1,* delims=:" %a in ('findstr /B /C:"OS Name" "%TEMP%\sys.info"') do @for /f "tokens=*" %c in ("%b") do @echo os      %c & for /f "tokens=1,* delims=:" %a in ('findstr /B /C:"OS Version" "%TEMP%\sys.info"') do @for /f "tokens=1" %c in ("%b") do @echo kernel  %c & echo shell   cmd & for /f "tokens=1,* delims=:" %a in ('findstr /B /C:"System Boot Time" "%TEMP%\sys.info"') do @for /f "tokens=*" %c in ("%b") do @echo uptime  since %c & for /f "tokens=2,*" %a in ('reg query "HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0" /v ProcessorNameString 2^>nul') do @echo cpu     %b - %NUMBER_OF_PROCESSORS%c & set "a_mb=" & set "t_mb=" & for /f "tokens=1,* delims=:" %a in ('findstr /B /C:"Available Physical Memory" "%TEMP%\sys.info"') do @for /f "tokens=1" %c in ("%b") do @set "a_mb=%c" & for /f "tokens=1,* delims=:" %a in ('findstr /B /C:"Total Physical Memory" "%TEMP%\sys.info"') do @for /f "tokens=1" %c in ("%b") do @set "t_mb=%c" & call set "a_mb=%%a_mb:,=%%" & call set "t_mb=%%t_mb:,=%%" & call set /a "u_mb=%%t_mb%%-%%a_mb%%" >nul 2>&1 & call set /a "uw=%%u_mb%%/1024" >nul 2>&1 & call set /a "ut=(%%u_mb%% - %%u_mb%%/1024*1024)*10/1024" >nul 2>&1 & call set /a "tw=%%t_mb%%/1024" >nul 2>&1 & call set /a "tt=(%%t_mb%% - %%t_mb%%/1024*1024)*10/1024" >nul 2>&1 & call echo memory  %%uw%%.%%ut%% GiB / %%tw%%.%%tt%% GiB & if defined WT_SESSION echo term    Generic & if not defined WT_SESSION echo term    conhost & del "%TEMP%\sys.info" >nul 2>&1"#;

/// Native startup summary. This remains useful before a shell is ready; the
/// toolbar action uses `command` so its output stays in terminal history.
pub fn ansi(shell: &str) -> String {
    let mut s = System::new();
    s.refresh_memory();
    s.refresh_cpu_all();

    let user = whoami::username();
    let host = whoami::fallible::hostname().unwrap_or_else(|_| "unknown".into());
    let os = System::long_os_version().unwrap_or_else(whoami::distro);
    let kernel = System::kernel_version().unwrap_or_else(|| "—".into());
    let cpu = s
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_else(|| "—".into());
    let cores = s.cpus().len();

    const DIM: &str = "\x1b[38;5;245m";
    const MUT: &str = "\x1b[38;5;240m";
    const FG: &str = "\x1b[39m";
    const DOT: &str = "\x1b[38;5;112m";
    const R: &str = "\x1b[0m";

    let head_len = user.chars().count() + host.chars().count() + 3;
    let kv = |k: &str, v: String| format!("  {DIM}{k:<8}{R}{FG}{v}{R}");

    let rows = [
        format!("  {DOT}●{R} {FG}{user}{MUT}@{FG}{host}{R}"),
        format!("  {MUT}{}{R}", "─".repeat(head_len)),
        kv("os", format!("{os} ({})", std::env::consts::ARCH)),
        kv("kernel", kernel),
        kv("shell", shell.to_string()),
        kv("uptime", fmt_uptime(System::uptime())),
        kv("cpu", format!("{cpu} - {cores}c")),
        kv(
            "memory",
            format!(
                "{} / {}",
                fmt_bytes(s.used_memory()),
                fmt_bytes(s.total_memory())
            ),
        ),
        kv("term", "openterm".into()),
    ];

    let mut out = String::from("\r\n");
    for row in rows {
        out.push_str(&row);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out
}

fn fmt_bytes(bytes: u64) -> String {
    let gib = bytes as f64 / 1024f64.powi(3);

    if gib >= 1.0 {
        format!("{gib:.1} GiB")
    } else {
        format!("{:.0} MiB", bytes as f64 / 1024f64.powi(2))
    }
}

fn fmt_uptime(seconds: u64) -> String {
    let (days, hours, minutes) = (
        seconds / 86_400,
        (seconds % 86_400) / 3600,
        (seconds % 3600) / 60,
    );

    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [&str; 7] = ["os", "kernel", "shell", "uptime", "cpu", "memory", "term"];

    #[test]
    fn selects_plain_one_liner_for_platform_and_shell() {
        assert_eq!(command("Linux", "bash"), LINUX_SCRIPT);
        assert_eq!(command("Darwin", "zsh"), MACOS_SCRIPT);
        assert_eq!(command("WINDOWS", "PowerShell"), WINDOWS_POWERSHELL_SCRIPT);
        assert_eq!(command("Windows", "CMD.EXE"), WINDOWS_CMD_SCRIPT);
        assert_eq!(command("MinGW64_NT", "cmd"), WINDOWS_CMD_SCRIPT);
    }

    #[test]
    fn every_script_is_pure_ascii() {
        for script in [
            LINUX_SCRIPT,
            MACOS_SCRIPT,
            WINDOWS_POWERSHELL_SCRIPT,
            WINDOWS_CMD_SCRIPT,
        ] {
            assert!(script.is_ascii(), "non-ASCII byte in script");
        }
    }

    #[test]
    fn cmd_script_is_direct_interactive_one_liner() {
        let script = WINDOWS_CMD_SCRIPT;
        let lower = script.to_ascii_lowercase();

        assert!(
            !lower.contains("cmd /"),
            "do not add another cmd.exe parser layer"
        );
        assert!(!lower.contains("powershell"));
        assert!(!lower.contains("pwsh"));
        assert!(!lower.contains("wmic"));
        assert!(
            script.contains(r#"%TEMP%\sys.info"#),
            "temporary snapshot must be sys.info"
        );
        assert!(script.contains("systeminfo"));
        assert!(script.contains(r#"del "%TEMP%\sys.info""#));
        assert!(
            !script.contains('\n'),
            "CMD command must remain one physical line"
        );
    }

    /// Smoke test: the dispatcher picking the right constant means nothing if
    /// the constant does not actually produce the seven expected rows.
    #[cfg(unix)]
    #[test]
    fn host_script_emits_every_key() {
        let script = if cfg!(target_os = "macos") {
            MACOS_SCRIPT
        } else {
            LINUX_SCRIPT
        };

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("failed to run host script");

        assert!(out.status.success(), "host script exited non-zero");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();

        assert_eq!(lines.len(), KEYS.len(), "unexpected row count:\n{stdout}");

        for (line, key) in lines.iter().zip(KEYS) {
            assert!(
                line.starts_with(key),
                "row {line:?} should start with {key}"
            );

            let value = line[key.len()..].trim();
            assert!(!value.is_empty(), "{key} row has no value");
            // Intentionally no `!= "unknown"` check: TERM, shell, memory, and
            // uptime can all legitimately resolve to "unknown" in stripped CI
            // containers.
        }

        assert!(stdout.contains("GiB / "), "memory is missing used / total");
    }
}
