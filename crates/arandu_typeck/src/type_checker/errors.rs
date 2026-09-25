use crate::{DiagCode, Diagnostic, SymbolId, SymbolTable};

use super::TypeInfo;
use super::constraints::{Constraint, ConstraintOrigin};
use super::provenance;
use super::types::ArType;
use arandu_parser::UnaryOp;

// ── Flow-based error message generation ─────────────────────────────

/// Convert a failed constraint into a `Diagnostic` with flow-based
/// labels and active hints.
///
/// The key insight: instead of saying "expected X, found Y", we show
/// *where* X came from and *where* Y came from — the flow.
#[must_use]
pub fn constraint_to_diagnostic(
    constraint: &Constraint,
    symbols: &SymbolTable,
    type_info: &TypeInfo,
) -> Diagnostic {
    let expected_str = constraint
        .expected
        .display(symbols, &type_info.type_interner);
    let found_str = constraint.found.display(symbols, &type_info.type_interner);

    match &constraint.origin {
        ConstraintOrigin::Assignment { lhs_span, rhs_span } => {
            let flow = provenance::causal_chain(constraint);
            Diagnostic::error(
                DiagCode::T002IncompatibleAssignment,
                format!(
                    "incompatible type in assignment: expected '{expected_str}', found '{found_str}'"
                ),
                *rhs_span,
            )
            .with_label(*lhs_span, format!("type '{expected_str}' declared here"))
            .with_label(*rhs_span, format!("value has type '{found_str}'"))
            .with_note(format!(
                "flow: {} → {}",
                flow[0].description, flow[1].description
            ))
            .with_hint(format!(
                "convert with '{found_str} as {expected_str}' or change the declaration type"
            ))
        }

        ConstraintOrigin::CallArg {
            call_span,
            param_span,
            arg_span,
            arg_index,
        } => Diagnostic::error(
            DiagCode::T003IncompatibleCallArg,
            format!(
                "incompatible type in argument {}: expected '{}', found '{}'",
                arg_index + 1,
                expected_str,
                found_str,
            ),
            *call_span,
        )
        .with_label(
            *param_span,
            format!("parameter {} expects '{}'", arg_index + 1, expected_str),
        )
        .with_label(*arg_span, format!("argument has type '{found_str}'"))
        .with_note(format!(
            "flow: argument {} ({}) → parameter ({} expected)",
            arg_index + 1,
            found_str,
            expected_str,
        )),

        ConstraintOrigin::ReturnType {
            return_span,
            declared_span,
        } => Diagnostic::error(
            DiagCode::T004IncompatibleReturnType,
            format!(
                "incompatible return type: expected '{expected_str}', found '{found_str}'"
            ),
            *return_span,
        )
        .with_label(
            *declared_span,
            format!("return type '{expected_str}' declared here"),
        )
        .with_label(*return_span, format!("returns '{found_str}'"))
        .with_hint(format!(
            "convert the return value with 'as {expected_str}' or change the function's return type"
        )),

        ConstraintOrigin::IfBranches {
            then_span,
            else_span,
        } => Diagnostic::error(
            DiagCode::T007IfBranchMismatch,
            format!(
                "if branches have incompatible types: '{expected_str}' and '{found_str}'"
            ),
            *else_span,
        )
        .with_label(
            *then_span,
            format!("then branch has type '{expected_str}'"),
        )
        .with_label(*else_span, format!("else branch has type '{found_str}'"))
        .with_hint(
            "both branches must have the same type when if is used as an expression".to_string(),
        ),

        ConstraintOrigin::MatchArms {
            first_span,
            mismatch_span,
            arm_index,
        } => Diagnostic::error(
            DiagCode::T008MatchArmMismatch,
            format!(
                "match arm {} has type '{}', expected '{}'",
                arm_index + 1,
                found_str,
                expected_str,
            ),
            *mismatch_span,
        )
        .with_label(
            *first_span,
            format!("first arm has type '{expected_str}'"),
        )
        .with_label(
            *mismatch_span,
            format!("arm {} has type '{}'", arm_index + 1, found_str),
        )
        .with_hint("all match arms must have the same type".to_string()),

        ConstraintOrigin::BinaryOp {
            op_span,
            left_span,
            right_span,
        } => {
            let diag = Diagnostic::error(
                DiagCode::T005OperatorNotApplicable,
                format!(
                    "operator not applicable to '{expected_str}' and '{found_str}'",
                ),
                *op_span,
            )
            .with_label(*left_span, format!("type '{expected_str}'"))
            .with_label(*right_span, format!("type '{found_str}'"));

            add_operator_hint(diag, &constraint.expected, &constraint.found, symbols, &type_info.type_interner)
        }

        ConstraintOrigin::UnaryOp {
            op,
            op_span,
            operand_span,
        } => {
            let mut diag = Diagnostic::error(
                DiagCode::T005OperatorNotApplicable,
                format!("operator not applicable to '{found_str}'"),
                *op_span,
            )
            .with_label(*operand_span, format!("type '{found_str}'"));

            if *op == UnaryOp::Neg
                && constraint.found.is_integer()
                && !constraint.found.is_signed()
            {
                diag = diag.with_hint("unsigned integer types cannot be negated with `-`");
            }
            diag
        }

        ConstraintOrigin::Condition { span } => Diagnostic::error(
            DiagCode::T009ConditionNotBool,
            format!("condition must be 'bool', found '{found_str}'"),
            *span,
        )
        .with_label(*span, format!("type '{found_str}' is not bool"))
        .with_hint(suggest_bool_conversion(&constraint.found)),

        ConstraintOrigin::FieldInit {
            struct_span,
            field_name,
            field_span,
            value_span,
        } => Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            format!(
                "incompatible type for field '{field_name}': expected '{expected_str}', found '{found_str}'",
            ),
            *value_span,
        )
        .with_label(
            *struct_span,
            format!("initializing field '{field_name}' of this struct"),
        )
        .with_label(
            *field_span,
            format!("field '{field_name}' has type '{expected_str}'"),
        )
        .with_label(*value_span, format!("value has type '{found_str}'")),

        ConstraintOrigin::SetTarget {
            place_span,
            value_span,
        } => Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            format!(
                "incompatible type in set: expected '{expected_str}', found '{found_str}'",
            ),
            *value_span,
        )
        .with_label(*place_span, format!("place has type '{expected_str}'"))
        .with_label(*value_span, format!("value has type '{found_str}'")),

        ConstraintOrigin::CastExpr {
            expr_span,
            target_span,
        } => Diagnostic::error(
            DiagCode::T010InvalidCast,
            format!("invalid cast from '{found_str}' to '{expected_str}'"),
            *target_span,
        )
        .with_label(*expr_span, format!("expression has type '{found_str}'"))
        .with_label(
            *target_span,
            format!("cast to '{expected_str}' is not possible"),
        ),

        ConstraintOrigin::ImplicitWidening {
            source_span,
            target_span,
        } => Diagnostic::error(
            DiagCode::T015ImplicitWidening,
            format!(
                "implicit widening from '{found_str}' to '{expected_str}' is not allowed",
            ),
            *source_span,
        )
        .with_label(*source_span, format!("type '{found_str}' here"))
        .with_label(*target_span, format!("expected '{expected_str}'"))
        .with_hint(format!(
            "use explicit conversion: 'value as {expected_str}'"
        )),

        ConstraintOrigin::TryInvalid { span } => Diagnostic::error(
            DiagCode::T016TryInvalid,
            format!(
                "the '?' operator can only be applied to Result<T,E> or Option<T>, found '{found_str}'"
            ),
            *span,
        )
        .with_label(*span, format!("this has type '{found_str}'"))
        .with_hint("use a `Result<T, E>` or `Option<T>` value here"),

        ConstraintOrigin::TryReturnInvalid { span, return_span } => Diagnostic::error(
            DiagCode::T016TryInvalid,
            format!("the '?' operator cannot propagate into function return type '{found_str}'"),
            *span,
        )
        .with_label(*span, "this may propagate an error or None")
        .with_label(*return_span, format!("function returns '{found_str}'"))
        .with_hint("return a compatible Result/Option or handle the value with match or catch"),

        ConstraintOrigin::AwaitInvalid { span } => Diagnostic::error(
            DiagCode::T032AwaitInvalid,
            format!(
                "the 'await' operator can only be applied to Coroutine<T>, found '{found_str}'"
            ),
            *span,
        )
        .with_label(*span, format!("this has type '{found_str}'"))
        .with_hint("use a `Coroutine<T>` value here"),

        ConstraintOrigin::InvalidIndex {
            base_span,
            index_span,
            is_base_error,
        } => {
            if *is_base_error {
                Diagnostic::error(
                    DiagCode::T017InvalidIndex,
                    format!("type '{expected_str}' cannot be indexed"),
                    *base_span,
                )
                .with_label(
                    *base_span,
                    format!("this has type '{expected_str}', expected array or slice"),
                )
            } else {
                Diagnostic::error(
                    DiagCode::T017InvalidIndex,
                    format!("index must be of type 'int', found '{found_str}'"),
                    *index_span,
                )
                .with_label(*index_span, format!("this has type '{found_str}'"))
            }
        }

        ConstraintOrigin::UndefinedField {
            base_span,
            field_span,
            field_name,
        } => {
            let mut diag = Diagnostic::error(
                DiagCode::T018UndefinedField,
                format!("no field '{field_name}' on type '{expected_str}'"),
                *field_span,
            )
            .with_label(*base_span, format!("this has type '{expected_str}'"))
            .with_label(*field_span, "unknown field".to_string());

            // Helper to recursively find structure ID
            fn get_struct_id(ty: &ArType, interner: &super::types::TypeInterner) -> Option<SymbolId> {
                match ty {
                    ArType::Named(id, _) => Some(*id),
                    ArType::Ptr(inner) | ArType::Nullable(inner) => {
                        let inner_ty = interner.resolve(*inner);
                        get_struct_id(&inner_ty, interner)
                    }
                    _ => None,
                }
            }

            if let Some(struct_id) = get_struct_id(&constraint.expected, &type_info.type_interner) {
                struct Candidate {
                    name: String,
                    is_method: bool,
                }
                let mut candidates = Vec::new();

                // Add struct fields as candidates
                if let Some(fields) = type_info.struct_fields.get(&struct_id) {
                    for f in fields.iter() {
                        candidates.push(Candidate {
                            name: f.name.to_string(),
                            is_method: false,
                        });
                    }
                }

                // Add associated methods as candidates
                for (type_sym, m_name) in symbols.associated_members.keys() {
                    if *type_sym == struct_id {
                        candidates.push(Candidate {
                            name: m_name.to_string(),
                            is_method: true,
                        });
                    }
                }

                // Sort candidates alphabetically to ensure deterministic suggestions (fixes golden tests on 32-bit vs 64-bit)
                candidates.sort_by(|a, b| a.name.cmp(&b.name));

                let max_distance = if field_name.len() <= 4 { 2 } else { 3 };
                let best_match = candidates
                    .iter()
                    .map(|cand| {
                        let dist = if cand.name.to_lowercase() == field_name.to_lowercase() {
                            0
                        } else {
                            strsim::levenshtein(field_name, &cand.name)
                        };
                        (cand, dist)
                    })
                    .filter(|(_, dist)| *dist <= max_distance)
                    .min_by_key(|(_, dist)| *dist)
                    .map(|(cand, _)| cand);

                if let Some(suggestion) = best_match {
                    let formatted = if suggestion.is_method {
                        format!("{}()", suggestion.name)
                    } else {
                        suggestion.name.clone()
                    };
                    diag = diag.with_hint(format!("did you mean '{formatted}'?"));
                }
            }

            diag
        }

        ConstraintOrigin::ArrayLiteral {
            array_span,
            item_span,
            item_index,
        } => Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            format!(
                "array element {} has type '{found_str}', expected '{expected_str}'",
                item_index + 1,
            ),
            *item_span,
        )
        .with_label(
            *array_span,
            format!("first element type is '{expected_str}'"),
        )
        .with_label(*item_span, format!("element has type '{found_str}'")),

        ConstraintOrigin::NullCoalesce {
            left_span,
            right_span,
        } => Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            format!(
                "incompatible types in `??`: nullable side expects inner type '{expected_str}', found '{found_str}'"
            ),
            *right_span,
        )
        .with_label(*left_span, format!("nullable value has inner type '{expected_str}'"))
        .with_label(*right_span, format!("right-hand side has type '{found_str}'")),

        ConstraintOrigin::CatchHandler {
            expr_span,
            handler_span,
        } => Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            format!(
                "catch handler type '{found_str}' is incompatible with Result ok type '{expected_str}'"
            ),
            *handler_span,
        )
        .with_label(
            *expr_span,
            format!("expression ok type is '{expected_str}'"),
        )
        .with_label(*handler_span, format!("handler has type '{found_str}'")),

        ConstraintOrigin::LiteralPromotion {
            literal,
            literal_span,
            target_span,
        } => {
            let mut diag = Diagnostic::error(
                DiagCode::T038IntegerLiteralOutOfRange,
                format!("integer literal `{literal}` does not fit in `{expected_str}`"),
                *literal_span,
            )
            .with_label(
                *literal_span,
                format!("value is outside the range of `{expected_str}`"),
            )
            .with_hint("use a wider integer type or change the literal value");
            if target_span != literal_span {
                diag = diag.with_label(*target_span, "promoted literal type demanded here");
            }
            diag
        }
    }
}

