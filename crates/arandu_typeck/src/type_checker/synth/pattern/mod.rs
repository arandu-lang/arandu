mod aggregate;
mod bind;
mod enum_pat;

use arandu_parser::{Pattern, ast_pool::PatternId};

use super::super::TypeChecker;
use super::super::types::{self, ArType, TypeId};

fn instantiate_enum_payload(
    checker: &mut TypeChecker<'_>,
    enum_id: crate::SymbolId,
    variant_id: crate::SymbolId,
    enum_args: &[TypeId],
    payload: &[TypeId],
) -> Vec<TypeId> {
    let span = checker.symbols.try_get(variant_id).map_or(
        arandu_middle::Span::new(variant_id.file_id, 0, 0),
        |symbol| symbol.span,
    );
    checker.type_info.demand_header(enum_id, enum_args, span);
    let frozen = checker
        .type_info
        .variant_payload_for(variant_id, enum_args)
        .cloned();
    let payload = match &frozen {
        Some(super::super::EnumPayloadShape::Tuple(items)) => items.as_slice(),
        _ => payload,
    };
    let params = checker
        .type_info
        .generic_params
        .get(&enum_id)
        .map(|params| params.as_ref().clone())
        .or_else(|| enum_params_from_variant(checker, variant_id));
    let Some(params) = params else {
        return payload.to_vec();
    };
    if enum_args.is_empty() || params.is_empty() {
        return payload.to_vec();
    }
    let concrete: Vec<_> = enum_args.iter().map(|&id| checker.resolve(id)).collect();
    let subst = types::build_subst(&params, &concrete[..params.len().min(concrete.len())]);
    payload
        .iter()
        .map(|&id| {
            let ty = checker.resolve(id);
            checker.intern(types::substitute_type(
                &ty,
                &subst,
                &checker.type_info.type_interner,
            ))
        })
        .collect()
}

fn enum_params_from_variant(
    checker: &TypeChecker<'_>,
    variant_id: crate::SymbolId,
) -> Option<Vec<crate::SymbolId>> {
    let ret_ty = match checker.type_info.decl_type(variant_id)? {
        ArType::Func(_, ret) => checker.resolve(ret),
        ty => ty,
    };
    let ArType::Named(_, args) = ret_ty else {
        return None;
    };
    let args = checker.type_info.type_interner.type_args(args);
    let params: Vec<_> = args
        .into_iter()
        .filter_map(|arg| match checker.resolve(arg) {
            ArType::Named(id, args) if args.is_empty() => Some(id),
            _ => None,
        })
        .collect();
    (!params.is_empty()).then_some(params)
}

/// Match ergonomics: a payload reached through a shared or mutable reference
/// binds non-Copy values by reference, so checking a pattern cannot duplicate
/// ownership or move out of the borrowed scrutinee.
fn borrowed_payload_type(
    checker: &mut TypeChecker<'_>,
    source_ty: TypeId,
    payload_ty: TypeId,
) -> TypeId {
    if checker.type_info.is_copy(payload_ty) {
        return payload_ty;
    }
    match checker.resolve(source_ty) {
        ArType::Ref(_) => checker.intern(ArType::Ref(payload_ty)),
        ArType::RefMut(_) => checker.intern(ArType::RefMut(payload_ty)),
        _ => payload_ty,
    }
}

pub fn check_pattern(checker: &mut TypeChecker<'_>, pattern: PatternId, value_ty: TypeId) {
    let pat = checker.pool.pattern(pattern);
    let pattern_source_ty = value_ty;
    let borrowed_scrutinee = matches!(
        checker.resolve(value_ty),
        ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)
    );
    let value_ty = if matches!(pat, Pattern::Bind { .. }) {
        value_ty
    } else {
        let mut id = value_ty;
        for _ in 0..4 {
            match checker.resolve(id) {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => id = inner,
                _ => break,
            }
        }
        id
    };
    match pat {
        Pattern::Wildcard { .. } => bind::check_wildcard(),
        Pattern::Bind { span, .. } => bind::check_bind(checker, span, value_ty),
        Pattern::Literal { expr, .. } => bind::check_literal(checker, pat, expr, value_ty),
        Pattern::Enum {
            span,
            type_name,
            variant,
            payload,
        } => enum_pat::check_enum(
            checker,
            span,
            type_name,
            variant,
            payload,
            value_ty,
            pattern_source_ty,
        ),
        Pattern::TypeTuple {
            span,
            name,
            payload,
        } => aggregate::check_type_tuple(checker, span, name, payload, value_ty, pattern_source_ty),
        Pattern::Tuple { items, span: _ } => aggregate::check_tuple(checker, pat, items, value_ty),
        Pattern::Struct {
            type_name,
            fields,
            span: _,
        } => aggregate::check_struct(
            checker,
            pat,
            type_name,
            fields,
            value_ty,
            pattern_source_ty,
            borrowed_scrutinee,
        ),
        Pattern::Range { start, end, .. } => {
            aggregate::check_range(checker, pat, start, end, value_ty)
        }
        Pattern::Or { alts, .. } => aggregate::check_or(checker, alts, value_ty),
    }
}
