//! Injected, reproducible UUID-shaped identities; no ambient random source.

use parking_lot::Mutex;
use uuid::Uuid;

/// Supplies correlation, subscription, and generated-session identities.
pub trait IdSource: Send + Sync {
    /// Returns the next UUID from this owner's stream.
    fn next_uuid(&self) -> Uuid;
}

/// Deterministic UUID stream initialized by an explicit boundary-provided seed.
pub struct SeededIds {
    seed: Uuid,
    serial: Mutex<u128>,
}

impl SeededIds {
    /// Seeds a distinct identity stream; tests may reuse a seed to reproduce identifiers.
    pub fn new(seed: [u8; 16]) -> Self {
        Self {
            seed: Uuid::from_bytes(seed),
            serial: Mutex::new(0),
        }
    }
}

impl IdSource for SeededIds {
    fn next_uuid(&self) -> Uuid {
        let mut serial = self.serial.lock();
        let mut bytes = *Uuid::new_v5(&self.seed, &serial.to_le_bytes()).as_bytes();
        *serial = serial.wrapping_add(1);
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Uuid::from_bytes(bytes)
    }
}
