//! Single source of truth for mapping import AST nodes → module file keys.
//!
//! Used by both name resolution and the Salsa query layer so path rewriting
//! cannot drift (RC-PATH-TRIPLE).

use arandu_parser::ImportDecl;
use smallvec::SmallVec;

/// Expands a `ModuleGroup` import (`import std.{env, fs as f}`) into individual
/// `ModuleAlias` imports preserving each item's span. For all other variants,
/// returns a single-element list containing a clone of `import`.
#[must_use]
pub fn expand_module_group(import: &ImportDecl) -> SmallVec<[ImportDecl; 1]> {
    match import {
        ImportDecl::ModuleGroup { prefix, items, .. } => items
            .iter()
            .map(|item| {
                let mut path = prefix.clone();
                path.push(item.name.clone());
                let alias = item.alias.clone().unwrap_or_else(|| item.name.clone());
                ImportDecl::ModuleAlias {
                    span: item.span,
                    path,
                    alias,
                }
            })
            .collect(),
        other => smallvec::smallvec![other.clone()],
    }
}

/// Canonical on-disk / registry keys for all modules referenced by `import`.
/// For single-module imports returns 0 or 1 key; for `ModuleGroup` returns one
/// key per grouped item.
#[must_use]
pub fn canonicalize_import_paths(import: &ImportDecl) -> SmallVec<[String; 1]> {
    match import {
        ImportDecl::ModuleGroup { prefix, items, .. } => items
            .iter()
            .filter_map(|item| {
                let mut path = prefix.clone();
                path.push(item.name.clone());
                canonicalize_dotted_path(&path)
            })
            .collect(),
        other => canonicalize_import_path(other).into_iter().collect(),
    }
}

fn canonicalize_dotted_path(path: &[smol_str::SmolStr]) -> Option<String> {
    let path_str = path.join("/");
    if let Some(stripped) = path_str.strip_prefix("std/core/") {
        Some(format!("stdlib/core/{stripped}.aru"))
    } else if let Some(stripped) = path_str.strip_prefix("std/alloc/") {
        Some(format!("stdlib/alloc/{stripped}.aru"))
    } else if let Some(stripped) = path_str.strip_prefix("std/math/") {
        Some(format!("stdlib/math/{stripped}.aru"))
    } else if let Some(stripped) = path_str.strip_prefix("std/") {
        Some(format!("stdlib/std/{stripped}.aru"))
    } else {
        Some(format!("{path_str}.aru"))
    }
}

/// Transport-free import identity. Package discovery supplies its mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalImport {
    Std { module: String },
    SelfModule { module: String },
    DependencyModule { alias: String, module: String },
    LegacyLocal { module: String },
    LegacyExternal { source: String },
}

#[must_use]
pub fn logical_import(import: &ImportDecl) -> Option<LogicalImport> {
    match import {
        ImportDecl::ModuleAlias { path, .. }
        | ImportDecl::Named { path, .. }
        | ImportDecl::ReExport { path, .. } => {
            let mut segments = path.iter().map(ToString::to_string);
            let first = segments.next()?;
            let rest = segments.collect::<Vec<_>>().join("/");
            match first.as_str() {
                "std" => Some(LogicalImport::Std { module: rest }),
                "self" if !rest.is_empty() => Some(LogicalImport::SelfModule { module: rest }),
                _ if rest.is_empty() => Some(LogicalImport::LegacyLocal { module: first }),
                _ => Some(LogicalImport::DependencyModule {
                    alias: first,
                    module: rest,
                }),
            }
        }
        ImportDecl::ModuleGroup { prefix, .. } => {
            let mut segments = prefix.iter().map(ToString::to_string);
            let first = segments.next()?;
            let rest = segments.collect::<Vec<_>>().join("/");
            match first.as_str() {
                "std" => Some(LogicalImport::Std { module: rest }),
                "self" => Some(LogicalImport::SelfModule { module: rest }),
                _ => Some(LogicalImport::DependencyModule {
                    alias: first,
                    module: rest,
                }),
            }
        }
        ImportDecl::ExternalAlias { source, .. } | ImportDecl::ExternalNamed { source, .. } => {
            if let Some(module) = source.strip_prefix("std.") {
                Some(LogicalImport::Std {
                    module: module.replace('.', "/"),
                })
            } else {
                Some(LogicalImport::LegacyExternal {
                    source: source.to_string(),
                })
            }
        }
    }
}

