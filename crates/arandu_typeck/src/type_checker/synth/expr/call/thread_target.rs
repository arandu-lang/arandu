use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind};

use super::thread_capture;
use crate::type_checker::TypeChecker;
use crate::type_checker::types::ArType;

use arandu_middle::types::type_interner::TypeId;

/// Returns true when the call is rejected because the target has no OS.
/// WebAssembly keeps its `U001` and returns false so the existing spawn
/// checks still run.
pub(crate) fn check_target_thread_capabilities(
    checker: &mut TypeChecker<'_>,
    sym_id: arandu_middle::SymbolId,
    span: Span,
) -> bool {
    let Some(lang_item) = checker.symbols.get(sym_id).lang_item else {
        return false;
    };
    if checker.type_info.target_identity.is_bare_metal() {
        let module = match lang_item {
            arandu_middle::symbol_table::LangItem::ThreadParkTimeout
            | arandu_middle::symbol_table::LangItem::ThreadPark
            | arandu_middle::symbol_table::LangItem::ThreadUnpark
            | arandu_middle::symbol_table::LangItem::ThreadSpawn
            | arandu_middle::symbol_table::LangItem::StdThread => "std.thread",
            arandu_middle::symbol_table::LangItem::StdFs => "std.fs",
            _ => return false,
        };
        let identity = &checker.type_info.target_identity;
        let target = if identity.triple.is_empty() {
            format!("{}-{}", identity.arch, identity.os)
        } else {
            identity.triple.clone()
        };
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T099RequiresOperatingSystem,
                format!(
                    "The module '{module}' requires an underlying operating system, but target '{target}' is bare-metal."
                ),
                span,
            )
            .with_label(span, "this operation needs an operating system"),
        );
        return true;
    }
    if !checker.type_info.target_identity.arch.starts_with("wasm") {
        return false;
    }
    match lang_item {
        arandu_middle::symbol_table::LangItem::ThreadParkTimeout => {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::U001FeatureNotSupported,
                    "`parkTimeout` requires target thread support to wake on unpark, which is unavailable on wasm32; use `std.thread.sleep` for a non-interruptible delay",
                    span,
                )
                .with_label(
                    span,
                    "`parkTimeout` cannot be used on WebAssembly without threads",
                ),
            );
        }
        arandu_middle::symbol_table::LangItem::ThreadPark
        | arandu_middle::symbol_table::LangItem::ThreadUnpark
        | arandu_middle::symbol_table::LangItem::ThreadSpawn => {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::U001FeatureNotSupported,
                    "thread synchronization and spawning are unsupported on single-threaded WebAssembly targets; use cooperative tasks or async execution instead",
                    span,
                )
                .with_label(
                    span,
                    "thread operations are unsupported on this target",
                ),
            );
        }
        _ => {}
    }
    false
}

fn span_contains(outer: Span, inner: Span) -> bool {
    outer.file_id == inner.file_id && outer.start <= inner.start && outer.end >= inner.end
}

/// Every resolved value use inside the lambda, in source order.
///
/// Captures are read from the resolver's tables instead of a syntax walk:
/// a hand-written walker silently drops uses inside forms it forgets
/// (interpolation, match statements, nested lambdas, assignment targets),
/// and a dropped capture leaves the `run` body naming a local of another
/// function. Expression uses come from `expr_symbols`; assignment roots,
/// which are places rather than expressions, come from `value_refs`.
fn value_uses_within(
    checker: &TypeChecker<'_>,
    lambda: Span,
) -> Vec<(Span, arandu_middle::SymbolId)> {
    let mut uses = Vec::new();
    for (index, symbol) in checker.resolved.expr_symbols.iter().enumerate() {
        let (Some(symbol), Some(span)) = (*symbol, checker.pool.expr_spans.get(index)) else {
            continue;
        };
        if *span != lambda && span_contains(lambda, *span) {
            uses.push((*span, symbol));
        }
    }
    for (key, symbol) in &checker.resolved.value_refs {
        let span = Span::new(lambda.file_id, key.start, key.end);
        if span_contains(lambda, span) {
            uses.push((span, *symbol));
        }
    }
    uses.sort_unstable_by_key(|(span, symbol)| (span.start, span.end, *symbol));
    uses.dedup();
    uses
}

