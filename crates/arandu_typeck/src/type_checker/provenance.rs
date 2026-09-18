//! Causal provenance for type-constraint failures (roadmap section C).
//!
//! Every [`ConstraintOrigin`] already
//! carries the source spans of both sides of a failed relation; this module
//! turns those spans into an ordered, role-tagged chain — *where the type was
//! expected* vs *where the incompatible value was produced* — that renderers
//! (terminal diagnostics today, IDE explain views later) consume instead of
//! re-deriving roles per origin variant. Derivation is total over all origin
//! variants and depends only on the constraint, so output ordering is
//! deterministic.

use arandu_lexer::Span;

use super::constraints::{Constraint, ConstraintOrigin};

// ── Provenance model ────────────────────────────────────────────────

/// Role a source location plays in the causal chain of a type error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceRole {
    /// Establishes the expected type (annotation, parameter, first branch…).
    ExpectedOrigin,
    /// Produces the incompatible value (rhs, argument, handler…).
    FoundOrigin,
    /// Frames both sides (call site, operator, container literal).
    Context,
}

/// One step of a causal chain: a span, the role it plays, and a short
/// user-facing description of why it participates.
#[derive(Debug, Clone)]
pub struct ProvenanceStep {
    pub role: ProvenanceRole,
    pub span: Span,
    pub description: String,
}

impl ProvenanceStep {
    fn new(role: ProvenanceRole, span: Span, description: impl Into<String>) -> Self {
        Self {
            role,
            span,
            description: description.into(),
        }
    }
}

/// Derive the causal chain of `constraint`, ordered expected-side first.
#[must_use]
pub fn causal_chain(constraint: &Constraint) -> Vec<ProvenanceStep> {
    use ProvenanceRole::{Context, ExpectedOrigin, FoundOrigin};
    match &constraint.origin {
        ConstraintOrigin::Assignment { lhs_span, rhs_span } => vec![
            ProvenanceStep::new(ExpectedOrigin, *lhs_span, "declaration"),
            ProvenanceStep::new(FoundOrigin, *rhs_span, "value"),
        ],
        ConstraintOrigin::CallArg {
            call_span,
            param_span,
            arg_span,
            arg_index,
        } => vec![
            ProvenanceStep::new(Context, *call_span, "call"),
            ProvenanceStep::new(
                ExpectedOrigin,
                *param_span,
                format!("parameter {}", arg_index + 1),
            ),
            ProvenanceStep::new(
                FoundOrigin,
                *arg_span,
                format!("argument {}", arg_index + 1),
            ),
        ],
        ConstraintOrigin::ReturnType {
            return_span,
            declared_span,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *declared_span, "return type declaration"),
            ProvenanceStep::new(FoundOrigin, *return_span, "return expression"),
        ],
        ConstraintOrigin::IfBranches {
            then_span,
            else_span,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *then_span, "then branch"),
            ProvenanceStep::new(FoundOrigin, *else_span, "else branch"),
        ],
        ConstraintOrigin::MatchArms {
            first_span,
            mismatch_span,
            ..
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *first_span, "first arm"),
            ProvenanceStep::new(FoundOrigin, *mismatch_span, "mismatching arm"),
        ],
        ConstraintOrigin::BinaryOp {
            op_span,
            left_span,
            right_span,
        } => vec![
            ProvenanceStep::new(Context, *op_span, "operator"),
            ProvenanceStep::new(ExpectedOrigin, *left_span, "left operand"),
            ProvenanceStep::new(FoundOrigin, *right_span, "right operand"),
        ],
        ConstraintOrigin::UnaryOp {
            op: _,
            op_span,
            operand_span,
        } => vec![
            ProvenanceStep::new(Context, *op_span, "operator"),
            ProvenanceStep::new(FoundOrigin, *operand_span, "operand"),
        ],
        ConstraintOrigin::Condition { span } => {
            vec![ProvenanceStep::new(FoundOrigin, *span, "condition")]
        }
        ConstraintOrigin::FieldInit {
            struct_span,
            field_name,
            field_span,
            value_span,
        } => vec![
            ProvenanceStep::new(Context, *struct_span, format!("'{field_name}' initializer")),
            ProvenanceStep::new(ExpectedOrigin, *field_span, format!("field '{field_name}'")),
            ProvenanceStep::new(FoundOrigin, *value_span, "value"),
        ],
        ConstraintOrigin::SetTarget {
            place_span,
            value_span,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *place_span, "place"),
            ProvenanceStep::new(FoundOrigin, *value_span, "value"),
        ],
        ConstraintOrigin::CastExpr {
            expr_span,
            target_span,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *target_span, "cast target"),
            ProvenanceStep::new(FoundOrigin, *expr_span, "expression"),
        ],
        ConstraintOrigin::ImplicitWidening {
            source_span,
            target_span,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *target_span, "wider target"),
            ProvenanceStep::new(FoundOrigin, *source_span, "narrower value"),
        ],
        ConstraintOrigin::TryInvalid { span } => {
            vec![ProvenanceStep::new(FoundOrigin, *span, "'?' operand")]
        }
        ConstraintOrigin::AwaitInvalid { span } => {
            vec![ProvenanceStep::new(FoundOrigin, *span, "'await' operand")]
        }
        ConstraintOrigin::InvalidIndex {
            base_span,
            index_span,
            is_base_error,
        } => {
            if *is_base_error {
                vec![ProvenanceStep::new(FoundOrigin, *base_span, "indexed base")]
            } else {
                vec![
                    ProvenanceStep::new(Context, *base_span, "container"),
                    ProvenanceStep::new(FoundOrigin, *index_span, "index"),
                ]
            }
        }
        ConstraintOrigin::UndefinedField {
            base_span,
            field_span,
            field_name,
        } => vec![
            ProvenanceStep::new(Context, *base_span, "receiver"),
            ProvenanceStep::new(
                FoundOrigin,
                *field_span,
                format!("unknown field '{field_name}'"),
            ),
        ],
        ConstraintOrigin::ArrayLiteral {
            array_span,
            item_span,
            item_index,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *array_span, "first element"),
            ProvenanceStep::new(
                FoundOrigin,
                *item_span,
                format!("element {}", item_index + 1),
            ),
        ],
        ConstraintOrigin::NullCoalesce {
            left_span,
            right_span,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *left_span, "nullable left side"),
            ProvenanceStep::new(FoundOrigin, *right_span, "fallback right side"),
        ],
        ConstraintOrigin::CatchHandler {
            expr_span,
            handler_span,
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *expr_span, "Result ok type"),
            ProvenanceStep::new(FoundOrigin, *handler_span, "handler"),
        ],
        ConstraintOrigin::LiteralPromotion {
            literal_span,
            target_span,
            ..
        } => vec![
            ProvenanceStep::new(ExpectedOrigin, *target_span, "demanded type"),
            ProvenanceStep::new(FoundOrigin, *literal_span, "literal value"),
        ],
    }
}
