//! local pty backend (ConPTY on windows, openpty on unix) via portable-pty.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub struct LocalPty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    rx: Receiver<Vec<u8>>,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
}

impl LocalPty {
    pub fn spawn_program(program: &str, args: &[String], cols: u16, rows: u16) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty failed")?;

        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        if let Some(home) = dirs::home_dir() {
            cmd.cwd(home);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "OpenTerm");

        let mut child = pair.slave.spawn_command(cmd).context("spawn shell")?;
        let killer = child.clone_killer();
        drop(pair.slave);

        let writer = pair.master.take_writer().context("take writer")?;
        let mut reader = pair.master.try_clone_reader().context("clone reader")?;
        let (tx, rx) = mpsc::channel::<Vec<u8>>();

        thread::spawn(move || {
            let mut buf = [0u8; 16384];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        crate::wake::poke();
                    }
                }
            }
            let _ = tx.send(b"\r\n\x1b[90m[process exited]\x1b[0m\r\n".to_vec());
            crate::wake::poke();
        });
        thread::spawn(move || {
            let _ = child.wait();
        });

        Ok(Self {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            rx,
            killer: Some(killer),
        })
    }

    pub fn write(&self, data: &[u8]) {
        let mut w = self.writer.lock();
        let _ = w.write_all(data);
        let _ = w.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.lock().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn drain(&self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.rx.try_recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }
}

impl Drop for LocalPty {
    fn drop(&mut self) {
        if let Some(mut k) = self.killer.take() {
            let _ = k.kill();
        }
    }
}

pub fn powershell_shell() -> Option<String> {
    if cfg!(windows) {
        which("pwsh.exe").or_else(|| which("powershell.exe"))
    } else {
        which("pwsh")
    }
}

pub fn bash_shell() -> String {
    if cfg!(windows) {
        which("bash.exe").unwrap_or_else(|| "bash.exe".into())
    } else {
        which("bash").unwrap_or_else(|| "/bin/bash".into())
    }
}

pub fn cmd_shell() -> String {
    std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
}

fn which(bin: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join(bin))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}
