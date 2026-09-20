use rustc_hash::FxHashMap;
use std::sync::Arc;

use arandu_middle::SymbolId;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use arandu_parser::{GenericParam, IndexRange};

use crate::type_checker::TypeChecker;
use crate::type_checker::types::{
    ArType, GenericSubst, LowerCtx, TypeId, TypeInterner, build_subst, substitute_type,
    type_name_base,
};
use arandu_middle::types::lower::lower_type_expr_ctx;

#[must_use]
pub fn extract_generic_param_symbols(
    checker: &TypeChecker<'_>,
    params: &[GenericParam],
) -> Vec<SymbolId> {
    params
        .iter()
        .filter_map(|param| {
            checker
                .resolved
                .definitions
                .get(&crate::NodeKey::from(param.span))
                .copied()
        })
        .collect()
}

#[must_use]
pub(crate) fn instantiate_type(
    ty: &ArType,
    subst: &GenericSubst,
    interner: &mut TypeInterner,
) -> ArType {
    substitute_type(ty, subst, interner)
}

/// T2.1: pad trailing type args with declared defaults when fewer args are given.
///
/// Returns `None` if `provided.len() > params.len()`, or if a missing trailing
/// parameter has no default. Full arity (`provided.len() == params.len()`) is a
/// no-op pass-through.
#[must_use]
pub fn expand_type_args_with_defaults(
    checker: &TypeChecker<'_>,
    owner: SymbolId,
    provided: &[ArType],
) -> Option<Vec<ArType>> {
    let params = checker.type_info.generic_params.get(&owner)?;
    if provided.len() > params.len() {
        return None;
    }
    if provided.len() == params.len() {
        return Some(provided.to_vec());
    }
    let mut out = provided.to_vec();
    for &param_sym in &params[provided.len()..] {
        let &def_tid = checker.type_info.generic_defaults.get(&param_sym)?;
        out.push(checker.resolve(def_tid));
    }
    Some(out)
}

/// Expand defaults on a `Named` type when args are a trailing subset of params.
#[must_use]
pub fn expand_named_with_defaults(checker: &mut TypeChecker<'_>, ty: ArType) -> ArType {
    match ty {
        ArType::Named(id, args) => {
            let arg_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(args);
            let provided: Vec<ArType> = arg_ids
                .into_iter()
                .map(|a| {
                    let resolved = checker.resolve(a);
                    expand_named_with_defaults(checker, resolved)
                })
                .collect();
            let expanded = if checker.type_info.generic_params.contains_key(&id) {
                expand_type_args_with_defaults(checker, id, &provided).unwrap_or(provided)
            } else {
                provided
            };
            let arg_ids: Vec<TypeId> = expanded.into_iter().map(|t| checker.intern(t)).collect();
            ArType::named(id, &arg_ids, &checker.type_info.type_interner)
        }
        ArType::Func(params, ret) => {
            let params_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(params);
            let new_params: Vec<TypeId> = params_ids
                .into_iter()
                .map(|p| {
                    let p_ty = checker.resolve(p);
                    let p_exp = expand_named_with_defaults(checker, p_ty);
                    checker.intern(p_exp)
                })
                .collect();
            let ret_ty = checker.resolve(ret);
            let ret_exp = expand_named_with_defaults(checker, ret_ty);
            let ret_id = checker.intern(ret_exp);
            ArType::func(&new_params, ret_id, &checker.type_info.type_interner)
        }
        ArType::Ref(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Ref(tid)
        }
        ArType::RefMut(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::RefMut(tid)
        }
        ArType::Ptr(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Ptr(tid)
        }
        ArType::Slice(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Slice(tid)
        }
        ArType::Array(n, inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Array(n, tid)
        }
        ArType::ConstArray(param, inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::ConstArray(param, tid)
        }
        ArType::Nullable(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Nullable(tid)
        }
        ArType::Option(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Option(tid)
        }
        ArType::Coroutine(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Coroutine(tid)
        }
        ArType::Poll(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Poll(tid)
        }
        ArType::Range(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_named_with_defaults(checker, inner_ty);
            let tid = checker.intern(expanded);
            ArType::Range(tid)
        }
        ArType::Result(ok, err) => {
            let ok_ty = checker.resolve(ok);
            let err_ty = checker.resolve(err);
            let ok_exp = expand_named_with_defaults(checker, ok_ty);
            let err_exp = expand_named_with_defaults(checker, err_ty);
            let ok_id = checker.intern(ok_exp);
            let err_id = checker.intern(err_exp);
            ArType::Result(ok_id, err_id)
        }
        ArType::Tuple(elems) => {
            let elems_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(elems);
            let mut new_elems = Vec::with_capacity(elems_ids.len());
            for e in elems_ids {
                let e_ty = checker.resolve(e);
                let e_exp = expand_named_with_defaults(checker, e_ty);
                new_elems.push(checker.intern(e_exp));
            }
            ArType::tuple(&new_elems, &checker.type_info.type_interner)
        }
        other => other,
    }
}

