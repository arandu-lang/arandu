use arandu_lexer::Span;
use arandu_parser::ast_pool::ExprId;
use rustc_hash::FxHashMap;

use super::constraints::{Constraint, ConstraintOrigin};
use super::types::{ArType, Primitive, TypeId};
use super::{TypeChecker, errors};
use crate::SymbolId;

// ── Solver (roadmap 4.1 & TYP.3.3) ────────────────────────────────────

/// Unique identifier of a literal type variable in the solver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVarId(pub u32);

/// The literal category (integer or floating point).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralKind {
    Int,
    Float,
}

/// An occurrence of a literal in the source code.
#[derive(Debug, Clone)]
pub struct LiteralOccurrence {
    pub expr: ExprId,
    pub span: Span,
    pub raw: String,
}

/// A node in the disjoint-set union (DSU) graph of literal type variables.
#[derive(Debug, Clone)]
pub struct LiteralVar {
    pub id: TypeVarId,
    pub parent: TypeVarId,
    pub rank: u32,
    pub kind: LiteralKind,
    pub concrete_type: Option<TypeId>,
    pub literals: Vec<LiteralOccurrence>,
    pub symbols: Vec<SymbolId>,
    pub exprs: Vec<ExprId>,
}

/// Disjoint-set union (DSU) table tracking late constraints on literal expressions.
#[derive(Debug, Default, Clone)]
pub struct LiteralTable {
    pub(crate) vars: Vec<LiteralVar>,
    pub(crate) symbol_to_var: FxHashMap<SymbolId, TypeVarId>,
    pub(crate) expr_to_var: FxHashMap<ExprId, TypeVarId>,
}

