//! vault — encrypted local store for SSH credentials.
//!
//! Design:
//! * Per-vault random 16-byte salt.
//! * Per-secret random 24-byte nonce.
//! * On disk we store ONLY: salt, (per-record nonce + ciphertext), metadata.
//!   The derived key never touches disk and is zeroized the moment the vault is locked.
//!   When the user enables an unlock grace period, a time-limited copy is held
//!   by the operating-system credential store, never in the settings file.
//!
//! Two unlock modes, chosen at vault creation and recorded in the header:
//!   * `Password`   — argon2id over the user's master password (requires typing it each session)
//!   * `OsKeystore` — a random 32-byte seed held in Windows DPAPI / macOS Keychain /
//!                    Linux Secret Service, run through argon2id with the vault's salt
//!                    to produce the same 256-bit key material shape as password mode.
//!
//! File layout (`vault.json` in the config dir):
//! ```text
//! { "v":1, "mode":"password"|"os",
//!   "salt": base64,
//!   "params": { "m_cost":65536, "t_cost":3, "p_cost":1 },
//!   "records": [ { "id","label","host","port","user","created",
//!                  "nonce","ct" } ] }
//! ```
//! `ct` is XChaCha20-Poly1305 ciphertext of a JSON Secret blob. The record's
//! public fields (host/user/label/port) are additional authenticated data, so
//! swapping them on disk fails the Poly1305 check.

use anyhow::{anyhow, bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, Payload},
    KeyInit, XChaCha20Poly1305, XNonce,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const VAULT_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const KEY_CHECK_PLAINTEXT: &[u8] = b"openterm-vault-key-check-v1";
const KEY_CHECK_AAD: &[u8] = b"openterm-vault-header-v1";
const KEYRING_SERVICE: &str = "dev.susmic.openterm";
const KEYRING_SEED_USER: &str = "vault-seed";
const KEYRING_REMEMBERED_USER: &str = "vault-remembered-key";

// -------- argon2id tuning (interactive desktop; ~250ms on a modern laptop) --------
const DEFAULT_M_COST: u32 = 64 * 1024; // 64 MiB
const DEFAULT_T_COST: u32 = 3;
const DEFAULT_P_COST: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Password,
    Os,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