#[must_use]
pub fn struct_fields_instantiated(
    checker: &mut TypeChecker<'_>,
    struct_id: SymbolId,
    generic_args: &[ArType],
) -> Option<FxHashMap<String, ArType>> {
    let fields = Arc::clone(checker.type_info.struct_fields.get(&struct_id)?);
    let params = Arc::clone(checker.type_info.generic_params.get(&struct_id)?);
    let generic_args = expand_type_args_with_defaults(checker, struct_id, generic_args)?;
    if params.len() != generic_args.len() {
        return None;
    }
    let span = checker.symbols.get(struct_id).span;
    super::interfaces::check_instantiation_constraints(checker, &params, &generic_args, span);
    let subst = build_subst(&params, &generic_args);
    let res: FxHashMap<String, ArType> = fields
        .iter()
        .map(|f| {
            let ty = checker.resolve(f.ty);
            let inst = instantiate_type(&ty, &subst, &mut checker.type_info.type_interner);
            (f.name.to_string(), inst)
        })
        .collect();
    Some(res)
}

/// Instantiate one named field without allocating a map for the entire struct.
#[must_use]
pub fn struct_field_instantiated(
    checker: &mut TypeChecker<'_>,
    struct_id: SymbolId,
    generic_args: &[ArType],
    field_name: &str,
) -> Option<ArType> {
    let fields = Arc::clone(checker.type_info.struct_fields.get(&struct_id)?);
    let field_ty = fields.get(field_name)?.ty;
    let params = Arc::clone(checker.type_info.generic_params.get(&struct_id)?);
    let generic_args = expand_type_args_with_defaults(checker, struct_id, generic_args)?;
    if params.len() != generic_args.len() {
        return None;
    }
    let span = checker.symbols.get(struct_id).span;
    super::interfaces::check_instantiation_constraints(checker, &params, &generic_args, span);
    let subst = build_subst(&params, &generic_args);
    let ty = checker.resolve(field_ty);
    Some(instantiate_type(
        &ty,
        &subst,
        &mut checker.type_info.type_interner,
    ))
}

