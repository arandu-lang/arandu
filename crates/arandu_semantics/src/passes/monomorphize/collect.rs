use arandu_diagnostics::{DiagCode, Diagnostic};
use arandu_lexer::Span;
use arandu_middle::hir::{
    HirCatchHandler, HirCondition, HirDecl, HirExprId, HirExprKind, HirLambdaBody, HirMatchArmBody,
    HirProgram, HirSimpleStmt, HirStmt, HirStmtKind,
};
use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::{ArType, TypeId, TypeInterner};
use arandu_typeck::TypeCheckResult;

use super::graph::{InstantiationGraph, InstantiationKey, InstantiationNodeId, MonoError};

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(tc, hir))]
pub fn analyze_instantiations<'bump>(
    tc: &TypeCheckResult,
    hir: &HirProgram,
    bump: &'bump bumpalo::Bump,
) -> Result<InstantiationGraph<'bump>, Vec<Diagnostic>> {
    let mut analyzer = InstantiationAnalyzer {
        tc,
        hir,
        interner: &tc.type_info.type_interner,
        bump,
        graph: InstantiationGraph::new(bump),
        diagnostics: Vec::new(),
    };

    for &decl_id in &hir.decls {
        let decl = hir.pool.decl(decl_id);
        if let HirDecl::Func(func) = decl
            && let Some(body_id) = func.body
        {
            let current = analyzer.current_generic_node(func.symbol);
            analyzer.visit_block(body_id, current);
        }
    }

    if let Some(cycle) = analyzer.graph.find_cycle() {
        let names: Vec<String> = cycle
            .iter()
            .map(|node| analyzer.graph.get_node(*node).mangled_name.to_string())
            .collect();
        analyzer.diagnostics.push(Diagnostic::error(
            DiagCode::G001GenericInstantiationCycle,
            format!(
                "generic instantiation cycle detected: {}",
                names.join(" -> ")
            ),
            Span::new(0, 0, 0),
        ));
    }

    if analyzer.diagnostics.is_empty() {
        Ok(analyzer.graph)
    } else {
        Err(analyzer.diagnostics)
    }
}

