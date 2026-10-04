//! agent — control-side client for `otm-agent` (go sidecar).
//!
//! Spawns the agent binary as a child process, communicates over its stdin/stdout
//! with newline-delimited JSON. Each session/channel is identified by string ids.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const AGENT_PROTOCOL_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub size: i64,
    pub mode: u32,
    #[serde(rename = "dir")]
    pub is_dir: bool,
    pub mtim: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SyncChange {
    pub path: String,
    pub kind: String,
    pub size: i64,
}

#[derive(Debug, Clone)]
pub struct DirectoryListing {
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub hostname: String,
    pub platform: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct AgentMsg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(rename = "chan", skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_hosts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_fingerprints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_host_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_host_key: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<FileEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<SyncChange>>,
}

#[derive(Serialize, Deserialize, Clone, Zeroize, ZeroizeOnDrop)]
pub struct AuthSpec {
    pub kind: String, // "password" | "key" | "agent"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
}

pub struct Agent {
    _child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    // One broadcast channel; each SSH session subscribes and filters.
    subscribers: Arc<Mutex<Vec<Sender<AgentMsg>>>>,
}

impl Agent {
    pub fn spawn(exe: &std::path::Path) -> Result<Self> {
        let mut cmd = Command::new(exe);
        #[cfg(windows)]
        {
            // don't flash a console window for the sidecar
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("spawn otm-agent: {e}"))?;

        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let subscribers: Arc<Mutex<Vec<Sender<AgentMsg>>>> = Arc::new(Mutex::new(Vec::new()));
        let (router_tx, router_rx) = mpsc::channel::<AgentMsg>();
        let last_stderr = Arc::new(Mutex::new(None::<String>));

        // Keep the sidecar invisible on Windows, but preserve its last useful
        // diagnostic so a crash becomes an actionable connection error.
        {
            let last_stderr = last_stderr.clone();
            thread::spawn(move || capture_stderr(stderr, last_stderr));
        }

        // reader: parse JSON lines, push to router
        {
            let router_tx = router_tx.clone();
            thread::spawn(move || reader_loop(stdout, router_tx, last_stderr));
        }
        // dispatcher: fan out to subscribers
        {
            let subs = subscribers.clone();
            thread::spawn(move || {
                while let Ok(m) = router_rx.recv() {
                    let subs = subs.lock();
                    // Drop subscriptions whose receiver has gone away. SFTP
                    // requests use short-lived subscriptions, so retaining dead
                    // senders here would otherwise grow the list forever.
                    let mut subs = subs;
                    subs.retain(|s| s.send(m.clone()).is_ok());
                }
            });
        }

        let mut agent = Self {
            _child: child,
            stdin,
            subscribers,
        };
        let handshake = agent.request(
            AgentMsg {
                op: Some("hello".into()),
                protocol: Some(AGENT_PROTOCOL_VERSION),
                ..Default::default()
            },
            Duration::from_secs(2),
            "agent handshake",
        );
        match handshake {
            Ok(reply) if reply.protocol == Some(AGENT_PROTOCOL_VERSION) => Ok(agent),
            Ok(reply) => {
                let _ = agent._child.kill();
                Err(anyhow!(
                    "otm-agent protocol mismatch: app requires {}, agent reported {}; rebuild and ship both binaries together",
                    AGENT_PROTOCOL_VERSION,
                    reply.protocol.map_or_else(|| "none".into(), |value| value.to_string())
                ))
            }
            Err(error) => {
                let _ = agent._child.kill();
                Err(anyhow!(
                    "otm-agent is incompatible with this OpenTerm build ({error}); rebuild and ship both binaries together"
                ))
            }
        }
    }

    pub fn subscribe(&self) -> Receiver<AgentMsg> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().push(tx);
        rx
    }

    pub fn send(&self, m: &AgentMsg) -> Result<()> {
        // Scrub every serialized control frame after it reaches the pipe. Dial
        // secrets use send_with_secret and never enter this JSON allocation.
        let mut s = Zeroizing::new(serde_json::to_string(m)?);
        s.push('\n');
        self.stdin.lock().write_all(s.as_bytes())?;
        Ok(())
    }

    fn send_with_secret(&self, mut message: AgentMsg, secret: Option<&[u8]>) -> Result<()> {
        let secret = secret.filter(|value| !value.is_empty());
        message.secret_len = secret.map(<[u8]>::len);
        let mut frame = Zeroizing::new(serde_json::to_string(&message)?);
        frame.push('\n');
        let mut stdin = self.stdin.lock();
        stdin.write_all(frame.as_bytes())?;
        if let Some(secret) = secret {
            stdin.write_all(secret)?;
            stdin.write_all(b"\n")?;
        }
        stdin.flush()?;
        Ok(())
    }

