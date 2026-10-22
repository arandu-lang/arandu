//! Internal CTFE -> typed HIR -> ordinary runtime/backend proofs. Only the
//! selected literal is substituted; no public grammar or alternate emitter.
use super::*;
use arandu_middle::{
    amir::{AmirProgram, AmirStmt},
    hir::{HirDecl, HirExprKind, HirStmtKind},
    types::Primitive,
    DataLayout,
};
use arandu_query::{
    ctfe::{ctfe_eval_root, CtfeRoot, CtfeRootRequest, RootExpectedType, RootSelector},
    passes::declaration_signatures,
    DatabaseImpl,
};

#[test]
fn public_comptime_uses_the_same_residual_in_all_backends_and_optimization_levels() {
    let cases = [
        ("int", "20 + 22", "return folded"),
        ("bool", "3 < 4", "if folded { return 42 }; return 1"),
        (
            "i64",
            "-9223372036854775808",
            "if folded < -9223372036854775807 { return 42 }; return 1",
        ),
        (
            "u64",
            "18446744073709551615",
            "if folded == 18446744073709551615 { return 42 }; return 1",
        ),
        ("usize", "42", "return folded as int"),
        ("i8", "-128", "if folded < 0 { return 42 }; return 1"),
    ];
    let mut sources = cases.iter().map(|(ty, value, body)| format!(
        "func helper(): {ty} {{ return {value} }}\nfunc main(): int {{ let folded: {ty} = comptime helper()\n{body}\n}}"
    )).collect::<Vec<_>>();
    sources.extend([
        "func main(): int { let folded = comptime { let mut sum = 0; let mut i = 0; while i < 7 { set sum = sum + 6; set i = i + 1 }; return sum }; return folded }".into(),
        "func main(): int { let folded: int = comptime { if true { return 42 } else { return 1 } }; return folded }".into(),
        "func main(): int { comptime { return; }; return 42 }".into(),
        "func table(): [2]int { return [20, 22] }\nfunc main(): int { let a = comptime table(); return a[0] + a[1] }".into(),
        "func pair(): (int, bool) { return 42, true }\nfunc main(): int { let a, enabled = comptime pair(); if enabled { return a }; return 0 }".into(),
        "struct Point { x: int y: int }\nfunc make(): Point { return Point { y: 22, x: 20 } }\nfunc main(): int { let point = comptime make(); return point.x + point.y }".into(),
        "func grid(): [2][2]int { return [[1, 2], [20, 22]] }\nfunc main(): int { let a = comptime grid(); return a[1][0] + a[1][1] }".into(),
        "func main(): int { let a = comptime [20, 22]; return a[0] + a[1] }".into(),
        "func main(): int { let a = comptime { let mut a: [2]int = [2, 3]; set a[1] = 40; return a }; return a[0] + a[1] }".into(),
        "module std.core.frozen\nextern \"arandu-intrinsic\" { func strBytes(source: str): []u8\nfunc sliceSubslice<T>(source: []T, start: usize, len: usize): []T }\nfunc main(): int { let bytes = comptime { let all = strBytes(\"Olá\\0🦀\"); unsafe { return sliceSubslice<u8>(all, 3, 2) } }; if bytes[0] != 161 || bytes[1] != 0 { return 1 }; return 42 }".into(),
        "func text(): str { return \"Olá\" }\nfunc main(): int { let a: f64 = comptime (0.1 + 0.2); let z: f64 = comptime (-0.0); let n: f64 = comptime (0.0 / 0.0); let s = comptime text(); if a != 0.30000000000000004 || 1.0 / z >= 0.0 || n == n || s != \"Olá\" { return 1 }; return 42 }".into(),
        "func main(): int { let rounded: f32 = comptime 16777217.0; let subnormal: f32 = comptime 1.401298464324817e-45; let negative: f32 = comptime (-0.0); let unordered: f32 = comptime (0.0 / 0.0); if rounded != 16777216.0 || subnormal == 0.0 || 1.0 / negative >= 0.0 || unordered == unordered { return 1 }; return 42 }".into(),
        "func main(): int { let value: f32 = comptime 42.0; let answer = -(-(value * 2.0 / 2.0 + 1.0 - 1.0)); if answer < 42.0 || answer > 42.0 || answer <= 41.0 || answer >= 43.0 { return 1 }; return 42 }".into(),
        "func main(): int { let frozen: f32 = comptime 42.0; let wider = frozen as f64; let narrower = (wider + 0.0) as f32; let direct = (comptime 42.0) as f32; if wider != 42.0 || narrower != 42.0 || direct != 42.0 { return 1 }; return 42 }".into(),
        r#"module std.core.frozen
extern "arandu-intrinsic" { func strBytes(source: str): []u8 }
func main(): int {
    let ordinary = strBytes("A\n\t\r\0\u{1F980}\\\"\$")
    let frozen = comptime strBytes("A\n\t\r\0\u{1F980}\\\"\$")
    if frozen[0] != 65 || frozen[1] != 10 || frozen[2] != 9 || frozen[3] != 13 || frozen[4] != 0 || frozen[5] != 240 || frozen[9] != 92 || frozen[10] != 34 || frozen[11] != 36 { return 1 }
    if ordinary[1] != 10 || ordinary[4] != 0 || ordinary[5] != 240 || ordinary[9] != 92 || ordinary[10] != 34 || ordinary[11] != 36 { return 2 }
    return 42
}"#.into(),
    ]);
    for source in sources {
        for (layout, wasm) in [
            (DataLayout::host(), false),
            (DataLayout::ptr_width(4), true),
        ] {
            let mut db = DatabaseImpl::new();
            db.set_target_config(layout);
            let file = db.new_stdlib_file("stdlib/core/residual.aru".into(), source.clone());
            let artifacts = arandu_query::passes::lower_amir(&db, file);
            assert!(
                artifacts
                    .type_check
                    .diagnostics
                    .iter()
                    .all(|d| d.severity != arandu_middle::Severity::Error),
                "{source}: {:?}",
                artifacts.type_check.diagnostics
            );
            let tc = &artifacts.type_check;
            if source.contains("sliceSubslice<u8>(all, 3, 2)") {
                let bytes = tc
                    .type_info
                    .ctfe_values
                    .values()
                    .find_map(|(value, _)| {
                        if let arandu_middle::ctfe::ConstValue::Bytes(bytes) = value {
                            Some(bytes.as_bytes())
                        } else {
                            None
                        }
                    })
                    .expect("frozen byte root");
                assert_eq!(
                    bytes,
                    &[161, 0],
                    "frozen byte view before residual emission"
                );
            }
            // Independent owned optimization inputs, not a production clone.
            let mut original = artifacts.amir.clone();
            let main = original
                .funcs
                .iter()
                .find(|f| tc.symbols.get(f.symbol).name == "main")
                .expect("main")
                .symbol;
            original.funcs.retain(|f| f.symbol == main);
            original.debug_bindings.retain(|b| b.function == main);
            original.debug_blocks.retain(|b| b.function == main);
            assert!(
                original.funcs[0]
                    .stmts
                    .iter_ids()
                    .all(|id| !matches!(original.funcs[0].stmt(id), AmirStmt::Call { .. })),
                "{source}"
            );
            for level in [
                arandu_mir::OptLevel::O0,
                arandu_mir::OptLevel::O1,
                arandu_mir::OptLevel::O2,
            ] {
                let mut amir = original.clone();
                arandu_mir::optimize_amir_checked_with_level(
                    &mut amir,
                    &tc.symbols,
                    &tc.type_info.type_interner,
                    level,
                )
                .expect("optimization");
                let result = if wasm {
                    execute_wasm(&amir, &tc.symbols, &tc.type_info).expect("Wasm")
                } else {
                    let native =
                        execute_cranelift(&amir, &tc.symbols, &tc.type_info).expect("Cranelift");
                    assert_eq!(
                        native,
                        execute_c(&amir, &tc.symbols, &tc.type_info).expect("C"),
                        "{source}, {level:?}"
                    );
                    native
                };
                assert_eq!(result.result, 42, "{source}, {layout:?}, {level:?}");
            }
        }
    }
}

