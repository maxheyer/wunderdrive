//! Credential storage and resolution.
//!
//! Secrets (access key id, secret access key) are stored via [`keyring`] so
//! they never touch a plaintext dotfile. The non-secret endpoint/region/bucket
//! live in [`crate::config::Config`] on disk.
//
//! **Headless exception:** On NixOS build sandboxes, CI runners, containers,
//! and headless servers, no secret-service daemon is running, so the keyring
//! backend is unavailable. CLI flags and env vars work but are operationally
//! fragile (secrets visible in `ps` / `/proc/PID/environ`). As a last-resort
//! fallback, we support a `~/.config/wunderdrive/credentials.toml` file (mode
//! 0600, user-owned). This is a deliberate, documented exception to the spec's
//! "never a dotfile" rule (spec §5) — it exists solely so headless deployments
//! have a persistent, file-permission-protected credential store.

use std::path::Path;

use keyring::{Entry, Error as KrError};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::error::{Error, Result};

/// The service name used in the OS keychain.
pub const SERVICE: &str = "wunderdrive";

/// A pair of S3 long-term credentials.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// Build the keychain account name for a given bucket@endpoint.
///
/// Including the endpoint+bucket means multiple buckets are stored under
/// distinct keychain entries rather than clobbering each other.
pub fn account_name(bucket: &str, endpoint: Option<&str>) -> String {
    match endpoint {
        Some(e) => format!("{bucket}@{e}"),
        None => bucket.to_string(),
    }
}

/// Load credentials for `bucket` / `endpoint` from the keychain.
///
/// Returns `Ok(None)` when no entry exists (first run / not yet configured).
pub fn load(bucket: &str, endpoint: Option<&str>) -> Result<Option<Credentials>> {
    let name = account_name(bucket, endpoint);
    let id = Entry::new(SERVICE, &format!("{name}/access_key_id"))?;
    let secret = Entry::new(SERVICE, &format!("{name}/secret_access_key"))?;
    let access_key_id = match id.get_password() {
        Ok(s) => s,
        Err(KrError::NoEntry) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let secret_access_key = match secret.get_password() {
        Ok(s) => s,
        Err(KrError::NoEntry) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(Credentials {
        access_key_id,
        secret_access_key,
    }))
}

/// Resolve credentials in priority order:
/// CLI args → keychain → env vars → `credentials.toml` file.
///
/// `cli_*` are the optional `--access-key-id` / `--secret-access-key` flags
/// (highest priority, useful on headless boxes / containers). A missing
/// keychain *backend* (e.g. no secret-service daemon on NixOS/headless) is
/// logged at `warn` and skipped so headless use can fall through to env vars
/// or the credentials file. A real keychain lookup that simply has no entry
/// also falls through silently.
pub fn resolve(
    cli_id: Option<String>,
    cli_secret: Option<String>,
    bucket: &str,
    endpoint: Option<&str>,
) -> Result<Option<Credentials>> {
    // Tier 1: explicit CLI flags (highest priority).
    if let (Some(id), Some(secret)) = (cli_id, cli_secret) {
        return Ok(Some(Credentials {
            access_key_id: id,
            secret_access_key: secret,
        }));
    }

    // Tier 2: OS keychain.
    match load(bucket, endpoint) {
        Ok(Some(c)) => return Ok(Some(c)),
        Ok(None) => {}
        Err(e) => tracing::warn!(
            error = %e,
            "keyring unavailable (no secret-service daemon?); \
             falling back to env vars / credentials file"
        ),
    }

    // Tier 3: environment variables.
    let id = std::env::var("WUNDERDRIVE_ACCESS_KEY_ID")
        .or_else(|_| std::env::var("AWS_ACCESS_KEY_ID"))
        .ok();
    let secret = std::env::var("WUNDERDRIVE_SECRET_ACCESS_KEY")
        .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
        .ok();
    if let (Some(id), Some(secret)) = (id, secret) {
        return Ok(Some(Credentials {
            access_key_id: id,
            secret_access_key: secret,
        }));
    }

    // Tier 4: credentials.toml file (lowest priority, headless fallback).
    match load_from_file(&config::default_credentials_path()) {
        Ok(Some(c)) => Ok(Some(c)),
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, "could not read credentials file");
            Ok(None)
        }
    }
}

