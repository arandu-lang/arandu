//! Virtual FS inputs for package-local module discovery (PROMOTE-L2).
//!
//! Package sources are user-owned and can appear/disappear between runs
//! (and later under watch mode). Existence checks go through
//! [`DirectoryListing`] — a `#[salsa::input]` — never bare `fs::exists`
//! in the resolve hot path, so adding `src/novo.aru` can invalidate
//! `resolve_module_path` without restarting the process.
//!
//! Stdlib stays install-fixed (Camada D); package root is the second root
//! registered on the same `DatabaseImpl::resolve_module_path`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::FxHashSet;

use arandu_middle::{ModuleId, PackageId, TargetId};

use crate::SourceFile;

/// Reserved package name roots that must never appear in `Arandu.toml` `name`.
///
/// Prevents ambiguous resolution between `std.*` and a local package named `std`.
pub const RESERVED_PACKAGE_ROOTS: &[&str] = &[
    "std", "core", "alloc", "io", "err", "arandu", "stdlib", "prelude",
];

/// Why a package name is illegal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservedNameError {
    Empty,
    Reserved { name: String },
    StartsWithStd { name: String },
    InvalidIdent { name: String },
}

impl std::fmt::Display for ReservedNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReservedNameError::Empty => write!(f, "package name must be non-empty"),
            ReservedNameError::Reserved { name } => write!(
                f,
                "package name `{name}` is reserved (collides with a language/stdlib root)"
            ),
            ReservedNameError::StartsWithStd { name } => write!(
                f,
                "package name `{name}` cannot start with `std` (reserved stdlib prefix)"
            ),
            ReservedNameError::InvalidIdent { name } => write!(
                f,
                "package name `{name}` must be a simple identifier (ascii letters, digits, `_`)"
            ),
        }
    }
}

impl std::error::Error for ReservedNameError {}

/// Reject names that would collide with `std.*` / prelude roots.
pub fn validate_package_name(name: &str) -> Result<(), ReservedNameError> {
    if name.is_empty() {
        return Err(ReservedNameError::Empty);
    }
    if !is_simple_ident(name) {
        return Err(ReservedNameError::InvalidIdent {
            name: name.to_string(),
        });
    }
    let lower = name.to_ascii_lowercase();
    if RESERVED_PACKAGE_ROOTS.iter().any(|r| lower == *r) {
        return Err(ReservedNameError::Reserved {
            name: name.to_string(),
        });
    }
    if lower == "std" || lower.starts_with("std_") || lower.starts_with("std.") {
        return Err(ReservedNameError::StartsWithStd {
            name: name.to_string(),
        });
    }
    Ok(())
}

fn is_simple_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Snapshot of files under a directory (relative paths, forward slashes).
///
/// Updated by the CLI at package load (and later by a watcher) — not polled
/// with `fs::read_dir` inside every resolve.
#[salsa::input]
pub struct DirectoryListing {
    pub dir: Arc<PathBuf>,
    /// Relative paths of known `.aru` files under `dir` (e.g. `util.aru`, `sub/x.aru`).
    #[returns(ref)]
    pub entries: Arc<Vec<String>>,
}

/// Dual module roots: package (`Arandu.toml`) + stdlib (Camada D).
///
/// Both roots feed the **same** [`SourceDatabase::resolve_module_path`](arandu_middle::db::SourceDatabase::resolve_module_path);
/// there is no second resolver.
#[salsa::input]
pub struct ModuleRoots {
    /// Package name from `Arandu.toml` (`my_app` → import key `my_app/util.aru`).
    #[returns(ref)]
    pub package_name: String,
    /// Source root for package modules (directory of the entry file, usually `src/`).
    pub package_src: Arc<PathBuf>,
    /// Stdlib root when known (install cascade); `None` uses legacy cwd walk only.
    pub stdlib_root: Option<Arc<PathBuf>>,
    /// VFS listing for `package_src` — drives package-local existence checks.
    pub package_listing: DirectoryListing,
}

/// One pre-resolved logical module binding. Physical discovery and source
/// registration happen before this value enters Salsa.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ModuleBinding {
    pub package: PackageId,
    pub target: TargetId,
    pub module: ModuleId,
    pub file: SourceFile,
}

/// Immutable package/module namespace for one analysis root.
///
/// Keys are sorted logical imports (`self/x.aru`, `alias/x.aru`); queries never
/// turn them into filesystem paths.
#[salsa::input]
pub struct PackageModuleMap {
    pub current_package: PackageId,
    pub current_target: TargetId,
    #[returns(ref)]
    pub bindings: Arc<Vec<(String, ModuleBinding)>>,
}

#[must_use]
pub fn package_module(
    db: &dyn salsa::Database,
    map: PackageModuleMap,
    logical_path: &str,
) -> Option<ModuleBinding> {
    map.bindings(db)
        .binary_search_by(|(path, _)| path.as_str().cmp(logical_path))
        .ok()
        .map(|index| map.bindings(db)[index].1)
}

