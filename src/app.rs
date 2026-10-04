//! app — window chrome + layout.
//!   titlebar │ sidebar (devices + files) │ tabs / breadcrumb / terminal ┃ editor │ statusbar

use crate::{
    editor::Editor,
    fs_tree::{self, Node, RemoteNode},
    icons,
    pty::{self, LocalPty},
    settings::{Settings, ThemeMode, VaultGraceUnit},
    ssh::{HostKeyPrompt, SftpHandle, SshAuth, SshConfig, SshSession},
    system_info,
    term::TermView,
    theme::{self, *},
    vault::{self, Mode as VaultMode, Secret as VaultSecret, UnlockError, Vault},
    wake,
};
use egui::{
    pos2,
    text::{LayoutJob, TextFormat, TextWrapping},
    vec2, Color32, CursorIcon, Frame, Galley, Id, Key, Margin, Modifiers, Painter, Pos2, Rect,
    Response, Rounding, Sense, Stroke, TextEdit, Ui, Vec2, ViewportCommand,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};
use zeroize::{Zeroize, Zeroizing};

// ───────────────────────────── model ─────────────────────────────

enum Backend {
    Local(LocalPty),
    Ssh(SshSession),
}
impl Backend {
    fn write(&self, d: &[u8]) {
        match self {
            Self::Local(p) => p.write(d),
            Self::Ssh(s) => s.write(d),
        }
    }
    fn resize(&self, c: u16, r: u16) {
        match self {
            Self::Local(p) => p.resize(c, r),
            Self::Ssh(s) => s.resize(c, r),
        }
    }
    fn drain(&self) -> Vec<u8> {
        match self {
            Self::Local(p) => p.drain(),
            Self::Ssh(s) => s.drain(),
        }
    }
    fn sftp(&self) -> Option<SftpHandle> {
        match self {
            Self::Ssh(s) => Some(s.sftp()),
            _ => None,
        }
    }
}

struct Session {
    id: u64,
    label: String,
    device: String,
    backend: Option<Backend>, // local shells spawn lazily once the real size is known
    launch: Option<(String, Vec<String>)>,
    term: TermView,
    remote_tree: Option<RemoteNode>,
    hostname: String,
    platform: String,
    shell: String,
}

#[derive(Clone)]
enum DeviceKind {
    Local,
    Ssh(SshConfig),
    VaultRef(String),
    Command(CommandProfile),
}

#[derive(Clone)]
struct CommandProfile {
    kind: ConnectionType,
    terminal: TerminalKind,
    host: String,
    port: String,
    user: String,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum TerminalKind {
    PowerShell,
    Wsl,
    Bash,
    Cmd,
}

impl Default for TerminalKind {
    fn default() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Bash
        }
    }
}

impl TerminalKind {
    fn label(self) -> &'static str {
        match self {
            Self::PowerShell => "PowerShell",
            Self::Wsl => "WSL",
            Self::Bash => "Bash",
            Self::Cmd => "CMD",
        }
    }

    fn shell_name(self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Wsl | Self::Bash => "bash",
            Self::Cmd => "cmd",
        }
    }

    fn setting_id(self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Wsl => "wsl",
            Self::Bash => "bash",
            Self::Cmd => "cmd",
        }
    }

    fn from_setting(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "powershell" | "pwsh" => Self::PowerShell,
            "wsl" => Self::Wsl,
            "cmd" | "cmd.exe" => Self::Cmd,
            "bash" => Self::Bash,
            _ => Self::default(),
        }
    }
}

#[derive(Clone)]
struct Device {
    id: String,
    label: String,
    kind: DeviceKind,
}

struct Pending {
    device: String,
    label: String,
    cfg: SshConfig,
    rx: mpsc::Receiver<anyhow::Result<SshSession>>,
}

struct HostKeyDialog {
    device: String,
    label: String,
    cfg: SshConfig,
    fingerprint: String,
    key: String,
    known_hosts: String,
    known_fingerprints: Vec<String>,
    changed: bool,
}
struct PendingRemote {
    session_id: u64,
    path: String,
    rx: mpsc::Receiver<anyhow::Result<crate::agent::DirectoryListing>>,
}
struct PendingRemoteFile {
    session_id: u64,
    path: String,
    rx: mpsc::Receiver<anyhow::Result<Vec<u8>>>,
}
struct PendingRemoteWrite {
    session_id: u64,
    key: PathBuf,
    saved_data: Vec<u8>,
    rx: mpsc::Receiver<anyhow::Result<()>>,
}
struct PendingDeviceInfo {
    session_id: u64,
    rx: mpsc::Receiver<anyhow::Result<crate::agent::DeviceInfo>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferMode {
    DownloadFile,
    DownloadFolder,
    UploadFile,
    UploadFolder,
    Sync,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SyncDirection {
    LocalToRemote,
    RemoteToLocal,
}

impl SyncDirection {
    fn protocol(self) -> &'static str {
        match self {
            Self::LocalToRemote => "local_to_remote",
            Self::RemoteToLocal => "remote_to_local",
        }
    }
}

struct TransferDialog {
    session_id: u64,
    remote: String,
    local: String,
    mode: TransferMode,
    direction: SyncDirection,
    changes: Vec<crate::agent::SyncChange>,
    previewed: bool,
    busy: bool,
    error: Option<String>,
}

enum TransferResult {
    Preview(Vec<crate::agent::SyncChange>),
    Complete(String),
}

struct PendingTransfer {
    session_id: u64,
    rx: mpsc::Receiver<anyhow::Result<TransferResult>>,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum AuthMode {
    #[default]
    Password,
    Key,
    Agent,
}

#[derive(Default, PartialEq, Eq, Clone, Copy, Debug)]
enum ConnectionType {
    #[default]
    Ssh,
    Telnet,
    Rsh,
    Xdmcp,
    Rdp,
    Vnc,
    Ftp,
    Sftp,
    Serial,
    File,
    Shell,
    Browser,
    Mosh,
    AwsS3,
    Wsl,
}

impl ConnectionType {
    const ALL: [Self; 15] = [
        Self::Ssh,
        Self::Telnet,
        Self::Rsh,
        Self::Xdmcp,
        Self::Rdp,
        Self::Vnc,
        Self::Ftp,
        Self::Sftp,
        Self::Serial,
        Self::File,
        Self::Shell,
        Self::Browser,
        Self::Mosh,
        Self::AwsS3,
        Self::Wsl,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Ssh => "SSH",
            Self::Telnet => "Telnet",
            Self::Rsh => "Rsh",
            Self::Xdmcp => "XDMCP",
            Self::Rdp => "RDP",
            Self::Vnc => "VNC",
            Self::Ftp => "FTP",
            Self::Sftp => "SFTP",
            Self::Serial => "Serial",
            Self::File => "File",
            Self::Shell => "Shell",
            Self::Browser => "Browser",
            Self::Mosh => "Mosh",
            Self::AwsS3 => "AWS S3",
            Self::Wsl => "WSL",
        }
    }

    fn default_port(self) -> &'static str {
        match self {
            Self::Ssh | Self::Sftp | Self::Mosh => "22",
            Self::Telnet => "23",
            Self::Rsh => "514",
            Self::Xdmcp => "177",
            Self::Rdp => "3389",
            Self::Vnc => "5900",
            Self::Ftp => "21",
            Self::Serial => "115200",
            _ => "",
        }
    }

    fn uses_ssh_auth(self) -> bool {
        matches!(self, Self::Ssh | Self::Sftp)
    }
}

#[derive(Default)]
struct SshForm {
    connection: ConnectionType,
    terminal: TerminalKind,
    focus_host: bool,
    host: String,
    port: String,
    user: String,
    password: Zeroizing<String>,
    key_path: String,
    mode: AuthMode,
    save_to_vault: bool,
    label: String,
    error: Option<String>,
}

#[derive(Default)]
enum VaultState {
    #[default]
    Idle,
    Setup {
        choice: VaultMode,
        pw: Zeroizing<String>,
        pw2: Zeroizing<String>,
        err: Option<String>,
    },
    Unlock {
        pw: Zeroizing<String>,
        err: Option<String>,
        focus: bool,
    },
    KeystoreError {
        err: String,
    },
    ResetConfirm {
        confirm: String,
        err: Option<String>,
        focus: bool,
    },
    Ready,
}

enum Act {
    SelectDevice(String),
    NewSession(String),
    NewTab,
    Activate(u64),
    Close(u64),
    OpenFile(PathBuf),
    SetRoot(PathBuf),
    RootUp,
    RefreshTree,
    ToggleEditor,
    SysInfo,
    OpenModal,
    CloseModal,
    OpenSettings,
    CloseSettings,
    SetTheme(ThemeMode),
    SetDefaultShell(TerminalKind),
    SetVaultGrace(u64, VaultGraceUnit),
    Connect,
    VaultChooseMode(VaultMode),
    VaultCreate,
    VaultUnlock,
    VaultOsRetry,
    VaultLock,
    DeleteDevice(String),
    VaultConnect(String),
    VaultResetBegin,
    VaultResetCancel,
    VaultReset,
    RemoteLoad(u64, String),
    RemoteSetRoot(u64, String),
    RemoteUp(u64),
    RemoteRefresh(u64),
    RemoteOpen(u64, String),
    RemoteDownload(u64, String, bool),
    RemoteUpload(u64, String, bool),
    RemoteSync(u64, String),
    TransferPickLocalFolder,
    TransferPreview,
    TransferApply,
    TransferClose,
    TrustHostKey,
    RejectHostKey,
}

pub struct OpenTerm {
    devices: Vec<Device>,
    selected: String,
    sessions: Vec<Session>,
    active: Option<u64>,
    next_id: u64,
    tree: Node,
    editor: Editor,
    show_editor: bool,
    modal: bool,
    settings_open: bool,
    settings: Settings,
    form: SshForm,
    pending: Vec<Pending>,
    pending_remote: Vec<PendingRemote>,
    pending_remote_files: Vec<PendingRemoteFile>,
    pending_remote_writes: Vec<PendingRemoteWrite>,
    pending_device_info: Vec<PendingDeviceInfo>,
    connection_errors: HashMap<String, String>,
    transfer_dialog: Option<TransferDialog>,
    pending_transfer: Option<PendingTransfer>,
    host_key_dialog: Option<HostKeyDialog>,
    flash: Option<(String, bool, f64)>,
    vault: Option<Vault>,
    vault_ui: VaultState,
    window_maximized: bool,
}

impl OpenTerm {
    pub fn new(cc: &eframe::CreationContext, settings: Settings) -> Self {
        wake::set(cc.egui_ctx.clone());
        let default_terminal = TerminalKind::from_setting(&settings.default_shell);
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let vault = Vault::load().ok().flatten();
        let vault_ui = match &vault {
            Some(v) if v.mode() == VaultMode::Os => VaultState::Idle, // auto-unlocked below
            Some(_) => VaultState::Unlock {
                pw: Zeroizing::new(String::new()),
                err: None,
                focus: true,
            },
            None => VaultState::Setup {
                choice: VaultMode::Os,
                pw: Zeroizing::new(String::new()),
                pw2: Zeroizing::new(String::new()),
                err: None,
            },
        };

        let mut app = Self {
            devices: vec![Device {
                id: "local".into(),
                label: "Local machine".into(),
                kind: DeviceKind::Local,
            }],
            selected: "local".into(),
            sessions: Vec::new(),
            active: None,
            next_id: 1,
            tree: Node::root(&home),
            editor: Editor::default(),
            show_editor: false,
            modal: false,
            settings_open: false,
            settings,
            form: SshForm {
                port: "22".into(),
                ..Default::default()
            },
            pending: Vec::new(),
            pending_remote: Vec::new(),
            pending_remote_files: Vec::new(),
            pending_remote_writes: Vec::new(),
            pending_device_info: Vec::new(),
            connection_errors: HashMap::new(),
            transfer_dialog: None,
            pending_transfer: None,
            host_key_dialog: None,
            flash: None,
            vault,
            vault_ui,
            window_maximized: false,
        };
        if let Some(v) = &mut app.vault {
            if v.mode() == VaultMode::Os {
                match v.unlock(None) {
                    Ok(()) => {
                        app.vault_ui = VaultState::Ready;
                        app.load_saved_devices();
                    }
                    Err(e) => {
                        app.vault_ui = VaultState::KeystoreError {
                            err: os_unlock_error(&e),
                        };
                    }
                }
            } else {
                match v.unlock_remembered() {
                    Ok(true) => {
                        app.vault_ui = VaultState::Ready;
                        app.load_saved_devices();
                    }
                    Ok(false) => {}
                    Err(error) => log::warn!("remembered vault unlock failed: {error}"),
                }
            }
        }
        if let Err(error) = app.open_local(default_terminal) {
            app.flash = Some((error, true, cc.egui_ctx.input(|i| i.time) + 6.0));
        }
        app
    }