/// On-disk format for the credentials.toml fallback file.
///
/// Only used when the keychain is unavailable and no CLI/env creds were
/// provided. The file must be mode 0600.
#[derive(Debug, Serialize, Deserialize)]
struct CredsFile {
    access_key_id: String,
    secret_access_key: String,
}

/// Load credentials from a TOML file (the headless fallback path).
///
/// Returns `Ok(None)` if the file does not exist. Refuses to read the file if
/// it is group- or world-readable (mode bits permit access beyond the owner),
/// since that would defeat the purpose of storing secrets on disk.
pub fn load_from_file(path: &Path) -> Result<Option<Credentials>> {
    if !path.exists() {
        return Ok(None);
    }
    check_file_permissions(path)?;
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
    let creds: CredsFile = toml::from_str(&raw)
        .map_err(|e| Error::Config(format!("parsing {}: {e}", path.display())))?;
    Ok(Some(Credentials {
        access_key_id: creds.access_key_id,
        secret_access_key: creds.secret_access_key,
    }))
}

/// Write credentials to a TOML file with mode 0600.
///
/// Creates the file with restrictive permissions before writing. On Unix,
/// sets the mode to 0600 (owner read/write only).
pub fn store_to_file(path: &Path, creds: &Credentials) -> Result<()> {
    let data = CredsFile {
        access_key_id: creds.access_key_id.clone(),
        secret_access_key: creds.secret_access_key.clone(),
    };
    let raw = toml::to_string_pretty(&data)
        .map_err(|e| Error::Config(format!("encoding credentials: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_file_mode_0600(path, &raw)?;
    tracing::info!(path = %path.display(), "credentials written to file");
    Ok(())
}

/// Write `content` to `path` with mode 0600 on Unix.
#[cfg(unix)]
fn write_file_mode_0600(path: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_file_mode_0600(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content)?;
    Ok(())
}

/// Verify that `path` is not group- or world-readable on Unix.
#[cfg(unix)]
fn check_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::metadata(path)?.permissions().mode();
    if perms & 0o077 != 0 {
        return Err(Error::Config(format!(
            "credentials file {} is too permissive (mode {:04o}); \
             expected 0600 or stricter. Run: chmod 600 {}",
            path.display(),
            perms,
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Store credentials for `bucket` / `endpoint` in the keychain.
pub fn store(bucket: &str, endpoint: Option<&str>, creds: &Credentials) -> Result<()> {
    let name = account_name(bucket, endpoint);
    let id = Entry::new(SERVICE, &format!("{name}/access_key_id"))?;
    let secret = Entry::new(SERVICE, &format!("{name}/secret_access_key"))?;
    id.set_password(&creds.access_key_id)?;
    secret.set_password(&creds.secret_access_key)?;
    Ok(())
}

/// Delete stored credentials (used by reset flows).
pub fn delete(bucket: &str, endpoint: Option<&str>) -> Result<()> {
    let name = account_name(bucket, endpoint);
    if let Ok(id) = Entry::new(SERVICE, &format!("{name}/access_key_id")) {
        let _ = id.delete_credential();
    }
    if let Ok(secret) = Entry::new(SERVICE, &format!("{name}/secret_access_key")) {
        let _ = secret.delete_credential();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        assert!(load_from_file(&path).unwrap().is_none());

        let creds = Credentials {
            access_key_id: "AKIATEST".into(),
            secret_access_key: "secret123".into(),
        };
        store_to_file(&path, &creds).unwrap();

        let loaded = load_from_file(&path).unwrap().expect("should load");
        assert_eq!(loaded.access_key_id, "AKIATEST");
        assert_eq!(loaded.secret_access_key, "secret123");
    }

    #[cfg(unix)]
    #[test]
    fn credentials_file_rejects_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let creds = Credentials {
            access_key_id: "AKIATEST".into(),
            secret_access_key: "secret123".into(),
        };
        store_to_file(&path, &creds).unwrap();
        // Make it world-readable — should be rejected on load.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = load_from_file(&path).unwrap_err();
        assert!(
            matches!(err, Error::Config(ref m) if m.contains("too permissive")),
            "expected permission error, got: {err}"
        );
    }
}
