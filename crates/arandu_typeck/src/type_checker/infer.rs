//! Inference variables in body checking (TYP.NEXT S1, rustc's model).
//!
//! Checking stays bidirectional and eager. Where a generic parameter has no
//! argument, annotation or expected type to bind it (`Vec.new()`), the
//! checker uses a fresh variable instead of failing; unification binds it as
//! the body goes on (`v.push(1)`), and the interner resolves it everywhere.
//! Checks that need a concrete type are deferred as obligations.
//!
//! At the end of a body, [`TypeChecker::write_back_inference`] makes the
//! body's recorded types concrete: literal groups and variables agree on
//! one width, integer and float variables take their defaults, and a
//! variable still unbound in a recorded type is `T001` at the place it was
//! created, once, in creation order. Nothing after type checking sees a
//! variable.

use arandu_lexer::Span;
use arandu_middle::types::infer::{resolve_deep, unbound_vars};
use arandu_middle::types::{InferKind, Primitive};

use super::solver::TypeVarId;
use super::types::{ArType, TypeId};
use super::{TypeChecker, constraints::ConstraintOrigin};
use crate::SymbolId;

/// Where a variable came from, for the diagnostic when it stays unbound.
#[derive(Debug, Clone)]
pub(crate) struct InferOrigin {
    /// The expression that needed it, such as the call `Vec.new()`.
    pub span: Span,
    /// The generic parameter it stands for; `None` for a closure's result.
    pub param: Option<SymbolId>,
    /// The generic item, when known.
    pub item: Option<SymbolId>,
}

/// An instantiation whose interface bounds wait for its arguments.
#[derive(Debug, Clone)]
pub(crate) struct DeferredInstantiation {
    pub params: Vec<SymbolId>,
    pub args: Vec<TypeId>,
    pub span: Span,
}

/// A `todo` hole waiting for its goal to be solved (RFC 0036, H1).
#[derive(Debug, Clone)]
pub(crate) struct PendingHole {
    pub span: Span,
    pub goal: TypeId,
    pub reason: Option<smol_str::SmolStr>,
    /// The scope the hole was written in; its locals are candidate fills.
    pub scope: Option<crate::ScopeId>,
}

/// Inference state of the body being checked.
#[derive(Debug, Clone, Default)]
pub(crate) struct InferState {
    pub origins: Vec<InferOrigin>,
    /// A literal group checked against an unbound variable: whichever of
    /// the two is fixed first fixes the other.
    pub literal_links: Vec<(u32, TypeVarId)>,
    pub deferred: Vec<DeferredInstantiation>,
    pub holes: Vec<PendingHole>,
}

