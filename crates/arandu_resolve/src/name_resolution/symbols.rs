use arandu_lexer::Span;
use arandu_parser::ast_pool::ExprId;

use crate::{DiagCode, Diagnostic, ScopeId, SymbolKind};

use super::Resolver;

impl<'a> Resolver<'a> {
    pub(crate) fn resolve_value_name(
        &mut self,
        scope: ScopeId,
        name: &str,
        expr: ExprId,
        span: Span,
    ) {
        if let Some(ref cur_mod) = self.current_module
            && let Some(symbol) = self.symbols.lookup_module_member(cur_mod, name)
        {
            self.record_expr_ref(expr, symbol);
            return;
        }
        if let Some(symbol) = self.symbols.lookup_value(scope, name) {
            self.record_expr_ref(expr, symbol);
            return;
        }
        if self.lookup_and_record_module(scope, name).is_some() {
            self.diagnostics.push(Diagnostic::error(
                DiagCode::M003NamespaceUsedAsValue,
                format!("namespace '{name}' cannot be used as a value"),
                span,
            ));
            return;
        }
        if let Some(symbol) = self.symbols.lookup_any(scope, name)
            && self.symbols.get(symbol).kind.is_type()
        {
            self.diagnostics.push(Diagnostic::error(
                DiagCode::N004TypeUsedAsValue,
                format!("type '{name}' cannot be used as a value"),
                span,
            ));
            return;
        }
        let mut diagnostic = Diagnostic::error(
            DiagCode::N001UndefinedValue,
            format!("value '{name}' is not declared"),
            span,
        );
        if let Some(suggestion) = self.suggest_value(scope, name) {
            diagnostic = diagnostic.with_hint(format!("did you mean '{suggestion}'?"));
        }
        self.diagnostics.push(diagnostic);
    }

    pub(crate) fn resolve_assignment_target(&mut self, scope: ScopeId, name: &str, span: Span) {
        if let Some(ref cur_mod) = self.current_module
            && let Some(symbol) = self.symbols.lookup_module_member(cur_mod, name)
        {
            self.record_value_ref(span, symbol);
            return;
        }
        if let Some(symbol) = self.symbols.lookup_value(scope, name) {
            self.record_value_ref(span, symbol);
            return;
        }
        let diagnostic = Diagnostic::error(
            DiagCode::N007UndefinedAssignmentTarget,
            format!("assignment target '{name}' is not declared"),
            span,
        )
        .with_hint(format!(
            "declare it first with `{name} = ...`, then mutate with `set {name} = ...`"
        ));
        self.diagnostics.push(diagnostic);
    }

    pub(crate) fn is_namespace(&self, scope: ScopeId, name: &str) -> bool {
        let root = name.split('.').next().unwrap_or(name);
        if self.symbols.lookup_module(scope, root).is_some() {
            return true;
        }
        self.symbols.lookup_value(scope, root).is_some_and(|id| {
            matches!(
                self.symbols.get(id).kind,
                SymbolKind::ImportValue | SymbolKind::Module
            )
        })
    }

    pub(crate) fn expand_namespace_alias(&self, namespace: &str) -> String {
        let mut segments = namespace.split('.');
        if let Some(first) = segments.next() {
            if let Some(expanded) = self.import_aliases.get(first) {
                let rest: Vec<&str> = segments.collect();
                if rest.is_empty() {
                    expanded.to_string()
                } else {
                    format!("{expanded}.{}", rest.join("."))
                }
            } else {
                namespace.to_string()
            }
        } else {
            namespace.to_string()
        }
    }

