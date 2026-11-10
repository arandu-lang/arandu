use super::block::BlockId;
use super::local::TempId;
use super::value::{AmirOperand, AmirPlace, AmirRvalue};
use crate::index_vec::IndexVec;
use crate::newtype_index;
use smallvec::SmallVec;

use crate::types::ReturnBorrowSummary;

newtype_index!(InstrId);

/// Provenance carried by a reference returned from a call.
pub type CallBorrowDependency = ReturnBorrowSummary;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmirStmtKind {
    Assign,
    Store,
    Call,
    Free,
    ArenaRewind,
    StorageLive,
    StorageDead,
    Destroy,
    Nop,
}

#[derive(Debug, Clone)]
pub enum AmirStmt {
    /// Binds an SSA register to an RValue.
    Assign {
        lhs: TempId,
        rhs: AmirRvalue,
    },
    /// Stores an operand into a memory location (local stack slot or projection).
    Store {
        lhs: AmirPlace,
        rhs: AmirOperand,
    },
    Call {
        lhs: Option<TempId>,
        callee: AmirOperand,
        args: SmallVec<[AmirOperand; 4]>,
        return_borrow: Option<CallBorrowDependency>,
    },
    Free(AmirOperand),
    /// Rewinds the current thread's `TaskArena` cursor to a checkpoint mark (RFC 0043).
    ArenaRewind(AmirOperand),
    /// Declares that a stack slot's lifetime is active.
    StorageLive(super::local::LocalId),
    /// Declares that a stack slot's lifetime has ended.
    StorageDead(super::local::LocalId),
    /// Explicitly drop/destroy a value at a place.
    Destroy(AmirPlace),
    Nop,
}