    fn load_saved_devices(&mut self) {
        let Some(v) = &self.vault else { return };
        for r in v.records().iter() {
            let id = format!("vault:{}", r.id);
            let label = if r.label.is_empty() {
                format!("{}@{}", r.user, r.host)
            } else {
                r.label.clone()
            };
            if let Some(device) = self.devices.iter_mut().find(|device| device.id == id) {
                device.label = label;
                device.kind = DeviceKind::VaultRef(r.id.clone());
                continue;
            }
            self.devices.push(Device {
                id,
                label,
                kind: DeviceKind::VaultRef(r.id.clone()),
            });
        }
    }

    fn open_local(&mut self, terminal: TerminalKind) -> Result<(), String> {
        let profile = CommandProfile {
            kind: ConnectionType::Shell,
            terminal,
            host: String::new(),
            port: String::new(),
            user: String::new(),
        };
        let (program, args, label) = command_for_profile(&profile)?;
        let term = TermView::new();
        let shell = terminal.shell_name().to_string();
        // splash goes in *before* the shell starts, so ConPTY (which asks where
        // the cursor is) and unix shells both put the prompt underneath it
        if !self.sessions.iter().any(|s| s.device == "local") {
            term.feed(system_info::ansi(&shell).as_bytes());
        }
        let id = self.next_id;
        self.next_id += 1;
        self.sessions.push(Session {
            id,
            label,
            device: "local".into(),
            backend: None,
            launch: Some((program, args)),
            term,
            remote_tree: None,
            hostname: whoami::fallible::hostname().unwrap_or_else(|_| "localhost".into()),
            platform: profile_platform(&profile).to_string(),
            shell,
        });
        self.active = Some(id);
        self.selected = "local".into();
        Ok(())
    }

    fn connect_from_vault(&mut self, rid: &str, ctx: &egui::Context) {
        let Some(v) = self.vault.as_ref() else {
            self.say(ctx, "No vault", true);
            return;
        };
        if !v.is_unlocked() {
            self.say(ctx, "Unlock the vault first", true);
            return;
        }
        let Some(rec) = v.records().iter().find(|r| r.id == rid).cloned() else {
            self.say(ctx, "No such record", true);
            return;
        };
        let auth = match v.decrypt(rid) {
            Ok(s) => match s.to_auth() {
                vault::PlainAuth::Password(p) => SshAuth::Password(p),
                vault::PlainAuth::Key { path, passphrase } => SshAuth::KeyFile { path, passphrase },
                vault::PlainAuth::Agent => SshAuth::Agent,
            },
            Err(e) => {
                self.say(ctx, format!("decrypt: {e}"), true);
                return;
            }
        };
        let cfg = SshConfig {
            host: rec.host,
            port: rec.port,
            user: rec.user,
            auth,
            trust_host_key: None,
            replace_host_key: false,
            known_fingerprints: Vec::new(),
        };
        let label = if rec.label.is_empty() {
            format!("{}@{}", cfg.user, cfg.host)
        } else {
            rec.label
        };
        self.open_ssh_on_device(cfg, Some((format!("vault:{rid}"), label)), ctx);
    }

    fn open_ssh(&mut self, cfg: SshConfig, ctx: &egui::Context) {
        self.open_ssh_on_device(cfg, None, ctx);
    }

    fn open_ssh_on_device(
        &mut self,
        cfg: SshConfig,
        saved: Option<(String, String)>,
        ctx: &egui::Context,
    ) {
        let fallback_label = format!("{}@{}", cfg.user, cfg.host);
        let (dev, label) = saved.unwrap_or_else(|| {
            (
                format!("ssh:{}@{}:{}", cfg.user, cfg.host, cfg.port),
                fallback_label,
            )
        });
        if !self.devices.iter().any(|d| d.id == dev) {
            self.devices.push(Device {
                id: dev.clone(),
                label: label.clone(),
                kind: DeviceKind::Ssh(cfg.clone()),
            });
        }
        self.selected = dev.clone();
        self.connection_errors.remove(&dev);
        self.active = self.sessions.iter().find(|s| s.device == dev).map(|s| s.id);
        let (tx, rx) = mpsc::channel();
        let connect_cfg = cfg.clone();
        std::thread::spawn(move || {
            let _ = tx.send(SshSession::connect(connect_cfg, 120, 34));
            wake::poke();
        });
        self.pending.push(Pending {
            device: dev,
            label: label.clone(),
            cfg,
            rx,
        });
        self.say(ctx, format!("connecting to {label}…"), false);
    }

    fn open_command_profile(&mut self, profile: CommandProfile, ctx: &egui::Context) {
        let (program, args, label) = match command_for_profile(&profile) {
            Ok(command) => command,
            Err(error) => {
                self.say(ctx, error, true);
                return;
            }
        };
        let platform = profile_platform(&profile).to_string();
        let shell = profile_shell(&profile).to_string();
        match LocalPty::spawn_program(&program, &args, 120, 34) {
            Ok(pty) => {
                let terminal_key = if profile.kind == ConnectionType::Shell {
                    profile.terminal.label().to_ascii_lowercase()
                } else {
                    String::new()
                };
                let device_id = format!(
                    "{}:{}:{}:{}:{}",
                    profile.kind.label().to_ascii_lowercase(),
                    terminal_key,
                    profile.user,
                    profile.host,
                    profile.port
                );
                if !self.devices.iter().any(|device| device.id == device_id) {
                    self.devices.push(Device {
                        id: device_id.clone(),
                        label: label.clone(),
                        kind: DeviceKind::Command(profile.clone()),
                    });
                }
                let id = self.next_id;
                self.next_id += 1;
                self.sessions.push(Session {
                    id,
                    label,
                    device: device_id.clone(),
                    backend: Some(Backend::Local(pty)),
                    launch: None,
                    term: TermView::new(),
                    remote_tree: None,
                    hostname: if profile.host.is_empty() {
                        whoami::fallible::hostname().unwrap_or_else(|_| "localhost".into())
                    } else {
                        profile.host.clone()
                    },
                    platform,
                    shell,
                });
                self.selected = device_id;
                self.active = Some(id);
            }
            Err(error) => self.say(
                ctx,
                format!("{} client is unavailable: {error}", profile.kind.label()),
                true,
            ),
        }
    }

    fn say(&mut self, ctx: &egui::Context, msg: impl Into<String>, err: bool) {
        self.flash = Some((msg.into(), err, ctx.input(|i| i.time) + 4.0));
    }

