use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};

const KEYCHAIN_SERVICE: &str = "com.datasync.studio";
const KEYCHAIN_ACCOUNT: &str = "master-key";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub dialect: Dialect,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub database: Option<String>,
    /// one of: disable | prefer | require
    #[serde(default = "default_ssl_mode")]
    pub ssl_mode: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub remark: Option<String>,
    #[serde(default)]
    pub save_password: bool,
    /// Never written to `connections.json`; kept in the encrypted secret store.
    #[serde(default)]
    pub password: Option<String>,
    /// Derived flag so the UI can show "已保存密码" without the value.
    #[serde(default)]
    pub has_password: bool,
}

fn default_ssl_mode() -> String {
    "prefer".to_string()
}

impl ConnectionConfig {
    pub fn trimmed_database(&self) -> Option<&str> {
        self.database
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    connections: Vec<ConnectionConfig>,
    #[serde(default)]
    secrets: HashMap<String, String>,
}

pub struct Store {
    dir: PathBuf,
    key: [u8; 32],
    dir_is_fallback: bool,
    key_is_ephemeral: bool,
    data: Mutex<Persisted>,
}

impl Store {
    /// Never fails. When the preferred application data directory is not
    /// writable the store falls back to a temporary directory, and when no
    /// master key can be persisted passwords simply do not survive a restart.
    /// Failing to start would be a far worse outcome than a degraded store.
    pub fn load(preferred: &Path) -> Store {
        let (dir, dir_is_fallback) = ensure_dir(preferred);
        // `master_key` reports whether the key could be *persisted*.
        let (key, key_persisted) = master_key(&dir);
        let key_is_ephemeral = !key_persisted;
        let path = dir.join("connections.json");
        let data = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Persisted>(&raw).ok())
                .unwrap_or_default()
        } else {
            Persisted::default()
        };
        Store {
            dir,
            key,
            dir_is_fallback,
            key_is_ephemeral,
            data: Mutex::new(data),
        }
    }

    /// Human readable description of any degraded persistence, for the UI.
    pub fn storage_warning(&self) -> Option<String> {
        if self.dir_is_fallback && self.key_is_ephemeral {
            Some(format!(
                "配置目录不可写，已临时改用 {}；主密钥无法持久化，保存的密码在重启后失效。",
                self.dir.display()
            ))
        } else if self.dir_is_fallback {
            Some(format!(
                "配置目录不可写，已临时改用 {}；连接配置仅在本次会话内有效。",
                self.dir.display()
            ))
        } else if self.key_is_ephemeral {
            Some("无法持久化主密钥（钥匙串与密钥文件均不可用），保存的密码在重启后失效。".to_string())
        } else {
            None
        }
    }

    fn persist(&self, data: &Persisted) -> AppResult<()> {
        let path = self.dir.join("connections.json");
        let tmp = self.dir.join("connections.json.tmp");
        let raw = serde_json::to_string_pretty(data)?;
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn encrypt(&self, plain: &str) -> AppResult<String> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let mut nonce_bytes = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plain.as_bytes())
            .map_err(|_| AppError::msg("加密密码失败"))?;
        let mut blob = nonce_bytes.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(B64.encode(blob))
    }

    fn decrypt(&self, encoded: &str) -> AppResult<String> {
        let blob = B64
            .decode(encoded)
            .map_err(|_| AppError::msg("密码记录已损坏"))?;
        if blob.len() < 13 {
            return Err(AppError::msg("密码记录已损坏"));
        }
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let plain = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| AppError::msg("解密密码失败（主密钥可能已变更）"))?;
        String::from_utf8(plain).map_err(|_| AppError::msg("密码记录编码无效"))
    }

    pub fn list(&self) -> Vec<ConnectionConfig> {
        let guard = self.data.lock().expect("store poisoned");
        guard
            .connections
            .iter()
            .map(|c| self.decorate(c, &guard))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<ConnectionConfig> {
        let guard = self.data.lock().expect("store poisoned");
        guard
            .connections
            .iter()
            .find(|c| c.id == id)
            .map(|c| self.decorate(c, &guard))
    }

    fn decorate(&self, config: &ConnectionConfig, data: &Persisted) -> ConnectionConfig {
        let mut out = config.clone();
        let stored = data.secrets.get(&config.id);
        out.has_password = stored.is_some();
        out.password = stored.and_then(|s| self.decrypt(s).ok());
        out
    }

    /// Password to use for a connection: the explicit one if provided, else the
    /// stored secret.
    pub fn resolve_password(&self, config: &ConnectionConfig) -> Option<String> {
        if let Some(pw) = config.password.as_ref() {
            if !pw.is_empty() {
                return Some(pw.clone());
            }
        }
        let guard = self.data.lock().expect("store poisoned");
        guard.secrets.get(&config.id).and_then(|s| self.decrypt(s).ok())
    }

    pub fn upsert(&self, mut config: ConnectionConfig) -> AppResult<ConnectionConfig> {
        if config.name.trim().is_empty() {
            return Err(AppError::msg("连接名称不能为空"));
        }
        if config.host.trim().is_empty() {
            return Err(AppError::msg("主机地址不能为空"));
        }
        if config.id.trim().is_empty() {
            config.id = uuid::Uuid::new_v4().to_string();
        }
        if config.port == 0 {
            config.port = config.dialect.default_port();
        }
        let incoming_password = config.password.clone();
        let save_password = config.save_password;

        let mut guard = self.data.lock().expect("store poisoned");
        if save_password {
            match incoming_password.clone() {
                Some(pw) if !pw.is_empty() => {
                    let sealed = self.encrypt(&pw)?;
                    guard.secrets.insert(config.id.clone(), sealed);
                }
                _ => {}
            }
        } else {
            guard.secrets.remove(&config.id);
        }

        let stored = ConnectionConfig {
            password: None,
            has_password: guard.secrets.contains_key(&config.id),
            ..config.clone()
        };
        match guard.connections.iter_mut().find(|c| c.id == stored.id) {
            Some(existing) => *existing = stored.clone(),
            None => guard.connections.push(stored.clone()),
        }
        self.persist(&guard)?;
        Ok(ConnectionConfig {
            password: incoming_password,
            ..stored
        })
    }

    pub fn remove(&self, id: &str) -> AppResult<()> {
        let mut guard = self.data.lock().expect("store poisoned");
        guard.connections.retain(|c| c.id != id);
        guard.secrets.remove(id);
        self.persist(&guard)
    }

    pub fn reorder(&self, ids: &[String]) -> AppResult<()> {
        let mut guard = self.data.lock().expect("store poisoned");
        let mut reordered: Vec<ConnectionConfig> = Vec::with_capacity(guard.connections.len());
        for id in ids {
            if let Some(found) = guard.connections.iter().find(|c| &c.id == id) {
                reordered.push(found.clone());
            }
        }
        for existing in guard.connections.iter() {
            if !ids.contains(&existing.id) {
                reordered.push(existing.clone());
            }
        }
        guard.connections = reordered;
        self.persist(&guard)
    }

    pub fn storage_dir(&self) -> &Path {
        &self.dir
    }
}

