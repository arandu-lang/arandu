//! Body typing of one item in the coordinates of its top-level item
//! (RFC 0042, step B). File views place it back through [`ItemPlacement`].

use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::{Diagnostic, NodeKey, Span, SymbolId};
use arandu_resolve::ResolvedNames;
use arandu_semantics::{TypeCheckResult, TypeInfo};
use arandu_typeck::type_checker::info::ItemPlacement;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// What the body typing of one item adds to its file's declaration
/// signatures. Expression tables index the arena of its top-level item, and
/// spans and [`NodeKey`]s are relative to that item's start. Entries outside
/// the declaration, which only repeat the signatures, are left out. Symbol
/// positions belong to the file and are not part of it.
#[derive(Clone)]
pub struct LocalTyping {
    pub resolved: ResolvedNames,
    pub type_info: TypeInfo,
    pub diagnostics: Vec<Diagnostic>,
}

/// [`crate::ctfe::item_staged_typing`] of `item_sym` in local coordinates, with
/// where it sits in the file. `None` when the file AST is not composed from
/// its items, or when the typing holds a position that cannot be made local:
/// a file position outside the item that the signatures do not hold, or a
/// thread capture, whose synthetic names derive from file positions.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_local_typing", file = ?file.file_id(db), item = ?item_sym,
))]
pub fn item_local_typing(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> Option<(HashEq<LocalTyping>, ItemPlacement)> {
    let (program, bases) = super::items::composed_file(db, file).as_ref()?;
    let Ok(program) = &**program else {
        return None;
    };
    let id = *super::items::item_ast_ids(db, file).get(&item_sym)?;
    let bases = bases.get(id.index as usize)?;
    let own = *super::typing::item_source_spans(db, file).get(&item_sym)?;
    let item_exprs = match &*super::items::item_ast(db, file, id).value {
        Ok(item) => item.pool.exprs.len(),
        Err(_) => return None,
    };
    let at = ItemPlacement {
        file_id: *file.file_id(db),
        exprs: bases.exprs,
        offset: bases.offset,
    };
    let signatures = super::signatures::declaration_signatures(db, file);
    let typed = crate::ctfe::item_staged_typing(db, file, item_sym);
    let frame = Frame {
        at,
        own,
        exprs: at.exprs as usize..at.exprs as usize + item_exprs,
        expr_spans: &program.pool.expr_spans,
    };
    let local = frame.localize(typed, signatures)?;
    let hash = crate::stable_hash::local_typing_hash(&local, &signatures.symbols);
    Some((HashEq::with_identity(Arc::new(local), hash), at))
}

/// Merge a local typing into a file view at `at`: the inverse of
/// [`item_local_typing`] for everything a file view takes from an item.
pub(super) fn place(
    info: &mut TypeInfo,
    names: &mut ResolvedNames,
    diagnostics: &mut Vec<Diagnostic>,
    local: &LocalTyping,
    at: ItemPlacement,
) {
    info.merge_item_from(&local.type_info, at);
    let key = |key: &NodeKey| NodeKey {
        start: key.start + at.offset,
        end: key.end + at.offset,
    };
    let resolved = &local.resolved;
    names
        .value_refs
        .extend(resolved.value_refs.iter().map(|(k, &v)| (key(k), v)));
    names
        .type_refs
        .extend(resolved.type_refs.iter().map(|(k, &v)| (key(k), v)));
    for (index, slot) in resolved.expr_symbols.iter().enumerate() {
        let Some(value) = *slot else { continue };
        let index = at.exprs as usize + index;
        if names.expr_symbols.len() <= index {
            names.expr_symbols.resize(index + 1, None);
        }
        names.expr_symbols[index] = Some(value);
    }
    names.typed_comptime_arguments.extend(
        resolved
            .typed_comptime_arguments
            .iter()
            .map(|(k, v)| (key(k), v.clone())),
    );
    names.comptime_arguments.extend(
        resolved
            .comptime_arguments
            .iter()
            .map(|(k, &v)| (key(k), v)),
    );
    diagnostics.extend(local.diagnostics.iter().map(|diagnostic| {
        let mut diagnostic = diagnostic.clone();
        for_each_span(&mut diagnostic, |span| *span = at.span(*span));
        diagnostic
    }));
}

/// The item a typing is made local to, in file coordinates.
struct Frame<'a> {
    at: ItemPlacement,
    /// Declaration span; only what lies inside it belongs to the item.
    own: Span,
    /// The item's slice of the file expression arena.
    exprs: std::ops::Range<usize>,
    expr_spans: &'a [Span],
}