struct KdfParams {
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}
impl Default for KdfParams {
    fn default() -> Self {
        Self {
            m_cost: DEFAULT_M_COST,
            t_cost: DEFAULT_T_COST,
            p_cost: DEFAULT_P_COST,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Record {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub created: u64,
    nonce: String, // base64
    ct: String,    // base64
}

#[derive(Serialize, Deserialize, Clone)]
pub struct VaultFile {
    v: u8,
    mode: Mode,
    salt: String,
    #[serde(default)]
    params: KdfParams,
    #[serde(default)]
    records: Vec<Record>,
    #[serde(default)]
    check: Option<KeyCheck>,
}

#[derive(Serialize, Deserialize, Clone)]
struct KeyCheck {
    nonce: String,
    ct: String,
}

#[derive(Serialize, Deserialize, ZeroizeOnDrop)]
struct RememberedUnlock {
    v: u8,
    salt: String,
    expires_at: u64,
    key: String,
}

/// Plaintext secret stored inside the encrypted blob of a Record.
///
/// Fields are private + `ZeroizeOnDrop`: callers get copies through the methods
/// below so the stored buffer is always wiped on scope exit.
#[derive(Serialize, Deserialize, ZeroizeOnDrop)]
#[serde(tag = "kind")]
pub enum Secret {
    #[serde(rename = "password")]
    Password { password: String },
    #[serde(rename = "key")]
    Key {
        key_path: String,
        passphrase: Option<String>,
    },
    #[serde(rename = "agent")]
    Agent,
}

#[derive(Clone)]
pub enum PlainAuth {
    Password(Zeroizing<String>),
    Key {
        path: String,
        passphrase: Option<Zeroizing<String>>,
    },
    Agent,
}

impl Secret {
    /// Copy out the secret in a form the SSH layer can use. The original stays
    /// wrapped in ZeroizeOnDrop and vanishes on scope exit; the returned String
    /// becomes the caller's responsibility.
    pub fn to_auth(&self) -> PlainAuth {
        match self {
            Self::Password { password } => PlainAuth::Password(Zeroizing::new(password.clone())),
            Self::Key {
                key_path,
                passphrase,
            } => PlainAuth::Key {
                path: key_path.clone(),
                passphrase: passphrase
                    .as_ref()
                    .map(|value| Zeroizing::new(value.clone())),
            },
            Self::Agent => PlainAuth::Agent,
        }
    }
}

// Heap allocation keeps the address stable while the key's page is locked.
struct Key {
    bytes: Box<[u8; KEY_LEN]>,
}

#[cfg(unix)]
fn lock_key_memory(bytes: &mut [u8]) -> bool {
    unsafe { libc::mlock(bytes.as_ptr().cast(), bytes.len()) == 0 }
}

#[cfg(unix)]
fn unlock_key_memory(bytes: &mut [u8]) {
    unsafe {
        libc::munlock(bytes.as_ptr().cast(), bytes.len());
    }
}

#[cfg(windows)]
fn lock_key_memory(bytes: &mut [u8]) -> bool {
    unsafe {
        windows_sys::Win32::System::Memory::VirtualLock(bytes.as_ptr().cast(), bytes.len()) != 0
    }
}

#[cfg(windows)]
fn unlock_key_memory(bytes: &mut [u8]) {
    unsafe {
        windows_sys::Win32::System::Memory::VirtualUnlock(bytes.as_ptr().cast(), bytes.len());
    }
}

impl Key {
    fn new(bytes: [u8; KEY_LEN]) -> Result<Self> {
        let mut source = bytes;
        let mut bytes = Box::new([0u8; KEY_LEN]);
        bytes.copy_from_slice(&source);
        source.zeroize();
        if !lock_key_memory(bytes.as_mut_slice()) {
            bytes.zeroize();
            return Err(anyhow!(
                "lock vault key memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { bytes })
    }

    fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.bytes.zeroize();
        unlock_key_memory(self.bytes.as_mut_slice());
    }
}

// -------- in-memory handle --------

/// Locked view — can list records, add new ones (new records only need the
/// cipher, which only `Unlocked` has), etc.
pub struct Vault {
    path: PathBuf,
    file: VaultFile,
    key: Option<Key>, // present iff unlocked
    unlock_expires_at: Option<u64>,
}

impl Vault {
    // ------- open / create -------

    pub fn config_path() -> Result<PathBuf> {
        #[cfg(test)]
        if let Some(path) = std::env::var_os("OPENTERM_TEST_VAULT_PATH") {
            let path = PathBuf::from(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).ok();
            }
            return Ok(path);
        }
        let dir = dirs::config_dir()
            .ok_or_else(|| anyhow!("no config dir"))?
            .join("openterm");
        fs::create_dir_all(&dir).ok();
        Ok(dir.join("vault.json"))
    }

    /// Load the on-disk vault, or signal that one doesn't exist yet.
    pub fn load() -> Result<Option<Self>> {
        let path = Self::config_path()?;
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let file: VaultFile = match serde_json::from_slice(&bytes) {
            Ok(file) => file,
            Err(main_error) => {
                let backup = backup_path(&path);
                let backup_bytes = fs::read(&backup).with_context(|| {
                    format!(
                        "vault.json is corrupt ({main_error}) and backup {} is unavailable",
                        backup.display()
                    )
                })?;
                let file = serde_json::from_slice(&backup_bytes)
                    .context("vault.json and its backup are corrupt")?;
                fs::copy(&backup, &path)
                    .with_context(|| format!("restore vault backup {}", backup.display()))?;
                file
            }
        };
        if file.v != VAULT_VERSION {
            bail!("unsupported vault version {}", file.v);
        }
        Ok(Some(Self {
            path,
            file,
            key: None,
            unlock_expires_at: None,
        }))
    }

    /// Create a fresh vault + persist it. For `Password` mode the derived key
    /// is kept in memory so the caller can start adding records immediately.
    pub fn create(mode: Mode, master: Option<&str>) -> Result<Self> {
        let path = Self::config_path()?;
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);

        let mut v = Self {
            path,
            file: VaultFile {
                v: VAULT_VERSION,
                mode,
                salt: B64.encode(salt),
                params: KdfParams::default(),
                records: Vec::new(),
                check: None,
            },
            key: None,
            unlock_expires_at: None,
        };
        match mode {
            Mode::Password => {
                let pw = master.ok_or_else(|| anyhow!("master password required"))?;
                v.key = Some(derive(pw.as_bytes(), &salt, &v.file.params)?);
            }
            Mode::Os => {
                let seed = ensure_os_seed()?;
                v.key = Some(derive(&seed, &salt, &v.file.params)?);
            }
        }
        v.file.check = Some(make_key_check(
            v.key.as_ref().expect("new vault has a key"),
        )?);
        v.save()?;
        Ok(v)
    }

    pub fn mode(&self) -> Mode {
        self.file.mode
    }
    pub fn is_unlocked(&self) -> bool {
        self.key.is_some()
    }
    pub fn records(&self) -> &[Record] {
        &self.file.records
    }

    pub fn unlock_remaining(&self) -> Option<std::time::Duration> {
        self.unlock_expires_at
            .map(|expires_at| std::time::Duration::from_secs(expires_at.saturating_sub(now())))
    }

    /// Unlock using the matching input for the vault's mode. New vaults carry
    /// a dedicated encrypted key check, so password verification does not
    /// depend on the presence or health of a saved machine record.
    pub fn unlock(&mut self, master: Option<&str>) -> Result<(), UnlockError> {
        if self.key.is_some() {
            return Ok(());
        }
        let salt = decode_salt(&self.file.salt).map_err(UnlockError::Other)?;
        let key = match self.file.mode {
            Mode::Password => {
                let pw = master.ok_or(UnlockError::NeedPassword)?;
                derive(pw.as_bytes(), &salt, &self.file.params).map_err(UnlockError::Other)?
            }
            Mode::Os => {
                let seed = match load_os_seed().map_err(UnlockError::Other)? {
                    Some(seed) => seed,
                    None if self.file.records.is_empty() => {
                        ensure_os_seed().map_err(UnlockError::Other)?
                    }
                    None => {
                        return Err(UnlockError::Other(anyhow!(
                            "the OS keystore key for this vault is missing"
                        )))
                    }
                };
                derive(&seed, &salt, &self.file.params).map_err(UnlockError::Other)?
            }
        };
        let mut repair_check = self.file.check.is_none();
        if let Some(check) = &self.file.check {
            if verify_key_check(&key, check).is_err() {
                // A damaged header verifier must not lock someone out when the
                // supplied key still authenticates an encrypted record. Record
                // decryption is an equally strong AEAD proof of the key, so use
                // it to repair only the verifier (never the encrypted secrets).
                let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
                let authenticates_record = self
                    .file
                    .records
                    .iter()
                    .any(|record| decrypt_record(&cipher, record).is_ok());
                if !authenticates_record {
                    return Err(UnlockError::BadPassword);
                }
                repair_check = true;
            }
        } else if let Some(r) = self.file.records.first() {
            // Legacy v1 vaults did not have a dedicated check. Verify one
            // record once, then add the check after a successful unlock.
            let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
            decrypt_record(&cipher, r).map_err(|_| UnlockError::BadPassword)?;
        }
        if repair_check {
            let previous_check = self.file.check.clone();
            let check = make_key_check(&key).map_err(UnlockError::Other)?;
            self.file.check = Some(check);
            if let Err(error) = self.save() {
                self.file.check = previous_check;
                return Err(UnlockError::Other(error));
            }
        }
        self.key = Some(key);
        self.unlock_expires_at = None;
        Ok(())
    }

    /// Keep a password-mode vault unlocked across restarts without persisting
    /// the password. The already-derived key is stored in the OS credential
    /// store with a hard expiry and is still validated against this vault's
    /// authenticated key check when restored.
    pub fn remember_for(&mut self, seconds: u64) -> Result<()> {
        if self.file.mode != Mode::Password {
            return Ok(());
        }
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| anyhow!("vault is locked"))?;
        let expires_at = now().saturating_add(seconds.max(1));
        let remembered = RememberedUnlock {
            v: VAULT_VERSION,
            salt: self.file.salt.clone(),
            expires_at,
            key: B64.encode(key.as_bytes()),
        };
        let mut encoded = serde_json::to_string(&remembered)?;
        let result = remembered_key_entry()?
            .set_password(&encoded)
            .context("store remembered vault key");
        encoded.zeroize();
        result?;
        self.unlock_expires_at = Some(expires_at);
        Ok(())
    }

    /// Restore a still-valid password-mode unlock from the OS credential
    /// store. Invalid, expired, or cross-vault entries are revoked.
    pub fn unlock_remembered(&mut self) -> Result<bool> {
        if self.file.mode != Mode::Password || self.key.is_some() {
            return Ok(self.key.is_some());
        }
        let entry = remembered_key_entry()?;
        let mut encoded = match entry.get_password() {
            Ok(value) => value,
            Err(keyring::Error::NoEntry) => return Ok(false),
            Err(error) => return Err(anyhow!("read remembered vault key: {error}")),
        };
        let parsed = serde_json::from_str(&encoded);
        encoded.zeroize();
        let remembered: RememberedUnlock = match parsed {
            Ok(value) => value,
            Err(_) => {
                let _ = entry.delete_credential();
                return Ok(false);
            }
        };
        if remembered.v != VAULT_VERSION
            || remembered.salt != self.file.salt
            || remembered.expires_at <= now()
        {
            let _ = entry.delete_credential();
            return Ok(false);
        }

        let mut decoded = match B64.decode(&remembered.key) {
            Ok(value) if value.len() == KEY_LEN => value,
            _ => {
                let _ = entry.delete_credential();
                return Ok(false);
            }
        };
        let mut bytes = [0u8; KEY_LEN];
        bytes.copy_from_slice(&decoded);
        decoded.zeroize();
        let key = Key::new(bytes)?;
        let valid = if let Some(check) = &self.file.check {
            verify_key_check(&key, check).is_ok()
        } else if let Some(record) = self.file.records.first() {
            let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
            decrypt_record(&cipher, record).is_ok()
        } else {
            false
        };
        if !valid {
            let _ = entry.delete_credential();
            return Ok(false);
        }
        self.key = Some(key);
        self.unlock_expires_at = Some(remembered.expires_at);
        Ok(true)
    }

    /// Returns true exactly when this call expired and locked the vault.
    pub fn expire_if_needed(&mut self) -> bool {
        if self
            .unlock_expires_at
            .is_some_and(|expires_at| expires_at <= now())
        {
            self.lock();
            let _ = forget_remembered_key();
            return true;
        }
        false
    }

    /// Forget the derived key (zeroized on drop of the `Key` newtype).
    pub fn lock(&mut self) {
        self.key = None;
        self.unlock_expires_at = None;
    }

    /// Explicit user lock also revokes the cross-restart grace token.
    pub fn lock_and_forget(&mut self) {
        self.lock();
        let _ = forget_remembered_key();
    }

    /// Permanently discard the encrypted vault. This is intentionally the only
    /// recovery available when a master password is lost: encrypted secrets
    /// cannot be recovered, but the user must not be locked out of OpenTerm.
    pub fn destroy(&mut self) -> Result<()> {
        self.lock_and_forget();
        for path in [self.path.clone(), backup_path(&self.path)] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
            }
        }
        Ok(())
    }

