use arandu_parser::{
    Attribute, ConstDecl, EnumDecl, EnumPayload, EnumVariant, ExprKind, FieldDecl, FuncDecl,
    FuncName, FuncSignature, GenericParam, InterfaceDecl, Param, StructDecl, TopLevelDecl,
    TypeAliasDecl, TypeName, WhereItem,
};

use crate::{ScopeId, SymbolKind};

use super::Resolver;

impl<'a> Resolver<'a> {
    pub(crate) fn resolve_top_level(&mut self, scope: ScopeId, decl: &TopLevelDecl) {
        match decl {
            TopLevelDecl::Const(decl) => self.resolve_const(scope, decl),
            TopLevelDecl::TypeAlias(decl) => self.resolve_type_alias(scope, decl),
            TopLevelDecl::Func(decl) => self.resolve_func(scope, decl),
            TopLevelDecl::Struct(decl) => self.resolve_struct(scope, decl),
            TopLevelDecl::Enum(decl) => self.resolve_enum(scope, decl),
            TopLevelDecl::Interface(decl) => self.resolve_interface(scope, decl),
            TopLevelDecl::Extern(decl) => {
                self.resolve_attrs(scope, &decl.attrs);
                for member in &decl.members {
                    self.resolve_signature(scope, member);
                }
            }
            TopLevelDecl::Submodule(decl) => {
                self.resolve_attrs(scope, &decl.attrs);
                let sub_scope = self
                    .symbols
                    .lookup_module(scope, &decl.name)
                    .and_then(|id| self.symbols.module_scopes.get(&id).copied())
                    .unwrap_or(scope);
                for &inner_id in &decl.decls {
                    let inner_decl = self.pool.decl(inner_id);
                    self.resolve_top_level(sub_scope, inner_decl);
                }
            }
            TopLevelDecl::Error(_) => {}
        }
    }

    pub(crate) fn resolve_const(&mut self, scope: ScopeId, decl: &ConstDecl) {
        self.resolve_attrs(scope, &decl.attrs);
        if let Some(ty) = &decl.ty {
            self.resolve_type_expr(scope, *ty);
        }
        self.resolve_expr(scope, decl.value);
    }

    pub(crate) fn resolve_type_alias(&mut self, scope: ScopeId, decl: &TypeAliasDecl) {
        self.resolve_attrs(scope, &decl.attrs);
        let alias_scope = self.symbols.new_scope(scope);
        self.define_generics(alias_scope, &decl.generic_params);
        self.resolve_type_expr(alias_scope, decl.ty);
    }

    pub(crate) fn resolve_func(&mut self, scope: ScopeId, decl: &FuncDecl) {
        let func_scope = self.resolve_func_header(scope, decl);
        self.resolve_block_in_scope(func_scope, self.pool, &decl.body);
    }

    pub(crate) fn resolve_func_header(&mut self, scope: ScopeId, decl: &FuncDecl) -> ScopeId {
        self.resolve_attrs(scope, &decl.attrs);
        let func_scope = self.symbols.new_scope(scope);
        // Methods on generic types must see the receiver type's type params
        // (`func Box.get(): T` needs `T` from `struct Box<T>`).
        if let FuncName::Method { receiver, .. } = &decl.name {
            self.import_receiver_type_params(scope, func_scope, receiver, decl);
            self.resolve_type_name(func_scope, receiver);
        }
        self.define_generics(func_scope, &decl.generic_params);
        for where_item in &decl.where_clause {
            self.resolve_where_item(func_scope, where_item);
        }
        for param in &decl.params {
            self.resolve_param(func_scope, param);
        }
        if let Some(result) = &decl.result {
            self.resolve_result_type(func_scope, result);
        }
        func_scope
    }

