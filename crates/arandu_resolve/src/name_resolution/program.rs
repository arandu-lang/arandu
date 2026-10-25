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
            reusable_definitions: rustc_hash::FxHashSet::default(),
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
            self.define_with_visibility(
                global,
                root,
                SymbolKind::Module,
                module.span,
                arandu_parser::Visibility::Public,
            );
        }

        for decl_id in &program.decls {
            poll();
            let decl = self.pool.decl(*decl_id);
            self.collect_top_level(global, decl);
        }

        // Preserve local explicit implementation edges in the lightweight
        // symbol result. Imported interface edges are added by the full
        // import pass; this local subset is enough for standalone sealed
        // interfaces and their exported metadata.
        for implementation in &program.interface_impls {
            poll();
            if let (Some(for_type), Some(interface)) = (
                super::lookup_type_name(&self.symbols, &implementation.for_type),
                super::lookup_type_name(&self.symbols, &implementation.interface),
            ) {
                self.symbols
                    .interface_implementations
                    .insert((for_type, interface));
            }
        }

        {
            let module_name = program
                .module
                .as_ref()
                .map_or_else(String::new, |module| module.path.join("."));
            let mut pending = program
                .decls
                .iter()
                .rev()
                .map(|&id| (id, module_name.clone()))
                .collect::<Vec<_>>();
            while let Some((decl_id, module_name)) = pending.pop() {
                poll();
                let name_span = match self.pool.decl(decl_id) {
                    TopLevelDecl::Func(decl) => match &decl.name {
                        FuncName::Free { span, .. } | FuncName::Method { span, .. } => *span,
                    },
                    TopLevelDecl::Struct(decl) => decl.span,
                    TopLevelDecl::Enum(decl) => decl.span,
                    TopLevelDecl::Submodule(decl) => {
                        let prefix = if module_name.is_empty() {
                            decl.name.to_string()
                        } else {
                            format!("{module_name}.{}", decl.name)
                        };
                        pending.extend(decl.decls.iter().rev().map(|&id| (id, prefix.clone())));
                        continue;
                    }
                    TopLevelDecl::Const(_)
                    | TopLevelDecl::TypeAlias(_)
                    | TopLevelDecl::Interface(_)
                    | TopLevelDecl::Extern(_)
                    | TopLevelDecl::Error(_) => continue,
                };
                let Some(symbol_id) = self.resolved.definitions.get(&name_span.into()).copied()
                else {
                    continue;
                };
                // Preserve source-declared external runner roots without
                // performing annotation validation in name resolution.
                if let TopLevelDecl::Func(decl) = self.pool.decl(decl_id)
                    && decl
                        .attrs
                        .iter()
                        .any(|attr| matches!(attr.name.as_str(), "Test" | "Benchmark"))
                {
                    self.symbols.tooling_entrypoints.insert(symbol_id);
                }
                let symbol = self.symbols.get(symbol_id);
                if module_name.is_empty()
                    || symbol.name == "main"
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
            .define_with_visibility(
                global_scope,
                "alloc",
                SymbolKind::Func,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok();
        if let Some(sym) = self.symbols.builtin_alloc {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Alloc);
        }
        self.symbols.builtin_free = self
            .symbols
            .define_with_visibility(
                global_scope,
                "free",
                SymbolKind::Func,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok();
        if let Some(sym) = self.symbols.builtin_free {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Free);
        }

        let res_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "Result",
                SymbolKind::Enum,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Result"));
        if let Some(sym) = res_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Result);
        }

        let opt_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "Option",
                SymbolKind::Enum,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Option"));
        if let Some(sym) = opt_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Option);
        }

        let coro_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "Coroutine",
                SymbolKind::Enum,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Coroutine"));
        if let Some(sym) = coro_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Coroutine);
        }

        let poll_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "Poll",
                SymbolKind::Enum,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "Poll"));
        if let Some(sym) = poll_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Poll);
        }

        let str_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "str",
                SymbolKind::Struct,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "str"));
        if let Some(sym) = str_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Str);
        }

        let slice_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "slice",
                SymbolKind::Struct,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "slice"));
        if let Some(sym) = slice_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Slice);
        }

        let char_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "char",
                SymbolKind::Struct,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "char"));
        if let Some(sym) = char_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Char);
        }

        let int_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "int",
                SymbolKind::Struct,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "int"));
        if let Some(sym) = int_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Int);
        }

        for (name, lang_item) in [
            ("u32", arandu_middle::symbol_table::LangItem::U32),
            ("u64", arandu_middle::symbol_table::LangItem::U64),
            ("usize", arandu_middle::symbol_table::LangItem::Usize),
        ] {
            let symbol = self
                .symbols
                .define_with_visibility(
                    global_scope,
                    name,
                    SymbolKind::Struct,
                    span,
                    arandu_parser::Visibility::Public,
                )
                .ok()
                .or_else(|| self.symbols.lookup_type(global_scope, name));
            if let Some(symbol) = symbol {
                self.symbols.set_lang_item(symbol, lang_item);
            }
        }

        let float_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "float",
                SymbolKind::Struct,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "float"));
        if let Some(sym) = float_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Float);
        }

        let ptr_sym = self
            .symbols
            .define_with_visibility(
                global_scope,
                "ptr",
                SymbolKind::Struct,
                span,
                arandu_parser::Visibility::Public,
            )
            .ok()
            .or_else(|| self.symbols.lookup_type(global_scope, "ptr"));
        if let Some(sym) = ptr_sym {
            self.symbols
                .set_lang_item(sym, arandu_middle::symbol_table::LangItem::Ptr);
        }

        let global = self.symbols.global_scope();
        let has_result = self.symbols.lookup_type(global, "Result").is_some();
        let has_option = self.symbols.lookup_type(global, "Option").is_some();
        let has_poll = self.symbols.lookup_type(global, "Poll").is_some();
        tracing::debug!(target: "arandu_resolve", has_result, has_option, has_poll, "Prelude types in scope");
        tracing::debug!(target: "arandu_resolve", total = self.symbols.iter().count(), "Symbol table after prelude load");
    }
}
