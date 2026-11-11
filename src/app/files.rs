//! Background file reads, writes, and transfers.

use super::{
    BinaryFileDialog, OpenTerm, PendingLocalFile, PendingRemote, PendingRemoteFile,
    PendingRemoteWrite, TransferResult,
};
use crate::ssh::SftpHandle;
use crate::wake;
use anyhow::{anyhow, Context};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const MAX_LOCAL_EDITOR_FILE: u64 = 256 * 1024 * 1024;

pub(super) fn suggested_download_path(remote: &str, fallback: &str) -> String {
    let name = remote
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next();
    let name = name
        .filter(|name| {
            !name.is_empty()
                && *name != "."
                && *name != ".."
                && !name.chars().any(|c| c.is_control() || c == ':')
        })
        .unwrap_or(fallback);
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_default()
        .join(name)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_reads_up_to_the_limit() {
        let path =
            std::env::temp_dir().join(format!("openterm-editor-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"1234").unwrap();
        let exact = read_editor_file(&path, 4);
        let oversized = read_editor_file(&path, 3);
        std::fs::remove_file(path).unwrap();
        assert_eq!(exact.unwrap(), b"1234");
        assert!(oversized.is_err());
    }
}

fn read_editor_file(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let metadata = path.metadata()?;
    anyhow::ensure!(metadata.is_file(), "the editor can only open regular files");
    let oversized = || {
        anyhow!(
            "file exceeds the {} MiB editor limit",
            limit / (1024 * 1024)
        )
    };
    if metadata.len() > limit {
        return Err(oversized());
    }
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "the editor can only open regular files");
    if metadata.len() > limit {
        return Err(oversized());
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(metadata.len() as usize)?;
    let mut chunk = [0; 64 * 1024];
    loop {
        let count = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 + count as u64 > limit {
            return Err(oversized());
        }
        bytes.try_reserve_exact(count)?;
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes)
}

impl OpenTerm {
    pub(super) fn large_file_notice(&mut self, ctx: &egui::Context) {
        self.flash = Some((
            crate::editor::LARGE_FILE_NOTICE.into(),
            false,
            ctx.input(|i| i.time) + 10.0,
        ));
    }

    pub(super) fn spawn_local_file(&mut self, path: PathBuf) {
        let (tx, rx) = mpsc::channel();
        let request_path = path.clone();
        std::thread::spawn(move || {
            let result = read_editor_file(&request_path, MAX_LOCAL_EDITOR_FILE)
                .with_context(|| format!("read {}", request_path.display()));
            let _ = tx.send(result);
            wake::poke();
        });
        self.pending_local_files.push(PendingLocalFile { path, rx });
    }

    pub(super) fn spawn_remote_load(&mut self, session_id: u64, path: String, sftp: SftpHandle) {
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

    pub(super) fn spawn_remote_file(&mut self, session_id: u64, path: String, sftp: SftpHandle) {
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

    pub(super) fn spawn_remote_write(
        &mut self,
        save: crate::editor::RemoteSave,
    ) -> std::result::Result<(), (crate::editor::RemoteSave, anyhow::Error)> {
        let Some(sftp) = self
            .sessions
            .iter()
            .find(|session| session.id == save.session_id)
            .and_then(|session| session.backend.as_ref()?.sftp())
        else {
            return Err((save, anyhow!("SSH session is no longer available")));
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

    pub(super) fn poll_remote(&mut self, ctx: &egui::Context) {
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

    pub(super) fn poll_remote_files(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.pending_remote_files.len() {
            match self.pending_remote_files[i].rx.try_recv() {
                Ok(result) => {
                    let p = self.pending_remote_files.remove(i);
                    match result {
                        Ok(bytes) if !crate::editor::is_text_bytes(&bytes) => {
                            let local = suggested_download_path(&p.path, "download.bin");
                            self.binary_file_dialog = Some(BinaryFileDialog {
                                remote: p.path,
                                local,
                                bytes,
                                error: None,
                            });
                        }
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

    pub(super) fn poll_binary_download(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_binary_download.as_ref() else {
            return;
        };
        match pending.rx.try_recv() {
            Ok((result, bytes)) => {
                let pending = self.pending_binary_download.take().unwrap();
                match result {
                    Ok(()) => {
                        let path = PathBuf::from(&pending.local);
                        if pending.view_hex {
                            self.editor.open_hex_bytes(&path, bytes);
                            self.show_editor = true;
                            self.say(ctx, "downloaded and opened hex view", false);
                        } else {
                            self.say(ctx, format!("downloaded {}", path.display()), false);
                        }
                    }
                    Err(error) => {
                        self.binary_file_dialog = Some(BinaryFileDialog {
                            remote: pending.remote,
                            local: pending.local,
                            bytes,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending_binary_download = None;
                self.say(ctx, "binary download worker stopped", true);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    pub(super) fn poll_local_files(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.pending_local_files.len() {
            match self.pending_local_files[i].rx.try_recv() {
                Ok(result) => {
                    let pending = self.pending_local_files.remove(i);
                    match result {
                        Ok(bytes) => match self.editor.open_bytes(&pending.path, bytes) {
                            Ok(()) => self.show_editor = true,
                            Err(error) => self.say(ctx, format!("can't open: {error}"), true),
                        },
                        Err(error) => self.say(ctx, format!("can't open: {error}"), true),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => i += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_local_files.remove(i);
                }
            }
        }
    }

    pub(super) fn poll_remote_writes(&mut self, ctx: &egui::Context) {
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

    pub(super) fn poll_transfer(&mut self, ctx: &egui::Context) {
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
}
