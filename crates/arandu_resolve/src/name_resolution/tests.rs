use arandu_lexer::Span;
use smallvec::SmallVec;
use smol_str::SmolStr;

use crate::{DiagCode, NodeKey, ResolvedNames, ScopeId, SymbolId, SymbolKind, SymbolTable};

use super::Resolver;
use super::util::is_type_case;

fn dummy_span() -> Span {
    Span::new(0, 0, 0)
}

fn new_pool() -> arandu_parser::ast_pool::AstPool {
    arandu_parser::ast_pool::AstPool::new()
}

fn make_resolver(pool: &arandu_parser::ast_pool::AstPool) -> Resolver<'_> {
    Resolver {
        symbols: SymbolTable::new(0),
        resolved: ResolvedNames::default(),
        docs: crate::DocCommentMap::default(),
        diagnostics: Vec::new(),
        pool,
        import_aliases: rustc_hash::FxHashMap::default(),
        failed_import_aliases: rustc_hash::FxHashSet::default(),
        current_module: None,
        imported_symbols: rustc_hash::FxHashMap::default(),
        used_symbols: rustc_hash::FxHashSet::default(),
    }
}

fn resolver_no_pool() -> Resolver<'static> {
    // Only used for tests that don't touch the pool
    let pool = Box::new(arandu_parser::ast_pool::AstPool::new());
    make_resolver(Box::leak(pool))
}

fn dummy_expr() -> arandu_parser::Expr {
    arandu_parser::Expr::new(0)
}

fn dummy_block() -> arandu_parser::Block {
    arandu_parser::Block {
        span: dummy_span(),
        statements: arandu_parser::IndexRange::empty(),
    }
}

fn dummy_type_name(name: &str) -> arandu_parser::TypeName {
    arandu_parser::TypeName {
        span: dummy_span(),
        path: vec![SmolStr::new(name)].into(),
    }
}

// ── is_type_case ──

#[test]
fn is_type_case_upper() {
    assert!(is_type_case("Int"));
    assert!(is_type_case("String"));
    assert!(is_type_case("MyType"));
}

#[test]
fn is_type_case_lower() {
    assert!(!is_type_case("int"));
    assert!(!is_type_case("x"));
    assert!(!is_type_case("my_var"));
}

// ── define ──

#[test]
fn define_new_symbol() {
    let mut r = resolver_no_pool();
    let sym = r.define(ScopeId(0), "x", SymbolKind::Local, dummy_span());
    assert!(sym.is_some());
    assert_eq!(r.symbols.get(sym.unwrap()).name, "x");
}

#[test]
fn define_duplicate_in_same_scope_returns_none() {
    let mut r = resolver_no_pool();
    let _ = r.define(ScopeId(0), "x", SymbolKind::Local, dummy_span());
    let dup = r.define(ScopeId(0), "x", SymbolKind::Local, dummy_span());
    assert!(dup.is_none());
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.diagnostics[0].code, DiagCode::N003RedefinedName);
}

/// Methods restate receiver type params (`func Vec.push<T, A>(…)` after
/// `import_receiver_type_params`). Must reuse SymbolIds, not emit N003.
#[test]
fn method_restated_type_params_reuse_existing_bindings() {
    let source = r#"
module t
struct BoxG<T> {
    v: T
}
func BoxG.get<T>(shared self): T {
    return self.v
}
"#;
    let program = arandu_parser::parse(source).expect("parse");
    let r = crate::resolve_for_test(0, &program);
    let n003: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::N003RedefinedName)
        .collect();
    assert!(
        n003.is_empty(),
        "restating receiver type params must not redefine: {n003:?}"
    );
}

#[test]
fn define_duplicate_module_returns_previous() {
    let mut r = resolver_no_pool();
    let a = r.define(ScopeId(0), "mymod", SymbolKind::Module, dummy_span());
    let b = r.define(ScopeId(0), "mymod", SymbolKind::Module, dummy_span());
    assert_eq!(a, b);
    assert!(r.diagnostics.is_empty());
}

// ── is_namespace ──

#[test]
fn is_namespace_for_module() {
    let mut r = resolver_no_pool();
    r.define(ScopeId(0), "io", SymbolKind::Module, dummy_span());
    assert!(r.is_namespace(ScopeId(0), "io"));
    assert!(!r.is_namespace(ScopeId(0), "nonexistent"));
}

