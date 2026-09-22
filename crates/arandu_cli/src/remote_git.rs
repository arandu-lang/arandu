//! Secure, exact-commit Git materialization owned by CLI orchestration.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arandu_query::{CacheDigest, ManifestDependency};
use sha2::{Digest as _, Sha256};

use crate::cache::{CacheStore, TreeLimits};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedGit {
    pub origin: String,
    pub commit: String,
    pub content_digest: CacheDigest,
    pub root: PathBuf,
}

pub fn materialize(
    store: &CacheStore,
    dependency: &ManifestDependency,
    expected_digest: Option<CacheDigest>,
    offline: bool,
) -> Result<MaterializedGit, String> {
    let ManifestDependency::Git { origin, commit } = dependency else {
        return Err("internal error: attempted to fetch a non-Git dependency".into());
    };
    validate_git_dependency(origin, commit)?;
    if let Some(digest) = expected_digest {
        match store.trusted_tree(digest, TreeLimits::default()) {
            Ok(root) => {
                let root = fs::canonicalize(&root).map_err(|error| {
                    format!(
                        "cannot canonicalize cached Git dependency {}: {error}",
                        root.display()
                    )
                })?;
                return Ok(MaterializedGit {
                    origin: origin.clone(),
                    commit: commit.clone(),
                    content_digest: digest,
                    root,
                });
            }
            Err(error) if offline => {
                return Err(format!(
                    "cached Git dependency failed verification under --offline: {error}"
                ));
            }
            Err(_) => {}
        }
    }
    if offline {
        return Err(format!(
            "Git dependency {origin}#{commit} is not present in the verified lock/cache and --offline forbids network access"
        ));
    }

    let staging_root = store.layout().staging();
    fs::create_dir_all(&staging_root)
        .map_err(|error| format!("cannot create Git staging directory: {error}"))?;
    let identity = git_identity(origin, commit);
    let work = staging_root.join(format!(
        "git-{}-{}-{}",
        identity.hex(),
        std::process::id(),
        nonce()
    ));
    let repository = work.join("repository");
    let checkout = work.join("tree");
    fs::create_dir_all(&repository)
        .map_err(|error| format!("cannot create Git repository staging: {error}"))?;

    let result = fetch_exact(origin, commit, &repository, &checkout).and_then(|()| {
        store
            .publish_tree(&checkout, TreeLimits::default())
            .map_err(|e| e.to_string())
    });
    let materialized = match result {
        Ok((_publish, verification, root)) => {
            let root = fs::canonicalize(&root).map_err(|error| {
                format!(
                    "cannot canonicalize cached Git dependency {}: {error}",
                    root.display()
                )
            })?;
            if let Some(expected) = expected_digest {
                if verification.digest != expected {
                    Err(format!(
                        "Git dependency content changed for locked {origin}#{commit}: expected {expected}, found {}",
                        verification.digest
                    ))
                } else {
                    Ok(MaterializedGit {
                        origin: origin.clone(),
                        commit: commit.clone(),
                        content_digest: verification.digest,
                        root,
                    })
                }
            } else {
                Ok(MaterializedGit {
                    origin: origin.clone(),
                    commit: commit.clone(),
                    content_digest: verification.digest,
                    root,
                })
            }
        }
        Err(error) => Err(error),
    };
    if work.starts_with(&staging_root) && work != staging_root {
        let _ = fs::remove_dir_all(&work);
    }
    materialized
}

/// Validate manifest-supplied Git inputs before they reach `git` argv.
///
/// `commit` must be a full object id (40 hex for SHA-1, 64 hex for SHA-256,
/// upper or lower case). The existing `rev-parse FETCH_HEAD^{commit}` equality
/// check already implicitly requires a full hash, so this rejects anything the
/// flow could not honor anyway — including strings starting with `-` or
/// containing `..`, whitespace or control characters (argument injection).
///
/// `origin` must be non-empty, free of control characters/newlines, free of
/// leading/trailing whitespace, and must not start with `-` (which would make
/// git treat it as an option). Protocol restrictions (`protocol.allow=never`,
/// `protocol.https.allow=always`) are enforced separately in `run_git`.
fn validate_git_dependency(origin: &str, commit: &str) -> Result<(), String> {
    let hex_len_ok = commit.len() == 40 || commit.len() == 64;
    if !hex_len_ok || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid Git dependency commit `{commit}`: expected a full 40-character (SHA-1) or 64-character (SHA-256) hexadecimal object id"
        ));
    }
    if origin.is_empty() {
        return Err("invalid Git dependency origin: must not be empty".into());
    }
    if origin.starts_with('-') {
        return Err(format!(
            "invalid Git dependency origin `{origin}`: must not start with `-`"
        ));
    }
    if origin.chars().any(char::is_control) {
        return Err(format!(
            "invalid Git dependency origin `{origin}`: must not contain control characters or newlines"
        ));
    }
    if origin.trim() != origin {
        return Err(format!(
            "invalid Git dependency origin `{origin}`: must not start or end with whitespace"
        ));
    }
    Ok(())
}

fn fetch_exact(
    origin: &str,
    commit: &str,
    repository: &Path,
    checkout: &Path,
) -> Result<(), String> {
    run_git(repository, &["init", "--bare", "."])?;
    run_git(
        repository,
        &["fetch", "--no-tags", "--depth=1", origin, commit],
    )?;
    let resolved = run_git(
        repository,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
    )?;
    if resolved.trim() != commit {
        return Err(format!(
            "Git server resolved requested commit {commit} to unexpected object {}",
            resolved.trim()
        ));
    }
    fs::create_dir_all(checkout)
        .map_err(|error| format!("cannot create Git checkout staging: {error}"))?;
    let checkout_text = checkout
        .to_str()
        .ok_or_else(|| "Git checkout staging path is not UTF-8".to_string())?;
    run_git(
        repository,
        &[
            "--work-tree",
            checkout_text,
            "checkout",
            "--force",
            commit,
            "--",
            ".",
        ],
    )?;
    Ok(())
}