impl Frame<'_> {
    fn inside(&self, start: u32, end: u32) -> bool {
        self.own.start <= start && end <= self.own.end
    }

    fn key(&self, key: NodeKey) -> Option<NodeKey> {
        self.inside(key.start, key.end).then(|| NodeKey {
            start: key.start - self.at.offset,
            end: key.end - self.at.offset,
        })
    }

    fn span(&self, span: Span) -> Option<Span> {
        (span.file_id == self.at.file_id && self.inside(span.start, span.end)).then(|| {
            Span::new(
                span.file_id,
                span.start - self.at.offset,
                span.end - self.at.offset,
            )
        })
    }

    /// Local index of a file expression that belongs to the item.
    fn expr(&self, index: usize) -> Option<usize> {
        let span = self.expr_spans.get(index)?;
        (self.exprs.contains(&index) && self.inside(span.start, span.end))
            .then(|| index - self.exprs.start)
    }

    fn keys<V: Clone + PartialEq>(
        &self,
        map: &FxHashMap<NodeKey, V>,
        signatures: &FxHashMap<NodeKey, V>,
    ) -> Option<FxHashMap<NodeKey, V>> {
        let mut local = FxHashMap::default();
        for (&key, value) in map {
            match self.key(key) {
                Some(key) => {
                    local.insert(key, value.clone());
                }
                None if signatures.get(&key) == Some(value) => {}
                None => return None,
            }
        }
        Some(local)
    }

    fn key_set(
        &self,
        set: &FxHashSet<NodeKey>,
        signatures: &FxHashSet<NodeKey>,
    ) -> Option<FxHashSet<NodeKey>> {
        let mut local = FxHashSet::default();
        for &key in set {
            match self.key(key) {
                Some(key) => {
                    local.insert(key);
                }
                None if signatures.contains(&key) => {}
                None => return None,
            }
        }
        Some(local)
    }

    fn spans<V: Clone + PartialEq>(
        &self,
        map: &FxHashMap<Span, V>,
        signatures: &FxHashMap<Span, V>,
    ) -> Option<FxHashMap<Span, V>> {
        let mut local = FxHashMap::default();
        for (&span, value) in map {
            match self.span(span) {
                Some(span) => {
                    local.insert(span, value.clone());
                }
                None if signatures.get(&span) == Some(value) => {}
                None => return None,
            }
        }
        Some(local)
    }

    /// The item's slice of a table indexed by the file expression arena,
    /// without trailing empty slots.
    fn slice<T: Copy + PartialEq>(
        &self,
        table: &[Option<T>],
        signatures: &[Option<T>],
    ) -> Option<Vec<Option<T>>> {
        let mut local = Vec::new();
        for (index, slot) in table.iter().enumerate() {
            let Some(value) = slot else { continue };
            match self.expr(index) {
                Some(local_index) => {
                    if local.len() <= local_index {
                        local.resize(local_index + 1, None);
                    }
                    local[local_index] = Some(*value);
                }
                None if signatures.get(index).copied().flatten() == Some(*value) => {}
                None => return None,
            }
        }
        Some(local)
    }

    fn localize(
        &self,
        typed: &TypeCheckResult,
        signatures: &TypeCheckResult,
    ) -> Option<LocalTyping> {
        Some(LocalTyping {
            resolved: self.resolved(&typed.resolved, &signatures.resolved)?,
            type_info: self.type_info(&typed.type_info, &signatures.type_info)?,
            diagnostics: typed
                .diagnostics
                .iter()
                .map(|diagnostic| self.diagnostic(diagnostic))
                .collect::<Option<_>>()?,
        })
    }

    fn resolved(&self, typed: &ResolvedNames, signatures: &ResolvedNames) -> Option<ResolvedNames> {
        let ResolvedNames {
            deferred_comptime_functions,
            deferred_loop_bodies,
            comptime_branches,
            comptime_loops,
            comptime_arguments,
            typed_comptime_arguments,
            definitions,
            expr_symbols,
            value_refs,
            type_refs,
            mutable_symbols,
        } = typed;
        Some(ResolvedNames {
            deferred_comptime_functions: deferred_comptime_functions.clone(),
            deferred_loop_bodies: self
                .key_set(deferred_loop_bodies, &signatures.deferred_loop_bodies)?,
            comptime_branches: self.keys(comptime_branches, &signatures.comptime_branches)?,
            comptime_loops: self.keys(comptime_loops, &signatures.comptime_loops)?,
            comptime_arguments: self.keys(comptime_arguments, &signatures.comptime_arguments)?,
            typed_comptime_arguments: self.keys(
                typed_comptime_arguments,
                &signatures.typed_comptime_arguments,
            )?,
            definitions: self.keys(definitions, &signatures.definitions)?,
            expr_symbols: self.slice(expr_symbols, &signatures.expr_symbols)?,
            value_refs: self.keys(value_refs, &signatures.value_refs)?,
            type_refs: self.keys(type_refs, &signatures.type_refs)?,
            mutable_symbols: mutable_symbols.clone(),
        })
    }

    fn type_info(&self, typed: &TypeInfo, signatures: &TypeInfo) -> Option<TypeInfo> {
        // Every field is named so that a new positional table cannot be
        // copied in file coordinates unnoticed. The rest are keyed by symbol
        // or by type and carry no position.
        let TypeInfo {
            target_identity: _,
            target_pointer_width: _,
            target_layout: _,
            type_interner: _,
            expr_types,
            ctfe_roots,
            ctfe_values,
            ctfe_global_values: _,
            decl_types: _,
            deferred_headers: _,
            concrete_headers: _,
            header_requests,
            return_borrow_summaries: _,
            struct_fields: _,
            private_fields: _,
            interior_mutable_types: _,
            enum_variants: _,
            enum_variant_tags: _,
            destructors: _,
            destructor_instances: _,
            generic_params: _,
            generic_defaults: _,
            param_constraints: _,
            interfaces: _,
            variant_instantiations: _,
            function_effects: _,
            unsafe_functions: _,
            struct_repr_c: _,
            struct_field_defaults,
            thread_captures,
        } = typed;
        if !thread_captures.is_empty() || *struct_field_defaults != signatures.struct_field_defaults
        {
            return None;
        }
        let mut local = typed.clone();
        local.expr_types = self.slice(expr_types, &signatures.expr_types)?;
        local.ctfe_roots = Vec::new();
        for root in ctfe_roots {
            match self.expr(root.as_usize()) {
                Some(index) => local
                    .ctfe_roots
                    .push(arandu_parser::ast_pool::ExprId::new(index)),
                None if signatures.ctfe_roots.contains(root) => {}
                None => return None,
            }
        }
        local.ctfe_values = self.spans(ctfe_values, &signatures.ctfe_values)?;
        local.header_requests = FxHashMap::default();
        for (key, &span) in header_requests {
            match self.span(span) {
                Some(span) => {
                    local.header_requests.insert(key.clone(), span);
                }
                None if signatures.header_requests.get(key) == Some(&span) => {}
                None => return None,
            }
        }
        local.struct_field_defaults = FxHashMap::default();
        without_signatures(&mut local, signatures);
        Some(local)
    }

    /// Every position of the diagnostic in this file must lie in the item.
    fn diagnostic(&self, diagnostic: &Diagnostic) -> Option<Diagnostic> {
        let mut local = diagnostic.clone();
        let mut ok = true;
        for_each_span(&mut local, |span| {
            if span.file_id != self.at.file_id || *span == Span::new(0, 0, 0) {
                return;
            }
            match self.span(*span) {
                Some(relative) => *span = relative,
                None => ok = false,
            }
        });
        ok.then_some(local)
    }
}