    /// Allocate declaration scopes before any function body. Body scopes are
    /// revision-local continuations, not new persistent symbol identities.
    pub(crate) fn resolve_top_level_headers(
        &mut self,
        scope: ScopeId,
        decl: &TopLevelDecl,
        bodies: &mut rustc_hash::FxHashMap<crate::NodeKey, ScopeId>,
    ) {
        match decl {
            TopLevelDecl::Func(function) => {
                let func_scope = self.resolve_func_header(scope, function);
                let key = match &function.name {
                    FuncName::Free { span, .. } | FuncName::Method { span, .. } => (*span).into(),
                };
                bodies.insert(key, func_scope);
            }
            TopLevelDecl::Submodule(module) => {
                self.resolve_attrs(scope, &module.attrs);
                let sub_scope = self
                    .symbols
                    .lookup_module(scope, &module.name)
                    .and_then(|id| self.symbols.module_scopes.get(&id).copied())
                    .unwrap_or(scope);
                for &id in &module.decls {
                    self.resolve_top_level_headers(sub_scope, self.pool.decl(id), bodies);
                }
            }
            TopLevelDecl::Const(_)
            | TopLevelDecl::TypeAlias(_)
            | TopLevelDecl::Struct(_)
            | TopLevelDecl::Enum(_)
            | TopLevelDecl::Interface(_)
            | TopLevelDecl::Extern(_)
            | TopLevelDecl::Error(_) => self.resolve_top_level(scope, decl),
        }
    }

    /// Bind parent type parameters into a method scope (same `SymbolId`s as the type),
    /// unless the method explicitly redeclares that parameter with its own constraints.
    fn import_receiver_type_params(
        &mut self,
        scope: ScopeId,
        func_scope: ScopeId,
        receiver: &TypeName,
        decl: &FuncDecl,
    ) {
        let Some(root) = receiver.path.first() else {
            return;
        };
        let Some(type_id) = self.symbols.lookup_type(scope, root) else {
            return;
        };
        let params: smallvec::SmallVec<[crate::SymbolId; 4]> = self
            .symbols
            .type_params_of(type_id)
            .iter()
            .copied()
            .collect();
        for param in params {
            let param_name = self.symbols.get(param).name.clone();
            let has_method_constraints = decl
                .generic_params
                .iter()
                .any(|gp| gp.name == param_name && !gp.constraints.is_empty())
                || decl
                    .where_clause
                    .iter()
                    .any(|w| w.name == param_name && !w.constraints.is_empty());
            if !has_method_constraints {
                self.symbols.bind_existing(func_scope, param);
            }
        }
    }

