use super::needs_drop::{emit_recursive_drops, projected_place_type, type_needs_drop};
use crate::Span;
use crate::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirLocal, AmirOperand, AmirProjection, AmirRvalue,
    AmirStmt, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, LocalId, TempId,
};
use crate::move_checker::MoveState;
use crate::ops::BinaryOp;
use arandu_middle::layout::DenseRange;
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::type_context::AmirTypeContext;
use arandu_middle::types::{ArType, Primitive};

#[derive(Clone)]
struct ArrayDropBlockWork {
    block: BlockId,
    params: DenseRange,
    statements: Vec<AmirStmt>,
    move_states: Vec<MoveState>,
    terminator: AmirTerminator,
}

fn large_array_elem(
    stmt: &AmirStmt,
    func: &AmirFunc,
    type_info: &dyn AmirTypeContext,
) -> Option<(u64, arandu_middle::types::TypeId)> {
    let AmirStmt::Destroy(place) = stmt else {
        return None;
    };
    let ty = projected_place_type(place, func, type_info)?;
    let ArType::Array(len, elem) = type_info.resolve_type_id(ty) else {
        return None;
    };
    (len > 4 && type_needs_drop(elem, type_info)).then_some((len, elem))
}

/// Replace `Destroy` on large arrays (`len > 4`) with an indexed reverse loop
/// from `len - 1` down to `0` so large `[N]Resource` values do not unroll N
/// sequential drop statements.
pub(super) fn expand_array_drop_loops(
    func: &mut AmirFunc,
    type_info: &dyn AmirTypeContext,
    literal_pool: &mut AmirLiteralPool,
) -> bool {
    if func.blocks.is_empty()
        || !func
            .stmts
            .payloads
            .raw
            .iter()
            .any(|stmt| large_array_elem(stmt, func, type_info).is_some())
    {
        return false;
    }

    let moved_before = crate::move_checker::move_states_before_statements(func);
    let old_blocks = std::mem::take(&mut func.blocks);
    let old_stmts = std::mem::replace(&mut func.stmts, AmirStmtTable::new());

    let mut pending = std::collections::VecDeque::new();
    for block in &old_blocks {
        pending.push_back(ArrayDropBlockWork {
            block: block.id,
            params: block.params,
            statements: old_stmts
                .payloads
                .raw
                .get(block.statements.as_range())
                .unwrap_or(&[])
                .to_vec(),
            move_states: moved_before[block.id.as_usize()].clone(),
            terminator: block.terminator.clone(),
        });
    }

    let usize_ty = type_info
        .type_interner()
        .intern(ArType::Primitive(Primitive::USize));
    let bool_ty = type_info
        .type_interner()
        .intern(ArType::Primitive(Primitive::Bool));
    let zero_lit = literal_pool.intern_int("0");
    let one_lit = literal_pool.intern_int("1");

    let mut blocks: Vec<Option<AmirBasicBlock>> = vec![None; old_blocks.len()];
    let mut statements = AmirStmtTable::new();

    while let Some(work) = pending.pop_front() {
        let current_id = work.block;
        let current_params = work.params;
        let mut remaining = work.statements;
        let mut remaining_move_states = work.move_states;
        let current_term = work.terminator;
        loop {
            let split_at = remaining
                .iter()
                .position(|stmt| large_array_elem(stmt, func, type_info).is_some());
            let Some(split_at) = split_at else {
                let start = statements.len();
                for stmt in remaining {
                    statements.push(stmt);
                }
                blocks[current_id.as_usize()] = Some(AmirBasicBlock {
                    id: current_id,
                    params: current_params,
                    statements: DenseRange::new(start, statements.len() - start),
                    terminator: current_term,
                });
                break;
            };

            let Some((len, elem_ty)) = large_array_elem(&remaining[split_at], func, type_info)
            else {
                unreachable!("position matched large_array_elem");
            };
            let Some(AmirStmt::Destroy(arr_place)) = remaining.get(split_at).cloned() else {
                continue;
            };
            let tail = remaining.split_off(split_at + 1);
            let tail_move_states = remaining_move_states.split_off(split_at + 1);
            let arr_move_state = remaining_move_states[split_at].clone();
            remaining.truncate(split_at);
            remaining_move_states.truncate(split_at);

            let idx_local = LocalId::from_usize(func.locals.len());
            func.locals.push(AmirLocal {
                id: idx_local,
                ty: usize_ty,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 0),
                use_span: None,
            });
            let idx_place = crate::amir::AmirPlace {
                local: idx_local,
                projections: smallvec::SmallVec::new(),
            };
            let len_lit = literal_pool.intern_int(len.to_string());

            let header = BlockId::from_usize(blocks.len());
            blocks.push(None);
            let body = BlockId::from_usize(blocks.len());
            blocks.push(None);
            let continuation = BlockId::from_usize(blocks.len());
            blocks.push(None);

            // Entry prefix -> initialize counter to `len` -> header.
            let entry_start = statements.len();
            for stmt in remaining.drain(..) {
                statements.push(stmt);
            }
            statements.push(AmirStmt::StorageLive(idx_local));
            statements.push(AmirStmt::Store {
                lhs: idx_place.clone(),
                rhs: AmirOperand::Constant(AmirConstant::Pool(len_lit)),
            });
            blocks[current_id.as_usize()] = Some(AmirBasicBlock {
                id: current_id,
                params: current_params,
                statements: DenseRange::new(entry_start, statements.len() - entry_start),
                terminator: AmirTerminator::Goto {
                    target: header,
                    args: Vec::new(),
                },
            });

            // Header: cond = idx > 0; Branch(cond, body, continuation)
            let cur_temp = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: cur_temp,
                ty: usize_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            let cond_temp = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: cond_temp,
                ty: bool_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            let header_stmts = vec![
                AmirStmt::Assign {
                    lhs: cur_temp,
                    rhs: AmirRvalue::Load(idx_place.clone()),
                },
                AmirStmt::Assign {
                    lhs: cond_temp,
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Gt,
                        left: AmirOperand::Copy(cur_temp),
                        right: AmirOperand::Constant(AmirConstant::Pool(zero_lit)),
                    },
                },
            ];
            let header_move_states = vec![arr_move_state.clone(); header_stmts.len()];
            pending.push_back(ArrayDropBlockWork {
                block: header,
                params: DenseRange::empty(),
                statements: header_stmts,
                move_states: header_move_states,
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(cond_temp),
                    if_true: body,
                    true_args: Vec::new(),
                    if_false: continuation,
                    false_args: Vec::new(),
                },
            });

            // Body: next = idx - 1; idx = next; emit_recursive_drops(arr[next]); Goto(header)
            let cur_temp2 = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: cur_temp2,
                ty: usize_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            let next_temp = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: next_temp,
                ty: usize_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            let mut elem_place = arr_place;
            elem_place
                .projections
                .push(AmirProjection::Index(AmirOperand::Copy(next_temp)));
            let mut elem_drops = AmirStmtTable::new();
            emit_recursive_drops(
                &elem_place,
                elem_ty,
                type_info,
                &arr_move_state,
                false,
                true,
                &mut elem_drops,
            );
            let mut body_stmts = vec![
                AmirStmt::Assign {
                    lhs: cur_temp2,
                    rhs: AmirRvalue::Load(idx_place.clone()),
                },
                AmirStmt::Assign {
                    lhs: next_temp,
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Sub,
                        left: AmirOperand::Copy(cur_temp2),
                        right: AmirOperand::Constant(AmirConstant::Pool(one_lit)),
                    },
                },
                AmirStmt::Store {
                    lhs: idx_place.clone(),
                    rhs: AmirOperand::Copy(next_temp),
                },
            ];
            body_stmts.extend(elem_drops.payloads.raw);
            let body_move_states = vec![arr_move_state.clone(); body_stmts.len()];
            pending.push_back(ArrayDropBlockWork {
                block: body,
                params: DenseRange::empty(),
                statements: body_stmts,
                move_states: body_move_states,
                terminator: AmirTerminator::Goto {
                    target: header,
                    args: Vec::new(),
                },
            });

            // Continuation: StorageDead(idx) + tail
            let mut cont_stmts = vec![AmirStmt::StorageDead(idx_local)];
            cont_stmts.extend(tail);
            let mut cont_move_states = vec![arr_move_state];
            cont_move_states.extend(tail_move_states);
            pending.push_back(ArrayDropBlockWork {
                block: continuation,
                params: DenseRange::empty(),
                statements: cont_stmts,
                move_states: cont_move_states,
                terminator: current_term,
            });
            break;
        }
    }

    func.blocks = blocks
        .into_iter()
        .enumerate()
        .map(|(index, block)| {
            block.unwrap_or(AmirBasicBlock {
                id: BlockId::from_usize(index),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Unreachable,
            })
        })
        .collect();
    func.stmts = statements;
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
    true
}
