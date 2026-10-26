//! Split nonescaping Copy aggregate locals into independent scalar places.
//! Projections, mutation and joins retain ordinary Store/Load semantics. No
//! byte offsets or host layout assumptions enter the transform.
use crate::amir::{
    AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt,
    AmirStmtTable, LocalId, TempId,
};
use crate::types::{ArType, Primitive, TypeId, TypeInterner};
use crate::{SymbolTable, layout::DenseRange};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Literal {
    names: Vec<smol_str::SmolStr>,
    values: Vec<AmirOperand>,
}
struct Plan {
    names: Vec<smol_str::SmolStr>,
    types: Vec<Option<TypeId>>,
    slots: Vec<Option<LocalId>>,
}

fn scalar(ty: TypeId, interner: &TypeInterner) -> bool {
    matches!(
        interner.resolve(ty),
        ArType::Primitive(Primitive::Bool | Primitive::Char)
    ) || matches!(interner.resolve(ty), ArType::Primitive(primitive) if primitive.is_numeric())
}
fn index(place: &AmirPlace, names: &[smol_str::SmolStr], symbols: &SymbolTable) -> Option<usize> {
    match place.projections.as_slice() {
        [AmirProjection::TupleField(index)] if *index < names.len() => Some(*index),
        [AmirProjection::Field(symbol)] => {
            let name = &symbols.try_get(*symbol)?.name;
            names.iter().position(|field| field == name)
        }
        _ => None,
    }
}