/// Instantiate a generic callee (`identity<int>`, `Result.Ok<int>`, …) to its value type.
pub fn synth_generic_instantiation(
    checker: &mut TypeChecker<'_>,
    callee: ExprId,
    type_args: IndexRange,
    span: arandu_lexer::Span,
) -> ArType {
    let arg_ids = checker.pool.type_expr_list(type_args).to_vec();
    let scope = checker.type_scope();
    let ctx = LowerCtx {
        pool: checker.pool,
        symbols: &checker.symbols,
        scope,
        resolved: &checker.resolved,
    };
    let arg_tys: Vec<ArType> = arg_ids
        .iter()
        .map(|a| lower_type_expr_ctx(*a, &ctx, &mut checker.type_info.type_interner))
        .collect();

    if let ExprKind::TypePath { type_name, member } = checker.pool.expr(callee) {
        let base_name = type_name_base(type_name);
        if base_name == "Result" {
            if arg_tys.len() != 1 {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!(
                        "Result.{member} expects 1 type argument, found {}",
                        arg_tys.len()
                    ),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "generic callee is here")
                .with_label(span, format!("{} type arguments provided", arg_tys.len()));
                checker.diagnostics.push(diag);
                return ArType::Error;
            }
            let inner = arg_tys[0].clone();
            return match member.as_str() {
                "Ok" => {
                    let inner_id = checker.intern(inner);
                    let err_id = checker.intern(ArType::Err);
                    let result_id = checker.intern(ArType::Result(inner_id, err_id));
                    ArType::func(&[inner_id], result_id, &checker.type_info.type_interner)
                }
                "Err" => {
                    let err_id = checker.intern(ArType::Err);
                    let void_id = checker.intern(ArType::Void);
                    let result_id = checker.intern(ArType::Result(void_id, err_id));
                    ArType::func(&[err_id], result_id, &checker.type_info.type_interner)
                }
                _ => {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T018UndefinedField,
                        format!("unknown Result member '{member}'"),
                        checker.pool.expr_span(callee),
                    ));
                    ArType::Error
                }
            };
        }
        if base_name == "Option" && member == "Some" {
            if arg_tys.len() != 1 {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!(
                        "Option.Some expects 1 type argument, found {}",
                        arg_tys.len()
                    ),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "generic callee is here")
                .with_label(span, format!("{} type arguments provided", arg_tys.len()));
                checker.diagnostics.push(diag);
                return ArType::Error;
            }
            let inner = arg_tys[0].clone();
            let inner_id = checker.intern(inner);
            let opt_id = checker.intern(ArType::Option(inner_id));
            return ArType::func(&[inner_id], opt_id, &checker.type_info.type_interner);
        }
    }

    let Some(callee_symbol) = resolve_generic_callee_symbol(checker, callee) else {
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::N001UndefinedValue,
            "cannot resolve generic callee".to_string(),
            span,
        ));
        return ArType::Error;
    };

    let Some(param_symbols) = checker
        .type_info
        .generic_params
        .get(&callee_symbol)
        .cloned()
    else {
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::T011GenericConstraintNotSatisfied,
            "callee is not generic".to_string(),
            span,
        ));
        return ArType::Error;
    };

    // T2.1: fill trailing defaults when fewer type args are written.
    let arg_tys = match expand_type_args_with_defaults(checker, callee_symbol, &arg_tys) {
        Some(expanded) => expanded,
        None => {
            let diag = crate::Diagnostic::error(
                crate::DiagCode::T012WrongArgCount,
                format!(
                    "generic callee expects {} type argument(s), found {}",
                    param_symbols.len(),
                    arg_tys.len()
                ),
                span,
            )
            .with_label(checker.pool.expr_span(callee), "generic callee is here")
            .with_label(span, format!("{} type arguments provided", arg_tys.len()));
            checker.diagnostics.push(diag);
            return ArType::Error;
        }
    };

    if param_symbols.len() != arg_tys.len() {
        let diag = crate::Diagnostic::error(
            crate::DiagCode::T012WrongArgCount,
            format!(
                "generic callee expects {} type argument(s), found {}",
                param_symbols.len(),
                arg_tys.len()
            ),
            span,
        )
        .with_label(checker.pool.expr_span(callee), "generic callee is here")
        .with_label(span, format!("{} type arguments provided", arg_tys.len()));
        checker.diagnostics.push(diag);
        return ArType::Error;
    }

    let Some(template) = checker.decl_type(callee_symbol) else {
        return ArType::Error;
    };

    let subst = build_subst(&param_symbols, &arg_tys);
    super::interfaces::check_instantiation_constraints(checker, &param_symbols, &arg_tys, span);
    instantiate_type(&template, &subst, &mut checker.type_info.type_interner)
}

