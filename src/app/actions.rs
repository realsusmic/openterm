//! Apply actions raised by the user interface.

use super::commands::{launch_external, os_unlock_error};
use super::file_picker::{pick_local_folder, sync_root_too_broad};
use super::files::suggested_download_path;
use super::{
    resolved_terminal, Act, AuthMode, CommandProfile, ConnectionType, DeviceKind, OpenTerm,
    PendingBinaryDownload, PendingTransfer, RenameDeviceDialog, SyncDirection, TransferDialog,
    TransferMode, TransferResult, VaultState,
};
use crate::fs_tree::{self, Node, RemoteNode};
use crate::ssh::{SshAuth, SshConfig};
use crate::theme;
use crate::vault::Mode as VaultMode;
use crate::vault::Secret as VaultSecret;
use crate::vault::{UnlockError, Vault};
use crate::{system_info, wake};
use anyhow::Context;
use std::path::PathBuf;
use std::sync::mpsc;
use zeroize::{Zeroize, Zeroizing};

impl OpenTerm {
    pub(super) fn apply(&mut self, ctx: &egui::Context, act: Act) {
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
                        let terminal = resolved_terminal(
                            &self.settings.default_shell,
                            &self.available_terminals,
                        );
                        if let Err(error) = self.open_local(terminal) {
                            self.say(ctx, error.to_string(), true);
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
            Act::NewTerminal(terminal) => {
                if let Err(error) = self.open_local(terminal) {
                    self.say(ctx, error.to_string(), true);
                }
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
            Act::OpenFile(p) => {
                if self
                    .pending_local_files
                    .iter()
                    .any(|pending| pending.path == p)
                {
                    return;
                }
                let is_large = std::fs::metadata(&p)
                    .map(|metadata| metadata.len() > crate::editor::LARGE_FILE_THRESHOLD)
                    .unwrap_or(false);
                self.spawn_local_file(p);
                if is_large {
                    self.large_file_notice(ctx);
                } else {
                    self.say(ctx, "opening file…", false);
                }
            }
            Act::SetRoot(p) => self.tree = Node::root(&p),
            Act::RootUp => {
                if let Some(parent) = self.tree.path.parent().map(|p| p.to_path_buf()) {
                    self.tree = Node::root(&parent)
                }
            }
            Act::RefreshTree => self.tree.refresh(),
            Act::ToggleEditor => self.show_editor = !self.show_editor,
            Act::Info => {
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
                            if let Err(error) = backend.write(format!("{command}\r").as_bytes()) {
                                self.say(ctx, format!("{error:#}"), true);
                            }
                        }
                    }
                    Some((_, false)) => {
                        if self
                            .active_session()
                            .is_some_and(|session| session.connection_error.is_some())
                        {
                            self.say(ctx, "Reconnect the SSH session to run Info", true);
                        } else {
                            self.say(ctx, "shell is still starting…", false);
                        }
                    }
                    None => self.say(ctx, "open a terminal session first", true),
                }
            }
            Act::OpenModal => {
                self.editing_device = None;
                self.form.error = None;
                self.form.focus_host = true;
                self.form.save_to_vault =
                    self.vault.as_ref().is_some_and(|vault| vault.is_unlocked());
                self.modal = true;
            }
            Act::CloseModal => {
                self.modal = false;
                self.editing_device = None;
                self.form.password.zeroize();
            }
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
                if !self.available_terminals.contains(&terminal) {
                    self.say(ctx, format!("{} is not installed", terminal.label()), true);
                    return;
                }
                self.settings.default_shell = terminal.setting_id().into();
                if let Err(error) = self.settings.save() {
                    self.say(ctx, format!("couldn't save settings: {error}"), true);
                }
            }
            Act::SetTerminalFont(family) => {
                let font = match family
                    .as_deref()
                    .map(|name| self.load_terminal_font(name))
                    .transpose()
                {
                    Ok(font) => font,
                    Err(error) => {
                        self.say(ctx, format!("couldn't load terminal font: {error:#}"), true);
                        return;
                    }
                };
                theme::set_terminal_font(ctx, font);
                self.settings.terminal_font = family;
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
                let f = self.form.clone();
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

                if let Some(device_id) = self.editing_device.clone() {
                    match self.save_device_edit(&device_id, &f) {
                        Ok(()) => {
                            self.modal = false;
                            self.editing_device = None;
                            self.form.password.zeroize();
                            self.say(ctx, "Connection settings saved", false);
                        }
                        Err(error) => self.form.error = Some(error.to_string()),
                    }
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
                        Err(error) => self.form.error = Some(error.to_string()),
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
            Act::EditDevice(device_id) => self.begin_edit_device(&device_id, ctx),
            Act::RenameDevice(device_id) => {
                if let Some(device) = self.devices.iter().find(|device| device.id == device_id) {
                    self.rename_device = Some(RenameDeviceDialog {
                        device: device_id,
                        value: device.label.clone(),
                        error: None,
                        focus: true,
                    });
                    self.modal = false;
                    self.editing_device = None;
                }
            }
            Act::ConfirmRenameDevice => {
                let Some((device_id, value)) = self
                    .rename_device
                    .as_ref()
                    .map(|dialog| (dialog.device.clone(), dialog.value.trim().to_string()))
                else {
                    return;
                };
                if value.is_empty() {
                    if let Some(dialog) = self.rename_device.as_mut() {
                        dialog.error = Some("Enter a name".into());
                    }
                    return;
                }
                let kind = self
                    .devices
                    .iter()
                    .find(|device| device.id == device_id)
                    .map(|device| device.kind.clone());
                if let Some(DeviceKind::VaultRef(record_id)) = kind {
                    let Some(vault) = self.vault.as_mut() else {
                        if let Some(dialog) = self.rename_device.as_mut() {
                            dialog.error = Some("Vault is unavailable".into());
                        }
                        return;
                    };
                    if let Err(error) = vault.rename(&record_id, &value) {
                        if let Some(dialog) = self.rename_device.as_mut() {
                            dialog.error = Some(format!("Rename failed: {error}"));
                        }
                        return;
                    }
                }
                if let Some(device) = self.devices.iter_mut().find(|d| d.id == device_id) {
                    device.label = value.clone();
                }
                for session in self
                    .sessions
                    .iter_mut()
                    .filter(|session| session.device == device_id)
                {
                    session.label = value.clone();
                }
                self.rename_device = None;
                self.say(ctx, "Connection renamed", false);
            }
            Act::CancelRenameDevice => self.rename_device = None,
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
            Act::RemoteOpen(id, path, size) => {
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
                    if size > crate::editor::LARGE_FILE_THRESHOLD as i64 {
                        self.large_file_notice(ctx);
                    } else {
                        self.say(ctx, "opening remote file…", false);
                    }
                }
            }
            Act::RemoteDownload(id, remote, recursive) => {
                let local = suggested_download_path(&remote, "download");
                self.transfer_dialog = Some(TransferDialog {
                    session_id: id,
                    remote,
                    local,
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
            Act::BinaryDownload(view_hex) => {
                if self.pending_binary_download.is_some() {
                    return;
                }
                let Some(mut dialog) = self.binary_file_dialog.take() else {
                    return;
                };
                let local = dialog.local.trim().to_string();
                if local.is_empty() {
                    dialog.error = Some("Choose where to save the file".into());
                    self.binary_file_dialog = Some(dialog);
                    return;
                }
                let remote = dialog.remote;
                let bytes = dialog.bytes;
                let write_path = PathBuf::from(&local);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let result = (|| {
                        if let Some(parent) = write_path
                            .parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                        {
                            std::fs::create_dir_all(parent).with_context(|| {
                                format!("create destination folder {}", parent.display())
                            })?;
                        }
                        std::fs::write(&write_path, &bytes)
                            .with_context(|| format!("write {}", write_path.display()))
                    })();
                    let _ = tx.send((result, bytes));
                    wake::poke();
                });
                self.pending_binary_download = Some(PendingBinaryDownload {
                    remote,
                    local,
                    view_hex,
                    rx,
                });
                self.say(ctx, "downloading file…", false);
            }
            Act::BinaryCancel => {
                self.binary_file_dialog = None;
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
                            dialog.error = Some(error.to_string());
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
}
