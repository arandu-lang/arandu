use super::places::{
    borrows_through_external_storage, is_return_temp, local_span, operand_stack_place, place_span,
    temp_span, type_is_borrow_carrier,
};
use super::types::{EscapeEvent, EscapeKind};
use crate::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, LocalId};
use crate::borrow_facts::analyze_borrow_facts_with_types;
use crate::types::ArType;

/// Pure escape finder (no diagnostics).
#[must_use]
pub fn find_escapes(func: &AmirFunc, interner: &crate::types::TypeInterner) -> Vec<EscapeEvent> {
    find_escapes_impl(func, interner, None)
}

/// Like [`find_escapes`], but uses typeck's structural metadata to avoid
/// treating named wrappers around slice views as hidden reference carriers.
pub fn find_escapes_with_type_info(
    func: &AmirFunc,
    type_info: &arandu_typeck::TypeInfo,
) -> Vec<EscapeEvent> {
    find_escapes_impl(func, &type_info.type_interner, Some(type_info))
}

fn find_escapes_impl(
    func: &AmirFunc,
    interner: &crate::types::TypeInterner,
    type_info: Option<&arandu_typeck::TypeInfo>,
) -> Vec<EscapeEvent> {
    let facts = analyze_borrow_facts_with_types(func, interner);
    // Generated functions commonly reuse the same types across many stores.
    // Resolve the structural carrier question once per used store type. Keep
    // this lazy: partial IDE MIR may contain temp TypeIds from a broader
    // interner than the narrow legacy API receives.
    let mut borrow_carrier_by_type = rustc_hash::FxHashMap::default();
    // Temp → place_local for stack-derived refs (from loans + propagation).
    let mut temp_to_place: Vec<Option<LocalId>> = vec![None; func.temps.len()];
    for loan in &facts.loans {
        if borrows_through_external_storage(
            func,
            interner,
            loan.place_local,
            &loan.place_projections,
        ) {
            // Reference slots and slice descriptors do not own their pointees. The
            // original owner's loan, propagated by borrow_facts, remains the
            // stack origin when there is one. Formal reference parameters
            // instead borrow caller-owned storage and introduce no stack root.
            continue;
        }
        for t in loan.holder_temps.iter() {
            let i = t.as_usize();
            if i < temp_to_place.len() {
                temp_to_place[i] = Some(loan.place_local);
            }
        }
    }

    let mut enum_construct_temps = rustc_hash::FxHashSet::default();
    // Also walk primary Borrow assigns and aggregate constructions
    // that incorporate stack references.
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            for stmt in func.block_stmts(block.id) {
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                } = stmt
                {
                    let i = lhs.as_usize();
                    if i < temp_to_place.len()
                        && temp_to_place[i].is_none()
                        && !borrows_through_external_storage(
                            func,
                            interner,
                            place.local,
                            &place.projections,
                        )
                    {
                        temp_to_place[i] = Some(place.local);
                        changed = true;
                    }
                }
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::RelativeBorrow { local, .. },
                } = stmt
                {
                    let i = lhs.as_usize();
                    if i < temp_to_place.len() && temp_to_place[i].is_none() {
                        temp_to_place[i] = Some(*local);
                        changed = true;
                    }
                }
                if let AmirStmt::Assign { lhs, rhs } = stmt {
                    if func.temps.get(lhs.as_usize()).is_some_and(|temp| {
                        !crate::borrow_facts::type_may_carry_borrow(temp.ty, interner)
                    }) {
                        continue;
                    }
                    let i = lhs.as_usize();
                    match rhs {
                        AmirRvalue::Use(op) => {
                            if i < temp_to_place.len()
                                && temp_to_place[i].is_none()
                                && let Some(p) = operand_stack_place(op, &temp_to_place)
                            {
                                temp_to_place[i] = Some(p);
                                changed = true;
                            }
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op
                                && enum_construct_temps.contains(t)
                                && enum_construct_temps.insert(*lhs)
                            {
                                changed = true;
                            }
                        }
                        AmirRvalue::StructLiteral { fields, .. } => {
                            if i < temp_to_place.len() && temp_to_place[i].is_none() {
                                for (_, op) in fields {
                                    if let Some(p) = operand_stack_place(op, &temp_to_place) {
                                        temp_to_place[i] = Some(p);
                                        changed = true;
                                        break;
                                    }
                                }
                            }
                        }
                        AmirRvalue::Tuple { items } | AmirRvalue::Array { items } => {
                            if i < temp_to_place.len() && temp_to_place[i].is_none() {
                                for op in items {
                                    if let Some(p) = operand_stack_place(op, &temp_to_place) {
                                        temp_to_place[i] = Some(p);
                                        changed = true;
                                        break;
                                    }
                                }
                            }
                        }
                        AmirRvalue::EnumConstruct { payload, .. } => {
                            if enum_construct_temps.insert(*lhs) {
                                changed = true;
                            }
                            if i < temp_to_place.len()
                                && temp_to_place[i].is_none()
                                && let Some(op) = payload
                                && let Some(p) = operand_stack_place(op, &temp_to_place)
                            {
                                temp_to_place[i] = Some(p);
                                changed = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let mut events = Vec::new();
    for block in &func.blocks {
        // Return of stack-derived ref.
        if matches!(block.terminator, AmirTerminator::Return) {
            // Return value is typically stored into local/temp _0 (return slot).
            // Look for last assigns to return temp (params[0] or TempId(0)).
            for stmt in func.block_stmts(block.id) {
                // Provenance may reach the return slot through a load or a
                // call, not just Use/Borrow/aggregate syntax. Scalar values
                // were removed from this map by the shared typed facts.
                if let AmirStmt::Assign { lhs, .. } = stmt
                    && is_return_temp(*lhs, func)
                    && let Some(place) = temp_to_place.get(lhs.as_usize()).copied().flatten()
                {
                    events.push(EscapeEvent {
                        kind: EscapeKind::Return,
                        place_local: place,
                        span: temp_span(*lhs, func).unwrap_or_else(|| local_span(place, func)),
                        block: block.id,
                        reason: "reference escapes via return of the current function",
                    });
                }
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Use(op),
                } = stmt
                    && is_return_temp(*lhs, func)
                    && let Some(place) = operand_stack_place(op, &temp_to_place)
                {
                    events.push(EscapeEvent {
                        kind: EscapeKind::Return,
                        place_local: place,
                        span: temp_span(*lhs, func).unwrap_or_else(|| local_span(place, func)),
                        block: block.id,
                        reason: "reference escapes via return of the current function",
                    });
                }
                // Direct: return temp = Borrow(local)
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                } = stmt
                    && is_return_temp(*lhs, func)
                {
                    let origin = temp_to_place
                        .get(lhs.as_usize())
                        .copied()
                        .flatten()
                        .or_else(|| {
                            (!borrows_through_external_storage(
                                func,
                                interner,
                                place.local,
                                &place.projections,
                            ))
                            .then_some(place.local)
                        });
                    if let Some(origin) = origin {
                        events.push(EscapeEvent {
                            kind: EscapeKind::Return,
                            place_local: origin,
                            span: temp_span(*lhs, func).unwrap_or_else(|| place_span(place, func)),
                            block: block.id,
                            reason: "reference escapes via return of the current function",
                        });
                    }
                }
                // Return of aggregate containing stack ref directly assigned to return temp
                if let AmirStmt::Assign { lhs, rhs } = stmt
                    && is_return_temp(*lhs, func)
                {
                    match rhs {
                        AmirRvalue::StructLiteral { fields, .. } => {
                            for (_, op) in fields {
                                if let Some(place) = operand_stack_place(op, &temp_to_place) {
                                    events.push(EscapeEvent {
                                        kind: EscapeKind::Return,
                                        place_local: place,
                                        span: temp_span(*lhs, func).unwrap_or_else(|| local_span(place, func)),
                                        block: block.id,
                                        reason: "reference escapes via return of the current function",
                                    });
                                    break;
                                }
                            }
                        }
                        AmirRvalue::Tuple { items } | AmirRvalue::Array { items } => {
                            for op in items {
                                if let Some(place) = operand_stack_place(op, &temp_to_place) {
                                    events.push(EscapeEvent {
                                        kind: EscapeKind::Return,
                                        place_local: place,
                                        span: temp_span(*lhs, func).unwrap_or_else(|| local_span(place, func)),
                                        block: block.id,
                                        reason: "reference escapes via return of the current function",
                                    });
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Heap / aggregate store of stack-derived ref.
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Store { lhs, rhs } = stmt {
                let Some(place) = operand_stack_place(rhs, &temp_to_place) else {
                    continue;
                };
                // A projected field or materialized local is still stack
                // storage. However, storing a borrow-carrying aggregate in a
                // local can outlive the borrow's static window when the
                // aggregate hides its provenance (for example `Holder { item:
                // ref T }`). Keep that conservative fallback for structural
                // carriers, but not for the fat-slice view itself: match
                // temporaries such as `Option<[]T>` remain ordinary scoped
                // borrows and must not be mistaken for frame escapes.
                let destination_is_indirect = lhs
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, crate::amir::AmirProjection::Deref));
                let rhs_is_borrow_carrier = match rhs {
                    AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                        func.temps.get(temp.as_usize()).is_some_and(|temp| {
                            *borrow_carrier_by_type.entry(temp.ty).or_insert_with(|| {
                                type_is_borrow_carrier(temp.ty, interner, type_info)
                            })
                        })
                    }
                    _ => false,
                };
                let rhs_is_enum = match rhs {
                    AmirOperand::Copy(t) | AmirOperand::Move(t) => enum_construct_temps.contains(t),
                    _ => false,
                };
                let dest_is_ref_slot = lhs.projections.is_empty()
                    && (rhs_is_enum
                        || func.locals.get(lhs.local.as_usize()).is_some_and(|l| {
                            match interner.resolve(l.ty) {
                                ArType::Ref(_) | ArType::RefMut(_) => true,
                                ArType::Option(inner) | ArType::Nullable(inner) => {
                                    matches!(
                                        interner.resolve(inner),
                                        ArType::Ref(_) | ArType::RefMut(_)
                                    )
                                }
                                ArType::Result(ok, err) => {
                                    matches!(
                                        interner.resolve(ok),
                                        ArType::Ref(_) | ArType::RefMut(_)
                                    ) || matches!(
                                        interner.resolve(err),
                                        ArType::Ref(_) | ArType::RefMut(_)
                                    )
                                }
                                _ => false,
                            }
                        }));
                if (destination_is_indirect || rhs_is_borrow_carrier) && !dest_is_ref_slot {
                    events.push(EscapeEvent {
                        kind: EscapeKind::HeapStore,
                        place_local: place,
                        span: local_span(place, func),
                        block: block.id,
                        reason: "reference is stored into memory that may outlive the stack frame (generational fallback candidate)",
                    });
                }
            }
        }
    }

    // Dedup by (kind, place, block).
    events.sort_by_key(|e| (e.block.as_usize(), e.place_local.as_usize(), e.kind as u8));
    events.dedup_by_key(|e| (e.block, e.place_local, e.kind));
    events
}
