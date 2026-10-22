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

fn walk_expr_ids(pool: &arandu_parser::ast_pool::AstPool, expr: ExprId, out: &mut Vec<ExprId>) {
    out.push(expr);
    match pool.expr(expr) {
        ExprKind::Path { .. }
        | ExprKind::TypePath { .. }
        | ExprKind::Int { .. }
        | ExprKind::Float { .. }
        | ExprKind::Bool { .. }
        | ExprKind::Char { .. }
        | ExprKind::Byte { .. }
        | ExprKind::Nil
        | ExprKind::Error => {}
        ExprKind::Field { base, .. }
        | ExprKind::SafeField { base, .. }
        | ExprKind::Unary { expr: base, .. }
        | ExprKind::Alloc { expr: base, .. }
        | ExprKind::Try { expr: base, .. }
        | ExprKind::Cast { expr: base, .. }
        | ExprKind::Group { expr: base }
        | ExprKind::ArrayRepeat { value: base, .. }
        | ExprKind::Generic { callee: base, .. } => {
            walk_expr_ids(pool, *base, out);
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NullCoalesce { left, right, .. }
        | ExprKind::Index {
            base: left,
            index: right,
        }
        | ExprKind::SafeIndex {
            base: left,
            index: right,
        } => {
            walk_expr_ids(pool, *left, out);
            walk_expr_ids(pool, *right, out);
        }
        ExprKind::Call {
            callee,
            args,
            trailing_block,
        } => {
            walk_expr_ids(pool, *callee, out);
            for &arg in pool.expr_list(*args) {
                walk_expr_ids(pool, arg, out);
            }
            if let Some(tb) = trailing_block {
                walk_block(pool, pool.block(*tb), out);
            }
        }
        ExprKind::Array { items } => {
            for &item in pool.expr_list(*items) {
                walk_expr_ids(pool, item, out);
            }
        }
        ExprKind::StructLiteral { fields, .. } => {
            for &field in pool.field_init_list(*fields) {
                walk_expr_ids(pool, pool.field_init(field).value, out);
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            match condition {
                arandu_parser::Condition::Expr { expr: e, .. }
                | arandu_parser::Condition::Is { expr: e, .. } => {
                    walk_expr_ids(pool, *e, out);
                }
                arandu_parser::Condition::And { conditions, .. } => {
                    for c in conditions.iter() {
                        if let arandu_parser::Condition::Expr { expr: e, .. }
                        | arandu_parser::Condition::Is { expr: e, .. } = c
                        {
                            walk_expr_ids(pool, *e, out);
                        }
                    }
                }
            }
            walk_block(pool, pool.block(*then_block), out);
            walk_block(pool, pool.block(*else_block), out);
        }
        ExprKind::Match { value, arms } => {
            walk_expr_ids(pool, *value, out);
            for &arm in pool.match_arm_list(*arms) {
                let arm_data = pool.match_arm(arm);
                match &arm_data.body {
                    arandu_parser::MatchArmBody::Expr { expr: e, .. } => {
                        walk_expr_ids(pool, *e, out)
                    }
                    arandu_parser::MatchArmBody::Block { block: b, .. } => walk_block(pool, b, out),
                }
            }
        }
        ExprKind::Catch {
            expr: base,
            handler,
        } => {
            walk_expr_ids(pool, *base, out);
            match pool.catch_handler(*handler) {
                arandu_parser::CatchHandler::Expr { expr: e, .. } => walk_expr_ids(pool, *e, out),
                arandu_parser::CatchHandler::Block { block: b, .. } => walk_block(pool, b, out),
            }
        }
        ExprKind::Lambda { .. } => {}
        _ => {}
    }
}

fn walk_block(
    pool: &arandu_parser::ast_pool::AstPool,
    block: &arandu_parser::Block,
    out: &mut Vec<ExprId>,
) {
    for &stmt_id in pool.stmt_list(block.statements) {
        match pool.stmt(stmt_id) {
            arandu_parser::Stmt::Expr { expr, .. } | arandu_parser::Stmt::Free { expr, .. } => {
                walk_expr_ids(pool, *expr, out);
            }
            arandu_parser::Stmt::VarDecl { value, .. } | arandu_parser::Stmt::Set { value, .. } => {
                walk_expr_ids(pool, *value, out);
            }
            arandu_parser::Stmt::Return { values, .. } => {
                for v in values {
                    walk_expr_ids(pool, *v, out);
                }
            }
            arandu_parser::Stmt::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                match condition {
                    arandu_parser::Condition::Expr { expr: e, .. }
                    | arandu_parser::Condition::Is { expr: e, .. } => {
                        walk_expr_ids(pool, *e, out);
                    }
                    arandu_parser::Condition::And { conditions, .. } => {
                        for c in conditions.iter() {
                            if let arandu_parser::Condition::Expr { expr: e, .. }
                            | arandu_parser::Condition::Is { expr: e, .. } = c
                            {
                                walk_expr_ids(pool, *e, out);
                            }
                        }
                    }
                }
                walk_block(pool, then_block, out);
                if let Some(eb) = else_block {
                    walk_block(pool, eb, out);
                }
            }
            arandu_parser::Stmt::While {
                condition, body, ..
            } => {
                if let arandu_parser::Condition::Expr { expr: e, .. }
                | arandu_parser::Condition::Is { expr: e, .. } = condition
                {
                    walk_expr_ids(pool, *e, out);
                }
                walk_block(pool, body, out);
            }
            arandu_parser::Stmt::For { clause, body, .. } => {
                match clause {
                    arandu_parser::ForClause::In { iterable, .. } => {
                        walk_expr_ids(pool, *iterable, out);
                    }
                    arandu_parser::ForClause::CStyle { condition, .. } => {
                        if let Some(c) = condition {
                            walk_expr_ids(pool, *c, out);
                        }
                    }
                }
                walk_block(pool, body, out);
            }
            arandu_parser::Stmt::Match { expr, .. } => {
                walk_expr_ids(pool, *expr, out);
            }
            _ => {}
        }
    }
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
        _ => unreachable!(),
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

    // Inspect captured symbols
    let mut expr_ids = Vec::new();
    match &body {
        arandu_parser::LambdaBody::Expr {
            expr: inner_expr, ..
        } => {
            walk_expr_ids(checker.pool, *inner_expr, &mut expr_ids);
        }
        arandu_parser::LambdaBody::Block { block, .. } => {
            walk_block(checker.pool, block, &mut expr_ids);
        }
    }

    let mut captures = Vec::new();
    let mut seen_captures = rustc_hash::FxHashSet::default();
    let mut rejected = false;
    for eid in expr_ids {
        if let Some(sym_id) = checker.resolved.expr_symbol(eid) {
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
                let expr_span = checker.pool.expr_span(eid);
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
                } else if seen_captures.insert(sym_id) {
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
            let b_ty = crate::type_checker::check::check_block(checker, checker.pool, block);
            checker.intern(b_ty)
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
