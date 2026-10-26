#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ArType, Primitive, TypeInterner};

    #[test]
    fn local_id_roundtrip() {
        let id = LocalId::from_usize(42);
        assert_eq!(id.as_usize(), 42);
    }

    #[test]
    fn temp_id_roundtrip() {
        let id = TempId::from_usize(7);
        assert_eq!(id.as_usize(), 7);
    }

    #[test]
    fn amir_local_construction() {
        let interner = TypeInterner::new();
        let ty = interner.intern(ArType::Primitive(Primitive::Int));
        let local = AmirLocal {
            id: LocalId::from_usize(0),
            ty,
            is_memory: false,
            symbol: Some(SymbolId::new(0, 1)),
            span: Span::new(0, 0, 0),
            use_span: None,
        };
        assert_eq!(local.id.as_usize(), 0);
        assert!(local.use_span.is_none());
    }

    #[test]
    fn amir_temp_construction() {
        let interner = TypeInterner::new();
        let ty = interner.intern(ArType::Primitive(Primitive::Bool));
        let temp = AmirTemp {
            id: TempId::from_usize(3),
            ty,
            is_copy: true,
            is_nullable: false,
            span: Span::new(0, 0, 0),
        };
        assert_eq!(temp.id.as_usize(), 3);
        assert!(temp.is_copy);
    }

    #[test]
    fn amir_receiver_construction() {
        let recv = AmirReceiver {
            temp: TempId::from_usize(0),
            kind: ReceiverKind::Shared,
        };
        assert_eq!(recv.temp.as_usize(), 0);
        assert_eq!(recv.kind, ReceiverKind::Shared);
    }
}

use crate::Span;
use crate::SymbolId;
use crate::hir::ReceiverKind;
use crate::newtype_index;
use crate::types::TypeId;

newtype_index!(LocalId);
newtype_index!(TempId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AmirReceiver {
    pub temp: TempId,
    pub kind: ReceiverKind,
}

/// Stack local. `ty` is a dense [`TypeId`] (DoD — no owned `ArType` tree).
#[derive(Debug, Clone)]
pub struct AmirLocal {
    pub id: LocalId,
    pub ty: TypeId,
    /// Denormalized: true if this local is memory-backed (struct/str/array/…).
    /// Lets prune/analyses avoid resolving `TypeId` without an interner.
    pub is_memory: bool,
    pub symbol: Option<SymbolId>,
    pub span: Span,
    /// Latest source use site recorded during AMIR lower (S-USE-SPAN).
    pub use_span: Option<Span>,
}

/// SSA temporary. `ty` is interned; `is_copy` / `is_nullable` are denormalized
/// so move analysis and SCCP need no `TypeInterner` on the hot path.
#[derive(Debug, Clone)]
pub struct AmirTemp {
    pub id: TempId,
    pub ty: TypeId,
    pub is_copy: bool,
    /// True when `ty` is `T?` (null-or-pointer handle). SCCP must not fold
    /// non-Nil scalar constants into these temps (`int? = 0` ≠ `nil`).
    pub is_nullable: bool,
    pub span: Span,
}
