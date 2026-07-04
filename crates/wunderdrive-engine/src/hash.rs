//! blake3 content hashing (streaming over a file).

use std::io::Read;
use std::path::Path;

use blake3::Hasher;

use crate::error::Result;

/// Hash of a byte slice (convenience for tests / small blobs).
pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(bytes);
    h.finalize().into()
}

/// Streaming blake3 of a file. Returns the 32-byte digest.
///
/// Identity is the content hash — used as the source of truth for "same file"
/// across rename/move/second-device (spec §5), so we read the file once and
/// hash the whole thing (no partial hashing).
pub fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Hex-encode a 32-byte digest.
pub fn to_hex(d: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse a 64-char lowercase hex string into a 32-byte digest.
pub fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let b = s.as_bytes();
    for i in 0..32 {
        let hi = hex_nib(b[i * 2])?;
        let lo = hex_nib(b[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nib(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let d = hash_bytes(b"hello world");
        let s = to_hex(&d);
        assert_eq!(from_hex(&s), Some(d));
    }

    #[test]
    fn file_hash_matches_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"hello world").unwrap();
        assert_eq!(hash_file(&p).unwrap(), hash_bytes(b"hello world"));
    }
}