#[test]
fn is_namespace_for_import_value() {
    let mut r = resolver_no_pool();
    r.define(ScopeId(0), "fmt", SymbolKind::ImportValue, dummy_span());
    assert!(r.is_namespace(ScopeId(0), "fmt"));
}

// ── expand_namespace_alias ──

#[test]
fn expand_namespace_no_alias() {
    let r = resolver_no_pool();
    assert_eq!(r.expand_namespace_alias("io.println"), "io.println");
}

#[test]
fn expand_namespace_with_alias() {
    let mut r = resolver_no_pool();
    r.import_aliases
        .insert("fmt".into(), "std.core.format".into());
    assert_eq!(
        r.expand_namespace_alias("fmt.println"),
        "std.core.format.println"
    );
}

#[test]
fn expand_namespace_alias_only() {
    let mut r = resolver_no_pool();
    r.import_aliases.insert("io".into(), "std.core.io".into());
    assert_eq!(r.expand_namespace_alias("io"), "std.core.io");
}

// ── suggest_from ──

#[test]
fn suggest_from_exact_match() {
    let r = resolver_no_pool();
    let syms = vec![crate::Symbol {
        id: SymbolId::new(0, 0),
        name: "println".into(),
        kind: SymbolKind::Func,
        span: dummy_span(),
        scope: ScopeId(0),
        is_public: true,
        lang_item: None,
    }];
    assert_eq!(
        r.suggest_from("println", &syms),
        Some("println".to_string())
    );
}

#[test]
fn suggest_from_levenshtein() {
    let r = resolver_no_pool();
    let syms = vec![crate::Symbol {
        id: SymbolId::new(0, 0),
        name: "println".into(),
        kind: SymbolKind::Func,
        span: dummy_span(),
        scope: ScopeId(0),
        is_public: true,
        lang_item: None,
    }];
    assert_eq!(r.suggest_from("prntln", &syms), Some("println".to_string()));
}

#[test]
fn suggest_from_no_match() {
    let r = resolver_no_pool();
    let syms = vec![crate::Symbol {
        id: SymbolId::new(0, 0),
        name: "println".into(),
        kind: SymbolKind::Func,
        span: dummy_span(),
        scope: ScopeId(0),
        is_public: true,
        lang_item: None,
    }];
    assert_eq!(r.suggest_from("abcdef", &syms), None);
}

#[test]
fn suggest_from_case_insensitive() {
    let r = resolver_no_pool();
    let syms = vec![crate::Symbol {
        id: SymbolId::new(0, 0),
        name: "Println".into(),
        kind: SymbolKind::Func,
        span: dummy_span(),
        scope: ScopeId(0),
        is_public: true,
        lang_item: None,
    }];
    assert_eq!(
        r.suggest_from("println", &syms),
        Some("Println".to_string())
    );
}

// ── check_unused_imports ──

#[test]
fn unused_import_emits_warning() {
    let mut r = resolver_no_pool();
    let sym = r
        .define(ScopeId(0), "foo", SymbolKind::ImportValue, dummy_span())
        .unwrap();
    r.imported_symbols.insert(sym, ("foo".into(), dummy_span()));
    r.check_unused_imports();
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.diagnostics[0].code, DiagCode::W007UnusedImport);
}

#[test]
fn used_import_no_warning() {
    let mut r = resolver_no_pool();
    let sym = r
        .define(ScopeId(0), "foo", SymbolKind::ImportValue, dummy_span())
        .unwrap();
    r.imported_symbols.insert(sym, ("foo".into(), dummy_span()));
    r.used_symbols.insert(sym);
    r.check_unused_imports();
    assert!(r.diagnostics.is_empty());
}

// ── collect_import ──

#[test]
fn collect_import_module_defines_symbol() {
    let mut r = resolver_no_pool();
    let import = arandu_parser::ImportDecl::ModuleAlias {
        span: dummy_span(),
        path: vec![SmolStr::new("std"), SmolStr::new("io")].into(),
        alias: SmolStr::new("std"),
    };
    r.collect_import(ScopeId(0), &import);
    let sym = r.symbols.lookup_module(ScopeId(0), "std");
    assert!(sym.is_some());
}

