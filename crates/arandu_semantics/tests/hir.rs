#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use arandu_semantics::hir::*;
use arandu_semantics::passes::type_checker::types::{Primitive, TypeInterner};
use arandu_semantics::{SymbolTable, lower_to_hir, resolve_for_test, type_check};

fn lower(src: &str) -> (HirProgram, SymbolTable) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "type check errors: {:?}",
        tc.diagnostics
    );
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    hir.validate_invariants(&hir.pool, &tc.symbols)
        .expect("HIR invariant validation failed");
    (hir, std::sync::Arc::unwrap_or_clone(tc.symbols))
}

fn int_ty() -> arandu_middle::types::TypeId {
    TypeInterner::preinterned_primitive(Primitive::Int)
}

#[test]
fn canonical_and_legacy_no_fallback_lower_to_the_same_hir_flag() {
    for annotation in ["NoFallback", "no_fallback", "no_generational_fallback"] {
        let source = format!("@{annotation}\nfunc critical() {{}}\n");
        let (hir, _) = lower(&source);
        let HirDecl::Func(function) = hir.pool.decl(hir.decls[0]) else {
            panic!("expected function HIR");
        };
        assert!(
            function.no_fallback,
            "@{annotation} must enable no_fallback"
        );
    }
}

#[test]
fn destructor_annotation_records_an_explicit_type_contract() {
    let program = arandu_parser::parse(
        r#"
struct Resource { handle: ptr[u8] }

@Destructor
func Resource.close(own self): void {}
"#,
    )
    .expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(tc.diagnostics.is_empty(), "{:?}", tc.diagnostics);
    lower_to_hir(&mut tc, &program).expect("HIR lowering");
    assert_eq!(tc.type_info.destructors.len(), 1);
    let (&type_symbol, &destructor) = tc.type_info.destructors.iter().next().unwrap();
    assert_eq!(tc.symbols.get(type_symbol).name, "Resource");
    assert!(tc.symbols.get(destructor).name.contains("close"));
}

#[test]
fn destructor_requires_a_consuming_receiver() {
    let program = arandu_parser::parse(
        r#"
struct Resource { handle: ptr[u8] }

@Destructor
func Resource.close(mut self): void {}
"#,
    )
    .expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let errors = lower_to_hir(&mut tc, &program).expect_err("invalid destructor");
    assert_eq!(
        errors[0].code,
        arandu_semantics::DiagCode::T035InvalidDestructor
    );
}

#[test]
fn lowers_io_println_call() {
    lower(
        r#"
import io

func main() {
    io.println("done")
    io.eprint("error")
}
"#,
    );
}

#[test]
fn lowers_err_new_in_result_err() {
    lower(
        r#"
import err

func main(): Result<void, Err> {
    return Result.Err(err.new("boom"))
}
"#,
    );
}

#[test]
fn resolves_prelude_namespace_symbols() {
    let program = arandu_parser::parse(
        r#"
import err

func main() {
    _ = err.new("x")
}
"#,
    )
    .expect("parse");
    let resolution = resolve_for_test(0, &program);
    let tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let scope = tc.symbols.global_scope();
    assert!(
        tc.symbols.lookup_module(scope, "err").is_some(),
        "expected imported module `err`"
    );
    assert!(
        tc.symbols.lookup_module_member("io", "println").is_some(),
        "expected prelude member `io.println`"
    );
    assert!(
        tc.symbols.lookup_module_member("io", "eprint").is_some(),
        "expected prelude member `io.eprint`"
    );
    assert!(
        tc.symbols.lookup_module_member("err", "new").is_some(),
        "expected prelude member `err.new`"
    );
}

#[test]
fn lowers_imported_namespace_calls() {
    lower(
        r#"
import err
import io

func main() {
    err.new("boom")
    io.println("ok")
}
"#,
    );
}

// ── Struct lowering ─────────────────────────────────────────────────

#[test]
fn lowers_struct_fields_with_types() {
    let (hir, symbols) = lower(
        "struct Point {
            x: int
            y: int
        }",
    );
    assert_eq!(hir.decls.len(), 1);
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Struct(s) => {
            assert_eq!(symbol_name(&symbols, s.symbol), "Point");
            let fields = hir.pool.struct_fields_list(s.fields);
            assert_eq!(fields.len(), 2);
            assert_eq!(symbol_name(&symbols, fields[0].symbol), "x");
            assert_eq!(fields[0].ty, int_ty());
            assert_eq!(symbol_name(&symbols, fields[1].symbol), "y");
        }
        other => panic!("expected Struct, got {other:?}"),
    }
}

