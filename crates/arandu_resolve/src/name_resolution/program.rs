use arandu_lexer::Span;
use arandu_parser::{FuncName, Program, TopLevelDecl};
use smol_str::SmolStr;

use crate::{DocCommentMap, NodeKey, ResolutionResult, ResolvedNames, SymbolKind, SymbolTable};

use super::Resolver;

impl<'a> Resolver<'a> {
    pub(crate) fn new(
        file_id: u32,
        pool: &'a arandu_parser::ast_pool::AstPool,
        program: Option<&Program>,
    ) -> Self {
        let current_module = program.and_then(|p| p.module.as_ref().map(|m| m.path.join(".")));
        let mut resolver = Self {
            symbols: SymbolTable::new(file_id),
            resolved: ResolvedNames::default(),
            docs: DocCommentMap::default(),
            diagnostics: Vec::new(),
            pool,
            import_aliases: rustc_hash::FxHashMap::default(),
            failed_import_aliases: rustc_hash::FxHashSet::default(),
            current_module,
            imported_symbols: rustc_hash::FxHashMap::default(),
            used_symbols: rustc_hash::FxHashSet::default(),
        };
        resolver.define_prelude(program);
        resolver.symbols.setup_prelude_scope();
        resolver
    }

    pub(crate) fn resolve_local(self, program: &Program) -> ResolutionResult {
        self.resolve_local_with_poll(program, || {})
    }

    pub(crate) fn resolve_local_with_poll(
        mut self,
        program: &Program,
        mut poll: impl FnMut(),
    ) -> ResolutionResult {
        for doc in &program.docs {
            poll();
            self.docs
                .entry(NodeKey::from(doc.target_span))
                .or_default()
                .push(doc.text.to_string());
        }

        let global = self.symbols.global_scope();
        if let Some(module) = &program.module
            && let Some(root) = module.path.first()
        {
            // Module path root is always part of the file's public identity.
            self.define_vis(global, root, SymbolKind::Module, module.span, true);
        }

        for decl_id in &program.decls {
            poll();
            let decl = self.pool.decl(*decl_id);
            self.collect_top_level(global, decl);
        }

        if let Some(module) = &program.module {
            let module_name = module.path.join(".");
            for decl_id in &program.decls {
                poll();
                let TopLevelDecl::Func(decl) = self.pool.decl(*decl_id) else {
                    continue;
                };
                let name_span = match &decl.name {
                    FuncName::Free { span, .. } | FuncName::Method { span, .. } => *span,
                };
                let Some(symbol_id) = self.resolved.definitions.get(&name_span.into()).copied()
                else {
                    continue;
                };
                let symbol = self.symbols.get(symbol_id);
                if symbol.name == "main"
                    || symbol.name.starts_with("_A$")
                    || matches!(symbol.kind, SymbolKind::ExternFunc)
                {
                    continue;
                }
                let host_name = SmolStr::new(format!("{}.{}", module_name, symbol.name));
                self.symbols
                    .host_function_names
                    .insert(symbol_id, host_name);
            }
        }

        ResolutionResult {
            is_cycle_fallback: false,
            symbols: std::sync::Arc::new(self.symbols),
            resolved: std::sync::Arc::new(self.resolved),
            docs: self.docs,
            diagnostics: self.diagnostics,
        }
    }

    pub(crate) fn define_prelude(&mut self, _program: Option<&Program>) {
        let span = Span::new(0, 0, 0);
        for (module, members) in super::PRELUDE_MODULE_MEMBERS {
            for member in *members {
                let _ = self.symbols.define_module_member(module, member, span);
            }
        }
        let global_scope = self.symbols.global_scope();
        // Prelude builtins are always public (language surface).
        self.symbols.builtin_alloc = self
            .symbols
            .define_vis(global_scope, "alloc", SymbolKind::Func, span, true)
            .ok();
        self.symbols.builtin_free = self
            .symbols
            .define_vis(global_scope, "free", SymbolKind::Func, span, true)
            .ok();

        let res_sym = self
            .symbols
            .define_vis(global_scope, "Result", SymbolKind::Enum, span, true)
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Result"));
        if let Some(sym) = res_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Result);
        }

        let opt_sym = self
            .symbols
            .define_vis(global_scope, "Option", SymbolKind::Enum, span, true)
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Option"));
        if let Some(sym) = opt_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Option);
        }

        let coro_sym = self
            .symbols
            .define_vis(global_scope, "Coroutine", SymbolKind::Enum, span, true)
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Coroutine"));
        if let Some(sym) = coro_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Coroutine);
        }

        let poll_sym = self
            .symbols
            .define_vis(global_scope, "Poll", SymbolKind::Enum, span, true)
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Poll"));
        if let Some(sym) = poll_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Poll);
        }

        let global = self.symbols.global_scope();
        let has_result = self.symbols.lookup_type(global, "Result").is_some();
        let has_option = self.symbols.lookup_type(global, "Option").is_some();
        let has_poll = self.symbols.lookup_type(global, "Poll").is_some();
        tracing::debug!(target: "arandu_resolve", has_result, has_option, has_poll, "Prelude types in scope");
        tracing::debug!(target: "arandu_resolve", total = self.symbols.iter().count(), "Symbol table after prelude load");
    }
}
