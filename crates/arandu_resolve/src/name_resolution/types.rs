use arandu_parser::{ResultType, TypeExpr, TypeExprId, TypeName};

use crate::{DiagCode, Diagnostic, ScopeId};

use super::Resolver;

impl<'a> Resolver<'a> {
    pub(crate) fn resolve_result_type(&mut self, scope: ScopeId, result: &ResultType) {
        match result {
            ResultType::Single { ty, .. } => self.resolve_type_expr(scope, *ty),
            ResultType::Multi { types, .. } => {
                for ty in self.pool.type_expr_list(*types) {
                    self.resolve_type_expr(scope, *ty);
                }
            }
        }
    }

    pub(crate) fn resolve_type_expr(&mut self, scope: ScopeId, ty: TypeExprId) {
        match self.pool.type_expr(ty) {
            TypeExpr::Const { .. } | TypeExpr::Primitive { .. } => {}
            TypeExpr::ConstExpression { expression, .. } => self.resolve_expr(scope, *expression),
            TypeExpr::Named { name, args, .. } => {
                self.resolve_type_name(scope, name);
                for arg in self.pool.type_expr_list(*args) {
                    self.resolve_type_expr(scope, *arg);
                }
            }
            TypeExpr::Nullable { inner, .. }
            | TypeExpr::Pointer { inner, .. }
            | TypeExpr::Ref { inner, .. }
            | TypeExpr::RefMut { inner, .. }
            | TypeExpr::Slice { inner, .. }
            | TypeExpr::Group { inner, .. } => self.resolve_type_expr(scope, *inner),
            TypeExpr::Array {
                size,
                size_span,
                size_expression,
                elem,
                ..
            } => {
                if let Some(expression) = size_expression {
                    self.resolve_type_expr(scope, *expression);
                } else if size.parse::<u64>().is_err() {
                    let name = TypeName {
                        span: *size_span,
                        path: smallvec::smallvec![size.clone()],
                    };
                    self.resolve_type_name(scope, &name);
                }
                self.resolve_type_expr(scope, *elem);
            }
            TypeExpr::Func { params, result, .. } => {
                for param in self.pool.type_expr_list(*params) {
                    self.resolve_type_expr(scope, *param);
                }
                if let Some(result) = result {
                    self.resolve_result_type(scope, result);
                }
            }
        }
    }

    pub(crate) fn resolve_type_name(&mut self, scope: ScopeId, name: &TypeName) -> bool {
        let Some(root) = name.path.first() else {
            return false;
        };
        if name.path.len() > 1 && self.is_namespace(scope, root) {
            if self.failed_import_aliases.contains(root) {
                // The module import failed (M001 reports it); keep the alias
                // marked as used and skip the member lookup so the diagnostic
                // surface stays at the root failure instead of cascading.
                let _ = self.lookup_and_record_module(scope, root);
                return false;
            }
            let _ = self.lookup_and_record_module(scope, root);
            let namespace_parts = &name.path[0..name.path.len() - 1];
            let namespace = namespace_parts.join(".");
            let Some(member) = name.path.last() else {
                return false;
            };
            let expanded_namespace = self.expand_namespace_alias(&namespace);
            if let Some(symbol) = self
                .symbols
                .lookup_module_member(&expanded_namespace, member)
            {
                self.record_type_ref(name.span, symbol);
                return true;
            }
            if name.path.len() >= 3 {
                let enum_ns = name.path[0..name.path.len() - 2].join(".");
                let expanded_enum_ns = self.expand_namespace_alias(&enum_ns);
                let enum_name = &name.path[name.path.len() - 2];
                if let Some(enum_sym) = self
                    .symbols
                    .lookup_module_member(&expanded_enum_ns, enum_name)
                    && let Some(variant_sym) =
                        self.symbols.lookup_associated_member(enum_sym, member)
                {
                    self.record_type_ref(name.span, variant_sym);
                    return true;
                }
            }
            self.diagnostics.push(Diagnostic::error(
                DiagCode::M002UndefinedNamespaceMember,
                format!("namespace member '{namespace}.{member}' is not declared"),
                name.span,
            ));
            return false;
        }
        if name.path.len() == 2 {
            let enum_sym_opt = self
                .current_module
                .as_ref()
                .and_then(|cur_mod| self.symbols.lookup_module_member(cur_mod, root))
                .or_else(|| self.symbols.lookup_type(scope, root))
                .or_else(|| self.symbols.lookup_type(self.symbols.global_scope(), root));
            if let Some(enum_sym) = enum_sym_opt
                && let Some(variant_sym) = self
                    .symbols
                    .lookup_associated_member(enum_sym, &name.path[1])
            {
                self.record_type_ref(name.span, variant_sym);
                return true;
            }
        }
        if let Some(ref cur_mod) = self.current_module
            && let Some(symbol) = self.symbols.lookup_module_member(cur_mod, root)
        {
            self.record_type_ref(name.span, symbol);
            return true;
        }
        if let Some(symbol) = self.symbols.lookup_type(scope, root) {
            self.record_type_ref(name.span, symbol);
            return true;
        }
        if let Some(symbol) = self.symbols.lookup_any(scope, root)
            && self.symbols.get(symbol).kind.is_value()
        {
            self.diagnostics.push(Diagnostic::error(
                DiagCode::N005ValueUsedAsType,
                format!("value '{root}' cannot be used as a type"),
                name.span,
            ));
            return false;
        }
        if let Some(symbol) = self.symbols.lookup_type(self.symbols.global_scope(), root) {
            self.record_type_ref(name.span, symbol);
            return true;
        }
        if name.path.len() == 1 {
            let mut matched_variant: Option<crate::SymbolId> = None;
            for ((parent_id, member_name), &variant_sym) in &self.symbols.associated_members {
                if member_name == root
                    && self
                        .symbols
                        .try_get(*parent_id)
                        .is_some_and(|s| s.kind == crate::SymbolKind::Enum)
                    && matched_variant.is_none_or(|prev| {
                        (variant_sym.file_id, variant_sym.local_id.0)
                            < (prev.file_id, prev.local_id.0)
                    })
                {
                    matched_variant = Some(variant_sym);
                }
            }
            if let Some(variant_sym) = matched_variant {
                self.record_type_ref(name.span, variant_sym);
                return true;
            }
        }
        if matches!(root.as_str(), "void" | "Err") {
            return true;
        }
        let mut diagnostic = Diagnostic::error(
            DiagCode::N002UndefinedType,
            format!("type '{root}' is not declared"),
            name.span,
        )
        .with_primary_label(format!("type `{root}` not found"));
        if let Some(suggestion) = self.suggest_type(scope, root) {
            diagnostic = diagnostic.with_hint(format!("did you mean '{suggestion}'?"));
        }
        self.diagnostics.push(diagnostic);
        false
    }
}