fn resolve_generic_callee_symbol(
    checker: &mut TypeChecker<'_>,
    callee: ExprId,
) -> Option<SymbolId> {
    match checker.pool.expr(callee) {
        ExprKind::Path { .. } => checker.resolved.expr_symbol(callee),
        ExprKind::TypePath { .. } => checker.resolved.expr_symbol(callee).or_else(|| {
            checker
                .resolved
                .type_refs
                .get(&crate::NodeKey::from(checker.pool.expr_span(callee)))
                .copied()
        }),
        ExprKind::Field { base, field } => {
            if let Some(sym) = checker.resolved.expr_symbol(callee) {
                return Some(sym);
            }
            // Namespace free/extern generic: `mem.sizeOf<T>` / `mem.alignOf<T>`.
            // Must resolve before method dispatch — `mem` is a Module, not a value type.
            if let ExprKind::Path { path } = checker.pool.expr(*base)
                && path.len() == 1
                && let Some(sym) = checker.symbols.lookup_module_member(&path[0], field)
            {
                checker.resolved.expr_ref(callee, sym);
                return Some(sym);
            }
            let base_ty_id = checker
                .expr_type_id(*base)
                .unwrap_or_else(|| crate::passes::type_checker::synth::synth_expr(checker, *base));
            let base_ty = checker.resolve(base_ty_id);
            let actual_base_ty = match &base_ty {
                ArType::Nullable(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                    checker.resolve(*inner)
                }
                other => other.clone(),
            };
            let struct_id = match &actual_base_ty {
                ArType::Named(id, _) => Some(*id),
                ArType::Ptr(inner) => match checker.resolve(*inner) {
                    ArType::Named(id, _) => Some(id),
                    _ => None,
                },
                _ => None,
            };
            if let Some(struct_id) = struct_id {
                if let Some(sym) = checker.symbols.lookup_associated_member(struct_id, field) {
                    return Some(sym);
                }
                if let Some(constraints) = checker.type_info.param_constraints.get(&struct_id) {
                    for bound in constraints.iter() {
                        if let Some(sym) = checker
                            .symbols
                            .lookup_associated_member(bound.iface_sym, field)
                        {
                            return Some(sym);
                        }
                    }
                }
            }
            checker.resolved.expr_symbol(callee)
        }
        _ => None,
    }
}

#[must_use]
pub fn expand_aliases(checker: &mut TypeChecker<'_>, ty: ArType) -> ArType {
    expand_aliases_rec(checker, ty, 0)
}