    pub(crate) fn resolve_struct(&mut self, scope: ScopeId, decl: &StructDecl) {
        self.resolve_attrs(scope, &decl.attrs);
        let struct_scope = self.symbols.new_scope(scope);
        self.define_generics(struct_scope, &decl.generic_params);
        self.record_named_type_params(decl.span, &decl.generic_params);
        for where_item in &decl.where_clause {
            self.resolve_where_item(struct_scope, where_item);
        }
        let mut seen_fields = smallvec::SmallVec::<[(&str, arandu_lexer::Span); 8]>::new();
        for field in &decl.fields {
            if let Some((_, prev_span)) = seen_fields.iter().find(|(name, _)| *name == field.name) {
                self.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T030DuplicateFieldDecl,
                        format!(
                            "field '{}' is already declared in struct '{}'",
                            field.name, decl.name
                        ),
                        field.span,
                    )
                    .with_label(*prev_span, "first declaration here")
                    .with_label(field.span, "duplicate field"),
                );
            } else {
                seen_fields.push((&field.name, field.span));
                self.resolve_field(struct_scope, field);
            }
        }
    }

    pub(crate) fn resolve_enum(&mut self, scope: ScopeId, decl: &EnumDecl) {
        self.resolve_attrs(scope, &decl.attrs);
        let enum_scope = self.symbols.new_scope(scope);
        self.define_generics(enum_scope, &decl.generic_params);
        self.record_named_type_params(decl.span, &decl.generic_params);
        for where_item in &decl.where_clause {
            self.resolve_where_item(enum_scope, where_item);
        }
        for variant in &decl.variants {
            self.resolve_enum_variant(enum_scope, variant);
        }
    }

    pub(crate) fn resolve_interface(&mut self, scope: ScopeId, decl: &InterfaceDecl) {
        self.resolve_attrs(scope, &decl.attrs);
        let interface_scope = self.symbols.new_scope(scope);
        self.define_generics(interface_scope, &decl.generic_params);
        // TYP.2: `Self` is the implementing type in interface method signatures
        // (`func fmt(shared self): str` types `self` as `Self`).
        // Use a synthetic span so we do **not** overwrite `definitions[decl.span]`
        // (that key already maps to the interface symbol itself).
        if self
            .symbols
            .find_in_scope(interface_scope, "Self")
            .is_none()
        {
            let self_span =
                arandu_lexer::Span::new(decl.span.file_id, decl.span.start, decl.span.start);
            self.define(interface_scope, "Self", SymbolKind::TypeParam, self_span);
        }
        for where_item in &decl.where_clause {
            self.resolve_where_item(interface_scope, where_item);
        }
        for member in &decl.members {
            self.resolve_signature(interface_scope, member);
        }
    }

    pub(crate) fn resolve_signature(&mut self, scope: ScopeId, signature: &FuncSignature) {
        self.resolve_attrs(scope, &signature.attrs);
        let sig_scope = self.symbols.new_scope(scope);
        self.define_generics(sig_scope, &signature.generic_params);
        for where_item in &signature.where_clause {
            self.resolve_where_item(sig_scope, where_item);
        }
        for param in &signature.params {
            self.resolve_param(sig_scope, param);
        }
        if let Some(result) = &signature.result {
            self.resolve_result_type(sig_scope, result);
        }
    }

    pub(crate) fn define_generics(&mut self, scope: ScopeId, generics: &[GenericParam]) {
        // Pass 1: Bind all type parameter names into scope first so forward/mutual
        // references between type parameters (e.g. `<I: Iterator<Item>, Item>`) resolve.
        for generic in generics {
            // Methods on generic types restate receiver params (`func Vec.push<T, A>(…)`).
            // Those names were already bound via `import_receiver_type_params` to the
            // **same** SymbolIds as the type — redefining them was the root of N003
            // cascades and bogus T025 ("type 'A' does not satisfy Allocator").
            let kind = if generic.const_ty.is_some() {
                SymbolKind::ConstParam
            } else {
                SymbolKind::TypeParam
            };
            if let Some(existing) = self.symbols.find_in_scope(scope, &generic.name)
                && self.symbols.get(existing).kind == kind
            {
                self.resolved.define(generic.span, existing);
            } else {
                self.define(scope, &generic.name, kind, generic.span);
            }
        }

        // Pass 2: Resolve constraints and default types now that all parameters are in scope.
        for generic in generics {
            if let Some(const_ty) = generic.const_ty {
                self.resolve_type_expr(scope, const_ty);
            }
            for constraint in &generic.constraints {
                self.resolve_type_expr(scope, *constraint);
            }
            // T2.1: default type arg (`A = GlobalAllocator`) must be name-resolved
            // so typeck can lower it into `generic_defaults`.
            if let Some(def_ty) = generic.default {
                self.resolve_type_expr(scope, def_ty);
            }
        }
    }

    /// After `define_generics`, attach the param symbols to the named type's SymbolId.
    fn record_named_type_params(
        &mut self,
        type_span: arandu_lexer::Span,
        generics: &[GenericParam],
    ) {
        let type_key = crate::NodeKey::from(type_span);
        let Some(type_id) = self.resolved.definitions.get(&type_key).copied() else {
            return;
        };
        let params: smallvec::SmallVec<[crate::SymbolId; 4]> = generics
            .iter()
            .filter_map(|gp| {
                self.resolved
                    .definitions
                    .get(&crate::NodeKey::from(gp.span))
                    .copied()
            })
            .collect();
        if !params.is_empty() {
            self.symbols.record_type_params(type_id, params);
        }
    }

    pub(crate) fn resolve_where_item(&mut self, scope: ScopeId, item: &WhereItem) {
        self.resolve_type_name(
            scope,
            &TypeName {
                span: item.span,
                path: vec![item.name.clone()].into(),
            },
        );
        for constraint in &item.constraints {
            self.resolve_type_expr(scope, *constraint);
        }
    }

    pub(crate) fn resolve_param(&mut self, scope: ScopeId, param: &Param) {
        self.resolve_attrs(scope, &param.attrs);
        self.resolve_type_expr(scope, param.ty);
        self.define(scope, &param.name, SymbolKind::Param, param.span);
        let is_mut = param.ownership == Some(arandu_parser::Ownership::Mut);
        if let Some(symbol_id) = self
            .resolved
            .definitions
            .get(&param.span.into())
            .copied()
            .filter(|_| is_mut)
        {
            self.resolved.mutable_symbols.insert(symbol_id);
        }
    }

    pub(crate) fn resolve_field(&mut self, scope: ScopeId, field: &FieldDecl) {
        self.resolve_attrs(scope, &field.attrs);
        self.resolve_type_expr(scope, field.ty);
        self.define(scope, &field.name, SymbolKind::Field, field.span);
    }

    pub(crate) fn resolve_enum_variant(&mut self, scope: ScopeId, variant: &EnumVariant) {
        self.resolve_attrs(scope, &variant.attrs);
        if let Some(&canonical_id) = self
            .resolved
            .definitions
            .get(&crate::NodeKey::from(variant.span))
        {
            self.symbols.bind_existing(scope, canonical_id);
        }
        match &variant.payload {
            Some(EnumPayload::Tuple { types, .. }) => {
                for ty in self.pool.type_expr_list(*types) {
                    self.resolve_type_expr(scope, *ty);
                }
            }
            Some(EnumPayload::Struct { fields, .. }) => {
                let variant_scope = self.symbols.new_scope(scope);
                let mut seen_fields = smallvec::SmallVec::<[(&str, arandu_lexer::Span); 8]>::new();
                for field in fields {
                    if let Some((_, prev_span)) =
                        seen_fields.iter().find(|(name, _)| *name == field.name)
                    {
                        self.diagnostics.push(
                            crate::Diagnostic::error(
                                crate::DiagCode::T030DuplicateFieldDecl,
                                format!(
                                    "field '{}' is already declared in enum variant '{}'",
                                    field.name, variant.name
                                ),
                                field.span,
                            )
                            .with_label(*prev_span, "first declaration here")
                            .with_label(field.span, "duplicate field"),
                        );
                    } else {
                        seen_fields.push((&field.name, field.span));
                        self.resolve_field(variant_scope, field);
                    }
                }
            }
            None => {}
        }
    }
}

