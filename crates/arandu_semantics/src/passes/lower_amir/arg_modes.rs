//! Call-site argument consume modes (shared/mut borrow vs own move).
//!
//! Built once from HIR after monomorphization — O(1) lookup per call arg,
//! no per-site HIR scan.

use arandu_middle::SymbolId;
use arandu_middle::hir::{HirDecl, HirProgram, ReceiverKind};
use arandu_middle::types::{ArType, TypeInterner};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

/// How a formal parameter is consumed at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgConsumeKind {
    /// Move (or copy if type is copy) — default for free params and `own self`.
    Move,
    /// `shared self` / shared reborrow — do not move the local.
    BorrowShared,
    /// `mut self` / exclusive reborrow — do not move; exclusive for O003.
    BorrowMut,
}

impl ArgConsumeKind {
    #[inline]
    pub fn is_borrow(self) -> bool {
        matches!(self, Self::BorrowShared | Self::BorrowMut)
    }

    /// Exclusive loan (conflicts with any other loan of the same place — O003).
    #[inline]
    pub fn is_exclusive(self) -> bool {
        matches!(self, Self::BorrowMut)
    }
}

/// SymbolId → parallel vector of modes (index = formal param index).
#[derive(Debug, Default, Clone)]
pub struct CalleeArgModes {
    modes: FxHashMap<SymbolId, SmallVec<[ArgConsumeKind; 8]>>,
}

impl CalleeArgModes {
    /// One HIR walk: every `HirDecl::Func` (including mono specializations).
    #[must_use]
    pub fn from_hir(hir: &HirProgram, interner: &TypeInterner) -> Self {
        let mut modes = FxHashMap::default();
        for &decl_id in &hir.decls {
            let HirDecl::Func(f) = hir.pool.decl(decl_id) else {
                continue;
            };
            let params = hir.pool.params_list(f.params);
            let mut vec = SmallVec::with_capacity(params.len());
            for p in params {
                let kind = match p.receiver_kind {
                    Some(ReceiverKind::Own) => ArgConsumeKind::Move,
                    Some(ReceiverKind::Mut) => ArgConsumeKind::BorrowMut,
                    Some(ReceiverKind::Shared) => ArgConsumeKind::BorrowShared,
                    None => match interner.resolve(p.ty) {
                        ArType::Ref(_) => ArgConsumeKind::BorrowShared,
                        ArType::RefMut(_) => ArgConsumeKind::BorrowMut,
                        _ if p.is_receiver => ArgConsumeKind::BorrowShared,
                        _ => ArgConsumeKind::Move,
                    },
                };
                vec.push(kind);
            }
            modes.insert(f.symbol, vec);
        }
        Self { modes }
    }

    #[inline]
    #[must_use]
    pub fn kind(&self, callee: SymbolId, arg_index: usize) -> ArgConsumeKind {
        self.modes
            .get(&callee)
            .and_then(|v| v.get(arg_index).copied())
            .unwrap_or(ArgConsumeKind::Move)
    }
}