    fn spawn_remote_load(&mut self, session_id: u64, path: String, sftp: SftpHandle) {
        let (tx, rx) = mpsc::channel();
        let request_path = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(sftp.list_dir(&request_path));
            wake::poke();
        });
        self.pending_remote.push(PendingRemote {
            session_id,
            path,
            rx,
        });
    }

    fn spawn_remote_file(&mut self, session_id: u64, path: String, sftp: SftpHandle) {
        let (tx, rx) = mpsc::channel();
        let request_path = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(sftp.read_file(&request_path));
            wake::poke();
        });
        self.pending_remote_files.push(PendingRemoteFile {
            session_id,
            path,
            rx,
        });
    }

    fn spawn_remote_write(
        &mut self,
        save: crate::editor::RemoteSave,
    ) -> Result<(), (crate::editor::RemoteSave, String)> {
        let Some(sftp) = self
            .sessions
            .iter()
            .find(|session| session.id == save.session_id)
            .and_then(|session| session.backend.as_ref()?.sftp())
        else {
            return Err((save, "SSH session is no longer available".into()));
        };
        let crate::editor::RemoteSave {
            key,
            session_id,
            path,
            data,
        } = save;
        let (tx, rx) = mpsc::channel();
        let saved_data = data.clone();
        std::thread::spawn(move || {
            let _ = tx.send(sftp.write_file(&path, &data));
            wake::poke();
        });
        self.pending_remote_writes.push(PendingRemoteWrite {
            session_id,
            key,
            saved_data,
            rx,
        });
        Ok(())
    }

    fn spawn_device_info(&mut self, session_id: u64, sftp: SftpHandle) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(sftp.device_info());
            wake::poke();
        });
        self.pending_device_info
            .push(PendingDeviceInfo { session_id, rx });
    }

    fn poll_pending(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.pending.len() {
            match self.pending[i].rx.try_recv() {
                Ok(res) => {
                    let p = self.pending.remove(i);
                    match res {
                        Ok(sess) => {
                            self.connection_errors.remove(&p.device);
                            let id = self.next_id;
                            self.next_id += 1;
                            let sftp = sess.sftp();
                            self.sessions.push(Session {
                                id,
                                label: p.label.clone(),
                                device: p.device.clone(),
                                backend: Some(Backend::Ssh(sess)),
                                launch: None,
                                term: TermView::new(),
                                remote_tree: Some(RemoteNode::root(".")),
                                hostname: "detecting…".into(),
                                platform: "Unknown".into(),
                                shell: "unknown".into(),
                            });
                            self.spawn_remote_load(id, ".".into(), sftp.clone());
                            self.spawn_device_info(id, sftp);
                            if self.selected == p.device {
                                self.active = Some(id);
                            }
                            self.say(ctx, format!("connected to {}", p.label), false);
                        }
                        Err(e) => {
                            let e = match e.downcast::<HostKeyPrompt>() {
                                Ok(prompt) => {
                                    self.host_key_dialog = Some(HostKeyDialog {
                                        device: p.device,
                                        label: p.label,
                                        cfg: p.cfg,
                                        fingerprint: prompt.fingerprint,
                                        key: prompt.key,
                                        known_hosts: prompt.known_hosts,
                                        known_fingerprints: prompt.known_fingerprints,
                                        changed: prompt.changed,
                                    });
                                    continue;
                                }
                                Err(e) => e,
                            };
                            let message = e.to_string();
                            self.connection_errors
                                .insert(p.device.clone(), message.clone());
                            self.say(ctx, format!("{}: {message}", p.label), true);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending.remove(i);
                }
            }
        }
    }

    fn poll_remote(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.pending_remote.len() {
            match self.pending_remote[i].rx.try_recv() {
                Ok(result) => {
                    let p = self.pending_remote.remove(i);
                    if let Some(tree) = self
                        .sessions
                        .iter_mut()
                        .find(|s| s.id == p.session_id)
                        .and_then(|s| s.remote_tree.as_mut())
                    {
                        match result {
                            Ok(listing) => tree.apply_listing(&p.path, listing),
                            Err(e) => {
                                let msg = e.to_string();
                                tree.fail_load(&p.path, msg.clone());
                                self.say(ctx, format!("remote files: {msg}"), true);
                            }
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_remote.remove(i);
                }
            }
        }
    }

    fn poll_remote_files(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.pending_remote_files.len() {
            match self.pending_remote_files[i].rx.try_recv() {
                Ok(result) => {
                    let p = self.pending_remote_files.remove(i);
                    match result {
                        Ok(bytes) => match self.editor.open_remote(p.session_id, &p.path, bytes) {
                            Ok(()) => self.show_editor = true,
                            Err(e) => self.say(ctx, format!("can't open remote file: {e}"), true),
                        },
                        Err(e) => self.say(ctx, format!("can't open remote file: {e}"), true),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_remote_files.remove(i);
                }
            }
        }
    }

    fn poll_remote_writes(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.pending_remote_writes.len() {
            match self.pending_remote_writes[i].rx.try_recv() {
                Ok(result) => {
                    let pending = self.pending_remote_writes.remove(i);
                    if let Some((message, error)) = self.editor.finish_remote_save(
                        &pending.key,
                        &pending.saved_data,
                        result.map_err(|error| error.to_string()),
                    ) {
                        self.say(ctx, message, error);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    let pending = self.pending_remote_writes.remove(i);
                    if let Some((message, error)) = self.editor.finish_remote_save(
                        &pending.key,
                        &pending.saved_data,
                        Err("remote save worker stopped".into()),
                    ) {
                        self.say(ctx, message, error);
                    }
                }
            }
        }
    }

    fn poll_transfer(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_transfer.as_ref() else {
            return;
        };
        match pending.rx.try_recv() {
            Ok(result) => {
                self.pending_transfer = None;
                if let Some(dialog) = self.transfer_dialog.as_mut() {
                    dialog.busy = false;
                    match result {
                        Ok(TransferResult::Preview(changes)) => {
                            dialog.changes = changes;
                            dialog.previewed = true;
                            dialog.error = None;
                        }
                        Ok(TransferResult::Complete(message)) => {
                            self.say(ctx, message, false);
                            self.transfer_dialog = None;
                        }
                        Err(error) => dialog.error = Some(error.to_string()),
                    }
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending_transfer = None;
                if let Some(dialog) = self.transfer_dialog.as_mut() {
                    dialog.busy = false;
                    dialog.error = Some("transfer worker stopped".into());
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn poll_device_info(&mut self) {
        let mut i = 0;
        while i < self.pending_device_info.len() {
            match self.pending_device_info[i].rx.try_recv() {
                Ok(result) => {
                    let pending = self.pending_device_info.remove(i);
                    if let (Ok(info), Some(session)) = (
                        result,
                        self.sessions
                            .iter_mut()
                            .find(|s| s.id == pending.session_id),
                    ) {
                        session.hostname = info.hostname;
                        session.platform = info.platform;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_device_info.remove(i);
                }
            }
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let mut failed = None;
        let configured_fallback =
            terminal_command(TerminalKind::from_setting(&self.settings.default_shell), "");
        for s in &mut self.sessions {
            if s.backend.is_none() {
                if s.device == "local" {
                    if let Some((c, r)) = s.term.take_resize() {
                        let launch = s
                            .launch
                            .take()
                            .or_else(|| configured_fallback.as_ref().ok().cloned());
                        let Some(launch) = launch else {
                            let error = configured_fallback
                                .as_ref()
                                .err()
                                .cloned()
                                .unwrap_or_else(|| "configured shell has no launch command".into());
                            s.term.feed(
                                format!("\r\n\x1b[31mcouldn't start shell: {error}\x1b[0m\r\n")
                                    .as_bytes(),
                            );
                            failed = Some(error);
                            continue;
                        };
                        match LocalPty::spawn_program(&launch.0, &launch.1, c, r) {
                            Ok(p) => s.backend = Some(Backend::Local(p)),
                            Err(e) => {
                                s.term.feed(
                                    format!("\r\n\x1b[31mcouldn't start shell: {e}\x1b[0m\r\n")
                                        .as_bytes(),
                                );
                                failed = Some(e.to_string());
                            }
                        }
                    }
                }
                continue;
            }
            let b = s.backend.as_ref().unwrap();
            if let Some((c, r)) = s.term.take_resize() {
                b.resize(c, r);
            }
            let out = b.drain();
            if !out.is_empty() {
                s.term.feed(&out);
            }
            let input = s.term.take_input();
            if !input.is_empty() {
                b.write(&input);
            }
        }
        if let Some(e) = failed {
            self.say(ctx, e, true);
        }
    }

    fn device_live(&self, id: &str) -> bool {
        id == "local"
            || self
                .sessions
                .iter()
                .any(|s| s.device == id && s.backend.is_some())
    }

    fn apply(&mut self, ctx: &egui::Context, act: Act) {
        match act {
            Act::SelectDevice(d) => {
                self.active = self.sessions.iter().find(|s| s.device == d).map(|s| s.id);
                self.selected = d;
            }
            Act::NewSession(d) => {
                match self
                    .devices
                    .iter()
                    .find(|x| x.id == d)
                    .map(|x| x.kind.clone())
                {
                    Some(DeviceKind::Local) => {
                        let terminal = TerminalKind::from_setting(&self.settings.default_shell);
                        if let Err(error) = self.open_local(terminal) {
                            self.say(ctx, error, true);
                        }
                    }
                    Some(DeviceKind::Ssh(cfg)) => self.open_ssh(cfg, ctx),
                    Some(DeviceKind::VaultRef(rid)) => self.connect_from_vault(&rid, ctx),
                    Some(DeviceKind::Command(profile)) => self.open_command_profile(profile, ctx),
                    None => {}
                }
            }
            Act::NewTab => {
                self.apply(ctx, Act::NewSession(self.selected.clone()));
            }
            Act::Activate(id) => self.active = Some(id),
            Act::Close(id) => {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    self.sessions.remove(i);
                    if self.active == Some(id) {
                        self.active = self
                            .sessions
                            .iter()
                            .filter(|s| s.device == self.selected)
                            .last()
                            .map(|s| s.id);
                    }
                }
            }
            Act::OpenFile(p) => match self.editor.open(&p) {
                Ok(()) => self.show_editor = true,
                Err(e) => self.say(ctx, format!("can't open: {e}"), true),
            },
            Act::SetRoot(p) => self.tree = Node::root(&p),
            Act::RootUp => {
                if let Some(parent) = self.tree.path.parent().map(|p| p.to_path_buf()) {
                    self.tree = Node::root(&parent)
                }
            }
            Act::RefreshTree => self.tree.refresh(),
            Act::ToggleEditor => self.show_editor = !self.show_editor,
            Act::SysInfo => {
                let command = self.active_session().map(|s| {
                    (
                        system_info::command(&s.platform, &s.shell),
                        s.backend.is_some(),
                    )
                });
                match command {
                    Some((command, true)) => {
                        if let Some(backend) =
                            self.active_session().and_then(|s| s.backend.as_ref())
                        {
                            backend.write(command.as_bytes());
                            backend.write(b"\r");
                        }
                    }
                    Some((_, false)) => self.say(ctx, "shell is still starting…", false),
                    None => self.say(ctx, "open a terminal session first", true),
                }
            }
            Act::OpenModal => {
                self.form.error = None;
                self.form.focus_host = true;
                self.form.save_to_vault =
                    self.vault.as_ref().is_some_and(|vault| vault.is_unlocked());
                self.modal = true;
            }
            Act::CloseModal => self.modal = false,
            Act::OpenSettings => self.settings_open = true,
            Act::CloseSettings => self.settings_open = false,
            Act::SetTheme(mode) => {
                self.settings.theme = mode;
                theme::apply(ctx, mode);
                if let Err(error) = self.settings.save() {
                    self.say(ctx, format!("couldn't save settings: {error}"), true);
                }
            }
            Act::SetDefaultShell(terminal) => {
                self.settings.default_shell = terminal.setting_id().into();
                if let Err(error) = self.settings.save() {
                    self.say(ctx, format!("couldn't save settings: {error}"), true);
                }
            }
            Act::SetVaultGrace(value, unit) => {
                self.settings.vault_grace_unit = unit;
                self.settings.vault_grace_value = value.max(1).min(unit.max_value());
                if let Err(error) = self.settings.save() {
                    self.say(ctx, format!("couldn't save settings: {error}"), true);
                }
                let remember_result = self.vault.as_mut().and_then(|vault| {
                    (vault.mode() == VaultMode::Password && vault.is_unlocked())
                        .then(|| vault.remember_for(self.settings.vault_grace_seconds()))
                });
                if let Some(Err(error)) = remember_result {
                    self.say(
                        ctx,
                        format!("couldn't remember vault unlock: {error}"),
                        true,
                    );
                }
            }
            Act::Connect => {
                let f = &self.form;
                let connection = f.connection;
                let host = f.host.trim().to_string();
                let user = f.user.trim().to_string();
                let port_text = f.port.trim().to_string();
                let label = f.label.trim().to_string();
                if !matches!(connection, ConnectionType::Shell | ConnectionType::Wsl)
                    && host.is_empty()
                {
                    self.form.error = Some(match connection {
                        ConnectionType::File => "A directory path is required".into(),
                        ConnectionType::Serial => "A serial device is required".into(),
                        ConnectionType::Browser => "A URL is required".into(),
                        _ => "A host is required".into(),
                    });
                    return;
                }
                if connection.uses_ssh_auth() && user.is_empty() {
                    self.form.error = Some("A user is required".into());
                    return;
                }

                if connection == ConnectionType::File {
                    let path = PathBuf::from(&host);
                    if !path.is_dir() {
                        self.form.error = Some("That directory does not exist".into());
                        return;
                    }
                    self.tree = Node::root(&path);
                    self.selected = "local".into();
                    self.modal = false;
                    return;
                }

                let profile = CommandProfile {
                    kind: connection,
                    terminal: f.terminal,
                    host: host.clone(),
                    port: port_text.clone(),
                    user: user.clone(),
                };
                if matches!(
                    connection,
                    ConnectionType::Browser
                        | ConnectionType::Rdp
                        | ConnectionType::Vnc
                        | ConnectionType::Xdmcp
                ) {
                    match launch_external(&profile) {
                        Ok(()) => self.modal = false,
                        Err(error) => self.form.error = Some(error),
                    }
                    return;
                }

                if !connection.uses_ssh_auth() {
                    self.modal = false;
                    self.open_command_profile(profile, ctx);
                    return;
                }

                let port = port_text.parse().unwrap_or(22);
                let auth = match f.mode {
                    AuthMode::Password => SshAuth::Password(f.password.clone()),
                    AuthMode::Key => SshAuth::KeyFile {
                        path: f.key_path.trim().to_string(),
                        passphrase: None,
                    },
                    AuthMode::Agent => SshAuth::Agent,
                };
                let save = f.save_to_vault;
                let secret = if save {
                    Some(match &auth {
                        SshAuth::Password(p) => VaultSecret::Password {
                            password: p.to_string(),
                        },
                        SshAuth::KeyFile { path, passphrase } => VaultSecret::Key {
                            key_path: path.clone(),
                            passphrase: passphrase.as_ref().map(|value| value.to_string()),
                        },
                        SshAuth::Agent => VaultSecret::Agent,
                    })
                } else {
                    None
                };
                let cfg = SshConfig {
                    host: host.clone(),
                    port,
                    user: user.clone(),
                    auth,
                    trust_host_key: None,
                    replace_host_key: false,
                    known_fingerprints: Vec::new(),
                };
                self.modal = false;
                self.form.password.zeroize();
                self.form.label.clear();

                let mut saved_device = None;
                if let (Some(sec), Some(v)) = (secret, self.vault.as_mut()) {
                    if v.is_unlocked() {
                        match v.upsert(&label, &host, port, &user, &sec) {
                            Ok(record_id) => {
                                let saved_label = if label.is_empty() {
                                    format!("{user}@{host}")
                                } else {
                                    label.clone()
                                };
                                saved_device = Some((format!("vault:{record_id}"), saved_label));
                                self.load_saved_devices();
                                self.say(ctx, "Saved to vault", false);
                            }
                            Err(e) => self.say(ctx, format!("vault save failed: {e}"), true),
                        }
                    } else {
                        self.say(ctx, "Vault is locked — connection made but not saved", true);
                    }
                }
                self.open_ssh_on_device(cfg, saved_device, ctx);
            }
            Act::VaultChooseMode(m) => {
                if let VaultState::Setup {
                    choice,
                    pw,
                    pw2,
                    err,
                } = &mut self.vault_ui
                {
                    *choice = m;
                    *err = None;
                    if m == VaultMode::Os {
                        pw.zeroize();
                        pw2.zeroize();
                    }
                }
            }
            Act::VaultCreate => {
                let (mode, master) = {
                    let VaultState::Setup {
                        choice,
                        pw,
                        pw2,
                        err,
                    } = &mut self.vault_ui
                    else {
                        return;
                    };
                    if *choice == VaultMode::Password {
                        if pw.len() < 8 {
                            *err = Some("Master password must be at least 8 characters".into());
                            return;
                        }
                        if pw != pw2 {
                            *err = Some("Passwords don't match".into());
                            return;
                        }
                    }
                    // Keep the setup fields intact until the freshly written
                    // vault has been reopened and verified with these exact
                    // bytes. This prevents accepting a vault that the same UI
                    // value cannot immediately unlock.
                    let master = (*choice == VaultMode::Password).then(|| pw.clone());
                    (*choice, master)
                };
                match Vault::create(mode, master.as_ref().map(|value| value.as_str())) {
                    Ok(mut v) => {
                        if mode == VaultMode::Password {
                            v.lock();
                            if let Err(error) =
                                v.unlock(master.as_ref().map(|value| value.as_str()))
                            {
                                let _ = v.destroy();
                                if let VaultState::Setup { err, .. } = &mut self.vault_ui {
                                    *err = Some(format!(
                                        "Vault verification failed before use: {error}"
                                    ));
                                }
                                return;
                            }
                        }
                        let remember_error = (mode == VaultMode::Password)
                            .then(|| v.remember_for(self.settings.vault_grace_seconds()))
                            .and_then(Result::err);
                        self.vault = Some(v);
                        self.vault_ui = VaultState::Ready;
                        if let Some(error) = remember_error {
                            self.say(
                                ctx,
                                format!("Vault created, but couldn't remember unlock: {error}"),
                                true,
                            );
                        } else {
                            self.say(ctx, "Vault created", false);
                        }
                    }
                    Err(e) => {
                        if let VaultState::Setup { err, .. } = &mut self.vault_ui {
                            *err = Some(e.to_string());
                        }
                    }
                }
            }
            Act::VaultUnlock => {
                let VaultState::Unlock { pw, err, .. } = &mut self.vault_ui else {
                    return;
                };
                let Some(v) = self.vault.as_mut() else { return };
                let input = pw.clone();
                let trimmed = input.trim();
                let result = match v.unlock(Some(&input)) {
                    Err(UnlockError::BadPassword) if trimmed != input.as_str() => {
                        v.unlock(Some(trimmed))
                    }
                    result => result,
                };
                match result {
                    Ok(()) => {
                        pw.zeroize();
                        let remember_error =
                            v.remember_for(self.settings.vault_grace_seconds()).err();
                        self.vault_ui = VaultState::Ready;
                        self.load_saved_devices();
                        if let Some(error) = remember_error {
                            self.say(
                                ctx,
                                format!(
                                    "Vault unlocked for this session; remember failed: {error}"
                                ),
                                true,
                            );
                        } else {
                            self.say(ctx, "Vault unlocked", false);
                        }
                    }
                    Err(UnlockError::NeedPassword) => {
                        *err = Some("Enter your master password".into())
                    }
                    Err(UnlockError::BadPassword) => {
                        *err = Some(
                            "Wrong password. Your entry was left in place so you can reveal and verify it."
                                .into(),
                        )
                    }
                    Err(UnlockError::Other(e)) => *err = Some(e.to_string()),
                }
            }
            Act::VaultOsRetry => {
                let Some(v) = self.vault.as_mut() else { return };
                if v.mode() != VaultMode::Os {
                    return;
                }
                match v.unlock(None) {
                    Ok(()) => {
                        self.vault_ui = VaultState::Ready;
                        self.load_saved_devices();
                        self.say(ctx, "Vault unlocked with OS sign-in", false);
                    }
                    Err(e) => {
                        self.vault_ui = VaultState::KeystoreError {
                            err: os_unlock_error(&e),
                        };
                    }
                }
            }
            Act::VaultLock => {
                if let Some(v) = self.vault.as_mut() {
                    v.lock_and_forget();
                    self.devices
                        .retain(|d| !matches!(d.kind, DeviceKind::VaultRef(_)));
                    self.vault_ui = if v.mode() == VaultMode::Os {
                        VaultState::KeystoreError {
                            err: "Vault locked. Retry OS sign-in to unlock it.".into(),
                        }
                    } else {
                        VaultState::Unlock {
                            pw: Zeroizing::new(String::new()),
                            err: None,
                            focus: true,
                        }
                    };
                    self.say(ctx, "Vault locked", false);
                }
            }
            Act::DeleteDevice(device_id) => {
                if device_id == "local" {
                    return;
                }
                let Some(kind) = self
                    .devices
                    .iter()
                    .find(|device| device.id == device_id)
                    .map(|device| device.kind.clone())
                else {
                    return;
                };
                if let DeviceKind::VaultRef(record_id) = kind {
                    let Some(vault) = self.vault.as_mut() else {
                        self.say(ctx, "Vault is unavailable", true);
                        return;
                    };
                    if let Err(error) = vault.remove(&record_id) {
                        self.say(ctx, format!("delete failed: {error}"), true);
                        return;
                    }
                }
                let removed_sessions: Vec<u64> = self
                    .sessions
                    .iter()
                    .filter(|session| session.device == device_id)
                    .map(|session| session.id)
                    .collect();
                self.sessions.retain(|session| session.device != device_id);
                self.pending.retain(|pending| pending.device != device_id);
                self.pending_remote
                    .retain(|pending| !removed_sessions.contains(&pending.session_id));
                self.pending_remote_files
                    .retain(|pending| !removed_sessions.contains(&pending.session_id));
                self.pending_remote_writes
                    .retain(|pending| !removed_sessions.contains(&pending.session_id));
                self.pending_device_info
                    .retain(|pending| !removed_sessions.contains(&pending.session_id));
                if self
                    .pending_transfer
                    .as_ref()
                    .is_some_and(|pending| removed_sessions.contains(&pending.session_id))
                {
                    self.pending_transfer = None;
                    self.transfer_dialog = None;
                }
                self.devices.retain(|device| device.id != device_id);
                self.connection_errors.remove(&device_id);
                if self.selected == device_id {
                    self.selected = "local".into();
                    self.active = self
                        .sessions
                        .iter()
                        .find(|session| session.device == "local")
                        .map(|session| session.id);
                } else if self
                    .active
                    .is_some_and(|active| removed_sessions.contains(&active))
                {
                    self.active = self
                        .sessions
                        .iter()
                        .find(|session| session.device == self.selected)
                        .map(|session| session.id);
                }
                self.say(ctx, "Device deleted", false);
            }
            Act::VaultConnect(rid) => self.connect_from_vault(&rid, ctx),
            Act::VaultResetBegin => {
                self.vault_ui = VaultState::ResetConfirm {
                    confirm: String::new(),
                    err: None,
                    focus: true,
                };
            }
            Act::VaultResetCancel => {
                self.vault_ui = if self
                    .vault
                    .as_ref()
                    .is_some_and(|vault| vault.mode() == VaultMode::Os)
                {
                    VaultState::KeystoreError {
                        err: "OS sign-in is required to unlock this vault.".into(),
                    }
                } else {
                    VaultState::Unlock {
                        pw: Zeroizing::new(String::new()),
                        err: None,
                        focus: true,
                    }
                };
            }
            Act::VaultReset => {
                let VaultState::ResetConfirm { confirm, err, .. } = &mut self.vault_ui else {
                    return;
                };
                if confirm.trim() != "RESET" {
                    *err = Some("Type RESET to confirm".into());
                    return;
                }
                let result = self.vault.as_mut().map_or(Ok(()), Vault::destroy);
                match result {
                    Ok(()) => {
                        self.vault = None;
                        self.devices
                            .retain(|d| !matches!(d.kind, DeviceKind::VaultRef(_)));
                        self.selected = "local".into();
                        self.active = self
                            .sessions
                            .iter()
                            .find(|s| s.device == "local")
                            .map(|s| s.id);
                        self.vault_ui = VaultState::Setup {
                            choice: VaultMode::Os,
                            pw: Zeroizing::new(String::new()),
                            pw2: Zeroizing::new(String::new()),
                            err: None,
                        };
                        self.say(ctx, "Vault reset. Saved credentials were deleted.", false);
                    }
                    Err(e) => *err = Some(format!("Couldn't reset vault: {e}")),
                }
            }
            Act::RemoteLoad(id, path) => {
                let work = self.sessions.iter_mut().find(|s| s.id == id).and_then(|s| {
                    let tree = s.remote_tree.as_mut()?;
                    if !tree.begin_load(&path) {
                        return None;
                    }
                    Some(s.backend.as_ref()?.sftp()?)
                });
                if let Some(sftp) = work {
                    self.spawn_remote_load(id, path, sftp);
                }
            }
            Act::RemoteSetRoot(id, path) => {
                let sftp = self.sessions.iter_mut().find(|s| s.id == id).and_then(|s| {
                    s.remote_tree = Some(RemoteNode::root(path.clone()));
                    s.backend.as_ref()?.sftp()
                });
                if let Some(sftp) = sftp {
                    self.spawn_remote_load(id, path, sftp);
                }
            }
            Act::RemoteUp(id) => {
                let parent = self
                    .sessions
                    .iter()
                    .find(|s| s.id == id)
                    .and_then(|s| s.remote_tree.as_ref())
                    .and_then(|t| fs_tree::remote_parent(&t.path));
                if let Some(parent) = parent {
                    self.apply(ctx, Act::RemoteSetRoot(id, parent));
                }
            }
            Act::RemoteRefresh(id) => {
                let path = self
                    .sessions
                    .iter()
                    .find(|s| s.id == id)
                    .and_then(|s| s.remote_tree.as_ref())
                    .map(|t| t.path.clone());
                if let Some(path) = path {
                    self.apply(ctx, Act::RemoteSetRoot(id, path));
                }
            }
            Act::RemoteOpen(id, path) => {
                if self
                    .pending_remote_files
                    .iter()
                    .any(|p| p.session_id == id && p.path == path)
                {
                    return;
                }
                let sftp = self
                    .sessions
                    .iter()
                    .find(|s| s.id == id)
                    .and_then(|s| s.backend.as_ref()?.sftp());
                if let Some(sftp) = sftp {
                    self.spawn_remote_file(id, path, sftp);
                    self.say(ctx, "opening remote file…", false);
                }
            }
            Act::RemoteDownload(id, remote, recursive) => {
                let name = remote
                    .trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("download")
                    .to_string();
                let base = dirs::download_dir()
                    .or_else(dirs::home_dir)
                    .unwrap_or_default();
                self.transfer_dialog = Some(TransferDialog {
                    session_id: id,
                    remote,
                    local: base.join(&name).display().to_string(),
                    mode: if recursive {
                        TransferMode::DownloadFolder
                    } else {
                        TransferMode::DownloadFile
                    },
                    direction: SyncDirection::RemoteToLocal,
                    changes: Vec::new(),
                    previewed: false,
                    busy: false,
                    error: None,
                });
            }
            Act::RemoteUpload(id, remote, recursive) => {
                self.transfer_dialog = Some(TransferDialog {
                    session_id: id,
                    remote,
                    local: self.tree.path.display().to_string(),
                    mode: if recursive {
                        TransferMode::UploadFolder
                    } else {
                        TransferMode::UploadFile
                    },
                    direction: SyncDirection::LocalToRemote,
                    changes: Vec::new(),
                    previewed: false,
                    busy: false,
                    error: None,
                });
            }
            Act::RemoteSync(id, remote) => {
                let local = self.tree.path.display().to_string();
                let error = sync_root_too_broad(&local).then(|| {
                    "Choose the local project folder. Syncing your entire home directory is blocked."
                        .to_string()
                });
                self.transfer_dialog = Some(TransferDialog {
                    session_id: id,
                    remote,
                    local,
                    mode: TransferMode::Sync,
                    direction: SyncDirection::RemoteToLocal,
                    changes: Vec::new(),
                    previewed: false,
                    busy: false,
                    error,
                });
            }
            Act::TransferPickLocalFolder => {
                let start = self
                    .transfer_dialog
                    .as_ref()
                    .map(|dialog| dialog.local.clone())
                    .unwrap_or_default();
                match pick_local_folder(&start) {
                    Ok(Some(path)) => {
                        if let Some(dialog) = self.transfer_dialog.as_mut() {
                            dialog.local = path;
                            dialog.previewed = false;
                            dialog.changes.clear();
                            dialog.error = None;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if let Some(dialog) = self.transfer_dialog.as_mut() {
                            dialog.error = Some(error);
                        }
                    }
                }
            }
            current @ (Act::TransferPreview | Act::TransferApply) => {
                if self.pending_transfer.is_some() {
                    return;
                }
                let Some(dialog) = self.transfer_dialog.as_mut() else {
                    return;
                };
                let Some(sftp) = self
                    .sessions
                    .iter()
                    .find(|session| session.id == dialog.session_id)
                    .and_then(|session| session.backend.as_ref()?.sftp())
                else {
                    dialog.error = Some("SSH session is no longer available".into());
                    return;
                };
                let preview = matches!(current, Act::TransferPreview);
                if preview && dialog.mode != TransferMode::Sync {
                    return;
                }
                let session_id = dialog.session_id;
                let mode = dialog.mode;
                let direction = dialog.direction;
                let local = dialog.local.trim().to_string();
                let remote = dialog.remote.trim().to_string();
                if local.is_empty() || remote.is_empty() {
                    dialog.error = Some("Both local and remote paths are required".into());
                    return;
                }
                if mode == TransferMode::Sync && sync_root_too_broad(&local) {
                    dialog.error = Some(
                        "Choose the local project folder. Syncing your entire home directory is blocked."
                            .into(),
                    );
                    return;
                }
                dialog.busy = true;
                dialog.error = None;
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let result = if preview {
                        sftp.sync_plan(&remote, &local, direction.protocol())
                            .map(TransferResult::Preview)
                    } else {
                        let operation = match mode {
                            TransferMode::DownloadFile => sftp.download(&remote, &local, false),
                            TransferMode::DownloadFolder => sftp.download(&remote, &local, true),
                            TransferMode::UploadFile => {
                                let name = PathBuf::from(&local)
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "upload".into());
                                let target = fs_tree::remote_join(&remote, &name);
                                sftp.upload(&local, &target, false)
                            }
                            TransferMode::UploadFolder => sftp.upload(&local, &remote, true),
                            TransferMode::Sync => {
                                sftp.sync_apply(&remote, &local, direction.protocol())
                            }
                        };
                        operation.map(TransferResult::Complete)
                    };
                    let _ = tx.send(result);
                    wake::poke();
                });
                self.pending_transfer = Some(PendingTransfer { session_id, rx });
            }
            Act::TransferClose => {
                if !self
                    .transfer_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.busy)
                {
                    self.transfer_dialog = None;
                }
            }
            Act::TrustHostKey => {
                if let Some(mut prompt) = self.host_key_dialog.take() {
                    prompt.cfg.trust_host_key = Some(prompt.key);
                    prompt.cfg.replace_host_key = prompt.changed;
                    prompt.cfg.known_fingerprints = prompt.known_fingerprints;
                    self.open_ssh_on_device(prompt.cfg, Some((prompt.device, prompt.label)), ctx);
                }
            }
            Act::RejectHostKey => {
                if let Some(prompt) = self.host_key_dialog.take() {
                    let message = if prompt.changed {
                        "Host key replacement declined"
                    } else {
                        "Host key was not trusted"
                    };
                    self.connection_errors
                        .insert(prompt.device.clone(), message.into());
                    self.say(
                        ctx,
                        format!("connection to {} cancelled: {message}", prompt.label),
                        false,
                    );
                }
            }
        }
    }

    fn active_session(&self) -> Option<&Session> {
        self.active.and_then(|id| {
            self.sessions
                .iter()
                .find(|s| s.id == id && s.device == self.selected)
        })
    }

    fn shortcuts(&mut self, ctx: &egui::Context, acts: &mut Vec<Act>) {
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::CTRL, Key::T) {
                acts.push(Act::NewTab);
            }
            let cs = Modifiers::COMMAND | Modifiers::SHIFT;
            if i.consume_key(cs, Key::W) {
                if let Some(id) = self.active {
                    acts.push(Act::Close(id));
                }
            }
            if i.consume_key(cs, Key::E) {
                acts.push(Act::ToggleEditor);
            }
        });
    }
}

// ───────────────────────────── frame loop ─────────────────────────────

impl eframe::App for OpenTerm {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        bg().to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let mut acts = Vec::new();
        if let Some(inner) = ctx.input(|input| input.viewport().inner_rect) {
            let minimum = vec2(crate::MIN_WINDOW_SIZE[0], crate::MIN_WINDOW_SIZE[1]);
            if inner.width() + 1.0 < minimum.x || inner.height() + 1.0 < minimum.y {
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(
                    inner.width().max(minimum.x),
                    inner.height().max(minimum.y),
                )));
            }
        }
        let (vault_expired, vault_remaining) = self
            .vault
            .as_mut()
            .map(|vault| (vault.expire_if_needed(), vault.unlock_remaining()))
            .unwrap_or((false, None));
        if vault_expired {
            self.devices
                .retain(|device| !matches!(device.kind, DeviceKind::VaultRef(_)));
            if !self.devices.iter().any(|device| device.id == self.selected) {
                self.selected = "local".into();
                self.active = self
                    .sessions
                    .iter()
                    .find(|session| session.device == "local")
                    .map(|session| session.id);
            }
            self.vault_ui = VaultState::Unlock {
                pw: Zeroizing::new(String::new()),
                err: Some("Unlock time expired. Enter your master password again.".into()),
                focus: true,
            };
            self.say(ctx, "Vault unlock expired", false);
        } else if let Some(remaining) = vault_remaining {
            ctx.request_repaint_after(remaining.max(std::time::Duration::from_secs(1)));
        }
        self.poll_pending(ctx);
        self.poll_remote(ctx);
        self.poll_remote_files(ctx);
        self.poll_remote_writes(ctx);
        self.poll_transfer(ctx);
        self.poll_device_info();
        self.shortcuts(ctx, &mut acts);
        self.pump(ctx);

        titlebar(ctx, self, &mut acts);
        statusbar(ctx, self);
        sidebar(ctx, self, &mut acts);
        workspace(ctx, self, &mut acts);
        if let Some(save) = self.editor.take_remote_save() {
            if self
                .pending_remote_writes
                .iter()
                .any(|pending| pending.key == save.key)
            {
                // Keep the latest requested buffer queued until the current
                // write has confirmed completion.
                self.editor.requeue_remote_save(save);
            } else if let Err((save, error)) = self.spawn_remote_write(save) {
                if let Some((message, is_error)) =
                    self.editor
                        .finish_remote_save(&save.key, &save.data, Err(error))
                {
                    self.say(ctx, message, is_error);
                }
            }
        }
        vault_dialog(ctx, &mut self.vault_ui, &mut acts);
        if self.settings_open && matches!(self.vault_ui, VaultState::Ready | VaultState::Idle) {
            settings_dialog(ctx, &self.settings, &mut acts);
        }
        if self.modal
            && !self.settings_open
            && matches!(self.vault_ui, VaultState::Ready | VaultState::Idle)
        {
            ssh_modal(
                ctx,
                &mut self.form,
                &mut acts,
                self.vault.is_some() && self.vault.as_ref().unwrap().is_unlocked(),
            );
        }
        if let Some(dialog) = self.transfer_dialog.as_mut() {
            transfer_dialog(ctx, dialog, &mut acts);
        }
        if let Some(prompt) = self.host_key_dialog.as_ref() {
            host_key_dialog(ctx, prompt, &mut acts);
        }

        for a in acts {
            self.apply(ctx, a);
        }
        self.pump(ctx); // flush this frame's keystrokes immediately

        #[cfg(not(target_os = "macos"))]
        window_edges(ctx);

        if let Some((_, _, until)) = &self.flash {
            let left = until - ctx.input(|i| i.time);
            if left <= 0.0 {
                self.flash = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
            }
        }
    }
}

// ───────────────────────────── chrome ─────────────────────────────

const TITLE_H: f32 = 38.0;

fn titlebar(ctx: &egui::Context, app: &mut OpenTerm, acts: &mut Vec<Act>) {
    egui::TopBottomPanel::top("ot_titlebar")
        .exact_height(TITLE_H)
        .show_separator_line(false)
        .frame(Frame::none().fill(bg2()))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let p = ui.painter().clone();
            p.line_segment(
                [rect.left_bottom(), rect.right_bottom()],
                Stroke::new(1.0_f32, border()),
            );

            if let Some(maximized) = ctx.input(|i| i.viewport().maximized) {
                app.window_maximized = maximized;
            }
            let native_controls_left = if cfg!(target_os = "macos") {
                rect.right()
            } else {
                rect.right() - 138.0
            };
            let settings_rect = (cfg!(target_os = "macos") || rect.width() >= 310.0).then(|| {
                let x = if cfg!(target_os = "macos") {
                    rect.right() - 20.0
                } else {
                    native_controls_left - 18.0
                };
                Rect::from_center_size(pos2(x, rect.center().y), vec2(30.0, 28.0))
            });

            // Keep the drag hit target completely clear of settings and native
            // window controls. A full-width drag interaction swallowed their
            // clicks on undecorated Windows/Linux windows.
            let drag_rect = Rect::from_min_max(
                rect.min,
                pos2(
                    settings_rect
                        .map(|settings| settings.left() - 4.0)
                        .unwrap_or(native_controls_left)
                        .max(rect.left()),
                    rect.bottom(),
                ),
            );
            let drag = ui.interact(drag_rect, Id::new("ot_drag"), Sense::click_and_drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                toggle_max(ctx, &mut app.window_maximized);
            }

            let left = rect.left()
                + if cfg!(target_os = "macos") {
                    78.0
                } else {
                    12.0
                };
            if cfg!(target_os = "macos") || rect.width() >= 300.0 {
                let badge =
                    Rect::from_min_size(pos2(left, rect.center().y - 11.0), vec2(22.0, 22.0));
                p.rect(
                    badge,
                    Rounding::same(6.0),
                    bg4(),
                    Stroke::new(1.0_f32, border_d()),
                );
                icons::prompt(&p, badge.center(), fg());
                p.text(
                    pos2(badge.right() + 9.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "OpenTerm",
                    medium(12.5),
                    fg(),
                );
            }
            if rect.width() >= 620.0 {
                p.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Workspace",
                    ui_f(),
                    fg_dim(),
                );
            }

            if let Some(settings_rect) = settings_rect {
                if icon_at(
                    ui,
                    settings_rect,
                    Id::new("ot_settings"),
                    "settings",
                    |p, c, col| icons::gear(p, c, if app.settings_open { fg() } else { col }),
                )
                .clicked()
                {
                    acts.push(Act::OpenSettings);
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                let max = app.window_maximized;
                let w = 46.0;
                let mut x = rect.right() - w;
                for kind in [2u8, 1, 0] {
                    let r = Rect::from_min_size(pos2(x, rect.top()), vec2(w, TITLE_H - 1.0));
                    let resp = ui.interact(r, Id::new(("ot_wc", kind)), Sense::click());
                    let col = if resp.hovered() { fg() } else { fg_dim() };
                    if resp.hovered() {
                        p.rect_filled(
                            r,
                            Rounding::ZERO,
                            if kind == 2 { close_hover() } else { bg4() },
                        );
                    }
                    match kind {
                        0 => icons::minimize(&p, r.center(), col),
                        1 => icons::maximize(&p, r.center(), !max, col),
                        _ => icons::cross(
                            &p,
                            r.center(),
                            4.6,
                            if resp.hovered() {
                                Color32::WHITE
                            } else {
                                fg_dim()
                            },
                        ),
                    }
                    if resp.clicked() {
                        match kind {
                            0 => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                            1 => toggle_max(ctx, &mut app.window_maximized),
                            _ => ctx.send_viewport_cmd(ViewportCommand::Close),
                        }
                    }
                    x -= w;
                }
            }
            let _ = (app, acts);
        });
}

fn toggle_max(ctx: &egui::Context, maximized: &mut bool) {
    *maximized = !*maximized;
    ctx.send_viewport_cmd(ViewportCommand::Maximized(*maximized));
}

fn platform_label(platform: &str) -> &str {
    let lower = platform.to_ascii_lowercase();
    if lower.contains("windows") || lower.contains("mingw") || lower.contains("cygwin") {
        "Windows"
    } else if lower.contains("darwin") || lower.contains("macos") || lower.contains("mac os") {
        "macOS"
    } else if lower.contains("linux") {
        "Linux"
    } else if platform.trim().is_empty() {
        "Unknown"
    } else {
        platform
    }
}

fn os_unlock_error(error: &UnlockError) -> String {
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

fn command_for_profile(profile: &CommandProfile) -> Result<(String, Vec<String>, String), String> {
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
            return Err(format!(
                "{} opens outside the terminal",
                profile.kind.label()
            ))
        }
    };
    Ok((command.0, command.1, label))
}

