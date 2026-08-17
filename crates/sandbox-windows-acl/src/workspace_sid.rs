//! Deterministic workspace and private-temp capability SID derivation.

use sha2::{Digest as _, Sha256};

const SUBAUTHORITY_MODULUS: u32 = (1 << 30) - 1;

fn subauthorities(bytes: &[u8]) -> (u32, u32) {
    let digest = Sha256::digest(bytes);
    let first = u32::from_le_bytes(digest[0..4].try_into().expect("fixed SHA-256 width"))
        % SUBAUTHORITY_MODULUS
        + 1;
    let second = u32::from_le_bytes(digest[4..8].try_into().expect("fixed SHA-256 width"))
        % SUBAUTHORITY_MODULUS
        + 1;
    (first, second)
}

/// Derives `S-1-4-x-y` from the caller-supplied canonical workspace spelling.
#[must_use]
pub fn workspace_write_sid(workspace_root: &str) -> String {
    let (first, second) = subauthorities(workspace_root.as_bytes());
    format!("S-1-4-{first}-{second}")
}

/// Derives domain-separated `S-1-4-x-y-1` from one random private temp path.
#[must_use]
pub fn temp_write_sid(temp_dir: &str) -> String {
    let mut input = b"temp\0".to_vec();
    input.extend_from_slice(temp_dir.as_bytes());
    let (first, second) = subauthorities(&input);
    format!("S-1-4-{first}-{second}-1")
}
