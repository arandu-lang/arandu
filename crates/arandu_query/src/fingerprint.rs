//! Input fingerprints that stand in for content hashes of large query outputs.
//!
//! Some tracked outputs embed a type-check context holding the declaration
//! closure of the program. Hashing that content once per item costs
//! O(program) per item. Such a query may instead identify its output by the
//! digests Salsa uses to compare everything it read (its fingerprint).
//!
//! That is sound only when the fingerprint covers every read and the output is
//! a deterministic function of them: a missing read would let Salsa backdate a
//! changed value and leave consumers stale. Debug builds therefore also hash
//! the content and fail when one database produces two contents for the same
//! fingerprint. Outputs carry their identity in [`crate::db::FingerprintEq`].

/// Fails when this database already produced different content for
/// `fingerprint` of the same query kind, which means the fingerprint omits a
/// read.
#[cfg(any(test, debug_assertions))]
pub(crate) fn verify(
    db: &dyn crate::ArandCompilerDb,
    kind: &'static str,
    fingerprint: blake3::Hash,
    content: blake3::Hash,
) {
    oracle::check(
        db.as_db_impl()
            .map_or(0, crate::DatabaseImpl::fingerprint_scope),
        kind,
        fingerprint,
        content,
    );
}

#[cfg(any(test, debug_assertions))]
pub(crate) mod oracle {
    use rustc_hash::FxHashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};

    /// Entries kept before the oracle forgets older observations.
    const LIMIT: usize = 1 << 20;

    type Seen = FxHashMap<(u64, &'static str, [u8; 32]), blake3::Hash>;

    static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();

    /// A scope never reused within the process, shared by a database's clones.
    pub(crate) fn next_scope() -> u64 {
        NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
    }

    pub(super) fn check(
        scope: u64,
        kind: &'static str,
        fingerprint: blake3::Hash,
        content: blake3::Hash,
    ) {
        let previous = {
            let mut seen = SEEN
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if seen.len() >= LIMIT {
                seen.clear();
            }
            *seen
                .entry((scope, kind, *fingerprint.as_bytes()))
                .or_insert(content)
        };
        assert_eq!(
            previous, content,
            "{kind}: one input fingerprint produced two contents; the fingerprint omits a read"
        );
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn equal_fingerprints_with_equal_content_pass_and_scopes_are_isolated() {
            let scope = next_scope();
            let fingerprint = blake3::hash(b"inputs");
            check(scope, "test", fingerprint, blake3::hash(b"a"));
            check(scope, "test", fingerprint, blake3::hash(b"a"));
            check(next_scope(), "test", fingerprint, blake3::hash(b"b"));
        }

        #[test]
        #[should_panic(expected = "omits a read")]
        fn equal_fingerprints_with_different_content_fail() {
            let scope = next_scope();
            let fingerprint = blake3::hash(b"inputs");
            check(scope, "test", fingerprint, blake3::hash(b"a"));
            check(scope, "test", fingerprint, blake3::hash(b"b"));
        }
    }
}