struct InstantiationAnalyzer<'a, 'bump> {
    tc: &'a TypeCheckResult,
    hir: &'a HirProgram,
    interner: &'a TypeInterner,
    bump: &'bump bumpalo::Bump,
    graph: InstantiationGraph<'bump>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a, 'bump> InstantiationAnalyzer<'a, 'bump> {
    fn current_generic_node(&mut self, symbol: SymbolId) -> Option<InstantiationNodeId> {
        let params = self.tc.type_info.generic_params.get(&symbol)?;
        let type_args_vec: Vec<TypeId> = params
            .iter()
            .map(|param| {
                let ty =
                    if self.tc.symbols.get(*param).kind == arandu_middle::SymbolKind::ConstParam {
                        ArType::ConstParam(*param)
                    } else {
                        ArType::named(*param, &[], self.interner)
                    };
                self.interner.intern(ty)
            })
            .collect();
        let type_args = self.bump.alloc_slice_copy(&type_args_vec);
        self.insert_key(InstantiationKey { symbol, type_args }, Span::new(0, 0, 0))
    }

    fn insert_key(
        &mut self,
        key: InstantiationKey<'bump>,
        span: Span,
    ) -> Option<InstantiationNodeId> {
        match self
            .graph
            .get_or_insert(&key, self.bump, self.interner, &self.tc.symbols)
        {
            Ok(node) => Some(node),
            Err(MonoError::RecursionLimitExceeded { symbol, limit }) => {
                self.diagnostics.push(Diagnostic::error(
                    DiagCode::G002GenericInstantiationLimit,
                    format!(
                        "generic instantiation recursion limit exceeded for `{}` (limit {limit})",
                        self.tc.symbols.get(symbol).name
                    ),
                    span,
                ));
                None
            }
        }
    }

    fn visit_block(
        &mut self,
        block: arandu_middle::hir::HirBlockId,
        current: Option<InstantiationNodeId>,
    ) {
        let blk = self.hir.pool.block(block);
        for &stmt_id in self.hir.pool.stmt_list(blk.statements) {
            let stmt = self.hir.pool.stmt(stmt_id);
            self.visit_stmt(stmt, current);
        }
    }

    fn visit_stmt(&mut self, stmt: &HirStmt, current: Option<InstantiationNodeId>) {
        match &stmt.kind {
            HirStmtKind::VarDecl { value, .. }
            | HirStmtKind::Expr(value)
            | HirStmtKind::Free(value) => self.visit_expr(*value, current),
            HirStmtKind::Set { value, .. } => self.visit_expr(*value, current),
            HirStmtKind::Return { values } => {
                for &value in self.hir.pool.expr_list(*values) {
                    self.visit_expr(value, current);
                }
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.visit_condition(condition, current);
                self.visit_block(*then_block, current);
                if let Some(block) = else_block {
                    self.visit_block(*block, current);
                }
            }
            HirStmtKind::For { clause, body } => {
                match clause {
                    arandu_middle::hir::HirForClause::In { iterable, .. } => {
                        self.visit_expr(*iterable, current);
                    }
                    arandu_middle::hir::HirForClause::CStyle {
                        init,
                        condition,
                        step,
                        ..
                    } => {
                        if let Some(init) = init {
                            self.visit_simple_stmt(init, current);
                        }
                        if let Some(condition) = condition {
                            self.visit_expr(*condition, current);
                        }
                        if let Some(step) = step {
                            self.visit_simple_stmt(step, current);
                        }
                    }
                }
                self.visit_block(*body, current);
            }
            HirStmtKind::While { condition, body } => {
                self.visit_condition(condition, current);
                self.visit_block(*body, current);
            }
            HirStmtKind::Match { value, arms } => {
                self.visit_expr(*value, current);
                for arm in self.hir.pool.match_arms_list(*arms) {
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(*guard, current);
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(expr) => self.visit_expr(*expr, current),
                        HirMatchArmBody::Block(block) => self.visit_block(*block, current),
                    }
                }
            }
            HirStmtKind::Defer(block)
            | HirStmtKind::ErrDefer(block)
            | HirStmtKind::Unsafe(block) => {
                self.visit_block(*block, current);
            }
            HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}
        }
    }

    fn visit_simple_stmt(&mut self, stmt: &HirSimpleStmt, current: Option<InstantiationNodeId>) {
        match stmt {
            HirSimpleStmt::VarDecl { value, .. }
            | HirSimpleStmt::Set { value, .. }
            | HirSimpleStmt::Expr(value) => self.visit_expr(*value, current),
        }
    }

    fn visit_condition(&mut self, condition: &HirCondition, current: Option<InstantiationNodeId>) {
        match condition {
            HirCondition::Expr(expr) | HirCondition::Is { expr, .. } => {
                self.visit_expr(*expr, current);
            }
            HirCondition::And(conditions) => {
                for condition in conditions {
                    self.visit_condition(condition, current);
                }
            }
        }
    }

    fn visit_expr(&mut self, expr_id: HirExprId, current: Option<InstantiationNodeId>) {
        let expr = self.hir.pool.expr(expr_id);
        match &expr.kind {
            HirExprKind::Generic { callee, args } => {
                self.visit_expr(*callee, current);
                if let Some(symbol) = generic_callee_symbol(*callee, self.hir, self.tc) {
                    // HIR generic args are already interned TypeIds.
                    let type_args = self.bump.alloc_slice_copy(args);
                    let key = InstantiationKey { symbol, type_args };
                    if let Some(callee_node) = self.insert_key(key, expr.span)
                        && let Some(caller_node) = current
                    {
                        self.graph.add_edge(caller_node, callee_node);
                    }
                }
            }
            HirExprKind::Field { base, .. }
            | HirExprKind::SafeField { base, .. }
            | HirExprKind::Alloc { expr: base }
            | HirExprKind::Try { expr: base }
            | HirExprKind::Cast { expr: base, .. }
            | HirExprKind::Unary { expr: base, .. }
            | HirExprKind::ToStr { value: base } => self.visit_expr(*base, current),
            HirExprKind::Index { base, index } | HirExprKind::SafeIndex { base, index } => {
                self.visit_expr(*base, current);
                self.visit_expr(*index, current);
            }
            HirExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                self.visit_expr(*callee, current);
                for &arg in self.hir.pool.expr_list(*args) {
                    self.visit_expr(arg, current);
                }
                if let Some(block) = trailing_block {
                    self.visit_block(*block, current);
                }
                // Methods/funcs whose type args come only from the receiver or
                // argument types (no `Generic` node): still need mono keys.
                // Pass the call's result type so `join<T>(h)` can recover `T`
                // from the expected/inferred return type on the Call expr.
                if let Some((symbol, type_args_vec)) = instantiation_key_for_call(
                    self.hir, self.tc, *callee, *args, expr.ty, expr.span,
                ) {
                    let type_args = self.bump.alloc_slice_copy(&type_args_vec);
                    let key = InstantiationKey { symbol, type_args };
                    if let Some(callee_node) = self.insert_key(key, expr.span)
                        && let Some(caller_node) = current
                    {
                        self.graph.add_edge(caller_node, callee_node);
                    }
                }
            }
            HirExprKind::ResultCtor { value, .. } => self.visit_expr(*value, current),
            HirExprKind::StructLiteral { fields, .. } => {
                for field in self.hir.pool.field_inits_list(*fields) {
                    self.visit_expr(field.value, current);
                }
            }
            HirExprKind::Array { items } => {
                for &item in self.hir.pool.expr_list(*items) {
                    self.visit_expr(item, current);
                }
            }
            HirExprKind::Lambda { body, .. } => match body {
                HirLambdaBody::Expr(expr) => self.visit_expr(*expr, current),
                HirLambdaBody::Block(block) => self.visit_block(*block, current),
            },
            HirExprKind::AsyncBlock { block } | HirExprKind::UnsafeBlock { block } => {
                self.visit_block(*block, current);
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.visit_condition(condition, current);
                self.visit_block(*then_block, current);
                self.visit_block(*else_block, current);
            }
            HirExprKind::Match { value, arms } => {
                self.visit_expr(*value, current);
                for arm in self.hir.pool.match_arms_list(*arms) {
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(*guard, current);
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(expr) => self.visit_expr(*expr, current),
                        HirMatchArmBody::Block(block) => self.visit_block(*block, current),
                    }
                }
            }
            HirExprKind::Catch { expr, handler } => {
                self.visit_expr(*expr, current);
                match handler {
                    HirCatchHandler::Expr(expr) => self.visit_expr(*expr, current),
                    HirCatchHandler::Block { block, .. } => self.visit_block(*block, current),
                }
            }
            HirExprKind::NullCoalesce { left, right } | HirExprKind::Binary { left, right, .. } => {
                self.visit_expr(*left, current);
                self.visit_expr(*right, current);
            }
            HirExprKind::Path { .. }
            | HirExprKind::TypePath { .. }
            | HirExprKind::Int(_)
            | HirExprKind::Float(_)
            | HirExprKind::Bool(_)
            | HirExprKind::Char(_)
            | HirExprKind::Str(_)
            | HirExprKind::Nil
            | HirExprKind::Error => {}
            HirExprKind::StringInterp { parts } => {
                for part in parts {
                    if let crate::hir::HirStringPart::Expr(e) = part {
                        self.visit_expr(*e, current);
                    }
                }
            }
        }
    }
}

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
        ArType::Named(id, _) => Some(id),
        ArType::Ptr(inner) => match tc.type_info.type_interner.resolve(inner) {
            ArType::Named(id, _) => Some(id),
            _ => None,
        },
        // Builtin Result/Option methods (`expectOrAbort`) are associated to the
        // prelude type symbols after import re-index (not ArType::Named).
        ArType::Result(_, _) => tc.symbols.lookup_type(tc.symbols.global_scope(), "Result"),
        ArType::Option(_) => tc.symbols.lookup_type(tc.symbols.global_scope(), "Option"),
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
            if recv_args.len() == params.len() && !recv_args.is_empty() {
                (sym, recv_args)
            } else {
                let call_info = CallSiteInfo {
                    callee_id,
                    args,
                    result_ty: call_result_ty,
                    recv_args: &recv_args,
                };
                let inferred = infer_call_type_args(tc, sym, &params, pool, &call_info)?;
                (sym, inferred)
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

struct CallSiteInfo<'a> {
    callee_id: HirExprId,
    args: arandu_middle::hir::IndexRange,
    result_ty: arandu_middle::types::TypeId,
    recv_args: &'a [TypeId],
}