    /// Encrypt a secret and add it to the vault. Public metadata is bound into
    /// the AEAD so a tampered record won't decrypt.
    pub fn add(
        &mut self,
        label: &str,
        host: &str,
        port: u16,
        user: &str,
        secret: &Secret,
    ) -> Result<String> {
        self.write_record(label, host, port, user, secret, false)
    }

    /// Save a machine without creating duplicates when the same endpoint and
    /// account are connected again. This is what the connection dialog uses.
    pub fn upsert(
        &mut self,
        label: &str,
        host: &str,
        port: u16,
        user: &str,
        secret: &Secret,
    ) -> Result<String> {
        self.write_record(label, host, port, user, secret, true)
    }

    fn write_record(
        &mut self,
        label: &str,
        host: &str,
        port: u16,
        user: &str,
        secret: &Secret,
        replace_existing: bool,
    ) -> Result<String> {
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| anyhow!("vault is locked"))?;
        let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
        let existing = replace_existing.then(|| {
            self.file.records.iter().position(|record| {
                record.host == host && record.port == port && record.user == user
            })
        });
        let existing = existing.flatten();
        let id = existing
            .map(|index| self.file.records[index].id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let created = existing
            .map(|index| self.file.records[index].created)
            .unwrap_or_else(now);
        let aad = aad(&id, label, host, port, user);
        let pt = Zeroizing::new(serde_json::to_vec(secret)?);
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let ct = cipher
            .encrypt(
                XNonce::from_slice(&nonce_bytes),
                Payload {
                    msg: &pt,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("encrypt failed"))?;
        let rec = Record {
            id: id.clone(),
            label: label.into(),
            host: host.into(),
            port,
            user: user.into(),
            created,
            nonce: B64.encode(nonce_bytes),
            ct: B64.encode(ct),
        };
        let backup = self.file.records.clone();
        if let Some(index) = existing {
            self.file.records[index] = rec;
        } else {
            self.file.records.push(rec);
        }
        if let Err(error) = self.save() {
            self.file.records = backup;
            return Err(error);
        }
        Ok(id)
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        let before = self.file.records.len();
        self.file.records.retain(|r| r.id != id);
        if self.file.records.len() == before {
            bail!("no such record");
        }
        self.save()
    }

    pub fn decrypt(&self, id: &str) -> Result<Secret> {
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| anyhow!("vault is locked"))?;
        let r = self
            .file
            .records
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow!("no such record"))?;
        let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
        decrypt_record(&cipher, r)
    }

    fn save(&self) -> Result<()> {
        let data = serde_json::to_vec_pretty(&self.file)?;
        atomic_write(&self.path, &data)?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum UnlockError {
    NeedPassword, // password-mode vault, caller must prompt
    BadPassword,  // Poly1305 said no
    Other(anyhow::Error),
}
impl std::fmt::Display for UnlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::NeedPassword => write!(f, "master password required"),
            Self::BadPassword => write!(f, "wrong password"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

// ---------------- helpers ----------------

fn aad(id: &str, label: &str, host: &str, port: u16, user: &str) -> Vec<u8> {
    // Length-prefixed concat so (label="ab", host="cd") can't collide with
    // (label="a", host="bcd").
    let mut out = Vec::with_capacity(64 + id.len() + label.len() + host.len() + user.len());
    for s in [id, label, host, user] {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out.extend_from_slice(&port.to_le_bytes());
    out
}

fn decrypt_record(cipher: &XChaCha20Poly1305, r: &Record) -> Result<Secret> {
    let nonce = B64.decode(&r.nonce).context("bad nonce b64")?;
    if nonce.len() != NONCE_LEN {
        bail!("bad nonce length");
    }
    let ct = B64.decode(&r.ct).context("bad ct b64")?;
    let aad = aad(&r.id, &r.label, &r.host, r.port, &r.user);
    let pt = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("decrypt failed (bad key or tampered record)"))?,
    );
    Ok(serde_json::from_slice(&pt)?)
}

fn make_key_check(key: &Key) -> Result<KeyCheck> {
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: KEY_CHECK_PLAINTEXT,
                aad: KEY_CHECK_AAD,
            },
        )
        .map_err(|_| anyhow!("create vault key check failed"))?;
    Ok(KeyCheck {
        nonce: B64.encode(nonce),
        ct: B64.encode(ct),
    })
}

