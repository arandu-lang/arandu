//! Shared compiler types and intermediate representations for Arandu.
//!
//! This crate is the central dependency hub. It owns:
//! - **AMIR** (`amir`): the Arandu Mid-level IR (SSA-like, typed basic blocks).
//! - **HIR** (`hir`): the High-level IR produced by the lowering pass.
//! - **Type system** (`types`): [`types::ArType`], [`types::TypeInterner`], and primitives.
//! - **Layout engine** (`layout`): struct/enum memory layout computation.
//! - **Symbol table** (`symbol_table`): scoped identifier registry.
//! - **Diagnostics** (`diagnostics`): re-exported from `arandu_diagnostics`.
//!
//! Incremental state lives in `arandu_query` (Salsa). This crate keeps pure IRs and types.
//!
//! Re-exports from `arandu_base` (bitset, index_vec, span, etc.)
//! are provided so downstream crates need only depend on this crate.

pub mod amir;
pub mod amir_validate;
pub mod cfg;
pub mod db;
pub mod diagnostics;
pub mod docs;
pub mod effects;
pub mod hir;
pub mod ice;
pub mod intrinsics;
pub mod layout;
pub mod literal_pool;
pub mod ops;
pub mod package;
pub mod resolved;
pub mod symbol_table;
pub mod types;

pub use arandu_base::bitset;
pub use arandu_base::index_vec;
pub use arandu_base::span::Span;
pub use smol_str::SmolStr;

/// G2 / F2.3.3 global opt-in: promote O004 notes to errors.
pub use arandu_base::NO_GENERATIONAL_FALLBACK;
pub use arandu_base::bitset::{BitMatrix, BitSet};
pub use arandu_base::newtype_index;
pub use layout::{
    DataLayout, DenseRange, EnumPayloadShape, GenPayloadLayout, LayoutEngine, LayoutError,
    LayoutOperation, SizeAlign, StructLayoutProvider, TypeLayout,
};
pub use package::{ModuleId, PackageId, TargetId};

pub use amir_validate::validate_amir_program;
pub use diagnostics::{CodeReplacement, DiagCode, Diagnostic, Hint, Label, Severity};
pub use effects::EffectFlags;
pub use intrinsics::IntrinsicKind;
pub use resolved::{DocCommentMap, NodeKey, ResolvedNames};
use std::sync::Arc;
pub use symbol_table::{ScopeId, Symbol, SymbolId, SymbolKind, SymbolTable};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExportedSymbolTable {
    pub symbols: std::collections::BTreeMap<String, (SymbolId, SymbolKind)>,
}

/// Resolution output shared across queries and type-check fan-out.
///
/// PERF: `symbols`/`resolved` are O(1)-clone [`Arc`] handles so that Salsa
/// memoization, IDE snapshots and per-item type-check share one table instead
/// of deep-cloning on every query call. Mutation goes through
/// `Arc::make_mut` / `Arc::unwrap_or_clone` (copy-on-write, once per shared
/// handle). `diagnostics` stays an owned `Vec` — it is typically empty and
/// pushed to in many sites.
#[derive(Debug, Clone)]
pub struct ResolutionResult {
    pub symbols: Arc<SymbolTable>,
    pub resolved: Arc<ResolvedNames>,
    pub docs: DocCommentMap,
    pub diagnostics: Vec<Diagnostic>,
    pub is_cycle_fallback: bool,
}

impl ResolutionResult {
    #[must_use]
    pub fn cycle_fallback() -> Self {
        Self {
            symbols: Arc::new(SymbolTable::new(0)),
            resolved: Arc::new(ResolvedNames::default()),
            docs: DocCommentMap::default(),
            diagnostics: Vec::new(),
            is_cycle_fallback: true,
        }
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}