fn is_identity_args(
    tc: &TypeCheckResult,
    symbol: SymbolId,
    type_args: &[arandu_middle::types::TypeId],
) -> bool {
    let Some(params) = tc.type_info.generic_params.get(&symbol) else {
        return false;
    };
    if params.len() != type_args.len() {
        return false;
    }
    let interner = &tc.type_info.type_interner;
    params.iter().zip(type_args.iter()).all(|(&param, &tid)| {
        matches!(
            interner.resolve(tid),
            ArType::Named(id, ref args) if id == param && args.is_empty()
        ) || matches!(interner.resolve(tid), ArType::ConstParam(id) if id == param)
    })
}

/// Infer function/method type arguments by matching formal param types against
/// arg expr types, plus the specialized callee type typeck recorded on the
/// callee expr (covers `join<T>(h)` where `T` only appears in the return type).
fn infer_call_type_args(
    tc: &TypeCheckResult,
    symbol: SymbolId,
    params: &[SymbolId],
    pool: &arandu_middle::hir::HirPool,
    call: &CallSiteInfo<'_>,
) -> Option<Vec<arandu_middle::types::TypeId>> {
    let func_ty = tc.type_info.decl_type(symbol)?;
    let ArType::Func(formals, ret) = func_ty else {
        return None;
    };
    let arg_ids = pool.expr_list(call.args);
    let interner = &tc.type_info.type_interner;
    let formals_vec = interner.type_args(formals);
    if formals_vec.len() != arg_ids.len() {
        return None;
    }

    // param_sym → concrete TypeId
    let mut bindings: rustc_hash::FxHashMap<SymbolId, arandu_middle::types::TypeId> =
        rustc_hash::FxHashMap::default();

    for (&param_sym, &concrete_tid) in params.iter().zip(call.recv_args.iter()) {
        bindings.insert(param_sym, concrete_tid);
    }

    for (&formal_id, &arg_eid) in formals_vec.iter().zip(arg_ids.iter()) {
        let formal = interner.resolve(formal_id);
        let arg_ty_id = pool.expr(arg_eid).ty;
        collect_param_bindings(interner, params, &formal, arg_ty_id, &mut bindings);
    }

    // Specialized Func type on the callee (typeck inference → HIR .ty).
    let cal_ty = interner.resolve(pool.expr(call.callee_id).ty);
    if let ArType::Func(spec_formals, spec_ret) = cal_ty {
        let spec_formals_vec = interner.type_args(spec_formals);
        for (&orig, &spec) in formals_vec.iter().zip(spec_formals_vec.iter()) {
            let formal = interner.resolve(orig);
            collect_param_bindings(interner, params, &formal, spec, &mut bindings);
        }
        let ret_formal = interner.resolve(ret);
        collect_param_bindings(interner, params, &ret_formal, spec_ret, &mut bindings);
    }

    // Call expression result type (e.g. `return join(h)` expects int).
    {
        let ret_formal = interner.resolve(ret);
        collect_param_bindings(interner, params, &ret_formal, call.result_ty, &mut bindings);
    }

    // Deduce unbound type parameters from constraints of bound parameters.
    for &p in params {
        if bindings.contains_key(&p) {
            continue;
        }
        let bound_pairs: Vec<(SymbolId, arandu_middle::types::TypeId)> =
            bindings.iter().map(|(&k, &v)| (k, v)).collect();
        for (b_sym, b_tid) in bound_pairs {
            let Some(constraints) = tc.type_info.param_constraints.get(&b_sym) else {
                continue;
            };
            for bound in constraints.iter() {
                let Some(iface_params) = tc.type_info.generic_params.get(&bound.iface_sym) else {
                    continue;
                };
                for (&iface_param_sym, &arg_tid) in iface_params.iter().zip(bound.type_args.iter())
                {
                    if let ArType::Named(arg_sym, _) = interner.resolve(arg_tid)
                        && arg_sym == p
                        && let Some(deduced) =
                            deduce_mono_iface_param(tc, b_tid, bound.iface_sym, iface_param_sym)
                    {
                        bindings.insert(p, deduced);
                        break;
                    }
                }
                if bindings.contains_key(&p) {
                    break;
                }
            }
            if bindings.contains_key(&p) {
                break;
            }
        }
    }

    let mut out = Vec::with_capacity(params.len());
    for &p in params {
        let tid = *bindings.get(&p)?;
        if matches!(interner.resolve(tid), ArType::Error) {
            return None;
        }
        out.push(tid);
    }
    Some(out)
}