// ── Active hints ────────────────────────────────────────────────────

/// Add operator-specific hints (e.g., suggest interpolation for str+int).
fn add_operator_hint(
    diag: Diagnostic,
    left: &ArType,
    right: &ArType,
    symbols: &SymbolTable,
    interner: &super::types::TypeInterner,
) -> Diagnostic {
    let left_str = left.display(symbols, interner);
    let right_str = right.display(symbols, interner);

    // str + something → suggest interpolation
    if is_string_type(left) || is_string_type(right) {
        return diag
            .with_hint("to concatenate with string, use interpolation: \"${value}\"".to_string());
    }

    // numeric type mismatch → suggest cast
    if left.is_numeric() && right.is_numeric() {
        return diag.with_hint(format!(
            "convert explicitly: 'value as {}'",
            if left.is_float() { left_str } else { right_str }
        ));
    }

    if super::types::unify(left, right, interner) {
        if is_string_type(left) {
            return diag.with_hint(
                "relational comparisons on strings are not supported directly; use `std.core.cmp` or `.compare()`",
            );
        }
        if matches!(left, ArType::Primitive(super::types::Primitive::Bool)) {
            return diag.with_hint("type `bool` is not orderable");
        }
        if left.is_float() {
            return diag.with_hint("modulo operator `%` is only defined for integer types");
        }
        if matches!(left, ArType::Named(_, _) | ArType::Tuple(_)) {
            return diag.with_hint(
                "composite types require implementing an explicit comparison interface",
            );
        }
    }

    diag
}

/// Suggest how to convert a value to bool.
fn suggest_bool_conversion(ty: &ArType) -> String {
    match ty {
        ArType::Primitive(p) if p.is_numeric() => {
            "use explicit comparison: 'value != 0'".to_string()
        }
        ArType::Nullable(_) => "use explicit comparison: 'value != nil'".to_string(),
        ArType::Primitive(super::types::Primitive::Str) => {
            "strings are not automatically bool — use explicit comparison".to_string()
        }
        _ => "type is not convertible to bool".to_string(),
    }
}

fn is_string_type(ty: &ArType) -> bool {
    matches!(ty, ArType::Primitive(super::types::Primitive::Str))
}