#[test]
fn collect_import_module_empty_path_emits_error() {
    let mut r = resolver_no_pool();
    let import = arandu_parser::ImportDecl::ModuleAlias {
        span: dummy_span(),
        path: SmallVec::new(),
        alias: SmolStr::new("empty"),
    };
    r.collect_import(ScopeId(0), &import);
}

#[test]
fn collect_import_named_type_case() {
    let mut r = resolver_no_pool();
    let import = arandu_parser::ImportDecl::Named {
        span: dummy_span(),
        path: vec![SmolStr::new("std")].into(),
        items: vec![arandu_parser::ImportItem {
            span: dummy_span(),
            name: SmolStr::new("String"),
            alias: None,
        }],
    };
    r.collect_import(ScopeId(0), &import);
    let sym = r.symbols.lookup_type(ScopeId(0), "String");
    assert!(sym.is_some());
}

#[test]
fn collect_import_named_value_case() {
    let mut r = resolver_no_pool();
    let import = arandu_parser::ImportDecl::Named {
        span: dummy_span(),
        path: vec![SmolStr::new("std")].into(),
        items: vec![arandu_parser::ImportItem {
            span: dummy_span(),
            name: SmolStr::new("println"),
            alias: None,
        }],
    };
    r.collect_import(ScopeId(0), &import);
    let sym = r.symbols.lookup_value(ScopeId(0), "println");
    assert!(sym.is_some());
}

#[test]
fn collect_import_named_with_alias() {
    let mut r = resolver_no_pool();
    let import = arandu_parser::ImportDecl::Named {
        span: dummy_span(),
        path: vec![SmolStr::new("std")].into(),
        items: vec![arandu_parser::ImportItem {
            span: dummy_span(),
            name: SmolStr::new("println"),
            alias: Some(SmolStr::new("print")),
        }],
    };
    r.collect_import(ScopeId(0), &import);
    let sym = r.symbols.lookup_value(ScopeId(0), "print");
    assert!(sym.is_some());
}

#[test]
fn collect_import_external() {
    let mut r = resolver_no_pool();
    let import = arandu_parser::ImportDecl::ExternalAlias {
        span: dummy_span(),
        source: SmolStr::new("std.core.io"),
        alias: SmolStr::new("io"),
    };
    r.collect_import(ScopeId(0), &import);
    let sym = r.symbols.lookup_module(ScopeId(0), "io");
    assert!(sym.is_some());
    assert_eq!(
        r.import_aliases.get("io"),
        Some(&SmolStr::new("std.core.io"))
    );
}

// ── collect_top_level ──

#[test]
fn collect_top_level_const() {
    let mut r = resolver_no_pool();
    let decl = arandu_parser::TopLevelDecl::Const(arandu_parser::ConstDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        visibility: arandu_parser::Visibility::Private,
        name: "MAX".into(),
        ty: None,
        value: dummy_expr(),
    });
    r.collect_top_level(ScopeId(0), &decl);
    let sym = r.symbols.lookup_value(ScopeId(0), "MAX");
    assert!(sym.is_some());
}

#[test]
fn collect_top_level_type_alias() {
    let mut r = resolver_no_pool();
    let decl = arandu_parser::TopLevelDecl::TypeAlias(arandu_parser::TypeAliasDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        visibility: arandu_parser::Visibility::Private,
        name: "MyInt".into(),
        generic_params: Vec::new().into(),
        ty: arandu_parser::TypeExprId::new(0),
    });
    r.collect_top_level(ScopeId(0), &decl);
    let sym = r.symbols.lookup_type(ScopeId(0), "MyInt");
    assert!(sym.is_some());
}

#[test]
fn collect_top_level_func_free() {
    let mut r = resolver_no_pool();
    let decl = arandu_parser::TopLevelDecl::Func(arandu_parser::FuncDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        visibility: arandu_parser::Visibility::Private,
        is_async: false,
        name: arandu_parser::FuncName::Free {
            span: dummy_span(),
            name: "main".into(),
        },
        generic_params: Vec::new().into(),
        params: Vec::new(),
        result: None,
        where_clause: Vec::new().into(),
        body: dummy_block(),
    });
    r.collect_top_level(ScopeId(0), &decl);
    let sym = r.symbols.lookup_value(ScopeId(0), "main");
    assert!(sym.is_some());
}

