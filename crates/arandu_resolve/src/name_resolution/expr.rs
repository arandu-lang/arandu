use arandu_parser::{
    CatchHandler, ExprId, ExprKind, FieldInit, FieldPattern, LambdaBody, LambdaParam, MatchArm,
    MatchArmBody, Pattern, PatternId,
};

use crate::{DiagCode, Diagnostic, ScopeId, SymbolKind};

use super::Resolver;

impl<'a> Resolver<'a> {
    pub(crate) fn resolve_expr(&mut self, scope: ScopeId, expr: ExprId) {
        let span = self.pool.expr_span(expr);
        match self.pool.expr(expr) {
            ExprKind::Layout { ty, .. } => {
                if let Some(ty) = ty {
                    self.resolve_type_expr(scope, *ty);
                }
            }
            ExprKind::Path { path } => {
                if let Some(root) = path.first() {
                    if path.len() > 1
                        && self.resolve_namespace_member(scope, root, &path[1], expr, span)
                    {
                        return;
                    }
                    self.resolve_value_name(scope, root, expr, span);
                }
            }
            ExprKind::VariantSugar { args, .. } => {
                // Resolved later in typeck via expected type; still walk payload exprs.
                for arg in self.pool.expr_list(*args) {
                    self.resolve_expr(scope, *arg);
                }
            }
            ExprKind::TypePath { type_name, member } => {
                if type_name.path.len() == 1
                    && let Some(symbol) = self.symbols.lookup_value(scope, &type_name.path[0])
                    && matches!(
                        self.symbols.get(symbol).kind,
                        crate::SymbolKind::ConstParam | crate::SymbolKind::Const
                    )
                {
                    self.record_expr_ref(expr, symbol);
                    self.record_value_ref(type_name.span, symbol);
                    return;
                }
                let type_resolved = self.resolve_type_name(scope, type_name);
                let type_sym = self.resolved.type_refs.get(&type_name.span.into()).copied();
                let is_builtin_variant = type_sym.is_some_and(|symbol| {
                    (self.symbols.is_result_type(symbol) && matches!(member.as_str(), "Ok" | "Err"))
                        || (self.symbols.is_option_type(symbol)
                            && matches!(member.as_str(), "Some" | "None"))
                        || (self.symbols.is_poll_type(symbol)
                            && matches!(member.as_str(), "Ready" | "Pending"))
                });
                if is_builtin_variant {
                    return;
                }
                if type_resolved {
                    let ty = type_name.path.join(".");
                    // Prefer SymbolId-keyed lookup (Sprint 3); fall back to name for
                    // types that were not in scope (import path not yet resolved).
                    let symbol =
                        type_sym.and_then(|id| self.symbols.lookup_associated_member(id, member));
                    if let Some(symbol) = symbol {
                        self.record_expr_ref(expr, symbol);
                    } else {
                        let mut diag = Diagnostic::error(
                            DiagCode::N010UndefinedAssociatedFunction,
                            format!("associated function '{ty}.{member}' is not declared"),
                            span,
                        );
                        // Suggest close matches from the members of that type.
                        if let Some(id) = type_sym {
                            let mut candidates: Vec<&str> = self
                                .symbols
                                .associated_members
                                .keys()
                                .filter(|(parent, _)| *parent == id)
                                .map(|(_, name)| name.as_str())
                                .collect();
                            candidates.sort_unstable();
                            let max_distance = if member.len() <= 4 { 2 } else { 3 };
                            let best_match = candidates
                                .into_iter()
                                .map(|name| {
                                    let dist = if name.to_lowercase() == member.to_lowercase() {
                                        0
                                    } else {
                                        strsim::levenshtein(member, name)
                                    };
                                    (name, dist)
                                })
                                .filter(|(_, dist)| *dist <= max_distance)
                                .min_by_key(|(_, dist)| *dist)
                                .map(|(name, _)| name.to_string());
                            if let Some(suggestion) = best_match {
                                diag = diag.with_hint(format!("did you mean '{suggestion}'?"));
                            }
                        }
                        self.diagnostics.push(diag);
                    }
                }
            }
            ExprKind::Generic { callee, args, .. } => {
                self.resolve_expr(scope, *callee);
                let type_expr_ids = self.pool.type_expr_list(*args);
                for arg_id in type_expr_ids {
                    self.resolve_type_expr(scope, *arg_id);
                }
            }
            ExprKind::Field { base, field: _ } | ExprKind::SafeField { base, field: _ } => {
                if let Some((namespace, member)) = self.flatten_namespace_path(expr)
                    && self.resolve_namespace_member(scope, &namespace, &member, expr, span)
                {
                    return;
                }
                self.resolve_expr(scope, *base);
            }
            ExprKind::Try { expr: base, .. } => {
                self.resolve_expr(scope, *base);
            }
            ExprKind::Index { base, index, .. } | ExprKind::SafeIndex { base, index, .. } => {
                self.resolve_expr(scope, *base);
                self.resolve_expr(scope, *index);
            }
            ExprKind::Call {
                callee,
                args,
                trailing_block,
                ..
            } => {
                self.resolve_expr(scope, *callee);
                let arg_ids = self.pool.expr_list(*args);
                for arg in arg_ids {
                    self.resolve_expr(scope, *arg);
                }
                if let Some(block_id) = trailing_block {
                    self.resolve_block_child(scope, self.pool, self.pool.block(*block_id));
                }
            }
            ExprKind::StructLiteral { ty, fields, .. } => {
                self.resolve_type_expr(scope, *ty);
                let field_init_ids = self.pool.field_init_list(*fields);
                for field_id in field_init_ids {
                    self.resolve_field_init(scope, self.pool.field_init(*field_id));
                }
            }
            ExprKind::Array { items, .. } => {
                let item_ids = self.pool.expr_list(*items);
                for item in item_ids {
                    self.resolve_expr(scope, *item);
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                self.resolve_expr(scope, *value);
                self.resolve_type_expr(scope, *count);
            }
            ExprKind::Lambda { params, body, .. } => {
                let param_ids = self.pool.lambda_param_list(*params);
                let mut params_vec = Vec::new();
                for param_id in param_ids {
                    params_vec.push(self.pool.lambda_param(*param_id).clone());
                }
                self.resolve_lambda(scope, &params_vec, body);
            }
            ExprKind::Alloc { expr } | ExprKind::Group { expr, .. } => {
                self.resolve_expr(scope, *expr)
            }
            ExprKind::AsyncBlock { block, .. } | ExprKind::UnsafeBlock { block, .. } => {
                self.resolve_block_child(scope, self.pool, self.pool.block(*block));
            }
            ExprKind::Comptime { body } => match body {
                arandu_parser::ast_pool::ComptimeBody::Expression(expr) => {
                    self.resolve_expr(scope, *expr)
                }
                arandu_parser::ast_pool::ComptimeBody::Block(block) => {
                    self.resolve_block_child(scope, self.pool, self.pool.block(*block))
                }
            },
            ExprKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                let then_scope = self.resolve_condition(scope, self.pool, condition);
                self.resolve_block_child(then_scope, self.pool, self.pool.block(*then_block));
                self.resolve_block_child(scope, self.pool, self.pool.block(*else_block));
            }
            ExprKind::Match { value, arms, .. } => {
                self.resolve_expr(scope, *value);
                let arm_ids = self.pool.match_arm_list(*arms);
                for arm_id in arm_ids {
                    self.resolve_match_arm(scope, self.pool.match_arm(*arm_id));
                }
            }
            ExprKind::Catch { expr, handler, .. } => {
                self.resolve_expr(scope, *expr);
                self.resolve_catch_handler(scope, self.pool.catch_handler(*handler));
            }
            ExprKind::NullCoalesce { left, right, .. } => {
                self.resolve_expr(scope, *left);
                self.resolve_expr(scope, *right);
            }
            ExprKind::Cast { expr, ty, .. } => {
                self.resolve_expr(scope, *expr);
                self.resolve_type_expr(scope, *ty);
            }
            ExprKind::Unary { op: _, expr, .. } => self.resolve_expr(scope, *expr),
            ExprKind::Binary {
                op: _, left, right, ..
            } => {
                self.resolve_expr(scope, *left);
                self.resolve_expr(scope, *right);
            }
            ExprKind::InterpolatedString { parts, .. } => {
                let part_ids = self.pool.string_part_list(*parts);
                for part_id in part_ids {
                    if let arandu_parser::StringPart::Expr { expr, .. } =
                        self.pool.string_part(*part_id)
                    {
                        self.resolve_expr(scope, *expr);
                    }
                }
            }
            ExprKind::Int { .. }
            | ExprKind::Float { .. }
            | ExprKind::Bool { .. }
            | ExprKind::Char { .. }
            | ExprKind::Byte { .. }
            | ExprKind::Nil
            | ExprKind::Error => {}
        }
    }

