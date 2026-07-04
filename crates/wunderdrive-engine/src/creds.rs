//! Credential storage in the OS keychain.
//!
//! Secrets (access key id, secret access key) are stored via [`keyring`] so
//! they never touch a plaintext dotfile. The non-secret endpoint/region/bucket
//! live in [`crate::config::Config`] on disk.

use keyring::{Entry, Error as KrError};

use crate::error::Result;

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

/// Resolve credentials in priority order: explicit CLI args → keychain → env.
///
/// `cli_*` are the optional `--access-key-id` / `--secret-access-key` flags
/// (highest priority, useful on headless boxes / containers). A missing
/// keychain *backend* is silently skipped (not propagated) so headless use can
/// fall through to env or CLI flags; a real keychain lookup that simply has no
/// entry also falls through.
pub fn resolve(
    cli_id: Option<String>,
    cli_secret: Option<String>,
    bucket: &str,
    endpoint: Option<&str>,
) -> Result<Option<Credentials>> {
    if let (Some(id), Some(secret)) = (cli_id, cli_secret) {
        return Ok(Some(Credentials {
            access_key_id: id,
            secret_access_key: secret,
        }));
    }

    match load(bucket, endpoint) {
        Ok(Some(c)) => return Ok(Some(c)),
        Ok(None) => {}
        Err(e) => tracing::debug!(error = %e, "keyring unavailable; trying env/flags"),
    }

    let id = std::env::var("WUNDERDRIVE_ACCESS_KEY_ID")
        .or_else(|_| std::env::var("AWS_ACCESS_KEY_ID"))
        .ok();
    let secret = std::env::var("WUNDERDRIVE_SECRET_ACCESS_KEY")
        .or_else(|_| std::env::var("AWS_SECRET_ACCESS_KEY"))
        .ok();
    match (id, secret) {
        (Some(id), Some(secret)) => Ok(Some(Credentials {
            access_key_id: id,
            secret_access_key: secret,
        })),
        _ => Ok(None),
    }
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
