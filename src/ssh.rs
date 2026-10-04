//! ssh — thin shim preserving the SshSession/SshConfig API from v0.1.
//! Delegates all actual SSH work to the go `otm-agent` sidecar (see agent.rs).

use crate::agent::{Agent, AgentMsg, AuthSpec, DeviceInfo, DirectoryListing, SyncChange};
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Process-wide handle to the running `otm-agent` sidecar. Spawned on first use.
static AGENT: OnceLock<Arc<Agent>> = OnceLock::new();

fn agent() -> Result<Arc<Agent>> {
    if let Some(a) = AGENT.get() {
        return Ok(a.clone());
    }
    // search path: next to our exe, then PATH
    let exe = find_agent_binary()?;
    let a = Arc::new(Agent::spawn(&exe)?);
    let _ = AGENT.set(a.clone());
    Ok(a)
}

fn find_agent_binary() -> Result<std::path::PathBuf> {
    // 1. same dir as the current exe
    if let Ok(mut p) = std::env::current_exe() {
        p.pop();
        let name = if cfg!(windows) {
            "otm-agent.exe"
        } else {
            "otm-agent"
        };
        let candidate = p.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    // 2. PATH
    let name = if cfg!(windows) {
        "otm-agent.exe"
    } else {
        "otm-agent"
    };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(anyhow!("otm-agent binary not found next to exe or in PATH"))
}

#[derive(Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub(crate) trust_host_key: Option<String>,
    pub(crate) replace_host_key: bool,
    pub(crate) known_fingerprints: Vec<String>,
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub enum SshAuth {
    Password(Zeroizing<String>),
    KeyFile {
        path: String,
        passphrase: Option<Zeroizing<String>>,
    },
    Agent,
}

#[derive(Debug)]
pub struct HostKeyPrompt {
    pub host: String,
    pub fingerprint: String,
    pub key: String,
    pub known_hosts: String,
    pub known_fingerprints: Vec<String>,
    pub changed: bool,
}

impl std::fmt::Display for HostKeyPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown SSH host key for {} ({})",
            self.host, self.fingerprint
        )
    }
}

impl std::error::Error for HostKeyPrompt {}

impl SshAuth {
    fn to_spec(&self) -> AuthSpec {
        match self {
            SshAuth::Password(_) => AuthSpec {
                kind: "password".into(),
                key_path: None,
            },
            SshAuth::KeyFile { path, .. } => AuthSpec {
                kind: "key".into(),
                key_path: Some(path.clone()),
            },
            SshAuth::Agent => AuthSpec {
                kind: "agent".into(),
                key_path: None,
            },
        }
    }

    fn secret(&self) -> Option<&[u8]> {
        match self {
            Self::Password(password) => Some(password.as_bytes()),
            Self::KeyFile {
                passphrase: Some(passphrase),
                ..
            } => Some(passphrase.as_bytes()),
            Self::KeyFile {
                passphrase: None, ..
            }
            | Self::Agent => None,
        }
    }
}

pub struct SshSession {
    agent: Arc<Agent>,
    pub session_id: String,
    pub channel_id: String,
    pub label: String,
    rx: Mutex<Receiver<AgentMsg>>,
    out_buf: Mutex<Vec<u8>>,
}

/// Cloneable SFTP-only view of an SSH connection. It deliberately does not
/// disconnect the session when dropped, so directory loads can run on worker
/// threads while the terminal remains owned by `SshSession`.
#[derive(Clone)]
pub struct SftpHandle {
    agent: Arc<Agent>,
    session_id: String,
}