#[test]
fn collect_top_level_method() {
    let mut r = resolver_no_pool();
    let _ = r.define(ScopeId(0), "Foo", SymbolKind::Struct, dummy_span());
    let decl = arandu_parser::TopLevelDecl::Func(arandu_parser::FuncDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        visibility: arandu_parser::Visibility::Private,
        is_async: false,
        name: arandu_parser::FuncName::Method {
            span: dummy_span(),
            receiver: dummy_type_name("Foo"),
            name: "bar".into(),
        },
        generic_params: Vec::new().into(),
        params: Vec::new(),
        result: None,
        where_clause: Vec::new().into(),
        body: dummy_block(),
    });
    r.collect_top_level(ScopeId(0), &decl);
    let foo_sym = r.symbols.lookup_type(ScopeId(0), "Foo");
    assert!(foo_sym.is_some());
    let sym = r.symbols.lookup_associated_member(foo_sym.unwrap(), "bar");
    assert!(sym.is_some());
}

#[test]
fn collect_top_level_struct() {
    let mut r = resolver_no_pool();
    let decl = arandu_parser::TopLevelDecl::Struct(arandu_parser::StructDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        visibility: arandu_parser::Visibility::Private,
        name: "Point".into(),
        generic_params: Vec::new().into(),
        where_clause: Vec::new().into(),
        fields: Vec::new(),
    });
    r.collect_top_level(ScopeId(0), &decl);
    let sym = r.symbols.lookup_type(ScopeId(0), "Point");
    assert!(sym.is_some());
}

#[test]
fn collect_top_level_enum_with_variants() {
    let mut r = resolver_no_pool();
    let decl = arandu_parser::TopLevelDecl::Enum(arandu_parser::EnumDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        visibility: arandu_parser::Visibility::Private,
        name: "Color".into(),
        generic_params: Vec::new().into(),
        where_clause: Vec::new().into(),
        variants: vec![
            arandu_parser::EnumVariant {
                span: dummy_span(),
                attrs: Vec::new().into(),
                name: "Red".into(),
                payload: None,
            },
            arandu_parser::EnumVariant {
                span: dummy_span(),
                attrs: Vec::new().into(),
                name: "Blue".into(),
                payload: None,
            },
        ],
    });
    r.collect_top_level(ScopeId(0), &decl);
    let sym = r.symbols.lookup_type(ScopeId(0), "Color");
    assert!(sym.is_some());
    let color_sym = r.symbols.lookup_type(ScopeId(0), "Color").unwrap();
    assert!(
        r.symbols
            .lookup_associated_member(color_sym, "Red")
            .is_some()
    );
    assert!(
        r.symbols
            .lookup_associated_member(color_sym, "Blue")
            .is_some()
    );
}

#[test]
fn collect_top_level_interface() {
    let mut r = resolver_no_pool();
    let decl = arandu_parser::TopLevelDecl::Interface(arandu_parser::InterfaceDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        visibility: arandu_parser::Visibility::Private,
        name: "Stringable".into(),
        generic_params: Vec::new().into(),
        where_clause: Vec::new().into(),
        members: Vec::new(),
    });
    r.collect_top_level(ScopeId(0), &decl);
    let sym = r.symbols.lookup_type(ScopeId(0), "Stringable");
    assert!(sym.is_some());
}

#[test]
fn collect_top_level_extern() {
    let mut r = resolver_no_pool();
    let decl = arandu_parser::TopLevelDecl::Extern(arandu_parser::ExternDecl {
        span: dummy_span(),
        attrs: Vec::new().into(),
        abi: "C".into(),
        members: vec![arandu_parser::FuncSignature {
            span: dummy_span(),
            attrs: Vec::new().into(),
            name: "malloc".into(),
            generic_params: Vec::new().into(),
            params: Vec::new(),
            result: None,
            where_clause: Vec::new().into(),
        }],
    });
    r.collect_top_level(ScopeId(0), &decl);
    let sym = r.symbols.lookup_value(ScopeId(0), "malloc");
    assert!(sym.is_some());
}

