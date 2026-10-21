use super::needs_drop::{
    emit_recursive_drops, enum_variant_payload_types, projected_place_type, type_needs_drop,
};
use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirOperand, AmirProjection, AmirRvalue, AmirStmt, AmirStmtTable,
    AmirTemp, AmirTerminator, BlockId, TempId,
};
use crate::move_checker::MoveState;
use arandu_lexer::Span;
use arandu_middle::layout::DenseRange;
use arandu_middle::types::ArType;
use arandu_typeck::TypeInfo;

#[derive(Clone)]
struct DropBlockWork {
    block: BlockId,
    params: DenseRange,
    statements: Vec<AmirStmt>,
    move_states: Vec<MoveState>,
    terminator: AmirTerminator,
}

/// Turn dynamic enum cleanup into ordinary discriminant branches plus atomic
/// `Destroy` places. Backends only read the discriminant and run named
/// destructors; they do not recurse through sum-type payloads.
pub(super) fn expand_dynamic_enum_drops(func: &mut AmirFunc, type_info: &TypeInfo) {
    let moved_before = crate::move_checker::move_states_before_statements(func);
    let old_blocks = std::mem::take(&mut func.blocks);
    let old_stmts = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    if old_blocks.is_empty() {
        func.blocks = old_blocks;
        func.stmts = old_stmts;
        return;
    }

    let mut pending = std::collections::VecDeque::new();
    for block in &old_blocks {
        pending.push_back(DropBlockWork {
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
    let mut blocks: Vec<Option<AmirBasicBlock>> = vec![None; old_blocks.len()];
    let mut statements = AmirStmtTable::new();

    while let Some(work) = pending.pop_front() {
        let current_id = work.block;
        let current_params = work.params;
        let mut remaining = work.statements;
        let mut remaining_move_states = work.move_states;
        let current_term = work.terminator;
        loop {
            let split_at = remaining.iter().position(|stmt| {
                let AmirStmt::Destroy(place) = stmt else {
                    return false;
                };
                let Some(ty) = projected_place_type(place, func, type_info) else {
                    return false;
                };
                enum_variant_payload_types(&type_info.resolve_type_id(ty), type_info).is_some_and(
                    |variants| {
                        variants.iter().any(|(_, fields)| {
                            fields
                                .iter()
                                .any(|field| type_needs_drop(*field, type_info))
                        })
                    },
                )
            });
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

            let Some(AmirStmt::Destroy(enum_place)) = remaining.get(split_at).cloned() else {
                continue;
            };
            let tail = remaining.split_off(split_at + 1);
            let tail_move_states = remaining_move_states.split_off(split_at + 1);
            let enum_move_state = remaining_move_states[split_at].clone();
            remaining.truncate(split_at);
            remaining_move_states.truncate(split_at);
            let enum_ty = projected_place_type(&enum_place, func, type_info)
                .map(|ty| type_info.resolve_type_id(ty))
                .unwrap_or(ArType::Error);
            let Some(variants) = enum_variant_payload_types(&enum_ty, type_info) else {
                remaining.push(AmirStmt::Destroy(enum_place));
                remaining.extend(tail);
                remaining_move_states.push(enum_move_state);
                remaining_move_states.extend(tail_move_states);
                continue;
            };

            let start = statements.len();
            for stmt in remaining.drain(..) {
                statements.push(stmt);
            }
            let int_ty = type_info.type_interner.intern(ArType::Primitive(
                crate::passes::type_checker::types::Primitive::Int,
            ));
            let enum_ty_id = type_info.type_interner.intern(enum_ty.clone());
            let discriminant_ty = type_info.type_interner.intern(ArType::Ref(enum_ty_id));
            let borrowed_temp = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: borrowed_temp,
                ty: discriminant_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            statements.push(AmirStmt::Assign {
                lhs: borrowed_temp,
                rhs: AmirRvalue::Borrow(enum_place.clone()),
            });
            let tag_temp = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: tag_temp,
                ty: int_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            statements.push(AmirStmt::Assign {
                lhs: tag_temp,
                rhs: AmirRvalue::Discriminant {
                    value: AmirOperand::Copy(borrowed_temp),
                },
            });

            let continuation = BlockId::from_usize(blocks.len());
            blocks.push(None);
            pending.push_back(DropBlockWork {
                block: continuation,
                params: DenseRange::empty(),
                statements: tail,
                move_states: tail_move_states,
                terminator: current_term,
            });
            let mut targets = Vec::with_capacity(variants.len());
            for (tag, fields) in variants {
                let branch = BlockId::from_usize(blocks.len());
                blocks.push(None);
                let tuple_ty = if fields.len() > 1 {
                    let field_ids: Vec<_> = fields.clone();
                    Some(
                        type_info
                            .type_interner
                            .intern(ArType::tuple(&field_ids, &type_info.type_interner)),
                    )
                } else {
                    None
                };
                let mut payload_drops = AmirStmtTable::new();
                for (index, field_ty) in fields.into_iter().enumerate().rev() {
                    if !type_needs_drop(field_ty, type_info) {
                        continue;
                    }
                    let mut payload_place = enum_place.clone();
                    payload_place.projections.push(AmirProjection::Variant(tag));
                    payload_place.projections.push(AmirProjection::Payload {
                        variant_tag: tag,
                        index,
                        field_ty,
                        tuple_ty,
                    });
                    emit_recursive_drops(
                        &payload_place,
                        field_ty,
                        type_info,
                        &enum_move_state,
                        false,
                        &mut payload_drops,
                    );
                }
                let branch_move_states = vec![enum_move_state.clone(); payload_drops.len()];
                pending.push_back(DropBlockWork {
                    block: branch,
                    params: DenseRange::empty(),
                    statements: payload_drops.payloads.raw,
                    move_states: branch_move_states,
                    terminator: AmirTerminator::Goto {
                        target: continuation,
                        args: Vec::new(),
                    },
                });
                targets.push((tag as i128, branch, Vec::new()));
            }
            blocks[current_id.as_usize()] = Some(AmirBasicBlock {
                id: current_id,
                params: current_params,
                statements: DenseRange::new(start, statements.len() - start),
                terminator: AmirTerminator::SwitchInt {
                    discriminant: AmirOperand::Copy(tag_temp),
                    targets,
                    otherwise: (continuation, Vec::new()),
                },
            });
            // Continuation statements are owned by the pending queue.
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
}