impl SftpHandle {
    pub fn list_dir(&self, path: &str) -> Result<DirectoryListing> {
        self.agent.sftp_list(&self.session_id, path)
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.agent.sftp_read(&self.session_id, path)
    }

    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.agent.sftp_write(&self.session_id, path, data)
    }

    pub fn download(&self, remote: &str, local: &str, recursive: bool) -> Result<String> {
        self.agent.sftp_transfer(
            &self.session_id,
            if recursive {
                "sftp_get_tree"
            } else {
                "sftp_get"
            },
            remote,
            local,
        )
    }

    pub fn upload(&self, local: &str, remote: &str, recursive: bool) -> Result<String> {
        self.agent.sftp_transfer(
            &self.session_id,
            if recursive {
                "sftp_put_tree"
            } else {
                "sftp_put"
            },
            remote,
            local,
        )
    }

    pub fn sync_plan(&self, remote: &str, local: &str, direction: &str) -> Result<Vec<SyncChange>> {
        self.agent
            .sftp_sync_plan(&self.session_id, remote, local, direction)
    }

    pub fn sync_apply(&self, remote: &str, local: &str, direction: &str) -> Result<String> {
        self.agent
            .sftp_sync_apply(&self.session_id, remote, local, direction)
    }

    pub fn device_info(&self) -> Result<DeviceInfo> {
        self.agent.device_info(&self.session_id)
    }
}

impl SshSession {
    /// Connect — matches the v0.1 API. Lazily boots the shared agent on first call.
    pub fn connect(cfg: SshConfig, cols: u16, rows: u16) -> Result<Self> {
        let agent = agent()?;
        let session_id = uuid::Uuid::new_v4().to_string();
        let channel_id = uuid::Uuid::new_v4().to_string();
        let rx = agent.subscribe();

        agent.dial(
            &session_id,
            &cfg.host,
            cfg.port,
            &cfg.user,
            cfg.auth.to_spec(),
            cfg.auth.secret(),
            cfg.trust_host_key.clone(),
            cfg.replace_host_key,
            cfg.known_fingerprints.clone(),
        )?;

        // wait for dial reply (bounded)
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut connected = false;
        let mut err_msg = String::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(m) if m.op.as_deref() == Some("agent_exit") => {
                    return Err(anyhow!(m
                        .err
                        .unwrap_or_else(|| "SSH helper exited unexpectedly".into())));
                }
                Ok(m) if m.id.as_deref() == Some(session_id.as_str()) => {
                    if m.op.as_deref() == Some("host_key") {
                        return Err(HostKeyPrompt {
                            host: format!("{}:{}", cfg.host, cfg.port),
                            fingerprint: m
                                .fingerprint
                                .unwrap_or_else(|| "unknown fingerprint".into()),
                            key: m.host_key.unwrap_or_default(),
                            known_hosts: m.known_hosts.unwrap_or_default(),
                            known_fingerprints: m.known_fingerprints.unwrap_or_default(),
                            changed: m.replace_host_key.unwrap_or(false),
                        }
                        .into());
                    }
                    if m.ok.unwrap_or(false) {
                        connected = true;
                        break;
                    }
                    err_msg = m.err.unwrap_or_else(|| "unknown error".into());
                    break;
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        }
        if !connected {
            return Err(anyhow!("ssh dial failed: {err_msg}"));
        }

        // open a shell channel
        agent.exec_shell(&session_id, &channel_id, cols, rows)?;

        Ok(Self {
            agent,
            session_id,
            channel_id: channel_id.clone(),
            label: format!("ssh · {}@{}", cfg.user, cfg.host),
            rx: Mutex::new(rx),
            out_buf: Mutex::new(Vec::new()),
        })
    }

    pub fn write(&self, data: &[u8]) {
        let _ = self.agent.write(&self.channel_id, data);
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.agent.resize(&self.channel_id, cols, rows);
    }
    pub fn drain(&self) -> Vec<u8> {
        let rx = self.rx.lock();
        let mut out = self.out_buf.lock();
        while let Ok(m) = rx.try_recv() {
            if m.channel.as_deref() == Some(self.channel_id.as_str()) {
                if let Some(d) = self.agent.decode_data(&m) {
                    out.extend_from_slice(&d);
                }
            }
        }
        std::mem::take(&mut *out)
    }
    pub fn sftp(&self) -> SftpHandle {
        SftpHandle {
            agent: self.agent.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        let _ = self.agent.disconnect(&self.session_id);
    }
}