// ── Function lowering ───────────────────────────────────────────────

#[test]
fn lowers_function_with_params_and_return_type() {
    let (hir, symbols) = lower(
        "func add(x: int, y: int): int {
            return x + y
        }",
    );
    assert_eq!(hir.decls.len(), 1);
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Func(f) => {
            assert_eq!(symbol_name(&symbols, f.symbol), "add");
            let params = hir.pool.params_list(f.params);
            assert_eq!(params.len(), 2);
            assert_eq!(symbol_name(&symbols, params[0].symbol), "x");
            assert_eq!(symbol_name(&symbols, params[1].symbol), "y");
            // return_type is the interned return TypeId from Func(params, ret)
            assert_eq!(
                f.return_type,
                int_ty(),
                "expected int return type, got {:?}",
                f.return_type
            );
            assert!(f.body.is_some());
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            assert!(!statements.is_empty());
            match &hir.pool.stmt(statements[0]).kind {
                HirStmtKind::Return { values } => {
                    let exprs = hir.pool.expr_list(*values);
                    assert_eq!(exprs.len(), 1);
                    assert!(matches!(
                        hir.pool.expr(exprs[0]).kind,
                        HirExprKind::Binary { .. }
                    ));
                }
                other => panic!("expected Return, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Variable declarations ───────────────────────────────────────────

#[test]
fn lowers_var_decl_with_type_annotation() {
    let (hir, symbols) = lower(
        "func main() {
            let x: int = 42
        }",
    );
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Func(f) => {
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[0]).kind {
                HirStmtKind::VarDecl { bindings, value } => {
                    let bindings_slice = hir.pool.bindings_list(*bindings);
                    assert_eq!(bindings_slice.len(), 1);
                    assert_eq!(symbol_name(&symbols, bindings_slice[0].symbol), "x");
                    assert!(
                        bindings_slice[0].ty
                            != arandu_middle::types::TypeInterner::preinterned_error_id(),
                        "expected resolved type, got Error"
                    );
                    match &hir.pool.expr(*value).kind {
                        HirExprKind::Int(v) => assert_eq!(v, "42"),
                        other => panic!("expected Int, got {other:?}"),
                    }
                }
                other => panic!("expected VarDecl, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Expressions carry types ─────────────────────────────────────────

#[test]
fn expr_nodes_carry_resolved_types() {
    let (hir, _symbols) = lower(
        "func main() {
            let x: int = 10
            let y: int = x + 5
        }",
    );
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Func(f) => {
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[1]).kind {
                HirStmtKind::VarDecl { value, .. } => {
                    assert!(
                        hir.pool.expr(*value).ty
                            != arandu_middle::types::TypeInterner::preinterned_error_id(),
                        "binary expr should have a resolved type"
                    );
                }
                other => panic!("expected VarDecl, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Path expressions resolve to symbols ─────────────────────────────

#[test]
fn path_expr_resolves_symbol() {
    let (hir, _symbols) = lower(
        "func main() {
            let x: int = 10
            let y: int = x
        }",
    );
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Func(f) => {
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[1]).kind {
                HirStmtKind::VarDecl { value, .. } => match &hir.pool.expr(*value).kind {
                    HirExprKind::Path { symbol } => {
                        assert!(symbol.local_id.0 != 0, "symbol should be resolved");
                    }
                    other => panic!("expected Path, got {other:?}"),
                },
                other => panic!("expected VarDecl, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Call expressions ────────────────────────────────────────────────

#[test]
fn lowers_call_expression() {
    let (hir, _symbols) = lower(
        "func add(x: int, y: int): int {
            return x + y
        }
        func main() {
            let result: int = add(1, 2)
        }",
    );
    match hir.pool.decl(hir.decls[1]) {
        HirDecl::Func(f) => {
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[0]).kind {
                HirStmtKind::VarDecl { value, .. } => match &hir.pool.expr(*value).kind {
                    HirExprKind::Call { callee, args, .. } => {
                        assert!(matches!(
                            hir.pool.expr(*callee).kind,
                            HirExprKind::Path { .. }
                        ));
                        let args_slice = hir.pool.expr_list(*args);
                        assert_eq!(args_slice.len(), 2);
                    }
                    other => panic!("expected Call, got {other:?}"),
                },
                other => panic!("expected VarDecl, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

#[test]
fn lowers_call_with_trailing_block() {
    let (hir, _symbols) = lower(
        "func foo(a: int): int {
            return a
        }
        func main() {
            let result: int = foo(1) {
                2
            }
        }",
    );
    // find the `main` function decl
    let main_func = hir
        .decls
        .iter()
        .find_map(|&d| match hir.pool.decl(d) {
            HirDecl::Func(f) if symbol_name(&_symbols, f.symbol) == "main" => Some(f),
            _ => None,
        })
        .expect("main function not found");
    let body = hir.pool.block(main_func.body.unwrap());
    let statements = hir.pool.stmt_list(body.statements);
    match &hir.pool.stmt(statements[0]).kind {
        HirStmtKind::VarDecl { value, .. } => match &hir.pool.expr(*value).kind {
            HirExprKind::Call { trailing_block, .. } => {
                assert!(trailing_block.is_some());
                let bid = trailing_block.unwrap();
                let blk = hir.pool.block(bid);
                let block_stmts = hir.pool.stmt_list(blk.statements);
                assert!(!block_stmts.is_empty());
            }
            other => panic!("expected Call, got {other:?}"),
        },
        other => panic!("expected VarDecl, got {other:?}"),
    }
}

// ── Enum lowering ───────────────────────────────────────────────────

#[test]
fn lowers_enum_with_variants() {
    let (hir, symbols) = lower(
        "enum Color {
            Red
            Green
            Blue
        }",
    );
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Enum(e) => {
            assert_eq!(symbol_name(&symbols, e.symbol), "Color");
            let variants = hir.pool.enum_variants_list(e.variants);
            assert_eq!(variants.len(), 3);
            assert_eq!(symbol_name(&symbols, variants[0].symbol), "Color.Red");
            assert_eq!(symbol_name(&symbols, variants[1].symbol), "Color.Green");
            assert_eq!(symbol_name(&symbols, variants[2].symbol), "Color.Blue");
            assert!(variants[0].payload.is_none());
        }
        other => panic!("expected Enum, got {other:?}"),
    }
}

// ── If statement lowering ───────────────────────────────────────────

#[test]
fn lowers_if_stmt() {
    let (hir, _symbols) = lower(
        "func main() {
            let x: int = 10
            if x > 5 {
                let y: int = 1
            } else {
                let y: int = 2
            }
        }",
    );
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Func(f) => {
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[1]).kind {
                HirStmtKind::If {
                    condition,
                    then_block,
                    else_block,
                } => {
                    assert!(matches!(condition, HirCondition::Expr(_)));
                    assert!(
                        !hir.pool
                            .stmt_list(hir.pool.block(*then_block).statements)
                            .is_empty()
                    );
                    assert!(else_block.is_some());
                }
                other => panic!("expected If, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Group expressions are transparent ───────────────────────────────

#[test]
fn group_expressions_unwrap() {
    let (hir, _symbols) = lower(
        "func main() {
            let x: int = (42)
        }",
    );
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Func(f) => {
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[0]).kind {
                HirStmtKind::VarDecl { value, .. } => {
                    assert!(
                        matches!(hir.pool.expr(*value).kind, HirExprKind::Int(_)),
                        "expected Int after group unwrap, got {:?}",
                        hir.pool.expr(*value).kind
                    );
                }
                other => panic!("expected VarDecl, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Const lowering ──────────────────────────────────────────────────

#[test]
fn lowers_const_declaration() {
    let (hir, symbols) = lower("const MAX int = 100");
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Const(c) => {
            assert_eq!(symbol_name(&symbols, c.symbol), "MAX");
            // const type comes from synth_expr, which gives IntLiteral for unadorned int
            assert!(
                c.ty != arandu_middle::types::TypeInterner::preinterned_error_id(),
                "const type should be resolved, got {:?}",
                c.ty
            );
            assert!(matches!(hir.pool.expr(c.value).kind, HirExprKind::Int(_)));
        }
        other => panic!("expected Const, got {other:?}"),
    }
}

// ── Struct literal lowering ─────────────────────────────────────────

#[test]
fn lowers_struct_literal() {
    let (hir, _symbols) = lower(
        "struct Point {
            x: int
            y: int
        }
        func main() {
            let p: Point = Point { x: 1, y: 2 }
        }",
    );
    match hir.pool.decl(hir.decls[1]) {
        HirDecl::Func(f) => {
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[0]).kind {
                HirStmtKind::VarDecl { value, .. } => match &hir.pool.expr(*value).kind {
                    HirExprKind::StructLiteral {
                        struct_symbol,
                        fields,
                    } => {
                        assert!(
                            struct_symbol.local_id.0 != 0,
                            "struct symbol should be resolved"
                        );
                        let fields_slice = hir.pool.field_inits_list(*fields);
                        assert_eq!(fields_slice.len(), 2);
                        assert_eq!(fields_slice[0].name, "x");
                        assert_eq!(fields_slice[1].name, "y");
                    }
                    other => panic!("expected StructLiteral, got {other:?}"),
                },
                other => panic!("expected VarDecl, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Symbol consistency ──────────────────────────────────────────────

#[test]
fn func_param_symbols_match_usage() {
    let (hir, _symbols) = lower(
        "func identity(x: int): int {
            return x
        }",
    );
    match hir.pool.decl(hir.decls[0]) {
        HirDecl::Func(f) => {
            let params = hir.pool.params_list(f.params);
            let param_symbol = params[0].symbol;
            let body = hir.pool.block(f.body.unwrap());
            let statements = hir.pool.stmt_list(body.statements);
            match &hir.pool.stmt(statements[0]).kind {
                HirStmtKind::Return { values } => {
                    let exprs = hir.pool.expr_list(*values);
                    match &hir.pool.expr(exprs[0]).kind {
                        HirExprKind::Path { symbol } => {
                            assert_eq!(
                                *symbol, param_symbol,
                                "usage symbol should match param definition"
                            );
                        }
                        other => panic!("expected Path, got {other:?}"),
                    }
                }
                other => panic!("expected Return, got {other:?}"),
            }
        }
        other => panic!("expected Func, got {other:?}"),
    }
}

// ── Golden Tests ───────────────────────────────────────────────────

#[test]
fn test_hir_golden_files() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let root_dir = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let fixtures_dir = root_dir.join("tests").join("hir");

    if !fixtures_dir.exists() {
        return;
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&fixtures_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "aru") {
            entries.push(path);
        }
    }

    entries.sort();

    for path in entries {
        let name = path.file_stem().unwrap().to_str().unwrap();
        // Golden spans use LF byte offsets on every platform, including
        // pre-existing Windows worktrees whose fixtures may still be CRLF.
        let src = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");

        let program = arandu_parser::parse(&src).unwrap_or_else(|err| {
            panic!("failed to parse {name}: {err:?}");
        });
        let resolution = resolve_for_test(0, &program);
        let mut tc = type_check(
            resolution,
            &program,
            arandu_semantics::TargetInfo { pointer_width: 64 },
        );
        assert!(
            tc.diagnostics.is_empty(),
            "type check failed for {}: {:?}",
            name,
            tc.diagnostics
        );
        let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
        hir.validate_invariants(&hir.pool, &tc.symbols)
            .expect("HIR invariant validation failed");
        let ctx = HirPrettyCtx {
            pool: &hir.pool,
            symbols: &tc.symbols,
            show_spans: false,
            type_interner: Some(&tc.type_info.type_interner),
        };
        let pretty = hir.pretty_print(&ctx);

        arandu_test_support::assert_golden_text("hir", name, "hir", &pretty);
    }
}

#[test]
fn test_type_aliases() {
    lower(
        r#"
        struct Box {
            val: int
        }
        type MyBox = Box
        type NestedBox = MyBox

        func takes_box(b: Box): int {
            return b.val
        }

        func main(): int {
            let x: MyBox = Box { val: 42 }
            let y: NestedBox = x
            return takes_box(y)
        }
        "#,
    );
}

#[test]
fn test_cyclic_type_alias_safety() {
    let src = r#"
        type X = Y
        type Y = X
        func main() {}
    "#;
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    // Since X = Y = X is cyclic, it should result in a compiler error or poison, but not crash!
    assert!(!tc.diagnostics.is_empty() || tc.type_info.expr_types.iter().all(|s| s.is_none()));
}