    pub(crate) fn resolve_field_init(&mut self, scope: ScopeId, field: &FieldInit) {
        self.resolve_expr(scope, field.value);
    }

    pub(crate) fn resolve_lambda(
        &mut self,
        parent: ScopeId,
        params: &[LambdaParam],
        body: &LambdaBody,
    ) {
        let scope = self.symbols.new_scope(parent);
        for param in params {
            if let Some(ty) = &param.ty {
                self.resolve_type_expr(scope, *ty);
            }
            self.define(scope, &param.name, SymbolKind::Param, param.span);
        }
        match body {
            LambdaBody::Expr { expr, .. } => self.resolve_expr(scope, *expr),
            LambdaBody::Block { block, .. } => self.resolve_block_in_scope(scope, self.pool, block),
        }
    }

    pub(crate) fn resolve_catch_handler(&mut self, parent: ScopeId, handler: &CatchHandler) {
        match handler {
            CatchHandler::Expr { expr, .. } => self.resolve_expr(parent, *expr),
            CatchHandler::Block { span, error, block } => {
                let scope = self.symbols.new_scope(parent);
                self.define(scope, error, SymbolKind::Local, *span);
                self.resolve_block_in_scope(scope, self.pool, block);
            }
        }
    }

    pub(crate) fn resolve_match_arm(&mut self, parent: ScopeId, arm: &MatchArm) {
        let scope = self.symbols.new_scope(parent);
        self.resolve_pattern(scope, arm.pattern);
        if let Some(guard) = &arm.guard {
            self.resolve_expr(scope, *guard);
        }
        match &arm.body {
            MatchArmBody::Expr { expr, .. } => self.resolve_expr(scope, *expr),
            MatchArmBody::Block { block, .. } => {
                self.resolve_block_in_scope(scope, self.pool, block)
            }
        }
    }

