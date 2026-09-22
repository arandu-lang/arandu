//! Splicing engine for inlining an AMIR leaf function into a caller.

use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirProjection, AmirRvalue,
    AmirStmt, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, BlockParam, InstrId, LocalId,
    TempId,
};
use crate::layout::DenseRange;
use smallvec::SmallVec;

/// Slices `callee` into `caller` at the call site `(caller_bid, call_instr_id)`.
///
/// Returns `true` if inlining succeeded.
pub fn splice_call(
    caller: &mut AmirFunc,
    caller_bid: BlockId,
    call_instr_id: InstrId,
    call_lhs: Option<TempId>,
    call_args: &[AmirOperand],
    callee: &AmirFunc,
) -> bool {
    if callee.params.len() != call_args.len() {
        return false;
    }

    let orig_caller_block_count = caller.blocks.len();
    if caller_bid.as_usize() >= orig_caller_block_count {
        return false;
    }

    // 1. Collect caller statements for `caller_bid` and find split index
    let caller_bid_stmts: Vec<InstrId> = caller.block_stmt_ids(caller_bid).collect();
    let split_pos = match caller_bid_stmts.iter().position(|&id| id == call_instr_id) {
        Some(pos) => pos,
        None => return false,
    };

    // 2. Allocate fresh TempIds in caller for callee temps
    let mut temp_map = Vec::with_capacity(callee.temps.len());
    let span = caller.blocks[caller_bid.as_usize()]
        .statements
        .iter_ids::<InstrId>()
        .next()
        .and_then(|id| caller.try_stmt(id))
        .map_or(arandu_lexer::Span::new(0, 0, 0), |_| {
            caller.temps[caller
                .params
                .first()
                .copied()
                .unwrap_or(TempId(0))
                .as_usize()]
            .span
        });

    // TempId(0) represents the return register of the callee
    let return_dest_temp = if let Some(lhs) = call_lhs {
        lhs
    } else {
        let dummy_id = TempId::from_usize(caller.temps.len());
        caller.temps.push(AmirTemp {
            id: dummy_id,
            ty: callee.return_type,
            is_copy: true,
            is_nullable: false,
            span,
        });
        dummy_id
    };
    temp_map.push(return_dest_temp);

    // Map each callee temporary to a new caller temporary
    for (t_idx, callee_temp) in callee.temps.iter().enumerate().skip(1) {
        let new_id = TempId::from_usize(caller.temps.len());
        caller.temps.push(AmirTemp {
            id: new_id,
            ty: callee_temp.ty,
            is_copy: callee_temp.is_copy,
            is_nullable: callee_temp.is_nullable,
            span: callee_temp.span,
        });
        debug_assert_eq!(temp_map.len(), t_idx);
        temp_map.push(new_id);
    }

    // 3. Allocate fresh LocalIds in caller for callee locals
    let mut local_map = Vec::with_capacity(callee.locals.len());
    for callee_local in &callee.locals {
        let new_local_id = LocalId::from_usize(caller.locals.len());
        caller.locals.push(AmirLocal {
            id: new_local_id,
            ty: callee_local.ty,
            is_memory: callee_local.is_memory,
            symbol: None,
            span: callee_local.span,
            use_span: None,
        });
        local_map.push(new_local_id);
    }

    // 4. Determine block IDs
    // Callee blocks will be placed starting at `orig_caller_block_count`
    let callee_block_start = orig_caller_block_count;
    let mut block_map = Vec::with_capacity(callee.blocks.len());
    for i in 0..callee.blocks.len() {
        block_map.push(BlockId::from_usize(callee_block_start + i));
    }

    // The resume block receives the tail statements after the call in `caller_bid`
    let resume_bid = BlockId::from_usize(callee_block_start + callee.blocks.len());
    let total_new_blocks = orig_caller_block_count + callee.blocks.len() + 1;

    // 5. Build statements and terminators for every block in the new layout
    let mut all_stmts: Vec<Vec<AmirStmt>> = Vec::with_capacity(total_new_blocks);
    let mut all_block_params: Vec<Vec<BlockParam>> = Vec::with_capacity(total_new_blocks);
    let mut all_terminators: Vec<AmirTerminator> = Vec::with_capacity(total_new_blocks);

    // Snapshot existing caller blocks
    for b in 0..orig_caller_block_count {
        let bid = BlockId::from_usize(b);
        let block_params: Vec<BlockParam> = caller.block_params(caller.block(bid).params).to_vec();
        all_block_params.push(block_params);

        if bid == caller_bid {
            // Head statements before the call
            let mut head_stmts = Vec::new();
            for &instr_id in &caller_bid_stmts[..split_pos] {
                if let Some(stmt) = caller.try_stmt(instr_id) {
                    head_stmts.push(stmt.clone());
                }
            }

            // Assign actual call arguments to the callee's parameter temps
            for (param_idx, &param_temp) in callee.params.iter().enumerate() {
                let remapped_param_temp = temp_map[param_temp.as_usize()];
                head_stmts.push(AmirStmt::Assign {
                    lhs: remapped_param_temp,
                    rhs: AmirRvalue::Use(call_args[param_idx]),
                });
            }

            all_stmts.push(head_stmts);
            // Connect to callee entry block
            all_terminators.push(AmirTerminator::Goto {
                target: block_map[0],
                args: Vec::new(),
            });
        } else {
            let mut existing_stmts = Vec::new();
            for instr_id in caller.block_stmt_ids(bid) {
                if let Some(stmt) = caller.try_stmt(instr_id) {
                    existing_stmts.push(stmt.clone());
                }
            }
            all_stmts.push(existing_stmts);
            all_terminators.push(caller.block(bid).terminator.clone());
        }
    }

    // Callee blocks
    for (cb_idx, callee_block) in callee.blocks.iter().enumerate() {
        let mut remapped_params = Vec::new();
        for bp in callee.block_params(callee_block.params) {
            remapped_params.push(BlockParam {
                id: temp_map[bp.id.as_usize()],
                local: local_map[bp.local.as_usize()],
                ty: bp.ty,
                from: bp.from.clone(),
                moved: bp.moved,
            });
        }
        all_block_params.push(remapped_params);

        let mut inlined_stmts = Vec::new();
        for instr_id in callee.block_stmt_ids(BlockId::from_usize(cb_idx)) {
            if let Some(stmt) = callee.try_stmt(instr_id) {
                inlined_stmts.push(remap_stmt(stmt, &temp_map, &local_map));
            }
        }
        all_stmts.push(inlined_stmts);

        // Remap terminator
        let term = match &callee_block.terminator {
            AmirTerminator::Return => AmirTerminator::Goto {
                target: resume_bid,
                args: Vec::new(),
            },
            AmirTerminator::Goto { target, args } => AmirTerminator::Goto {
                target: block_map[target.as_usize()],
                args: args.iter().map(|a| remap_op(a, &temp_map)).collect(),
            },
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
            } => AmirTerminator::Branch {
                condition: remap_op(condition, &temp_map),
                if_true: block_map[if_true.as_usize()],
                true_args: true_args.iter().map(|a| remap_op(a, &temp_map)).collect(),
                if_false: block_map[if_false.as_usize()],
                false_args: false_args.iter().map(|a| remap_op(a, &temp_map)).collect(),
            },
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => AmirTerminator::SwitchInt {
                discriminant: remap_op(discriminant, &temp_map),
                targets: targets
                    .iter()
                    .map(|(val, target, args)| {
                        (
                            *val,
                            block_map[target.as_usize()],
                            args.iter().map(|a| remap_op(a, &temp_map)).collect(),
                        )
                    })
                    .collect(),
                otherwise: (
                    block_map[otherwise.0.as_usize()],
                    otherwise.1.iter().map(|a| remap_op(a, &temp_map)).collect(),
                ),
            },
            AmirTerminator::Suspend { .. } => return false,
            AmirTerminator::Unreachable => AmirTerminator::Unreachable,
        };
        all_terminators.push(term);
    }

    // Resume block
    all_block_params.push(Vec::new());
    let mut tail_stmts = Vec::new();
    for &instr_id in &caller_bid_stmts[split_pos + 1..] {
        if let Some(stmt) = caller.try_stmt(instr_id) {
            tail_stmts.push(stmt.clone());
        }
    }
    all_stmts.push(tail_stmts);
    all_terminators.push(caller.block(caller_bid).terminator.clone());

    // 6. Reconstruct dense caller statement table and block table
    let mut new_stmt_table = AmirStmtTable::new();
    let mut new_blocks = Vec::with_capacity(total_new_blocks);
    let mut new_block_params_pool = Vec::new();

    for (b, ((stmts, params), terminator)) in all_stmts
        .into_iter()
        .zip(all_block_params)
        .zip(all_terminators)
        .enumerate()
    {
        let stmt_start = new_stmt_table.len();
        for stmt in stmts {
            new_stmt_table.push(stmt);
        }
        let stmt_range = DenseRange::new(stmt_start, new_stmt_table.len() - stmt_start);

        let param_start = new_block_params_pool.len();
        for param in params {
            new_block_params_pool.push(param);
        }
        let param_range = DenseRange::new(param_start, new_block_params_pool.len() - param_start);

        new_blocks.push(AmirBasicBlock {
            id: BlockId::from_usize(b),
            params: param_range,
            statements: stmt_range,
            terminator,
        });
    }

    caller.stmts = new_stmt_table;
    caller.blocks = new_blocks;
    caller.block_params = new_block_params_pool;
    caller.cfg = crate::cfg::compute_cfg_edges(&caller.blocks);

    true
}