/// Drops the entries the item typing copied from the signatures; merging them
/// onto a file view that starts from the signatures changes nothing. Types
/// are compared by `TypeId` only when the item interner extends theirs.
fn without_signatures(local: &mut TypeInfo, signatures: &TypeInfo) {
    fn same_map<K: Eq + std::hash::Hash, V: PartialEq>(
        map: &mut FxHashMap<K, V>,
        base: &FxHashMap<K, V>,
    ) {
        map.retain(|key, value| base.get(key) != Some(value));
    }
    fn same_arc<K: Eq + std::hash::Hash, V>(
        map: &mut FxHashMap<K, Arc<V>>,
        base: &FxHashMap<K, Arc<V>>,
    ) {
        map.retain(|key, value| !base.get(key).is_some_and(|base| Arc::ptr_eq(value, base)));
    }
    fn same_set<K: Eq + std::hash::Hash>(set: &mut FxHashSet<K>, base: &FxHashSet<K>) {
        set.retain(|key| !base.contains(key));
    }
    if local.type_interner.extends(&signatures.type_interner) {
        same_map(&mut local.decl_types, &signatures.decl_types);
        same_map(&mut local.generic_defaults, &signatures.generic_defaults);
    }
    same_map(
        &mut local.ctfe_global_values,
        &signatures.ctfe_global_values,
    );
    same_map(
        &mut local.return_borrow_summaries,
        &signatures.return_borrow_summaries,
    );
    same_map(&mut local.enum_variant_tags, &signatures.enum_variant_tags);
    same_map(&mut local.destructors, &signatures.destructors);
    same_map(&mut local.function_effects, &signatures.function_effects);
    same_arc(&mut local.concrete_headers, &signatures.concrete_headers);
    same_arc(&mut local.struct_fields, &signatures.struct_fields);
    same_arc(&mut local.generic_params, &signatures.generic_params);
    same_arc(&mut local.param_constraints, &signatures.param_constraints);
    same_set(&mut local.deferred_headers, &signatures.deferred_headers);
    same_set(&mut local.private_fields, &signatures.private_fields);
    same_set(
        &mut local.interior_mutable_types,
        &signatures.interior_mutable_types,
    );
    same_set(&mut local.unsafe_functions, &signatures.unsafe_functions);
    same_set(&mut local.struct_repr_c, &signatures.struct_repr_c);
}

