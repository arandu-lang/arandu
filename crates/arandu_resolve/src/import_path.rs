//! Single source of truth for mapping import AST nodes → module file keys.
//!
//! Used by both name resolution and the Salsa query layer so path rewriting
//! cannot drift (RC-PATH-TRIPLE).

use arandu_parser::ImportDecl;

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
        ImportDecl::ModuleAlias { path, .. } | ImportDecl::Named { path, .. } => {
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
        ImportDecl::ModuleAlias { path, .. } | ImportDecl::Named { path, .. } => {
            let path_str = path.join("/");
            if let Some(stripped) = path_str.strip_prefix("std/core/") {
                Some(format!("stdlib/core/{stripped}.aru"))
            } else if let Some(stripped) = path_str.strip_prefix("std/alloc/") {
                Some(format!("stdlib/alloc/{stripped}.aru"))
            } else if let Some(stripped) = path_str.strip_prefix("std/math/") {
                Some(format!("stdlib/math/{stripped}.aru"))
            } else if let Some(stripped) = path_str.strip_prefix("std/") {
                // SL_S thin: `import std.io as io` → `stdlib/std/io.aru`
                Some(format!("stdlib/std/{stripped}.aru"))
            } else {
                Some(format!("{path_str}.aru"))
            }
        }
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