impl TypeChecker<'_> {
    /// A new variable standing for the generic parameter `param` of `item`,
    /// needed by the expression at `span`.
    pub(crate) fn fresh_type_var(
        &mut self,
        param: SymbolId,
        item: Option<SymbolId>,
        span: Span,
    ) -> TypeId {
        let id = self.type_info.type_interner.fresh_infer(InferKind::General);
        self.infer.origins.push(InferOrigin {
            span,
            param: Some(param),
            item,
        });
        id
    }

    /// A new variable for the result of the closure at `span`, which its
    /// body binds.
    pub(crate) fn fresh_result_var(&mut self, span: Span) -> TypeId {
        let id = self.type_info.type_interner.fresh_infer(InferKind::General);
        self.infer.origins.push(InferOrigin {
            span,
            param: None,
            item: None,
        });
        id
    }

    /// A literal group meets the unbound variable `var` (`v.push(1)`).
    pub(crate) fn link_literal_to_var(&mut self, var: u32, group: TypeVarId) {
        let kind = match self
            .literal_table
            .vars
            .get(group.0 as usize)
            .map(|v| v.kind)
        {
            Some(super::solver::LiteralKind::Float) => InferKind::Float,
            _ => InferKind::Int,
        };
        let interner = &self.type_info.type_interner;
        if interner.infer_kind(var) != InferKind::Float {
            interner.set_infer_kind(var, kind);
        }
        self.infer.literal_links.push((var, group));
    }

    /// Interface bounds of an instantiation whose arguments mention a
    /// variable are checked once the body has bound it.
    pub(crate) fn defer_instantiation(&mut self, params: &[SymbolId], args: &[TypeId], span: Span) {
        self.infer.deferred.push(DeferredInstantiation {
            params: params.to_vec(),
            args: args.to_vec(),
            span,
        });
    }

    /// An operand an operator needs the shape of. A variable takes the
    /// other operand's concrete type; with nothing concrete beside it, an
    /// integer or float variable takes its default width there, as a literal
    /// without context does. A general variable stays for the write-back.
    pub(crate) fn settle_operand(&mut self, id: TypeId, other: Option<TypeId>) -> TypeId {
        let ArType::Infer(var) = self.resolve(id) else {
            return id;
        };
        if let Some(other) = other
            && !matches!(
                self.resolve(other),
                ArType::Infer(_) | ArType::IntLiteral | ArType::FloatLiteral | ArType::Error
            )
            && self.unify_ids(id, other)
        {
            return id;
        }
        let interner = &self.type_info.type_interner;
        let default = match interner.infer_kind(var) {
            InferKind::Int => Primitive::Int,
            InferKind::Float => Primitive::Float,
            InferKind::General => return id,
        };
        interner.bind_infer(var, interner.intern(ArType::Primitive(default)));
        id
    }

    /// Lets literal groups and the variables they met agree before literals
    /// take their defaults: a fixed side fixes the other.
    pub(crate) fn sync_inference_literals(&mut self) {
        if self.infer.literal_links.is_empty() {
            return;
        }
        let links = std::mem::take(&mut self.infer.literal_links);
        for &(var, group) in &links {
            let interner = &self.type_info.type_interner;
            let var_ty = interner.intern(ArType::Infer(var));
            let resolved = resolve_deep(var_ty, interner);
            let root = self.literal_table.find_root(group);
            let concrete = self
                .literal_table
                .vars
                .get(root.0 as usize)
                .and_then(|v| v.concrete_type);
            match (self.resolve(resolved), concrete) {
                (ArType::Infer(_), Some(concrete)) => {
                    self.type_info.type_interner.bind_infer(var, concrete);
                }
                (ArType::Primitive(primitive), None) if primitive.is_numeric() => {
                    self.constrain_literal_var(
                        group,
                        resolved,
                        ConstraintOrigin::LiteralPromotion {
                            literal: String::new(),
                            literal_span: Span::new(0, 0, 0),
                            target_span: Span::new(0, 0, 0),
                        },
                    );
                }
                _ => {}
            }
        }
    }

    /// Makes the body's recorded types concrete; see the module docs.
    pub(crate) fn write_back_inference(&mut self) {
        if !self.type_info.type_interner.has_infer_vars() {
            self.report_holes();
            self.infer = InferState::default();
            return;
        }
        self.default_numeric_vars();
        // Before the recorded types settle: a goal still unknown is shown
        // as `_`, not reported again as T001.
        self.report_holes();
        let mut reported = Vec::new();
        // Recorded expression types, then declarations, then the tables
        // lowering reads.
        for index in 0..self.type_info.expr_types.len() {
            let Some(ty) = self.type_info.expr_types[index] else {
                continue;
            };
            let resolved = self.resolve_recorded(ty, &mut reported);
            if resolved != ty {
                self.type_info.expr_types[index] = Some(resolved);
                let span = self
                    .pool
                    .expr_span(arandu_parser::ast_pool::ExprId::new(index));
                self.type_info.demand_type(resolved, span);
            }
        }
        let mut declarations: Vec<(SymbolId, TypeId)> = self
            .type_info
            .decl_types
            .iter()
            .map(|(&s, &t)| (s, t))
            .collect();
        declarations.sort_unstable_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        for (symbol, ty) in declarations {
            let resolved = self.resolve_recorded(ty, &mut reported);
            if resolved != ty {
                self.type_info.decl_types.insert(symbol, resolved);
            }
        }
        self.write_back_tables(&mut reported);
        for deferred in std::mem::take(&mut self.infer.deferred) {
            let args: Vec<ArType> = deferred
                .args
                .iter()
                .map(|&arg| self.resolve(resolve_deep(arg, &self.type_info.type_interner)))
                .collect();
            if args.iter().any(|arg| matches!(arg, ArType::Infer(_))) {
                continue;
            }
            super::types::interfaces::check_instantiation_constraints(
                self,
                &deferred.params,
                &args,
                deferred.span,
            );
        }
        self.type_info.type_interner.reset_infer();
        self.infer = InferState::default();
    }

    /// Unbound integer and float variables take `int` and `float`, as the
    /// literals they met do.
    fn default_numeric_vars(&mut self) {
        let interner = &self.type_info.type_interner;
        for var in 0..interner.infer_var_count() {
            let id = interner.intern(ArType::Infer(var));
            let ArType::Infer(root) = interner.resolve(id) else {
                continue;
            };
            let default = match interner.infer_kind(root) {
                InferKind::Int => Primitive::Int,
                InferKind::Float => Primitive::Float,
                InferKind::General => continue,
            };
            interner.bind_infer(root, interner.intern(ArType::Primitive(default)));
        }
    }

    /// `ty` without variables. A variable still unbound is reported (once)
    /// and becomes `Error`, so nothing downstream cascades from it.
    fn resolve_recorded(&mut self, ty: TypeId, reported: &mut Vec<u32>) -> TypeId {
        let resolved = resolve_deep(ty, &self.type_info.type_interner);
        let unbound = unbound_vars(resolved, &self.type_info.type_interner);
        if unbound.is_empty() {
            return resolved;
        }
        for var in unbound {
            if !reported.contains(&var) {
                reported.push(var);
                self.report_unbound_var(var);
            }
            let error = self.intern(ArType::Error);
            self.type_info.type_interner.bind_infer(var, error);
        }
        resolve_deep(ty, &self.type_info.type_interner)
    }

    fn report_unbound_var(&mut self, var: u32) {
        let Some(origin) = self.infer.origins.get(var as usize).cloned() else {
            return;
        };
        let Some(param_symbol) = origin.param else {
            self.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::T001CannotInferType,
                    "cannot infer type: the result of this closure is never fixed",
                    origin.span,
                )
                .with_primary_label("nothing in the body fixes what it returns")
                .with_hint("give the binding a type, as in `let f: (int) -> int = ...`"),
            );
            return;
        };
        // A hole's own goal is part of its T102.
        if self.symbols.try_get(param_symbol).is_some_and(|param| {
            param.lang_item == Some(arandu_middle::symbol_table::LangItem::Hole)
        }) {
            return;
        }
        let param = self
            .symbols
            .try_get(param_symbol)
            .map_or_else(|| smol_str::SmolStr::new("T"), |param| param.name.clone());
        let item = origin
            .item
            .and_then(|item| self.symbols.try_get(item))
            .map(|item| item.name.clone());
        let message = match &item {
            Some(item) => {
                format!("cannot infer type: type annotation needed for `{param}` of `{item}`")
            }
            None => format!("cannot infer type: type annotation needed for `{param}`"),
        };
        self.diagnostics.push(
            crate::Diagnostic::error(crate::DiagCode::T001CannotInferType, message, origin.span)
                .with_label(origin.span, format!("`{param}` is not known here"))
                .with_hint(match &item {
                    Some(item) => format!(
                        "annotate the binding (`let v: {item}<int> = ...`) or pass the argument explicitly (`{item}<int>`)"
                    ),
                    None => "annotate the binding with its type".to_owned(),
                }),
        );
    }

    /// The tables lowering reads besides expression and declaration types.
    fn write_back_tables(&mut self, reported: &mut Vec<u32>) {
        let mut spans: Vec<_> = self.type_info.adjustments.keys().copied().collect();
        spans.sort_unstable_by_key(|span| (span.file_id, span.start, span.end));
        for span in spans {
            let Some(mut adjustment) = self.type_info.adjustments.get(&span).cloned() else {
                continue;
            };
            adjustment.from = self.resolve_recorded(adjustment.from, reported);
            adjustment.to = self.resolve_recorded(adjustment.to, reported);
            for arg in &mut adjustment.type_args {
                *arg = self.resolve_recorded(*arg, reported);
            }
            self.type_info.adjustments.insert(span, adjustment);
        }
        let mut spans: Vec<_> = self.type_info.closure_temporaries.keys().copied().collect();
        spans.sort_unstable_by_key(|span| (span.file_id, span.start, span.end));
        for span in spans {
            if let Some(mut temporary) = self.type_info.closure_temporaries.get(&span).copied() {
                temporary.ty = self.resolve_recorded(temporary.ty, reported);
                self.type_info.closure_temporaries.insert(span, temporary);
            }
        }
        let mut spans: Vec<_> = self.type_info.closures.keys().copied().collect();
        spans.sort_unstable_by_key(|span| (span.file_id, span.start, span.end));
        for span in spans {
            let Some(mut recipe) = self.type_info.closures.get(&span).cloned() else {
                continue;
            };
            recipe.return_type = self.resolve_recorded(recipe.return_type, reported);
            for param in &mut recipe.params {
                param.ty = self.resolve_recorded(param.ty, reported);
            }
            for field in &mut recipe.fields {
                field.ty = self.resolve_recorded(field.ty, reported);
                field.field_ty = self.resolve_recorded(field.field_ty, reported);
            }
            if let Some(code) = &mut recipe.owning {
                code.closure_ty = self.resolve_recorded(code.closure_ty, reported);
            }
            // The environment's field table carries the capture types too.
            if let Some(fields) = self
                .type_info
                .struct_fields
                .get(&recipe.struct_symbol)
                .cloned()
            {
                let mut fields = (*fields).clone();
                for field in &mut fields.fields {
                    field.ty = self.resolve_recorded(field.ty, reported);
                }
                self.type_info
                    .struct_fields
                    .insert(recipe.struct_symbol, std::sync::Arc::new(fields));
            }
            self.type_info.closures.insert(span, recipe);
        }
    }

    /// One `T102` per hole, in source order: its goal, its reason, and up to
    /// five locals in scope whose type fits, each a quick fix. A fill that is
    /// not `Copy` says it moves the local; none is applied automatically.
    fn report_holes(&mut self) {
        let mut holes = std::mem::take(&mut self.infer.holes);
        holes.sort_by_key(|hole| (hole.span.start, hole.span.end));
        for hole in holes {
            let interner = &self.type_info.type_interner;
            let goal = resolve_deep(hole.goal, interner);
            let goal_ty = self.resolve(goal);
            let known = !matches!(goal_ty, ArType::Infer(_) | ArType::Error)
                && unbound_vars(goal, interner).is_empty();
            let shown = goal_ty.display(&self.symbols, interner);
            let mut diagnostic = crate::Diagnostic::error(
                crate::DiagCode::T102TypedHole,
                format!("typed hole: expected '{shown}'"),
                hole.span,
            )
            .with_primary_label(if known {
                format!("a value of type '{shown}' goes here")
            } else {
                "nothing here fixes this value's type yet".to_owned()
            });
            if let Some(reason) = &hole.reason {
                diagnostic = diagnostic.with_note(format!("todo: {reason}"));
            }
            if known && !self.type_info.is_copy(goal) {
                diagnostic = diagnostic.with_note(format!(
                    "'{shown}' is not Copy: the value is moved into this place"
                ));
            }
            diagnostic = diagnostic.with_note(
                "a program with a hole is checked, but it does not build, run, test or evaluate at compile time",
            );
            if known {
                for (name, moves) in self.hole_fills(&hole, goal) {
                    let message = if moves {
                        format!("fill with `{name}` (moves `{name}`)")
                    } else {
                        format!("fill with `{name}`")
                    };
                    diagnostic = diagnostic.with_hint_replacement(arandu_middle::Hint {
                        message,
                        replacement: Some(arandu_middle::CodeReplacement {
                            span: hole.span,
                            new_text: name.to_string(),
                        }),
                    });
                }
            }
            self.diagnostics.push(diagnostic);
        }
    }

    /// Locals and parameters declared before the hole in the scopes around
    /// it, innermost first and most recent first, whose type is the goal.
    fn hole_fills(&self, hole: &PendingHole, goal: TypeId) -> Vec<(smol_str::SmolStr, bool)> {
        const LIMIT: usize = 5;
        let mut fills = Vec::new();
        let mut scope = hole.scope;
        let global = self.symbols.global_scope();
        let interner = &self.type_info.type_interner;
        while let Some(current) = scope {
            if current == global {
                break;
            }
            let entry = self.symbols.scope(current);
            for &symbol in entry.symbols().iter().rev() {
                let Some(local) = self.symbols.try_get(symbol) else {
                    continue;
                };
                if !matches!(
                    local.kind,
                    crate::SymbolKind::Local | crate::SymbolKind::Param
                ) || local.span.start >= hole.span.start
                    || fills.iter().any(|(name, _)| *name == local.name)
                {
                    continue;
                }
                let Some(ty) = self.type_info.decl_type_id(symbol) else {
                    continue;
                };
                let ty = resolve_deep(ty, interner);
                if !unbound_vars(ty, interner).is_empty() {
                    continue;
                }
                if ty == goal
                    || super::types::unify(&interner.resolve(ty), &interner.resolve(goal), interner)
                        && !matches!(interner.resolve(ty), ArType::Error)
                {
                    fills.push((local.name.clone(), !self.type_info.is_copy(goal)));
                    if fills.len() == LIMIT {
                        return fills;
                    }
                }
            }
            scope = entry.parent;
        }
        fills
    }
}