impl AmirStmt {
    #[must_use]
    pub const fn kind(&self) -> AmirStmtKind {
        match self {
            Self::Assign { .. } => AmirStmtKind::Assign,
            Self::Store { .. } => AmirStmtKind::Store,
            Self::Call { .. } => AmirStmtKind::Call,
            Self::Free(_) => AmirStmtKind::Free,
            Self::ArenaRewind(_) => AmirStmtKind::ArenaRewind,
            Self::StorageLive(_) => AmirStmtKind::StorageLive,
            Self::StorageDead(_) => AmirStmtKind::StorageDead,
            Self::Destroy(_) => AmirStmtKind::Destroy,
            Self::Nop => AmirStmtKind::Nop,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct AmirStmtTable {
    pub kinds: IndexVec<InstrId, AmirStmtKind>,
    pub payloads: IndexVec<InstrId, AmirStmt>,
}

impl AmirStmtTable {
    #[must_use]
    pub fn new() -> Self {
        Self {
            kinds: IndexVec::new(),
            payloads: IndexVec::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.payloads.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.payloads.is_empty()
    }

    pub fn push(&mut self, stmt: AmirStmt) -> InstrId {
        let kind = stmt.kind();
        let id = self.payloads.push(stmt);
        let kind_id = self.kinds.push(kind);
        debug_assert_eq!(id, kind_id);
        id
    }

    #[must_use]
    pub fn get(&self, id: InstrId) -> Option<&AmirStmt> {
        self.payloads.get(id)
    }

    pub fn get_mut(&mut self, id: InstrId) -> Option<&mut AmirStmt> {
        self.payloads.get_mut(id)
    }

    pub fn iter_ids(&self) -> impl Iterator<Item = InstrId> + '_ {
        self.payloads.ids()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::value::{AmirConstant, AmirOperand};

    #[test]
    fn empty_table() {
        let table = AmirStmtTable::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn push_and_get() {
        let mut table = AmirStmtTable::new();
        let id = table.push(AmirStmt::Nop);
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());
        assert!(table.get(id).is_some());
        assert_eq!(table.kinds[id], AmirStmtKind::Nop);
    }

    #[test]
    fn kinds_match_kind_method() {
        let mut table = AmirStmtTable::new();
        let id = table.push(AmirStmt::Nop);
        let fetched = table.get(id).unwrap();
        assert_eq!(fetched.kind(), AmirStmtKind::Nop);
    }

    #[test]
    fn kinds_match_for_storage() {
        let mut table = AmirStmtTable::new();
        let id = table.push(AmirStmt::StorageLive(
            super::super::local::LocalId::from_usize(7),
        ));
        assert_eq!(table.kinds[id], AmirStmtKind::StorageLive);
        assert_eq!(table.get(id).unwrap().kind(), AmirStmtKind::StorageLive);
    }

    #[test]
    fn push_multiple_get_correct_ids() {
        let mut table = AmirStmtTable::new();
        let id0 = table.push(AmirStmt::StorageLive(
            super::super::local::LocalId::from_usize(0),
        ));
        let id1 = table.push(AmirStmt::StorageDead(
            super::super::local::LocalId::from_usize(1),
        ));
        let id2 = table.push(AmirStmt::Free(AmirOperand::Constant(AmirConstant::Nil)));
        assert_eq!(id0, InstrId::from_usize(0));
        assert_eq!(id1, InstrId::from_usize(1));
        assert_eq!(id2, InstrId::from_usize(2));
        assert_eq!(
            table.kinds.as_slice(),
            &[
                AmirStmtKind::StorageLive,
                AmirStmtKind::StorageDead,
                AmirStmtKind::Free
            ]
        );
    }

    #[test]
    fn iter_ids_empty() {
        let table = AmirStmtTable::new();
        assert_eq!(table.iter_ids().count(), 0);
    }

    #[test]
    fn iter_ids_multiple() {
        let mut table = AmirStmtTable::new();
        table.push(AmirStmt::Nop);
        table.push(AmirStmt::Nop);
        table.push(AmirStmt::Nop);
        let ids: Vec<_> = table.iter_ids().collect();
        assert_eq!(
            ids,
            vec![
                InstrId::from_usize(0),
                InstrId::from_usize(1),
                InstrId::from_usize(2)
            ]
        );
    }

    #[test]
    fn get_mut_allows_modification() {
        let mut table = AmirStmtTable::new();
        let id = table.push(AmirStmt::Nop);
        assert_eq!(table.get(id).unwrap().kind(), AmirStmtKind::Nop);
        let replacement = AmirStmt::Free(AmirOperand::Constant(AmirConstant::Bool(true)));
        *table.get_mut(id).unwrap() = replacement;
        assert_eq!(table.kinds[id], AmirStmtKind::Nop);
        assert_eq!(table.get(id).unwrap().kind(), AmirStmtKind::Free);
    }
}

#[derive(Debug, Clone)]
pub enum AmirTerminator {
    Return,
    Goto {
        target: BlockId,
        args: Vec<AmirOperand>,
    },
    /// Boolean conditional branch: if `condition` is true, jump to `if_true`, else `if_false`.
    Branch {
        condition: AmirOperand,
        if_true: BlockId,
        true_args: Vec<AmirOperand>,
        if_false: BlockId,
        false_args: Vec<AmirOperand>,
    },
    /// Integer discriminant switch (e.g. enum tags, `switch` on int).
    SwitchInt {
        discriminant: AmirOperand,
        targets: Vec<(i128, BlockId, Vec<AmirOperand>)>,
        otherwise: (BlockId, Vec<AmirOperand>),
    },
    /// A3.1: suspension frontier at `await` inside an async function body.
    ///
    /// Ends the current basic block; control resumes at `resume` after the
    /// awaited coroutine yields a value. Live locals that must survive the
    /// suspension are passed as `args` (same convention as [`Self::Goto`]) — the
    /// coroutine state at this frontier is exactly those args (block params
    /// of `resume`), derived from what the lowerer / liveness marks live.
    ///
    /// Ready-only backends (A3.0) lower this as: drive `future` to completion,
    /// then `jump resume(args)`. Real poll/Pending is A3.1+runtime.
    Suspend {
        /// Coroutine / future being awaited.
        future: AmirOperand,
        /// Block that continues after the await (receives live state as params).
        resume: BlockId,
        /// Live state carried across the suspension (→ resume block params).
        args: Vec<AmirOperand>,
    },
    Unreachable,
}