fn deduce_mono_iface_param(
    tc: &TypeCheckResult,
    concrete_tid: arandu_middle::types::TypeId,
    iface_sym: SymbolId,
    iface_param_sym: SymbolId,
) -> Option<arandu_middle::types::TypeId> {
    let interner = &tc.type_info.type_interner;
    let concrete = interner.resolve(concrete_tid);
    let type_id = match concrete {
        ArType::Named(id, _) => id,
        ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
            match interner.resolve(inner) {
                ArType::Named(id, _) => id,
                _ => return None,
            }
        }
        _ => return None,
    };
    let iface = tc.type_info.interfaces.get(&iface_sym)?;
    for m in &iface.methods {
        let method_name = &m.name;
        let required_id = m.sig_id;
        let required_ty = interner.resolve(required_id);
        let Some(prov_sym) = tc.symbols.lookup_associated_member(type_id, method_name) else {
            continue;
        };
        let Some(mut prov_ty) = tc.type_info.decl_type(prov_sym) else {
            continue;
        };
        if let Some(prov_gp) = tc.type_info.generic_params.get(&prov_sym)
            && let ArType::Func(prov_p, _) = &prov_ty
        {
            let prov_args = tc.type_info.type_interner.type_args(*prov_p);
            if let Some(&self_fid) = prov_args.first() {
                let self_formal = interner.resolve(self_fid);
                let mut prov_bindings = rustc_hash::FxHashMap::default();
                collect_param_bindings(
                    interner,
                    prov_gp,
                    &self_formal,
                    concrete_tid,
                    &mut prov_bindings,
                );
                let prov_concrete: Vec<TypeId> = prov_gp
                    .iter()
                    .map(|p| {
                        prov_bindings
                            .get(p)
                            .copied()
                            .unwrap_or_else(|| interner.intern(ArType::named(*p, &[], interner)))
                    })
                    .collect();
                let subst =
                    arandu_middle::types::build_subst_ids(prov_gp, &prov_concrete, interner);
                if let Some(decl_tid) = tc.type_info.decl_type_id(prov_sym) {
                    let inst = arandu_middle::types::substitute_type_id(decl_tid, &subst, interner);
                    prov_ty = interner.resolve(inst);
                }
            }
        }
        let mut iface_bindings = rustc_hash::FxHashMap::default();
        if let (ArType::Func(req_p, req_ret), ArType::Func(prov_p, prov_ret)) =
            (&required_ty, &prov_ty)
        {
            collect_param_bindings(
                interner,
                &[iface_param_sym],
                &interner.resolve(*req_ret),
                *prov_ret,
                &mut iface_bindings,
            );
            let req_args = tc.type_info.type_interner.type_args(*req_p);
            let prov_args = tc.type_info.type_interner.type_args(*prov_p);
            for (&r, &pr) in req_args.iter().zip(prov_args.iter()) {
                collect_param_bindings(
                    interner,
                    &[iface_param_sym],
                    &interner.resolve(r),
                    pr,
                    &mut iface_bindings,
                );
            }
        }
        if let Some(&deduced) = iface_bindings.get(&iface_param_sym) {
            return Some(deduced);
        }
    }
    None
}