impl LiteralTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.vars.clear();
        self.symbol_to_var.clear();
        self.expr_to_var.clear();
    }

    pub fn alloc(&mut self, kind: LiteralKind, occurrence: Option<LiteralOccurrence>) -> TypeVarId {
        let id = TypeVarId(self.vars.len() as u32);
        let mut literals = Vec::new();
        let mut exprs = Vec::new();
        if let Some(occ) = occurrence {
            exprs.push(occ.expr);
            self.expr_to_var.insert(occ.expr, id);
            literals.push(occ);
        }
        self.vars.push(LiteralVar {
            id,
            parent: id,
            rank: 0,
            kind,
            concrete_type: None,
            literals,
            symbols: Vec::new(),
            exprs,
        });
        id
    }

    pub fn find_root(&mut self, var: TypeVarId) -> TypeVarId {
        let mut root = var;
        while self.vars[root.0 as usize].parent != root {
            root = self.vars[root.0 as usize].parent;
        }
        let mut curr = var;
        while curr != root {
            let nxt = self.vars[curr.0 as usize].parent;
            self.vars[curr.0 as usize].parent = root;
            curr = nxt;
        }
        root
    }

    #[must_use]
    pub fn find_root_imm(&self, var: TypeVarId) -> TypeVarId {
        let mut root = var;
        while self.vars[root.0 as usize].parent != root {
            root = self.vars[root.0 as usize].parent;
        }
        root
    }

    pub fn unify(&mut self, a: TypeVarId, b: TypeVarId) -> TypeVarId {
        let root_a = self.find_root(a);
        let root_b = self.find_root(b);
        if root_a == root_b {
            return root_a;
        }

        // Float absorbs Int (safe literal coercion rule)
        let kind = match (
            self.vars[root_a.0 as usize].kind,
            self.vars[root_b.0 as usize].kind,
        ) {
            (LiteralKind::Float, _) | (_, LiteralKind::Float) => LiteralKind::Float,
            _ => LiteralKind::Int,
        };

        // Both roots may already carry a concrete type if they were pinned
        // independently. A genuine mismatch (e.g. `u8` vs `u16`) is reported by
        // the binary operator's `unify_ids` check before this merge; keeping the
        // first here avoids dropping a type already propagated to symbols.
        let concrete_type = self.vars[root_a.0 as usize]
            .concrete_type
            .or(self.vars[root_b.0 as usize].concrete_type);

        let rank_a = self.vars[root_a.0 as usize].rank;
        let rank_b = self.vars[root_b.0 as usize].rank;

        let (parent, child) = if rank_a >= rank_b {
            (root_a, root_b)
        } else {
            (root_b, root_a)
        };

        self.vars[child.0 as usize].parent = parent;
        if rank_a == rank_b {
            self.vars[parent.0 as usize].rank += 1;
        }
        self.vars[parent.0 as usize].kind = kind;
        self.vars[parent.0 as usize].concrete_type = concrete_type;

        let mut child_literals = std::mem::take(&mut self.vars[child.0 as usize].literals);
        self.vars[parent.0 as usize]
            .literals
            .append(&mut child_literals);

        let mut child_symbols = std::mem::take(&mut self.vars[child.0 as usize].symbols);
        self.vars[parent.0 as usize]
            .symbols
            .append(&mut child_symbols);

        let mut child_exprs = std::mem::take(&mut self.vars[child.0 as usize].exprs);
        self.vars[parent.0 as usize].exprs.append(&mut child_exprs);

        parent
    }

    pub fn bind_symbol(&mut self, sym: SymbolId, var: TypeVarId) {
        let root = self.find_root(var);
        self.vars[root.0 as usize].symbols.push(sym);
        self.symbol_to_var.insert(sym, root);
    }

    pub fn bind_expr(&mut self, expr: ExprId, var: TypeVarId) {
        let root = self.find_root(var);
        self.vars[root.0 as usize].exprs.push(expr);
        self.expr_to_var.insert(expr, root);
    }

    /// Mark the literal occurrence for `literal_expr` as negated by an
    /// enclosing `UnaryOp::Neg` (e.g. the `128` in `-128`). Retroactive range
    /// checks (T038) then validate the *signed* value instead of the raw
    /// positive lexeme, so `-128` fits `i8` while `-129` does not.
    pub fn negate_occurrence(&mut self, var: TypeVarId, literal_expr: ExprId) {
        let root = self.find_root(var);
        for occ in &mut self.vars[root.0 as usize].literals {
            if occ.expr == literal_expr {
                if !occ.raw.starts_with('-') {
                    occ.raw.insert(0, '-');
                }
                return;
            }
        }
    }

    #[must_use]
    pub fn var_for_symbol(&self, sym: SymbolId) -> Option<TypeVarId> {
        self.symbol_to_var.get(&sym).map(|&v| self.find_root_imm(v))
    }

    #[must_use]
    pub fn var_for_expr(&self, expr: ExprId) -> Option<TypeVarId> {
        self.expr_to_var.get(&expr).map(|&v| self.find_root_imm(v))
    }

    /// The promoted integer root involved in a would-be widening merge with a
    /// float group, if any. Callers check this before merging so they can
    /// report T015 instead of silently turning the variable float.
    #[must_use]
    pub fn promoted_int_root(&self, a: TypeVarId, b: TypeVarId) -> Option<TypeVarId> {
        let ra = self.find_root_imm(a);
        let rb = self.find_root_imm(b);
        let va = &self.vars[ra.0 as usize];
        let vb = &self.vars[rb.0 as usize];
        if va.kind == LiteralKind::Int && !va.symbols.is_empty() && vb.kind == LiteralKind::Float {
            Some(ra)
        } else if vb.kind == LiteralKind::Int
            && !vb.symbols.is_empty()
            && va.kind == LiteralKind::Float
        {
            Some(rb)
        } else {
            None
        }
    }
}

/// A constraint failure that has been through the solver, in generation
/// order.
#[derive(Debug, Clone)]
pub struct SolvedConstraint {
    /// The failed constraint as generated; both types are interned in
    /// `TypeInfo::type_interner`.
    pub constraint: Constraint,
    /// Index of the rendered diagnostic inside [`TypeChecker::diagnostics`].
    pub diag_index: usize,
}