fn terminal_command(
    kind: TerminalKind,
    wsl_distribution: &str,
) -> Result<(String, Vec<String>), String> {
    match kind {
        TerminalKind::PowerShell => Ok((
            pty::powershell_shell().ok_or_else(|| "PowerShell is not installed".to_string())?,
            vec!["-NoLogo".into()],
        )),
        TerminalKind::Wsl => {
            if !cfg!(windows) {
                return Err("WSL terminals are available on Windows only".into());
            }
            let mut args = Vec::new();
            if !wsl_distribution.is_empty() {
                args.extend(["-d".into(), wsl_distribution.into()]);
            }
            Ok(("wsl.exe".into(), args))
        }
        TerminalKind::Bash => Ok((pty::bash_shell(), vec!["-l".into()])),
        TerminalKind::Cmd => {
            if !cfg!(windows) {
                return Err("CMD terminals are available on Windows only".into());
            }
            Ok((pty::cmd_shell(), Vec::new()))
        }
    }
}

fn profile_shell(profile: &CommandProfile) -> &str {
    match profile.kind {
        ConnectionType::Shell => profile.terminal.shell_name(),
        ConnectionType::Wsl => "bash",
        _ => "unknown",
    }
}

fn profile_platform(profile: &CommandProfile) -> &str {
    match profile.kind {
        ConnectionType::Wsl => "linux",
        ConnectionType::Shell if profile.terminal == TerminalKind::Wsl => "linux",
        ConnectionType::Shell if profile.terminal == TerminalKind::Bash && cfg!(windows) => "linux",
        _ => std::env::consts::OS,
    }
}

