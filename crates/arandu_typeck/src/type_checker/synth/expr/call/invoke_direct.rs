use std::sync::Arc;

use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind, IndexRange};

use super::thread_target::synth_thread_spawn_closure;
use super::{
    check_call_arg, check_target_thread_capabilities, infer_and_instantiate_func, synth_expr,
    synth_expr_expected,
};
use crate::type_checker::TypeChecker;
use crate::type_checker::types::ArType;
use arandu_middle::types::type_interner::TypeId;

pub(super) fn synth_direct_symbol_call(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    span: Span,
    callee_id: ExprId,
    args_range: IndexRange,
    expected: Option<TypeId>,
) -> Option<TypeId> {
    let callee_ty_id = synth_expr(checker, callee_id);
    let arg_ids = checker.pool.expr_list(args_range).to_vec();
    let callee_ty = checker.resolve(callee_ty_id);
    let func_info = if let ArType::Func(ref params, ret) = callee_ty {
        Some((checker.type_info.type_interner.type_args(*params), ret))
    } else {
        None
    };
    let is_error = matches!(callee_ty, ArType::Error);

    Some(if let Some((mut params, mut ret)) = func_info {
        let mut is_direct = false;
        let mut current_callee = callee_id;
        let had_explicit_generic = matches!(checker.pool.expr(callee_id), ExprKind::Generic { .. });
        if let ExprKind::Generic { callee: inner, .. } = checker.pool.expr(current_callee) {
            current_callee = *inner;
        }
        let mut callee_func_sym = None;
        match checker.pool.expr(current_callee) {
            ExprKind::Path { .. } => {
                let mut found_direct = false;
                if let Some(sym_id) = checker.resolved.expr_symbol(current_callee) {
                    let sym = checker.symbols.get(sym_id);
                    if matches!(
                        sym.kind,
                        arandu_middle::SymbolKind::Func
                            | arandu_middle::SymbolKind::ExternFunc
                            | arandu_middle::SymbolKind::EnumVariant
                            | arandu_middle::SymbolKind::AssociatedFunc
                    ) {
                        is_direct = true;
                        callee_func_sym = Some(sym_id);
                        found_direct = true;
                    }
                }
                if !found_direct
                    && let ExprKind::Path { path } = checker.pool.expr(current_callee)
                    && let [module, member] = path.as_slice()
                    && let Some(sym_id) = checker.symbols.lookup_module_member(module, member)
                {
                    is_direct = true;
                    callee_func_sym = Some(sym_id);
                }
            }
            ExprKind::TypePath { .. } => {
                // Any TypePath that resolves to a Func type is a static constructor or associated function.
                // It cannot be a local variable or field. Thus, if it's callable, it's a direct call.
                is_direct = true;
                if let Some(sym_id) = checker.resolved.expr_symbol(current_callee) {
                    callee_func_sym = Some(sym_id);
                }
            }
            // `mem.sizeOf` / `io.println` / `rt.spawn` after Generic unwrap:
            // Field on a module path.
            ExprKind::Field { base, field } => {
                if let ExprKind::Path { path } = checker.pool.expr(*base)
                    && path.len() == 1
                    && let Some(sym_id) = checker
                        .resolved
                        .expr_symbol(current_callee)
                        .or_else(|| checker.symbols.lookup_module_member(&path[0], field))
                {
                    let kind = checker.symbols.get(sym_id).kind;
                    if matches!(
                        kind,
                        arandu_middle::SymbolKind::Func
                            | arandu_middle::SymbolKind::ExternFunc
                            | arandu_middle::SymbolKind::AssociatedFunc
                    ) {
                        is_direct = true;
                        callee_func_sym = Some(sym_id);
                        Arc::make_mut(&mut checker.resolved).expr_ref(current_callee, sym_id);
                    }
                }
            }
            _ => {}
        }

        if let Some(sym_id) = callee_func_sym {
            if check_target_thread_capabilities(checker, sym_id, span) {
                let err = checker.intern(ArType::Error);
                checker.record_expr_type(expr, err);
                return Some(err);
            }
            if checker.type_info.unsafe_functions.contains(&sym_id) && !checker.ctx.is_in_unsafe() {
                checker.diagnostics.push(
                    arandu_middle::Diagnostic::error(
                        arandu_middle::DiagCode::O013ExternRequiresUnsafe,
                        "call to an `@Unsafe` function requires an `unsafe` block",
                        span,
                    )
                    .with_label(
                        span,
                        "the function's documented preconditions must be upheld by its caller",
                    ),
                );
            }
            if let Some(&eff) = checker.type_info.function_effects.get(&sym_id) {
                checker.current_observed_effects = checker.current_observed_effects.union(eff);
            }
        }

        if let Some(sym_id) = callee_func_sym
            && checker.symbols.get(sym_id).lang_item
                == Some(arandu_middle::symbol_table::LangItem::ThreadSpawn)
            && arg_ids.len() == 1
            && matches!(checker.pool.expr(arg_ids[0]), ExprKind::Lambda { .. })
        {
            return synth_thread_spawn_closure(checker, expr, sym_id, callee_id, arg_ids[0], span);
        }

        // Infer type args for bare `id(x)` (no `id<T>`): instantiate formal params
        // before arg checking so `T` becomes the concrete argument type.
        if !had_explicit_generic
            && let Some(sym_id) = callee_func_sym
            && let Some(gp) = checker.type_info.generic_params.get(&sym_id).cloned()
            && !gp.is_empty()
        {
            let arg_tys: Vec<TypeId> = arg_ids
                .iter()
                .copied()
                .map(|aid| synth_expr(checker, aid))
                .collect();
            if let Some((ip, ir)) = infer_and_instantiate_func(
                checker,
                (Some(sym_id), &gp),
                &params,
                ret,
                &arg_tys,
                expected,
                span,
            ) {
                params = ip;
                ret = ir;
                let func_ty = ArType::func(&params, ret, &checker.type_info.type_interner);
                let inst_func = checker.intern(func_ty);
                checker.record_expr_type(callee_id, inst_func);
            }
        }

        if !is_direct {
            let diag = crate::Diagnostic::error(
                crate::DiagCode::T033IndirectCallNotSupported,
                "indirect function calls are not supported",
                span,
            )
            .with_label(
                checker.pool.expr_span(callee_id),
                "this expression evaluates to a function, but Arandu only supports direct calls",
            );
            checker.diagnostics.push(diag);
        }

        if params.len() != arg_ids.len() {
            let diag = crate::Diagnostic::error(
                crate::DiagCode::T012WrongArgCount,
                format!(
                    "expected {} arguments, found {}",
                    params.len(),
                    arg_ids.len()
                ),
                span,
            )
            .with_label(checker.pool.expr_span(callee_id), "call target is here")
            .with_label(span, format!("{} arguments provided", arg_ids.len()));
            checker.diagnostics.push(diag);
        }
        for (i, arg_id) in arg_ids.iter().copied().enumerate() {
            let formal = params.get(i).copied();
            let arg_ty_id = synth_expr_expected(checker, arg_id, formal);
            if let Some(param_id) = formal {
                check_call_arg(
                    checker,
                    arg_id,
                    param_id,
                    arg_ty_id,
                    span,
                    checker.pool.expr_span(callee_id),
                    checker.pool.expr_span(arg_id),
                    i,
                );
            }
        }

        if !is_direct {
            checker.intern(ArType::Error)
        } else {
            ret
        }
    } else if is_error {
        checker.intern(ArType::Error)
    } else {
        let callee_ty = checker.resolve(callee_ty_id);
        let interner = &checker.type_info.type_interner;
        let diag = crate::Diagnostic::error(
            crate::DiagCode::T003IncompatibleCallArg,
            format!(
                "cannot call non-function type '{}'",
                callee_ty.display(&checker.symbols, interner)
            ),
            span,
        )
        .with_label(
            checker.pool.expr_span(callee_id),
            format!(
                "this has type '{}'",
                callee_ty.display(&checker.symbols, interner)
            ),
        );
        checker.diagnostics.push(diag);
        checker.intern(ArType::Error)
    })
}