fn is_effect_arg(expr: &ExprKind) -> bool {
    if let ExprKind::Path { path } = expr
        && path.len() == 1
        && let Some(name) = path.first()
    {
        return arandu_middle::EffectFlags::from_name(name).is_some();
    }
    false
}

impl<'a> Resolver<'a> {
    pub(crate) fn resolve_attrs(&mut self, scope: ScopeId, attrs: &[Attribute]) {
        for attr in attrs {
            for arg in &attr.args {
                // Annotation args are annotation payloads, not value
                // expressions. Known effect names (`@Effects(FileRead)`) are
                // resolved by typeck, so skip them here; this avoids spurious
                // `N001` (undefined value) for legal annotations.
                if is_effect_arg(self.pool.expr(*arg)) {
                    continue;
                }
                self.resolve_expr(scope, *arg);
            }
        }
    }

    pub(crate) fn resolve_method_receivers(&mut self, program: &arandu_parser::Program) {
        let global = self.symbols.global_scope();
        self.resolve_method_receivers_in_decls(global, &program.decls);
    }

    fn resolve_method_receivers_in_decls(
        &mut self,
        global: ScopeId,
        decls: &[arandu_parser::DeclId],
    ) {
        for decl_id in decls {
            let decl = self.pool.decl(*decl_id);
            match decl {
                arandu_parser::TopLevelDecl::Func(decl) => {
                    if let arandu_parser::FuncName::Method {
                        ref receiver,
                        ref name,
                        ref span,
                    } = decl.name
                        && self.resolve_type_name(global, receiver)
                        && let Some(struct_sym) =
                            self.resolved.type_refs.get(&receiver.span.into()).copied()
                        && let Some(method_sym) =
                            self.resolved.definitions.get(&(*span).into()).copied()
                    {
                        self.symbols
                            .associated_members
                            .insert((struct_sym, name.clone()), method_sym);
                    }
                }
                arandu_parser::TopLevelDecl::Submodule(decl) => {
                    let sub_scope = self
                        .symbols
                        .lookup_module(global, &decl.name)
                        .and_then(|id| self.symbols.module_scopes.get(&id).copied())
                        .unwrap_or(global);
                    self.resolve_method_receivers_in_decls(sub_scope, &decl.decls);
                }
                _ => {}
            }
        }
    }
}
