//! Construct the [`ObjectStore`] (any S3-compatible endpoint) from config+creds.

use std::sync::Arc;

use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey};
use object_store::ObjectStore;

use crate::config::Config;
use crate::creds::Credentials;
use crate::error::Result;

/// The metadata attribute name carrying the content blake3 hash on each object.
pub const HASH_ATTR: &str = "content-hash";

/// Build an S3-compatible [`ObjectStore`] for the given config + credentials.
///
/// `virtual_hosted_style` is left false (path-style) by default since most
/// S3-compatible providers (MinIO, R2, Garage, SeaweedFS, …) expect path-style.
pub fn build(cfg: &Config, creds: &Credentials) -> Result<Arc<dyn ObjectStore>> {
    let mut builder = AmazonS3Builder::new()
        .with_region(&cfg.region)
        .with_bucket_name(&cfg.bucket)
        .with_access_key_id(&creds.access_key_id)
        .with_secret_access_key(&creds.secret_access_key)
        .with_conditional_put(object_store::aws::S3ConditionalPut::ETagMatch);

    if let Some(endpoint) = cfg.endpoint.as_deref() {
        builder = builder.with_config(AmazonS3ConfigKey::Endpoint, endpoint);
    }
    // Allow path-style endpoints (non-AWS). Disable automatically deriving
    // virtual-hosted style from the bucket name.
    builder = builder.with_config(AmazonS3ConfigKey::VirtualHostedStyleRequest, "false");
    builder = builder.with_allow_http(true);

    let store = builder.build()?;
    // Apply the configured key prefix as a virtual folder.
    if cfg.prefix.is_empty() {
        Ok(Arc::new(store))
    } else {
        let prefix = object_store::path::Path::parse(&cfg.prefix)?;
        Ok(Arc::new(object_store::prefix::PrefixStore::new(
            store, prefix,
        )))
    }
}