fn launch_external(profile: &CommandProfile) -> Result<(), String> {
    use std::process::Command;

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
            return Err(format!(
                "{} is not an external connection",
                profile.kind.label()
            ))
        }
    };
    Command::new(&program)
        .args(&args)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("couldn't launch {program}: {error}"))
}

fn statusbar(ctx: &egui::Context, app: &OpenTerm) {
    egui::TopBottomPanel::bottom("ot_status")
        .exact_height(24.0)
        .show_separator_line(false)
        .frame(Frame::none().fill(bg2()))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let p = ui.painter();
            p.line_segment(
                [rect.left_top(), rect.right_top()],
                Stroke::new(1.0_f32, border()),
            );
            let y = rect.center().y;
            let mut x = rect.left() + 12.0;
            let live = app.device_live(&app.selected);
            icons::dot(
                p,
                pos2(x + 3.0, y),
                3.0,
                if live { signal() } else { fg_muted() },
            );
            x += 12.0;
            let dev = app
                .devices
                .iter()
                .find(|d| d.id == app.selected)
                .map(|d| d.label.clone())
                .unwrap_or_default();
            let mut parts = vec![(dev, false)];
            if let Some(s) = app.active_session() {
                if !s.hostname.trim().is_empty() {
                    parts.push((s.hostname.clone(), false));
                }
                parts.push((platform_label(&s.platform).to_string(), true));
                let (c, r) = s.term.size();
                parts.push((format!("{c}×{r}"), false));
            }
            for (i, (part, is_platform)) in parts.iter().enumerate() {
                if i > 0 {
                    let r = p.text(
                        pos2(x, y),
                        egui::Align2::LEFT_CENTER,
                        "·",
                        sans(11.0),
                        fg_muted(),
                    );
                    x = r.right() + 7.0;
                }
                if *is_platform {
                    icons::platform(p, pos2(x + 6.0, y), part, fg_muted());
                    x += 15.0;
                }
                let r = p.text(
                    pos2(x, y),
                    egui::Align2::LEFT_CENTER,
                    part,
                    mono(11.0),
                    fg_muted(),
                );
                x = r.right() + 7.0;
            }
            let (msg, col) = match &app.flash {
                Some((m, err, _)) => (m.clone(), if *err { red() } else { fg_dim() }),
                None => ("openterm 0.1".to_string(), fg_muted()),
            };
            p.text(
                pos2(rect.right() - 12.0, y),
                egui::Align2::RIGHT_CENTER,
                msg,
                sans(11.0),
                col,
            );
        });
}

