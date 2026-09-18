//! Elaborate semantic cleanup obligations into explicit AMIR `Destroy`.
//!
//! Arandu currently rejects inconsistent moves between CFG branches, so a
//! valid return boundary never needs a runtime drop flag: each initialized
//! local is either available on every path or moved on every path. This pass
//! reuses the move and definite-init facts that enforce that rule.

use crate::amir::{
    AmirFunc, AmirPlace, AmirProjection, AmirStmt, AmirStmtTable, AmirTerminator, LocalId,
};
use crate::move_checker::{MoveState, move_states_at_block_exit};
use arandu_middle::SymbolId;
use arandu_middle::layout::{DenseRange, instantiated_field_type};
use arandu_middle::types::{ArType, TypeId};
use arandu_typeck::TypeInfo;
use smallvec::SmallVec;

/// Checks recursively whether a type or any of its composite fields needs cleanup.
fn type_needs_drop(ty: TypeId, type_info: &TypeInfo) -> bool {
    let resolved = type_info.resolve_type_id(ty);
    match resolved {
        ArType::Named(sym, _) => {
            if type_info.destructor_instances.contains_key(&ty) {
                return true;
            }
            if let Some(fields) = type_info.struct_fields.get(&sym) {
                for f in fields.iter() {
                    let field_ty = instantiated_field_type(
                        &resolved,
                        &f.name,
                        &type_info.type_interner,
                        type_info,
                    )
                    .unwrap_or(f.ty);
                    if type_needs_drop(field_ty, type_info) {
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

/// Emits `Destroy` statements for `place`:
/// 1. Runs the type's custom `@Destructor` if declared (unless skipping top-level).
/// 2. Recursively destroys composite struct fields in reverse declaration order.
fn emit_recursive_drops(
    place: &AmirPlace,
    ty: TypeId,
    type_info: &TypeInfo,
    move_state: &MoveState,
    skip_top_level_destructor: bool,
    rebuilt: &mut AmirStmtTable,
) {
    if !move_state.place_itself_is_available(place) {
        return;
    }
    let resolved = type_info.resolve_type_id(ty);
    if let ArType::Named(sym, _) = resolved {
        let has_destructor = type_info.destructor_instances.contains_key(&ty);
        if has_destructor && !skip_top_level_destructor {
            rebuilt.push(AmirStmt::Destroy(place.clone()));
            return;
        }

        if let Some(fields) = type_info.struct_fields.get(&sym) {
            let mut indexed: Vec<(usize, Option<SymbolId>, TypeId)> = fields
                .iter()
                .map(|f| {
                    let ty = instantiated_field_type(
                        &resolved,
                        &f.name,
                        &type_info.type_interner,
                        type_info,
                    )
                    .unwrap_or(f.ty);
                    (f.index, f.symbol, ty)
                })
                .collect();
            indexed.sort_by_key(|(idx, _, _)| std::cmp::Reverse(*idx));

            for (_, fsym, fty) in indexed {
                if type_needs_drop(fty, type_info) {
                    let mut sub_place = place.clone();
                    if let Some(fsym) = fsym {
                        sub_place.projections.push(AmirProjection::Field(fsym));
                    }
                    emit_recursive_drops(&sub_place, fty, type_info, move_state, false, rebuilt);
                }
            }

            // The composite has no explicit destructor, so its own storage is
            // still a cleanup obligation: the fields were walked above and the
            // root comes last. Backends whose aggregates live in frames treat
            // this `Destroy` as a no-op; a backend that owns heap storage (the
            // wasm cell model) reclaims the cell here.
            if !has_destructor {
                rebuilt.push(AmirStmt::Destroy(place.clone()));
            }
        }
    }
}

/// Identifies locals that hold dynamically allocated string buffers from `ToStr` or `StringInterp`.
fn find_owned_string_locals(func: &AmirFunc) -> rustc_hash::FxHashSet<LocalId> {
    let mut owned_temps = rustc_hash::FxHashSet::default();
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Assign { lhs, rhs } = stmt
            && matches!(
                rhs,
                crate::amir::AmirRvalue::ToStr { .. }
                    | crate::amir::AmirRvalue::StringInterp { .. }
            )
        {
            owned_temps.insert(lhs);
        }
    }
    let mut owned_locals = rustc_hash::FxHashSet::default();
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Store { lhs, rhs } = stmt
            && lhs.projections.is_empty()
        {
            match rhs {
                crate::amir::AmirOperand::Copy(t) | crate::amir::AmirOperand::Move(t)
                    if owned_temps.contains(t) =>
                {
                    owned_locals.insert(lhs.local);
                }
                _ => {}
            }
        }
    }
    owned_locals
}

/// Insert exactly-once root-local and nested cascade destruction before normal function returns.
pub fn elaborate_drops(func: &mut AmirFunc, type_info: &TypeInfo) {
    let owned_string_locals = find_owned_string_locals(func);
    let is_destructor_func = type_info
        .destructor_instances
        .values()
        .any(|symbol| *symbol == func.symbol);

    let initialized = crate::definite_init::initialized_at_block_exit(func);
    let moved = move_states_at_block_exit(func);
    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut rebuilt = AmirStmtTable::new();
    let mut ranges = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let start = rebuilt.len();
        for id in block.statements.iter_ids::<crate::amir::InstrId>() {
            rebuilt.push(old.payloads[id].clone());
        }
        if matches!(block.terminator, AmirTerminator::Return) {
            let move_state = &moved[block.id.as_usize()];
            for local in func.locals.iter().rev() {
                let skip_self = is_destructor_func && local.id.as_usize() == 0;
                let is_owned_string = owned_string_locals.contains(&local.id);
                if (type_needs_drop(local.ty, type_info) || is_owned_string)
                    && initialized[block.id.as_usize()].contains(local.id)
                {
                    let root_place = AmirPlace {
                        local: LocalId::from_usize(local.id.as_usize()),
                        projections: SmallVec::new(),
                    };
                    if is_owned_string {
                        if move_state.place_itself_is_available(&root_place) {
                            rebuilt.push(AmirStmt::Destroy(root_place));
                        }
                    } else {
                        emit_recursive_drops(
                            &root_place,
                            local.ty,
                            type_info,
                            move_state,
                            skip_self,
                            &mut rebuilt,
                        );
                    }
                }
            }
        }
        ranges.push(DenseRange::new(start, rebuilt.len() - start));
    }

    func.stmts = rebuilt;
    for (block, range) in func.blocks.iter_mut().zip(ranges) {
        block.statements = range;
    }
}
