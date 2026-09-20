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
        ArType::Array(len, elem) => len > 0 && type_needs_drop(elem, type_info),
        ArType::ConstArray(_, elem) => type_needs_drop(elem, type_info),
        ArType::Option(elem) => type_needs_drop(elem, type_info),
        ArType::Result(ok, err) => {
            type_needs_drop(ok, type_info) || type_needs_drop(err, type_info)
        }
        ArType::Tuple(args) => {
            let arg_ids = type_info.type_interner.type_args(args);
            arg_ids
                .iter()
                .any(|&arg_ty| type_needs_drop(arg_ty, type_info))
        }
        _ => false,
    }
}

/// Emits `Destroy` statements for `place`:
/// 1. Runs the type's custom `@Destructor` if declared (unless skipping top-level).
/// 2. Recursively destroys composite struct fields in reverse declaration order.
/// 3. Destroys container types (`Array`, `Option`, `Result`, `Tuple`).
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
    match resolved {
        ArType::Named(sym, _) => {
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
                        emit_recursive_drops(
                            &sub_place, fty, type_info, move_state, false, rebuilt,
                        );
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
        ArType::Array(..)
        | ArType::ConstArray(..)
        | ArType::Option(..)
        | ArType::Result(..)
        | ArType::Tuple(..) => {
            rebuilt.push(AmirStmt::Destroy(place.clone()));
        }
        _ => {}
    }
}

/// Identifies locals whose string ownership is unambiguous for the whole function.
///
/// A primitive `str` currently carries no runtime ownership bit. Consequently,
/// a local is safe to destroy only when it has exactly one root store and that
/// store receives a freshly allocated string. Treating a local as owned merely
/// because *one* of its stores is owned can free static string data after a
/// later reassignment. Multiple stores are deliberately left alone until AMIR
/// models string ownership explicitly or this pass grows path-sensitive drop
/// flags.
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
    let mut root_store_count = rustc_hash::FxHashMap::<LocalId, usize>::default();
    let mut owned_store_count = rustc_hash::FxHashMap::<LocalId, usize>::default();
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Store { lhs, rhs } = stmt
            && lhs.projections.is_empty()
        {
            *root_store_count.entry(lhs.local).or_default() += 1;
            if matches!(
                rhs,
                crate::amir::AmirOperand::Copy(t) | crate::amir::AmirOperand::Move(t)
                    if owned_temps.contains(t)
            ) {
                *owned_store_count.entry(lhs.local).or_default() += 1;
            }
        }
    }
    owned_store_count
        .into_iter()
        .filter_map(|(local, owned)| {
            (owned == 1 && root_store_count.get(&local) == Some(&1)).then_some(local)
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolId;
    use crate::passes::type_checker::types::Primitive;

    #[test]
    fn test_type_needs_drop_containers() {
        let mut type_info = TypeInfo::new();
        let sym_destructible = SymbolId::new(0, 42);
        let destructible_ty_id = type_info.type_interner.intern(ArType::named(
            sym_destructible,
            &[],
            &type_info.type_interner,
        ));
        let destructor_fn = SymbolId::new(0, 99);
        type_info
            .destructor_instances
            .insert(destructible_ty_id, destructor_fn);

        let int_ty_id = type_info
            .type_interner
            .intern(ArType::Primitive(Primitive::Int));

        assert!(type_needs_drop(destructible_ty_id, &type_info));
        assert!(!type_needs_drop(int_ty_id, &type_info));

        // Array
        let arr_empty = type_info
            .type_interner
            .intern(ArType::Array(0, destructible_ty_id));
        assert!(!type_needs_drop(arr_empty, &type_info));

        let arr_active = type_info
            .type_interner
            .intern(ArType::Array(3, destructible_ty_id));
        assert!(type_needs_drop(arr_active, &type_info));

        let arr_int = type_info.type_interner.intern(ArType::Array(3, int_ty_id));
        assert!(!type_needs_drop(arr_int, &type_info));

        // ConstArray
        let const_arr = type_info
            .type_interner
            .intern(ArType::ConstArray(SymbolId::new(0, 10), destructible_ty_id));
        assert!(type_needs_drop(const_arr, &type_info));

        // Option
        let opt_res = type_info
            .type_interner
            .intern(ArType::Option(destructible_ty_id));
        assert!(type_needs_drop(opt_res, &type_info));
        let opt_int = type_info.type_interner.intern(ArType::Option(int_ty_id));
        assert!(!type_needs_drop(opt_int, &type_info));

        // Result
        let res_ok = type_info
            .type_interner
            .intern(ArType::Result(destructible_ty_id, int_ty_id));
        assert!(type_needs_drop(res_ok, &type_info));
        let res_err = type_info
            .type_interner
            .intern(ArType::Result(int_ty_id, destructible_ty_id));
        assert!(type_needs_drop(res_err, &type_info));
        let res_int = type_info
            .type_interner
            .intern(ArType::Result(int_ty_id, int_ty_id));
        assert!(!type_needs_drop(res_int, &type_info));

        // Tuple
        let tup_res = type_info.type_interner.intern(ArType::tuple(
            &[int_ty_id, destructible_ty_id],
            &type_info.type_interner,
        ));
        assert!(type_needs_drop(tup_res, &type_info));

        let tup_int = type_info.type_interner.intern(ArType::tuple(
            &[int_ty_id, int_ty_id],
            &type_info.type_interner,
        ));
        assert!(!type_needs_drop(tup_int, &type_info));
    }

    #[test]
    fn test_emit_recursive_drops_for_containers() {
        let mut type_info = TypeInfo::new();
        let sym_destructible = SymbolId::new(0, 42);
        let destructible_ty_id = type_info.type_interner.intern(ArType::named(
            sym_destructible,
            &[],
            &type_info.type_interner,
        ));
        let destructor_fn = SymbolId::new(0, 99);
        type_info
            .destructor_instances
            .insert(destructible_ty_id, destructor_fn);

        let arr_ty = type_info
            .type_interner
            .intern(ArType::Array(3, destructible_ty_id));

        let place = AmirPlace {
            local: LocalId::from_usize(0),
            projections: SmallVec::new(),
        };
        let move_state = MoveState::default();
        let mut rebuilt = AmirStmtTable::new();

        emit_recursive_drops(&place, arr_ty, &type_info, &move_state, false, &mut rebuilt);

        assert_eq!(rebuilt.len(), 1);
        match &rebuilt.payloads[crate::amir::stmt::InstrId::from_usize(0)] {
            AmirStmt::Destroy(p) => assert_eq!(p.local, place.local),
            other => panic!("expected Destroy statement, got: {other:?}"),
        }
    }
}
