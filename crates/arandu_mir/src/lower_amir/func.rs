use super::{CalleeArgModes, LowerCtx, MoveState};
use crate::TypeCheckResult;
use crate::amir::{AmirFunc, AmirOperand, AmirTemp, AmirTerminator, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirBlockId, HirFunc, HirProgram};
use crate::literal_pool::AmirLiteralPool;
use rustc_hash::{FxHashMap, FxHashSet};

#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_func(
    f: &HirFunc,
    body: HirBlockId,
    tc: &TypeCheckResult,
    hir: &HirProgram,
    arg_modes: &CalleeArgModes,
    literal_pool: &mut AmirLiteralPool,
    func_diagnostics: &mut Vec<Diagnostic>,
    pointer_width: u64,
) -> Result<(AmirFunc, Vec<(TempId, crate::amir::LocalId)>), Diagnostic> {
    let mut ctx = LowerCtx {
        tc,
        hir,
        arg_modes,
        func_return_type: f.return_type,
        func_is_async: f.is_async,
        coroutine_depth: 0,
        locals: Vec::new(),
        temps: Vec::new(),
        builder: super::builder::AmirBuilder::new(),
        symbol_map: FxHashMap::default(),
        loop_stack: Vec::new(),
        literal_pool,
        defer_frames: Vec::new(),
        temp_states: Vec::new(),
        temp_origins: Vec::new(),
        debug_bindings: Vec::new(),
        local_states: Vec::new(),
        sealed_blocks: FxHashSet::default(),
        current_def: FxHashMap::default(),
        incomplete_phis: FxHashMap::default(),
        redirected_temps: FxHashMap::default(),
        current_span: arandu_lexer::Span::new(0, 0, 0),
        pointer_width,
        owned_string_temps: FxHashSet::default(),
    };

    // Return register is TempId(0) — span is the function header.
    let ret_ty = f.return_type;
    let ret_is_copy = tc.type_info.is_copy(ret_ty);
    let ret_is_nullable = tc
        .type_info
        .type_interner
        .with_type(ret_ty, |t| matches!(t, crate::types::ArType::Nullable(_)));
    ctx.temps.push(AmirTemp {
        id: TempId(0),
        ty: ret_ty,
        is_copy: ret_is_copy,
        is_nullable: ret_is_nullable,
        span: f.span,
    });
    ctx.temp_states.push(MoveState::Available);
    ctx.temp_origins.push(None);

    let mut params = Vec::new();
    let mut receiver = None;

    // Start with bb0 so we can emit parameter store instructions there
    let bb0 = ctx.new_block();
    ctx.sealed_blocks.insert(bb0);
    ctx.builder.current_block = Some(bb0);

    for param in hir.pool.params_list(f.params) {
        let p_temp = ctx.with_span(param.span, |this| this.new_temp_id(param.ty));
        if param.is_receiver {
            receiver = Some(crate::amir::AmirReceiver {
                temp: p_temp,
                kind: param
                    .receiver_kind
                    .unwrap_or(crate::hir::ReceiverKind::Shared),
            });
        }
        params.push(p_temp);
        // Directly bind the parameter temp to the local variable in the entry block
        let p_local = ctx.new_local_id(param.ty, param.symbol, param.span);
        ctx.write_variable_source(p_local, AmirOperand::Copy(p_temp))?;
    }

    ctx.current_span = f.span;
    // SYN.1: only when the last statement is an expression (implicit return).
    // Empty bodies / last=`return`/`if`/… keep ordinary `lower_block` so we do
    // not invent a `Nil` store into a `void` return temp (breaks C backend).
    let last_is_expr = {
        let stmts = hir.pool.stmt_list(hir.pool.block(body).statements);
        stmts
            .last()
            .is_some_and(|&sid| matches!(hir.pool.stmt(sid).kind, crate::hir::HirStmtKind::Expr(_)))
    };
    if last_is_expr {
        // Async bodies return bare `T` in source; wrap as `CoroutineReady` (A3).
        if f.is_async {
            if let crate::types::ArType::Coroutine(payload_ty) =
                tc.type_info.type_interner.resolve(ret_ty)
            {
                ctx.lower_block_as_expr_async_tail(body, payload_ty, &tc.symbols)?;
            } else {
                ctx.lower_block_as_expr(body, Some(TempId(0)), &tc.symbols)?;
            }
        } else {
            ctx.lower_block_as_expr(body, Some(TempId(0)), &tc.symbols)?;
        }
    } else {
        ctx.lower_block(body, &tc.symbols)?;
    }

    // If last block does not have a terminator, implicitly return
    if let Some(curr) = ctx.builder.current_block
        && ctx.builder.blocks[curr.as_usize()]
            .terminator
            .is_unreachable()
    {
        ctx.builder.blocks[curr.as_usize()].terminator = AmirTerminator::Return;
    }

    // Seal all remaining unsealed blocks
    for i in 0..ctx.builder.blocks.len() {
        ctx.seal_block(crate::amir::BlockId::from_usize(i));
    }

    // OSSA Optimization passes
    // 1. Snapshot func for validation without cloning Vecs (take → check → put back).
    let raw_cfg = crate::cfg::compute_cfg_edges(&ctx.builder.blocks);
    let raw_block_params = ctx.builder.materialize_block_params();
    let mut raw_func = AmirFunc {
        symbol: f.symbol,
        return_type: ret_ty,
        receiver,
        params: params.clone(),
        locals: std::mem::take(&mut ctx.locals),
        temps: std::mem::take(&mut ctx.temps),
        blocks: std::mem::take(&mut ctx.builder.blocks),
        block_params: raw_block_params,
        stmts: std::mem::take(&mut ctx.builder.stmts),
        cfg: raw_cfg,
    };

    // 2. Run checks on raw func
    func_diagnostics.extend(crate::amir_validate::validate_amir_func(
        &raw_func,
        &tc.symbols,
        &tc.type_info.type_interner,
    ));
    func_diagnostics.extend(crate::definite_init::check_definite_init(
        &raw_func,
        &tc.symbols,
    ));
    func_diagnostics.extend(crate::move_checker::check_moves(&raw_func, &tc.symbols));

    // Restore into ctx for phi elimination / rewrite.
    ctx.locals = std::mem::take(&mut raw_func.locals);
    ctx.temps = std::mem::take(&mut raw_func.temps);
    ctx.builder.blocks = std::mem::take(&mut raw_func.blocks);
    ctx.builder.stmts = std::mem::take(&mut raw_func.stmts);
    ctx.builder
        .restore_block_params_scratch(std::mem::take(&mut raw_func.block_params));
    drop(raw_func);

    // 3. OSSA Optimization passes
    ctx.eliminate_trivial_phis();
    ctx.prune_eliminated_parameters();
    ctx.rewrite_all_operands();

    let mut debug_bindings = std::mem::take(&mut ctx.debug_bindings)
        .into_iter()
        .filter_map(|(temp, local)| ctx.canonical_debug_temp(temp).map(|temp| (temp, local)))
        .collect::<Vec<_>>();
    debug_bindings.sort_unstable_by_key(|(temp, local)| (temp.as_usize(), local.as_usize()));
    debug_bindings.dedup();

    let cfg = crate::cfg::compute_cfg_edges(&ctx.builder.blocks);
    let amir_block_params = ctx.builder.materialize_block_params();
    let mut amir_f = AmirFunc {
        symbol: f.symbol,
        return_type: ret_ty,
        receiver,
        params,
        locals: ctx.locals,
        temps: ctx.temps,
        blocks: ctx.builder.blocks,
        block_params: amir_block_params,
        stmts: ctx.builder.stmts,
        cfg,
    };

    // 4. Prune dummy loads and stores
    super::prune_dummy_loads_stores(&mut amir_f);

    // A3.4: rewrite Borrow→RelativeBorrow (LocalId index) for refs that cross Suspend;
    // *p becomes Load of the local so addresses never pin the state blob.
    crate::pin_free::apply_pin_free_refs(&mut amir_f, &tc.type_info.type_interner);

    // Drop obligations are part of the semantic AMIR inspected by M2. Inserting
    // them after borrow checking made O006 unreachable in the production
    // pipeline even though the checker itself implemented the rule.
    crate::drop_elaborate::elaborate_drops(&mut amir_f, &tc.type_info);

    // A3.2: remaining absolute Ref/RefMut live into resume → O010.
    func_diagnostics.extend(crate::suspend_check::check_borrow_across_suspend(
        &amir_f,
        &tc.symbols,
        &tc.type_info.type_interner,
    ));

    promote_escaped_coroutines(&mut amir_f);

    Ok((amir_f, debug_bindings))
}