fn remap_op(op: &AmirOperand, temp_map: &[TempId]) -> AmirOperand {
    match op {
        AmirOperand::Copy(t) => AmirOperand::Copy(temp_map[t.as_usize()]),
        AmirOperand::Move(t) => AmirOperand::Move(temp_map[t.as_usize()]),
        AmirOperand::Constant(c) => AmirOperand::Constant(*c),
        AmirOperand::FunctionRef(s) => AmirOperand::FunctionRef(*s),
        AmirOperand::GlobalRef(s) => AmirOperand::GlobalRef(*s),
    }
}

fn remap_place(place: &AmirPlace, temp_map: &[TempId], local_map: &[LocalId]) -> AmirPlace {
    let mut new_projections = SmallVec::new();
    for proj in &place.projections {
        match proj {
            AmirProjection::Field(sym) => new_projections.push(AmirProjection::Field(*sym)),
            AmirProjection::Index(op) => {
                new_projections.push(AmirProjection::Index(remap_op(op, temp_map)))
            }
            AmirProjection::Deref => new_projections.push(AmirProjection::Deref),
        }
    }
    AmirPlace {
        local: local_map[place.local.as_usize()],
        projections: new_projections,
    }
}

fn remap_rvalue(rv: &AmirRvalue, temp_map: &[TempId], local_map: &[LocalId]) -> AmirRvalue {
    match rv {
        AmirRvalue::Use(op) => AmirRvalue::Use(remap_op(op, temp_map)),
        AmirRvalue::Binary { op, left, right } => AmirRvalue::Binary {
            op: *op,
            left: remap_op(left, temp_map),
            right: remap_op(right, temp_map),
        },
        AmirRvalue::Unary { op, operand } => AmirRvalue::Unary {
            op: *op,
            operand: remap_op(operand, temp_map),
        },
        AmirRvalue::FieldAccess { base, field } => AmirRvalue::FieldAccess {
            base: remap_op(base, temp_map),
            field: *field,
        },
        AmirRvalue::StructLiteral {
            struct_symbol,
            fields,
        } => AmirRvalue::StructLiteral {
            struct_symbol: *struct_symbol,
            fields: fields
                .iter()
                .map(|(name, op)| (name.clone(), remap_op(op, temp_map)))
                .collect(),
        },
        AmirRvalue::IndexAccess { base, index } => AmirRvalue::IndexAccess {
            base: remap_op(base, temp_map),
            index: remap_op(index, temp_map),
        },
        AmirRvalue::Array { items } => AmirRvalue::Array {
            items: items.iter().map(|it| remap_op(it, temp_map)).collect(),
        },
        AmirRvalue::Tuple { items } => AmirRvalue::Tuple {
            items: items.iter().map(|it| remap_op(it, temp_map)).collect(),
        },
        AmirRvalue::Discriminant { value } => AmirRvalue::Discriminant {
            value: remap_op(value, temp_map),
        },
        AmirRvalue::EnumPayload {
            value,
            variant,
            index,
        } => AmirRvalue::EnumPayload {
            value: remap_op(value, temp_map),
            variant: *variant,
            index: *index,
        },
        AmirRvalue::EnumConstruct {
            variant_tag,
            payload,
        } => AmirRvalue::EnumConstruct {
            variant_tag: *variant_tag,
            payload: payload.as_ref().map(|p| remap_op(p, temp_map)),
        },
        AmirRvalue::Len(op) => AmirRvalue::Len(remap_op(op, temp_map)),
        AmirRvalue::SliceData(op) => AmirRvalue::SliceData(remap_op(op, temp_map)),
        AmirRvalue::SliceView { owner, data, len } => AmirRvalue::SliceView {
            owner: remap_op(owner, temp_map),
            data: remap_op(data, temp_map),
            len: remap_op(len, temp_map),
        },
        AmirRvalue::SliceSubslice { slice, start, len } => AmirRvalue::SliceSubslice {
            slice: remap_op(slice, temp_map),
            start: remap_op(start, temp_map),
            len: remap_op(len, temp_map),
        },
        AmirRvalue::StrBytes { source } => AmirRvalue::StrBytes {
            source: remap_op(source, temp_map),
        },
        AmirRvalue::StrView { owner } => AmirRvalue::StrView {
            owner: remap_op(owner, temp_map),
        },
        AmirRvalue::Alloc(op) => AmirRvalue::Alloc(remap_op(op, temp_map)),
        AmirRvalue::Load(place) => AmirRvalue::Load(remap_place(place, temp_map, local_map)),
        AmirRvalue::Borrow(place) => AmirRvalue::Borrow(remap_place(place, temp_map, local_map)),
        AmirRvalue::BorrowMut(place) => {
            AmirRvalue::BorrowMut(remap_place(place, temp_map, local_map))
        }
        AmirRvalue::RelativeBorrow { local, mutable } => AmirRvalue::RelativeBorrow {
            local: local_map[local.as_usize()],
            mutable: *mutable,
        },
        AmirRvalue::CoroutineReady {
            value,
            payload_ty,
            stack,
        } => AmirRvalue::CoroutineReady {
            value: remap_op(value, temp_map),
            payload_ty: *payload_ty,
            stack: *stack,
        },
        AmirRvalue::GenInsert {
            value,
            payload_ty,
            arena,
            origin,
        } => AmirRvalue::GenInsert {
            value: remap_op(value, temp_map),
            payload_ty: *payload_ty,
            arena: *arena,
            origin: *origin,
        },
        AmirRvalue::GenGet {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => AmirRvalue::GenGet {
            gen_ref: remap_op(gen_ref, temp_map),
            payload_ty: *payload_ty,
            arena: *arena,
            origin: *origin,
        },
        AmirRvalue::GenSet {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => AmirRvalue::GenSet {
            gen_ref: remap_op(gen_ref, temp_map),
            value: remap_op(value, temp_map),
            payload_ty: *payload_ty,
            arena: *arena,
            origin: *origin,
        },
        AmirRvalue::GenUpsert {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => AmirRvalue::GenUpsert {
            gen_ref: remap_op(gen_ref, temp_map),
            value: remap_op(value, temp_map),
            payload_ty: *payload_ty,
            arena: *arena,
            origin: *origin,
        },
        AmirRvalue::GenRemove {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => AmirRvalue::GenRemove {
            gen_ref: remap_op(gen_ref, temp_map),
            payload_ty: *payload_ty,
            arena: *arena,
            origin: *origin,
        },
        AmirRvalue::StringInterp { parts } => AmirRvalue::StringInterp {
            parts: parts.iter().map(|p| remap_op(p, temp_map)).collect(),
        },
        AmirRvalue::ToStr { value, src_ty } => AmirRvalue::ToStr {
            value: remap_op(value, temp_map),
            src_ty: *src_ty,
        },
        AmirRvalue::BlackBox { value, value_ty } => AmirRvalue::BlackBox {
            value: remap_op(value, temp_map),
            value_ty: *value_ty,
        },
    }
}

fn remap_stmt(stmt: &AmirStmt, temp_map: &[TempId], local_map: &[LocalId]) -> AmirStmt {
    match stmt {
        AmirStmt::Assign { lhs, rhs } => AmirStmt::Assign {
            lhs: temp_map[lhs.as_usize()],
            rhs: remap_rvalue(rhs, temp_map, local_map),
        },
        AmirStmt::Store { lhs, rhs } => AmirStmt::Store {
            lhs: remap_place(lhs, temp_map, local_map),
            rhs: remap_op(rhs, temp_map),
        },
        AmirStmt::Call {
            lhs,
            callee,
            args,
            return_borrow,
        } => AmirStmt::Call {
            lhs: lhs.map(|t| temp_map[t.as_usize()]),
            callee: remap_op(callee, temp_map),
            args: args.iter().map(|a| remap_op(a, temp_map)).collect(),
            return_borrow: return_borrow.clone(),
        },
        AmirStmt::Free(op) => AmirStmt::Free(remap_op(op, temp_map)),
        AmirStmt::StorageLive(l) => AmirStmt::StorageLive(local_map[l.as_usize()]),
        AmirStmt::StorageDead(l) => AmirStmt::StorageDead(local_map[l.as_usize()]),
        AmirStmt::Destroy(p) => AmirStmt::Destroy(remap_place(p, temp_map, local_map)),
        AmirStmt::Nop => AmirStmt::Nop,
    }
}