/// Every position a diagnostic carries.
pub(crate) fn for_each_span(diagnostic: &mut Diagnostic, mut f: impl FnMut(&mut Span)) {
    let Diagnostic {
        code: _,
        severity: _,
        kind: _,
        message: _,
        span,
        primary_label: _,
        labels,
        notes: _,
        hints,
    } = diagnostic;
    f(span);
    for label in labels {
        f(&mut label.span);
    }
    for hint in hints {
        if let Some(replacement) = &mut hint.replacement {
            f(&mut replacement.span);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::typing::compose_items;
    use crate::{DatabaseImpl, StableHash};
    use std::path::{Path, PathBuf};

    fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "aru") {
                files.push(path);
            }
        }
    }

    fn normalized(result: &arandu_semantics::TypeCheckResult) -> blake3::Hash {
        let mut result = result.clone();
        let types = &mut result.type_info_mut().expr_types;
        while types.last() == Some(&None) {
            types.pop();
        }
        result.stable_hash()
    }

    /// File views placed from local typings equal the views merged in file
    /// coordinates, except for empty slots past the last typed expression.
    #[test]
    fn local_placement_rebuilds_the_file_views() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for dir in ["examples", "stdlib", "tests"] {
            collect(&root.join(dir), &mut files);
        }
        files.sort();
        let (mut items, mut local) = (0, 0);
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut db = DatabaseImpl::new();
            let file = db.new_file(path.display().to_string(), text);
            let parsed = super::super::parse(&db, file);
            let Ok(program) = &**parsed else { continue };
            let signatures = super::super::declaration_signatures(&db, file);
            for symbol in arandu_semantics::body_item_symbols(program, &signatures.resolved) {
                items += 1;
                local += usize::from(super::item_local_typing(&db, file, symbol).is_some());
            }
            for declarations_only in [true, false] {
                let placed = compose_items(&db, file, declarations_only, true);
                let merged = compose_items(&db, file, declarations_only, false);
                assert_eq!(
                    normalized(&placed),
                    normalized(&merged),
                    "{} (declarations only: {declarations_only})",
                    path.display()
                );
            }
        }
        assert!(
            items > 500 && local * 100 >= items * 95,
            "{local} of {items} items typed locally"
        );
    }
}
