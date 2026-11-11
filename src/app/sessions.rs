//! Session creation, device profiles, and terminal I/O.

use super::commands::{command_for_profile, profile_platform, profile_shell, terminal_command};
use super::{
    resolved_terminal, AppResult, AuthMode, Backend, CommandProfile, ConnectionType, Device,
    DeviceKind, HostKeyDialog, OpenTerm, Pending, PendingDeviceInfo, Session, SshForm,
    TerminalKind,
};
use crate::fs_tree::RemoteNode;
use crate::pty::LocalPty;
use crate::ssh::{HostKeyPrompt, SftpHandle, SshAuth, SshConfig, SshSession};
use crate::term::TermView;
use crate::vault::{self, Secret as VaultSecret};
use crate::{system_info, wake};
use anyhow::{anyhow, Context};
use std::sync::mpsc;
use zeroize::{Zeroize, Zeroizing};

fn auth_mode(auth: &SshAuth) -> AuthMode {
    match auth {
        SshAuth::Password(_) => AuthMode::Password,
        SshAuth::KeyFile { .. } => AuthMode::Key,
        SshAuth::Agent => AuthMode::Agent,
    }
}

fn edited_ssh_auth(
    mode: AuthMode,
    password: &str,
    key_path: &str,
    current: &SshAuth,
) -> AppResult<SshAuth> {
    match mode {
        AuthMode::Password if !password.is_empty() => {
            Ok(SshAuth::Password(Zeroizing::new(password.to_string())))
        }
        AuthMode::Password => match current {
            SshAuth::Password(password) => Ok(SshAuth::Password(password.clone())),
            _ => Err(anyhow!("Enter a password for this connection")),
        },
        AuthMode::Key if key_path.is_empty() => Err(anyhow!("Enter a private key path")),
        AuthMode::Key => {
            let passphrase = match current {
                SshAuth::KeyFile { path, passphrase } if path == key_path => passphrase.clone(),
                _ => None,
            };
            Ok(SshAuth::KeyFile {
                path: key_path.into(),
                passphrase,
            })
        }
        AuthMode::Agent => Ok(SshAuth::Agent),
    }
}

fn edited_vault_secret(
    mode: AuthMode,
    password: &str,
    key_path: &str,
    current: VaultSecret,
) -> AppResult<VaultSecret> {
    match mode {
        AuthMode::Password if !password.is_empty() => Ok(VaultSecret::Password {
            password: password.to_string(),
        }),
        AuthMode::Password => match current {
            secret @ VaultSecret::Password { .. } => Ok(secret),
            _ => Err(anyhow!("Enter a password for this connection")),
        },
        AuthMode::Key if key_path.is_empty() => Err(anyhow!("Enter a private key path")),
        AuthMode::Key => {
            let passphrase = match &current {
                VaultSecret::Key {
                    key_path: current_path,
                    passphrase,
                } if current_path == key_path => passphrase.clone(),
                _ => None,
            };
            Ok(VaultSecret::Key {
                key_path: key_path.into(),
                passphrase,
            })
        }
        AuthMode::Agent => Ok(VaultSecret::Agent),
    }
}

