//! Presentation of already-frozen CTFE values. No evaluation or UI dependency.

use arandu_middle::{NodeKey, Span, ctfe::ConstValue};
use arandu_parser::{ExprKind, Program, TypeExpr};
use arandu_semantics::TypeCheckResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComptimePresentation {
    pub span: Span,
    pub values: Vec<String>,
}

/// The smallest staged source construct under the cursor. Failed/discarded
/// obligations have no value and never produce a guessed runtime fallback.
#[must_use]
pub fn value_at(
    program: &Program,
    tc: &TypeCheckResult,
    offset: u32,
) -> Option<ComptimePresentation> {
    let argument = program.pool.type_exprs.iter().filter_map(|ty| {
        let TypeExpr::ConstExpression { span, .. } = ty else {
            return None;
        };
        contains(*span, offset).then_some(())?;
        argument_value(tc, *span).map(|value| (*span, value))
    });
    let roots = program
        .pool
        .exprs
        .iter()
        .zip(&program.pool.expr_spans)
        .filter_map(|(kind, &span)| {
            if !matches!(kind, ExprKind::Comptime { .. }) || !contains(span, offset) {
                return None;
            }
            let (value, _) = tc.type_info.ctfe_values.get(&span)?;
            let value = present_value(value);
            Some((span, value))
        });
    argument
        .chain(roots)
        .min_by_key(|(span, _)| (span.end.saturating_sub(span.start), span.start))
        .map(|(span, value)| ComptimePresentation {
            span,
            values: vec![value],
        })
}

fn present_value(value: &ConstValue) -> String {
    match value {
        ConstValue::Integer(value) => value.value().to_string(),
        ConstValue::Bool(value) => value.to_string(),
        ConstValue::Void => "void".to_owned(),
        ConstValue::Float(value) => value.display(),
        ConstValue::String(value) => {
            let mut result = String::from("\"");
            for character in value.as_str().chars().take(80) {
                result.extend(character.escape_debug());
            }
            if value.as_str().chars().nth(80).is_some() {
                result.push('…');
            }
            result.push('"');
            result
        }
        ConstValue::Bytes(value) => format!("{} immutable bytes", value.len()),
        ConstValue::Aggregate(value) => format!("aggregate with {} elements", value.values().len()),
    }
}

fn argument_value(tc: &TypeCheckResult, span: Span) -> Option<String> {
    let key = NodeKey::from(span);
    if let Some(value) = tc.resolved.typed_comptime_arguments.get(&key) {
        return Some(present_value(value));
    }
    tc.resolved
        .comptime_arguments
        .get(&key)?
        .map(|value| value.to_string())
}

/// Values belonging to this generic callee, in argument order. Use the AST
/// relationship, not a byte window that can accidentally select a nearby call.
#[must_use]
pub fn arguments_at(
    program: &Program,
    tc: &TypeCheckResult,
    offset: u32,
) -> Option<ComptimePresentation> {
    let (span, args) = program
        .pool
        .exprs
        .iter()
        .filter_map(|kind| {
            let ExprKind::Generic { callee, args } = kind else {
                return None;
            };
            let span = program.pool.expr_span(*callee);
            contains(span, offset).then_some((span, *args))
        })
        .min_by_key(|(span, _)| (span.end.saturating_sub(span.start), span.start))?;
    let mut values = Vec::new();
    for &id in program.pool.type_expr_list(args) {
        if let TypeExpr::ConstExpression { span, .. } = program.pool.type_expr(id) {
            // One failed argument makes the instantiated call unavailable.
            values.push(argument_value(tc, *span)?);
        }
    }
    (!values.is_empty()).then_some(ComptimePresentation { span, values })
}