fn verify_key_check(key: &Key, check: &KeyCheck) -> Result<()> {
    let nonce = B64.decode(&check.nonce).context("bad key-check nonce")?;
    if nonce.len() != NONCE_LEN {
        bail!("bad key-check nonce length");
    }
    let ct = B64.decode(&check.ct).context("bad key-check ciphertext")?;
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ct,
                aad: KEY_CHECK_AAD,
            },
        )
        .map_err(|_| anyhow!("vault key check failed"))?;
    if plaintext != KEY_CHECK_PLAINTEXT {
        bail!("vault key check mismatch");
    }
    Ok(())
}

fn derive(secret: &[u8], salt: &[u8], p: &KdfParams) -> Result<Key> {
    let params = Params::new(p.m_cost, p.t_cost, p.p_cost, Some(KEY_LEN))
        .map_err(|e| anyhow!("argon2 params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(secret, salt, &mut out)
        .map_err(|e| anyhow!("argon2 kdf: {e}"))?;
    Key::new(out)
}

fn decode_salt(s: &str) -> Result<[u8; SALT_LEN]> {
    let v = B64.decode(s).context("bad salt b64")?;
    if v.len() != SALT_LEN {
        bail!("bad salt length");
    }
    let mut out = [0u8; SALT_LEN];
    out.copy_from_slice(&v);
    Ok(out)
}

fn os_seed_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_SEED_USER).context("open keyring entry")
}

