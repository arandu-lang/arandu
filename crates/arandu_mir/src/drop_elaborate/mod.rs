//! Elaborate semantic cleanup obligations into explicit AMIR `Destroy`.
//!
//! Arandu currently rejects inconsistent moves between CFG branches, so a
//! valid return boundary never needs a runtime drop flag: each initialized
//! local is either available on every path or moved on every path. This pass
//! reuses the move and definite-init facts that enforce that rule.

mod dynamic;
mod elaborate;
mod needs_drop;

pub use elaborate::elaborate_drops;
pub use needs_drop::type_needs_drop;

#[cfg(test)]
use crate::amir::{AmirPlace, AmirProjection, AmirStmt, AmirStmtTable, LocalId};
#[cfg(test)]
use crate::move_checker::MoveState;
#[cfg(test)]
use arandu_middle::types::ArType;
#[cfg(test)]
use arandu_typeck::TypeInfo;
#[cfg(test)]
use needs_drop::emit_recursive_drops;
#[cfg(test)]
use smallvec::SmallVec;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolId;
    use crate::types::Primitive;

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

        let destroyed_indices: Vec<_> = rebuilt
            .payloads
            .iter()
            .filter_map(|stmt| match stmt {
                AmirStmt::Destroy(place) => match place.projections.last() {
                    Some(AmirProjection::IndexConstant(index)) => Some(*index),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(destroyed_indices, [2, 1, 0]);
    }

    #[test]
    fn tuple_drop_glue_is_expanded_in_reverse_field_order() {
        let mut type_info = TypeInfo::new();
        let destructible = SymbolId::new(0, 42);
        let destructible_ty = type_info.type_interner.intern(ArType::named(
            destructible,
            &[],
            &type_info.type_interner,
        ));
        type_info
            .destructor_instances
            .insert(destructible_ty, SymbolId::new(0, 99));
        let int_ty = type_info
            .type_interner
            .intern(ArType::Primitive(Primitive::Int));
        let tuple_ty = type_info.type_interner.intern(ArType::tuple(
            &[destructible_ty, int_ty, destructible_ty],
            &type_info.type_interner,
        ));
        let place = AmirPlace {
            local: LocalId::from_usize(0),
            projections: SmallVec::new(),
        };
        let mut rebuilt = AmirStmtTable::new();

        emit_recursive_drops(
            &place,
            tuple_ty,
            &type_info,
            &MoveState::default(),
            false,
            &mut rebuilt,
        );

        let destroyed_fields = rebuilt
            .payloads
            .iter()
            .filter_map(|stmt| match stmt {
                AmirStmt::Destroy(place) => place.projections.last().and_then(|projection| {
                    if let AmirProjection::TupleField(index) = projection {
                        Some(*index)
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(destroyed_fields, [2, 0]);
    }
}
