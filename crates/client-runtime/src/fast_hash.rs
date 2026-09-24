//! A deterministic hasher for keys this process produces.
//!
//! The maps that grow per conversation event are keyed by native pointer addresses, event
//! sequences, and node keys — keys a caller cannot steer into collisions, so the default hasher's
//! collision resistance is paid for and never used. FNV-1a is unseeded, so it is also reproducible
//! where the default hasher is randomly seeded per map.

use std::hash::{BuildHasherDefault, Hasher};

/// FNV-1a, the 64-bit variant.
#[derive(Default)]
pub struct FnvHasher(u64);

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = if self.0 == 0 {
            Self::OFFSET_BASIS
        } else {
            self.0
        };
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(Self::PRIME);
        }
        self.0 = hash;
    }
}

impl FnvHasher {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
}

/// The [`std::collections::HashMap`] hasher this module owns.
pub type FnvBuildHasher = BuildHasherDefault<FnvHasher>;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn hashing_the_same_key_twice_agrees_and_different_keys_differ() {
        let mut map = HashMap::<usize, u64, FnvBuildHasher>::default();
        map.insert(1, 11);
        map.insert(2, 22);
        map.insert(usize::MAX, 33);
        assert_eq!(map.get(&1), Some(&11));
        assert_eq!(map.get(&2), Some(&22));
        assert_eq!(map.get(&usize::MAX), Some(&33));
        assert_eq!(map.get(&3), None);

        let mut map = HashMap::<String, u64, FnvBuildHasher>::default();
        map.insert("assistant/chunk".to_owned(), 1);
        map.insert("assistant/message".to_owned(), 2);
        assert_eq!(map.get("assistant/chunk"), Some(&1));
        assert_eq!(map.get("assistant/message"), Some(&2));
        assert_eq!(map.get("assistant"), None);
    }

    #[test]
    fn an_empty_key_still_hashes_to_the_offset_basis() {
        assert_eq!(
            FnvHasher::default().finish(),
            0,
            "the basis is applied on write"
        );
        let mut empty = FnvHasher::default();
        empty.write(&[]);
        assert_eq!(empty.finish(), FnvHasher::OFFSET_BASIS);
    }
}