fn remembered_key_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_REMEMBERED_USER)
        .context("open remembered-key entry")
}

fn forget_remembered_key() -> Result<()> {
    match remembered_key_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(anyhow!("delete remembered vault key: {error}")),
    }
}

/// Fetch the existing OS-protected seed without silently replacing a missing
/// key for a vault that already contains encrypted records.
fn load_os_seed() -> Result<Option<Vec<u8>>> {
    let entry = os_seed_entry()?;
    match entry.get_password() {
        Ok(b64) => {
            let seed = B64.decode(b64.trim()).context("bad seed in keyring")?;
            if seed.len() != KEY_LEN {
                bail!("unexpected seed length in keyring");
            }
            Ok(Some(seed))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("keyring: {e}")),
    }
}

/// Fetch the 32-byte OS-protected seed, creating it on first use.
fn ensure_os_seed() -> Result<Vec<u8>> {
    if let Some(seed) = load_os_seed()? {
        return Ok(seed);
    }
    let entry = os_seed_entry()?;
    let mut seed = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut seed);
    entry
        .set_password(&B64.encode(seed))
        .context("write to keyring")?;
    let out = seed.to_vec();
    seed.zeroize();
    Ok(out)
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    if path.is_file() {
        let backup = backup_path(path);
        fs::copy(path, &backup)
            .with_context(|| format!("back up vault to {}", backup.display()))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, MutexGuard};

    // These tests redirect process-wide config environment variables. Serialize
    // them so parallel test execution cannot make one test use another's vault.
    static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn tmp_env() -> (MutexGuard<'static, ()>, std::path::PathBuf) {
        let guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = env::temp_dir().join(format!("ot-vault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Use an explicit test-only override. XDG_CONFIG_HOME/HOME are ignored
        // by dirs::config_dir on Windows, which previously let these tests
        // overwrite the user's real %APPDATA%\openterm\vault.json.
        env::set_var("OPENTERM_TEST_VAULT_PATH", dir.join("vault.json"));
        (guard, dir)
    }

    #[test]
    fn password_mode_round_trip() {
        let (_guard, _dir) = tmp_env();
        let mut v = Vault::create(Mode::Password, Some("correct horse battery staple")).unwrap();
        let id = v
            .add(
                "home",
                "192.168.1.5",
                22,
                "susmic",
                &Secret::Password {
                    password: "p4ssw0rd!".into(),
                },
            )
            .unwrap();
        let id2 = v
            .add(
                "vps",
                "example.com",
                2222,
                "root",
                &Secret::Key {
                    key_path: "/home/s/.ssh/id_ed25519".into(),
                    passphrase: Some("hunter2".into()),
                },
            )
            .unwrap();

        // lock + reload (fresh handle)
        v.lock();
        let path = Vault::config_path().unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("p4ssw0rd"),
            "plaintext leaked into vault file!"
        );
        assert!(!raw.contains("hunter2"), "key passphrase leaked!");

        let mut v2 = Vault::load().unwrap().unwrap();
        assert!(matches!(
            v2.unlock(Some("wrong")),
            Err(UnlockError::BadPassword)
        ));
        v2.unlock(Some("correct horse battery staple")).unwrap();
        let s1 = v2.decrypt(&id).unwrap();
        match s1.to_auth() {
            PlainAuth::Password(p) => assert_eq!(p.as_str(), "p4ssw0rd!"),
            _ => panic!(),
        }
        let s2 = v2.decrypt(&id2).unwrap();
        match s2.to_auth() {
            PlainAuth::Key { path, passphrase } => {
                assert_eq!(path, "/home/s/.ssh/id_ed25519");
                assert_eq!(
                    passphrase.as_ref().map(|value| value.as_str()),
                    Some("hunter2")
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn tampering_record_fields_fails_decrypt() {
        let (_guard, _dir) = tmp_env();
        let mut v = Vault::create(Mode::Password, Some("masterpass12345")).unwrap();
        let id = v
            .add(
                "home",
                "192.168.1.5",
                22,
                "susmic",
                &Secret::Password {
                    password: "secret".into(),
                },
            )
            .unwrap();

        // mutate the metadata (host) in-memory and attempt decrypt
        if let Some(r) = v.file.records.iter_mut().find(|r| r.id == id) {
            r.host = "attacker.com".into();
        }
        let r = v.decrypt(&id);
        assert!(r.is_err(), "metadata tamper must break Poly1305");
    }

    #[test]
    fn empty_vault_rejects_wrong_password() {
        let (_guard, _dir) = tmp_env();
        let v = Vault::create(Mode::Password, Some("rightpass")).unwrap();
        drop(v);
        let mut v2 = Vault::load().unwrap().unwrap();
        assert!(matches!(
            v2.unlock(Some("wrongpass")),
            Err(UnlockError::BadPassword)
        ));
        v2.unlock(Some("rightpass")).unwrap();
    }

    #[test]
    fn valid_password_repairs_a_damaged_header_check() {
        let (_guard, _dir) = tmp_env();
        let mut vault = Vault::create(Mode::Password, Some("rightpass")).unwrap();
        let id = vault
            .add(
                "home",
                "server.example",
                22,
                "alice",
                &Secret::Password {
                    password: "ssh-secret".into(),
                },
            )
            .unwrap();
        vault.file.check.as_mut().unwrap().ct = B64.encode([0u8; 48]);
        vault.save().unwrap();
        drop(vault);

        let mut reloaded = Vault::load().unwrap().unwrap();
        assert!(matches!(
            reloaded.unlock(Some("wrongpass")),
            Err(UnlockError::BadPassword)
        ));
        reloaded.unlock(Some("rightpass")).unwrap();
        match reloaded.decrypt(&id).unwrap().to_auth() {
            PlainAuth::Password(password) => assert_eq!(password.as_str(), "ssh-secret"),
            _ => panic!(),
        }

        drop(reloaded);
        let mut repaired = Vault::load().unwrap().unwrap();
        repaired.unlock(Some("rightpass")).unwrap();
    }

    #[test]
    fn corrupt_primary_vault_restores_authenticated_backup() {
        let (_guard, _dir) = tmp_env();
        let mut vault = Vault::create(Mode::Password, Some("rightpass")).unwrap();
        let id = vault
            .add(
                "home",
                "server.example",
                22,
                "alice",
                &Secret::Password {
                    password: "ssh-secret".into(),
                },
            )
            .unwrap();
        vault.save().unwrap();
        let path = Vault::config_path().unwrap();
        assert!(backup_path(&path).is_file());
        std::fs::write(&path, b"not json").unwrap();
        drop(vault);

        let mut restored = Vault::load().unwrap().unwrap();
        restored.unlock(Some("rightpass")).unwrap();
        match restored.decrypt(&id).unwrap().to_auth() {
            PlainAuth::Password(password) => assert_eq!(password.as_str(), "ssh-secret"),
            _ => panic!(),
        }
        let reparsed: VaultFile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(reparsed.records.len(), 1);
    }

    #[test]
    fn upsert_keeps_one_machine_and_survives_reload() {
        let (_guard, _dir) = tmp_env();
        let mut vault =
            Vault::create(Mode::Password, Some("correct horse battery staple")).unwrap();
        let first_id = vault
            .upsert(
                "old label",
                "server.example",
                22,
                "alice",
                &Secret::Password {
                    password: "first".into(),
                },
            )
            .unwrap();
        let second_id = vault
            .upsert(
                "new label",
                "server.example",
                22,
                "alice",
                &Secret::Password {
                    password: "second".into(),
                },
            )
            .unwrap();
        assert_eq!(first_id, second_id);
        assert_eq!(vault.records().len(), 1);
        drop(vault);

        let mut reloaded = Vault::load().unwrap().unwrap();
        reloaded
            .unlock(Some("correct horse battery staple"))
            .unwrap();
        assert_eq!(reloaded.records().len(), 1);
        assert_eq!(reloaded.records()[0].label, "new label");
        match reloaded.decrypt(&first_id).unwrap().to_auth() {
            PlainAuth::Password(password) => assert_eq!(password.as_str(), "second"),
            _ => panic!(),
        }
    }
}