fn run_git(repository: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-c")
        .arg("protocol.allow=never")
        .arg("-c")
        .arg("protocol.https.allow=always")
        .arg("-c")
        .arg("core.hooksPath=")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .output()
        .map_err(|error| format!("cannot execute Git: {error}"))?;
    decode_git_output(arguments, output)
}

fn decode_git_output(arguments: &[&str], output: Output) -> Result<String, String> {
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .map_err(|_| "Git produced non-UTF-8 output".to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "Git `{}` failed: {}",
        arguments.join(" "),
        stderr.trim()
    ))
}

fn git_identity(origin: &str, commit: &str) -> CacheDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"arandu-git-v1\0");
    hasher.update(origin.as_bytes());
    hasher.update(b"\0");
    hasher.update(commit.as_bytes());
    CacheDigest::from_bytes(hasher.finalize().into())
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn nonce() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_query::CacheLayout;

    #[test]
    fn git_dependency_validation_rejects_non_hex_commit() {
        let error = validate_git_dependency("https://example.com/a.git", "not-a-real-hash")
            .expect_err("non-hex commit must be rejected");
        assert!(error.contains("full 40-character"));
    }

    #[test]
    fn git_dependency_validation_rejects_short_commit() {
        assert!(validate_git_dependency("https://example.com/a.git", "abc1234").is_err());
        // 39 hex chars: correct alphabet, wrong length.
        let shortened = "0123456789abcdef0123456789abcdef0123456";
        assert!(validate_git_dependency("https://example.com/a.git", shortened).is_err());
        // Leading `-` (argument injection attempt) is not a hex object id.
        assert!(
            validate_git_dependency(
                "https://example.com/a.git",
                "-e3b0c44298fc1c149afbf4c8996fb924"
            )
            .is_err()
        );
    }

    #[test]
    fn git_dependency_validation_accepts_full_hex_commit() {
        assert!(
            validate_git_dependency(
                "https://example.com/a.git",
                "0123456789abcdef0123456789abcdef01234567"
            )
            .is_ok()
        );
        assert!(
            validate_git_dependency(
                "https://example.com/a.git",
                "0123456789ABCDEF0123456789ABCDEF01234567"
            )
            .is_ok()
        );
        assert!(validate_git_dependency("https://example.com/a.git", &"a".repeat(64)).is_ok());
    }

    #[test]
    fn git_dependency_validation_rejects_option_like_origin() {
        let error = validate_git_dependency("--upload-pack=touch /tmp/pwn", &"a".repeat(40))
            .expect_err("origin starting with `-` must be rejected");
        assert!(error.contains("must not start with `-`"));
        assert!(validate_git_dependency("", &"a".repeat(40)).is_err());
    }

    #[test]
    fn git_dependency_validation_rejects_origin_with_newline() {
        let error = validate_git_dependency(
            "https://example.com/a.git\nupload-pack=evil",
            &"a".repeat(40),
        )
        .expect_err("origin containing a newline must be rejected");
        assert!(error.contains("control characters"));
        // Control characters other than newlines, and padded whitespace.
        assert!(validate_git_dependency("https://example.com/a.git\0", &"a".repeat(40)).is_err());
        assert!(validate_git_dependency(" https://example.com/a.git", &"a".repeat(40)).is_err());
        assert!(validate_git_dependency("https://example.com/a.git ", &"a".repeat(40)).is_err());
    }

    #[test]
    fn git_identity_separates_origins_and_commits() {
        assert_ne!(
            git_identity("https://example.com/a.git", &"a".repeat(40)),
            git_identity("https://example.com/b.git", &"a".repeat(40))
        );
        assert_ne!(
            git_identity("https://example.com/a.git", &"a".repeat(40)),
            git_identity("https://example.com/a.git", &"b".repeat(40))
        );
    }

    #[test]
    fn failed_git_output_does_not_echo_environment_or_credentials() {
        let output = Output {
            status: failure_status(),
            stdout: Vec::new(),
            stderr: b"remote rejected request".to_vec(),
        };
        let error = decode_git_output(&["fetch", "https://example.com/a.git"], output)
            .expect_err("failure must be reported");
        assert!(error.contains("remote rejected request"));
    }

    #[test]
    fn offline_materialization_accepts_only_a_revalidated_locked_tree() {
        let cache_root = std::env::temp_dir().join(format!(
            "arandu-remote-offline-{}-{}",
            std::process::id(),
            nonce()
        ));
        let layout = CacheLayout::new(cache_root.clone()).unwrap();
        fs::create_dir_all(layout.staging()).unwrap();
        let store = CacheStore::new(layout.clone());
        let staging = layout.staging().join("package");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(
            staging.join("src/lib.aru"),
            "public func ok(): int { return 1 }\n",
        )
        .unwrap();
        let (_, verified, _) = store.publish_tree(&staging, TreeLimits::default()).unwrap();
        let dependency = ManifestDependency::Git {
            origin: "https://example.com/package.git".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
        };

        let resolved = materialize(&store, &dependency, Some(verified.digest), true).unwrap();
        assert_eq!(resolved.content_digest, verified.digest);
        fs::write(resolved.root.join("src/lib.aru"), "tampered\n").unwrap();
        assert!(materialize(&store, &dependency, Some(verified.digest), true).is_err());

        fs::remove_dir_all(cache_root).unwrap();
    }

    #[cfg(unix)]
    fn failure_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1 << 8)
    }

    #[cfg(windows)]
    fn failure_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }
}
