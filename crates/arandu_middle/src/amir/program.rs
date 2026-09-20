use super::block::{AmirBasicBlock, BlockParam};
use super::local::{AmirLocal, AmirReceiver, AmirTemp, LocalId, TempId};
use super::stmt::{AmirStmt, AmirStmtTable, InstrId};
use crate::SymbolId;
use crate::cfg::ControlFlowGraph;
use crate::layout::DenseRange;
use crate::literal_pool::AmirLiteralPool;
use crate::types::TypeId;

#[derive(Debug, Clone)]
pub struct AmirProgram {
    pub funcs: Vec<AmirFunc>,
    pub literal_pool: AmirLiteralPool,
    pub extern_funcs:
        rustc_hash::FxHashMap<crate::SymbolId, (Vec<crate::types::ArType>, crate::types::ArType)>,
    /// Cold source-variable metadata used by native debug-info emission.
    ///
    /// Kept out of [`AmirTemp`] so release codegen and the dense hot temp table
    /// pay no per-value size cost. Entries are deterministic and use typed IDs;
    /// backends that do not emit debug information can ignore the table.
    pub debug_bindings: Vec<AmirDebugBinding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AmirDebugBinding {
    pub function: SymbolId,
    pub temp: TempId,
    pub local: LocalId,
}

#[derive(Debug, Clone)]
pub struct AmirFunc {
    pub symbol: SymbolId,
    /// Interned return type (dense `TypeId`).
    pub return_type: TypeId,
    pub receiver: Option<AmirReceiver>,
    pub params: Vec<TempId>,
    pub locals: Vec<AmirLocal>,
    pub temps: Vec<AmirTemp>,
    pub blocks: Vec<AmirBasicBlock>,
    /// Dense pool of block parameters. Each block's `params` range indexes
    /// into this pool; the pool is dense per `AmirFunc` (contiguity asserted
    /// wherever a block's range is re-materialized).
    pub block_params: Vec<BlockParam>,
    pub stmts: AmirStmtTable,
    pub cfg: ControlFlowGraph,
}

impl AmirFunc {
    #[must_use]
    pub fn block(&self, block: super::block::BlockId) -> &AmirBasicBlock {
        &self.blocks[block.as_usize()]
    }

    pub fn block_mut(&mut self, block: super::block::BlockId) -> &mut AmirBasicBlock {
        &mut self.blocks[block.as_usize()]
    }

    /// Block parameters of the block whose `params: DenseRange` is `range`,
    /// sliced out of the dense `block_params` pool.
    #[must_use]
    pub fn block_params(&self, range: DenseRange) -> &[BlockParam] {
        &self.block_params[range.as_range()]
    }

    pub fn block_params_mut(&mut self, range: DenseRange) -> &mut [BlockParam] {
        &mut self.block_params[range.as_range()]
    }

    #[must_use]
    pub fn try_stmt(&self, id: InstrId) -> Option<&AmirStmt> {
        self.stmts.get(id)
    }

    #[must_use]
    pub fn stmt(&self, id: InstrId) -> &AmirStmt {
        match self.stmts.get(id) {
            Some(s) => s,
            None => crate::ice::invalid_dense_id("AmirInstrId", id.as_usize()),
        }
    }

    pub fn stmt_mut(&mut self, id: InstrId) -> &mut AmirStmt {
        match self.stmts.get_mut(id) {
            Some(s) => s,
            None => crate::ice::invalid_dense_id("AmirInstrId", id.as_usize()),
        }
    }

    pub fn block_stmt_ids(
        &self,
        block: super::block::BlockId,
    ) -> impl Iterator<Item = InstrId> + '_ {
        self.block(block).statements.iter_ids::<InstrId>()
    }

    pub fn block_stmts(
        &self,
        block: super::block::BlockId,
    ) -> impl Iterator<Item = &AmirStmt> + '_ {
        self.block_stmt_ids(block).map(|id| self.stmt(id))
    }

    #[must_use]
    pub fn successors(&self, block: super::block::BlockId) -> &[super::block::BlockId] {
        &self.cfg.successors[block.as_usize()]
    }

    #[must_use]
    pub fn predecessors(&self, block: super::block::BlockId) -> &[super::block::BlockId] {
        &self.cfg.predecessors[block.as_usize()]
    }

    pub fn append_stmt_to_block(
        &mut self,
        block: super::block::BlockId,
        stmt: AmirStmt,
    ) -> InstrId {
        let id = self.stmts.push(stmt);
        extend_block_range(&mut self.blocks[block.as_usize()].statements, id);
        id
    }
}

pub fn extend_block_range(range: &mut DenseRange, id: InstrId) {
    let idx = id.as_usize();
    if range.is_empty() {
        *range = DenseRange::new(idx, 1);
        return;
    }
    debug_assert_eq!(
        range.end_usize(),
        idx,
        "AMIR block statements must be appended contiguously"
    );
    range.len += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::{AmirBasicBlock, AmirConstant, AmirOperand, AmirTerminator};
    use crate::types::ArType;

    fn block(id: usize) -> AmirBasicBlock {
        AmirBasicBlock {
            id: super::super::block::BlockId::from_usize(id),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }
    }

    fn func() -> AmirFunc {
        let interner = crate::types::TypeInterner::new();
        AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: interner.intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks: vec![block(0), block(1)],
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg: ControlFlowGraph::default(),
        }
    }

    #[test]
    fn appending_statements_allocates_dense_ids_and_block_ranges() {
        let mut func = func();
        let first = func.append_stmt_to_block(
            super::super::block::BlockId::from_usize(0),
            AmirStmt::Free(AmirOperand::Constant(AmirConstant::Bool(true))),
        );
        let second = func.append_stmt_to_block(
            super::super::block::BlockId::from_usize(0),
            AmirStmt::StorageLive(super::super::local::LocalId::from_usize(0)),
        );
        let third = func.append_stmt_to_block(
            super::super::block::BlockId::from_usize(1),
            AmirStmt::StorageDead(super::super::local::LocalId::from_usize(0)),
        );

        assert_eq!(first, InstrId::from_usize(0));
        assert_eq!(second, InstrId::from_usize(1));
        assert_eq!(third, InstrId::from_usize(2));
        assert_eq!(
            func.block(super::super::block::BlockId::from_usize(0))
                .statements,
            DenseRange::new(0, 2)
        );
        assert_eq!(
            func.block(super::super::block::BlockId::from_usize(1))
                .statements,
            DenseRange::new(2, 1)
        );
        assert_eq!(
            func.block_stmt_ids(super::super::block::BlockId::from_usize(0))
                .collect::<Vec<_>>(),
            vec![InstrId::from_usize(0), InstrId::from_usize(1)]
        );
        assert_eq!(
            func.stmts.kinds.as_slice(),
            &[
                super::super::stmt::AmirStmtKind::Free,
                super::super::stmt::AmirStmtKind::StorageLive,
                super::super::stmt::AmirStmtKind::StorageDead,
            ]
        );
    }
}