pub(crate) fn synth_thread_spawn_closure(
    checker: &mut TypeChecker<'_>,
    call_expr: ExprId,
    spawn_sym: arandu_middle::SymbolId,
    callee_id: ExprId,
    arg_id: ExprId,
    _span: Span,
) -> Option<TypeId> {
    let arg_span = checker.pool.expr_span(arg_id);
    let (params, body, is_move) = match checker.pool.expr(arg_id) {
        ExprKind::Lambda {
            params,
            body,
            is_move,
        } => (*params, body.clone(), *is_move),
        _ => {
            checker.diagnostics.push(crate::Diagnostic::ice(
                crate::DiagCode::ICET001,
                "thread closure lowering was reached for an expression that is not a lambda",
                arg_span,
            ));
            return Some(checker.intern(ArType::Error));
        }
    };

    if !is_move {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "thread closure requires explicit 'move || ...' to transfer captured values into the new thread",
                arg_span,
            )
            .with_label(arg_span, "missing 'move' keyword on thread closure"),
        );
        return Some(checker.intern(ArType::Error));
    }

    if !params.is_empty() {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "thread closure cannot take parameters; use 'move || ...'",
                arg_span,
            )
            .with_label(arg_span, "thread closure must take no parameters"),
        );
        return Some(checker.intern(ArType::Error));
    }

    let mut captures = Vec::new();
    let mut seen_captures = rustc_hash::FxHashSet::default();
    let mut rejected = false;
    for (expr_span, sym_id) in value_uses_within(checker, arg_span) {
        // One verdict per captured symbol: the first use names the capture.
        if seen_captures.contains(&sym_id) {
            continue;
        }
        {
            let (is_captured_var, sym_name) = {
                let sym = checker.symbols.get(sym_id);
                (
                    matches!(
                        sym.kind,
                        arandu_middle::SymbolKind::Local | arandu_middle::SymbolKind::Param
                    ) && !span_contains(arg_span, sym.span),
                    sym.name.clone(),
                )
            };
            if is_captured_var {
                seen_captures.insert(sym_id);
                let cap_ty_id = checker
                    .ctx
                    .lookup(sym_id)
                    .or_else(|| checker.decl_type_id(sym_id))
                    .unwrap_or_else(|| checker.intern(ArType::Error));
                let cap_ty = checker.resolve(cap_ty_id);
                if matches!(cap_ty, ArType::Ref(_) | ArType::RefMut(_)) {
                    rejected = true;
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T025InterfaceNotSatisfied,
                            "cannot move borrowed value into thread closure: references do not satisfy 'marker.Send' and cannot escape to another thread",
                            expr_span,
                        )
                        .with_label(expr_span, "reference cannot cross thread boundaries"),
                    );
                } else if !cap_ty.is_error()
                    && !crate::type_checker::types::transfer::satisfies(
                        checker,
                        &cap_ty,
                        arandu_middle::symbol_table::LangItem::Send,
                    )
                {
                    rejected = true;
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T025InterfaceNotSatisfied,
                            format!(
                                "captured variable '{}' of type '{}' does not satisfy 'marker.Send'",
                                sym_name,
                                cap_ty.display(&checker.symbols, &checker.type_info.type_interner)
                            ),
                            expr_span,
                        )
                        .with_label(expr_span, "non-Send type cannot cross thread boundaries"),
                    );
                } else if cap_ty.is_error() {
                    rejected = true;
                } else {
                    captures.push(thread_capture::OwnedCapture {
                        symbol: sym_id,
                        name: sym_name,
                        ty: cap_ty_id,
                        span: expr_span,
                    });
                }
            }
        }
    }

    // Synthesize the closure body to determine return type
    let ret_ty_id = match &body {
        arandu_parser::LambdaBody::Expr {
            expr: inner_expr, ..
        } => super::synth_expr(checker, *inner_expr),
        arandu_parser::LambdaBody::Block { block, .. } => {
            crate::type_checker::check::check_thread_closure_block(checker, block)
        }
    };
    let ret_ty = checker.resolve(ret_ty_id);
    if ret_ty.is_error() {
        rejected = true;
    } else if !crate::type_checker::types::transfer::satisfies(
        checker,
        &ret_ty,
        arandu_middle::symbol_table::LangItem::Send,
    ) {
        rejected = true;
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T025InterfaceNotSatisfied,
                format!(
                    "thread return type '{}' does not satisfy 'marker.Send'",
                    ret_ty.display(&checker.symbols, &checker.type_info.type_interner)
                ),
                arg_span,
            )
            .with_label(arg_span, "thread return type must satisfy 'marker.Send'"),
        );
    }

    let generic_params = checker
        .type_info
        .generic_params
        .get(&spawn_sym)
        .cloned()
        .unwrap_or_default();
    let decl_ty = checker.decl_type(spawn_sym);
    let res_ty = if let (Some(ArType::Func(_, ret_ty)), Some(&first_gp)) =
        (decl_ty, generic_params.first())
    {
        let ret = checker.resolve(ret_ty);
        let subst = arandu_middle::types::build_subst(&[first_gp], &[checker.resolve(ret_ty_id)]);
        let inst =
            arandu_middle::types::substitute_type(&ret, &subst, &checker.type_info.type_interner);
        checker.intern(inst)
    } else {
        checker.intern(ArType::Error)
    };

    if rejected {
        let err = checker.intern(ArType::Error);
        checker.record_expr_type(arg_id, err);
        checker.record_expr_type(call_expr, err);
        return Some(err);
    }
    Some(thread_capture::install_thread_capture(
        checker,
        thread_capture::CaptureInstall {
            call_expr,
            callee_id,
            arg_id,
            arg_span,
            captures: &captures,
            ret_ty: ret_ty_id,
            res_ty,
        },
    ))
}