fn expand_aliases_rec(checker: &mut TypeChecker<'_>, ty: ArType, depth: usize) -> ArType {
    if depth > 64 {
        let span = match &ty {
            ArType::Named(symbol_id, _) => checker.symbols.try_get(*symbol_id).map(|s| s.span),
            _ => None,
        }
        .unwrap_or_else(|| arandu_lexer::Span::new(0, 0, 0));
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::T029RecursiveStructInfiniteSize,
            "recursive type expansion exceeds recursion limit".to_string(),
            span,
        ));
        return ArType::Error;
    }
    match ty {
        ArType::Named(symbol_id, args) => {
            let is_alias = if let Some(sym) = checker.symbols.try_get(symbol_id) {
                sym.kind == arandu_middle::SymbolKind::TypeAlias
            } else {
                false
            };
            if is_alias {
                if let Some(target_tid) = checker.decl_type_id(symbol_id) {
                    let target_ty = checker.resolve(target_tid);
                    let arg_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(args);
                    let params = checker.type_info.generic_params.get(&symbol_id);
                    let target_expanded = if let Some(params) = params {
                        let subst = arandu_middle::types::build_subst_ids(
                            params,
                            &arg_ids,
                            &checker.type_info.type_interner,
                        );
                        arandu_middle::types::substitute_type(
                            &target_ty,
                            &subst,
                            &checker.type_info.type_interner,
                        )
                    } else {
                        target_ty
                    };
                    expand_aliases_rec(checker, target_expanded, depth + 1)
                } else {
                    ArType::Error
                }
            } else {
                let arg_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(args);
                let mut expanded_args = Vec::new();
                for arg in arg_ids {
                    let arg_ty = checker.resolve(arg);
                    let expanded_arg = expand_aliases_rec(checker, arg_ty, depth + 1);
                    expanded_args.push(checker.intern(expanded_arg));
                }
                ArType::named(symbol_id, &expanded_args, &checker.type_info.type_interner)
            }
        }
        ArType::Nullable(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Nullable(checker.intern(expanded))
        }
        ArType::Option(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Option(checker.intern(expanded))
        }
        ArType::Coroutine(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Coroutine(checker.intern(expanded))
        }
        ArType::Poll(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Poll(checker.intern(expanded))
        }
        ArType::Range(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Range(checker.intern(expanded))
        }
        ArType::Result(ok, err) => {
            let ok_ty = checker.resolve(ok);
            let err_ty = checker.resolve(err);
            let ok_expanded = expand_aliases_rec(checker, ok_ty, depth + 1);
            let err_expanded = expand_aliases_rec(checker, err_ty, depth + 1);
            ArType::Result(checker.intern(ok_expanded), checker.intern(err_expanded))
        }
        ArType::Slice(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Slice(checker.intern(expanded))
        }
        ArType::Array(len, inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Array(len, checker.intern(expanded))
        }
        ArType::ConstArray(param, inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::ConstArray(param, checker.intern(expanded))
        }
        ArType::Ptr(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Ptr(checker.intern(expanded))
        }
        ArType::Ref(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::Ref(checker.intern(expanded))
        }
        ArType::RefMut(inner) => {
            let inner_ty = checker.resolve(inner);
            let expanded = expand_aliases_rec(checker, inner_ty, depth + 1);
            ArType::RefMut(checker.intern(expanded))
        }
        ArType::Tuple(tys) => {
            let tys_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(tys);
            let mut expanded_tys = Vec::new();
            for t in tys_ids {
                let t_ty = checker.resolve(t);
                let expanded_t = expand_aliases_rec(checker, t_ty, depth + 1);
                expanded_tys.push(checker.intern(expanded_t));
            }
            ArType::tuple(&expanded_tys, &checker.type_info.type_interner)
        }
        ArType::Func(params, ret) => {
            let params_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(params);
            let mut expanded_params = Vec::new();
            for p in params_ids {
                let p_ty = checker.resolve(p);
                let expanded_p = expand_aliases_rec(checker, p_ty, depth + 1);
                expanded_params.push(checker.intern(expanded_p));
            }
            let ret_ty = checker.resolve(ret);
            let expanded_ret = expand_aliases_rec(checker, ret_ty, depth + 1);
            ArType::func(
                &expanded_params,
                checker.intern(expanded_ret),
                &checker.type_info.type_interner,
            )
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::type_checker::ResolvedNames;
    use crate::type_checker::types::Primitive;
    use arandu_lexer::Span;
    use arandu_middle::symbol_table::SymbolTable;
    use arandu_parser::GenericParam;
    use arandu_parser::ast_pool::AstPool;

    #[test]
    fn test_extract_generic_param_symbols() {
        let pool = AstPool::default();
        let symbols = SymbolTable::new(0);
        let mut resolved = ResolvedNames::default();

        let span1 = Span::new(10, 20, 0);
        let span2 = Span::new(30, 40, 0);

        let sym1 = SymbolId::new(0, 1);
        let sym2 = SymbolId::new(0, 2);

        resolved.define(span1, sym1);
        resolved.define(span2, sym2);

        let checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let param1 = GenericParam {
            span: span1,
            name: "T".into(),
            const_ty: None,
            constraints: smallvec::SmallVec::new(),
            default: None,
        };
        let param2 = GenericParam {
            span: span2,
            name: "U".into(),
            const_ty: None,
            constraints: smallvec::SmallVec::new(),
            default: None,
        };

        let result = extract_generic_param_symbols(&checker, &[param1, param2]);
        assert_eq!(result, vec![sym1, sym2]);
    }

    #[test]
    fn test_struct_fields_instantiated() {
        let pool = AstPool::default();
        let mut symbols = SymbolTable::new(0);

        let struct_id = SymbolId::new(1, 0);
        let struct_sym = arandu_middle::symbol_table::Symbol {
            id: struct_id,
            name: "MyStruct".into(),
            kind: arandu_middle::SymbolKind::Struct,
            span: Span::new(0, 0, 0),
            scope: arandu_middle::ScopeId(0),
            is_public: true,
            lang_item: None,
        };
        symbols.register_imported_symbol(struct_sym);

        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let param_sym = SymbolId::new(1, 1);
        checker
            .type_info
            .generic_params
            .insert(struct_id, Arc::new(vec![param_sym]));

        let param_type_id = checker.intern(ArType::named(
            param_sym,
            &[],
            &checker.type_info.type_interner,
        ));
        let fields_map = arandu_middle::layout::StructFields::from_entries([
            arandu_middle::layout::StructFieldInfo {
                name: "x".into(),
                symbol: None,
                ty: param_type_id,
                index: 0,
            },
        ]);
        checker
            .type_info
            .struct_fields
            .insert(struct_id, Arc::new(fields_map));

        let int_type = ArType::Primitive(Primitive::Int);
        let fields = struct_fields_instantiated(&mut checker, struct_id, &[int_type]).unwrap();

        let field_x_ty = fields.get("x").unwrap();
        assert_eq!(field_x_ty, &ArType::Primitive(Primitive::Int));
        assert_eq!(
            struct_field_instantiated(
                &mut checker,
                struct_id,
                &[ArType::Primitive(Primitive::Int)],
                "x",
            ),
            Some(ArType::Primitive(Primitive::Int))
        );
    }

    #[test]
    fn test_expand_named_with_defaults_recursive() {
        let pool = AstPool::default();
        let symbols = SymbolTable::new(0);
        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let container_sym = SymbolId::new(1, 0);
        let param_t = SymbolId::new(1, 1);
        let param_a = SymbolId::new(1, 2);
        let default_allocator_sym = SymbolId::new(1, 3);
        let default_alloc_tid = checker.intern(ArType::named(
            default_allocator_sym,
            &[],
            &checker.type_info.type_interner,
        ));

        checker
            .type_info
            .generic_params
            .insert(container_sym, Arc::new(vec![param_t, param_a]));
        checker
            .type_info
            .generic_defaults
            .insert(param_a, default_alloc_tid);

        let int_tid = checker.intern(ArType::Primitive(Primitive::Int));
        // Container<int> (missing default allocator parameter A)
        let container_partial_tid = checker.intern(ArType::named(
            container_sym,
            &[int_tid],
            &checker.type_info.type_interner,
        ));

        // 1. Direct Named: Container<int> -> Container<int, DefaultAllocator>
        let container_ty =
            ArType::named(container_sym, &[int_tid], &checker.type_info.type_interner);
        let expanded = expand_named_with_defaults(&mut checker, container_ty);
        assert_eq!(
            expanded,
            ArType::named(
                container_sym,
                &[int_tid, default_alloc_tid],
                &checker.type_info.type_interner
            )
        );

        // 2. Ref: ref Container<int> -> ref Container<int, DefaultAllocator>
        let ref_ty = ArType::Ref(container_partial_tid);
        let expanded_ref = expand_named_with_defaults(&mut checker, ref_ty);
        let expected_expanded_container_tid = checker.intern(ArType::named(
            container_sym,
            &[int_tid, default_alloc_tid],
            &checker.type_info.type_interner,
        ));
        assert_eq!(expanded_ref, ArType::Ref(expected_expanded_container_tid));

        // 3. Option: Option<Container<int>> -> Option<Container<int, DefaultAllocator>>
        let opt_ty = ArType::Option(container_partial_tid);
        let expanded_opt = expand_named_with_defaults(&mut checker, opt_ty);
        assert_eq!(
            expanded_opt,
            ArType::Option(expected_expanded_container_tid)
        );

        // 4. Tuple: (int, ref Container<int>)
        let tup_ty = ArType::tuple(
            &[int_tid, checker.intern(ArType::Ref(container_partial_tid))],
            &checker.type_info.type_interner,
        );
        let expanded_tup = expand_named_with_defaults(&mut checker, tup_ty);
        assert_eq!(
            expanded_tup,
            ArType::tuple(
                &[
                    int_tid,
                    checker.intern(ArType::Ref(expected_expanded_container_tid))
                ],
                &checker.type_info.type_interner,
            )
        );
    }
}