impl OpenTerm {
    pub(super) fn load_saved_devices(&mut self) {
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

    pub(super) fn open_local(&mut self, terminal: TerminalKind) -> AppResult<()> {
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

    pub(super) fn connect_from_vault(&mut self, rid: &str, ctx: &egui::Context) {
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

    pub(super) fn open_ssh(&mut self, cfg: SshConfig, ctx: &egui::Context) {
        self.open_ssh_on_device(cfg, None, ctx);
    }

    pub(super) fn open_ssh_on_device(
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

    pub(super) fn open_command_profile(&mut self, profile: CommandProfile, ctx: &egui::Context) {
        let (program, args, label) = match command_for_profile(&profile) {
            Ok(command) => command,
            Err(error) => {
                self.say(ctx, error.to_string(), true);
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

    pub(super) fn begin_edit_device(&mut self, device_id: &str, ctx: &egui::Context) {
        let Some(device) = self
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .cloned()
        else {
            return;
        };
        let mut form = SshForm {
            label: device.label.clone(),
            focus_host: true,
            ..Default::default()
        };
        match device.kind {
            DeviceKind::Local => return,
            DeviceKind::Ssh(cfg) => {
                form.connection = ConnectionType::Ssh;
                form.host = cfg.host;
                form.port = cfg.port.to_string();
                form.user = cfg.user;
                form.mode = auth_mode(&cfg.auth);
                if let SshAuth::KeyFile { path, .. } = &cfg.auth {
                    form.key_path = path.clone();
                }
            }
            DeviceKind::VaultRef(record_id) => {
                let Some(vault) = self.vault.as_ref() else {
                    self.say(ctx, "Vault is unavailable", true);
                    return;
                };
                if !vault.is_unlocked() {
                    self.say(ctx, "Unlock the vault before editing this connection", true);
                    return;
                }
                let Some(record) = vault
                    .records()
                    .iter()
                    .find(|record| record.id == record_id)
                    .cloned()
                else {
                    self.say(ctx, "Saved connection no longer exists", true);
                    return;
                };
                let secret = match vault.decrypt(&record_id) {
                    Ok(secret) => secret,
                    Err(error) => {
                        self.say(ctx, format!("couldn't decrypt connection: {error}"), true);
                        return;
                    }
                };
                form.connection = ConnectionType::Ssh;
                form.host = record.host;
                form.port = record.port.to_string();
                form.user = record.user;
                form.save_to_vault = true;
                match &secret {
                    VaultSecret::Password { .. } => form.mode = AuthMode::Password,
                    VaultSecret::Key { key_path, .. } => {
                        form.mode = AuthMode::Key;
                        form.key_path = key_path.clone();
                    }
                    VaultSecret::Agent => form.mode = AuthMode::Agent,
                }
            }
            DeviceKind::Command(profile) => {
                form.connection = profile.kind;
                form.terminal = profile.terminal;
                form.host = profile.host;
                form.port = profile.port;
                form.user = profile.user;
            }
        }
        form.error = None;
        self.form.password.zeroize();
        self.form = form;
        self.editing_device = Some(device_id.to_string());
        self.rename_device = None;
        self.modal = true;
    }

    pub(super) fn save_device_edit(&mut self, device_id: &str, form: &SshForm) -> AppResult<()> {
        let kind = self
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .map(|device| device.kind.clone())
            .ok_or_else(|| anyhow!("Connection no longer exists"))?;
        let host = form.host.trim().to_string();
        let user = form.user.trim().to_string();
        let port_text = form.port.trim().to_string();
        let requested_label = form.label.trim();

        let label = match kind {
            DeviceKind::Local => return Err(anyhow!("The local device cannot be edited")),
            DeviceKind::Ssh(mut cfg) => {
                let port = port_text.parse().unwrap_or(22);
                cfg.auth =
                    edited_ssh_auth(form.mode, &form.password, form.key_path.trim(), &cfg.auth)?;
                cfg.host = host.clone();
                cfg.port = port;
                cfg.user = user.clone();
                let label = if requested_label.is_empty() {
                    format!("{user}@{host}")
                } else {
                    requested_label.to_string()
                };
                if let Some(device) = self.devices.iter_mut().find(|d| d.id == device_id) {
                    device.kind = DeviceKind::Ssh(cfg);
                    device.label = label.clone();
                }
                label
            }
            DeviceKind::VaultRef(record_id) => {
                let port = port_text.parse().unwrap_or(22);
                let vault = self
                    .vault
                    .as_mut()
                    .ok_or_else(|| anyhow!("Vault is unavailable"))?;
                let current = vault
                    .decrypt(&record_id)
                    .context("couldn't decrypt connection")?;
                let secret =
                    edited_vault_secret(form.mode, &form.password, form.key_path.trim(), current)?;
                let label = if requested_label.is_empty() {
                    format!("{user}@{host}")
                } else {
                    requested_label.to_string()
                };
                vault
                    .update(&record_id, &label, &host, port, &user, &secret)
                    .context("couldn't save connection")?;
                if let Some(device) = self.devices.iter_mut().find(|d| d.id == device_id) {
                    device.label = label.clone();
                }
                label
            }
            DeviceKind::Command(mut profile) => {
                profile.host = host;
                profile.port = port_text;
                profile.user = user;
                profile.terminal = form.terminal;
                let fallback = command_for_profile(&profile)
                    .map(|(_, _, label)| label)
                    .unwrap_or_else(|_| profile.kind.label().to_string());
                let label = if requested_label.is_empty() {
                    fallback
                } else {
                    requested_label.to_string()
                };
                if let Some(device) = self.devices.iter_mut().find(|d| d.id == device_id) {
                    device.kind = DeviceKind::Command(profile);
                    device.label = label.clone();
                }
                label
            }
        };
        for session in self
            .sessions
            .iter_mut()
            .filter(|session| session.device == device_id)
        {
            session.label = label.clone();
        }
        self.connection_errors.remove(device_id);
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

    pub(super) fn poll_pending(&mut self, ctx: &egui::Context) {
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

    pub(super) fn poll_device_info(&mut self) {
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

    pub(super) fn pump(&mut self, ctx: &egui::Context) {
        let mut failed = None;
        let configured_fallback = terminal_command(
            resolved_terminal(&self.settings.default_shell, &self.available_terminals),
            "",
        )
        .map_err(|error| error.to_string());
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

    pub(super) fn device_live(&self, id: &str) -> bool {
        id == "local"
            || self
                .sessions
                .iter()
                .any(|s| s.device == id && s.backend.is_some())
    }

    pub(super) fn active_session(&self) -> Option<&Session> {
        self.active.and_then(|id| {
            self.sessions
                .iter()
                .find(|s| s.id == id && s.device == self.selected)
        })
    }
}
