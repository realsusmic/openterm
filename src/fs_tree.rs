//! fs_tree — lazy directory tree for the sidebar.
use crate::agent::DirectoryListing;
use std::path::{Path, PathBuf};

pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<Node>>,
}

impl Node {
    pub fn root(p: &Path) -> Self {
        Self {
            path: p.to_path_buf(),
            name: p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            is_dir: true,
            expanded: true,
            children: None,
        }
    }

    pub fn ensure_loaded(&mut self) {
        if !self.is_dir || self.children.is_some() {
            return;
        }
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.path) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || is_hidden(&e) {
                    continue;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                out.push(Node {
                    path: e.path(),
                    name,
                    is_dir,
                    expanded: false,
                    children: None,
                });
            }
        }
        out.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.children = Some(out);
    }

    pub fn refresh(&mut self) {
        self.children = None;
        self.ensure_loaded();
    }
}

#[cfg(windows)]
fn is_hidden(e: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM — hides AppData, NTUSER.DAT, desktop.ini …
    e.metadata()
        .map(|m| m.file_attributes() & 0x6 != 0)
        .unwrap_or(false)
}
#[cfg(not(windows))]
fn is_hidden(_: &std::fs::DirEntry) -> bool {
    false
}

/// A server-neutral SFTP tree. Remote paths stay as protocol strings rather
/// than `PathBuf`, because interpreting them with the local OS path rules is
/// wrong whenever the two machines run different operating systems.
pub struct RemoteNode {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<RemoteNode>>,
    pub loading: bool,
    pub error: Option<String>,
}

impl RemoteNode {
    pub fn root(path: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            name: remote_name(&path),
            path,
            is_dir: true,
            expanded: true,
            children: None,
            loading: true,
            error: None,
        }
    }

    pub fn begin_load(&mut self, path: &str) -> bool {
        let Some(node) = self.find_mut(path) else {
            return false;
        };
        if !node.is_dir || node.loading {
            return false;
        }
        node.loading = true;
        node.error = None;
        true
    }

    pub fn apply_listing(&mut self, requested: &str, listing: DirectoryListing) {
        let is_root = self.path == requested || (self.path == "." && requested == ".");
        let Some(node) = self.find_mut(requested) else {
            return;
        };
        if is_root {
            node.path = listing.path.clone();
            node.name = remote_name(&listing.path);
        }
        let base = listing.path;
        let mut children: Vec<_> = listing
            .entries
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| RemoteNode {
                path: remote_join(&base, &e.name),
                name: e.name,
                is_dir: e.is_dir,
                expanded: false,
                children: None,
                loading: false,
                error: None,
            })
            .collect();
        children.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        node.children = Some(children);
        node.loading = false;
        node.error = None;
    }

    pub fn fail_load(&mut self, requested: &str, error: String) {
        if let Some(node) = self.find_mut(requested) {
            node.loading = false;
            node.error = Some(error);
        }
    }

    pub fn find_mut(&mut self, path: &str) -> Option<&mut RemoteNode> {
        if self.path == path || (self.path == "." && path == ".") {
            return Some(self);
        }
        self.children
            .as_mut()?
            .iter_mut()
            .find_map(|child| child.find_mut(path))
    }
}

pub fn remote_join(base: &str, name: &str) -> String {
    if base == "/" {
        format!("/{name}")
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

pub fn remote_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return None;
    }
    let slash = trimmed.rfind('/')?;
    if slash == 0 {
        Some("/".into())
    } else if slash == 2 && trimmed.as_bytes().get(1) == Some(&b':') {
        Some(format!("{}/", &trimmed[..2]))
    } else {
        Some(trimmed[..slash].to_string())
    }
}

fn remote_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn remote_paths_do_not_use_local_platform_rules() {
        assert_eq!(remote_join("/home/alice", "src"), "/home/alice/src");
        assert_eq!(remote_join("C:/Users/alice", "src"), "C:/Users/alice/src");
        assert_eq!(remote_parent("/home/alice/src"), Some("/home/alice".into()));
        assert_eq!(remote_parent("C:/Users/alice"), Some("C:/Users".into()));
        assert_eq!(remote_parent("C:/"), None);
    }
}
