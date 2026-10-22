use std::sync::Arc;

use super::StableHash;

#[derive(Clone)]
pub struct HashEq<T> {
    pub value: Arc<T>,
    hash: blake3::Hash,
}

impl<T: StableHash> HashEq<T> {
    pub fn new(value: T) -> Self {
        let hash = value.stable_hash();
        Self {
            value: Arc::new(value),
            hash,
        }
    }

    /// Wrap an existing `Arc` (compute hash once).
    #[must_use]
    pub fn from_arc(value: Arc<T>) -> Self {
        let hash = value.stable_hash();
        Self { value, hash }
    }

    /// Share the same `Arc` and hash without re-hashing or deep-cloning `T`.
    #[must_use]
    pub fn share(other: &Self) -> Self {
        Self {
            value: Arc::clone(&other.value),
            hash: other.hash,
        }
    }
}

impl<T> PartialEq for HashEq<T> {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}
impl<T> Eq for HashEq<T> {}

impl<T> std::ops::Deref for HashEq<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