    fn request(&self, mut message: AgentMsg, timeout: Duration, label: &str) -> Result<AgentMsg> {
        let id = uuid::Uuid::new_v4().to_string();
        let rx = self.subscribe();
        message.id = Some(id.clone());
        self.send(&message)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(anyhow!("{label} timed out"));
            }
            match rx.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(reply) if reply.op.as_deref() == Some("agent_exit") => {
                    return Err(anyhow!(reply
                        .err
                        .unwrap_or_else(|| "SSH helper exited unexpectedly".into())));
                }
                Ok(reply) if reply.id.as_deref() == Some(id.as_str()) => {
                    if reply.ok.unwrap_or(false) {
                        return Ok(reply);
                    }
                    return Err(anyhow!(reply
                        .err
                        .unwrap_or_else(|| format!("{label} failed"))));
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(anyhow!("SSH agent stopped during {label}")),
            }
        }
    }

    pub fn dial(
        &self,
        id: &str,
        host: &str,
        port: u16,
        user: &str,
        auth: AuthSpec,
        secret: Option<&[u8]>,
        trust_host_key: Option<String>,
        replace_host_key: bool,
        known_fingerprints: Vec<String>,
    ) -> Result<()> {
        self.send_with_secret(
            AgentMsg {
                op: Some("dial".into()),
                id: Some(id.into()),
                host: Some(host.into()),
                port: Some(port),
                user: Some(user.into()),
                auth: Some(auth),
                trust_host_key,
                replace_host_key: replace_host_key.then_some(true),
                known_fingerprints: (!known_fingerprints.is_empty()).then_some(known_fingerprints),
                ..Default::default()
            },
            secret,
        )
    }
    pub fn exec_shell(&self, session: &str, channel: &str, cols: u16, rows: u16) -> Result<()> {
        self.send(&AgentMsg {
            op: Some("exec".into()),
            session: Some(session.into()),
            channel: Some(channel.into()),
            pty: Some(true),
            cols: Some(cols),
            rows: Some(rows),
            ..Default::default()
        })
    }
    pub fn write(&self, channel: &str, data: &[u8]) -> Result<()> {
        self.send(&AgentMsg {
            op: Some("write".into()),
            channel: Some(channel.into()),
            data: Some(B64.encode(data)),
            ..Default::default()
        })
    }
    pub fn resize(&self, channel: &str, cols: u16, rows: u16) -> Result<()> {
        self.send(&AgentMsg {
            op: Some("resize".into()),
            channel: Some(channel.into()),
            cols: Some(cols),
            rows: Some(rows),
            ..Default::default()
        })
    }
    pub fn disconnect(&self, session: &str) -> Result<()> {
        self.send(&AgentMsg {
            op: Some("disconnect".into()),
            session: Some(session.into()),
            ..Default::default()
        })
    }
    pub fn sftp_list(&self, session: &str, path: &str) -> Result<DirectoryListing> {
        let id = uuid::Uuid::new_v4().to_string();
        // Subscribe before sending so a very fast local/loopback server cannot
        // win the race and deliver the reply before we are listening.
        let rx = self.subscribe();
        self.send(&AgentMsg {
            op: Some("sftp_ls".into()),
            id: Some(id.clone()),
            session: Some(session.into()),
            path: Some(path.into()),
            ..Default::default()
        })?;

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(anyhow!("SFTP listing timed out"));
            }
            match rx.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(m) if m.id.as_deref() == Some(id.as_str()) => {
                    if !m.ok.unwrap_or(false) {
                        return Err(anyhow!(m
                            .err
                            .unwrap_or_else(|| "SFTP listing failed".into())));
                    }
                    return Ok(DirectoryListing {
                        path: m.path.unwrap_or_else(|| path.to_string()),
                        entries: m.entries.unwrap_or_default(),
                    });
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(anyhow!("SSH agent stopped while listing files")),
            }
        }
    }

    pub fn sftp_read(&self, session: &str, path: &str) -> Result<Vec<u8>> {
        let id = uuid::Uuid::new_v4().to_string();
        let rx = self.subscribe();
        self.send(&AgentMsg {
            op: Some("sftp_read".into()),
            id: Some(id.clone()),
            session: Some(session.into()),
            path: Some(path.into()),
            ..Default::default()
        })?;

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(anyhow!("remote file read timed out"));
            }
            match rx.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(m) if m.id.as_deref() == Some(id.as_str()) => {
                    if !m.ok.unwrap_or(false) {
                        return Err(anyhow!(m
                            .err
                            .unwrap_or_else(|| "remote file read failed".into())));
                    }
                    let data = m.data.unwrap_or_default();
                    return B64
                        .decode(data)
                        .map_err(|e| anyhow!("invalid remote file data: {e}"));
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(anyhow!("SSH agent stopped while reading file")),
            }
        }
    }

    pub fn sftp_write(&self, session: &str, path: &str, data: &[u8]) -> Result<()> {
        self.request(
            AgentMsg {
                op: Some("sftp_write".into()),
                session: Some(session.into()),
                path: Some(path.into()),
                data: Some(B64.encode(data)),
                ..Default::default()
            },
            Duration::from_secs(30),
            "remote file save",
        )?;
        Ok(())
    }

    pub fn sftp_transfer(
        &self,
        session: &str,
        op: &str,
        remote: &str,
        local: &str,
    ) -> Result<String> {
        let reply = self.request(
            AgentMsg {
                op: Some(op.into()),
                session: Some(session.into()),
                remote: Some(remote.into()),
                local: Some(local.into()),
                ..Default::default()
            },
            Duration::from_secs(60 * 30),
            "file transfer",
        )?;
        Ok(reply.message.unwrap_or_else(|| "transfer complete".into()))
    }

    pub fn sftp_sync_plan(
        &self,
        session: &str,
        remote: &str,
        local: &str,
        direction: &str,
    ) -> Result<Vec<SyncChange>> {
        let reply = self.request(
            AgentMsg {
                op: Some("sftp_sync_plan".into()),
                session: Some(session.into()),
                remote: Some(remote.into()),
                local: Some(local.into()),
                direction: Some(direction.into()),
                ..Default::default()
            },
            Duration::from_secs(60 * 2),
            "sync preview",
        )?;
        Ok(reply.changes.unwrap_or_default())
    }

    pub fn sftp_sync_apply(
        &self,
        session: &str,
        remote: &str,
        local: &str,
        direction: &str,
    ) -> Result<String> {
        let reply = self.request(
            AgentMsg {
                op: Some("sftp_sync_apply".into()),
                session: Some(session.into()),
                remote: Some(remote.into()),
                local: Some(local.into()),
                direction: Some(direction.into()),
                ..Default::default()
            },
            Duration::from_secs(60 * 10),
            "folder sync",
        )?;
        Ok(reply.message.unwrap_or_else(|| "sync complete".into()))
    }

    pub fn device_info(&self, session: &str) -> Result<DeviceInfo> {
        let id = uuid::Uuid::new_v4().to_string();
        let rx = self.subscribe();
        self.send(&AgentMsg {
            op: Some("device_info".into()),
            id: Some(id.clone()),
            session: Some(session.into()),
            ..Default::default()
        })?;

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(anyhow!("remote device info timed out"));
            }
            match rx.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(m) if m.id.as_deref() == Some(id.as_str()) => {
                    if !m.ok.unwrap_or(false) {
                        return Err(anyhow!(m
                            .err
                            .unwrap_or_else(|| "remote device info failed".into())));
                    }
                    return Ok(DeviceInfo {
                        hostname: m.hostname.unwrap_or_else(|| "unknown host".into()),
                        platform: m.platform.unwrap_or_else(|| "Unknown".into()),
                    });
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(anyhow!("SSH agent stopped while reading device info")),
            }
        }
    }
    pub fn decode_data(&self, m: &AgentMsg) -> Option<Vec<u8>> {
        m.data.as_ref().and_then(|d| B64.decode(d).ok())
    }
}

fn capture_stderr(stderr: ChildStderr, last_stderr: Arc<Mutex<Option<String>>>) {
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        let line = line.trim();
        if !line.is_empty() {
            log::warn!("otm-agent: {line}");
            *last_stderr.lock() = Some(line.to_string());
        }
    }
}

fn reader_loop(stdout: ChildStdout, tx: Sender<AgentMsg>, last_stderr: Arc<Mutex<Option<String>>>) {
    let r = BufReader::new(stdout);
    for line in r.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(m) = serde_json::from_str::<AgentMsg>(&line) {
            let _ = tx.send(m);
            crate::wake::poke();
        }
    }
    // stderr and stdout close together on process exit; give the stderr reader
    // a moment to publish the final panic/fatal line before reporting the exit.
    thread::sleep(Duration::from_millis(20));
    let detail = last_stderr.lock().clone();
    let err = detail.map_or_else(
        || "SSH helper exited unexpectedly".to_string(),
        |line| format!("SSH helper exited unexpectedly: {line}"),
    );
    let _ = tx.send(AgentMsg {
        op: Some("agent_exit".into()),
        ok: Some(false),
        err: Some(err),
        ..Default::default()
    });
    crate::wake::poke();
}
