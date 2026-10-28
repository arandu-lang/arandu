//! The program an item is typed and lowered from (RFC 0042): its own AST,
//! followed by the ASTs of the items whose field defaults it may lower, so a
//! struct literal never reads an expression of another item's arena.

use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile, StableHash};
use arandu_base::AstId;
use arandu_middle::SymbolId;
use arandu_parser::ast_pool::ExprId;
use arandu_parser::Program;
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A field default by its struct and field name.
pub(crate) type DefaultKey = (SymbolId, arandu_middle::SmolStr);

/// The field defaults declared by one top-level item, with their
/// expressions local to that item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DefaultItem {
    pub owner: AstId,
    pub defaults: Vec<(DefaultKey, u32)>,
}

/// Every top-level item that declares a field default visible from `file`:
/// this file's structs and those of the files whose structs it sees. `None`
/// when one of those files is not composed from its items.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "visible_defaults", file = ?file.file_id(db),
))]
pub(crate) fn visible_defaults(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> Option<Arc<Vec<DefaultItem>>> {
    let file_id = *file.file_id(db);
    let signatures = super::signatures::declaration_signatures(db, file);
    let mut files = std::collections::BTreeSet::from([file_id]);
    files.extend(
        signatures
            .type_info
            .struct_fields
            .keys()
            .map(|symbol| symbol.file_id),
    );
    let mut items: BTreeMap<AstId, Vec<(DefaultKey, u32)>> = BTreeMap::new();
    for owner_file in files {
        let source = if owner_file == file_id {
            file
        } else {
            match db.source_file_by_id(owner_file) {
                Some(source) => source,
                None => continue,
            }
        };
        let declared = if owner_file == file_id {
            signatures
        } else {
            super::signatures::declaration_signatures(db, source)
        };
        // A default's expression belongs to the arena of its struct's file.
        let mut own = declared
            .type_info
            .struct_field_defaults
            .iter()
            .filter(|((symbol, _), _)| symbol.file_id == owner_file)
            .peekable();
        if own.peek().is_none() {
            continue;
        }
        let (_, bases) = super::items::composed_file(db, source).as_ref()?;
        for (key, expr) in own {
            let index = bases
                .partition_point(|base| base.exprs as usize <= expr.as_usize())
                .checked_sub(1)?;
            let local = expr.as_usize() - bases[index].exprs as usize;
            items
                .entry(AstId::new(owner_file, index as u32))
                .or_default()
                .push((key.clone(), local as u32));
        }
    }
    Some(Arc::new(
        items
            .into_iter()
            .map(|(owner, mut defaults)| {
                defaults.sort_by(|((a, x), _), ((b, y), _)| {
                    (a.file_id, a.local_id.0, x).cmp(&(b.file_id, b.local_id.0, y))
                });
                DefaultItem { owner, defaults }
            })
            .collect(),
    ))
}

/// One appended item of an [`ItemProgram`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalSegment {
    pub owner: AstId,
    pub local_offset: u32,
    pub len: u32,
    pub local_exprs: u32,
    pub exprs_len: u32,
}

/// The AST an item is typed and lowered from, and the field defaults it may
/// read, by their expressions in it.
#[derive(Clone)]
pub(crate) struct ItemProgram {
    pub program: Arc<Program>,
    pub segments: Vec<LocalSegment>,
    pub defaults: FxHashMap<DefaultKey, ExprId>,
}

