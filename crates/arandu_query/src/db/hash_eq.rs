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

/// Every constructor stores `value.stable_hash()` of an immutable shared value,
/// so the stored digest is that hash. Without this impl, method lookup derefs
/// to `T` and rehashes the whole value on every fingerprint.
impl<T> StableHash for HashEq<T> {
    fn stable_hash(&self) -> blake3::Hash {
        self.hash
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counted {
        payload: u8,
        hashes: AtomicUsize,
    }

    impl StableHash for Counted {
        fn stable_hash(&self) -> blake3::Hash {
            self.hashes.fetch_add(1, Ordering::Relaxed);
            blake3::hash(&[self.payload])
        }
    }

    #[test]
    fn stable_hash_reuses_the_construction_digest() {
        let value = HashEq::new(Counted {
            payload: 7,
            hashes: AtomicUsize::new(0),
        });
        let shared = HashEq::share(&value);
        let wrapped = HashEq::from_arc(Arc::clone(&value.value));
        let expected = blake3::hash(&[7]);
        for _ in 0..3 {
            assert_eq!(value.stable_hash(), expected);
            assert_eq!(shared.stable_hash(), expected);
            assert_eq!(wrapped.stable_hash(), expected);
        }
        // `new` and `from_arc` hash once each; `share` and every lookup reuse it.
        assert_eq!(value.hashes.load(Ordering::Relaxed), 2);
    }
}