/// True if `rel` is present in the listing (exact relative path match).
#[must_use]
pub fn listing_contains(db: &dyn salsa::Database, listing: DirectoryListing, rel: &str) -> bool {
    let entries = listing.entries(db);
    entries.iter().any(|e| e == rel)
}

/// Maximum directory nesting depth to prevent runaway traversals.
pub const MAX_DIR_DEPTH: usize = 64;

/// Scan `dir` for `.aru` files using an iterative heap-allocated worklist.
/// Protected against symlink cycles and bounded by [`MAX_DIR_DEPTH`].
pub fn scan_aru_entries(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut visited_dirs: FxHashSet<PathBuf> = FxHashSet::default();

    let root_canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    visited_dirs.insert(root_canonical);

    while let Some((current_dir, depth)) = stack.pop() {
        if depth >= MAX_DIR_DEPTH {
            continue;
        }

        let Ok(rd) = std::fs::read_dir(&current_dir) else {
            continue;
        };

        for entry in rd.flatten() {
            let path = entry.path();
            let is_dir = match entry.file_type() {
                Ok(ft) => {
                    if ft.is_dir() {
                        true
                    } else if ft.is_symlink() {
                        std::fs::metadata(&path)
                            .map(|m| m.is_dir())
                            .unwrap_or(false)
                    } else {
                        false
                    }
                }
                Err(_) => false,
            };

            if is_dir {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                if !visited_dirs.insert(canonical) {
                    continue;
                }
                stack.push((path, depth + 1));
            } else if path.extension().and_then(|e| e.to_str()) == Some("aru") {
                if let Ok(rel) = path.strip_prefix(dir) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

/// Map a canonical import key (`my_app/util.aru`, `stdlib/std/io.aru`, `util.aru`)
/// onto a filesystem path under the configured roots, if the listing allows it.
///
/// Returns `(absolute_path, registry_key)` — registry key stays the import key.
#[must_use]
pub fn map_import_key(
    db: &dyn salsa::Database,
    roots: ModuleRoots,
    import_key: &str,
) -> Option<PathBuf> {
    let pkg = roots.package_name(db);
    let src = roots.package_src(db);
    let listing = roots.package_listing(db);
    let key = import_key.replace('\\', "/");

    // stdlib/… → install stdlib root (not package listing)
    if let Some(rest) = key.strip_prefix("stdlib/") {
        let stdlib = roots.stdlib_root(db).as_ref()?.clone();
        let candidate = stdlib.join(rest);
        // Stdlib is install-fixed; existence via metadata is OK (not watch-driven).
        if candidate.is_file() {
            return Some(candidate);
        }
        return None;
    }

    // my_app/util.aru → package_src/util.aru
    let pkg_prefix = format!("{pkg}/");
    if let Some(rest) = key.strip_prefix(&pkg_prefix) {
        if listing_contains(db, *listing, rest) {
            return Some(src.join(rest));
        }
        return None;
    }

    // self/util.aru → current target source root. `self` is logical, never cwd.
    if let Some(rest) = key.strip_prefix("self/") {
        if listing_contains(db, *listing, rest) {
            return Some(src.join(rest));
        }
        return None;
    }

    // Bare util.aru (legacy import form) relative to package src
    if !key.contains("..") && listing_contains(db, *listing, &key) {
        return Some(src.join(&key));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_reserved_std() {
        assert!(matches!(
            validate_package_name("std"),
            Err(ReservedNameError::Reserved { .. })
        ));
        assert!(matches!(
            validate_package_name("io"),
            Err(ReservedNameError::Reserved { .. })
        ));
    }

    #[test]
    fn accepts_normal_names() {
        assert!(validate_package_name("my_app").is_ok());
        assert!(validate_package_name("hello").is_ok());
    }

    #[test]
    fn rejects_invalid_ident() {
        assert!(matches!(
            validate_package_name("my-app"),
            Err(ReservedNameError::InvalidIdent { .. })
        ));
        assert!(matches!(
            validate_package_name("1x"),
            Err(ReservedNameError::InvalidIdent { .. })
        ));
    }

    #[test]
    fn scan_aru_entries_discovers_nested_and_handles_symlink_cycles() {
        use std::fs;
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!(
            "arandu_test_vfs_{}_{}",
            std::process::id(),
            unique_id
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();

        fs::write(tmp.join("root.aru"), "func main() {}").unwrap();
        fs::write(tmp.join("sub/nested.aru"), "func helper() {}").unwrap();
        fs::write(tmp.join("sub/ignored.txt"), "hello").unwrap();

        #[cfg(unix)]
        {
            // Create a cyclic symlink: sub/cycle -> ..
            let _ = std::os::unix::fs::symlink(&tmp, tmp.join("sub/cycle"));
        }

        let entries = scan_aru_entries(&tmp);
        let _ = fs::remove_dir_all(&tmp);

        assert_eq!(entries, vec!["root.aru", "sub/nested.aru"]);
    }
}