/// [`item_program`] when `item_sym` builds a struct literal, the only case in
/// which it may read a field default; `Some(None)` when its item AST is its
/// program, so no further query is consulted. `None` when it has no item.
pub(crate) fn defaults_program(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> Option<Option<&HashEq<ItemProgram>>> {
    let id = *super::items::item_ast_ids(db, file).get(&item_sym)?;
    let Ok(program) = &*super::items::item_ast(db, file, id).value else {
        return None;
    };
    if !builds_struct(program) {
        return Some(None);
    }
    Some(Some(item_program(db, file, item_sym).as_ref()?))
}

fn builds_struct(program: &Program) -> bool {
    program
        .pool
        .exprs
        .iter()
        .any(|expr| matches!(expr, arandu_parser::ExprKind::StructLiteral { .. }))
}

/// [`super::item_ast`] of `item_sym`, followed by the items that declare the
/// field defaults visible from its file when it builds a struct literal.
/// Each appended item starts past the end of the previous one, so their spans
/// stay disjoint and none depends on where the items sit.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_program", file = ?file.file_id(db), item = ?item_sym,
))]
pub(crate) fn item_program(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> Option<HashEq<ItemProgram>> {
    let id = *super::items::item_ast_ids(db, file).get(&item_sym)?;
    let item = super::items::item_ast(db, file, id);
    let Ok(program) = &*item.value else {
        return None;
    };
    let mut identity = blake3::Hasher::new();
    identity.update(b"ItemProgram/v1");
    identity.update(item.stable_hash().as_bytes());
    if !builds_struct(program) {
        return Some(HashEq::with_identity(
            Arc::new(ItemProgram {
                program: Arc::clone(program),
                segments: Vec::new(),
                defaults: FxHashMap::default(),
            }),
            identity.finalize(),
        ));
    }
    let visible = visible_defaults(db, file).as_ref()?.clone();
    let mut composed = None::<Program>;
    let mut segments = Vec::new();
    let mut defaults = FxHashMap::default();
    let length = |owner: AstId| {
        let source = db.source_file_by_id(owner.file_id)?;
        let span = super::items::item_positions(db, source).item_span(owner)?;
        Some(span.end - span.start)
    };
    let mut offset = if visible.is_empty() {
        0
    } else {
        length(id)? + 1
    };
    for owner in visible.iter() {
        if owner.owner == id {
            for (key, local) in &owner.defaults {
                defaults.insert(key.clone(), ExprId::new(*local as usize));
            }
            continue;
        }
        let source = db.source_file_by_id(owner.owner.file_id)?;
        let appended = super::items::item_ast(db, source, owner.owner);
        let Ok(appended_program) = &*appended.value else {
            return None;
        };
        let composed = composed.get_or_insert_with(|| (**program).clone());
        let kept = (
            composed.decls.len(),
            composed.imports.len(),
            composed.interface_impls.len(),
            composed.docs.len(),
        );
        let bases = composed.append_item(appended_program, offset);
        // Only the expressions are read; the appended declarations are not
        // items of this program.
        composed.decls.truncate(kept.0);
        composed.imports.truncate(kept.1);
        composed.interface_impls.truncate(kept.2);
        composed.docs.truncate(kept.3);
        let len = length(owner.owner)?;
        segments.push(LocalSegment {
            owner: owner.owner,
            local_offset: offset,
            len,
            local_exprs: bases.exprs,
            exprs_len: appended_program.pool.exprs.len() as u32,
        });
        for (key, local) in &owner.defaults {
            defaults.insert(
                key.clone(),
                ExprId::new(bases.exprs as usize + *local as usize),
            );
        }
        identity.update(&owner.owner.file_id.to_le_bytes());
        identity.update(&owner.owner.index.to_le_bytes());
        identity.update(appended.stable_hash().as_bytes());
        identity.update(&offset.to_le_bytes());
        identity.update(&len.to_le_bytes());
        offset += len + 1;
    }
    let mut sorted: Vec<_> = defaults.iter().collect();
    sorted.sort_by(|((a, x), _), ((b, y), _)| {
        (a.file_id, a.local_id.0, x).cmp(&(b.file_id, b.local_id.0, y))
    });
    identity.update(&(sorted.len() as u64).to_le_bytes());
    for ((symbol, name), expr) in sorted {
        identity.update(&symbol.file_id.to_le_bytes());
        identity.update(&symbol.local_id.0.to_le_bytes());
        identity.update(name.as_bytes());
        identity.update(&[0]);
        identity.update(&(expr.as_usize() as u64).to_le_bytes());
    }
    let program = composed.map_or_else(|| Arc::clone(program), Arc::new);
    Some(HashEq::with_identity(
        Arc::new(ItemProgram {
            program,
            segments,
            defaults,
        }),
        identity.finalize(),
    ))
}
