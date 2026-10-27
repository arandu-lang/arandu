use crate::ArandCompilerDb;
use arandu_semantics::TypeCheckResult;
use arandu_typeck::type_checker::TargetInfo;

use std::sync::Arc;

#[derive(Clone)]
pub struct ModuleSignatures {
    value: Arc<TypeCheckResult>,
    hash: blake3::Hash,
}

impl ModuleSignatures {
    fn new(value: TypeCheckResult) -> Self {
        Self::from_arc(Arc::new(value))
    }

    fn from_arc(value: Arc<TypeCheckResult>) -> Self {
        let hash = crate::stable_hash::type_signature_hash(&value);
        Self { value, hash }
    }

    /// The digest Salsa compares this output by, for input fingerprints.
    pub(crate) fn identity(&self) -> blake3::Hash {
        self.hash
    }
}

impl PartialEq for ModuleSignatures {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for ModuleSignatures {}

impl std::ops::Deref for ModuleSignatures {
    type Target = TypeCheckResult;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// Explicit type-check target read from the Salsa [`crate::db::TargetConfig`]
/// input (CLI `--layout=`, default host). Keeps typeck pure: the pointer width
/// never hard-codes a 64-bit assumption inside a query.
pub(crate) fn database_target_info(db: &dyn ArandCompilerDb) -> TargetInfo {
    let layout = db.target_config().data_layout(db);
    TargetInfo {
        // `DataLayout` stores byte widths; `TargetInfo` expects bits.
        pointer_width: (layout.pointer_width() * 8) as u8,
    }
}

mod items;
mod lower;
mod resolution;
mod signatures;
mod symbols;
mod typing;

pub use items::{item_ast_ids, item_positions, AstIdMap};
pub use lower::{
    borrow_interfaces, lower_amir, module_dependency_graph, prepare_hir, BorrowInterfaces,
    LowerAmirArtifacts, PreparedHir,
};
pub(crate) use resolution::signatures_from_program;
pub use resolution::{
    cycle_recover, parse, parse_diagnostics, resolve, resolved_headers, symbol_span, syntax_tree,
};
pub(crate) use signatures::module_import_edges;
pub use signatures::{
    cycle_recover_module_signatures, declaration_signatures, header_signatures, module_signatures,
    seed_header_signatures,
};
pub use symbols::{
    cycle_recover_exported_symbols, cycle_recover_internal_symbols, exported_symbols,
    internal_symbols, local_symbols,
};
pub use typing::{
    file_typeck_view, file_typing, ide_type_check, item_body_typeck, item_source_input,
    item_typing, type_check, FuncBodyInput, ItemSourceInput,
};