fn residual(
    source: &str,
    primitive: Primitive,
    layout: DataLayout,
) -> (AmirProgram, arandu_semantics::TypeCheckResult) {
    let mut db = DatabaseImpl::new();
    db.set_target_config(layout);
    let file = db.new_file("residual.aru".into(), source.into());
    let main = declaration_signatures(&db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "main")
        .expect("main")
        .id;
    let root = CtfeRoot::new(
        &db,
        file,
        main,
        RootSelector::Initializer {
            block: vec![],
            statement: 0,
        },
        Some(RootExpectedType::Primitive(primitive)),
    );
    let value = ctfe_eval_root(
        &db,
        CtfeRootRequest::new(
            &db,
            root,
            arandu_mir::ctfe::Budget {
                fuel: 100_000,
                frames: 64,
                values: 10_000,
            },
        ),
    )
    .as_ref()
    .expect("CTFE value")
    .clone();

    // The residual proof goes through the ordinary checker/lowerers. Syntax
    // integration will eventually supply this replacement before HIR lowering;
    // this test-only selection does not pretend to implement that staging.
    let program = arandu_parser::parse(source).expect("canonical parse");
    let resolution = arandu_semantics::resolve_for_test(0, &program);
    let mut tc = arandu_semantics::type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo {
            pointer_width: u8::try_from(layout.pointer_width() * 8).expect("pointer bits"),
        },
    );
    assert!(tc.diagnostics.is_empty(), "{:?}", tc.diagnostics);
    let mut hir = arandu_semantics::lower_to_hir(&mut tc, &program).expect("HIR");
    let function = hir
        .decls
        .iter()
        .find_map(|&id| match hir.pool.decl(id) {
            HirDecl::Func(function) if tc.symbols.get(function.symbol).name == "main" => {
                Some(function)
            }
            _ => None,
        })
        .expect("main HIR");
    let main = function.symbol;
    let body = hir.pool.block(function.body.expect("main body"));
    let first = hir.pool.stmt_list(body.statements)[0];
    let HirStmtKind::VarDecl {
        value: destination, ..
    } = hir.pool.stmt(first).kind
    else {
        panic!("initializer")
    };
    let destination_expr = hir.pool.expr(destination);
    assert!(
        matches!(destination_expr.kind, HirExprKind::Call { .. }),
        "the original root is a call, not a literal"
    );
    let literal = arandu_semantics::materialize_ctfe_scalar(
        value,
        destination_expr.ty,
        &tc.type_info.type_interner,
        layout,
        destination_expr.span,
    )
    .expect("typed residual literal");
    *hir.pool.expr_mut(destination) = literal;
    let (mut amir, _) =
        arandu_semantics::lower_to_amir_with_interfaces(&mut tc, &hir, layout.pointer_width())
            .expect("ordinary ownership/AMIR pipeline");
    // Publishing only main makes a retained helper call fail instead of
    // accidentally passing by computing the same value again at runtime.
    amir.funcs.retain(|function| function.symbol == main);
    amir.debug_bindings
        .retain(|binding| binding.function == main);
    amir.debug_blocks.retain(|block| block.function == main);
    assert_eq!(amir.funcs.len(), 1);
    assert!(amir.funcs[0]
        .stmts
        .iter_ids()
        .all(|id| !matches!(amir.funcs[0].stmt(id), AmirStmt::Call { .. })));
    assert!(arandu_middle::amir_validate::validate_amir_program(
        &amir,
        &tc.symbols,
        &tc.type_info.type_interner
    )
    .is_empty());
    (amir, tc)
}

