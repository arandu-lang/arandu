use crate::ParseErrorCode;
use crate::{parse, parse_recovering, parse_to_string};

#[test]
fn annotation_name_span_excludes_at_and_arguments_in_both_lowering_paths() {
    let source = "@Link(\"m\")\nextern \"C\" {}\n";
    let direct = parse(source).expect("direct parse");
    let tree = crate::parse_syntax_arc(std::sync::Arc::<str>::from(source));
    let cst = crate::lower_syntax_to_program(&tree, 0).expect("CST lowering");
    for program in [&direct, &cst] {
        let crate::TopLevelDecl::Extern(decl) = program.pool.decl(program.decls[0]) else {
            panic!("expected extern declaration");
        };
        let attr = &decl.attrs[0];
        assert_eq!((attr.name_span.start, attr.name_span.end), (1, 5));
        assert_eq!(
            &source[attr.name_span.start as usize..attr.name_span.end as usize],
            "Link"
        );
        assert_eq!(
            &source[attr.span.start as usize..attr.span.end as usize],
            "@Link(\"m\")"
        );
    }
}

fn strip_spans(s: &str) -> String {
    // Remove @line:col-line:col annotations for easier assertion
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '@' {
            // skip @digits:digits-digits:digits
            while let Some(&n) = chars.peek() {
                if n.is_ascii_digit() || n == ':' || n == '-' {
                    chars.next();
                } else {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn parse_empty_program() {
    let result = parse_to_string("").unwrap();
    assert!(result.starts_with("Program"));
}

#[test]
fn parse_module_only() {
    let result = parse_to_string("module mymod").unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("Module"));
    assert!(s.contains("mymod"));
}

#[test]
fn parse_func_no_params_void_return() {
    let result = parse_to_string("module test\nfunc main() {\n    return\n}\n").unwrap();
    let s = strip_spans(&result);
    // Debug: eprintln!("DUMP: {result}");
    // Debug: eprintln!("STRIPPED: {s}");
    assert!(s.contains("Func main") || s.contains("main"));
    assert!(s.contains("Return") || s.contains("return"));
}

#[test]
fn parse_func_with_params_and_return() {
    let source = "module test\nfunc add(a: int, b: int): int {\n    return a + b\n}\n";
    let program = parse(source).unwrap();
    let crate::TopLevelDecl::Func(function) = program.pool.decl(program.decls[0]) else {
        panic!("expected function declaration");
    };
    let crate::FuncName::Free { name, .. } = &function.name else {
        panic!("expected free function");
    };
    assert_eq!(name.as_str(), "add");
    assert_eq!(function.params.len(), 2);
    assert_eq!(function.params[0].name.as_str(), "a");
    assert_eq!(function.params[1].name.as_str(), "b");
    for parameter in &function.params {
        assert!(matches!(
            program.pool.type_expr(parameter.ty),
            crate::TypeExpr::Primitive { name, .. } if name == "int"
        ));
    }
    let Some(crate::ResultType::Single { ty, .. }) = &function.result else {
        panic!("expected single int return type");
    };
    assert!(matches!(
        program.pool.type_expr(*ty),
        crate::TypeExpr::Primitive { name, .. } if name == "int"
    ));
    let statements = program.pool.stmt_list(function.body.statements);
    assert!(matches!(
        statements
            .first()
            .map(|statement| program.pool.stmt(*statement)),
        Some(crate::Stmt::Return { .. })
    ));
}

#[test]
fn parse_int_literal() {
    let result = parse_to_string("module test\nfunc main() {\n    let x = 42\n}\n").unwrap();
    assert!(result.contains("42"));
}

#[test]
fn parse_float_literal() {
    let result = parse_to_string("module test\nfunc main() {\n    let x = 3.14\n}\n").unwrap();
    assert!(result.contains("3.14"));
}

#[test]
fn parse_bool_literal() {
    let result =
        parse_to_string("module test\nfunc main() {\n    let a = true\n    let b = false\n}\n")
            .unwrap();
    assert!(result.contains("true"));
    assert!(result.contains("false"));
}

#[test]
fn parse_string_literal() {
    let result = parse_to_string("module test\nfunc main() {\n    let s = \"hello\"\n}\n").unwrap();
    assert!(result.contains("hello"));
}

#[test]
fn parse_nil() {
    let result = parse_to_string("module test\nfunc main() {\n    let x = nil\n}\n").unwrap();
    assert!(result.contains("nil") || result.contains("Nil"));
}

#[test]
fn parse_binary_ops() {
    let result = parse_to_string("module test\nfunc main() {\n    let x = 1 + 2 * 3\n}\n").unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("+") || s.contains("*"));
}

#[test]
fn parse_if_else() {
    let source = "module test\nfunc main() {\n    if true {\n        return 1\n    } else {\n        return 2\n    }\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("If"));
    assert!(s.contains("Else"));
}

#[test]
fn parse_while() {
    let source = "module test\nfunc main() {\n    while true {\n        break\n    }\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("While"));
    assert!(s.contains("Break"));
}

#[test]
fn parse_for_in() {
    let source =
        "module test\nfunc main() {\n    for item in items {\n        io.println(item)\n    }\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("For") || s.contains("In"));
}

#[test]
fn parse_struct_decl() {
    let source = "module test\nstruct Point {\n    x: int\n    y: int\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("Struct") && s.contains("Point"));
    // Note: stripped spans leave double spaces: "Field  x", "Type  int"
    assert!(s.contains("Field  x"));
    assert!(s.contains("Field  y"));
    assert!(s.contains("Type  int"));
}

#[test]
fn parse_enum_decl() {
    let source = "module test\nenum Option {\n    Some(int)\n    None\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("Enum") && s.contains("Option"));
    assert!(s.contains("Some"));
    assert!(s.contains("None"));
}

#[test]
fn parse_extern_decl() {
    let source = "module test\nextern \"C\" {\n    func puts(s: ptr[u8]): int\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("Extern"));
}

#[test]
fn parse_type_alias() {
    let source = "module test\ntype MyInt = int\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("MyInt"));
}

#[test]
fn disambiguates_grouped_and_function_types() {
    let source = concat!(
        "module test\n",
        "type Grouped = (int)\n",
        "type Callback = (int, str) -> bool\n",
        "type Factory = () -> (int)\n",
        "type Higher = ((int) -> str, bool) -> int\n",
    );
    let result = parse_to_string(source).unwrap();
    let stripped = strip_spans(&result);
    assert!(stripped.contains("GroupType"), "{stripped}");
    assert!(stripped.contains("FuncType"), "{stripped}");

    assert!(
        parse_to_string("module test\ntype NotATuple = (int, str)\n").is_err(),
        "comma-separated parenthesized types require a function arrow"
    );
}

#[test]
fn parse_interface_decl() {
    let source = "module test\ninterface Printable {\n    func print(): void\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("Interface") || s.contains("Printable"));
}

#[test]
fn parse_match_int() {
    let source = "module test\nfunc main() {\n    match x {\n        1 => \"one\"\n        _ => \"other\"\n    }\n}\n";
    let result = parse_to_string(source).unwrap();
    assert!(result.contains("Match") || result.contains("Arm"));
}

#[test]
fn parse_import() {
    let source = "module test\nimport io\nfunc main() { }\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("Import"));
}

#[test]
fn parse_var_decl_with_type() {
    let source = "module test\nfunc main() {\n    let x: int = 42\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("x: int") || (s.contains("x") && s.contains("int")));
}

#[test]
fn parse_unary_minus() {
    let source = "module test\nfunc main() {\n    let x = -5\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("-"));
}

#[test]
fn parse_call() {
    let source = "module test\nfunc main() {\n    io.println(\"hi\")\n}\n";
    let result = parse_to_string(source).unwrap();
    assert!(result.contains("Call") || result.contains("println"));
}

#[test]
fn parse_generic_func() {
    let source = "module test\nfunc identity<T>(x: T): T {\n    return x\n}\n";
    let result = parse_to_string(source).unwrap();
    assert!(result.contains("identity") || result.contains("T"));
}

#[test]
fn parse_multi_module_func() {
    let source = "module test\nfunc foo() { }\nfunc bar() { }\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.contains("Func foo") || s.contains("foo"));
    assert!(s.contains("Func bar") || s.contains("bar"));
}

#[test]
fn parse_rejects_missing_rbrace() {
    let err = parse("module test\nfunc main() {\n    return\n").unwrap_err();
    assert!(err.code == ParseErrorCode::ExpectedToken);
}

#[test]
fn parse_recovery_continues_after_error() {
    let output = parse_recovering("module test\nfunc main() {\n    let x = \n    return x\n}\n");
    assert!(!output.diagnostics.is_empty());
    assert!(!output.program.decls.is_empty());
}

#[test]
fn parse_nested_if() {
    let source = "module test\nfunc main() {\n    if a {\n        if b {\n            return 1\n        }\n    }\n}\n";
    let result = parse_to_string(source).unwrap();
    let s = strip_spans(&result);
    assert!(s.matches("If").count() >= 2);
}

#[test]
#[rustfmt::skip]
fn recovery_suppresses_cascade_but_not_distinct_errors() {
    let output = parse_recovering(&"module test
        func main() {
            let x = 
            let good_stmt_1 = 1;
            let good_stmt_2 = 2;
            let y = 
            return x
        }
    ".replace("\r\n", "\n"));
    // Only two syntax errors should be reported, one for x and one for y.
    assert_eq!(
        output.diagnostics.len(),
        2,
        "Should report exactly 2 distinct syntax errors"
    );
}

#[test]
fn distinct_errors_near_each_other_are_not_suppressed() {
    // Two genuinely distinct errors (`let 1` and `let 3`) separated only by a
    // semicolon boundary. The recovery synchronizer advances over the broken
    // region via `advance_raw`, which must still prime the suppression window so
    // the second distinct error is reported rather than swallowed as a cascade.
    let output = parse_recovering("module test\nfunc main() {\n    let 1 = 2; let 3 = 4;\n}\n");
    assert_eq!(
        output.diagnostics.len(),
        2,
        "Two distinct `let <number>` errors must both be reported: {:?}",
        output
            .diagnostics
            .iter()
            .map(|d| format!("{:?}", d.code))
            .collect::<Vec<_>>()
    );
}

#[test]
#[rustfmt::skip]
fn recovery_does_not_leak_past_closing_brace() {
    let output = parse_recovering(&"module test
        func bad() {
            let x = 
        } // RBrace should sync and not leak error state
        
        func good() {
            return 1
        }
    ".replace("\r\n", "\n"));
    let dump = output.program.dump("");
    assert!(dump.contains("good"));
}

#[test]
fn recovery_synchronizes_on_impl_block() {
    let output = parse_recovering(
        "module test\n\
         func bad(\n\
         impl Foo {\n\
             func method(): i64 { return 42; }\n\
         }\n",
    );
    let dump = output.program.dump("");
    assert!(dump.contains("Foo"), "dump must contain impl Foo: {dump}");
    assert!(dump.contains("method"), "dump must contain method: {dump}");
    assert!(
        !output.diagnostics.is_empty(),
        "must have error from func bad("
    );
}

#[test]
fn recovery_synchronizes_on_let_and_var_stmt() {
    let output = parse_recovering(
        "module test\n\
         func main() {\n\
             let 1 = 2\n\
             let valid = 10\n\
             let mut another = 20\n\
             return valid + another\n\
         }\n",
    );
    let dump = output.program.dump("");
    assert!(
        dump.contains("valid"),
        "dump must contain valid variable: {dump}"
    );
    assert!(
        dump.contains("another"),
        "dump must contain another variable: {dump}"
    );
    assert!(
        !output.diagnostics.is_empty(),
        "must report error on let 1 = 2"
    );
}

// ── AstPool unit tests ────────────────────────────────────────────────────────

#[test]
fn ast_pool_alloc_expr_increments_id_and_vectors() {
    use crate::{AstPool, ExprKind};
    use arandu_lexer::Span;

    let mut pool = AstPool::new();
    let span = Span {
        file_id: 0,
        start: 0,
        end: 0,
    };

    // First allocation: id index == 0
    let id0 = pool.alloc_expr(ExprKind::Nil, span);
    assert_eq!(id0.as_usize(), 0);
    assert_eq!(pool.exprs.len(), 1);
    assert_eq!(pool.expr_spans.len(), 1);

    // Second allocation: id index == 1
    let id1 = pool.alloc_expr(ExprKind::Bool { value: true }, span);
    assert_eq!(id1.as_usize(), 1);
    assert_eq!(pool.exprs.len(), 2);
    assert_eq!(pool.expr_spans.len(), 2);

    // Values can be retrieved through the lookup helpers
    assert_eq!(*pool.expr(id0), ExprKind::Nil);
    assert_eq!(pool.expr_span(id0), span);
    assert_eq!(*pool.expr(id1), ExprKind::Bool { value: true });
}

#[test]
fn ast_pool_alloc_stmt_increments_id_and_vectors() {
    use crate::{AstPool, Stmt};
    use arandu_lexer::Span;

    let mut pool = AstPool::new();
    let span = Span {
        file_id: 0,
        start: 0,
        end: 0,
    };

    // First allocation: id index == 0
    let id0 = pool.alloc_stmt(Stmt::Break { span });
    assert_eq!(id0.as_usize(), 0);
    assert_eq!(pool.stmts.len(), 1);
    assert_eq!(pool.stmt_spans.len(), 1);

    // Second allocation: id index == 1
    let id1 = pool.alloc_stmt(Stmt::Continue { span });
    assert_eq!(id1.as_usize(), 1);
    assert_eq!(pool.stmts.len(), 2);
    assert_eq!(pool.stmt_spans.len(), 2);

    // Values and spans can be retrieved through lookup helpers
    assert!(matches!(*pool.stmt(id0), Stmt::Break { .. }));
    assert_eq!(pool.stmt_span(id0), span);
    assert!(matches!(*pool.stmt(id1), Stmt::Continue { .. }));
}

// ── Program::dump ─────────────────────────────────────────────────────────────

#[test]
fn program_dump_contains_structural_keywords() {
    let source = "module test\nfunc main() {\n    return 0\n}\n";
    let program = parse(source).unwrap();
    let dump = program.dump(source);
    // The dump should at least mention the program root and the function name
    assert!(
        dump.contains("Program"),
        "expected 'Program' in dump, got:\n{dump}"
    );
    assert!(
        dump.contains("main"),
        "expected 'main' in dump, got:\n{dump}"
    );
}

#[test]
fn program_dump_empty_source_starts_with_program() {
    let program = parse("").unwrap();
    let dump = program.dump("");
    assert!(
        dump.starts_with("Program"),
        "dump of empty source should start with 'Program'"
    );
}

// ── parse_recovering_with_file_id ─────────────────────────────────────────────

#[test]
fn parse_recovering_with_file_id_propagates_to_program_span() {
    use crate::parse_recovering_with_file_id;

    // Valid source: no diagnostics, program span carries file_id
    let output = parse_recovering_with_file_id("module test\nfunc main() { }\n", 42);
    assert!(
        output.diagnostics.is_empty(),
        "valid source should produce no diagnostics"
    );
    assert_eq!(
        output.program.span.file_id, 42,
        "program span should carry the supplied file_id"
    );
}

#[test]
fn parse_recovering_with_file_id_error_carries_file_id() {
    use crate::parse_recovering_with_file_id;

    // Invalid source: diagnostics must carry the correct file_id in their span
    let output = parse_recovering_with_file_id("module test\nfunc main() {\n    let x =\n}\n", 99);
    assert!(
        !output.diagnostics.is_empty(),
        "invalid source should produce at least one diagnostic"
    );
    for diag in &output.diagnostics {
        assert_eq!(
            diag.span.file_id, 99,
            "error span should carry file_id=99, got {}",
            diag.span.file_id
        );
    }
}

#[test]
fn deeply_nested_expression_does_not_stack_overflow_and_recovers() {
    let mut source = String::from("func main() {\n    let x = ");
    for _ in 0..115 {
        source.push('(');
    }
    source.push_str("42");
    for _ in 0..115 {
        source.push(')');
    }
    source.push_str("\n}\n");

    let tree = crate::syntax::parse_syntax(&source);
    let output = crate::syntax::lower_syntax_to_program_recovering(&tree, 0);
    assert!(!output.diagnostics.is_empty());
}
