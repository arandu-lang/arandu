//! Splicing engine for inlining an AMIR leaf function into a caller.

use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirProjection, AmirRvalue,
    AmirStmt, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, BlockParam, InstrId, LocalId,
    TempId,
};
use crate::layout::DenseRange;
use smallvec::SmallVec;

/// One call to splice: the call statement `instr_id` in `bid` and its callee.
pub struct CallSite<'c> {
    pub bid: BlockId,
    pub instr_id: InstrId,
    pub lhs: Option<TempId>,
    pub args: &'c [AmirOperand],
    pub callee: &'c AmirFunc,
}

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
    splice_calls(
        caller,
        &[CallSite {
            bid: caller_bid,
            instr_id: call_instr_id,
            lhs: call_lhs,
            args: call_args,
            callee,
        }],
    ) == 1
}

/// A site that passed validation, with its position in its block and the
/// caller temps and locals its callee's are renamed to.
struct PreparedSite<'c> {
    position: usize,
    site: &'c CallSite<'c>,
    temp_map: Vec<TempId>,
    local_map: Vec<LocalId>,
}

/// Splices every valid site into `caller` with a single rebuild of its
/// statement and block tables, and returns how many were spliced. Rebuilding
/// once per call made inlining a long function quadratic.
///
/// Each original block keeps its id and the statements before its first
/// spliced call; every call appends its callee's blocks and a continuation
/// holding the statements up to the next call (or the block's terminator).
pub fn splice_calls(caller: &mut AmirFunc, sites: &[CallSite<'_>]) -> usize {
    let block_count = caller.blocks.len();
    let block_stmt_ids: Vec<Vec<InstrId>> = (0..block_count)
        .map(|b| caller.block_stmt_ids(BlockId::from_usize(b)).collect())
        .collect();
    let span = caller
        .params
        .first()
        .map_or(crate::Span::new(0, 0, 0), |param| {
            caller.temps[param.as_usize()].span
        });

    // 1. Validate sites and group them by block in statement order. A callee
    // that suspends cannot be spliced into straight-line code.
    let mut per_block: Vec<Vec<(usize, &CallSite<'_>)>> = vec![Vec::new(); block_count];
    for site in sites {
        if site.callee.params.len() != site.args.len()
            || site.bid.as_usize() >= block_count
            || site
                .callee
                .blocks
                .iter()
                .any(|block| matches!(block.terminator, AmirTerminator::Suspend { .. }))
        {
            continue;
        }
        let Some(position) = block_stmt_ids[site.bid.as_usize()]
            .iter()
            .position(|&id| id == site.instr_id)
        else {
            continue;
        };
        let entries = &mut per_block[site.bid.as_usize()];
        if !entries.iter().any(|(p, _)| *p == position) {
            entries.push((position, site));
        }
    }
    for entries in &mut per_block {
        entries.sort_by_key(|(position, _)| *position);
    }

    // 2. Give each callee fresh caller temps and locals.
    let mut prepared: Vec<Vec<PreparedSite<'_>>> = Vec::with_capacity(block_count);
    for entries in &per_block {
        let mut block_sites = Vec::with_capacity(entries.len());
        for &(position, site) in entries {
            let (temp_map, local_map) = rename_callee_values(caller, site, span);
            block_sites.push(PreparedSite {
                position,
                site,
                temp_map,
                local_map,
            });
        }
        prepared.push(block_sites);
    }
    let spliced: usize = prepared.iter().map(Vec::len).sum();
    if spliced == 0 {
        return 0;
    }

    // 3. Move statements into the new layout.
    let mut slots: Vec<Option<AmirStmt>> = std::mem::take(&mut caller.stmts)
        .payloads
        .raw
        .into_iter()
        .map(Some)
        .collect();
    let mut take = |id: InstrId| slots.get_mut(id.as_usize()).and_then(Option::take);

    let mut all_stmts: Vec<Vec<AmirStmt>> = Vec::with_capacity(block_count);
    let mut all_params: Vec<Vec<BlockParam>> = Vec::with_capacity(block_count);
    let mut all_terminators: Vec<AmirTerminator> = Vec::with_capacity(block_count);
    let mut extra_stmts: Vec<Vec<AmirStmt>> = Vec::new();
    let mut extra_params: Vec<Vec<BlockParam>> = Vec::new();
    let mut extra_terminators: Vec<AmirTerminator> = Vec::new();

    for (b, block_sites) in prepared.iter().enumerate() {
        let bid = BlockId::from_usize(b);
        all_params.push(caller.block_params(caller.block(bid).params).to_vec());
        let ids = &block_stmt_ids[b];
        if block_sites.is_empty() {
            all_stmts.push(ids.iter().filter_map(|&id| take(id)).collect());
            all_terminators.push(caller.block(bid).terminator.clone());
            continue;
        }

        // Statements before the first call, then that call's argument binds.
        let first = &block_sites[0];
        let mut head: Vec<AmirStmt> = ids[..first.position]
            .iter()
            .filter_map(|&id| take(id))
            .collect();
        bind_params(&mut head, first);
        all_stmts.push(head);
        all_terminators.push(AmirTerminator::Goto {
            target: BlockId::from_usize(block_count + extra_stmts.len()),
            args: Vec::new(),
        });

        for (k, current) in block_sites.iter().enumerate() {
            let callee = current.site.callee;
            let entry = block_count + extra_stmts.len();
            let resume = BlockId::from_usize(entry + callee.blocks.len());
            for (cb_idx, callee_block) in callee.blocks.iter().enumerate() {
                extra_params.push(
                    callee
                        .block_params(callee_block.params)
                        .iter()
                        .map(|bp| BlockParam {
                            id: current.temp_map[bp.id.as_usize()],
                            local: current.local_map[bp.local.as_usize()],
                            ty: bp.ty,
                            from: bp.from.clone(),
                            moved: bp.moved,
                        })
                        .collect(),
                );
                extra_stmts.push(
                    callee
                        .block_stmt_ids(BlockId::from_usize(cb_idx))
                        .filter_map(|id| callee.try_stmt(id))
                        .map(|stmt| remap_stmt(stmt, &current.temp_map, &current.local_map))
                        .collect(),
                );
                extra_terminators.push(remap_terminator(
                    &callee_block.terminator,
                    entry,
                    resume,
                    &current.temp_map,
                ));
            }

            // Continuation: statements up to the next call, then its binds.
            let end = block_sites
                .get(k + 1)
                .map_or(ids.len(), |next| next.position);
            let mut tail: Vec<AmirStmt> = ids[current.position + 1..end]
                .iter()
                .filter_map(|&id| take(id))
                .collect();
            extra_params.push(Vec::new());
            let terminator = if let Some(next) = block_sites.get(k + 1) {
                bind_params(&mut tail, next);
                AmirTerminator::Goto {
                    target: BlockId::from_usize(resume.as_usize() + 1),
                    args: Vec::new(),
                }
            } else {
                caller.block(bid).terminator.clone()
            };
            extra_stmts.push(tail);
            extra_terminators.push(terminator);
        }
    }
    all_stmts.extend(extra_stmts);
    all_params.extend(extra_params);
    all_terminators.extend(extra_terminators);

    // 4. Reconstruct the dense statement, block and parameter tables once.
    let mut new_stmt_table = AmirStmtTable::new();
    let mut new_blocks = Vec::with_capacity(all_stmts.len());
    let mut new_block_params_pool = Vec::new();
    for (b, ((stmts, params), terminator)) in all_stmts
        .into_iter()
        .zip(all_params)
        .zip(all_terminators)
        .enumerate()
    {
        let stmt_start = new_stmt_table.len();
        for stmt in stmts {
            new_stmt_table.push(stmt);
        }
        let stmt_range = DenseRange::new(stmt_start, new_stmt_table.len() - stmt_start);
        let param_start = new_block_params_pool.len();
        new_block_params_pool.extend(params);
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
    spliced
}

/// Fresh caller temps and locals for one callee. `TempId(0)`, the callee's
/// return register, maps to the call's destination (or a fresh dummy).
fn rename_callee_values(
    caller: &mut AmirFunc,
    site: &CallSite<'_>,
    span: crate::Span,
) -> (Vec<TempId>, Vec<LocalId>) {
    let callee = site.callee;
    let mut temp_map = Vec::with_capacity(callee.temps.len());
    let return_dest = site.lhs.unwrap_or_else(|| {
        let dummy = TempId::from_usize(caller.temps.len());
        caller.temps.push(AmirTemp {
            id: dummy,
            ty: callee.return_type,
            is_copy: true,
            is_nullable: false,
            span,
        });
        dummy
    });
    temp_map.push(return_dest);
    for callee_temp in callee.temps.iter().skip(1) {
        let new_id = TempId::from_usize(caller.temps.len());
        caller.temps.push(AmirTemp {
            id: new_id,
            ty: callee_temp.ty,
            is_copy: callee_temp.is_copy,
            is_nullable: callee_temp.is_nullable,
            span: callee_temp.span,
        });
        temp_map.push(new_id);
    }
    let mut local_map = Vec::with_capacity(callee.locals.len());
    for callee_local in &callee.locals {
        let new_id = LocalId::from_usize(caller.locals.len());
        caller.locals.push(AmirLocal {
            id: new_id,
            ty: callee_local.ty,
            is_memory: callee_local.is_memory,
            symbol: None,
            span: callee_local.span,
            use_span: None,
        });
        local_map.push(new_id);
    }
    (temp_map, local_map)
}

/// Assigns the call's arguments to the renamed callee parameter temps.
fn bind_params(stmts: &mut Vec<AmirStmt>, prepared: &PreparedSite<'_>) {
    for (index, &param) in prepared.site.callee.params.iter().enumerate() {
        stmts.push(AmirStmt::Assign {
            lhs: prepared.temp_map[param.as_usize()],
            rhs: AmirRvalue::Use(prepared.site.args[index]),
        });
    }
}

/// A callee terminator in the caller: blocks shift by `entry`, `Return` jumps
/// to the continuation. `Suspend` callees are rejected before splicing.
fn remap_terminator(
    term: &AmirTerminator,
    entry: usize,
    resume: BlockId,
    temp_map: &[TempId],
) -> AmirTerminator {
    let block = |target: BlockId| BlockId::from_usize(entry + target.as_usize());
    let args = |args: &[AmirOperand]| args.iter().map(|a| remap_op(a, temp_map)).collect();
    match term {
        AmirTerminator::Return => AmirTerminator::Goto {
            target: resume,
            args: Vec::new(),
        },
        AmirTerminator::Goto { target, args: a } => AmirTerminator::Goto {
            target: block(*target),
            args: args(a),
        },
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => AmirTerminator::Branch {
            condition: remap_op(condition, temp_map),
            if_true: block(*if_true),
            true_args: args(true_args),
            if_false: block(*if_false),
            false_args: args(false_args),
        },
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => AmirTerminator::SwitchInt {
            discriminant: remap_op(discriminant, temp_map),
            targets: targets
                .iter()
                .map(|(value, target, a)| (*value, block(*target), args(a)))
                .collect(),
            otherwise: (block(otherwise.0), args(&otherwise.1)),
        },
        AmirTerminator::Suspend { .. } | AmirTerminator::Unreachable => AmirTerminator::Unreachable,
    }
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
            AmirProjection::Variant(tag) => {
                new_projections.push(AmirProjection::Variant(*tag));
            }
            projection @ AmirProjection::Payload { .. } => new_projections.push(*projection),
            AmirProjection::TupleField(index) => {
                new_projections.push(AmirProjection::TupleField(*index));
            }
            AmirProjection::Index(op) => {
                new_projections.push(AmirProjection::Index(remap_op(op, temp_map)))
            }
            AmirProjection::IndexConstant(index) => {
                new_projections.push(AmirProjection::IndexConstant(*index));
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
        AmirRvalue::VectorSelect { .. }
        | AmirRvalue::VectorCompare { .. }
        | AmirRvalue::VectorOp { .. }
        | AmirRvalue::VectorSplat { .. }
        | AmirRvalue::VectorShuffle { .. }
        | AmirRvalue::VectorExtractLane { .. }
        | AmirRvalue::VectorInsertLane { .. } => {
            let mut mapped = rv.clone();
            crate::amir::visit::for_each_rvalue_operand_mut(&mut mapped, |operand| {
                *operand = remap_op(operand, temp_map);
            });
            mapped
        }
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
        AmirRvalue::IndexAccess {
            base,
            index,
            in_bounds,
        } => AmirRvalue::IndexAccess {
            base: remap_op(base, temp_map),
            index: remap_op(index, temp_map),
            in_bounds: *in_bounds,
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
            variant_tag,
            field_ty,
            tuple_ty,
        } => AmirRvalue::EnumPayload {
            value: remap_op(value, temp_map),
            variant: *variant,
            variant_tag: *variant_tag,
            index: *index,
            field_ty: *field_ty,
            tuple_ty: *tuple_ty,
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

#[cfg(test)]
mod vector_tests {
    use super::*;
    use crate::amir::VectorOp;

    #[test]
    fn inlining_remaps_every_vector_operand_without_changing_lanes_or_shuffle() {
        let left = AmirOperand::Copy(TempId(0));
        let right = AmirOperand::Move(TempId(1));
        let mapped_left = AmirOperand::Copy(TempId(10));
        let mapped_right = AmirOperand::Move(TempId(11));
        let mask = [31, 0, 29, 2, 27, 4, 25, 6, 23, 8, 21, 10, 19, 12, 17, 14];
        let cases = [
            (
                AmirRvalue::VectorOp {
                    op: VectorOp::Add,
                    left,
                    right,
                },
                vec![mapped_left, mapped_right],
            ),
            (AmirRvalue::VectorSplat { value: left }, vec![mapped_left]),
            (
                AmirRvalue::VectorShuffle { left, right, mask },
                vec![mapped_left, mapped_right],
            ),
            (
                AmirRvalue::VectorExtractLane {
                    vector: left,
                    lane: 3,
                },
                vec![mapped_left],
            ),
            (
                AmirRvalue::VectorInsertLane {
                    vector: left,
                    lane: 3,
                    value: right,
                },
                vec![mapped_left, mapped_right],
            ),
        ];
        for (value, expected) in cases {
            let mapped = remap_rvalue(&value, &[TempId(10), TempId(11)], &[]);
            let mut actual = Vec::new();
            crate::amir::visit::for_each_rvalue_operand(&mapped, |operand| actual.push(*operand));
            assert_eq!(actual, expected);
            match mapped {
                AmirRvalue::VectorShuffle { mask: actual, .. } => assert_eq!(actual, mask),
                AmirRvalue::VectorExtractLane { lane, .. }
                | AmirRvalue::VectorInsertLane { lane, .. } => assert_eq!(lane, 3),
                AmirRvalue::VectorOp { op, .. } => assert_eq!(op, VectorOp::Add),
                AmirRvalue::VectorSplat { .. } => {}
                _ => unreachable!("vector remapping must retain its variant"),
            }
        }
    }
}