fn contains(span: Span, offset: u32) -> bool {
    span.start <= offset && offset < span.end
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use arandu_query::{AnalysisHost, passes};

    fn at(text: &str, needle: &str, argument: bool) -> Option<ComptimePresentation> {
        let mut host = AnalysisHost::new();
        let file = host.new_file("hover.aru".into(), text.into());
        let snap = host.snapshot();
        let program = passes::parse(&snap.db, file);
        let checked = passes::type_check(&snap.db, file);
        let offset =
            u32::try_from(text.rfind(needle).expect("cursor marker")).expect("source offset");
        let program = (**program).as_ref().expect("valid syntax");
        if argument {
            arguments_at(program, checked, offset)
        } else {
            value_at(program, checked, offset)
        }
    }

    #[test]
    fn values_belong_to_exact_callees_not_a_byte_window() {
        let text = format!(
            "func count<comptime N: uint>(): uint {{ return N }}\nfunc pair<comptime N: uint, comptime M: uint>(): uint {{ return N + M }}\nfunc plain(): uint {{ return 0 }}\nfunc main(): uint {{ let a = count<comptime (11)>() + count<comptime (22)>(); let b = pair<comptime (7), comptime (9)>(); plain(); return count<{}comptime (20 + 22)>() }}",
            " ".repeat(90)
        );
        assert_eq!(
            at(&text, "count<comptime (11)", true)
                .expect("first call")
                .values,
            ["11"]
        );
        assert_eq!(
            at(&text, "count<comptime (22)", true)
                .expect("second call")
                .values,
            ["22"]
        );
        assert_eq!(
            at(&text, "pair<comptime (7)", true)
                .expect("multiple arguments")
                .values,
            ["7", "9"]
        );
        assert_eq!(
            at(&text, "count< ", true)
                .expect("long generic call")
                .values,
            ["42"]
        );
        assert!(at(&text, "plain();", true).is_none());
        let value = at(&text, "comptime (20 + 22)", false).expect("direct argument");
        assert_eq!(value.values, ["42"]);
        let start = usize::try_from(value.span.start).expect("start");
        let end = usize::try_from(value.span.end).expect("end");
        assert_eq!(&text[start..end], "comptime (20 + 22)");
    }

    #[test]
    fn failed_and_discarded_arguments_have_no_hover_value() {
        let text = "func count<comptime N: uint>(): uint { return N }\nfunc main(): uint { comptime if false { return count<comptime (99)>() }; return count<comptime (1 / 0)>() }";
        for needle in ["comptime (99)", "comptime (1 / 0)"] {
            assert!(at(text, needle, false).is_none());
        }
        assert!(at(text, "count<comptime (1 / 0)", true).is_none());
    }

    #[test]
    fn typed_arguments_present_bool_and_signed_values_on_arguments_and_callees() {
        let text = "func choose<comptime ENABLED: bool, comptime N: i64>(): i64 { if ENABLED { return N }; return 0 }\nfunc main(): i64 { return choose<comptime (true), comptime (-42)>() }";
        assert_eq!(
            at(text, "choose<comptime", true)
                .expect("typed call")
                .values,
            ["true", "-42"]
        );
        assert_eq!(
            at(text, "comptime (true)", false)
                .expect("bool argument")
                .values,
            ["true"]
        );
        assert_eq!(
            at(text, "comptime (-42)", false)
                .expect("signed argument")
                .values,
            ["-42"]
        );
        let failed = text.replace("-42", "1 / 0");
        assert!(at(&failed, "choose<comptime", true).is_none());
        assert!(at(&failed, "comptime (1 / 0)", false).is_none());
    }

    #[test]
    fn scalar_roots_present_bool_void_and_full_unsigned_range() {
        let text = "func main(): void { let x: u64 = comptime 18446744073709551615; let yes = comptime true; comptime {} }";
        assert_eq!(
            at(text, "comptime 184467", false).expect("u64").values,
            ["18446744073709551615"]
        );
        assert_eq!(
            at(text, "comptime true", false).expect("bool").values,
            ["true"]
        );
        assert_eq!(
            at(text, "comptime {}", false).expect("void").values,
            ["void"]
        );
    }

    #[test]
    fn source_arena_changes_never_reuse_an_old_hover_value() {
        let mut host = AnalysisHost::new();
        let source =
            "func sibling(): int { return 0 + 0 }\nfunc main(): int { return comptime 42 }";
        let file = host.new_file("hover.aru".into(), source.into());
        for text in [
            source.to_owned(),
            source.replace("0 + 0", "    0"),
            source.replace("42", "43"),
        ] {
            host.set_text(file, text.clone());
            let snap = host.snapshot();
            let program = passes::parse(&snap.db, file);
            let checked = passes::type_check(&snap.db, file);
            let offset = u32::try_from(text.find("comptime").expect("root")).expect("offset");
            let hover = value_at((**program).as_ref().expect("syntax"), checked, offset)
                .expect("current root value");
            let expected = if text.contains("43") { "43" } else { "42" };
            assert_eq!(hover.values, [expected], "{text}");
        }
    }
}