#[test]
fn collect_top_level_error_is_noop() {
    let mut r = resolver_no_pool();
    r.collect_top_level(
        ScopeId(0),
        &arandu_parser::TopLevelDecl::Error(dummy_span()),
    );
    assert_eq!(r.symbols.iter().count(), 0);
}

// ── resolve_value_name ──

#[test]
fn resolve_value_name_found_in_scope() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let mut r = make_resolver(&pool);
    let sym = r
        .define(ScopeId(0), "x", SymbolKind::Local, dummy_span())
        .unwrap();
    r.resolve_value_name(ScopeId(0), "x", expr, dummy_span());
    assert_eq!(r.resolved.expr_symbol(expr), Some(sym));
}

#[test]
fn resolve_value_name_undefined() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let mut r = make_resolver(&pool);
    r.resolve_value_name(ScopeId(0), "nonexistent", expr, dummy_span());
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.diagnostics[0].code, DiagCode::N001UndefinedValue);
}

#[test]
fn resolve_value_name_type_used_as_value() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let mut r = make_resolver(&pool);
    let _ = r.define(ScopeId(0), "MyType", SymbolKind::Struct, dummy_span());
    r.resolve_value_name(ScopeId(0), "MyType", expr, dummy_span());
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.diagnostics[0].code, DiagCode::N004TypeUsedAsValue);
}

#[test]
fn resolve_value_name_namespace_used_as_value() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let mut r = make_resolver(&pool);
    let _ = r.define(ScopeId(0), "io", SymbolKind::Module, dummy_span());
    r.resolve_value_name(ScopeId(0), "io", expr, dummy_span());
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.diagnostics[0].code, DiagCode::M003NamespaceUsedAsValue);
}

#[test]
fn resolve_value_name_with_current_module() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let pool2 = new_pool();
    let mut r = make_resolver(&pool2);
    r.current_module = Some("mymod".to_string());
    let _ = r
        .symbols
        .define_module_member("mymod", "foo", dummy_span())
        .unwrap();
    r.resolve_value_name(ScopeId(0), "foo", expr, dummy_span());
    let sym = r.symbols.lookup_module_member("mymod", "foo");
    assert_eq!(r.resolved.expr_symbol(expr), sym);
}

// ── resolve_assignment_target ──

#[test]
fn resolve_assignment_target_found() {
    let mut r = resolver_no_pool();
    let _ = r
        .define(ScopeId(0), "x", SymbolKind::Local, dummy_span())
        .unwrap();
    r.resolve_assignment_target(ScopeId(0), "x", dummy_span());
    assert!(
        r.resolved
            .value_refs
            .contains_key(&NodeKey::from(dummy_span()))
    );
}

#[test]
fn resolve_assignment_target_undefined() {
    let mut r = resolver_no_pool();
    r.resolve_assignment_target(ScopeId(0), "nonexistent", dummy_span());
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(
        r.diagnostics[0].code,
        DiagCode::N007UndefinedAssignmentTarget
    );
}

// ── resolve_namespace_member ──

#[test]
fn resolve_namespace_member_found() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let mut r = make_resolver(&pool);
    let _ = r.define(ScopeId(0), "io", SymbolKind::Module, dummy_span());
    let sym = r
        .symbols
        .define_module_member("io", "println", dummy_span())
        .unwrap();
    let found = r.resolve_namespace_member(ScopeId(0), "io", "println", expr, dummy_span());
    assert!(found);
    assert_eq!(r.resolved.expr_symbol(expr), Some(sym));
}

#[test]
fn resolve_namespace_member_not_namespace() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let mut r = make_resolver(&pool);
    let found = r.resolve_namespace_member(ScopeId(0), "nonexistent", "foo", expr, dummy_span());
    assert!(!found);
}

#[test]
fn resolve_namespace_member_undefined_member() {
    let mut pool = new_pool();
    let expr = pool.alloc_expr(arandu_parser::ExprKind::Nil, dummy_span());
    let mut r = make_resolver(&pool);
    let _ = r.define(ScopeId(0), "io", SymbolKind::Module, dummy_span());
    let found = r.resolve_namespace_member(ScopeId(0), "io", "nonexistent", expr, dummy_span());
    assert!(found);
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(
        r.diagnostics[0].code,
        DiagCode::M002UndefinedNamespaceMember
    );
}
