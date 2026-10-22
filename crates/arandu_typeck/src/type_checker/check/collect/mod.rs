mod shapes;
mod signatures;

use arandu_parser::Ownership;

use crate::type_checker::TypeChecker;
use crate::type_checker::types::ArType;

pub(crate) use shapes::collect_type_shapes;
pub(crate) use signatures::collect_signature_types;

/// Wrap a receiver's bare type with the ownership qualifier.
///
/// Legacy receiver prefixes are lowered to the canonical `ref T` / `mut ref T`
/// types. Canonical type-qualified parameters already arrive wrapped and pass
/// through unchanged.
#[inline]
pub(crate) fn apply_receiver_ownership(
    checker: &mut TypeChecker<'_>,
    bare_ty_id: arandu_middle::types::TypeId,
    ownership: Option<Ownership>,
) -> arandu_middle::types::TypeId {
    match ownership {
        Some(Ownership::Shared) => checker.intern(ArType::Ref(bare_ty_id)),
        Some(Ownership::Mut) => checker.intern(ArType::RefMut(bare_ty_id)),
        Some(Ownership::Own) | None => bare_ty_id,
    }
}