    pub(crate) fn resolve_namespace_member(
        &mut self,
        scope: ScopeId,
        namespace: &str,
        member: &str,
        expr: ExprId,
        span: Span,
    ) -> bool {
        if !self.is_namespace(scope, namespace) {
            return false;
        }
        if let Some(root) = namespace.split('.').next()
            && self.failed_import_aliases.contains(root)
        {
            // The import failed to resolve (M001 already reports that), but
            // the alias is still referenced by this member path. Mark it used
            // so the unused-import pass does not double-report W007, then
            // short-circuit the member lookup to avoid an M002 cascade.
            let _ = self.lookup_and_record_module(scope, root);
            return true;
        }
        let _ = self.lookup_and_record_module(scope, namespace);
        let expanded = self.expand_namespace_alias(namespace);
        if let Some(symbol) = self.symbols.lookup_module_member(&expanded, member) {
            self.record_expr_ref(expr, symbol);
        } else {
            self.diagnostics.push(Diagnostic::error(
                DiagCode::M002UndefinedNamespaceMember,
                format!("namespace member '{namespace}.{member}' is not declared"),
                span,
            ));
        }
        true
    }

    pub(crate) fn define(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: SymbolKind,
        span: Span,
    ) -> Option<crate::SymbolId> {
        self.define_vis(scope, name, kind, span, false)
    }

    pub(crate) fn define_vis(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: SymbolKind,
        span: Span,
        is_public: bool,
    ) -> Option<crate::SymbolId> {
        // A name is reserved if it is already defined in the prelude (ScopeId(0))
        // and we are currently not inside the prelude/std.* itself.
        let is_reserved =
            scope != ScopeId(0) && self.symbols.find_in_scope(ScopeId(0), name).is_some();
        if is_reserved {
            let is_std = self
                .current_module
                .as_ref()
                .map(|m| m.starts_with("std."))
                .unwrap_or(false);
            if !is_std {
                self.diagnostics.push(Diagnostic::error(
                    DiagCode::N003RedefinedName,
                    format!("name '{name}' is a reserved builtin and cannot be redefined"),
                    span,
                ));
                return None;
            }
        }
        match self.symbols.define_vis(scope, name, kind, span, is_public) {
            Ok(symbol) => {
                self.resolved.define(span, symbol);
                Some(symbol)
            }
            Err(previous) => {
                let previous_symbol = self.symbols.get(previous);
                if kind == SymbolKind::Module && previous_symbol.kind == SymbolKind::Module {
                    // Two `import … as alias` bindings to the same name (e.g. stdlib +
                    // package-local) must not silently merge — emit N006 (PROMOTE-L2).
                    // Non-import redefines of the same module symbol still share id.
                    if self.imported_symbols.contains_key(&previous) {
                        let previous_span = previous_symbol.span;
                        self.diagnostics.push(
                            Diagnostic::error(
                                DiagCode::N006ImportConflict,
                                format!("import `{name}` conflicts with an existing declaration"),
                                span,
                            )
                            .with_label(previous_span, "already defined here"),
                        );
                        return None;
                    }
                    self.resolved.define(span, previous);
                    return Some(previous);
                }
                self.diagnostics.push(
                    Diagnostic::error(
                        DiagCode::N003RedefinedName,
                        format!("name '{name}' is already declared in this scope"),
                        span,
                    )
                    .with_label(previous_symbol.span, "previous declaration is here"),
                );
                None
            }
        }
    }

    pub(crate) fn suggest_value(&self, scope: ScopeId, name: &str) -> Option<String> {
        self.suggest_from(name, self.symbols.value_candidates(scope))
    }

    pub(crate) fn suggest_type(&self, scope: ScopeId, name: &str) -> Option<String> {
        self.suggest_from(name, self.symbols.type_candidates(scope))
    }

    pub(crate) fn suggest_from(
        &self,
        name: &str,
        candidates: impl IntoIterator<Item = &'a crate::Symbol>,
    ) -> Option<String> {
        let max_distance = if name.len() <= 4 { 2 } else { 3 };

        candidates
            .into_iter()
            .map(|symbol| {
                let dist = if symbol.name.to_lowercase() == name.to_lowercase() {
                    0
                } else {
                    strsim::levenshtein(name, &symbol.name)
                };
                (symbol, dist)
            })
            .filter(|(_, dist)| *dist <= max_distance)
            .min_by_key(|(_, dist)| *dist)
            .map(|(symbol, _)| symbol.name.to_string())
    }
}
