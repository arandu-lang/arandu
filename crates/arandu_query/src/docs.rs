//! Incremental documentation extraction and contracts query.

use std::sync::Arc;

use crate::passes::{item_body_typeck, module_signatures, parse, resolve};
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::docs::{
    derive_effect_badges, parse_doc_comment_lines, DocField, DocItem, DocItemKind, DocModule,
    DocVariant, DoctestSnippet,
};
use arandu_middle::{NodeKey, SymbolId, SymbolKind};
use arandu_typeck::EnumPayloadShape;

#[cfg(any(test, debug_assertions))]
pub static ITEM_DOC_EXEC_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn clean_overview_lines(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| arandu_middle::docs::clean_doc_line(l))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Query for documenting a single language item with item-level dependency tracking.
///
/// If a function body changes without modifying its signature, effects, or doc-comment,
/// Salsa applies early-cutoff and avoids invalidating downstream documentation renders.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_doc",
    file = ?file.file_id(db),
    item = ?item_sym,
))]
pub fn item_doc(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> Option<Arc<DocItem>> {
    #[cfg(any(test, debug_assertions))]
    ITEM_DOC_EXEC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let parsed = parse(db, file);
    let Ok(program) = parsed.as_ref() else {
        return None;
    };
    let signatures = module_signatures(db, file);
    let resolution = resolve(db, file);
    let symbol = signatures.symbols.try_get(item_sym)?;

    let kind = match symbol.kind {
        SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc => {
            DocItemKind::Function
        }
        SymbolKind::Struct => DocItemKind::Struct,
        SymbolKind::Enum => DocItemKind::Enum,
        SymbolKind::Interface => DocItemKind::Interface,
        SymbolKind::TypeAlias => DocItemKind::TypeAlias,
        SymbolKind::Const => DocItemKind::Constant,
        _ => return None,
    };

    // Find the declaration node to look up doc comments by both symbol span and decl span
    let mut doc_lines = resolution
        .docs
        .get(&NodeKey::from(symbol.span))
        .cloned()
        .unwrap_or_default();

    if doc_lines.is_empty() {
        for decl_id in &program.decls {
            let decl = program.pool.decl(*decl_id);
            if let Some(key) = arandu_semantics::primary_def_key(decl) {
                if signatures.resolved.definitions.get(&key) == Some(&item_sym) {
                    if let Some(lines) = resolution.docs.get(&NodeKey::from(decl.span())) {
                        doc_lines = lines.clone();
                        break;
                    }
                }
            }
        }
    }

    let (summary, sections) = parse_doc_comment_lines(&doc_lines);

    let signature = if let Some(decl_ty) = signatures.type_info.decl_type(item_sym) {
        decl_ty.display(&signatures.symbols, &signatures.type_info.type_interner)
    } else {
        symbol.name.to_string()
    };

    let mut effects = if matches!(kind, DocItemKind::Function) {
        let body_typeck = item_body_typeck(db, file, item_sym);
        body_typeck
            .type_info
            .function_effects
            .get(&item_sym)
            .copied()
            .or_else(|| {
                signatures
                    .type_info
                    .function_effects
                    .get(&item_sym)
                    .copied()
            })
            .unwrap_or_default()
    } else {
        signatures
            .type_info
            .function_effects
            .get(&item_sym)
            .copied()
            .unwrap_or_default()
    };
    if symbol.name == "sizeOf" || symbol.name == "alignOf" {
        effects = arandu_middle::effects::EffectFlags(
            arandu_middle::effects::EffectFlags::PURE.0
                | arandu_middle::effects::EffectFlags::NO_ALLOC.0,
        );
    }
    let effect_badges = if matches!(kind, DocItemKind::Function) {
        derive_effect_badges(effects)
    } else {
        Vec::new()
    };

    let mut fields = Vec::new();
    if let Some(struct_fields) = signatures.type_info.struct_fields.get(&item_sym) {
        for f in struct_fields.iter() {
            let field_doc = f
                .symbol
                .and_then(|sym| signatures.symbols.try_get(sym))
                .and_then(|sym| resolution.docs.get(&NodeKey::from(sym.span)))
                .and_then(|lines| parse_doc_comment_lines(lines).0);
            let ty_str = signatures
                .type_info
                .type_interner
                .resolve(f.ty)
                .display(&signatures.symbols, &signatures.type_info.type_interner);
            fields.push(DocField {
                name: f.name.to_string(),
                ty: ty_str,
                doc: field_doc,
            });
        }
    }

    let mut variants = Vec::new();
    if matches!(kind, DocItemKind::Enum) {
        let mut variant_symbols: Vec<SymbolId> = signatures
            .type_info
            .enum_variants
            .iter()
            .filter(|(_, (parent, _))| *parent == item_sym)
            .map(|(sym, _)| *sym)
            .collect();
        variant_symbols.sort_by_key(|s| s.local_id.0);

        for var_sym in variant_symbols {
            if let Some(v_sym) = signatures.symbols.try_get(var_sym) {
                let var_name = v_sym
                    .name
                    .rsplit('.')
                    .next()
                    .unwrap_or(&v_sym.name)
                    .to_string();
                let var_doc = resolution
                    .docs
                    .get(&NodeKey::from(v_sym.span))
                    .and_then(|lines| parse_doc_comment_lines(lines).0);

                let payload_str =
                    signatures
                        .type_info
                        .enum_variants
                        .get(&var_sym)
                        .and_then(|(_, shape)| match shape {
                            EnumPayloadShape::Unit => None,
                            EnumPayloadShape::Tuple(tids) => {
                                let parts: Vec<String> = tids
                                    .iter()
                                    .map(|tid| {
                                        signatures.type_info.type_interner.resolve(*tid).display(
                                            &signatures.symbols,
                                            &signatures.type_info.type_interner,
                                        )
                                    })
                                    .collect();
                                Some(format!("({})", parts.join(", ")))
                            }
                        });

                variants.push(DocVariant {
                    name: var_name,
                    payload: payload_str,
                    doc: var_doc,
                });
            }
        }
    }

    let return_borrow = signatures
        .type_info
        .return_borrow_summaries
        .get(&item_sym)
        .and_then(|summary| {
            if summary.dependencies.is_empty() {
                None
            } else {
                let mut params = Vec::new();
                for dep in &summary.dependencies {
                    for src in &dep.sources {
                        params.push(format!("param #{}", src.parameter_index));
                    }
                }
                params.sort();
                params.dedup();
                Some(format!("borrows from {}", params.join(", ")))
            }
        });

    Some(Arc::new(DocItem {
        symbol_id: item_sym,
        name: symbol.name.to_string(),
        kind,
        signature,
        effect_badges,
        summary,
        sections,
        fields,
        variants,
        return_borrow,
    }))
}

/// Query aggregating the documentation model for an entire module file.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "module_doc",
    file = ?file.file_id(db),
))]
pub fn module_doc(db: &dyn ArandCompilerDb, file: SourceFile) -> Arc<DocModule> {
    let parsed = parse(db, file);
    let Ok(program) = parsed.as_ref() else {
        return Arc::new(DocModule {
            name: "unknown".to_string(),
            path: "".to_string(),
            file_id: *file.file_id(db),
            overview: None,
            items: Vec::new(),
        });
    };

    let mod_name = program
        .module
        .as_ref()
        .map(|m| m.path.join("."))
        .unwrap_or_else(|| "main".to_string());

    let file_path = db
        .file_path(*file.file_id(db))
        .to_string_lossy()
        .to_string();

    let resolution = resolve(db, file);
    let signatures = module_signatures(db, file);

    // Overview precedence: docs attached directly to `module X` win.
    let direct_overview = program
        .module
        .as_ref()
        .and_then(|m| resolution.docs.get(&NodeKey::from(m.span)))
        .map(|lines| clean_overview_lines(lines));

    // Fallback: `///` blocks written between `module X` and the first
    // documentable item attach (via pending docs) to `import` statements,
    // which are never rendered as doc items. Surface those orphaned lines
    // as the module overview instead of dropping them silently.
    let overview = direct_overview
        .or_else(|| {
            let global_scope = signatures.symbols.global_scope();
            let first_item_start = signatures
                .symbols
                .iter()
                .filter(|sym| {
                    sym.scope == global_scope
                        && sym.id.file_id == *file.file_id(db)
                        && sym.is_public
                        && matches!(
                            sym.kind,
                            SymbolKind::Func
                                | SymbolKind::AssociatedFunc
                                | SymbolKind::ExternFunc
                                | SymbolKind::Struct
                                | SymbolKind::Enum
                                | SymbolKind::Interface
                                | SymbolKind::TypeAlias
                                | SymbolKind::Const
                        )
                })
                .map(|sym| sym.span.start)
                .min()?;
            program
                .imports
                .iter()
                .filter(|import| import.span().start < first_item_start)
                .filter_map(|import| resolution.docs.get(&NodeKey::from(import.span())))
                .next()
                .map(|lines| clean_overview_lines(lines))
        })
        .filter(|s| !s.is_empty());

    let mut items = Vec::new();
    let global_scope = signatures.symbols.global_scope();

    for sym in signatures.symbols.iter() {
        if sym.scope == global_scope && sym.id.file_id == *file.file_id(db) && sym.is_public {
            if let Some(doc) = item_doc(db, file, sym.id) {
                items.push((*doc.as_ref()).clone());
            }
        }
    }

    items.sort_by(|a, b| {
        let kind_order = |kind: DocItemKind| match kind {
            DocItemKind::Struct => 0,
            DocItemKind::Enum => 1,
            DocItemKind::Interface => 2,
            DocItemKind::TypeAlias => 3,
            DocItemKind::Constant => 4,
            DocItemKind::Function => 5,
        };
        kind_order(a.kind)
            .cmp(&kind_order(b.kind))
            .then_with(|| a.name.cmp(&b.name))
    });

    Arc::new(DocModule {
        name: mod_name,
        path: file_path,
        file_id: *file.file_id(db),
        overview,
        items,
    })
}

/// Discovers all executable doctest snippets from code examples in a source module.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "file_doctests",
    file = ?file.file_id(db),
))]
pub fn file_doctests(db: &dyn ArandCompilerDb, file: SourceFile) -> Arc<Vec<DoctestSnippet>> {
    let mod_doc = module_doc(db, file);
    let mut snippets = Vec::new();
    for item in &mod_doc.items {
        for snippet in &item.sections.examples {
            snippets.push(snippet.clone());
        }
    }
    Arc::new(snippets)
}
