//! User configuration.
//!
//! Non-secret connection details live in a TOML file on disk
//! (`~/.config/wunderdrive/config.toml` by default). Secrets (access key id
//! and secret access key) never touch disk — see [`crate::creds`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// On-disk configuration. Everything here is non-secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// S3 endpoint, e.g. `https://s3.amazonaws.com` or `https://s3.example.com`.
    /// For AWS itself this may be omitted and derived from `region`.
    pub endpoint: Option<String>,
    /// AWS region, e.g. `us-east-1`. Required even for non-AWS providers
    /// (SigV4 needs a region; most S3-compatible stores accept any value).
    pub region: String,
    /// Bucket name.
    pub bucket: String,
    /// Optional key prefix inside the bucket (treated as a virtual folder).
    #[serde(default)]
    pub prefix: String,
    /// Local mirror root. `~` is expanded.
    pub local_root: PathBuf,
    /// Remote poll interval in seconds. Default 45 (spec: 30–60s).
    #[serde(default = "default_remote_poll")]
    pub remote_poll_secs: u64,
    /// Full local rescan interval in seconds. Default 300.
    #[serde(default = "default_local_rescan")]
    pub local_rescan_secs: u64,
    /// Lazy download (spec §10 phase 4, pulled forward): remote objects appear
    /// as browseable stubs; bytes fetch only on explicit materialize. Default
    /// true for new installs. Set `false` for the original full-mirror behavior.
    #[serde(default = "default_lazy")]
    pub lazy: bool,
}

fn default_remote_poll() -> u64 {
    45
}
fn default_local_rescan() -> u64 {
    300
}
fn default_lazy() -> bool {
    true
}

impl Config {
    /// Load from the default path: `$WUNDERDRIVE_CONFIG`,
    /// else `~/.config/wunderdrive/config.toml`.
    pub fn load_default() -> Result<Self> {
        let path = if let Ok(p) = std::env::var("WUNDERDRIVE_CONFIG") {
            PathBuf::from(p)
        } else {
            default_config_path()
        };
        Self::load(&path)
    }

    /// Load from `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
        let mut cfg: Config = toml::from_str(&raw)
            .map_err(|e| Error::Config(format!("parsing {}: {e}", path.display())))?;
        cfg.expand()?;
        Ok(cfg)
    }

    /// Save to `path`.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw =
            toml::to_string_pretty(self).map_err(|e| Error::Config(format!("encoding: {e}")))?;
        std::fs::write(path, raw)?;
        Ok(())
    }

    /// Expand `~` in `local_root` and normalise the prefix.
    pub fn expand(&mut self) -> Result<()> {
        self.local_root = expand_tilde(&self.local_root);
        if !self.prefix.is_empty() && !self.prefix.ends_with('/') {
            self.prefix.push('/');
        }
        Ok(())
    }
}

/// Default config file location.
pub fn default_config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("wunderdrive").join("config.toml")
}

/// Default credentials file location (headless fallback).
///
/// Sits next to `config.toml` and holds S3 access keys for headless
/// deployments where no OS keychain is available (NixOS sandboxes, CI,
/// containers, headless servers). The file must be mode 0600;
/// [`crate::creds::load_from_file`] enforces this on read. See
/// [`crate::creds`] for the resolution chain.
pub fn default_credentials_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("wunderdrive").join("credentials.toml")
}

/// Default journal location (next to config, in cache dir).
pub fn default_journal_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("wunderdrive").join("journal.redb")
}

/// Default Tantivy index directory (next to the journal).
pub fn default_index_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("wunderdrive").join("index")
}

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(p: &Path) -> PathBuf {
    let s = match p.to_str() {
        Some(s) => s,
        None => return p.to_path_buf(),
    };
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cfg = Config {
            endpoint: Some("https://s3.example.com".into()),
            region: "us-east-1".into(),
            bucket: "b".into(),
            prefix: "drive/".into(),
            local_root: PathBuf::from("~/drive"),
            remote_poll_secs: 30,
            local_rescan_secs: 60,
            lazy: false,
        };
        cfg.save(tmp.path()).unwrap();
        let loaded = Config::load(tmp.path()).unwrap();
        assert_eq!(loaded.bucket, "b");
        assert!(loaded.local_root.is_absolute());
        assert_eq!(loaded.prefix, "drive/");
    }

    #[test]
    fn defaults_apply() {
        let toml = r#"
endpoint = "https://s3.example.com"
region = "us-east-1"
bucket = "b"
local_root = "/tmp/drive"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml).unwrap();
        let cfg = Config::load(tmp.path()).unwrap();
        assert_eq!(cfg.remote_poll_secs, 45);
        assert_eq!(cfg.local_rescan_secs, 300);
        assert!(cfg.lazy); // default true for new installs
    }
}
