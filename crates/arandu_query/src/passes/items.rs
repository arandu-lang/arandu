//! Position-free item identity (RFC 0042): [`AstId`] names a top-level item by
//! its ordinal, and [`AstIdMap`] is the only place that knows where each item
//! currently sits in its file.

use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_base::{AstId, Span};
use arandu_middle::SymbolId;
use arandu_parser::Program;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Absolute byte extent of every top-level CST item of one file (the item
/// with the doc comments that precede it), in source order. Changes whenever any item moves; consumers that only need item
/// identity depend on [`item_ast_ids`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstIdMap {
    file_id: u32,
    items: Vec<Span>,
}

impl AstIdMap {
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = AstId> + '_ {
        (0..self.items.len() as u32).map(|index| AstId::new(self.file_id, index))
    }

    /// Absolute span of the item, including its attributes and doc comments.
    #[must_use]
    pub fn item_span(&self, id: AstId) -> Option<Span> {
        if id.file_id != self.file_id {
            return None;
        }
        self.items.get(id.index as usize).copied()
    }

    /// Converts a span relative to the start of `id` into an absolute span.
    #[must_use]
    pub fn to_absolute(&self, id: AstId, relative: Span) -> Option<Span> {
        let item = self.item_span(id)?;
        Some(Span::new(
            self.file_id,
            item.start + relative.start,
            item.start + relative.end,
        ))
    }

    /// Top-level item that contains `span`, if any.
    #[must_use]
    pub fn ast_id_containing(&self, span: Span) -> Option<AstId> {
        if span.file_id != self.file_id {
            return None;
        }
        let index = self
            .items
            .iter()
            .position(|item| item.start <= span.start && span.end <= item.end)?;
        Some(AstId::new(self.file_id, index as u32))
    }
}

#[salsa::tracked]
#[tracing::instrument(level = "debug", skip(db), fields(
    file = ?file.file_id(db),
))]
pub fn item_positions(db: &dyn ArandCompilerDb, file: SourceFile) -> Arc<AstIdMap> {
    let file_id = *file.file_id(db);
    let tree = super::resolution::syntax_tree(db, file);
    let items = tree
        .item_extents()
        .into_iter()
        .map(|(start, end)| Span::new(file_id, start, end))
        .collect();
    Arc::new(AstIdMap { file_id, items })
}

/// The top-level item that defines each symbol of `file`. Members of `impl`
/// blocks, `extern` blocks and submodules map to their enclosing item.
#[salsa::tracked]
#[tracing::instrument(level = "debug", skip(db), fields(
    file = ?file.file_id(db),
))]
pub fn item_ast_ids(db: &dyn ArandCompilerDb, file: SourceFile) -> Arc<BTreeMap<SymbolId, AstId>> {
    use arandu_parser::TopLevelDecl;

    let program_res = super::resolution::parse(db, file);
    let Ok(program) = &**program_res else {
        return Arc::default();
    };
    let headers = super::resolution::resolved_headers(db, file);
    let definitions = &headers.declarations.resolved.definitions;
    let positions = item_positions(db, file);

    let mut ids = BTreeMap::new();
    program.for_each_decl_recursive(|_decl_id, decl| {
        let Some(id) = positions.ast_id_containing(arandu_semantics::item_source_span(decl)) else {
            return;
        };
        if let Some(symbol) =
            arandu_semantics::primary_def_key(decl).and_then(|key| definitions.get(&key))
        {
            ids.entry(*symbol).or_insert(id);
        }
        if let TopLevelDecl::Extern(ext) = decl {
            for member in &ext.members {
                if let Some(symbol) = definitions.get(&arandu_middle::NodeKey::from(member.span)) {
                    ids.entry(*symbol).or_insert(id);
                }
            }
        }
    });
    Arc::new(ids)
}

/// AST of one top-level item, lowered from its CST subtree without re-lexing,
/// with every position relative to the item start. Its identity is its
/// content, so moving the item does not change it. Parse diagnostics are
/// accumulated by `parse`, never here.
#[salsa::tracked]
#[tracing::instrument(level = "debug", skip(db), fields(
    file = ?file.file_id(db),
))]
pub fn item_ast(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    id: AstId,
) -> HashEq<Result<Arc<Program>, arandu_parser::ParseError>> {
    let tree = super::resolution::syntax_tree(db, file);
    let Some(item) = (id.file_id == *file.file_id(db))
        .then(|| tree.item_tree(id.index as usize))
        .flatten()
    else {
        return HashEq::new(Ok(Arc::new(super::typing::empty_program())));
    };
    let output = arandu_parser::syntax::lower_syntax_to_program_recovering(&item, id.file_id);
    match output.diagnostics.into_iter().next() {
        Some(error) => HashEq::new(Err(error)),
        None => HashEq::new(Ok(Arc::new(output.program))),
    }
}

/// The file AST as the concatenation of its item ASTs, so every item keeps a
/// fixed correspondence with its slice of the file arena (RFC 0042). `None`
/// when an item does not lower cleanly or text lies outside every item; the
/// whole-file walk then reports the diagnostics.
pub(super) fn composed_program(db: &dyn ArandCompilerDb, file: SourceFile) -> Option<Program> {
    let file_id = *file.file_id(db);
    let items = item_positions(db, file)
        .ids()
        .map(|id| match &*item_ast(db, file, id).value {
            Ok(program) => Some(Arc::clone(program)),
            Err(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let items: Vec<&Program> = items.iter().map(|program| &**program).collect();
    let tree = super::resolution::syntax_tree(db, file);
    tree.compose_program(file_id, &items)
        .map(|(program, _)| program)
}