/// Canonical on-disk / registry key for an import, if any.
///
/// Returns keys understood by [`arandu_middle::db::SourceDatabase::resolve_module_path`]:
/// - `import foo.bar` → `foo/bar.aru` (package-local when `foo` is `Arandu.toml` name)
/// - `import my_app.util` → `my_app/util.aru` (PROMOTE-L2 dual root)
/// - `import std.core.mem as mem` (path form) → `stdlib/core/mem.aru`
/// - `import "std.core.mem" as mem` → `stdlib/core/mem.aru`
/// - `import std.io as io` / `import "std.io" as io` → `stdlib/std/io.aru` (SL_S)
/// - `import "other/path.aru" as x` → `other/path.aru` (opaque external key)
///
/// Prelude modules (`io`, `err`) still produce keys like `io.aru`; callers may
/// short-circuit when the file is missing and the module is a builtin prelude.
///
/// Package vs stdlib is **not** chosen here — only the key shape. Dual-root
/// mapping lives in `DatabaseImpl::resolve_module_path` + `ModuleRoots`.
#[must_use]
pub fn canonicalize_import_path(import: &ImportDecl) -> Option<String> {
    match import {
        ImportDecl::ModuleAlias { path, .. }
        | ImportDecl::Named { path, .. }
        | ImportDecl::ReExport { path, .. } => canonicalize_dotted_path(path),
        ImportDecl::ModuleGroup { .. } => None,
        ImportDecl::ExternalAlias { source, .. } | ImportDecl::ExternalNamed { source, .. } => {
            if let Some(stripped) = source.strip_prefix("std.core.") {
                Some(format!("stdlib/core/{stripped}.aru"))
            } else if let Some(stripped) = source.strip_prefix("std.alloc.") {
                Some(format!("stdlib/alloc/{stripped}.aru"))
            } else if let Some(stripped) = source.strip_prefix("std.math.") {
                Some(format!("stdlib/math/{stripped}.aru"))
            } else if let Some(stripped) = source.strip_prefix("std.") {
                // SL_S: `import "std.io" as io` → `stdlib/std/io.aru`
                Some(format!("stdlib/std/{}.aru", stripped.replace('.', "/")))
            } else {
                // Opaque external / project path as registered in the DB.
                Some(source.to_string())
            }
        }
    }
}

/// Canonical paths and logical module names for standard library modules providing
/// inherent primitive methods available in all scopes without explicit imports.
pub const AMBIENT_PRIMITIVE_MODULES: &[(&str, &str)] = &[
    ("stdlib/core/str.aru", "std.core.str"),
    ("stdlib/core/slice.aru", "std.core.slice"),
    ("stdlib/core/char.aru", "std.core.char"),
    ("stdlib/core/num.aru", "std.core.num"),
    ("stdlib/core/pointer.aru", "std.core.pointer"),
    ("stdlib/core/option.aru", "std.core.option"),
    ("stdlib/core/result.aru", "std.core.result"),
];

/// The module defining the owning closure types behind `own (A) -> R`
/// (RFC 0032, L3). Its symbols are linked into every module outside the
/// standard library, like a prelude, but no name enters the scope: only the
/// compiler reaches them, through their lang items.
pub const AMBIENT_CLOSURE_MODULE: (&str, &str) = ("stdlib/alloc/closure.aru", "std.alloc.closure");

/// The module defining the owned `String` and `str.toString()`, exposed in the
/// layered `alloc` prelude outside `std.*`.
pub const AMBIENT_STRING_MODULE: (&str, &str) = ("stdlib/alloc/string.aru", "std.alloc.string");

/// The module defining `Vec<T>`, exposed in the layered `alloc` prelude outside `std.*`.
pub const AMBIENT_VEC_MODULE: (&str, &str) = ("stdlib/alloc/vec.aru", "std.alloc.vec");

/// The module defining `Box<T>`, exposed in the layered `alloc` prelude outside `std.*`.
pub const AMBIENT_BOX_MODULE: (&str, &str) = ("stdlib/alloc/boxed.aru", "std.alloc.boxed");

/// The module defining `Range<T>` and `RangeInclusive<T>`, exposed in the
/// prelude scope for `a..b` / `a..=b` and named references.
pub const AMBIENT_RANGE_MODULE: (&str, &str) = ("stdlib/core/range.aru", "std.core.range");

/// Core prelude modules whose foundational traits/enums (`Ordering`, `Iterator`,
/// `Copy`, `Clone`, `Send`, `Sync`) enter the prelude scope outside `std.core.*`.
pub const AMBIENT_CORE_PRELUDE_MODULES: &[(&str, &str)] = &[
    ("stdlib/core/cmp.aru", "std.core.cmp"),
    ("stdlib/core/iter.aru", "std.core.iter"),
    ("stdlib/core/marker.aru", "std.core.marker"),
    ("stdlib/core/hash.aru", "std.core.hash"),
    ("stdlib/core/fmt.aru", "std.core.fmt"),
];

