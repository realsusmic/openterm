//! Background file reads, writes, and transfers.

use super::{
    BinaryFileDialog, OpenTerm, PendingLocalFile, PendingRemote, PendingRemoteFile,
    PendingRemoteWrite, TransferResult,
};
use crate::ssh::SftpHandle;
use crate::wake;
use anyhow::{anyhow, Context};
use std::path::PathBuf;
use std::sync::mpsc;

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
            let result = std::fs::read(&request_path)
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
                            let name = p
                                .path
                                .trim_end_matches(['/', '\\'])
                                .rsplit(['/', '\\'])
                                .next()
                                .filter(|name| !name.is_empty())
                                .unwrap_or("download.bin")
                                .to_string();
                            let base = dirs::download_dir()
                                .or_else(dirs::home_dir)
                                .unwrap_or_default();
                            self.binary_file_dialog = Some(BinaryFileDialog {
                                remote: p.path,
                                local: base.join(name).display().to_string(),
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
