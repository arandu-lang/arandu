use arandu_parser::{FuncName, Program, TopLevelDecl};
use smol_str::SmolStr;

use crate::{ResolutionResult, ResolvedNames, SymbolKind, SymbolTable};

mod collect;
mod decls;
mod expr;
mod program;
mod staging;
mod stmt;
mod symbols;
mod types;
mod util;

mod bodies;
mod headers;
mod import_merge;

pub use bodies::{BodySelection, resolve_bodies_with_poll, resolve_selected_body_with_poll};
use headers::lookup_type_name;
pub use headers::resolve_headers_with_poll;

/// Builtin prelude modules injected by `define_prelude` / this helper.
/// Kept in one place so Salsa import resolution can short-circuit without
/// requiring on-disk `io.aru` / `err.aru` files.
pub const PRELUDE_MODULES: &[&str] = &["io", "err"];

/// Members registered for each prelude module (must stay in sync with
/// [`super::program::Resolver::define_prelude`]).
const PRELUDE_MODULE_MEMBERS: &[(&str, &[&str])] = &[
    ("io", &["println", "print", "create", "remove", "eprint"]),
    ("err", &["new"]),
];

/// Returns the prelude module name if `path` is a single-segment prelude path.
#[must_use]
pub fn prelude_module_from_path(path: &[SmolStr]) -> Option<&'static str> {
    if path.len() != 1 {
        return None;
    }
    let name = path[0].as_str();
    PRELUDE_MODULES.iter().copied().find(|&m| m == name)
}

#[must_use]
pub fn resolve_local(file_id: u32, program: &Program) -> ResolutionResult {
    Resolver::new(file_id, &program.pool, Some(program)).resolve_local(program)
}

#[must_use]
pub fn resolve_local_with_poll(
    file_id: u32,
    program: &Program,
    poll: impl FnMut(),
) -> ResolutionResult {
    Resolver::new(file_id, &program.pool, Some(program)).resolve_local_with_poll(program, poll)
}

/// Single-file / unit-test resolve that runs the **same** import pipeline as
/// production, with an empty module loader (no multi-file loads).
///
/// Prefer this over hand-rolled import collection so prelude short-circuit and
/// `canonicalize_import_path` stay shared with the CLI (RC-DUAL-RESOLVE).
#[must_use]
pub fn resolve_for_test(file_id: u32, program: &Program) -> ResolutionResult {
    let local = resolve_local(file_id, program);
    resolve_imports_and_bodies(&crate::EmptyModuleLoader, program, local)
}

#[must_use]
pub fn resolve_imports_and_bodies(
    db: &dyn crate::ModuleLoader,
    program: &Program,
    result: ResolutionResult,
) -> ResolutionResult {
    resolve_imports_and_bodies_with_poll(db, program, result, || {})
}

#[must_use]
pub fn resolve_imports_and_bodies_with_poll(
    db: &dyn crate::ModuleLoader,
    program: &Program,
    result: ResolutionResult,
    mut poll: impl FnMut(),
) -> ResolutionResult {
    let headers = resolve_headers_with_poll(db, program, result, &mut poll);
    resolve_bodies_with_poll(program, headers, poll)
}

/// Declaration/import state that can be consumed without resolving function
/// bodies. Continuation maps refer only to this AST revision and are retained
/// so finishing resolution does not reallocate headers or redo imports.
#[derive(Debug, Clone)]
pub struct HeaderResolution {
    pub declarations: ResolutionResult,
    pub body_scopes: rustc_hash::FxHashMap<crate::NodeKey, crate::ScopeId>,
    pub import_aliases: rustc_hash::FxHashMap<SmolStr, SmolStr>,
    pub current_module: Option<String>,
    pub imported_symbols: rustc_hash::FxHashMap<crate::SymbolId, (SmolStr, arandu_lexer::Span)>,
    pub used_symbols: rustc_hash::FxHashSet<crate::SymbolId>,
}