pub(crate) fn scalarize(
    func: &mut AmirFunc,
    symbols: &SymbolTable,
    interner: &TypeInterner,
) -> bool {
    let mut definitions = vec![0usize; func.temps.len()];
    for &param in func
        .params
        .iter()
        .chain(func.receiver.iter().map(|r| &r.temp))
    {
        definitions[param.as_usize()] += 1;
    }
    for block in &func.blocks {
        for param in func.block_params(block.params) {
            definitions[param.id.as_usize()] += 1;
        }
    }
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Assign { lhs, .. } | AmirStmt::Call { lhs: Some(lhs), .. } = stmt {
            definitions[lhs.as_usize()] += 1;
        }
    }
    let mut literals = FxHashMap::<TempId, Literal>::default();
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Assign { lhs, rhs } = stmt {
            if definitions[lhs.as_usize()] != 1 || !func.temps[lhs.as_usize()].is_copy {
                continue;
            }
            let literal = match rhs {
                AmirRvalue::StructLiteral { fields, .. } => Literal {
                    names: fields.iter().map(|(name, _)| name.clone()).collect(),
                    values: fields.iter().map(|(_, operand)| *operand).collect(),
                },
                AmirRvalue::Tuple { items } => Literal {
                    names: (0..items.len())
                        .map(|index| index.to_string().into())
                        .collect(),
                    values: items.clone(),
                },
                _ => continue,
            };
            if literal.values.iter().all(|operand| match operand {
                AmirOperand::Copy(temp) => {
                    definitions[temp.as_usize()] <= 1
                        && func.temps[temp.as_usize()].is_copy
                        && scalar(func.temps[temp.as_usize()].ty, interner)
                }
                AmirOperand::Constant(_) => true,
                _ => false,
            }) {
                literals.insert(*lhs, literal);
            }
        }
    }
    let mut plans = std::collections::BTreeMap::<usize, Plan>::new();
    let mut rejected = vec![false; func.locals.len()];
    for param in &func.block_params {
        rejected[param.local.as_usize()] = true;
    }
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Store { lhs, rhs } = stmt
            && lhs.projections.is_empty()
        {
            let source = match rhs {
                AmirOperand::Copy(temp) => literals.get(temp),
                _ => None,
            };
            if let Some(literal) = source {
                if let Some(previous) = plans.get(&lhs.local.as_usize())
                    && previous.names != literal.names
                {
                    rejected[lhs.local.as_usize()] = true;
                }
                plans.entry(lhs.local.as_usize()).or_insert_with(|| Plan {
                    names: literal.names.clone(),
                    types: vec![None; literal.names.len()],
                    slots: vec![None; literal.names.len()],
                });
            } else {
                rejected[lhs.local.as_usize()] = true;
            }
        }
    }
    let snapshots: FxHashMap<_, _> = func
        .stmts
        .payloads
        .iter()
        .filter_map(|stmt| match stmt {
            AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::Load(place),
            } if *lhs != TempId(0)
                && place.projections.is_empty()
                && definitions[lhs.as_usize()] == 1
                && func.temps[lhs.as_usize()].is_copy =>
            {
                Some((*lhs, place.local))
            }
            _ => None,
        })
        .collect();
    let mut required_fields: FxHashMap<TempId, BTreeSet<usize>> = FxHashMap::default();
    for stmt in func.stmts.payloads.iter() {
        crate::amir::visit::for_each_stmt_operand(stmt, |operand| {
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand
                && let Some(local) = snapshots.get(temp)
            {
                if let AmirStmt::Assign {
                    lhs,
                    rhs:
                        AmirRvalue::FieldAccess {
                            base: AmirOperand::Copy(base),
                            field,
                        },
                } = stmt
                    && base == temp
                    && scalar(func.temps[lhs.as_usize()].ty, interner)
                    && let Some(plan) = plans.get_mut(&local.as_usize())
                    && *field < plan.types.len()
                {
                    let ty = func.temps[lhs.as_usize()].ty;
                    if plan.types[*field].is_some_and(|previous| previous != ty) {
                        rejected[local.as_usize()] = true;
                    }
                    plan.types[*field] = Some(ty);
                    required_fields.entry(*temp).or_default().insert(*field);
                } else {
                    rejected[local.as_usize()] = true;
                }
            }
        });
        match stmt {
            AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::Load(place),
            } => {
                if let Some(plan) = plans.get_mut(&place.local.as_usize()) {
                    if place.projections.is_empty() && snapshots.contains_key(lhs) {
                        // Each scalar field will be captured at this load,
                        // preserving Copy snapshots across later mutations.
                    } else if let Some(field) = index(place, &plan.names, symbols)
                        && scalar(func.temps[lhs.as_usize()].ty, interner)
                    {
                        let ty = func.temps[lhs.as_usize()].ty;
                        if plan.types[field].is_some_and(|previous| previous != ty) {
                            rejected[place.local.as_usize()] = true;
                        }
                        plan.types[field] = Some(ty);
                    } else {
                        rejected[place.local.as_usize()] = true;
                    }
                }
            }
            AmirStmt::Store { lhs, rhs } if !lhs.projections.is_empty() => {
                if let Some(plan) = plans.get(&lhs.local.as_usize())
                    && (index(lhs, &plan.names, symbols).is_none()
                        || matches!(rhs, AmirOperand::Move(_)))
                {
                    rejected[lhs.local.as_usize()] = true;
                }
            }
            AmirStmt::Assign { rhs, .. } => {
                crate::amir::visit::for_each_rvalue_place(rhs, |place| {
                    rejected[place.local.as_usize()] = true;
                });
                if let AmirRvalue::RelativeBorrow { local, .. } = rhs {
                    rejected[local.as_usize()] = true;
                }
            }
            AmirStmt::Destroy(place) => rejected[place.local.as_usize()] = true,
            _ => {}
        }
    }
    for block in &func.blocks {
        crate::amir::visit::for_each_terminator_operand(&block.terminator, |operand| {
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand
                && let Some(local) = snapshots.get(temp)
            {
                rejected[local.as_usize()] = true;
            }
        });
    }
    plans.retain(|local, plan| !rejected[*local] && plan.types.iter().any(Option::is_some));
    if plans.is_empty() {
        return false;
    }
    for (&local, plan) in &mut plans {
        for (field, ty) in plan.types.iter().enumerate() {
            if let Some(ty) = ty {
                let mut scalar_local: AmirLocal = func.locals[local].clone();
                scalar_local.id = LocalId::from_usize(func.locals.len());
                scalar_local.ty = *ty;
                // The rewrite below reads and writes these slots with real
                // Store/Load places that no later pass prunes, so they are
                // memory-backed (backends allocate a stack home for them).
                scalar_local.is_memory = true;
                scalar_local.symbol = None;
                plan.slots[field] = Some(scalar_local.id);
                func.locals.push(scalar_local);
            }
        }
    }
    let mut captured = FxHashMap::default();
    let mut snapshot_order: Vec<_> = snapshots.iter().collect();
    snapshot_order.sort_unstable_by_key(|(temp, _)| **temp);
    for (&temp, &local) in snapshot_order {
        if let Some(plan) = plans.get(&local.as_usize()) {
            let mut values = BTreeMap::new();
            for &field in required_fields.get(&temp).into_iter().flatten() {
                if let Some(ty) = plan.types[field] {
                    let mut scalar_temp = func.temps[temp.as_usize()].clone();
                    scalar_temp.id = TempId::from_usize(func.temps.len());
                    scalar_temp.ty = ty;
                    scalar_temp.is_nullable = false;
                    values.insert(field, scalar_temp.id);
                    func.temps.push(scalar_temp);
                }
            }
            captured.insert(temp, values);
        }
    }
    let mut table = AmirStmtTable::new();
    for block in &mut func.blocks {
        let start = table.len();
        for id in block.statements.iter_ids::<crate::amir::InstrId>() {
            let stmt = &func.stmts.payloads[id];
            match stmt {
                AmirStmt::Store { lhs, rhs } if plans.contains_key(&lhs.local.as_usize()) => {
                    let plan = &plans[&lhs.local.as_usize()];
                    if lhs.projections.is_empty() {
                        if let AmirOperand::Copy(temp) = rhs
                            && let Some(literal) = literals.get(temp)
                        {
                            for (field, slot) in plan.slots.iter().enumerate() {
                                if let Some(local) = slot {
                                    table.push(AmirStmt::Store {
                                        lhs: AmirPlace {
                                            local: *local,
                                            projections: smallvec::SmallVec::new(),
                                        },
                                        rhs: literal.values[field],
                                    });
                                }
                            }
                        }
                    } else if let Some(field) = index(lhs, &plan.names, symbols)
                        && let Some(local) = plan.slots[field]
                    {
                        table.push(AmirStmt::Store {
                            lhs: AmirPlace {
                                local,
                                projections: smallvec::SmallVec::new(),
                            },
                            rhs: *rhs,
                        });
                    }
                }
                AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Load(place),
                } if plans.contains_key(&place.local.as_usize()) => {
                    let plan = &plans[&place.local.as_usize()];
                    if let Some(values) = captured.get(lhs) {
                        for (&field, value) in values {
                            if let Some(local) = plan.slots[field] {
                                table.push(AmirStmt::Assign {
                                    lhs: *value,
                                    rhs: AmirRvalue::Load(AmirPlace {
                                        local,
                                        projections: smallvec::SmallVec::new(),
                                    }),
                                });
                            }
                        }
                    } else if let Some(field) = index(place, &plan.names, symbols)
                        && let Some(local) = plan.slots[field]
                    {
                        table.push(AmirStmt::Assign {
                            lhs: *lhs,
                            rhs: AmirRvalue::Load(AmirPlace {
                                local,
                                projections: smallvec::SmallVec::new(),
                            }),
                        });
                    }
                }
                AmirStmt::Assign {
                    lhs,
                    rhs:
                        AmirRvalue::FieldAccess {
                            base: AmirOperand::Copy(base),
                            field,
                        },
                } if captured.contains_key(base) => {
                    if let Some(temp) = captured[base].get(field) {
                        table.push(AmirStmt::Assign {
                            lhs: *lhs,
                            rhs: AmirRvalue::Use(AmirOperand::Copy(*temp)),
                        });
                    }
                }
                AmirStmt::StorageLive(local) | AmirStmt::StorageDead(local)
                    if plans.contains_key(&local.as_usize()) => {}
                _ => {
                    table.push(stmt.clone());
                }
            }
        }
        block.statements = DenseRange::new(start, table.len() - start);
    }
    func.stmts = table;
    true
}