// ---------------------------------------------------------------------------
// Master key handling
// ---------------------------------------------------------------------------

/// Resolve a writable data directory, degrading to the temp directory rather
/// than refusing to start.
fn ensure_dir(preferred: &Path) -> (PathBuf, bool) {
    if std::fs::create_dir_all(preferred).is_ok() {
        return (preferred.to_path_buf(), false);
    }
    let fallback = std::env::temp_dir().join("datasync-studio");
    let _ = std::fs::create_dir_all(&fallback);
    (fallback, true)
}

/// Read the AES master key from the login keychain, generating and storing one
/// on first use. Falls back to a 0600 dotfile, then to an ephemeral in-memory
/// key, when the keychain is unavailable (unsigned dev builds, sandboxes, ...).
///
/// Returns the key and whether it could be persisted.
fn master_key(dir: &Path) -> ([u8; 32], bool) {
    let fallback = dir.join(".masterkey");

    // 1. Login keychain.
    if let Some(existing) = keychain_get() {
        if let Some(key) = parse_key(&existing) {
            return (key, true);
        }
    }

    // 2. Key file written on a previous launch. Reading it before generating a
    //    new key is essential: otherwise every start would rotate the key and
    //    silently invalidate all stored passwords.
    if let Ok(existing) = std::fs::read_to_string(&fallback) {
        if let Some(key) = parse_key(&existing) {
            return (key, true);
        }
    }

    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let encoded = hex::encode(key);

    // 3. Persist via the keychain when possible.
    if keychain_set(&encoded) {
        return (key, true);
    }

    // 4. Persist next to the configuration.
    if std::fs::write(&fallback, &encoded).is_ok() {
        restrict_permissions(&fallback);
        return (key, true);
    }

    // 5. Last resort: a session-only key.
    (key, false)
}

fn parse_key(raw: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(raw.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(key)
}

/// Keychain calls go through a worker thread so a blocked SecurityAgent prompt
/// can never freeze the whole application.
fn with_timeout<T, F>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(Duration::from_secs(6)).ok().flatten()
}

fn keychain_get() -> Option<String> {
    with_timeout(|| {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .ok()?
            .get_password()
            .ok()
    })
}

fn keychain_set(value: &str) -> bool {
    let owned = value.to_string();
    with_timeout(move || {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).ok()?;
        entry.set_password(&owned).ok().map(|_| true)
    })
    .unwrap_or(false)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}
