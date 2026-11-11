//! OpenTerm application state and frame orchestration.

use crate::{
    editor::Editor,
    fs_tree::{Node, RemoteNode},
    pty::{self, LocalPty},
    settings::{Settings, ThemeMode, VaultGraceUnit},
    ssh::{SftpHandle, SshConfig, SshSession},
    term::TermView,
    theme::*,
    vault::{Mode as VaultMode, Vault},
    wake,
};
use egui::{vec2, Key, Modifiers, ViewportCommand};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use zeroize::Zeroizing;

type AppResult<T> = anyhow::Result<T>;

mod actions;
mod chrome;
mod commands;
mod connection_ui;
mod file_picker;
mod files;
mod sessions;
mod settings_ui;
mod sidebar;
mod transfer_ui;
mod vault_ui;
mod widgets;
mod workspace;

#[cfg(not(target_os = "macos"))]
use chrome::window_edges;
use chrome::{statusbar, titlebar};
use commands::os_unlock_error;
use connection_ui::{host_key_dialog, rename_device_dialog, ssh_modal};
use settings_ui::settings_dialog;
use sidebar::sidebar;
use transfer_ui::{binary_file_dialog, transfer_dialog};
use vault_ui::vault_dialog;
use workspace::workspace;

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
            Self::Bash if cfg!(windows) => "Bash",
            Self::Bash => "Shell",
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

fn available_terminals() -> Vec<TerminalKind> {
    let mut terminals = Vec::new();
    if pty::posix_shell().is_some() {
        terminals.push(TerminalKind::Bash);
    }
    if pty::powershell_shell().is_some() {
        terminals.push(TerminalKind::PowerShell);
    }
    if pty::cmd_shell().is_some() {
        terminals.push(TerminalKind::Cmd);
    }
    if pty::wsl_shell().is_some() {
        terminals.push(TerminalKind::Wsl);
    }
    terminals
}

