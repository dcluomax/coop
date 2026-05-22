//! # coopd-vault
//!
//! Sealed credential storage for Coop.
//!
//! Credentials (API keys, signing keys) are sealed at rest using XChaCha20-Poly1305
//! with a key derived from the farmer's passphrase via Argon2id.
//!
//! v0.1 implements the minimum needed for BYOK Anthropic keys. TPM /
//! hardware-key integration is deferred.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroize;

/// Errors emitted by the vault.
#[derive(Debug, Error)]
pub enum VaultError {
    /// I/O failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Vault file format invalid.
    #[error("vault format: {0}")]
    Format(String),
    /// Decryption failed (wrong passphrase or tampered file).
    #[error("decryption failed (wrong passphrase or corrupted vault)")]
    Decrypt,
    /// JSON serialization failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Argon2 key derivation failed.
    #[error("kdf: {0}")]
    Kdf(String),
    /// Requested key not present.
    #[error("key not found: {0}")]
    NotFound(String),
}

type Result<T> = std::result::Result<T, VaultError>;

/// Sealed vault stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedVault {
    /// Format version.
    version: u32,
    /// Argon2 salt (32 bytes hex).
    salt: String,
    /// Cipher nonce (24 bytes hex).
    nonce: String,
    /// Encrypted ciphertext (hex).
    ciphertext: String,
}

/// The decrypted in-memory contents of a vault.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct VaultContents {
    /// Named secrets (e.g. `byok-anthropic-01` → API key).
    pub secrets: std::collections::HashMap<String, String>,
}

/// A vault handle. Open via [`Vault::open`] or create via [`Vault::create`].
#[derive(Debug)]
pub struct Vault {
    path: PathBuf,
    key: [u8; 32],
    salt: [u8; 32],
    contents: VaultContents,
}

impl Drop for Vault {
    fn drop(&mut self) {
        self.key.zeroize();
        self.salt.zeroize();
    }
}

const VAULT_FORMAT_VERSION: u32 = 1;
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_MEM_KIB: u32 = 64 * 1024; // 64 MiB
const ARGON2_PARALLELISM: u32 = 1;