#[test]
fn ctfe_values_replace_calls_without_losing_bits_in_c_cranelift_or_wasm() {
    let cases = [
        (Primitive::Int, "int", "20 + 22", "return folded", 42),
        (
            Primitive::Bool,
            "bool",
            "3 < 4",
            "if folded { return 42 }\nreturn 1",
            42,
        ),
        (
            Primitive::U64,
            "u64",
            "18446744073709551615",
            "if folded == 18446744073709551615 { return 42 }\nreturn 1",
            42,
        ),
        (
            Primitive::I64,
            "i64",
            "-9223372036854775808",
            "if folded < -9223372036854775807 { return 42 }\nreturn 1",
            42,
        ),
        (
            Primitive::I8,
            "i8",
            "-128",
            "if folded < 0 { return 42 }\nreturn 1",
            42,
        ),
        (
            Primitive::U32,
            "u32",
            "4294967295",
            "if folded > 2147483647 { return 42 }\nreturn 1",
            42,
        ),
        (Primitive::USize, "usize", "42", "return folded as int", 42),
    ];
    for (primitive, type_name, expression, runtime, expected) in cases {
        let source = format!(
            "func helper(): {type_name} {{ return {expression} }}\nfunc main(): int {{ let folded: {type_name} = helper()\n{runtime}\n}}"
        );
        for (layout, wasm) in [
            (DataLayout::host(), false),
            (DataLayout::ptr_width(4), true),
        ] {
            let (original, tc) = residual(&source, primitive, layout);
            for level in [
                arandu_mir::OptLevel::O0,
                arandu_mir::OptLevel::O1,
                arandu_mir::OptLevel::O2,
            ] {
                // Each optimization test intentionally owns one mutable program;
                // this is not a query hot-path clone.
                let mut amir = original.clone();
                arandu_mir::optimize_amir_checked_with_level(
                    &mut amir,
                    &tc.symbols,
                    &tc.type_info.type_interner,
                    level,
                )
                .expect("optimization");
                let observation = if wasm {
                    execute_wasm(&amir, &tc.symbols, &tc.type_info).expect("WASM residual")
                } else {
                    let cranelift = execute_cranelift(&amir, &tc.symbols, &tc.type_info)
                        .expect("Cranelift residual");
                    let c = execute_c(&amir, &tc.symbols, &tc.type_info).expect("C residual");
                    assert_eq!(cranelift, c, "{type_name}, {level:?}");
                    cranelift
                };
                assert_eq!(
                    observation.result, expected,
                    "{type_name}, {level:?}, {layout:?}"
                );
                assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
            }
        }
    }
}