fn promote_escaped_coroutines(func: &mut AmirFunc) {
    use crate::amir::{AmirRvalue, AmirStmt};
    let mut stack_coro_origins = FxHashMap::default();
    for block in &func.blocks {
        for instr_id in func.block_stmt_ids(block.id) {
            let stmt = &func.stmts.payloads[instr_id];
            if let AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::CoroutineReady { stack: true, .. },
            } = stmt
            {
                let lhs_val = TempId::from_usize(lhs.as_usize());
                stack_coro_origins.insert(lhs_val, instr_id);
            }
        }
    }

    if stack_coro_origins.is_empty() {
        return;
    }

    let mut temp_origins: FxHashMap<TempId, FxHashSet<TempId>> = FxHashMap::default();
    let mut local_origins: FxHashMap<crate::amir::LocalId, FxHashSet<TempId>> =
        FxHashMap::default();
    for t in stack_coro_origins.keys() {
        let t_val = TempId::from_usize(t.as_usize());
        let mut s = FxHashSet::default();
        s.insert(t_val);
        temp_origins.insert(t_val, s);
    }

    let mut escaped_origins = FxHashSet::default();

    let mut changed = true;
    while changed {
        changed = false;

        for block in &func.blocks {
            for instr_id in func.block_stmt_ids(block.id) {
                let stmt = &func.stmts.payloads[instr_id];
                match stmt {
                    AmirStmt::Assign { lhs, rhs } => {
                        let lhs_val = TempId::from_usize(lhs.as_usize());
                        let mut origins: FxHashSet<TempId> = FxHashSet::default();

                        match rhs {
                            AmirRvalue::Use(op)
                            | AmirRvalue::Discriminant { value: op }
                            | AmirRvalue::EnumPayload { value: op, .. }
                            | AmirRvalue::Len(op)
                            | AmirRvalue::Alloc(op)
                            | AmirRvalue::ToStr { value: op, .. }
                            | AmirRvalue::CoroutineReady { value: op, .. } => {
                                if let AmirOperand::Copy(src) | AmirOperand::Move(src) = op
                                    && let Some(src_origins) = temp_origins.get(src)
                                {
                                    origins.extend(src_origins);
                                }
                            }
                            AmirRvalue::Unary {
                                op: unary_op,
                                operand,
                            } => {
                                if !matches!(unary_op, crate::ops::UnaryOp::Await)
                                    && let AmirOperand::Copy(src) | AmirOperand::Move(src) = operand
                                    && let Some(src_origins) = temp_origins.get(src)
                                {
                                    origins.extend(src_origins);
                                }
                            }
                            AmirRvalue::Binary { left, right, .. } => {
                                if let AmirOperand::Copy(src) | AmirOperand::Move(src) = left
                                    && let Some(src_origins) = temp_origins.get(src)
                                {
                                    origins.extend(src_origins);
                                }
                                if let AmirOperand::Copy(src) | AmirOperand::Move(src) = right
                                    && let Some(src_origins) = temp_origins.get(src)
                                {
                                    origins.extend(src_origins);
                                }
                            }
                            AmirRvalue::FieldAccess {
                                base: AmirOperand::Copy(src) | AmirOperand::Move(src),
                                ..
                            } => {
                                if let Some(src_origins) = temp_origins.get(src) {
                                    origins.extend(src_origins);
                                }
                            }
                            AmirRvalue::StructLiteral { fields, .. } => {
                                for (_, op) in fields {
                                    if let AmirOperand::Copy(src) | AmirOperand::Move(src) = op
                                        && let Some(src_origins) = temp_origins.get(src)
                                    {
                                        origins.extend(src_origins);
                                    }
                                }
                            }
                            AmirRvalue::IndexAccess { base, index } => {
                                if let AmirOperand::Copy(src) | AmirOperand::Move(src) = base
                                    && let Some(src_origins) = temp_origins.get(src)
                                {
                                    origins.extend(src_origins);
                                }
                                if let AmirOperand::Copy(src) | AmirOperand::Move(src) = index
                                    && let Some(src_origins) = temp_origins.get(src)
                                {
                                    origins.extend(src_origins);
                                }
                            }
                            AmirRvalue::Array { items } | AmirRvalue::Tuple { items } => {
                                for op in items {
                                    if let AmirOperand::Copy(src) | AmirOperand::Move(src) = op
                                        && let Some(src_origins) = temp_origins.get(src)
                                    {
                                        origins.extend(src_origins);
                                    }
                                }
                            }
                            AmirRvalue::EnumConstruct {
                                payload: Some(AmirOperand::Copy(src) | AmirOperand::Move(src)),
                                ..
                            } => {
                                if let Some(src_origins) = temp_origins.get(src) {
                                    origins.extend(src_origins);
                                }
                            }
                            AmirRvalue::Load(place) => {
                                if let Some(src_origins) = local_origins.get(&place.local) {
                                    origins.extend(src_origins);
                                }
                            }
                            _ => {}
                        }

                        if !origins.is_empty() {
                            let entry = temp_origins.entry(lhs_val).or_default();
                            let old_len = entry.len();
                            entry.extend(origins);
                            if entry.len() > old_len {
                                changed = true;
                            }
                        }
                    }
                    AmirStmt::Store {
                        lhs,
                        rhs: AmirOperand::Copy(src) | AmirOperand::Move(src),
                    } => {
                        if let Some(src_origins) = temp_origins.get(src) {
                            let entry = local_origins.entry(lhs.local).or_default();
                            let old_len = entry.len();
                            entry.extend(src_origins);
                            if entry.len() > old_len {
                                changed = true;
                            }

                            if !lhs.projections.is_empty() {
                                for &origin in src_origins {
                                    if escaped_origins.insert(origin) {
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                    AmirStmt::Call { args, .. } => {
                        for arg in args {
                            if let AmirOperand::Copy(src) | AmirOperand::Move(src) = arg
                                && let Some(src_origins) = temp_origins.get(src)
                            {
                                for &origin in src_origins {
                                    if escaped_origins.insert(origin) {
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            match &block.terminator {
                AmirTerminator::Return => {
                    if let Some(src_origins) = temp_origins.get(&TempId::from_usize(0)) {
                        for &origin in src_origins {
                            if escaped_origins.insert(origin) {
                                changed = true;
                            }
                        }
                    }
                }
                AmirTerminator::Goto { target, args } => {
                    let target_block = &func.blocks[target.as_usize()];
                    for (idx, arg) in args.iter().enumerate() {
                        if let AmirOperand::Copy(src) | AmirOperand::Move(src) = arg
                            && let Some(src_origins) = temp_origins.get(src).cloned()
                            && let Some(param) = func.block_params(target_block.params).get(idx)
                        {
                            let entry = temp_origins.entry(param.id).or_default();
                            let old_len = entry.len();
                            entry.extend(src_origins);
                            if entry.len() > old_len {
                                changed = true;
                            }
                        }
                    }
                }
                AmirTerminator::Branch {
                    condition: _,
                    if_true,
                    true_args,
                    if_false,
                    false_args,
                } => {
                    let true_block = &func.blocks[if_true.as_usize()];
                    for (idx, arg) in true_args.iter().enumerate() {
                        if let AmirOperand::Copy(src) | AmirOperand::Move(src) = arg
                            && let Some(src_origins) = temp_origins.get(src).cloned()
                            && let Some(param) = func.block_params(true_block.params).get(idx)
                        {
                            let entry = temp_origins.entry(param.id).or_default();
                            let old_len = entry.len();
                            entry.extend(src_origins);
                            if entry.len() > old_len {
                                changed = true;
                            }
                        }
                    }
                    let false_block = &func.blocks[if_false.as_usize()];
                    for (idx, arg) in false_args.iter().enumerate() {
                        if let AmirOperand::Copy(src) | AmirOperand::Move(src) = arg
                            && let Some(src_origins) = temp_origins.get(src).cloned()
                            && let Some(param) = func.block_params(false_block.params).get(idx)
                        {
                            let entry = temp_origins.entry(param.id).or_default();
                            let old_len = entry.len();
                            entry.extend(src_origins);
                            if entry.len() > old_len {
                                changed = true;
                            }
                        }
                    }
                }
                AmirTerminator::SwitchInt {
                    discriminant: _,
                    targets,
                    otherwise,
                } => {
                    for (_, target, args) in targets {
                        let target_block = &func.blocks[target.as_usize()];
                        for (idx, arg) in args.iter().enumerate() {
                            if let AmirOperand::Copy(src) | AmirOperand::Move(src) = arg
                                && let Some(src_origins) = temp_origins.get(src).cloned()
                                && let Some(param) = func.block_params(target_block.params).get(idx)
                            {
                                let entry = temp_origins.entry(param.id).or_default();
                                let old_len = entry.len();
                                entry.extend(src_origins);
                                if entry.len() > old_len {
                                    changed = true;
                                }
                            }
                        }
                    }
                    let other_block = &func.blocks[otherwise.0.as_usize()];
                    for (idx, arg) in otherwise.1.iter().enumerate() {
                        if let AmirOperand::Copy(src) | AmirOperand::Move(src) = arg
                            && let Some(src_origins) = temp_origins.get(src).cloned()
                            && let Some(param) = func.block_params(other_block.params).get(idx)
                        {
                            let entry = temp_origins.entry(param.id).or_default();
                            let old_len = entry.len();
                            entry.extend(src_origins);
                            if entry.len() > old_len {
                                changed = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    for origin in escaped_origins {
        if let Some(&instr_id) = stack_coro_origins.get(&origin)
            && let AmirStmt::Assign {
                rhs: AmirRvalue::CoroutineReady { stack, .. },
                ..
            } = &mut func.stmts.payloads[instr_id]
        {
            *stack = false;
        }
    }
}

// Extension helper to check if terminator is unreachable
trait TerminatorExt {
    fn is_unreachable(&self) -> bool;
}

impl TerminatorExt for AmirTerminator {
    fn is_unreachable(&self) -> bool {
        matches!(self, AmirTerminator::Unreachable)
    }
}
