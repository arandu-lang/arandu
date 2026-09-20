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
                elem,
                ..
            } => {
                if size.parse::<u64>().is_err() {
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
            self.diagnostics.push(Diagnostic::error(
                DiagCode::M002UndefinedNamespaceMember,
                format!("namespace member '{namespace}.{member}' is not declared"),
                name.span,
            ));
            return false;
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
        if matches!(root.as_str(), "void" | "Err") {
            return true;
        }
        let mut diagnostic = Diagnostic::error(
            DiagCode::N002UndefinedType,
            format!("type '{root}' is not declared"),
            name.span,
        );
        if let Some(suggestion) = self.suggest_type(scope, root) {
            diagnostic = diagnostic.with_hint(format!("did you mean '{suggestion}'?"));
        }
        self.diagnostics.push(diagnostic);
        false
    }
}