/// Solver entry point: render `constraint` into a diagnostic, push it onto
/// `tc.diagnostics` and record the solve in generation order.
pub(crate) fn fail_constraint(tc: &mut TypeChecker<'_>, constraint: Constraint) {
    let diag = errors::constraint_to_diagnostic(&constraint, &tc.symbols, &tc.type_info);
    let diag_index = tc.diagnostics.len();
    tc.diagnostics.push(diag);
    tc.solved_constraints.push(SolvedConstraint {
        constraint,
        diag_index,
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstrainResult {
    Ok,
    Incompatible,
    OutOfRange,
}

impl TypeChecker<'_> {
    /// All constraint failures solved so far, oldest first. Indices align
    /// with generation order, not with `diagnostics` filtering.
    #[must_use]
    pub fn solved_constraints(&self) -> &[SolvedConstraint] {
        &self.solved_constraints
    }

    /// Constrain a literal type variable group to a concrete type `concrete_id`.
    /// Retroactively validates bounds with `TargetInfo` (emitting `T038IntegerLiteralOutOfRange`)
    /// and propagates the concrete type to all bound variables and expressions.
    pub fn constrain_literal_var(
        &mut self,
        var: TypeVarId,
        concrete_id: TypeId,
        origin: ConstraintOrigin,
    ) -> ConstrainResult {
        let root = self.literal_table.find_root(var);
        let target_ty = self.resolve(concrete_id);

        if target_ty.is_literal() || target_ty.is_error() {
            return ConstrainResult::Ok;
        }

        let promoted = !self.literal_table.vars[root.0 as usize].symbols.is_empty();
        let kind = self.literal_table.vars[root.0 as usize].kind;
        // A raw integer literal may be coerced to float (`let x: float = 1`).
        // Once promoted to a variable (`let a = 1`), it is an `int` binding and
        // widening it to float is an implicit widening (T015), not a free
        // literal coercion.
        let compatible = match kind {
            LiteralKind::Int => match target_ty {
                ArType::Primitive(p) => p.is_integer() || (!promoted && p.is_float()),
                _ => false,
            },
            LiteralKind::Float => match target_ty {
                ArType::Primitive(p) => p.is_float(),
                _ => false,
            },
        };

        if !compatible {
            // Report only when the group is still unpinned. If it already has
            // a concrete type, the caller's assignment path sees a non-literal
            // type and emits the widening itself, so this avoids a duplicate.
            if kind == LiteralKind::Int
                && promoted
                && self.literal_table.vars[root.0 as usize]
                    .concrete_type
                    .is_none()
                && matches!(target_ty, ArType::Primitive(p) if p.is_float())
            {
                self.report_promoted_literal_widening(root, concrete_id, &origin);
            }
            return ConstrainResult::Incompatible;
        }

        if let Some(existing_id) = self.literal_table.vars[root.0 as usize].concrete_type {
            return if existing_id == concrete_id {
                ConstrainResult::Ok
            } else {
                ConstrainResult::Incompatible
            };
        }

        self.literal_table.vars[root.0 as usize].concrete_type = Some(concrete_id);

        let mut out_of_range = false;
        // Retroactive range check on original literal occurrences (T038)
        if let ArType::Primitive(p) = target_ty
            && p.is_integer()
        {
            let literals = self.literal_table.vars[root.0 as usize].literals.clone();
            for occ in literals {
                if let Some(parsed) = arandu_middle::literal_pool::parse_int_literal(&occ.raw) {
                    let fits = match p {
                        Primitive::I8 => (i8::MIN as i128..=i8::MAX as i128).contains(&parsed),
                        Primitive::I16 => (i16::MIN as i128..=i16::MAX as i128).contains(&parsed),
                        Primitive::I32 => (i32::MIN as i128..=i32::MAX as i128).contains(&parsed),
                        Primitive::I64 => (i64::MIN as i128..=i64::MAX as i128).contains(&parsed),
                        Primitive::Int => (self.target_info.int_min()..=self.target_info.int_max())
                            .contains(&parsed),
                        Primitive::U8 | Primitive::Byte => (0..=u8::MAX as i128).contains(&parsed),
                        Primitive::U16 => (0..=u16::MAX as i128).contains(&parsed),
                        Primitive::U32 => (0..=u32::MAX as i128).contains(&parsed),
                        Primitive::U64 => parsed >= 0 && (parsed as u128 <= u64::MAX as u128),
                        Primitive::Uint => {
                            parsed >= 0 && (parsed as u128 <= self.target_info.uint_max())
                        }
                        _ => true,
                    };
                    if !fits {
                        out_of_range = true;
                        let origin_target = origin.target_span();
                        let target_span = if origin_target == Span::new(0, 0, 0) {
                            occ.span
                        } else {
                            origin_target
                        };
                        // Route through the solver so the failure is recorded
                        // as a solved constraint (causal provenance, 4.2) and
                        // rendered with the current diagnostic policy.
                        self.add_constraint(
                            concrete_id,
                            ArType::IntLiteral,
                            ConstraintOrigin::LiteralPromotion {
                                literal: occ.raw.clone(),
                                literal_span: occ.span,
                                target_span,
                            },
                        );
                    }
                }
            }
        }

        // Update all symbols bound to this group
        let symbols = self.literal_table.vars[root.0 as usize].symbols.clone();
        for sym in symbols {
            self.ctx.bind(sym, concrete_id);
            self.record_decl_type(sym, concrete_id);
        }

        // Update all expressions bound to this group
        let exprs = self.literal_table.vars[root.0 as usize].exprs.clone();
        for expr in exprs {
            self.record_expr_type(expr, concrete_id);
        }

        if out_of_range {
            ConstrainResult::OutOfRange
        } else {
            ConstrainResult::Ok
        }
    }

    /// Emit T015 for a promoted integer literal being widened to float.
    ///
    /// The origin points the diagnostic at the type that demanded the float
    /// type (annotation, parameter, field, …); the source points at the
    /// literal that promoted the variable.
    fn report_promoted_literal_widening(
        &mut self,
        root: TypeVarId,
        float_id: TypeId,
        origin: &ConstraintOrigin,
    ) {
        let source_span = self.literal_table.vars[root.0 as usize]
            .literals
            .first()
            .map_or(Span::new(0, 0, 0), |occ| occ.span);
        let origin_target = origin.target_span();
        let target_span = if origin_target == Span::new(0, 0, 0) {
            source_span
        } else {
            origin_target
        };
        let int_id = self.intern(ArType::Primitive(Primitive::Int));
        self.add_constraint(
            float_id,
            int_id,
            ConstraintOrigin::ImplicitWidening {
                source_span,
                target_span,
            },
        );
    }

    /// If merging the literal groups of `a` and `b` would implicitly widen a
    /// promoted integer literal to float, emit T015 and return `true`.
    ///
    /// Used by binary operators and array literals before they merge the
    /// groups. The source points at the literal that promoted the variable;
    /// `target_span` is the operation/array demanding a single type.
    pub(crate) fn report_promoted_widening(
        &mut self,
        a: TypeVarId,
        b: TypeVarId,
        target_span: Span,
    ) -> bool {
        let Some(int_root) = self.literal_table.promoted_int_root(a, b) else {
            return false;
        };
        let source_span = self.literal_table.vars[int_root.0 as usize]
            .literals
            .first()
            .map_or(target_span, |occ| occ.span);
        let int_id = self.intern(ArType::Primitive(Primitive::Int));
        let float_id = self.intern(ArType::Primitive(Primitive::Float));
        self.add_constraint(
            float_id,
            int_id,
            ConstraintOrigin::ImplicitWidening {
                source_span,
                target_span,
            },
        );
        true
    }

    /// Anti-ambiguity finalization (TYP.3.3):
    /// Defaults unconstrained integer literals to native pointer-width `int`,
    /// and float literals to `float`, never silently defaulting to `i32`.
    pub fn finalize_literal_vars(&mut self) {
        let count = self.literal_table.vars.len();
        for idx in 0..count {
            if self.literal_table.vars[idx].parent.0 != idx as u32 {
                continue;
            }
            if self.literal_table.vars[idx].concrete_type.is_some() {
                continue;
            }
            let kind = self.literal_table.vars[idx].kind;
            let default_ty = match kind {
                LiteralKind::Int => ArType::Primitive(Primitive::Int),
                LiteralKind::Float => ArType::Primitive(Primitive::Float),
            };
            let default_id = self.intern(default_ty);
            self.constrain_literal_var(
                TypeVarId(idx as u32),
                default_id,
                ConstraintOrigin::LiteralPromotion {
                    literal: String::new(),
                    literal_span: Span::new(0, 0, 0),
                    target_span: Span::new(0, 0, 0),
                },
            );
        }
        self.literal_table.clear();
    }
}