    pub(crate) fn resolve_pattern(&mut self, scope: ScopeId, pattern: PatternId) {
        match self.pool.pattern(pattern) {
            Pattern::Wildcard { .. } => {}
            Pattern::Bind {
                span,
                name,
                mutable,
            } => {
                self.define(scope, name, SymbolKind::Local, *span);
                if *mutable
                    && let Some(symbol) = self.resolved.definitions.get(&(*span).into()).copied()
                {
                    self.resolved.mutable_symbols.insert(symbol);
                }
            }
            Pattern::Literal { expr, .. } => self.resolve_expr(scope, *expr),
            Pattern::Enum {
                type_name, payload, ..
            } => {
                self.resolve_type_name(scope, type_name);
                for &item in self.pool.pattern_list(*payload) {
                    self.resolve_pattern(scope, item);
                }
            }
            Pattern::TypeTuple { payload, .. } => {
                for &item in self.pool.pattern_list(*payload) {
                    self.resolve_pattern(scope, item);
                }
            }
            Pattern::Struct {
                type_name, fields, ..
            } => {
                self.resolve_type_name(scope, type_name);
                for &field_id in self.pool.field_pattern_list(*fields) {
                    let field = self.pool.field_pattern(field_id);
                    self.resolve_field_pattern(scope, field);
                }
            }
            Pattern::Tuple { items, .. } => {
                for &item in self.pool.pattern_list(*items) {
                    self.resolve_pattern(scope, item);
                }
            }
            Pattern::Range { start, end, .. } => {
                self.resolve_expr(scope, *start);
                self.resolve_expr(scope, *end);
            }
            Pattern::Or { alts, .. } => {
                for &item in self.pool.pattern_list(*alts) {
                    self.resolve_pattern(scope, item);
                }
            }
        }
    }

    pub(crate) fn resolve_field_pattern(&mut self, scope: ScopeId, field: &FieldPattern) {
        if let Some(pattern) = field.pattern {
            self.resolve_pattern(scope, pattern);
        } else {
            self.define(scope, &field.name, SymbolKind::Local, field.span);
        }
    }

    fn flatten_namespace_path(
        &self,
        expr: arandu_parser::ast_pool::ExprId,
    ) -> Option<(String, String)> {
        use arandu_parser::ast_pool::ExprKind;
        let mut current = expr;
        let mut segments = Vec::new();
        loop {
            match self.pool.expr(current) {
                ExprKind::Field { base, field } | ExprKind::SafeField { base, field } => {
                    segments.push(field.clone());
                    current = *base;
                }
                ExprKind::Path { path } => {
                    if let Some(root) = path.first()
                        && path.len() == 1
                    {
                        segments.push(root.clone());
                        break;
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        segments.reverse();
        let member = segments.pop()?;
        let namespace = segments.join(".");
        Some((namespace, member.to_string()))
    }
}