#[test]
fn saved_wasm_reproducer_runs_without_the_rust_harness() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    let file = db.new_file(
        "reproducer.aru".into(),
        "func main(): int { return 42 }".into(),
    );
    let artifacts = arandu_query::passes::lower_amir(&db, file);
    let tc = &artifacts.type_check;
    let bytes = arandu_backend_wasm::emit_wasm(
        &artifacts.amir,
        &tc.symbols,
        &tc.type_info.type_interner,
        tc.type_info.as_ref(),
        DataLayout::ptr_width(4),
    )
    .expect("emit replay fixture");
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let wasm_path =
        std::env::temp_dir().join(format!("arandu_replay_{}_{}.wasm", std::process::id(), id));
    let _cleanup = TemporaryArtifacts(vec![wasm_path.clone()]);
    std::fs::write(&wasm_path, bytes).expect("write replay fixture");
    let runner = r#"(async()=>{
const fs=require('node:fs');
const {instance}=await WebAssembly.instantiate(fs.readFileSync(process.argv[1]),{});
fs.writeFileSync(process.env.ARANDU_SMITH_RESULT_PATH,JSON.stringify({result:instance.exports.main(),args:process.argv.slice(2)}));
})().catch(error=>{console.error(error);process.exitCode=1})"#;
    let directory = preserve_wasm_reproducer(&wasm_path, runner, "main").expect("preserve replay");
    let node = std::env::var_os("ARANDU_NODE").unwrap_or_else(|| "node".into());
    let mut command = Command::new(node);
    command
        .current_dir(&directory)
        .args(["runner.js", "program.wasm"])
        .env_remove("ARANDU_SMITH_RESULT_PATH")
        .env_remove("ARANDU_SMITH_PHASE_PATH");
    let output =
        run_with_timeout(&mut command, BACKEND_PROCESS_TIMEOUT).expect("run independent replay");
    assert!(output.status.success(), "{}", captured_stderr(&output));
    let result: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.join("result.txt")).expect("replay result"),
    )
    .expect("parse replay result");
    assert_eq!(result["result"], 42);
    assert_eq!(result["args"], serde_json::json!(SYNTHESIZED_PROGRAM_ARGS));
    std::fs::remove_dir_all(directory).expect("remove owned replay fixture");
}
