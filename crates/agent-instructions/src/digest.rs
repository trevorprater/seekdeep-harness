//! Content identity for workspace instruction duplicate suppression.

use sha1::{Digest, Sha1};

/// Computes the content identity used across instruction loading and session state.
#[must_use]
pub fn instruction_content_sha1(content: &str) -> String {
    hex_encode(&Sha1::digest(content.as_bytes()))
}

/// Computes the whitespace-insensitive identity used for per-directory duplicate
/// suppression.
#[must_use]
pub fn trimmed_instruction_digest(content: &str) -> String {
    instruction_content_sha1(content.trim())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    output
}
