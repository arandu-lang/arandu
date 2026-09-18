use arandu_lexer::Span;

use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{ArType, Primitive};

use arandu_middle::types::type_interner::TypeId;

/// Check a call argument against its formal parameter.
///
/// When the formal type is `str` and the argument is ToStr-v0.1-formatable,
/// accept without a constraint (AMIR lower inserts `ToStr`). When formal is
/// `str` and the argument is not formatable, emit T034. Otherwise fall back
/// to the usual CallArg constraint.
// The call-site context (callee, argument index and all three spans) is
// threaded through so diagnostics point at the exact argument. Bundling it
// would move the same data across every call site without shrinking it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_call_arg(
    checker: &mut TypeChecker<'_>,
    arg_expr: arandu_parser::ast_pool::ExprId,
    param_id: TypeId,
    arg_ty_id: TypeId,
    call_span: Span,
    param_span: Span,
    arg_span: Span,
    arg_index: usize,
) {
    let mut arg_ty_id = arg_ty_id;
    let param_ty = checker.resolve(param_id);
    let arg_ty = checker.resolve(arg_ty_id);

    if matches!(param_ty, ArType::Primitive(Primitive::Str)) {
        if arg_ty.is_error() || arg_ty.is_to_str_v01() {
            // ToStr v0.1: lower will insert AmirRvalue::ToStr.
            return;
        }
        let interner = &checker.type_info.type_interner;
        let found = arg_ty.display(&checker.symbols, interner);
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T034CannotFormat,
                format!("cannot format value of type `{found}` as `str`"),
                arg_span,
            )
            .with_note(
                "only bool, integers, floats, char, and str are supported in v0.1".to_string(),
            )
            .with_label(param_span, "parameter expects `str`"),
        );
        return;
    }

    // If param is a reference but arg is a value (auto-ref), target type for literal is the inner type.
    let target_param_id = match param_ty {
        ArType::Ref(inner) | ArType::RefMut(inner)
            if !matches!(arg_ty, ArType::Ref(_) | ArType::RefMut(_)) =>
        {
            inner
        }
        _ => param_id,
    };

    if let Some(var_id) = checker.literal_table.var_for_expr(arg_expr) {
        let target_ty = checker.resolve(target_param_id);
        if !target_ty.is_literal() && !target_ty.is_error() {
            let root = checker.literal_table.find_root(var_id);
            if checker.literal_table.vars[root.0 as usize]
                .concrete_type
                .is_none()
            {
                match checker.constrain_literal_var(
                    var_id,
                    target_param_id,
                    ConstraintOrigin::CallArg {
                        call_span,
                        param_span,
                        arg_span,
                        arg_index,
                    },
                ) {
                    crate::type_checker::solver::ConstrainResult::Ok => {
                        arg_ty_id = target_param_id;
                        if target_param_id == param_id {
                            return;
                        }
                    }
                    crate::type_checker::solver::ConstrainResult::OutOfRange => {
                        return;
                    }
                    crate::type_checker::solver::ConstrainResult::Incompatible => {}
                }
            }
        }
    }

    // Implicit widening of non-literal numerics must be reported (T015)
    // *before* the `is_assignable` check: `is_assignable` has no numeric
    // widening, so two differing non-literal numerics never satisfy it and
    // the branch below would be unreachable. This mirrors `let`/assignment
    // (`apply_assignment_constraints`), so `f(x: float)(int_var)` reports the
    // same widening error as `let a: float = int_var`.
    let arg_ty = checker.resolve(arg_ty_id);
    let param_ty = checker.resolve(param_id);
    if !arg_ty.is_literal() && arg_ty != param_ty && param_ty.is_numeric() && arg_ty.is_numeric() {
        checker.add_constraint(
            param_id,
            arg_ty_id,
            ConstraintOrigin::ImplicitWidening {
                source_span: arg_span,
                target_span: call_span,
            },
        );
        return;
    }

    if checker.is_assignable(arg_ty_id, param_id) {
        return;
    }

    let param_ty = checker.resolve(param_id);
    let arg_ty = checker.resolve(arg_ty_id);

    // W3.3 auto-ref: formal `&T` / `&mut T`, actual `T` → accept (lower inserts Borrow).
    if let ArType::Ref(inner) | ArType::RefMut(inner) = param_ty
        && checker.is_assignable(arg_ty_id, inner)
    {
        return;
    }
    // Auto-deref: formal `T`, actual `&T` / `&mut T`.
    if let ArType::Ref(inner) | ArType::RefMut(inner) = arg_ty
        && checker.is_assignable(inner, param_id)
    {
        return;
    }

    checker.add_subtype_constraint(
        param_id,
        arg_ty_id,
        ConstraintOrigin::CallArg {
            call_span,
            param_span,
            arg_span,
            arg_index,
        },
    );
}
