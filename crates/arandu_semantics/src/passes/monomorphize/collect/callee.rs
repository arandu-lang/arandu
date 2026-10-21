use super::CallSiteInfo;
use super::infer::{infer_call_type_args, is_identity_args};
use arandu_middle::hir::{HirExprId, HirExprKind, HirProgram};
use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::{ArType, Primitive, TypeId};
use arandu_typeck::TypeCheckResult;

pub(crate) fn generic_callee_symbol(
    callee_id: HirExprId,
    hir: &HirProgram,
    tc: &TypeCheckResult,
) -> Option<SymbolId> {
    let pool = &hir.pool;
    let callee = pool.expr(callee_id);
    match &callee.kind {
        HirExprKind::Path { symbol } => Some(*symbol),
        HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
        HirExprKind::Field { base, field } | HirExprKind::SafeField { base, field } => {
            method_symbol_from_field(pool, tc, *base, field.as_str())
        }
        _ => None,
    }
}

/// Peel Nullable / & / &mut / ptr layers so method mono keys see the Named receiver.
fn peel_recv_base_ty(tc: &TypeCheckResult, base_ty: ArType) -> ArType {
    let interner = &tc.type_info.type_interner;
    let mut actual = match base_ty {
        ArType::Nullable(inner) => interner.resolve(inner),
        other => other,
    };
    for _ in 0..4 {
        actual = match actual {
            ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                interner.resolve(inner)
            }
            other => return other,
        };
    }
    actual
}

fn method_symbol_from_field(
    pool: &arandu_middle::hir::HirPool,
    tc: &TypeCheckResult,
    base: HirExprId,
    field: &str,
) -> Option<SymbolId> {
    let base_ty = tc.type_info.type_interner.resolve(pool.expr(base).ty);
    let actual = peel_recv_base_ty(tc, base_ty);
    let type_id = match actual {
        ArType::Vector(shape) => tc.symbols.vector_type_symbol(shape),
        ArType::Named(id, _) => Some(id),
        ArType::Ptr(inner) => match tc.type_info.type_interner.resolve(inner) {
            ArType::Named(id, _) => Some(id),
            _ => tc.symbols.builtins.ptr_type,
        },
        // Builtin Result/Option/primitive methods are associated to the
        // prelude type symbols after import re-index (not ArType::Named).
        ArType::Result(_, _) => tc.symbols.builtins.result,
        ArType::Option(_) => tc.symbols.builtins.option,
        ArType::Primitive(Primitive::Str) => tc.symbols.builtins.str_type,
        ArType::Primitive(Primitive::Char) => tc.symbols.builtins.char_type,
        ArType::Primitive(Primitive::Int) | ArType::IntLiteral => tc.symbols.builtins.int_type,
        ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => {
            tc.symbols.builtins.float_type
        }
        ArType::Slice(_) => tc.symbols.builtins.slice_type,
        _ => None,
    }?;
    tc.symbols.lookup_associated_member(type_id, field)
}

/// Build an instantiation key for a call that is not wrapped in `Generic`.
///
/// Covers:
/// - method calls `obj.m(...)` where type args come from the receiver (`BoxG<int>.get`)
/// - free calls `f(x)` where type args are inferred from argument types (`id(41)`)
///
/// Returns `None` when the callee is not generic or type args cannot be recovered.
pub(in crate::passes::monomorphize) fn instantiation_key_for_call(
    hir: &HirProgram,
    tc: &TypeCheckResult,
    callee_id: HirExprId,
    args: arandu_middle::hir::IndexRange,
    call_result_ty: arandu_middle::types::TypeId,
    _span: arandu_lexer::Span,
) -> Option<(SymbolId, Vec<TypeId>)> {
    let pool = &hir.pool;
    let callee = pool.expr(callee_id);

    // Already handled by the Generic branch when the call is `f<T>(...)`.
    if matches!(callee.kind, HirExprKind::Generic { .. }) {
        return None;
    }

    let (symbol, type_args) = match &callee.kind {
        HirExprKind::Field { base, field } | HirExprKind::SafeField { base, field } => {
            let sym = method_symbol_from_field(pool, tc, *base, field.as_str())?;
            let params = tc.type_info.generic_params.get(&sym)?.clone();
            if params.is_empty() {
                return None;
            }
            // Type args from receiver `Named(S, [T1,…])` or builtin
            // `Result<T,E>` / `Option<T>` (params prefix for method mono).
            // Peel Nullable / & / &mut so a ref-typed receiver still specializes.
            let base_ty = tc.type_info.type_interner.resolve(pool.expr(*base).ty);
            let actual = peel_recv_base_ty(tc, base_ty);
            let recv_args: Vec<_> = match actual {
                ArType::Named(_, args) => tc.type_info.type_interner.type_args(args),
                ArType::Ptr(inner) => match tc.type_info.type_interner.resolve(inner) {
                    ArType::Named(_, args) => tc.type_info.type_interner.type_args(args),
                    _ => Vec::new(),
                },
                ArType::Result(ok, err) => vec![ok, err],
                ArType::Option(inner) => vec![inner],
                _ => Vec::new(),
            };
            let call_info = CallSiteInfo {
                callee_id,
                args,
                result_ty: call_result_ty,
                recv_args: &recv_args,
            };
            if let Some(inferred) = infer_call_type_args(tc, sym, &params, pool, &call_info) {
                (sym, inferred)
            } else if recv_args.len() == params.len() && !recv_args.is_empty() {
                (sym, recv_args)
            } else {
                return None;
            }
        }
        HirExprKind::Path { symbol } => {
            let params = tc.type_info.generic_params.get(symbol)?.clone();
            if params.is_empty() {
                return None;
            }
            let call_info = CallSiteInfo {
                callee_id,
                args,
                result_ty: call_result_ty,
                recv_args: &[],
            };
            let inferred = infer_call_type_args(tc, *symbol, &params, pool, &call_info)?;
            (*symbol, inferred)
        }
        HirExprKind::TypePath { member_symbol, .. } => {
            let params = tc.type_info.generic_params.get(member_symbol)?.clone();
            if params.is_empty() {
                return None;
            }
            let call_info = CallSiteInfo {
                callee_id,
                args,
                result_ty: call_result_ty,
                recv_args: &[],
            };
            let inferred = infer_call_type_args(tc, *member_symbol, &params, pool, &call_info)?;
            (*member_symbol, inferred)
        }
        _ => return None,
    };

    // Skip identity template keys (T -> T).
    if is_identity_args(tc, symbol, &type_args) {
        return None;
    }
    Some((symbol, type_args))
}