#[must_use]
#[tracing::instrument(level = "trace", target = "arandu_resolve", skip(program))]
pub fn collect_symbols(
    program: &Program,
) -> (
    SymbolTable,
    ResolvedNames,
    crate::DocCommentMap,
    Vec<crate::Diagnostic>,
) {
    let mut resolver = Resolver {
        reusable_definitions: rustc_hash::FxHashSet::default(),
        symbols: SymbolTable::new(0),
        resolved: ResolvedNames::default(),
        docs: crate::DocCommentMap::default(),
        diagnostics: Vec::new(),
        pool: &program.pool,
        import_aliases: rustc_hash::FxHashMap::default(),
        failed_import_aliases: rustc_hash::FxHashSet::default(),
        current_module: program.module.as_ref().map(|m| m.path.join(".")),
        imported_symbols: rustc_hash::FxHashMap::default(),
        used_symbols: rustc_hash::FxHashSet::default(),
    };

    for doc in &program.docs {
        resolver
            .docs
            .entry(crate::NodeKey::from(doc.target_span))
            .or_default()
            .push(doc.text.to_string());
    }

    let global = resolver.symbols.global_scope();
    if let Some(module) = &program.module
        && let Some(root) = module.path.first()
    {
        resolver.define(global, root, SymbolKind::Module, module.span);
    }

    for import in &program.imports {
        resolver.collect_import(global, import);
    }

    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
        resolver.collect_top_level(global, decl);
    }

    if let Some(module) = &program.module {
        let module_name = module.path.join(".");
        for decl_id in &program.decls {
            let decl = program.pool.decl(*decl_id);
            match decl {
                TopLevelDecl::Const(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::TypeAlias(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Func(d) => {
                    if let FuncName::Free { span, name } = &d.name {
                        let _ = resolver
                            .symbols
                            .define_module_member(&module_name, name, *span);
                    }
                }
                TopLevelDecl::Struct(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Enum(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Interface(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Extern(d) => {
                    for member in &d.members {
                        let _ = resolver.symbols.define_module_member(
                            &module_name,
                            &member.name,
                            member.span,
                        );
                    }
                }
                TopLevelDecl::Submodule(_) => {}
                TopLevelDecl::Error(_) => {}
            }
        }
    }

    (
        resolver.symbols,
        resolver.resolved,
        resolver.docs,
        resolver.diagnostics,
    )
}

#[must_use]
pub fn resolve_with_symbols(
    global_symbols: SymbolTable,
    resolved: ResolvedNames,
    docs: crate::DocCommentMap,
    diagnostics: Vec<crate::Diagnostic>,
    program: &Program,
) -> ResolutionResult {
    let mut resolver = Resolver {
        reusable_definitions: rustc_hash::FxHashSet::default(),
        symbols: global_symbols,
        resolved,
        docs,
        diagnostics,
        pool: &program.pool,
        import_aliases: rustc_hash::FxHashMap::default(),
        failed_import_aliases: rustc_hash::FxHashSet::default(),
        current_module: program.module.as_ref().map(|m| m.path.join(".")),
        imported_symbols: rustc_hash::FxHashMap::default(),
        used_symbols: rustc_hash::FxHashSet::default(),
    };

    for import in &program.imports {
        if let arandu_parser::ImportDecl::ExternalAlias { source, alias, .. } = import {
            resolver
                .import_aliases
                .insert(alias.clone(), source.clone());
        }
    }

    let global = resolver.symbols.global_scope();
    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
        resolver.resolve_top_level(global, decl);
    }

    resolver.check_unused_imports();

    ResolutionResult {
        is_cycle_fallback: false,
        symbols: std::sync::Arc::new(resolver.symbols),
        resolved: std::sync::Arc::new(resolver.resolved),
        docs: resolver.docs,
        diagnostics: resolver.diagnostics,
    }
}

struct Resolver<'a> {
    /// Seed identities may be rebound once by a selected lexical continuation.
    /// Definitions created during this traversal are never eligible for reuse.
    reusable_definitions: rustc_hash::FxHashSet<crate::NodeKey>,
    symbols: SymbolTable,
    resolved: ResolvedNames,
    docs: crate::DocCommentMap,
    diagnostics: Vec<crate::Diagnostic>,
    pool: &'a arandu_parser::ast_pool::AstPool,
    import_aliases: rustc_hash::FxHashMap<SmolStr, SmolStr>,
    failed_import_aliases: rustc_hash::FxHashSet<SmolStr>,
    current_module: Option<String>,
    imported_symbols: rustc_hash::FxHashMap<crate::SymbolId, (SmolStr, arandu_lexer::Span)>,
    used_symbols: rustc_hash::FxHashSet<crate::SymbolId>,
}

impl<'a> Resolver<'a> {
    pub(crate) fn mark_used(&mut self, symbol: crate::SymbolId) {
        self.used_symbols.insert(symbol);
    }

    pub(crate) fn record_expr_ref(
        &mut self,
        expr: arandu_parser::ast_pool::ExprId,
        symbol: crate::SymbolId,
    ) {
        self.resolved.expr_ref(expr, symbol);
        self.mark_used(symbol);
    }

    pub(crate) fn record_value_ref(&mut self, span: arandu_lexer::Span, symbol: crate::SymbolId) {
        self.resolved.value_ref(span, symbol);
        self.mark_used(symbol);
    }

    pub(crate) fn record_type_ref(&mut self, span: arandu_lexer::Span, symbol: crate::SymbolId) {
        self.resolved.type_ref(span, symbol);
        self.mark_used(symbol);
    }

    pub(crate) fn record_import_symbol(
        &mut self,
        symbol: crate::SymbolId,
        name: SmolStr,
        span: arandu_lexer::Span,
    ) {
        self.imported_symbols.insert(symbol, (name, span));
    }

    pub(crate) fn lookup_and_record_module(
        &mut self,
        scope: crate::ScopeId,
        name: &str,
    ) -> Option<crate::SymbolId> {
        let sym = self.symbols.lookup_module(scope, name)?;
        self.mark_used(sym);
        Some(sym)
    }

    pub(crate) fn check_unused_imports(&mut self) {
        for (sym_id, (name, span)) in &self.imported_symbols {
            if !self.used_symbols.contains(sym_id) {
                self.diagnostics.push(crate::Diagnostic::warning(
                    crate::DiagCode::W007UnusedImport,
                    format!("unused import `{name}`"),
                    *span,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests;