// ───────────────────────────── sidebar ─────────────────────────────

fn sidebar(ctx: &egui::Context, app: &mut OpenTerm, acts: &mut Vec<Act>) {
    egui::SidePanel::left("ot_side_v2")
        .resizable(true)
        .default_width(240.0)
        .width_range(200.0..=380.0)
        .show_separator_line(true)
        .frame(Frame::none().fill(bg2()))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add_space(8.0);
            section(ui, "DEVICES", |ui| {
                if icon_btn(ui, "add device", |p, c, col| icons::plus(p, c, 4.5, col)).clicked() {
                    acts.push(Act::OpenModal);
                }
            });

            for d in &app.devices {
                let selected = app.selected == d.id;
                let live = app.device_live(&d.id);
                let pending = app.pending.iter().any(|p| p.device == d.id);
                let removable = !matches!(d.kind, DeviceKind::Local);
                let resp = row(ui, 32.0, selected, |p, r, hovered| {
                    let ic = pos2(r.left() + 16.0, r.center().y);
                    let failed = app.connection_errors.contains_key(&d.id);
                    let col = if selected || hovered { fg() } else { fg_dim() };
                    match &d.kind {
                        DeviceKind::Local => icons::laptop(p, ic, col),
                        _ => icons::server(p, ic, col),
                    }
                    let gal = elide(p, &d.label, ui_f(), col, r.width() - 56.0);
                    p.galley(
                        pos2(r.left() + 32.0, r.center().y - gal.size().y / 2.0),
                        gal,
                        col,
                    );
                    let dc = pos2(r.right() - 14.0, r.center().y);
                    if removable && hovered {
                        icons::cross(p, dc, 4.0, if selected { fg() } else { fg_dim() });
                    } else if pending {
                        let t = p.ctx().input(|i| i.time) as f32;
                        icons::dot(
                            p,
                            dc,
                            3.5,
                            fg_dim().gamma_multiply(0.5 + 0.5 * (t * 4.0).sin().abs()),
                        );
                        p.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(60));
                    } else {
                        icons::dot(
                            p,
                            dc,
                            3.5,
                            if failed {
                                red()
                            } else if live {
                                signal()
                            } else {
                                fg_muted()
                            },
                        );
                    }
                });
                let delete_clicked = if removable {
                    let delete_rect = Rect::from_center_size(
                        pos2(resp.rect.right() - 14.0, resp.rect.center().y),
                        vec2(26.0, 28.0),
                    );
                    ui.interact(
                        delete_rect,
                        Id::new(("delete_device", d.id.as_str())),
                        Sense::click(),
                    )
                    .on_hover_text("Delete device")
                    .clicked()
                } else {
                    false
                };
                if delete_clicked {
                    acts.push(Act::DeleteDevice(d.id.clone()));
                } else if resp.double_clicked() {
                    acts.push(Act::NewSession(d.id.clone()));
                } else if resp.clicked() {
                    acts.push(Act::SelectDevice(d.id.clone()));
                }
                if !delete_clicked {
                    if let Some(error) = app.connection_errors.get(&d.id) {
                        resp.on_hover_text(format!(
                            "Connection failed: {error}\n\nDouble-click to retry"
                        ));
                    } else {
                        resp.on_hover_text("double-click for a new session");
                    }
                }
            }
            let add = row(ui, 30.0, false, |p, r, hovered| {
                let col = if hovered { fg_dim() } else { fg_muted() };
                icons::plus(p, pos2(r.left() + 16.0, r.center().y), 4.0, col);
                p.text(
                    pos2(r.left() + 32.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Add device",
                    ui_f(),
                    col,
                );
            });
            if add.clicked() {
                acts.push(Act::OpenModal);
            }

            ui.add_space(10.0);
            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().line_segment(
                [line.left_center(), line.right_center()],
                Stroke::new(1.0_f32, border()),
            );
            ui.add_space(8.0);

            let remote_session = app.active.and_then(|id| {
                app.sessions
                    .iter()
                    .position(|s| s.id == id && s.device == app.selected && s.remote_tree.is_some())
            });
            if let Some(index) = remote_session {
                let session_id = app.sessions[index].id;
                section(ui, "REMOTE FILES", |ui| {
                    if icon_btn(ui, "refresh remote files", icons::refresh).clicked() {
                        acts.push(Act::RemoteRefresh(session_id));
                    }
                    if icon_btn(ui, "parent folder", icons::up).clicked() {
                        acts.push(Act::RemoteUp(session_id));
                    }
                });
                let tree = app.sessions[index].remote_tree.as_mut().unwrap();
                let root_label = tree.path.clone();
                let r = row(ui, 26.0, false, |p, r, _| {
                    let gal = elide(p, &root_label, mono(11.5), fg_muted(), r.width() - 24.0);
                    p.galley(
                        pos2(r.left() + 12.0, r.center().y - gal.size().y / 2.0),
                        gal,
                        fg_muted(),
                    );
                });
                let r = r.on_hover_text(format!("Remote: {}", tree.path));
                let root_path = tree.path.clone();
                r.context_menu(|ui| {
                    if ui.button("Download folder…").clicked() {
                        acts.push(Act::RemoteDownload(session_id, root_path.clone(), true));
                        ui.close_menu();
                    }
                    if ui.button("Upload file here…").clicked() {
                        acts.push(Act::RemoteUpload(session_id, root_path.clone(), false));
                        ui.close_menu();
                    }
                    if ui.button("Upload folder here…").clicked() {
                        acts.push(Act::RemoteUpload(session_id, root_path.clone(), true));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Sync folder…").clicked() {
                        acts.push(Act::RemoteSync(session_id, root_path.clone()));
                        ui.close_menu();
                    }
                });

                egui::ScrollArea::vertical()
                    .id_salt(("ot_remote_tree", session_id))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if tree.loading {
                            tree_status(ui, "Loading remote files…", fg_muted());
                        } else if let Some(error) = &tree.error {
                            tree_status(ui, error, red());
                        } else if let Some(children) = tree.children.as_mut() {
                            if children.is_empty() {
                                tree_status(ui, "Empty folder", fg_muted());
                            }
                            for child in children.iter_mut() {
                                remote_tree_node(ui, child, 0, session_id, acts);
                            }
                        }
                        ui.add_space(12.0);
                    });
            } else {
                section(ui, "FILES", |ui| {
                    if icon_btn(ui, "refresh", icons::refresh).clicked() {
                        acts.push(Act::RefreshTree);
                    }
                    if icon_btn(ui, "parent folder", icons::up).clicked() {
                        acts.push(Act::RootUp);
                    }
                });

                // current root as a breadcrumb-ish header
                let root_label = contract_home(&app.tree.path);
                let r = row(ui, 26.0, false, |p, r, _| {
                    let gal = elide(p, &root_label, mono(11.5), fg_muted(), r.width() - 24.0);
                    p.galley(
                        pos2(r.left() + 12.0, r.center().y - gal.size().y / 2.0),
                        gal,
                        fg_muted(),
                    );
                });
                r.on_hover_text(app.tree.path.display().to_string());

                egui::ScrollArea::vertical()
                    .id_salt("ot_tree")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        app.tree.ensure_loaded();
                        if let Some(children) = app.tree.children.as_mut() {
                            if children.is_empty() {
                                tree_status(ui, "Empty folder", fg_muted());
                            }
                            for c in children.iter_mut() {
                                tree_node(ui, c, 0, acts);
                            }
                        }
                        ui.add_space(12.0);
                    });
            }
        });
}

fn tree_node(ui: &mut Ui, n: &mut Node, depth: usize, acts: &mut Vec<Act>) {
    let indent = 10.0 + depth as f32 * 14.0;
    let resp = row(ui, 26.0, false, |p, r, hovered| {
        let y = r.center().y;
        let x = r.left() + indent;
        if n.is_dir {
            icons::chevron(p, pos2(x + 4.0, y), n.expanded, fg_muted());
        }
        let col = if hovered {
            fg()
        } else if n.is_dir {
            fg()
        } else {
            fg_dim()
        };
        if n.is_dir {
            icons::folder(
                p,
                pos2(x + 18.0, y),
                if hovered { fg_dim() } else { fg_muted() },
            );
        } else {
            icons::file(p, pos2(x + 18.0, y), fg_muted());
        }
        let gal = elide(p, &n.name, ui_f(), col, r.right() - (x + 30.0) - 8.0);
        p.galley(pos2(x + 30.0, y - gal.size().y / 2.0), gal, col);
    });
    if resp.double_clicked() && n.is_dir {
        acts.push(Act::SetRoot(n.path.clone()));
    } else if resp.clicked() {
        if n.is_dir {
            n.expanded = !n.expanded;
        } else {
            acts.push(Act::OpenFile(n.path.clone()));
        }
    }
    if n.is_dir && n.expanded {
        n.ensure_loaded();
        if let Some(ch) = n.children.as_mut() {
            for c in ch.iter_mut() {
                tree_node(ui, c, depth + 1, acts);
            }
        }
    }
}

fn tree_status(ui: &mut Ui, text: &str, color: Color32) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.add(egui::Label::new(egui::RichText::new(text).color(color)).wrap());
    });
}

fn remote_tree_node(
    ui: &mut Ui,
    n: &mut RemoteNode,
    depth: usize,
    session_id: u64,
    acts: &mut Vec<Act>,
) {
    let indent = 10.0 + depth as f32 * 14.0;
    let resp = row(ui, 26.0, false, |p, r, hovered| {
        let y = r.center().y;
        let x = r.left() + indent;
        if n.is_dir {
            icons::chevron(p, pos2(x + 4.0, y), n.expanded, fg_muted());
        }
        let col = if hovered {
            fg()
        } else if n.is_dir {
            fg()
        } else {
            fg_dim()
        };
        if n.is_dir {
            icons::folder(
                p,
                pos2(x + 18.0, y),
                if hovered { fg_dim() } else { fg_muted() },
            );
        } else {
            icons::file(p, pos2(x + 18.0, y), fg_muted());
        }
        let gal = elide(p, &n.name, ui_f(), col, r.right() - (x + 30.0) - 8.0);
        p.galley(pos2(x + 30.0, y - gal.size().y / 2.0), gal, col);
    });
    let resp = resp.on_hover_text(format!("Remote: {}", n.path));
    resp.context_menu(|ui| {
        if n.is_dir {
            if ui.button("Download folder…").clicked() {
                acts.push(Act::RemoteDownload(session_id, n.path.clone(), true));
                ui.close_menu();
            }
            if ui.button("Upload file here…").clicked() {
                acts.push(Act::RemoteUpload(session_id, n.path.clone(), false));
                ui.close_menu();
            }
            if ui.button("Upload folder here…").clicked() {
                acts.push(Act::RemoteUpload(session_id, n.path.clone(), true));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Sync folder…").clicked() {
                acts.push(Act::RemoteSync(session_id, n.path.clone()));
                ui.close_menu();
            }
        } else if ui.button("Download file…").clicked() {
            acts.push(Act::RemoteDownload(session_id, n.path.clone(), false));
            ui.close_menu();
        }
    });
    if resp.double_clicked() && n.is_dir {
        acts.push(Act::RemoteSetRoot(session_id, n.path.clone()));
    } else if resp.clicked() {
        if n.is_dir {
            n.expanded = !n.expanded;
            if n.expanded && n.children.is_none() && !n.loading {
                acts.push(Act::RemoteLoad(session_id, n.path.clone()));
            }
        } else {
            acts.push(Act::RemoteOpen(session_id, n.path.clone()));
        }
    }
    if n.is_dir && n.expanded {
        if n.loading {
            tree_status(ui, "Loading…", fg_muted());
        } else if let Some(error) = &n.error {
            tree_status(ui, error, red());
        } else if let Some(children) = n.children.as_mut() {
            for child in children.iter_mut() {
                remote_tree_node(ui, child, depth + 1, session_id, acts);
            }
        }
    }
}

// ───────────────────────────── workspace ─────────────────────────────

