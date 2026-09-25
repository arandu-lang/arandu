//! Mechanical AMIR function construction.
//!
//! [`AmirBuilder`] owns the *structural* side of lowering: block allocation,
//! the dense statement table, block ranges and predecessor bookkeeping.
//! SSA naming state (`current_def`, incomplete phis, sealing) stays on
//! [`super::LowerCtx`] — that is semantic (Braun et al.), not structural.

use super::LowerCtx;
use crate::amir::program::extend_block_range;
use crate::amir::{AmirBasicBlock, AmirStmtTable, AmirTerminator, BlockId, BlockParam};
use crate::layout::DenseRange;
use arandu_lexer::Span;
use rustc_hash::FxHashMap;

pub(super) struct AmirBuilder {
    pub(super) blocks: Vec<AmirBasicBlock>,
    pub(super) stmts: AmirStmtTable,
    pub(super) current_block: Option<BlockId>,
    /// Predecessor lists per block, insertion-ordered (determinism matters).
    pub(super) predecessors: FxHashMap<BlockId, Vec<BlockId>>,
    /// Per-block scratch parameters, index-aligned with `blocks`. SSA inserts
    /// and prunes params non-contiguously across blocks, so they are staged
    /// here and linearized into the dense `block_params` pool only when a
    /// [`crate::amir::AmirFunc`] snapshot is materialized.
    pub(super) params_scratch: Vec<Vec<BlockParam>>,
    /// Source origin for each block, kept out of the hot AMIR block layout.
    pub(super) debug_block_spans: Vec<Span>,
}

impl AmirBuilder {
    pub(super) fn new() -> Self {
        Self {
            blocks: Vec::new(),
            stmts: AmirStmtTable::new(),
            current_block: None,
            predecessors: FxHashMap::default(),
            params_scratch: Vec::new(),
            debug_block_spans: Vec::new(),
        }
    }

    /// Allocates an empty block terminated in [`AmirTerminator::Unreachable`]
    /// (CFG-5 exempt until filled).
    pub(super) fn new_block(&mut self, span: Span) -> BlockId {
        let id = BlockId::from_usize(self.blocks.len());
        self.blocks.push(AmirBasicBlock {
            id,
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Unreachable,
        });
        self.params_scratch.push(Vec::new());
        self.debug_block_spans.push(span);
        id
    }

    pub(super) fn push_block_param(&mut self, block: BlockId, param: BlockParam) {
        self.params_scratch[block.as_usize()].push(param);
    }

    pub(super) fn block_params(&self, block: BlockId) -> &[BlockParam] {
        &self.params_scratch[block.as_usize()]
    }

    pub(super) fn take_block_params(&mut self, block: BlockId) -> Vec<BlockParam> {
        std::mem::take(&mut self.params_scratch[block.as_usize()])
    }

    pub(super) fn set_block_params(&mut self, block: BlockId, params: Vec<BlockParam>) {
        self.params_scratch[block.as_usize()] = params;
    }

    /// Linearizes `params_scratch` into a dense `block_params` pool and stamps
    /// each block's `params` range. Consumes the scratch.
    pub(super) fn materialize_block_params(&mut self) -> Vec<BlockParam> {
        let mut all = Vec::new();
        for i in 0..self.blocks.len() {
            let params = std::mem::take(&mut self.params_scratch[i]);
            let start = all.len();
            let len = params.len();
            self.blocks[i].params = DenseRange::new(start, len);
            all.extend(params);
        }
        all
    }

    /// Refills `params_scratch` from a materialized `block_params` pool (used
    /// to tear an [`crate::amir::AmirFunc`] snapshot back into the builder).
    pub(super) fn restore_block_params_scratch(&mut self, block_params: Vec<BlockParam>) {
        self.params_scratch = self
            .blocks
            .iter()
            .map(|b| block_params[b.params.as_range()].to_vec())
            .collect();
    }

    /// Appends `stmt` to the open block's dense range. Silently dropped when
    /// no block is open (dead control-flow arm) — preserved contract.
    pub(super) fn push_stmt(&mut self, stmt: crate::amir::AmirStmt) {
        if let Some(curr) = self.current_block {
            let id = self.stmts.push(stmt);
            extend_block_range(&mut self.blocks[curr.as_usize()].statements, id);
        }
    }

    /// Overwrites the terminator of `from`, registering every jump target as
    /// a predecessor edge. No-op semantics preserved by the caller, which
    /// only invokes this with an open block.
    pub(super) fn set_terminator(&mut self, from: BlockId, term: AmirTerminator) {
        match &term {
            AmirTerminator::Goto { target, .. } => {
                self.add_predecessor(from, *target);
            }
            AmirTerminator::Branch {
                if_true, if_false, ..
            } => {
                self.add_predecessor(from, *if_true);
                self.add_predecessor(from, *if_false);
            }
            AmirTerminator::SwitchInt {
                targets, otherwise, ..
            } => {
                for (_, dest, _) in targets {
                    self.add_predecessor(from, *dest);
                }
                self.add_predecessor(from, otherwise.0);
            }
            // A3.1: suspend edge → resume block (same pred tracking as Goto).
            AmirTerminator::Suspend { resume, .. } => {
                self.add_predecessor(from, *resume);
            }
            _ => {}
        }
        self.blocks[from.as_usize()].terminator = term;
    }

    pub(super) fn add_predecessor(&mut self, from: BlockId, to: BlockId) {
        self.predecessors.entry(to).or_default().push(from);
    }

    /// True when `block` has at least one registered predecessor.
    pub(super) fn has_predecessor(&self, block: BlockId) -> bool {
        self.predecessors
            .get(&block)
            .is_some_and(|preds| !preds.is_empty())
    }
}

impl LowerCtx<'_> {
    #[inline]
    pub(crate) fn new_block(&mut self) -> BlockId {
        let span = if Self::span_is_usable(self.current_span) {
            self.current_span
        } else {
            arandu_lexer::Span::new(0, 0, 0)
        };
        self.builder.new_block(span)
    }

    #[inline]
    pub(crate) fn new_block_at(&mut self, span: arandu_lexer::Span) -> BlockId {
        self.builder.new_block(span)
    }

    /// Seal a join/exit block and resume after it **only if** some arm fell
    /// through. With no predecessors the join stays `Unreachable` (CFG-5
    /// exempt) and no block is opened.
    pub(crate) fn finish_join(&mut self, join: BlockId) {
        self.seal_block(join);
        if self.builder.has_predecessor(join) {
            self.builder.current_block = Some(join);
        } else {
            self.builder.current_block = None;
        }
    }

    pub(crate) fn push_stmt(&mut self, stmt: crate::amir::AmirStmt) {
        self.builder.push_stmt(stmt);
    }

    pub(crate) fn set_terminator(&mut self, term: AmirTerminator) {
        if let Some(curr) = self.builder.current_block {
            self.builder.set_terminator(curr, term);
        }
    }
}