fn resolved_terminal(setting: &str, available: &[TerminalKind]) -> TerminalKind {
    let configured = TerminalKind::from_setting(setting);
    if available.contains(&configured) {
        configured
    } else {
        available.first().copied().unwrap_or(configured)
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
    rx: mpsc::Receiver<AppResult<SshSession>>,
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

struct RenameDeviceDialog {
    device: String,
    value: String,
    error: Option<String>,
    focus: bool,
}
struct PendingRemote {
    session_id: u64,
    path: String,
    rx: mpsc::Receiver<AppResult<crate::agent::DirectoryListing>>,
}
struct PendingRemoteFile {
    session_id: u64,
    path: String,
    rx: mpsc::Receiver<AppResult<Vec<u8>>>,
}
struct PendingLocalFile {
    path: PathBuf,
    rx: mpsc::Receiver<AppResult<Vec<u8>>>,
}
struct PendingRemoteWrite {
    session_id: u64,
    key: PathBuf,
    saved_data: Vec<u8>,
    rx: mpsc::Receiver<AppResult<()>>,
}
struct BinaryFileDialog {
    remote: String,
    local: String,
    bytes: Vec<u8>,
    error: Option<String>,
}
struct PendingBinaryDownload {
    remote: String,
    local: String,
    view_hex: bool,
    rx: mpsc::Receiver<(AppResult<()>, Vec<u8>)>,
}
struct PendingDeviceInfo {
    session_id: u64,
    rx: mpsc::Receiver<AppResult<crate::agent::DeviceInfo>>,
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
    rx: mpsc::Receiver<AppResult<TransferResult>>,
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

#[derive(Default, Clone)]
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
    NewTerminal(TerminalKind),
    Activate(u64),
    Close(u64),
    OpenFile(PathBuf),
    SetRoot(PathBuf),
    RootUp,
    RefreshTree,
    ToggleEditor,
    Info,
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
    EditDevice(String),
    RenameDevice(String),
    ConfirmRenameDevice,
    CancelRenameDevice,
    DeleteDevice(String),
    VaultConnect(String),
    VaultResetBegin,
    VaultResetCancel,
    VaultReset,
    RemoteLoad(u64, String),
    RemoteSetRoot(u64, String),
    RemoteUp(u64),
    RemoteRefresh(u64),
    RemoteOpen(u64, String, i64),
    RemoteDownload(u64, String, bool),
    BinaryDownload(bool),
    BinaryCancel,
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
    available_terminals: Vec<TerminalKind>,
    form: SshForm,
    pending: Vec<Pending>,
    pending_remote: Vec<PendingRemote>,
    pending_remote_files: Vec<PendingRemoteFile>,
    pending_local_files: Vec<PendingLocalFile>,
    pending_remote_writes: Vec<PendingRemoteWrite>,
    pending_device_info: Vec<PendingDeviceInfo>,
    connection_errors: HashMap<String, String>,
    transfer_dialog: Option<TransferDialog>,
    pending_transfer: Option<PendingTransfer>,
    binary_file_dialog: Option<BinaryFileDialog>,
    pending_binary_download: Option<PendingBinaryDownload>,
    host_key_dialog: Option<HostKeyDialog>,
    editing_device: Option<String>,
    rename_device: Option<RenameDeviceDialog>,
    flash: Option<(String, bool, f64)>,
    vault: Option<Vault>,
    vault_ui: VaultState,
    window_maximized: bool,
}

impl OpenTerm {
    pub fn new(cc: &eframe::CreationContext, mut settings: Settings) -> Self {
        wake::set(cc.egui_ctx.clone());
        let available_terminals = available_terminals();
        let default_terminal = resolved_terminal(&settings.default_shell, &available_terminals);
        if !available_terminals.is_empty()
            && settings.default_shell != default_terminal.setting_id()
        {
            settings.default_shell = default_terminal.setting_id().into();
            let _ = settings.save();
        }
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
            available_terminals,
            form: SshForm {
                port: "22".into(),
                ..Default::default()
            },
            pending: Vec::new(),
            pending_remote: Vec::new(),
            pending_remote_files: Vec::new(),
            pending_local_files: Vec::new(),
            pending_remote_writes: Vec::new(),
            pending_device_info: Vec::new(),
            connection_errors: HashMap::new(),
            transfer_dialog: None,
            pending_transfer: None,
            binary_file_dialog: None,
            pending_binary_download: None,
            host_key_dialog: None,
            editing_device: None,
            rename_device: None,
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
            app.flash = Some((error.to_string(), true, cc.egui_ctx.input(|i| i.time) + 6.0));
        }
        app
    }

    fn say(&mut self, ctx: &egui::Context, msg: impl Into<String>, err: bool) {
        self.flash = Some((msg.into(), err, ctx.input(|i| i.time) + 4.0));
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
        self.poll_local_files(ctx);
        self.poll_remote_files(ctx);
        self.poll_binary_download(ctx);
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
                        .finish_remote_save(&save.key, &save.data, Err(error.to_string()))
                {
                    self.say(ctx, message, is_error);
                }
            }
        }
        vault_dialog(ctx, &mut self.vault_ui, &mut acts);
        if self.settings_open && matches!(self.vault_ui, VaultState::Ready | VaultState::Idle) {
            settings_dialog(ctx, &self.settings, &self.available_terminals, &mut acts);
        }
        if self.modal
            && !self.settings_open
            && self.rename_device.is_none()
            && matches!(self.vault_ui, VaultState::Ready | VaultState::Idle)
        {
            let editing = self.editing_device.is_some();
            ssh_modal(
                ctx,
                &mut self.form,
                &mut acts,
                self.vault.is_some() && self.vault.as_ref().unwrap().is_unlocked(),
                editing,
            );
        }
        if let Some(dialog) = self.rename_device.as_mut() {
            rename_device_dialog(ctx, dialog, &mut acts);
        }
        if let Some(dialog) = self.transfer_dialog.as_mut() {
            transfer_dialog(ctx, dialog, &mut acts);
        }
        if let Some(dialog) = self.binary_file_dialog.as_mut() {
            binary_file_dialog(ctx, dialog, &mut acts);
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

#[cfg(test)]
mod tests {
    use super::commands::platform_label;

    #[test]
    fn unknown_platforms_use_the_unknown_status_label() {
        assert_eq!(platform_label(""), "Unknown");
        assert_eq!(platform_label("mystery-os"), "Unknown");
        assert_eq!(platform_label("Linux"), "Linux");
        assert_eq!(platform_label("Windows_NT"), "Windows");
        assert_eq!(platform_label("Darwin"), "macOS");
    }
}
