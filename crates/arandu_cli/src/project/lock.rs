//! Atomic lockfile synchronization and review policies.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;

use super::load::ProjectFlags;
use crate::cli_error::CliFailure;

pub fn synchronize_lockfile(
    root: &Path,
    expected: arandu_query::Lockfile,
    flags: &ProjectFlags,
) -> Result<(), String> {
    // Serialize the complete read/compare/publish transaction. A persistent
    // lock inode is intentional: deleting it after unlock can let two
    // processes lock different inodes. The OS releases this lock on crash.
    let lock_dir = root.join(".arandu").join("locks");
    fs::create_dir_all(&lock_dir)
        .map_err(|error| format!("cannot create {}: {error}", lock_dir.display()))?;
    let lock_path = lock_dir.join("lockfile");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("cannot open {}: {error}", lock_path.display()))?;
    lock.lock()
        .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;

    let path = root.join(arandu_query::LOCK_FILENAME);
    let expected_bytes = expected.to_canonical_bytes();
    let mut previous = None;
    match fs::read(&path) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                format!("invalid {}: file is not UTF-8: {error}", path.display())
            })?;
            let current = match arandu_query::Lockfile::parse(&path, text) {
                Ok(current) => Some(current),
                Err(error)
                    if flags.accept_lock
                        && !flags.locked
                        && !expected.contains_remote_packages() =>
                {
                    // Explicit recovery is restricted to a locally resolved graph.
                    // Retain the original bytes; never silently discard trust data
                    // or overwrite an earlier, different recovery copy.
                    let backup = lock_dir.join("invalid-arandu.lock");
                    match fs::read(&backup) {
                        Ok(previous) if previous != bytes => {
                            return Err(format!(
                                "{error}; recovery backup {} already contains a different lockfile",
                                backup.display()
                            ));
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == ErrorKind::NotFound => {
                            crate::artifact::atomic_replace(&backup, &bytes).map_err(|error| {
                                format!("cannot preserve invalid lockfile: {error:?}")
                            })?;
                        }
                        Err(error) => {
                            return Err(format!("cannot read {}: {error}", backup.display()));
                        }
                    }
                    eprintln!(
                        "recovering invalid local lockfile; original retained at {}",
                        backup.display()
                    );
                    None
                }
                Err(error) => {
                    return Err(format!(
                        "{error}; invalid lockfiles are never accepted as integrity evidence. For a local-only graph, run 'arandu update --accept' to regenerate it while preserving the original"
                    ));
                }
            };
            if let Some(current) = current {
                if current == expected && bytes == expected_bytes {
                    return Ok(());
                }
                if flags.locked {
                    return Err(format!(
                        "{} is stale or noncanonical and --locked forbids updating it",
                        path.display()
                    ));
                }
                previous = Some(current);
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if flags.locked {
                return Err(format!(
                    "{} is missing and --locked forbids creating it",
                    path.display()
                ));
            }
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    }
    let remote_authority_changes = expected.contains_remote_packages()
        || previous
            .as_ref()
            .is_some_and(arandu_query::Lockfile::contains_remote_packages);
    if remote_authority_changes {
        let empty = arandu_query::Lockfile {
            manifest_fingerprint:
                "blake3:0000000000000000000000000000000000000000000000000000000000000000".into(),
            packages: Vec::new(),
        };
        let current = previous.as_ref().unwrap_or(&empty);
        let diff = current.graph_diff(&expected).join("\n");
        if !flags.accept_lock {
            return Err(format!(
                "remote dependency graph requires explicit review:\n{diff}\nreview the complete diff, then run 'arandu update --accept'"
            ));
        }
        eprintln!("accepting reviewed remote dependency graph:\n{diff}");
    }
    crate::artifact::atomic_replace(&path, &expected_bytes).map_err(|error| match error {
        CliFailure::Operational {
            operation,
            context,
            source,
        } => format!(
            "{operation}{}: {source}",
            context
                .map(|path| format!(" {}", path.display()))
                .unwrap_or_default()
        ),
        other => format!("unexpected lockfile publication failure: {other:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_query::{LockedPackage, Lockfile};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn invalid_local_lock_requires_explicit_recovery_and_preserves_original() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lock-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temporary project");
        let path = root.join(arandu_query::LOCK_FILENAME);
        let invalid = b"manifest_fingerprint = 'blake3:placeholder'\n";
        fs::write(&path, invalid).expect("invalid lock");
        let expected = Lockfile {
            manifest_fingerprint: format!("blake3:{}", "0".repeat(64)),
            packages: Vec::new(),
        };
        let error = synchronize_lockfile(&root, expected.clone(), &ProjectFlags::default())
            .expect_err("must reject invalid hashes");
        assert!(error.contains("arandu update --accept"));
        assert_eq!(fs::read(&path).expect("retained lock"), invalid);
        assert!(
            synchronize_lockfile(
                &root,
                expected.clone(),
                &ProjectFlags {
                    locked: true,
                    accept_lock: true,
                    ..ProjectFlags::default()
                }
            )
            .is_err()
        );
        synchronize_lockfile(
            &root,
            expected.clone(),
            &ProjectFlags {
                accept_lock: true,
                ..ProjectFlags::default()
            },
        )
        .expect("explicit local recovery");
        assert_eq!(
            fs::read(&path).expect("new lock"),
            expected.to_canonical_bytes()
        );
        assert_eq!(
            fs::read(root.join(".arandu/locks/invalid-arandu.lock")).expect("backup"),
            invalid
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn remote_lock(commit: &str, digest: &str) -> Lockfile {
        let source = format!("git+https://example.com/math.git#{commit}");
        Lockfile {
            manifest_fingerprint:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            packages: vec![LockedPackage {
                name: "math".into(),
                version: "1.0.0".into(),
                source,
                manifest_fingerprint:
                    "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                origin: Some("https://example.com/math.git".into()),
                commit: Some(commit.into()),
                content_digest: Some(digest.into()),
                dependencies: Vec::new(),
            }],
        }
    }

    #[test]
    fn remote_graph_requires_review_then_explicit_acceptance() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lock-review-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temp project");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let expected = remote_lock(commit, digest);
        let error = synchronize_lockfile(&root, expected.clone(), &ProjectFlags::default())
            .expect_err("first remote trust must not be implicit");
        assert!(error.contains("+ package git+https://example.com/math.git"));
        assert!(error.contains("arandu update --accept"));
        assert!(!root.join(arandu_query::LOCK_FILENAME).exists());

        synchronize_lockfile(
            &root,
            expected.clone(),
            &ProjectFlags {
                accept_lock: true,
                ..ProjectFlags::default()
            },
        )
        .expect("explicit acceptance publishes lock");
        let bytes = fs::read(root.join(arandu_query::LOCK_FILENAME)).expect("published lock");
        assert_eq!(bytes, expected.to_canonical_bytes());

        let next = remote_lock(
            "1111111111111111111111111111111111111111",
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        );
        let error = synchronize_lockfile(&root, next, &ProjectFlags::default())
            .expect_err("remote update must not be implicit");
        assert!(error.contains("- package "));
        assert!(error.contains("+ package "));
        assert_eq!(
            fs::read(root.join(arandu_query::LOCK_FILENAME)).expect("old lock retained"),
            bytes
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
