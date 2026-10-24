use crate::Span;
use crate::amir::{AmirFunc, AmirOperand, LocalId, TempId};
use crate::types::ArType;

pub(super) fn borrows_through_external_storage(
    func: &AmirFunc,
    interner: &crate::types::TypeInterner,
    local: LocalId,
    projections: &[crate::amir::AmirProjection],
) -> bool {
    if projections.is_empty() && holds_only_static_strings(func, interner, local) {
        return true;
    }
    func.locals.get(local.as_usize()).is_some_and(|local| {
        matches!(
            (projections.first(), interner.try_resolve(local.ty)),
            (
                Some(crate::amir::AmirProjection::Deref),
                Some(ArType::Ref(_) | ArType::RefMut(_))
            ) | (
                Some(
                    crate::amir::AmirProjection::Index(_)
                        | crate::amir::AmirProjection::IndexConstant(_)
                ),
                Some(ArType::Slice(_) | ArType::MutSlice(_))
            )
        )
    })
}

/// A `str` local whose every write is a pool literal only holds descriptors of
/// static bytes: a view derived through `ref local` (e.g. `asBytes("")`) points
/// at those bytes, not at the stack slot. Owned interpolation results and any
/// mutable borrow keep the conservative stack origin.
fn holds_only_static_strings(
    func: &AmirFunc,
    interner: &crate::types::TypeInterner,
    local: LocalId,
) -> bool {
    use crate::amir::{AmirConstant, AmirRvalue, AmirStmt};
    let is_str = func.locals.get(local.as_usize()).is_some_and(|slot| {
        matches!(
            interner.try_resolve(slot.ty),
            Some(ArType::Primitive(crate::types::Primitive::Str))
        )
    });
    if !is_str {
        return false;
    }
    let mut writes = 0usize;
    for statement in func
        .blocks
        .iter()
        .flat_map(|block| func.block_stmts(block.id))
    {
        match statement {
            AmirStmt::Store { lhs, rhs } if lhs.local == local => {
                if !lhs.projections.is_empty()
                    || !matches!(rhs, AmirOperand::Constant(AmirConstant::Pool(_)))
                {
                    return false;
                }
                writes += 1;
            }
            AmirStmt::Assign {
                rhs: AmirRvalue::BorrowMut(place),
                ..
            } if place.local == local => return false,
            _ => {}
        }
    }
    writes > 0
}

/// Whether a value can hide a first-class borrow inside an aggregate. Slices
/// are intentionally excluded: their fat-view ABI is Copy and their owner
/// lifetime is checked at use sites, while aggregates containing explicit
/// references still require the generational-fallback diagnostic.
pub(super) fn type_is_borrow_carrier(
    ty: crate::types::TypeId,
    interner: &crate::types::TypeInterner,
    type_info: Option<&arandu_typeck::TypeInfo>,
) -> bool {
    if let Some(type_info) = type_info {
        let is_aggregate = matches!(
            interner.resolve(ty),
            ArType::Named(_, _)
                | ArType::Tuple(_)
                | ArType::Option(_)
                | ArType::Nullable(_)
                | ArType::Array(_, _)
                | ArType::ConstArray(_, _)
                | ArType::Result(_, _)
                | ArType::Poll(_)
                | ArType::Coroutine(_)
                | ArType::Range(_)
        );
        return is_aggregate && type_info.contains_explicit_reference(ty);
    }

    fn contains_reference(
        ty: crate::types::TypeId,
        interner: &crate::types::TypeInterner,
        seen: &mut rustc_hash::FxHashSet<crate::types::TypeId>,
    ) -> bool {
        let mut pending = vec![ty];
        while let Some(ty) = pending.pop() {
            if !seen.insert(ty) {
                continue;
            }
            match interner.resolve(ty) {
                ArType::Ref(_) | ArType::RefMut(_) => return true,
                ArType::Named(_, _) => return true,
                ArType::Option(inner)
                | ArType::Nullable(inner)
                | ArType::Array(_, inner)
                | ArType::ConstArray(_, inner)
                | ArType::Poll(inner)
                | ArType::Coroutine(inner)
                | ArType::Range(inner) => pending.push(inner),
                ArType::Result(ok, err) => {
                    pending.push(ok);
                    pending.push(err);
                }
                ArType::Tuple(args) => pending.extend(interner.type_args(args)),
                // Slices are borrowed fat views, but not hidden
                // reference-bearing aggregates for this fallback heuristic.
                ArType::Slice(_) | ArType::MutSlice(_) => {}
                _ => {}
            }
        }
        false
    }

    match interner.resolve(ty) {
        ArType::Named(_, _) => true,
        ArType::Tuple(args) => interner
            .type_args(args)
            .into_iter()
            .any(|ty| contains_reference(ty, interner, &mut rustc_hash::FxHashSet::default())),
        ArType::Option(inner)
        | ArType::Nullable(inner)
        | ArType::Array(_, inner)
        | ArType::ConstArray(_, inner)
        | ArType::Poll(inner)
        | ArType::Coroutine(inner)
        | ArType::Range(inner) => {
            contains_reference(inner, interner, &mut rustc_hash::FxHashSet::default())
        }
        ArType::Result(ok, err) => {
            contains_reference(ok, interner, &mut rustc_hash::FxHashSet::default())
                || contains_reference(err, interner, &mut rustc_hash::FxHashSet::default())
        }
        _ => false,
    }
}

pub(super) fn is_return_temp(t: TempId, func: &AmirFunc) -> bool {
    t.as_usize() == 0 || func.params.first().is_some_and(|p| *p == t)
}

pub(super) fn operand_stack_place(
    op: &AmirOperand,
    temp_to_place: &[Option<LocalId>],
) -> Option<LocalId> {
    match op {
        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
            temp_to_place.get(t.as_usize()).copied().flatten()
        }
        _ => None,
    }
}

pub(super) fn local_span(local: LocalId, func: &AmirFunc) -> Span {
    func.locals
        .get(local.as_usize())
        .map(|l| l.span)
        .unwrap_or_else(|| Span::new(0, 0, 0))
}

pub(super) fn temp_span(t: TempId, func: &AmirFunc) -> Option<Span> {
    func.temps.get(t.as_usize()).map(|t| t.span)
}

pub(super) fn place_span(place: &crate::amir::AmirPlace, func: &AmirFunc) -> Span {
    local_span(place.local, func)
}
