//! Lint levels of a declaration: `@Suppress("lint")`, `@Deny("lint")` and
//! `@Forbid("lint")` on declarations. A lint is named by its `snake_case` name
//! (`"unhandled_result"`) or its code (`"W006"`), and `"warnings"` names them all.
//!
//! They apply to the warnings reported under their scope, with Rust's precedence:
//! `@Forbid` and `@Deny` make the warning an error, `@Suppress` removes it,
//! and a lint that is forbidden cannot be suppressed on the same
//! declaration. A warning reported outside a body (an unused import) has no
//! declaration to carry a level.

use crate::{DiagCode, Diagnostic};
use arandu_middle::Severity;
use arandu_parser::{Attribute, ExprId, ExprKind, StringPart};

use crate::type_checker::TypeChecker;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Suppress,
    Deny,
    Forbid,
}

/// What a level annotation names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lint {
    /// `"warnings"`: every warning.
    All,
    One(DiagCode),
}

impl Lint {
    fn covers(self, code: DiagCode) -> bool {
        match self {
            Self::All => true,
            Self::One(lint) => lint == code,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::All => "warnings",
            Self::One(lint) => lint.lint_name().unwrap_or_else(|| lint.as_str()),
        }
    }
}

fn parse_arg_lint(checker: &TypeChecker<'_>, arg: ExprId) -> Option<Lint> {
    let ExprKind::InterpolatedString { parts } = checker.pool.expr(arg) else {
        return None;
    };
    let [part] = checker.pool.string_part_list(*parts) else {
        return None;
    };
    let StringPart::Text { text, .. } = checker.pool.string_part(*part) else {
        return None;
    };
    if text == "warnings" {
        return Some(Lint::All);
    }
    DiagCode::from_lint_name_or_code(text.as_str()).map(Lint::One)
}

/// Applies the levels declared in `attrs` to the diagnostics reported from index `from` on.
pub(crate) fn apply(checker: &mut TypeChecker<'_>, attrs: &[Attribute], from: usize) {
    let mut levels: Vec<(Lint, Level, &Attribute)> = Vec::new();
    let mut invalid = Vec::new();
    for attr in attrs {
        let level = match attr.name.as_str() {
            "Suppress" => Level::Suppress,
            "Deny" => Level::Deny,
            "Forbid" => Level::Forbid,
            _ => continue,
        };
        if attr.args.is_empty() {
            invalid.push(
                Diagnostic::error(
                    DiagCode::N014InvalidAnnotationArguments,
                    format!("annotation '@{}' expects the name of a warning", attr.name),
                    attr.span,
                )
                .with_hint(
                    "name a lint as in `@Suppress(\"unhandled_result\")`, by its code (`\"W006\"`), or use `\"warnings\"` for all",
                ),
            );
            continue;
        }

        let mut attr_valid = true;
        let mut parsed_lints = Vec::with_capacity(attr.args.len());
        for &arg in &attr.args {
            match parse_arg_lint(checker, arg) {
                Some(lint) => parsed_lints.push(lint),
                None => {
                    attr_valid = false;
                    break;
                }
            }
        }

        if attr_valid {
            for lint in parsed_lints {
                levels.push((lint, level, attr));
            }
        } else {
            invalid.push(
                Diagnostic::error(
                    DiagCode::N014InvalidAnnotationArguments,
                    format!("annotation '@{}' expects the name of a warning", attr.name),
                    attr.span,
                )
                .with_hint(
                    "name a lint as in `@Suppress(\"unhandled_result\")`, by its code (`\"W006\"`), or use `\"warnings\"` for all",
                ),
            );
        }
    }
    // A forbidden lint cannot be suppressed.
    for &(lint, level, attr) in &levels {
        if level == Level::Suppress
            && let Some((_, _, forbid)) = levels.iter().find(|(other, level, _)| {
                *level == Level::Forbid && (*other == lint || *other == Lint::All)
            })
        {
            invalid.push(
                Diagnostic::error(
                    DiagCode::N014InvalidAnnotationArguments,
                    format!("'{}' is forbidden and cannot be suppressed", lint.name()),
                    attr.span,
                )
                .with_label(forbid.span, "forbidden here"),
            );
        }
    }
    if !levels.is_empty() {
        let level_of = |code: DiagCode| {
            levels
                .iter()
                .filter(|(lint, _, _)| lint.covers(code))
                .map(|(_, level, _)| *level)
                .max()
        };
        let body: Vec<Diagnostic> = checker.diagnostics.drain(from..).collect();
        for mut diagnostic in body {
            if diagnostic.severity != Severity::Warning {
                checker.diagnostics.push(diagnostic);
                continue;
            }
            match level_of(diagnostic.code) {
                Some(Level::Suppress) => {}
                Some(level @ (Level::Deny | Level::Forbid)) => {
                    diagnostic.severity = Severity::Error;
                    let annotation = if level == Level::Forbid {
                        "Forbid"
                    } else {
                        "Deny"
                    };
                    checker.diagnostics.push(diagnostic.with_note(format!(
                        "this warning is an error here: the declaration has '@{annotation}'"
                    )));
                }
                None => checker.diagnostics.push(diagnostic),
            }
        }
    }
    checker.diagnostics.extend(invalid);
}