fn workspace(ctx: &egui::Context, app: &mut OpenTerm, acts: &mut Vec<Act>) {
    egui::CentralPanel::default()
        .frame(Frame::none().fill(bg()))
        .show(ctx, |ui| {
            // tab bar
            egui::TopBottomPanel::top("ot_tabs")
                .exact_height(38.0)
                .show_separator_line(false)
                .frame(Frame::none().fill(bg2()))
                .show_inside(ui, |ui| {
                    let rect = ui.max_rect();
                    ui.painter().line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.0_f32, border()),
                    );
                    let mut x = rect.left() + 8.0;
                    let show_tools = rect.width() >= 390.0;
                    let tabs_right = if show_tools {
                        rect.right() - 140.0
                    } else {
                        rect.right() - 8.0
                    };
                    for s in app.sessions.iter().filter(|s| s.device == app.selected) {
                        let remaining = tabs_right - x;
                        if remaining < 70.0 {
                            break;
                        }
                        let active = app.active == Some(s.id);
                        let live = s.backend.is_some();
                        let reserve_new_tab = if remaining >= 104.0 { 34.0 } else { 0.0 };
                        let (resp, closed, w) = tab(
                            ui,
                            pos2(x, rect.top() + 6.0),
                            s.id,
                            &s.label,
                            active,
                            live,
                            remaining - reserve_new_tab,
                        );
                        if closed {
                            acts.push(Act::Close(s.id));
                        } else if resp.clicked() {
                            acts.push(Act::Activate(s.id));
                        }
                        if resp.middle_clicked() {
                            acts.push(Act::Close(s.id));
                        }
                        x += w + 4.0;
                    }
                    if x + 28.0 <= tabs_right {
                        let plus = Rect::from_center_size(
                            pos2(x + 14.0, rect.center().y),
                            vec2(26.0, 26.0),
                        );
                        if icon_at(
                            ui,
                            plus,
                            Id::new("ot_newtab"),
                            "new tab  (ctrl+t)",
                            |p, c, col| icons::plus(p, c, 4.5, col),
                        )
                        .clicked()
                        {
                            acts.push(Act::NewTab);
                        }
                    }

                    // right side: SysInfo pill + editor toggle
                    if show_tools {
                        let er = Rect::from_center_size(
                            pos2(rect.right() - 22.0, rect.center().y),
                            vec2(28.0, 26.0),
                        );
                        let tip = if app.show_editor {
                            "hide editor  (ctrl+shift+e)"
                        } else {
                            "show editor  (ctrl+shift+e)"
                        };
                        let on = app.show_editor;
                        if icon_at(
                            ui,
                            er,
                            Id::new("ot_editor_toggle"),
                            tip,
                            move |p, c, col| icons::split(p, c, if on { fg() } else { col }),
                        )
                        .clicked()
                        {
                            acts.push(Act::ToggleEditor);
                        }
                        let gal =
                            ui.painter()
                                .layout_no_wrap("SysInfo".into(), sans(11.5), fg_dim());
                        let nr = Rect::from_min_size(
                            pos2(
                                er.left() - 10.0 - gal.size().x - 38.0,
                                rect.center().y - 12.0,
                            ),
                            vec2(gal.size().x + 38.0, 24.0),
                        );
                        let nresp = ui
                            .interact(nr, Id::new("ot_sysinfo"), Sense::click())
                            .on_hover_text("show system information");
                        let p = ui.painter();
                        p.rect(
                            nr,
                            Rounding::same(6.0),
                            if nresp.hovered() { bg4() } else { bg3() },
                            Stroke::new(1.0_f32, border()),
                        );
                        let control_color = if nresp.hovered() { fg() } else { fg_dim() };
                        icons::info(p, pos2(nr.left() + 13.0, nr.center().y), control_color);
                        p.galley(
                            pos2(nr.left() + 25.0, nr.center().y - gal.size().y / 2.0),
                            gal,
                            control_color,
                        );
                        if nresp.clicked() {
                            acts.push(Act::SysInfo);
                        }
                    }
                });

            // breadcrumb
            egui::TopBottomPanel::top("ot_crumb")
                .exact_height(32.0)
                .show_separator_line(false)
                .frame(Frame::none().fill(bg()))
                .show_inside(ui, |ui| {
                    let rect = ui.max_rect();
                    let p = ui.painter();
                    p.line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.0_f32, border()),
                    );
                    let y = rect.center().y;
                    icons::prompt(p, pos2(rect.left() + 20.0, y), fg_muted());
                    let dev = app
                        .devices
                        .iter()
                        .find(|d| d.id == app.selected)
                        .map(|d| d.label.clone())
                        .unwrap_or_default();
                    let r1 = p.text(
                        pos2(rect.left() + 34.0, y),
                        egui::Align2::LEFT_CENTER,
                        dev,
                        ui_f(),
                        fg(),
                    );
                    icons::chevron(p, pos2(r1.right() + 10.0, y), false, fg_muted());
                    let tail = app
                        .active_session()
                        .map(|s| {
                            let t = s.term.title();
                            if t.is_empty() {
                                s.label.clone()
                            } else {
                                t
                            }
                        })
                        .unwrap_or_else(|| "no session".into());
                    let gal = elide(
                        p,
                        &tail,
                        mono(11.5),
                        fg_dim(),
                        rect.right() - r1.right() - 40.0,
                    );
                    p.galley(
                        pos2(r1.right() + 20.0, y - gal.size().y / 2.0),
                        gal,
                        fg_dim(),
                    );
                });

            // editor (resizable, right)
            if app.show_editor {
                egui::SidePanel::right("ot_editor")
                    .resizable(true)
                    .default_width(ui.available_width() * 0.45)
                    .width_range(260.0..=ui.available_width() * 0.8)
                    .show_separator_line(true)
                    .frame(Frame::none().fill(bg()))
                    .show_inside(ui, |ui| {
                        if let Some((m, err)) = app.editor.ui(ui) {
                            let until = ui.input(|i| i.time) + 4.0;
                            app.flash = Some((m, err, until));
                        }
                    });
            }

            // terminal
            egui::CentralPanel::default()
                .frame(Frame::none().fill(bg()))
                .show_inside(ui, |ui| {
                    let area = ui.max_rect();
                    let modal = app.modal || app.settings_open;
                    let connection_error = app.connection_errors.get(&app.selected).cloned();
                    match app.active_session() {
                        Some(s) => {
                            let r = Rect::from_min_max(
                                area.min + vec2(14.0, 10.0),
                                area.max - vec2(10.0, 6.0),
                            );
                            s.term.ui(ui, r, !modal);
                        }
                        None => {
                            let c = area.center();
                            let p = ui.painter();
                            let heading = if connection_error.is_some() {
                                "Connection failed"
                            } else {
                                "No session on this device"
                            };
                            p.text(
                                c - vec2(0.0, 38.0),
                                egui::Align2::CENTER_CENTER,
                                heading,
                                medium(13.0),
                                if connection_error.is_some() {
                                    red()
                                } else {
                                    fg_dim()
                                },
                            );
                            if let Some(error) = &connection_error {
                                let galley = elide(
                                    p,
                                    error,
                                    mono(11.5),
                                    fg_dim(),
                                    (area.width() - 80.0).max(120.0),
                                );
                                let error_rect = Rect::from_center_size(
                                    c - vec2(0.0, 12.0),
                                    galley.size() + vec2(12.0, 8.0),
                                );
                                p.galley(
                                    error_rect.center() - galley.size() / 2.0,
                                    galley,
                                    fg_dim(),
                                );
                                ui.interact(
                                    error_rect,
                                    Id::new(("ot_connection_error", app.selected.as_str())),
                                    Sense::hover(),
                                )
                                .on_hover_text(error);
                            }
                            let btn =
                                Rect::from_center_size(c + vec2(0.0, 28.0), vec2(120.0, 30.0));
                            let resp = ui.interact(btn, Id::new("ot_empty_open"), Sense::click());
                            let p = ui.painter();
                            p.rect(
                                btn,
                                Rounding::same(7.0),
                                if resp.hovered() { bg4() } else { bg3() },
                                Stroke::new(1.0_f32, border_d()),
                            );
                            p.text(
                                btn.center(),
                                egui::Align2::CENTER_CENTER,
                                if connection_error.is_some() {
                                    "Retry"
                                } else {
                                    "Open session"
                                },
                                ui_f(),
                                fg(),
                            );
                            if resp.clicked() {
                                acts.push(Act::NewSession(app.selected.clone()));
                            }
                        }
                    }
                });
        });
}

fn tab(
    ui: &mut Ui,
    at: Pos2,
    id: u64,
    label: &str,
    active: bool,
    live: bool,
    max_width: f32,
) -> (Response, bool, f32) {
    let gal = ui.painter().layout_no_wrap(
        label.to_string(),
        ui_f(),
        if active { fg() } else { fg_dim() },
    );
    let w = (gal.size().x + 56.0).min(220.0).min(max_width.max(70.0));
    let rect = Rect::from_min_size(at, vec2(w, 32.0));
    let resp = ui.interact(rect, Id::new(("ot_tab", id)), Sense::click());
    let xc = pos2(rect.right() - 14.0, rect.center().y - 1.0);
    let xresp = ui.interact(
        Rect::from_center_size(xc, vec2(18.0, 18.0)),
        Id::new(("ot_tabx", id)),
        Sense::click(),
    );
    let p = ui.painter();
    if active {
        p.rect(
            rect,
            Rounding {
                nw: 7.0,
                ne: 7.0,
                sw: 0.0,
                se: 0.0,
            },
            bg(),
            Stroke::NONE,
        );
        p.add(egui::Shape::line(
            vec![rect.left_bottom(), rect.left_top() + vec2(0.0, 7.0)],
            Stroke::new(1.0_f32, border()),
        ));
        p.add(egui::Shape::line(
            vec![rect.right_bottom(), rect.right_top() + vec2(0.0, 7.0)],
            Stroke::new(1.0_f32, border()),
        ));
        p.line_segment(
            [
                rect.left_top() + vec2(7.0, 0.0),
                rect.right_top() - vec2(7.0, 0.0),
            ],
            Stroke::new(1.0_f32, border()),
        );
    } else if resp.hovered() {
        p.rect_filled(
            rect.shrink2(vec2(0.0, 3.0)).translate(vec2(0.0, -2.0)),
            Rounding::same(6.0),
            bg3(),
        );
    }
    let y = rect.center().y - 1.0;
    icons::dot(
        p,
        pos2(rect.left() + 14.0, y),
        3.2,
        if live { signal() } else { fg_muted() },
    );
    let gal = elide(
        p,
        label,
        ui_f(),
        if active { fg() } else { fg_dim() },
        w - 56.0,
    );
    p.galley(pos2(rect.left() + 26.0, y - gal.size().y / 2.0), gal, fg());
    if xresp.hovered() {
        p.rect_filled(
            Rect::from_center_size(xc, vec2(18.0, 18.0)),
            Rounding::same(4.0),
            bg4(),
        );
    }
    if active || resp.hovered() || xresp.hovered() {
        icons::cross(p, xc, 3.2, if xresp.hovered() { fg() } else { fg_muted() });
    }
    (resp, xresp.clicked(), w)
}

// ───────────────────────────── ssh modal ─────────────────────────────

fn settings_dialog(ctx: &egui::Context, settings: &Settings, acts: &mut Vec<Act>) {
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
                    let selected = TerminalKind::from_setting(&settings.default_shell);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                        for terminal in [
                            TerminalKind::PowerShell,
                            TerminalKind::Wsl,
                            TerminalKind::Bash,
                            TerminalKind::Cmd,
                        ] {
                            if terminal_seg(ui, terminal, selected == terminal).clicked() {
                                acts.push(Act::SetDefaultShell(terminal));
                            }
                        }
                    });

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

fn host_key_dialog(ctx: &egui::Context, prompt: &HostKeyDialog, acts: &mut Vec<Act>) {
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

fn ssh_modal(ctx: &egui::Context, f: &mut SshForm, acts: &mut Vec<Act>, vault_ready: bool) {
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
                        egui::RichText::new("New connection")
                            .font(medium(15.0))
                            .color(fg()),
                    );
                    ui.label(
                        egui::RichText::new("Choose a protocol or local session type.")
                            .color(fg_muted()),
                    );
                    ui.add_space(8.0);

                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
                        for connection in ConnectionType::ALL {
                            if connection_card(ui, connection, f.connection == connection).clicked()
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
                        if f.save_to_vault {
                            ui.add_space(2.0);
                            let _ =
                                field(ui, "Label (optional)", &mut f.label, "home server", false);
                        }
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
                                    egui::RichText::new("Connect")
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

fn sync_root_too_broad(value: &str) -> bool {
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
fn pick_local_folder(start: &str) -> Result<Option<String>, String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"Add-Type -AssemblyName System.Windows.Forms; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $d=New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description='Select local project folder'; if(Test-Path -LiteralPath $env:OPENTERM_PICKER_START){$d.SelectedPath=$env:OPENTERM_PICKER_START}; if($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK){[Console]::Out.Write($d.SelectedPath)}"#;
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-STA", "-Command", SCRIPT])
        .env("OPENTERM_PICKER_START", start)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("couldn't open the folder picker: {error}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "the folder picker failed".into()
        } else {
            message
        });
    }
    let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!selected.is_empty()).then_some(selected))
}

#[cfg(target_os = "macos")]
fn pick_local_folder(_start: &str) -> Result<Option<String>, String> {
    let output = Command::new("osascript")
        .args([
            "-e",
            "POSIX path of (choose folder with prompt \"Select local project folder\")",
        ])
        .output()
        .map_err(|error| format!("couldn't open the folder picker: {error}"))?;
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
fn pick_local_folder(start: &str) -> Result<Option<String>, String> {
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
            Err(error) => return Err(format!("couldn't open the folder picker: {error}")),
        }
    }
    Err("No folder picker is installed (tried zenity and kdialog). You can still enter the path manually.".into())
}

