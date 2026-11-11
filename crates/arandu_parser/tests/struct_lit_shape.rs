#![allow(clippy::expect_used, clippy::unwrap_used)]
use arandu_parser::ast_pool::ExprKind;
use arandu_parser::{lower_syntax_to_program, parse_syntax_arc};
use std::sync::Arc;

fn shape(src: &str) -> String {
    let tree = parse_syntax_arc(Arc::from(src));
    let prog = lower_syntax_to_program(&tree, 0).expect("parse");
    let mut out = String::new();
    use arandu_parser::TopLevelDecl;
    for d in &prog.decls {
        if let TopLevelDecl::Func(f) = prog.pool.decl(*d) {
            for &sid in prog.pool.stmt_list(f.body.statements) {
                if let arandu_parser::Stmt::VarDecl { value, .. } = prog.pool.stmt(sid) {
                    out.push_str(&fmt_expr(&prog, *value));
                }
            }
        }
    }
    out
}

fn fmt_expr(prog: &arandu_parser::Program, e: arandu_parser::ast_pool::ExprId) -> String {
    match prog.pool.expr(e) {
        ExprKind::StructLiteral { fields, .. } => {
            format!(
                "StructLiteral(nfields={})",
                prog.pool.field_init_list(*fields).len()
            )
        }
        ExprKind::Call { callee, args } => {
            let trailing = prog
                .pool
                .expr_list(*args)
                .last()
                .is_some_and(|&arg| matches!(prog.pool.expr(arg), ExprKind::Lambda { .. }));
            format!("Call(trailing={trailing}, cal={})", fmt_expr(prog, *callee))
        }
        ExprKind::Field { base, field } => {
            format!("Field({}, .{})", fmt_expr(prog, *base), field)
        }
        ExprKind::Path { path } => format!("Path({:?})", path),
        other => format!("{:?}", other),
    }
}

#[test]
fn shapes() {
    let empty = shape("func main(): int {\n    let b = lib.Empty {}\n    return 1\n}");
    let fielded = shape("func main(): int {\n    let b = lib.Fielded { x: 1 }\n    return 1\n}");
    let local = shape("func main(): int {\n    let b = Empty {}\n    return 1\n}");
    assert_eq!(empty, "StructLiteral(nfields=0)");
    assert_eq!(fielded, "StructLiteral(nfields=1)");
    assert_eq!(local, "StructLiteral(nfields=0)");
}

#[test]
fn variant_sugar_shape() {
    let tree = parse_syntax_arc(Arc::from("func f(): Option<int> {\n    return .None\n}"));
    let prog = lower_syntax_to_program(&tree, 0).expect("parse");
    use arandu_parser::TopLevelDecl;
    let mut found = false;
    for d in &prog.decls {
        if let TopLevelDecl::Func(f) = prog.pool.decl(*d) {
            for &sid in prog.pool.stmt_list(f.body.statements) {
                if let arandu_parser::Stmt::Return { values, .. } = prog.pool.stmt(sid)
                    && let Some(&e) = values.first()
                {
                    assert!(
                        matches!(
                            prog.pool.expr(e),
                            ExprKind::VariantSugar { name, .. } if name == "None"
                        ),
                        "got {:?}",
                        prog.pool.expr(e)
                    );
                    found = true;
                }
            }
        }
    }
    assert!(found, "return .None not found");
}
