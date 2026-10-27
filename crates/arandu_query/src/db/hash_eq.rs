use std::sync::{Arc, OnceLock};

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

    /// Content identity of a fingerprinted output, for consumers that compare
    /// by content. Computes the content hash if no comparison did yet.
    #[must_use]
    pub fn from_fingerprinted(other: &FingerprintEq<T>) -> Self {
        Self {
            value: Arc::clone(&other.value),
            hash: other.content_hash(),
        }
    }

    /// Share `value` under an identity coarser than its content, such as a
    /// surface digest that deliberately ignores positions. Consumers then
    /// observe only what `hash` covers.
    #[must_use]
    pub(crate) fn with_identity(value: Arc<T>, hash: blake3::Hash) -> Self {
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

/// A query output identified by an input fingerprint (see
/// [`crate::fingerprint`]) instead of an eager content hash.
///
/// Equal fingerprints imply equal content. Different fingerprints fall back to
/// the content hash, computed on first comparison, so the relation is exactly
/// content equality and early cutoff is unchanged. A fresh database never
/// compares against an earlier memo and therefore never pays for the content
/// hash.
pub struct FingerprintEq<T> {
    pub value: Arc<T>,
    fingerprint: blake3::Hash,
    content: Arc<OnceLock<blake3::Hash>>,
}

impl<T> Clone for FingerprintEq<T> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
            fingerprint: self.fingerprint,
            content: Arc::clone(&self.content),
        }
    }
}

impl<T: StableHash> FingerprintEq<T> {
    /// Identity by `fingerprint`, which must cover every read that determines
    /// `value`. Debug builds check that with the fingerprint oracle.
    pub(crate) fn fingerprinted(
        db: &dyn crate::ArandCompilerDb,
        kind: &'static str,
        value: impl Into<Arc<T>>,
        fingerprint: blake3::Hash,
    ) -> Self {
        let value = value.into();
        let content = OnceLock::new();
        #[cfg(any(test, debug_assertions))]
        {
            let hash = value.stable_hash();
            crate::fingerprint::verify(db, kind, fingerprint, hash);
            let _ = content.set(hash);
        }
        #[cfg(not(any(test, debug_assertions)))]
        let _ = (db, kind);
        Self {
            value,
            fingerprint,
            content: Arc::new(content),
        }
    }

    /// Identity by content, for outputs whose reads are not fingerprinted.
    pub(crate) fn exact(value: impl Into<Arc<T>>) -> Self {
        let value = value.into();
        let hash = value.stable_hash();
        Self {
            value,
            fingerprint: hash,
            content: Arc::new(OnceLock::from(hash)),
        }
    }

    fn content_hash(&self) -> blake3::Hash {
        *self.content.get_or_init(|| self.value.stable_hash())
    }
}

impl<T> FingerprintEq<T> {
    /// Identity by the content hash `value` already stores.
    pub(crate) fn from_content(value: &HashEq<T>) -> Self {
        Self {
            value: Arc::clone(&value.value),
            fingerprint: value.hash,
            content: Arc::new(OnceLock::from(value.hash)),
        }
    }
}

/// Embeds the fingerprint. Folding it into another identity is sound because
/// equal fingerprints imply equal content; it can only cost a cutoff when this
/// output was backdated with a new fingerprint and the consumer re-executes
/// for another reason.
impl<T> StableHash for FingerprintEq<T> {
    fn stable_hash(&self) -> blake3::Hash {
        self.fingerprint
    }
}

impl<T: StableHash> PartialEq for FingerprintEq<T> {
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint || self.content_hash() == other.content_hash()
    }
}
impl<T: StableHash> Eq for FingerprintEq<T> {}

impl<T> std::ops::Deref for FingerprintEq<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.value
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