fn collect_param_bindings(
    interner: &arandu_middle::types::TypeInterner,
    type_params: &[SymbolId],
    formal: &ArType,
    actual_id: arandu_middle::types::TypeId,
    bindings: &mut rustc_hash::FxHashMap<SymbolId, arandu_middle::types::TypeId>,
) {
    match formal {
        ArType::ConstParam(id) if type_params.contains(id) => {
            bindings.entry(*id).or_insert(actual_id);
        }
        ArType::Named(id, args) if args.is_empty() && type_params.contains(id) => {
            bindings.entry(*id).or_insert(actual_id);
        }
        ArType::Named(_, args) => {
            let actual = interner.resolve(actual_id);
            let act_args = match actual {
                ArType::Named(_, a) => Some(a),
                _ => None,
            };
            if let Some(act_args) = act_args
                && args.len == act_args.len
            {
                let formal_args = interner.type_args(*args);
                let actual_args = interner.type_args(act_args);
                for (&fa, &aa) in formal_args.iter().zip(actual_args.iter()) {
                    let fty = interner.resolve(fa);
                    collect_param_bindings(interner, type_params, &fty, aa, bindings);
                }
            }
        }
        // Auto-ref: bare `Vec<int>` matches formal `&Vec<T>` / `&mut Vec<T>`.
        ArType::Ref(inner) | ArType::RefMut(inner) => {
            let actual = interner.resolve(actual_id);
            let act_inner = match actual {
                ArType::Ref(i) | ArType::RefMut(i) | ArType::Ptr(i) => i,
                _ => actual_id,
            };
            let fty = interner.resolve(*inner);
            collect_param_bindings(interner, type_params, &fty, act_inner, bindings);
        }
        ArType::Ptr(inner)
        | ArType::Nullable(inner)
        | ArType::Slice(inner)
        | ArType::Option(inner)
        | ArType::Array(_, inner)
        | ArType::Coroutine(inner)
        | ArType::Poll(inner)
        | ArType::Range(inner) => {
            let actual = interner.resolve(actual_id);
            let act_inner = match actual {
                ArType::Ptr(i)
                | ArType::Nullable(i)
                | ArType::Slice(i)
                | ArType::Option(i)
                | ArType::Array(_, i)
                | ArType::Coroutine(i)
                | ArType::Poll(i)
                | ArType::Range(i) => Some(i),
                _ => None,
            };
            if let Some(ai) = act_inner {
                let fty = interner.resolve(*inner);
                collect_param_bindings(interner, type_params, &fty, ai, bindings);
            }
        }
        ArType::ConstArray(param, inner) => {
            if let ArType::Array(length, actual_inner) = interner.resolve(actual_id) {
                if type_params.contains(param) {
                    let length_id = interner.intern(ArType::Const(length));
                    bindings.entry(*param).or_insert(length_id);
                }
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*inner),
                    actual_inner,
                    bindings,
                );
            }
        }
        ArType::Result(ok, err) => {
            if let ArType::Result(aok, aerr) = interner.resolve(actual_id) {
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*ok),
                    aok,
                    bindings,
                );
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*err),
                    aerr,
                    bindings,
                );
            }
        }
        ArType::Func(fps, fret) => {
            if let ArType::Func(aps, aret) = interner.resolve(actual_id)
                && fps.len == aps.len
            {
                let formal_fps = interner.type_args(*fps);
                let actual_aps = interner.type_args(aps);
                for (&fp, &ap) in formal_fps.iter().zip(actual_aps.iter()) {
                    collect_param_bindings(
                        interner,
                        type_params,
                        &interner.resolve(fp),
                        ap,
                        bindings,
                    );
                }
                collect_param_bindings(
                    interner,
                    type_params,
                    &interner.resolve(*fret),
                    aret,
                    bindings,
                );
            }
        }
        ArType::Tuple(items) => {
            if let ArType::Tuple(acts) = interner.resolve(actual_id)
                && items.len == acts.len
            {
                let formal_items = interner.type_args(*items);
                let actual_items = interner.type_args(acts);
                for (&fi, &ai) in formal_items.iter().zip(actual_items.iter()) {
                    collect_param_bindings(
                        interner,
                        type_params,
                        &interner.resolve(fi),
                        ai,
                        bindings,
                    );
                }
            }
        }
        _ => {}
    }
}