/// The modules linked into `current` without an import, in a fixed order.
/// Core modules see no primitive/core-prelude modules and the standard library
/// sees no alloc/closure prelude modules, which keeps both free of import cycles.
pub fn ambient_modules(
    current: Option<&str>,
) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
    let core = current.is_some_and(|module| module.starts_with("std.core."));
    let standard = current.is_some_and(|module| module.starts_with("std."));
    AMBIENT_PRIMITIVE_MODULES
        .iter()
        .copied()
        .filter(move |_| !core)
        .chain(
            AMBIENT_CORE_PRELUDE_MODULES
                .iter()
                .copied()
                .filter(move |_| !core),
        )
        .chain(std::iter::once(AMBIENT_CLOSURE_MODULE).filter(move |_| !standard))
        .chain(std::iter::once(AMBIENT_STRING_MODULE).filter(move |_| !standard))
        .chain(std::iter::once(AMBIENT_VEC_MODULE).filter(move |_| !standard))
        .chain(std::iter::once(AMBIENT_BOX_MODULE).filter(move |_| !standard))
        // `std.core.range` imports `std.core.cmp.Ord`; keep `cmp` free of an ambient cycle.
        .chain(
            std::iter::once(AMBIENT_RANGE_MODULE).filter(move |_| current != Some("std.core.cmp")),
        )
        .filter(move |&(_, name)| current != Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_lexer::Span;
    use smol_str::SmolStr;

    fn span() -> Span {
        Span::new(0, 0, 0)
    }

    #[test]
    fn module_alias_relative() {
        let import = ImportDecl::ModuleAlias {
            span: span(),
            path: vec![SmolStr::new("mod_b")].into(),
            alias: SmolStr::new("mod_b"),
        };
        assert_eq!(
            canonicalize_import_path(&import).as_deref(),
            Some("mod_b.aru")
        );
    }

    #[test]
    fn module_alias_std_core() {
        let import = ImportDecl::ModuleAlias {
            span: span(),
            path: vec![
                SmolStr::new("std"),
                SmolStr::new("core"),
                SmolStr::new("mem"),
            ]
            .into(),
            alias: SmolStr::new("mem"),
        };
        // path joined with / → std/core/mem → stdlib rewrite
        assert_eq!(
            canonicalize_import_path(&import).as_deref(),
            Some("stdlib/core/mem.aru")
        );
    }

    #[test]
    fn external_std_core() {
        let import = ImportDecl::ExternalAlias {
            span: span(),
            source: SmolStr::new("std.core.option"),
            alias: SmolStr::new("option"),
        };
        assert_eq!(
            canonicalize_import_path(&import).as_deref(),
            Some("stdlib/core/option.aru")
        );
    }

    #[test]
    fn module_alias_std_io_sls() {
        let import = ImportDecl::ModuleAlias {
            span: span(),
            path: vec![SmolStr::new("std"), SmolStr::new("io")].into(),
            alias: SmolStr::new("io"),
        };
        assert_eq!(
            canonicalize_import_path(&import).as_deref(),
            Some("stdlib/std/io.aru")
        );
    }

    #[test]
    fn external_std_io_sls() {
        let import = ImportDecl::ExternalAlias {
            span: span(),
            source: SmolStr::new("std.io"),
            alias: SmolStr::new("io"),
        };
        assert_eq!(
            canonicalize_import_path(&import).as_deref(),
            Some("stdlib/std/io.aru")
        );
    }

    #[test]
    fn external_opaque() {
        let import = ImportDecl::ExternalAlias {
            span: span(),
            source: SmolStr::new("vendor/lib.aru"),
            alias: SmolStr::new("lib"),
        };
        assert_eq!(
            canonicalize_import_path(&import).as_deref(),
            Some("vendor/lib.aru")
        );
    }

    #[test]
    fn package_local_my_app_util_key() {
        let import = ImportDecl::ModuleAlias {
            span: span(),
            path: vec![SmolStr::new("my_app"), SmolStr::new("util")].into(),
            alias: SmolStr::new("util"),
        };
        assert_eq!(
            canonicalize_import_path(&import).as_deref(),
            Some("my_app/util.aru")
        );
    }

    #[test]
    fn logical_roots_do_not_embed_transport_or_filesystem_policy() {
        let self_import = ImportDecl::ModuleAlias {
            span: span(),
            path: vec![
                SmolStr::new("self"),
                SmolStr::new("geometry"),
                SmolStr::new("vector"),
            ]
            .into(),
            alias: SmolStr::new("vector"),
        };
        assert_eq!(
            logical_import(&self_import),
            Some(LogicalImport::SelfModule {
                module: "geometry/vector".into()
            })
        );

        let dependency = ImportDecl::ModuleAlias {
            span: span(),
            path: vec![SmolStr::new("math"), SmolStr::new("geometry")].into(),
            alias: SmolStr::new("geometry"),
        };
        assert_eq!(
            logical_import(&dependency),
            Some(LogicalImport::DependencyModule {
                alias: "math".into(),
                module: "geometry".into()
            })
        );
    }
}
