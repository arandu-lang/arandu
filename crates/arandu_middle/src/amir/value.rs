use super::local::{LocalId, TempId};
use crate::SymbolId;
use crate::literal_pool::LiteralId;
use crate::ops::{BinaryOp, UnaryOp};
use crate::types::TypeId;
use arandu_lexer::Span;
use smallvec::SmallVec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmirPlace {
    pub local: LocalId,
    pub projections: SmallVec<[AmirProjection; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmirProjection {
    Field(SymbolId),
    Index(AmirOperand),
    /// One level of indirection: base local holds a pointer (BC.4a heap/`ptr`).
    /// Address of place = value of local (+ later field/index offsets).
    Deref,
}

#[derive(Debug, Clone)]
pub enum AmirRvalue {
    Use(AmirOperand),
    Binary {
        op: BinaryOp,
        left: AmirOperand,
        right: AmirOperand,
    },
    Unary {
        op: UnaryOp,
        operand: AmirOperand,
    },
    FieldAccess {
        base: AmirOperand,
        field: usize,
    },
    StructLiteral {
        struct_symbol: SymbolId,
        fields: Vec<(smol_str::SmolStr, AmirOperand)>,
    },
    IndexAccess {
        base: AmirOperand,
        index: AmirOperand,
    },
    Array {
        items: Vec<AmirOperand>,
    },
    Tuple {
        items: Vec<AmirOperand>,
    },
    Discriminant {
        value: AmirOperand,
    },
    EnumPayload {
        value: AmirOperand,
        variant: SymbolId,
        index: usize,
    },
    EnumConstruct {
        variant_tag: usize,
        payload: Option<AmirOperand>,
    },
    Len(AmirOperand),
    /// Extract the raw data pointer from a slice fat pointer.
    SliceData(AmirOperand),

    /// Construct a safe `[]T` view from an owner borrow plus raw storage.
    /// `owner` is compile-time provenance; backends erase it and emit ptr+len.
    SliceView {
        owner: AmirOperand,
        data: AmirOperand,
        len: AmirOperand,
    },
    /// Derive a ptr+len subview while preserving the source slice provenance.
    SliceSubslice {
        slice: AmirOperand,
        start: AmirOperand,
        len: AmirOperand,
    },
    /// Borrow a string's UTF-8 storage as a `[]u8` descriptor.
    StrBytes {
        source: AmirOperand,
    },
    /// Reinterpret the `String` owner prefix (`data`, `len`) as `ref str`.
    /// The owner operand is both the runtime address and borrow provenance.
    StrView {
        owner: AmirOperand,
    },

    Alloc(AmirOperand),
    /// Load value from a stack-allocated local place (memory) into an SSA register.
    Load(AmirPlace),
    /// Create a shared borrow (reference) of a place.
    Borrow(AmirPlace),
    /// Create a mutable borrow (mutable reference) of a place.
    BorrowMut(AmirPlace),
    /// A3.4: pin-free borrow — value is `LocalId` as an index into coroutine /
    /// frame state, **not** a raw address. Survives stack↔heap moves of the
    /// state blob. Loads through it are rewritten to `Load` of the local.
    RelativeBorrow {
        local: LocalId,
        /// When true, surface type is `&mut T`; otherwise `&T`.
        mutable: bool,
    },
    /// A3.0/A3.3: wrap a ready payload as `Coroutine[T]` (pointer to state / payload).
    /// Full multi-state machines land in later A3; ready-only has state = payload at +0.
    CoroutineReady {
        value: AmirOperand,
        /// Payload type `T` inside `Coroutine[T]` (for layout / store width).
        payload_ty: TypeId,
        /// A3.3: when true, backends place state on the **stack** (no malloc).
        /// Set for non-escaping `async {}` values; `async func` returns use `false`
        /// (state must outlive the callee frame). Escape of a stack coroutine is
        /// rewritten to heap or rejected by analysis.
        stack: bool,
    },
    /// Insert `value` into a generational arena.
    ///
    /// `payload_ty` is semantic metadata: it prevents backends from recovering
    /// `T` from an incidental machine representation when producing layout and
    /// drop glue. `origin` preserves the source escape that introduced the
    /// operation for diagnostics and deterministic traps.
    GenInsert {
        value: AmirOperand,
        payload_ty: TypeId,
        arena: GenArenaDomain,
        origin: Span,
    },
    /// Borrow/load a payload through a gen ref (traps on an invalid handle).
    GenGet {
        gen_ref: AmirOperand,
        payload_ty: TypeId,
        arena: GenArenaDomain,
        origin: Span,
    },
    /// Replace the payload behind a live gen ref and return the same handle.
    ///
    /// Returning the handle keeps the operation in SSA form while preserving
    /// the arena identity shared by every alias. It is effectful even when the
    /// result is unused because the previous payload may be dropped.
    GenSet {
        gen_ref: AmirOperand,
        value: AmirOperand,
        payload_ty: TypeId,
        arena: GenArenaDomain,
        origin: Span,
    },
    /// Insert when `gen_ref` is the reserved zero handle, otherwise replace
    /// the live payload, returning the canonical handle in either case.
    ///
    /// This models path-sensitive initialization without speculating that one
    /// textual store dominates every CFG path.
    GenUpsert {
        gen_ref: AmirOperand,
        value: AmirOperand,
        payload_ty: TypeId,
        arena: GenArenaDomain,
        origin: Span,
    },
    /// Move a payload out and recycle/retire its slot, invalidating the ref.
    GenRemove {
        gen_ref: AmirOperand,
        payload_ty: TypeId,
        arena: GenArenaDomain,
        origin: Span,
    },
    /// Concatenate a sequence of `str` operands into a single `str` value.
    /// Generated by the compiler for string interpolation (`"hello $name"`).
    StringInterp {
        parts: Vec<AmirOperand>,
    },
    /// Format a ToStr-v0.1-supported value as `str` (fat pointer).
    /// `src_ty` is the source type so backends can pick the right conversion
    /// without re-inferring from the operand alone.
    ToStr {
        value: AmirOperand,
        src_ty: TypeId,
    },
    /// Benchmark-only optimization barrier. It is an identity operation at
    /// runtime, but remains observable to AMIR optimizations and is lowered to
    /// an opaque runtime call by every backend.
    BlackBox {
        value: AmirOperand,
        value_ty: TypeId,
    },
}

/// Logical arena selected by a Gen operation.
///
/// The compiler-managed fallback is deliberately distinct from the explicit
/// `GenArena<T>` surface API. A future explicit-arena AMIR form can extend this
/// enum without changing the meaning of existing fallback operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenArenaDomain {
    CompilerManaged,
}

/// SSA/register operands are pure ids + constants — always cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmirOperand {
    Copy(TempId),
    Move(TempId),
    Constant(AmirConstant),
    FunctionRef(SymbolId),
    GlobalRef(SymbolId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operand_copy_vs_move() {
        let t = TempId::from_usize(42);
        match AmirOperand::Copy(t) {
            AmirOperand::Copy(id) => assert_eq!(id, t),
            _ => panic!("expected Copy"),
        }
        match AmirOperand::Move(t) {
            AmirOperand::Move(id) => assert_eq!(id, t),
            _ => panic!("expected Move"),
        }
    }

    #[test]
    fn constant_bool() {
        assert_eq!(AmirConstant::Bool(true), AmirConstant::Bool(true));
        assert_ne!(AmirConstant::Bool(true), AmirConstant::Bool(false));
    }

    #[test]
    fn constant_pool() {
        let id = LiteralId(7);
        assert_eq!(AmirConstant::Pool(id), AmirConstant::Pool(id));
        assert_ne!(AmirConstant::Pool(id), AmirConstant::Nil);
    }

    #[test]
    fn place_no_projections() {
        let p = AmirPlace {
            local: LocalId::from_usize(0),
            projections: SmallVec::new(),
        };
        assert!(p.projections.is_empty());
        assert_eq!(p.local, LocalId::from_usize(0));
    }

    #[test]
    fn place_with_field_projection() {
        let sym = SymbolId::new(0, 99);
        let proj = AmirProjection::Field(sym);
        let mut projections = SmallVec::new();
        projections.push(proj);
        let p = AmirPlace {
            local: LocalId::from_usize(1),
            projections,
        };
        assert_eq!(p.projections.len(), 1);
        match &p.projections[0] {
            AmirProjection::Field(s) => assert_eq!(*s, sym),
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn place_with_index_projection() {
        let idx = AmirOperand::Constant(AmirConstant::Bool(true));
        let proj = AmirProjection::Index(idx);
        let mut projections = SmallVec::new();
        projections.push(proj);
        let p = AmirPlace {
            local: LocalId::from_usize(2),
            projections,
        };
        assert_eq!(p.projections.len(), 1);
        match &p.projections[0] {
            AmirProjection::Index(_) => {}
            _ => panic!("expected Index"),
        }
    }

    #[test]
    fn place_with_deref_projection() {
        let mut projections = SmallVec::new();
        projections.push(AmirProjection::Deref);
        let p = AmirPlace {
            local: LocalId::from_usize(3),
            projections,
        };
        assert_eq!(p.projections.len(), 1);
        assert!(matches!(p.projections[0], AmirProjection::Deref));
    }

    #[test]
    fn rvalue_use_operand() {
        let op = AmirOperand::Constant(AmirConstant::Nil);
        let rv = AmirRvalue::Use(op);
        match rv {
            AmirRvalue::Use(_) => {}
            _ => panic!("expected Use"),
        }
    }

    #[test]
    fn rvalue_unary() {
        let rv = AmirRvalue::Unary {
            op: crate::ops::UnaryOp::Neg,
            operand: AmirOperand::Constant(AmirConstant::Bool(false)),
        };
        match rv {
            AmirRvalue::Unary { op: _, operand: _ } => {}
            _ => panic!("expected Unary"),
        }
    }

    #[test]
    fn rvalue_field_access() {
        let rv = AmirRvalue::FieldAccess {
            base: AmirOperand::Copy(TempId::from_usize(5)),
            field: 1,
        };
        match rv {
            AmirRvalue::FieldAccess { base: _, field } => assert_eq!(field, 1),
            _ => panic!("expected FieldAccess"),
        }
    }

    #[test]
    fn rvalue_discriminant() {
        let rv = AmirRvalue::Discriminant {
            value: AmirOperand::Copy(TempId::from_usize(3)),
        };
        match rv {
            AmirRvalue::Discriminant { value: _ } => {}
            _ => panic!("expected Discriminant"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AmirConstant {
    Pool(LiteralId),
    Bool(bool),
    Nil,
}