fn transfer_dialog(ctx: &egui::Context, dialog: &mut TransferDialog, acts: &mut Vec<Act>) {
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

fn vault_dialog(ctx: &egui::Context, state: &mut VaultState, acts: &mut Vec<Act>) {
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

fn primary_button(ui: &mut Ui, label: &str) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(bg()).font(medium(12.5)))
            .fill(fg())
            .stroke(Stroke::NONE)
            .rounding(7.0)
            .min_size(vec2(100.0, 30.0)),
    )
}

fn field(ui: &mut Ui, label: &str, val: &mut String, hint: &str, password: bool) -> Response {
    ui.label(egui::RichText::new(label).size(11.0).color(fg_muted()));
    let eye_id = ui.make_persistent_id(("password_eye", label, hint));
    let mut revealed = password && ui.data(|d| d.get_temp::<bool>(eye_id).unwrap_or(false));
    let margin = if password {
        Margin {
            left: 9.0,
            right: 39.0,
            top: 7.0,
            bottom: 7.0,
        }
    } else {
        Margin::symmetric(9.0, 7.0)
    };
    let r = ui.add(
        TextEdit::singleline(val)
            .hint_text(egui::RichText::new(hint).color(fg_muted()))
            .password(password && !revealed)
            .margin(margin)
            .desired_width(f32::INFINITY),
    );
    if password {
        // A field reopened with an empty value starts safely hidden again.
        if revealed && val.is_empty() && !r.has_focus() {
            revealed = false;
            ui.data_mut(|d| d.insert_temp(eye_id, false));
        }
        let button_rect = Rect::from_center_size(
            pos2(r.rect.right() - 17.0, r.rect.center().y),
            vec2(28.0, 28.0),
        );
        let eye = ui.interact(button_rect, eye_id.with("button"), Sense::click());
        if eye.clicked() {
            revealed = !revealed;
            let now = ui.input(|i| i.time);
            ui.data_mut(|d| {
                d.insert_temp(eye_id, revealed);
                d.insert_temp(eye_id.with("next_blink"), now + blink_delay(now, eye_id));
            });
            // Clicking the eye should not make the user reacquire the field.
            r.request_focus();
        }
        let eye = eye.on_hover_text(if revealed {
            "Hide password"
        } else {
            "Show password"
        });
        paint_password_eye(ui, &eye, eye_id, revealed, val, &r);
    }
    ui.add_space(4.0);
    r
}

/// Native version of the password-eye easter egg: it wakes when the secret is
/// revealed, follows the pointer (or the typing caret), and blinks at irregular
/// intervals. Hidden passwords get a sleepy closed eye with lashes.
fn paint_password_eye(
    ui: &Ui,
    response: &Response,
    id: Id,
    revealed: bool,
    value: &str,
    input: &Response,
) {
    let now = ui.input(|i| i.time);
    let next_id = id.with("next_blink");
    let blink_id = id.with("blink_until");
    let caret_id = id.with("caret_until");
    let stored_next = ui.data(|d| d.get_temp::<f64>(next_id));
    let mut next = stored_next.unwrap_or_else(|| now + blink_delay(now, id));
    if stored_next.is_none() {
        ui.data_mut(|d| d.insert_temp(next_id, next));
    }
    let mut blink_until = ui.data(|d| d.get_temp::<f64>(blink_id).unwrap_or(0.0));
    let mut caret_until = ui.data(|d| d.get_temp::<f64>(caret_id).unwrap_or(0.0));

    if input.changed() || (input.has_focus() && ui.input(|i| i.pointer.primary_clicked())) {
        caret_until = now + 0.85;
        ui.data_mut(|d| d.insert_temp(caret_id, caret_until));
    }
    if revealed && now >= next {
        blink_until = now + 0.17;
        next = blink_until + blink_delay(blink_until, id);
        ui.data_mut(|d| {
            d.insert_temp(blink_id, blink_until);
            d.insert_temp(next_id, next);
        });
    } else if !revealed && next < now + 1.0 {
        next = now + blink_delay(now, id);
        ui.data_mut(|d| d.insert_temp(next_id, next));
    }
    let blinking = revealed && now < blink_until;
    if blinking {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    } else if revealed {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(
                (next - now).clamp(0.016, 7.0),
            ));
    }

    let open = ui
        .ctx()
        .animate_bool_with_time(id.with("lid"), revealed && !blinking, 0.2);
    let center = response.rect.center();
    let mut target = Vec2::ZERO;
    if revealed {
        if input.has_focus() && now < caret_until {
            let text_w = ui.fonts(|f| f.layout_no_wrap(value.to_owned(), ui_f(), fg()).size().x);
            let caret = pos2(
                (input.rect.left() + 9.0 + text_w).min(input.rect.right() - 41.0),
                input.rect.center().y,
            );
            target = caret - center;
        } else if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
            target = pointer - center;
        }
    }
    let distance = target.length();
    let reach = (distance / 160.0).min(1.0);
    let desired = if distance > 0.5 {
        vec2(
            target.x / distance * 3.2 * reach,
            target.y / distance * 1.7 * reach,
        )
    } else {
        Vec2::ZERO
    };
    let gaze_x = ui
        .ctx()
        .animate_value_with_time(id.with("gaze_x"), desired.x, 0.12);
    let gaze_y = ui
        .ctx()
        .animate_value_with_time(id.with("gaze_y"), desired.y, 0.12);

    let color = if revealed || response.hovered() {
        fg()
    } else {
        fg_dim()
    };
    if response.hovered() {
        ui.painter()
            .rect_filled(response.rect, Rounding::same(6.0), bg4());
    }
    let map = |x: f32, y: f32| pos2(center.x + (x - 12.0) * 0.72, center.y + (y - 12.0) * 0.72);
    let lid_y = 19.0 - 14.0 * open;
    if open > 0.08 {
        let iris = map(12.0 + gaze_x, 12.0 + gaze_y);
        ui.painter()
            .circle_stroke(iris, 2.45 * open.max(0.35), Stroke::new(1.15_f32, color));
        ui.painter()
            .circle_filled(iris, 0.95 * open.max(0.4), color);
    }
    paint_cubic(
        ui.painter(),
        map(2.0, 12.0),
        map(6.0, lid_y),
        map(18.0, lid_y),
        map(22.0, 12.0),
        color,
    );
    paint_cubic(
        ui.painter(),
        map(2.0, 12.0),
        map(6.0, 19.0),
        map(18.0, 19.0),
        map(22.0, 12.0),
        color,
    );
    if !revealed {
        for ((x1, y1), (x2, y2)) in [
            ((5.2, 15.4), (3.9, 17.4)),
            ((12.0, 17.3), (12.0, 19.8)),
            ((18.8, 15.4), (20.1, 17.4)),
        ] {
            ui.painter()
                .line_segment([map(x1, y1), map(x2, y2)], Stroke::new(1.15_f32, color));
        }
    }
}

fn paint_cubic(p: &Painter, a: Pos2, b: Pos2, c: Pos2, d: Pos2, color: Color32) {
    let mut points = Vec::with_capacity(13);
    for i in 0..=12 {
        let t = i as f32 / 12.0;
        let u = 1.0 - t;
        points.push(pos2(
            u * u * u * a.x + 3.0 * u * u * t * b.x + 3.0 * u * t * t * c.x + t * t * t * d.x,
            u * u * u * a.y + 3.0 * u * u * t * b.y + 3.0 * u * t * t * c.y + t * t * t * d.y,
        ));
    }
    p.add(egui::Shape::line(points, Stroke::new(1.15_f32, color)));
}

fn blink_delay(now: f64, id: Id) -> f64 {
    // Organic 3–7s spacing without an RNG or another UI-thread dependency.
    let seed = (now.to_bits() ^ id.value()).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    3.0 + (seed % 4000) as f64 / 1000.0
}

fn seg(ui: &mut Ui, label: &str, on: bool) -> Response {
    let gal =
        ui.painter()
            .layout_no_wrap(label.into(), sans(12.0), if on { fg() } else { fg_dim() });
    let (r, resp) = ui.allocate_exact_size(vec2(gal.size().x + 22.0, 28.0), Sense::click());
    let fill = if on {
        bg4()
    } else if resp.hovered() {
        bg3()
    } else {
        bg2()
    };
    ui.painter().rect(
        r,
        Rounding::same(6.0),
        fill,
        Stroke::new(1.0_f32, if on { border_d() } else { border() }),
    );
    ui.painter()
        .galley(r.center() - gal.size() / 2.0, gal, fg());
    resp
}

fn terminal_seg(ui: &mut Ui, terminal: TerminalKind, on: bool) -> Response {
    let label = terminal.label();
    let color = if on { fg() } else { fg_dim() };
    let gal = ui.painter().layout_no_wrap(label.into(), sans(12.0), color);
    let (r, resp) = ui.allocate_exact_size(vec2(gal.size().x + 42.0, 30.0), Sense::click());
    let fill = if on {
        bg4()
    } else if resp.hovered() {
        bg3()
    } else {
        bg2()
    };
    let painter = ui.painter();
    painter.rect(
        r,
        Rounding::same(6.0),
        fill,
        Stroke::new(1.0_f32, if on { border_d() } else { border() }),
    );
    let icon_center = pos2(r.left() + 15.0, r.center().y);
    match terminal {
        TerminalKind::PowerShell => icons::powershell(painter, icon_center, color),
        TerminalKind::Wsl => icons::wsl(painter, icon_center, color),
        TerminalKind::Bash => icons::bash(painter, icon_center, color),
        TerminalKind::Cmd => icons::cmd(painter, icon_center, color),
    }
    painter.galley(
        pos2(r.left() + 28.0, r.center().y - gal.size().y / 2.0),
        gal,
        color,
    );
    resp
}

// ───────────────────────────── widgets ─────────────────────────────

fn ui_f() -> egui::FontId {
    sans(UI_SIZE)
}

fn section(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_height(26.0);
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new(title)
                .font(medium(10.5))
                .color(fg_muted()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            right(ui);
        });
    });
    ui.add_space(2.0);
}

/// full-width, inset, rounded row. `paint(painter, rect, hovered)`
fn row(ui: &mut Ui, h: f32, selected: bool, paint: impl FnOnce(&Painter, Rect, bool)) -> Response {
    let (outer, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let r = outer.shrink2(vec2(6.0, 0.0));
    let hovered = resp.hovered();
    if selected {
        ui.painter().rect_filled(r, Rounding::same(6.0), bg4());
    } else if hovered {
        ui.painter().rect_filled(r, Rounding::same(6.0), bg3());
    }
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    paint(ui.painter(), r, hovered);
    resp
}

fn icon_btn(ui: &mut Ui, tip: &str, paint: impl FnOnce(&Painter, Pos2, Color32)) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    paint_icon_btn(ui, r, &resp, paint);
    resp.on_hover_text(tip)
}

fn icon_at(
    ui: &mut Ui,
    r: Rect,
    id: Id,
    tip: &str,
    paint: impl FnOnce(&Painter, Pos2, Color32),
) -> Response {
    let resp = ui.interact(r, id, Sense::click());
    paint_icon_btn(ui, r, &resp, paint);
    resp.on_hover_text(tip)
}

fn paint_icon_btn(ui: &Ui, r: Rect, resp: &Response, paint: impl FnOnce(&Painter, Pos2, Color32)) {
    let col = if resp.hovered() { fg() } else { fg_dim() };
    if resp.hovered() {
        ui.painter().rect_filled(r, Rounding::same(6.0), bg4());
    }
    paint(ui.painter(), r.center(), col);
}

fn elide(p: &Painter, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: max_w.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    p.layout_job(job)
}

fn contract_home(p: &std::path::Path) -> String {
    if let Some(h) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&h) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display()).replace('\\', "/")
            };
        }
    }
    p.display().to_string()
}

// borderless window: resize from the edges
#[cfg(not(target_os = "macos"))]
fn window_edges(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as D;
    let screen = ctx.screen_rect();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if !maximized {
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::new("ot_border"),
        ))
        .rect_stroke(
            screen.shrink(0.5),
            Rounding::ZERO,
            Stroke::new(1.0_f32, border_d()),
        );
    } else {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let m = 5.0;
    let (l, r, t, b) = (
        pos.x < screen.left() + m,
        pos.x > screen.right() - m,
        pos.y < screen.top() + m,
        pos.y > screen.bottom() - m,
    );
    let hit = match (l, r, t, b) {
        (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNwSe)),
        (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNeSw)),
        (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeNeSw)),
        (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeNwSe)),
        (true, ..) => Some((D::West, CursorIcon::ResizeHorizontal)),
        (_, true, ..) => Some((D::East, CursorIcon::ResizeHorizontal)),
        (_, _, true, _) => Some((D::North, CursorIcon::ResizeVertical)),
        (.., true) => Some((D::South, CursorIcon::ResizeVertical)),
        _ => None,
    };
    if let Some((dir, icon)) = hit {
        ctx.set_cursor_icon(icon);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}