impl Vault {
    /// Create a fresh vault at `path`, sealed by `passphrase`.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::Format`] if a file already exists at `path`,
    /// or propagates I/O / Argon2 KDF errors when generating the salt,
    /// deriving the key, or persisting the sealed file.
    pub fn create(path: impl AsRef<Path>, passphrase: &str) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(VaultError::Format(format!(
                "vault already exists: {}",
                path.display()
            )));
        }
        let mut salt = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut salt);
        let key = derive_key(passphrase, &salt)?;
        let v = Self {
            path,
            key,
            salt,
            contents: VaultContents::default(),
        };
        v.persist()?;
        Ok(v)
    }

    /// Open an existing vault at `path`, unsealing with `passphrase`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, if its JSON envelope is
    /// malformed, if `version` is unsupported, if the salt/nonce/ciphertext
    /// hex fields are invalid, or if AEAD decryption fails (wrong passphrase
    /// or tampered ciphertext → [`VaultError::Decrypt`]).
    pub fn open(path: impl AsRef<Path>, passphrase: &str) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let bytes = std::fs::read(&path)?;
        let sealed: SealedVault = serde_json::from_slice(&bytes)?;
        if sealed.version != VAULT_FORMAT_VERSION {
            return Err(VaultError::Format(format!(
                "unsupported vault version: {}",
                sealed.version
            )));
        }
        let salt_vec =
            hex::decode(&sealed.salt).map_err(|e| VaultError::Format(format!("salt hex: {e}")))?;
        if salt_vec.len() != 32 {
            return Err(VaultError::Format(format!(
                "salt length: expected 32, got {}",
                salt_vec.len()
            )));
        }
        let mut salt = [0u8; 32];
        salt.copy_from_slice(&salt_vec);
        let nonce = hex::decode(&sealed.nonce)
            .map_err(|e| VaultError::Format(format!("nonce hex: {e}")))?;
        let ciphertext = hex::decode(&sealed.ciphertext)
            .map_err(|e| VaultError::Format(format!("ciphertext hex: {e}")))?;

        let key = derive_key(passphrase, &salt)?;
        let cipher = XChaCha20Poly1305::new((&key).into());
        let plaintext = cipher
            .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| VaultError::Decrypt)?;
        let contents: VaultContents = serde_json::from_slice(&plaintext)?;
        Ok(Self {
            path,
            key,
            salt,
            contents,
        })
    }

    /// Store a secret under `name`.
    ///
    /// # Errors
    ///
    /// Returns an error if re-sealing and persisting the vault fails
    /// (I/O error writing the temp file or atomic rename).
    pub fn put(&mut self, name: impl Into<String>, value: impl Into<String>) -> Result<()> {
        self.contents.secrets.insert(name.into(), value.into());
        self.persist()
    }

    /// Retrieve a secret by name.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::NotFound`] if no secret is stored under `name`.
    pub fn get(&self, name: &str) -> Result<&str> {
        self.contents
            .secrets
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| VaultError::NotFound(name.to_string()))
    }

    /// List secret names (values are not returned).
    pub fn list(&self) -> Vec<&str> {
        self.contents.secrets.keys().map(String::as_str).collect()
    }

    /// Remove a secret.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::NotFound`] if no secret is stored under `name`,
    /// or propagates I/O errors when persisting the updated vault.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        if self.contents.secrets.remove(name).is_some() {
            self.persist()
        } else {
            Err(VaultError::NotFound(name.to_string()))
        }
    }

    fn persist(&self) -> Result<()> {
        let mut nonce_bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let plaintext = serde_json::to_vec(&self.contents)?;
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce_bytes), plaintext.as_ref())
            .map_err(|_| VaultError::Decrypt)?;
        let sealed = SealedVault {
            version: VAULT_FORMAT_VERSION,
            salt: hex::encode(self.salt),
            nonce: hex::encode(nonce_bytes),
            ciphertext: hex::encode(&ciphertext),
        };
        let serialized = serde_json::to_vec_pretty(&sealed)?;
        // Atomic write: write to tmp then rename.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serialized)?;
        std::fs::rename(&tmp, &self.path)?;
        // H1: confine to owner only. Files holding sealed keys + a unique
        // salt should never be world-readable, even sealed.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(
        ARGON2_MEM_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(32),
    )
    .map_err(|e| VaultError::Kdf(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_open_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vault.json");
        {
            let mut v = Vault::create(&path, "test-passphrase").unwrap();
            v.put("byok-anthropic-01", "sk-ant-test-123").unwrap();
            v.put("other", "value").unwrap();
        }
        let v = Vault::open(&path, "test-passphrase").unwrap();
        assert_eq!(v.get("byok-anthropic-01").unwrap(), "sk-ant-test-123");
        assert_eq!(v.list().len(), 2);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vault.json");
        {
            let mut v = Vault::create(&path, "right").unwrap();
            v.put("k", "v").unwrap();
        }
        assert!(matches!(
            Vault::open(&path, "wrong"),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn remove_works() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut v = Vault::create(&path, "p").unwrap();
        v.put("a", "1").unwrap();
        v.remove("a").unwrap();
        assert!(v.get("a").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn vault_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let _ = Vault::create(&path, "p").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    #[test]
    fn persist_survives_file_deletion() {
        // H2 regression: salt is held in-memory; deleting & re-persisting
        // must NOT silently rotate to an unrecoverable key.
        let dir = tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut v = Vault::create(&path, "p").unwrap();
        v.put("k1", "v1").unwrap();
        std::fs::remove_file(&path).unwrap();
        v.put("k2", "v2").unwrap();
        let v2 = Vault::open(&path, "p").unwrap();
        assert_eq!(v2.get("k1").unwrap(), "v1");
        assert_eq!(v2.get("k2").unwrap(), "v2");
    }
}
