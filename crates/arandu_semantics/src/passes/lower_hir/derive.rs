//! HIR synthesis for built-in `@Derive(PartialEq, Eq, Ord, Clone, Hash, Debug)` methods.

use std::sync::Arc;

use arandu_lexer::Span;
use arandu_middle::derive::DeriveMethodKind;
use arandu_middle::ops::{BinaryOp, SetOp, UnaryOp};
use arandu_middle::symbol_table::LangItem;
use arandu_middle::types::{ArType, GenericSubst, Primitive, TypeId, substitute_type};
use arandu_middle::{Symbol, SymbolId, SymbolKind, Visibility};
use arandu_typeck::DeriveRecipe;
use arandu_typeck::type_checker::info::EnumPayloadShape;
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

use crate::TypeCheckResult;
use crate::diagnostics::{DiagCode, Diagnostic, Severity};
use crate::hir::{
    HirBindingItem, HirBlock, HirBlockId, HirCondition, HirDecl, HirDeclId, HirExpr, HirExprId,
    HirExprKind, HirFieldInit, HirFunc, HirMatchArm, HirMatchArmBody, HirParam, HirPattern,
    HirPlace, HirProgram, HirStmt, HirStmtId, HirStmtKind, ReceiverKind, ResultCtorVariant,
};

struct DeriveBuilder<'a> {
    tc: &'a mut TypeCheckResult,
    pool: &'a mut crate::hir::HirPool,
    recipe: &'a DeriveRecipe,
    span: Span,
    next_local_role: u32,
}

/// Lowers one derived method to a standalone [`HirProgram`] for per-item `function_hir`.
pub fn lower_derived_method_hir(
    type_check: &mut TypeCheckResult,
    method_symbol: SymbolId,
) -> Result<Option<HirProgram>, Vec<Diagnostic>> {
    if type_check
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return Err(type_check.diagnostics.clone());
    }
    let Some(recipe) = type_check
        .type_info
        .derived_methods
        .get(&method_symbol)
        .cloned()
    else {
        return Ok(None);
    };
    let mut hir_pool = crate::hir::HirPool::new();
    let hir_func =
        build_derived_hir_func(type_check, &mut hir_pool, &recipe, true).map_err(|d| vec![d])?;
    let decl_id = hir_pool.alloc_decl(HirDecl::Func(hir_func));
    Ok(Some(HirProgram {
        span: recipe.span,
        module: None,
        decls: vec![decl_id],
        pool: hir_pool,
    }))
}

/// Appends header-only [`HirFunc`] declarations (`body: None`) for derived methods owned by
/// types in `owner_types`. Used by `lower_declarations_to_hir`.
pub(crate) fn lower_derived_decl_headers(
    type_check: &TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    decls: &mut Vec<HirDeclId>,
    owner_types: &FxHashSet<SymbolId>,
) {
    let mut recipes: Vec<&DeriveRecipe> = type_check
        .type_info
        .derived_methods
        .values()
        .filter(|recipe| owner_types.contains(&recipe.type_symbol))
        .collect();
    recipes.sort_by_key(|r| (r.method_symbol.file_id, r.method_symbol.local_id.0));
    for recipe in recipes {
        let params = build_hir_params(hir_pool, recipe);
        decls.push(hir_pool.alloc_decl(HirDecl::Func(HirFunc {
            symbol: recipe.method_symbol,
            params,
            return_type: recipe.return_ty,
            body: None,
            span: recipe.span,
            is_async: false,
            no_fallback: false,
        })));
    }
}

/// Appends full derived method definitions for `file_id` during whole-file [`super::lower_to_hir`].
pub(crate) fn lower_derived_methods_for_file(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    decls: &mut Vec<HirDeclId>,
    file_id: u32,
) -> Result<(), Diagnostic> {
    let mut recipes: Vec<DeriveRecipe> = type_check
        .type_info
        .derived_methods
        .values()
        .filter(|recipe| recipe.method_symbol.file_id == file_id)
        .cloned()
        .collect();
    recipes.sort_by_key(|r| r.method_symbol.local_id.0);
    for recipe in recipes {
        let func = build_derived_hir_func(type_check, hir_pool, &recipe, true)?;
        decls.push(hir_pool.alloc_decl(HirDecl::Func(func)));
    }
    Ok(())
}

fn build_hir_params(
    hir_pool: &mut crate::hir::HirPool,
    recipe: &DeriveRecipe,
) -> crate::hir::IndexRange {
    let mut params = Vec::with_capacity(recipe.param_symbols.len());
    if let Some(&self_sym) = recipe.param_symbols.first() {
        params.push(HirParam {
            symbol: self_sym,
            ty: recipe.self_param_ty,
            span: recipe.span,
            is_receiver: true,
            receiver_kind: Some(ReceiverKind::Shared),
        });
    }
    for (idx, &ty) in recipe.explicit_param_tys.iter().enumerate() {
        if let Some(&sym) = recipe.param_symbols.get(idx + 1) {
            params.push(HirParam {
                symbol: sym,
                ty,
                span: recipe.span,
                is_receiver: false,
                receiver_kind: None,
            });
        }
    }
    hir_pool.alloc_param_list(&params)
}

fn build_derived_hir_func(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    recipe: &DeriveRecipe,
    include_body: bool,
) -> Result<HirFunc, Diagnostic> {
    let params = build_hir_params(hir_pool, recipe);
    let body = if include_body {
        let mut b = DeriveBuilder {
            tc: type_check,
            pool: hir_pool,
            recipe,
            span: recipe.span,
            next_local_role: 0x4000,
        };
        Some(match recipe.kind {
            DeriveMethodKind::Eq => b.build_eq_body()?,
            DeriveMethodKind::Cmp => b.build_cmp_body()?,
            DeriveMethodKind::Clone => b.build_clone_body()?,
            DeriveMethodKind::Hash => b.build_hash_body()?,
            DeriveMethodKind::Debug => b.build_debug_body()?,
        })
    } else {
        None
    };
    Ok(HirFunc {
        symbol: recipe.method_symbol,
        params,
        return_type: recipe.return_ty,
        body,
        span: recipe.span,
        is_async: false,
        no_fallback: false,
    })
}

#[derive(Clone)]
struct FieldSpec {
    name: SmolStr,
    ty: TypeId,
}

#[derive(Clone)]
struct VariantSpec {
    symbol: SymbolId,
    name: SmolStr,
    tag: usize,
    payload_tys: Vec<TypeId>,
}

impl<'a> DeriveBuilder<'a> {
    fn bool_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::Bool))
    }

    fn int_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::Int))
    }

    fn i64_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::I64))
    }

    fn u64_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::U64))
    }

    fn u8_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::U8))
    }

    fn u16_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::U16))
    }

    fn u32_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::U32))
    }

    fn usize_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::USize))
    }

    fn str_ty(&mut self) -> TypeId {
        self.intern(ArType::Primitive(Primitive::Str))
    }

    fn void_ty(&mut self) -> TypeId {
        self.intern(ArType::Void)
    }

    fn intern(&mut self, ty: ArType) -> TypeId {
        self.tc.type_info_mut().type_interner.intern(ty)
    }

    fn resolve(&self, ty: TypeId) -> ArType {
        self.tc.type_info.type_interner.resolve(ty)
    }

    fn subst_for_method(&self) -> GenericSubst {
        let mut subst = GenericSubst::default();
        let struct_gps = self
            .tc
            .type_info
            .generic_params
            .get(&self.recipe.type_symbol)
            .cloned();
        let method_gps = self
            .tc
            .type_info
            .generic_params
            .get(&self.recipe.method_symbol)
            .cloned();
        if let (Some(sgp), Some(mgp)) = (struct_gps, method_gps) {
            for (&orig, &meth) in sgp.iter().zip(mgp.iter()) {
                let mty = ArType::named(meth, &[], &self.tc.type_info.type_interner);
                subst.push((orig, mty));
            }
        }
        subst
    }

    fn instantiate_type(&mut self, ty: TypeId, subst: &GenericSubst) -> TypeId {
        if subst.is_empty() {
            return ty;
        }
        let resolved = self.resolve(ty);
        let substituted = substitute_type(&resolved, subst, &self.tc.type_info.type_interner);
        self.intern(substituted)
    }

    fn is_phantom_data(&self, ty: TypeId) -> bool {
        matches!(
            self.resolve(ty),
            ArType::Named(sym, _)
                if self
                    .tc
                    .symbols
                    .try_get(sym)
                    .is_some_and(|s| s.lang_item == Some(LangItem::PhantomData))
        )
    }

    fn is_enum(&self) -> bool {
        self.tc
            .symbols
            .try_get(self.recipe.type_symbol)
            .is_some_and(|s| s.kind == SymbolKind::Enum)
    }

    fn struct_fields(&mut self) -> Vec<FieldSpec> {
        let subst = self.subst_for_method();
        let Some(fields) = self
            .tc
            .type_info
            .struct_fields
            .get(&self.recipe.type_symbol)
            .cloned()
        else {
            return Vec::new();
        };
        fields
            .iter()
            .map(|f| FieldSpec {
                name: f.name.clone(),
                ty: self.instantiate_type(f.ty, &subst),
            })
            .collect()
    }

    fn enum_variants(&mut self) -> Vec<VariantSpec> {
        let subst = self.subst_for_method();
        let mut variants: Vec<_> = self
            .tc
            .type_info
            .enum_variants
            .iter()
            .filter_map(|(&var_sym, (parent, shape))| {
                if *parent != self.recipe.type_symbol {
                    return None;
                }
                let tag = self
                    .tc
                    .type_info
                    .enum_variant_tags
                    .get(&var_sym)
                    .copied()
                    .unwrap_or(0);
                let full_name = self.tc.symbols.get(var_sym).name.clone();
                let short_name = full_name
                    .rsplit('.')
                    .next()
                    .map_or_else(|| full_name.clone(), SmolStr::new);
                let raw_payload = match shape {
                    EnumPayloadShape::Unit => Vec::new(),
                    EnumPayloadShape::Tuple(items) => items.clone(),
                };
                Some((var_sym, short_name, tag, raw_payload))
            })
            .collect();
        variants.sort_by_key(|(_, _, tag, sym_order_fix)| (*tag, sym_order_fix.len()));
        variants
            .into_iter()
            .map(|(symbol, name, tag, raw_payload)| {
                let payload_tys = raw_payload
                    .into_iter()
                    .map(|ty| self.instantiate_type(ty, &subst))
                    .collect();
                VariantSpec {
                    symbol,
                    name,
                    tag,
                    payload_tys,
                }
            })
            .collect()
    }

    fn mint_local(&mut self, name: &str, ty: TypeId) -> SymbolId {
        let file_id = self.recipe.method_symbol.file_id;
        let global_scope = self.tc.symbols.global_scope();
        loop {
            let role = self.next_local_role;
            self.next_local_role = self.next_local_role.wrapping_add(1);
            let mixed = self
                .recipe
                .method_symbol
                .local_id
                .0
                .wrapping_mul(0x9E37_79B1)
                ^ role.wrapping_mul(0x85EB_CA6B);
            let local = SymbolId::SYNTHETIC_LOCAL_BIT | (mixed & 0x7FFF_FFFF);
            let id = SymbolId::new(file_id, local);
            if self.tc.symbols.try_get(id).is_none() {
                Arc::make_mut(&mut self.tc.symbols).register_synthetic(Symbol {
                    id,
                    name: SmolStr::new(name),
                    kind: SymbolKind::Local,
                    span: self.span,
                    scope: global_scope,
                    visibility: Visibility::Private,
                    lang_item: None,
                });
                self.tc.type_info_mut().decl_types.insert(id, ty);
                return id;
            }
        }
    }

    fn expr(&mut self, kind: HirExprKind, ty: TypeId) -> HirExprId {
        self.pool.alloc_expr(HirExpr {
            kind,
            ty,
            span: self.span,
        })
    }

    fn stmt(&mut self, kind: HirStmtKind) -> HirStmtId {
        self.pool.alloc_stmt(HirStmt {
            kind,
            span: self.span,
        })
    }

    fn block(&mut self, stmts: Vec<HirStmtId>) -> HirBlockId {
        let statements = self.pool.alloc_stmt_list(&stmts);
        self.pool.alloc_block(HirBlock {
            statements,
            span: self.span,
        })
    }

    fn return_expr(&mut self, expr: HirExprId) -> HirStmtId {
        let values = self.pool.alloc_expr_list(&[expr]);
        self.stmt(HirStmtKind::Return { values })
    }

    fn path(&mut self, symbol: SymbolId, ty: TypeId) -> HirExprId {
        self.expr(HirExprKind::Path { symbol }, ty)
    }

    fn self_expr(&mut self) -> HirExprId {
        self.path(self.recipe.param_symbols[0], self.recipe.self_param_ty)
    }

    fn other_expr(&mut self) -> HirExprId {
        self.path(
            self.recipe.param_symbols[1],
            self.recipe.explicit_param_tys[0],
        )
    }

    fn second_param_expr(&mut self) -> HirExprId {
        self.path(
            self.recipe.param_symbols[1],
            self.recipe.explicit_param_tys[0],
        )
    }

    fn field_access(&mut self, base: HirExprId, name: &str, ty: TypeId) -> HirExprId {
        self.expr(
            HirExprKind::Field {
                base,
                field: SmolStr::new(name),
            },
            ty,
        )
    }

    fn as_ref_arg(&mut self, val: HirExprId, val_ty: TypeId) -> HirExprId {
        match self.resolve(val_ty) {
            ArType::Ref(_) | ArType::RefMut(_) => val,
            _ => {
                let ref_ty = self.intern(ArType::Ref(val_ty));
                self.expr(
                    HirExprKind::Unary {
                        op: UnaryOp::Ref,
                        expr: val,
                    },
                    ref_ty,
                )
            }
        }
    }

    fn deref_val(&mut self, val: HirExprId, val_ty: TypeId) -> (HirExprId, TypeId) {
        match self.resolve(val_ty) {
            ArType::Ref(inner) | ArType::RefMut(inner) => (
                self.expr(
                    HirExprKind::Unary {
                        op: UnaryOp::Deref,
                        expr: val,
                    },
                    inner,
                ),
                inner,
            ),
            _ => (val, val_ty),
        }
    }

    fn call_method(
        &mut self,
        recv: HirExprId,
        recv_ty: TypeId,
        method_name: &str,
        extra_args: Vec<HirExprId>,
        extra_arg_tys: Vec<TypeId>,
        ret_ty: TypeId,
    ) -> HirExprId {
        // Resolve concrete associated method symbol when receiver isn't a bare TypeParam.
        let peel = |this: &Self, ty: TypeId| -> Option<SymbolId> {
            let mut cur = this.resolve(ty);
            for _ in 0..4 {
                match cur {
                    ArType::Ref(i) | ArType::RefMut(i) | ArType::Ptr(i) => cur = this.resolve(i),
                    ArType::Named(sym, _) => {
                        let is_tp = this
                            .tc
                            .symbols
                            .try_get(sym)
                            .is_some_and(|s| s.kind == SymbolKind::TypeParam);
                        return (!is_tp).then_some(sym);
                    }
                    ArType::Primitive(Primitive::Str) => return this.tc.symbols.builtins.str_type,
                    _ => return None,
                }
            }
            None
        };
        let assoc_sym = peel(self, recv_ty)
            .and_then(|owner| self.tc.symbols.lookup_associated_member(owner, method_name));

        let callee_expr = if let Some(sym) = assoc_sym {
            let sig_ty = self.tc.type_info.decl_type_id(sym).unwrap_or_else(|| {
                let mut all_tys = vec![recv_ty];
                all_tys.extend_from_slice(&extra_arg_tys);
                let f = ArType::func(&all_tys, ret_ty, &self.tc.type_info.type_interner);
                self.intern(f)
            });
            self.expr(HirExprKind::Path { symbol: sym }, sig_ty)
        } else {
            let mut all_tys = vec![recv_ty];
            all_tys.extend_from_slice(&extra_arg_tys);
            let sig_ty = self.intern(ArType::func(
                &all_tys,
                ret_ty,
                &self.tc.type_info.type_interner,
            ));
            self.expr(
                HirExprKind::Field {
                    base: recv,
                    field: SmolStr::new(method_name),
                },
                sig_ty,
            )
        };

        let mut args = Vec::with_capacity(1 + extra_args.len());
        args.push(recv);
        args.extend(extra_args);
        let args_range = self.pool.alloc_expr_list(&args);
        self.expr(
            HirExprKind::Call {
                callee: callee_expr,
                args: args_range,
            },
            ret_ty,
        )
    }

    fn bool_lit(&mut self, v: bool) -> HirExprId {
        let bty = self.bool_ty();
        self.expr(HirExprKind::Bool(v), bty)
    }

    fn str_lit(&mut self, text: &str) -> HirExprId {
        let sty = self.str_ty();
        self.expr(HirExprKind::Str(SmolStr::new(text)), sty)
    }

    fn enum_ctor(
        &mut self,
        variant_sym: SymbolId,
        args: Vec<HirExprId>,
        enum_ty: TypeId,
    ) -> HirExprId {
        let type_symbol = self.recipe.type_symbol;
        let path_ty = self
            .tc
            .type_info
            .decl_type_id(variant_sym)
            .unwrap_or(enum_ty);
        let path_expr = self.expr(
            HirExprKind::TypePath {
                type_symbol,
                member_symbol: variant_sym,
            },
            if args.is_empty() { enum_ty } else { path_ty },
        );
        if args.is_empty() {
            path_expr
        } else {
            let args_range = self.pool.alloc_expr_list(&args);
            self.expr(
                HirExprKind::Call {
                    callee: path_expr,
                    args: args_range,
                },
                enum_ty,
            )
        }
    }

    fn ordering_variant(&mut self, variant_item: LangItem) -> Result<HirExprId, Diagnostic> {
        let Some(ord_sym) = self.tc.symbols.lang_item_symbol(LangItem::Ordering) else {
            return Err(Diagnostic::ice(
                DiagCode::ICEL001,
                "missing std.core.cmp.Ordering lang item during derive lowering",
                self.span,
            ));
        };
        let Some(var_sym) = self.tc.symbols.lang_item_symbol(variant_item) else {
            return Err(Diagnostic::ice(
                DiagCode::ICEL001,
                "missing Ordering variant lang item during derive lowering",
                self.span,
            ));
        };
        let ord_ty = self.intern(ArType::named(
            ord_sym,
            &[],
            &self.tc.type_info.type_interner,
        ));
        Ok(self.expr(
            HirExprKind::TypePath {
                type_symbol: ord_sym,
                member_symbol: var_sym,
            },
            ord_ty,
        ))
    }

    // ── PartialEq / Eq ───────────────────────────────────────────────

    fn eq_values(&mut self, lhs: HirExprId, rhs: HirExprId, val_ty: TypeId) -> HirExprId {
        let bool_ty = self.bool_ty();
        let (l_val, l_inner) = self.deref_val(lhs, val_ty);
        let (r_val, _) = self.deref_val(rhs, val_ty);
        match self.resolve(l_inner) {
            ArType::Primitive(_) | ArType::Ptr(_) => self.expr(
                HirExprKind::Binary {
                    op: BinaryOp::Equal,
                    left: l_val,
                    right: r_val,
                },
                bool_ty,
            ),
            ArType::Tuple(elems) => {
                let elem_tys = self.tc.type_info.type_interner.type_args(elems);
                let mut acc = self.bool_lit(true);
                for (i, elem_ty) in elem_tys.into_iter().enumerate() {
                    let idx_name = i.to_string();
                    let lf = self.field_access(lhs, &idx_name, elem_ty);
                    let rf = self.field_access(rhs, &idx_name, elem_ty);
                    let cmp = self.eq_values(lf, rf, elem_ty);
                    acc = if i == 0 {
                        cmp
                    } else {
                        self.expr(
                            HirExprKind::Binary {
                                op: BinaryOp::And,
                                left: acc,
                                right: cmp,
                            },
                            bool_ty,
                        )
                    };
                }
                acc
            }
            _ => {
                let l_ref = self.as_ref_arg(lhs, val_ty);
                let r_ref = self.as_ref_arg(rhs, val_ty);
                let l_ref_ty = self.pool.expr(l_ref).ty;
                let r_ref_ty = self.pool.expr(r_ref).ty;
                self.call_method(l_ref, l_ref_ty, "eq", vec![r_ref], vec![r_ref_ty], bool_ty)
            }
        }
    }

    fn build_eq_body(&mut self) -> Result<HirBlockId, Diagnostic> {
        let bool_ty = self.bool_ty();
        let self_e = self.self_expr();
        let other_e = self.other_expr();

        if !self.is_enum() {
            let fields = self.struct_fields();
            let mut stmts = Vec::new();
            for f in fields {
                if self.is_phantom_data(f.ty) {
                    continue;
                }
                let lf = self.field_access(self_e, &f.name, f.ty);
                let rf = self.field_access(other_e, &f.name, f.ty);
                let eq_e = self.eq_values(lf, rf, f.ty);
                let ne_e = self.expr(
                    HirExprKind::Unary {
                        op: UnaryOp::Not,
                        expr: eq_e,
                    },
                    bool_ty,
                );
                let false_e = self.bool_lit(false);
                let ret_false = self.return_expr(false_e);
                let then_blk = self.block(vec![ret_false]);
                stmts.push(self.stmt(HirStmtKind::If {
                    condition: HirCondition::Expr(ne_e),
                    then_block: then_blk,
                    else_block: None,
                }));
            }
            let true_e = self.bool_lit(true);
            stmts.push(self.return_expr(true_e));
            return Ok(self.block(stmts));
        }

        let variants = self.enum_variants();
        let mut outer_arms = Vec::with_capacity(variants.len());
        let multi = variants.len() > 1;

        for v in &variants {
            let (self_pat, l_binds) = self.build_enum_bind_pattern(v, "la");
            let (other_pat, r_binds) = self.build_enum_bind_pattern(v, "lb");

            let mut cmp_expr = self.bool_lit(true);
            let mut first = true;
            for ((l_sym, l_ty), (r_sym, r_ty)) in l_binds.iter().zip(r_binds.iter()) {
                let Some(lty) = *l_ty else { continue };
                let Some(rty) = *r_ty else { continue };
                let le = self.path(*l_sym, lty);
                let re = self.path(*r_sym, rty);
                let c = self.eq_values(le, re, lty);
                cmp_expr = if first {
                    first = false;
                    c
                } else {
                    self.expr(
                        HirExprKind::Binary {
                            op: BinaryOp::And,
                            left: cmp_expr,
                            right: c,
                        },
                        bool_ty,
                    )
                };
            }
            let mut inner_arms = vec![HirMatchArm {
                span: self.span,
                pattern: other_pat,
                guard: None,
                body: HirMatchArmBody::Expr(cmp_expr),
            }];
            if multi {
                let wild = self
                    .pool
                    .alloc_pattern(HirPattern::Wildcard { span: self.span });
                let fls = self.bool_lit(false);
                inner_arms.push(HirMatchArm {
                    span: self.span,
                    pattern: wild,
                    guard: None,
                    body: HirMatchArmBody::Expr(fls),
                });
            }
            let inner_range = self.pool.alloc_match_arm_list(&inner_arms);
            let inner_match = self.expr(
                HirExprKind::Match {
                    value: other_e,
                    arms: inner_range,
                },
                bool_ty,
            );
            outer_arms.push(HirMatchArm {
                span: self.span,
                pattern: self_pat,
                guard: None,
                body: HirMatchArmBody::Expr(inner_match),
            });
        }

        let outer_range = self.pool.alloc_match_arm_list(&outer_arms);
        let result_expr = self.expr(
            HirExprKind::Match {
                value: self_e,
                arms: outer_range,
            },
            bool_ty,
        );
        let ret = self.return_expr(result_expr);
        Ok(self.block(vec![ret]))
    }

    // ── Ord ──────────────────────────────────────────────────────────

    fn emit_cmp_step(
        &mut self,
        stmts: &mut Vec<HirStmtId>,
        lhs: HirExprId,
        rhs: HirExprId,
        val_ty: TypeId,
    ) -> Result<(), Diagnostic> {
        let bool_ty = self.bool_ty();
        let (_, inner) = self.deref_val(lhs, val_ty);
        let resolved = self.resolve(inner);
        if resolved.is_orderable() {
            let (l_val, _) = self.deref_val(lhs, val_ty);
            let (r_val, _) = self.deref_val(rhs, val_ty);
            let lt = self.expr(
                HirExprKind::Binary {
                    op: BinaryOp::Lt,
                    left: l_val,
                    right: r_val,
                },
                bool_ty,
            );
            let less = self.ordering_variant(LangItem::OrderingLess)?;
            let ret_less = self.return_expr(less);
            let blk_less = self.block(vec![ret_less]);
            stmts.push(self.stmt(HirStmtKind::If {
                condition: HirCondition::Expr(lt),
                then_block: blk_less,
                else_block: None,
            }));

            let gt = self.expr(
                HirExprKind::Binary {
                    op: BinaryOp::Gt,
                    left: l_val,
                    right: r_val,
                },
                bool_ty,
            );
            let greater = self.ordering_variant(LangItem::OrderingGreater)?;
            let ret_greater = self.return_expr(greater);
            let blk_greater = self.block(vec![ret_greater]);
            stmts.push(self.stmt(HirStmtKind::If {
                condition: HirCondition::Expr(gt),
                then_block: blk_greater,
                else_block: None,
            }));
            return Ok(());
        }
        if let ArType::Tuple(elems) = resolved {
            let elem_tys = self.tc.type_info.type_interner.type_args(elems);
            for (i, elem_ty) in elem_tys.into_iter().enumerate() {
                let idx_name = i.to_string();
                let lf = self.field_access(lhs, &idx_name, elem_ty);
                let rf = self.field_access(rhs, &idx_name, elem_ty);
                self.emit_cmp_step(stmts, lf, rf, elem_ty)?;
            }
            return Ok(());
        }

        let Some(ord_sym) = self.tc.symbols.lang_item_symbol(LangItem::Ordering) else {
            return Err(Diagnostic::ice(
                DiagCode::ICEL001,
                "missing Ordering lang item",
                self.span,
            ));
        };
        let ord_ty = self.intern(ArType::named(
            ord_sym,
            &[],
            &self.tc.type_info.type_interner,
        ));
        let eq_var = self.ordering_variant(LangItem::OrderingEqual)?;
        let l_ref = self.as_ref_arg(lhs, val_ty);
        let r_ref = self.as_ref_arg(rhs, val_ty);
        let l_ref_ty = self.pool.expr(l_ref).ty;
        let r_ref_ty = self.pool.expr(r_ref).ty;
        let cmp_call =
            self.call_method(l_ref, l_ref_ty, "cmp", vec![r_ref], vec![r_ref_ty], ord_ty);
        let res_sym = self.mint_local("ord_step", ord_ty);
        let bindings = self.pool.alloc_binding_list(&[HirBindingItem {
            symbol: res_sym,
            ty: ord_ty,
            span: self.span,
        }]);
        stmts.push(self.stmt(HirStmtKind::VarDecl {
            bindings,
            value: cmp_call,
        }));
        let res_path = self.path(res_sym, ord_ty);
        let ne_eq = self.expr(
            HirExprKind::Binary {
                op: BinaryOp::NotEqual,
                left: res_path,
                right: eq_var,
            },
            bool_ty,
        );
        let ret_res = self.return_expr(res_path);
        let then_blk = self.block(vec![ret_res]);
        stmts.push(self.stmt(HirStmtKind::If {
            condition: HirCondition::Expr(ne_eq),
            then_block: then_blk,
            else_block: None,
        }));
        Ok(())
    }

    fn build_cmp_body(&mut self) -> Result<HirBlockId, Diagnostic> {
        let self_e = self.self_expr();
        let other_e = self.other_expr();
        let mut stmts = Vec::new();

        if !self.is_enum() {
            let fields = self.struct_fields();
            for f in fields {
                if self.is_phantom_data(f.ty) {
                    continue;
                }
                let lf = self.field_access(self_e, &f.name, f.ty);
                let rf = self.field_access(other_e, &f.name, f.ty);
                self.emit_cmp_step(&mut stmts, lf, rf, f.ty)?;
            }
            let eq = self.ordering_variant(LangItem::OrderingEqual)?;
            stmts.push(self.return_expr(eq));
            return Ok(self.block(stmts));
        }

        let variants = self.enum_variants();
        let int_ty = self.int_ty();
        let tag_expr = |this: &mut Self, target: HirExprId| -> HirExprId {
            let mut arms = Vec::with_capacity(variants.len());
            for v in &variants {
                let pat = this.build_enum_wild_pattern(v);
                let lit = this.expr(HirExprKind::Int(SmolStr::new(v.tag.to_string())), int_ty);
                arms.push(HirMatchArm {
                    span: this.span,
                    pattern: pat,
                    guard: None,
                    body: HirMatchArmBody::Expr(lit),
                });
            }
            let arm_range = this.pool.alloc_match_arm_list(&arms);
            this.expr(
                HirExprKind::Match {
                    value: target,
                    arms: arm_range,
                },
                int_ty,
            )
        };

        let tag_a = tag_expr(self, self_e);
        let tag_b = tag_expr(self, other_e);
        let bool_ty = self.bool_ty();
        let lt = self.expr(
            HirExprKind::Binary {
                op: BinaryOp::Lt,
                left: tag_a,
                right: tag_b,
            },
            bool_ty,
        );
        let less = self.ordering_variant(LangItem::OrderingLess)?;
        let ret_less = self.return_expr(less);
        let blk_less = self.block(vec![ret_less]);
        stmts.push(self.stmt(HirStmtKind::If {
            condition: HirCondition::Expr(lt),
            then_block: blk_less,
            else_block: None,
        }));

        let gt = self.expr(
            HirExprKind::Binary {
                op: BinaryOp::Gt,
                left: tag_a,
                right: tag_b,
            },
            bool_ty,
        );
        let greater = self.ordering_variant(LangItem::OrderingGreater)?;
        let ret_greater = self.return_expr(greater);
        let blk_greater = self.block(vec![ret_greater]);
        stmts.push(self.stmt(HirStmtKind::If {
            condition: HirCondition::Expr(gt),
            then_block: blk_greater,
            else_block: None,
        }));

        // Discriminants match; compare payload fields if any variant carries payloads.
        let has_payloads = variants
            .iter()
            .any(|v| v.payload_tys.iter().any(|&t| !self.is_phantom_data(t)));
        if has_payloads {
            let mut outer_arms = Vec::with_capacity(variants.len());
            let eq_default = self.ordering_variant(LangItem::OrderingEqual)?;
            for v in &variants {
                let (self_pat, l_binds) = self.build_enum_bind_pattern(v, "ca");
                let (other_pat, r_binds) = self.build_enum_bind_pattern(v, "cb");
                let mut arm_stmts = Vec::new();
                for ((ls, lt_opt), (rs, _)) in l_binds.iter().zip(r_binds.iter()) {
                    let Some(lty) = *lt_opt else { continue };
                    let le = self.path(*ls, lty);
                    let re = self.path(*rs, lty);
                    self.emit_cmp_step(&mut arm_stmts, le, re, lty)?;
                }
                arm_stmts.push(self.return_expr(eq_default));
                let inner_blk = self.block(arm_stmts);
                let mut inner_arms = vec![HirMatchArm {
                    span: self.span,
                    pattern: other_pat,
                    guard: None,
                    body: HirMatchArmBody::Block(inner_blk),
                }];
                if variants.len() > 1 {
                    let wild = self
                        .pool
                        .alloc_pattern(HirPattern::Wildcard { span: self.span });
                    let eq_val = self.ordering_variant(LangItem::OrderingEqual)?;
                    inner_arms.push(HirMatchArm {
                        span: self.span,
                        pattern: wild,
                        guard: None,
                        body: HirMatchArmBody::Expr(eq_val),
                    });
                }
                let inner_range = self.pool.alloc_match_arm_list(&inner_arms);
                let inner_match = self.stmt(HirStmtKind::Match {
                    value: other_e,
                    arms: inner_range,
                });
                let outer_blk = self.block(vec![inner_match]);
                outer_arms.push(HirMatchArm {
                    span: self.span,
                    pattern: self_pat,
                    guard: None,
                    body: HirMatchArmBody::Block(outer_blk),
                });
            }
            let outer_range = self.pool.alloc_match_arm_list(&outer_arms);
            stmts.push(self.stmt(HirStmtKind::Match {
                value: self_e,
                arms: outer_range,
            }));
        }

        let eq = self.ordering_variant(LangItem::OrderingEqual)?;
        stmts.push(self.return_expr(eq));
        Ok(self.block(stmts))
    }

    // ── Clone ────────────────────────────────────────────────────────

    fn clone_value(&mut self, val: HirExprId, val_ty: TypeId) -> HirExprId {
        let (bare, bare_ty) = self.deref_val(val, val_ty);
        if self.tc.type_info.is_copy(bare_ty) {
            return bare;
        }
        if let ArType::Named(sym, _) = self.resolve(bare_ty)
            && self
                .tc
                .symbols
                .try_get(sym)
                .is_some_and(|s| s.lang_item == Some(LangItem::PhantomData))
        {
            let empty = self.pool.alloc_field_init_list(&[]);
            return self.expr(
                HirExprKind::StructLiteral {
                    struct_symbol: sym,
                    fields: empty,
                },
                bare_ty,
            );
        }
        let ref_val = self.as_ref_arg(val, val_ty);
        let ref_ty = self.pool.expr(ref_val).ty;
        self.call_method(ref_val, ref_ty, "clone", vec![], vec![], bare_ty)
    }

    fn build_clone_body(&mut self) -> Result<HirBlockId, Diagnostic> {
        let self_e = self.self_expr();
        let ret_ty = self.recipe.return_ty;

        // Structural Copy fast-path: `*self` directly without cloning field-by-field.
        if self.recipe.is_copy {
            let deref = self.expr(
                HirExprKind::Unary {
                    op: UnaryOp::Deref,
                    expr: self_e,
                },
                ret_ty,
            );
            let ret = self.return_expr(deref);
            return Ok(self.block(vec![ret]));
        }

        if !self.is_enum() {
            let fields = self.struct_fields();
            let mut inits = Vec::with_capacity(fields.len());
            for f in fields {
                let access = self.field_access(self_e, &f.name, f.ty);
                let cloned = self.clone_value(access, f.ty);
                inits.push(HirFieldInit {
                    span: self.span,
                    name: f.name,
                    value: cloned,
                });
            }
            let init_range = self.pool.alloc_field_init_list(&inits);
            let lit = self.expr(
                HirExprKind::StructLiteral {
                    struct_symbol: self.recipe.type_symbol,
                    fields: init_range,
                },
                ret_ty,
            );
            let ret = self.return_expr(lit);
            return Ok(self.block(vec![ret]));
        }

        let variants = self.enum_variants();
        let mut arms = Vec::with_capacity(variants.len());
        for v in &variants {
            let (pat, binds) = self.build_enum_bind_pattern_all(v, "cl");
            let mut args = Vec::with_capacity(v.payload_tys.len());
            for ((sym, bind_ty), &target_ty) in binds.iter().zip(v.payload_tys.iter()) {
                let b_expr = self.path(*sym, *bind_ty);
                args.push(self.clone_value(b_expr, target_ty));
            }
            let constructed = self.enum_ctor(v.symbol, args, ret_ty);
            arms.push(HirMatchArm {
                span: self.span,
                pattern: pat,
                guard: None,
                body: HirMatchArmBody::Expr(constructed),
            });
        }
        let arms_range = self.pool.alloc_match_arm_list(&arms);
        let matched = self.expr(
            HirExprKind::Match {
                value: self_e,
                arms: arms_range,
            },
            ret_ty,
        );
        let ret = self.return_expr(matched);
        Ok(self.block(vec![ret]))
    }

    // ── Hash ─────────────────────────────────────────────────────────

    fn emit_hash_value(
        &mut self,
        stmts: &mut Vec<HirStmtId>,
        state_e: HirExprId,
        val: HirExprId,
        val_ty: TypeId,
    ) -> Result<(), Diagnostic> {
        let state_ty = self.recipe.explicit_param_tys[0];
        let void_ty = self.void_ty();
        let (bare, bare_ty) = self.deref_val(val, val_ty);

        match self.resolve(bare_ty) {
            ArType::Primitive(Primitive::Bool) => {
                let u8_ty = self.u8_ty();
                let one = self.expr(HirExprKind::Int(SmolStr::new("1")), u8_ty);
                let zero = self.expr(HirExprKind::Int(SmolStr::new("0")), u8_ty);
                let one_stmt = self.stmt(HirStmtKind::Expr(one));
                let zero_stmt = self.stmt(HirStmtKind::Expr(zero));
                let then_blk = self.block(vec![one_stmt]);
                let else_blk = self.block(vec![zero_stmt]);
                let byte = self.expr(
                    HirExprKind::If {
                        condition: HirCondition::Expr(bare),
                        then_block: then_blk,
                        else_block: else_blk,
                    },
                    u8_ty,
                );
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "writeU8",
                    vec![byte],
                    vec![u8_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Primitive(Primitive::U8) => {
                let u8_ty = self.u8_ty();
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "writeU8",
                    vec![bare],
                    vec![u8_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Primitive(Primitive::U16) => {
                let u16_ty = self.u16_ty();
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "writeU16",
                    vec![bare],
                    vec![u16_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Primitive(Primitive::U32) => {
                let u32_ty = self.u32_ty();
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "writeU32",
                    vec![bare],
                    vec![u32_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Primitive(Primitive::Char) => {
                let u32_ty = self.u32_ty();
                let cast = self.expr(
                    HirExprKind::Cast {
                        expr: bare,
                        target_ty: u32_ty,
                    },
                    u32_ty,
                );
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "writeU32",
                    vec![cast],
                    vec![u32_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Primitive(Primitive::U64 | Primitive::USize) => {
                let u64_ty = self.u64_ty();
                let arg = if bare_ty == u64_ty {
                    bare
                } else {
                    self.expr(
                        HirExprKind::Cast {
                            expr: bare,
                            target_ty: u64_ty,
                        },
                        u64_ty,
                    )
                };
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "writeU64",
                    vec![arg],
                    vec![u64_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Primitive(p) if p.is_integer() => {
                let int_ty = self.int_ty();
                let arg = if bare_ty == int_ty {
                    bare
                } else {
                    self.expr(
                        HirExprKind::Cast {
                            expr: bare,
                            target_ty: int_ty,
                        },
                        int_ty,
                    )
                };
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "writeInt",
                    vec![arg],
                    vec![int_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Primitive(Primitive::Str) => {
                let u8_ty = self.u8_ty();
                let slice_u8_ty = self.intern(ArType::Slice(u8_ty));
                let s_ref = self.as_ref_arg(bare, bare_ty);
                let s_ref_ty = self.pool.expr(s_ref).ty;
                let bytes =
                    self.call_method(s_ref, s_ref_ty, "asBytes", vec![], vec![], slice_u8_ty);
                let call = self.call_method(
                    state_e,
                    state_ty,
                    "write",
                    vec![bytes],
                    vec![slice_u8_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
            ArType::Tuple(elems) => {
                let elem_tys = self.tc.type_info.type_interner.type_args(elems);
                for (i, elem_ty) in elem_tys.into_iter().enumerate() {
                    let idx_name = i.to_string();
                    let fe = self.field_access(val, &idx_name, elem_ty);
                    self.emit_hash_value(stmts, state_e, fe, elem_ty)?;
                }
            }
            _ => {
                let ref_val = self.as_ref_arg(val, val_ty);
                let ref_ty = self.pool.expr(ref_val).ty;
                let call = self.call_method(
                    ref_val,
                    ref_ty,
                    "hash",
                    vec![state_e],
                    vec![state_ty],
                    void_ty,
                );
                stmts.push(self.stmt(HirStmtKind::Expr(call)));
            }
        }
        Ok(())
    }

    fn build_hash_body(&mut self) -> Result<HirBlockId, Diagnostic> {
        let self_e = self.self_expr();
        let state_e = self.second_param_expr();
        let void_ty = self.void_ty();
        let state_ty = self.recipe.explicit_param_tys[0];
        let mut stmts = Vec::new();

        if !self.is_enum() {
            let fields = self.struct_fields();
            for f in fields {
                if self.is_phantom_data(f.ty) {
                    continue;
                }
                let fe = self.field_access(self_e, &f.name, f.ty);
                self.emit_hash_value(&mut stmts, state_e, fe, f.ty)?;
            }
            let void_ret = self.pool.alloc_expr_list(&[]);
            stmts.push(self.stmt(HirStmtKind::Return { values: void_ret }));
            return Ok(self.block(stmts));
        }

        let variants = self.enum_variants();
        let u64_ty = self.u64_ty();
        let mut arms = Vec::with_capacity(variants.len());
        for v in &variants {
            let (pat, binds) = self.build_enum_bind_pattern(v, "ha");
            let mut arm_stmts = Vec::new();
            let tag_lit = self.expr(HirExprKind::Int(SmolStr::new(v.tag.to_string())), u64_ty);
            let tag_call = self.call_method(
                state_e,
                state_ty,
                "writeU64",
                vec![tag_lit],
                vec![u64_ty],
                void_ty,
            );
            arm_stmts.push(self.stmt(HirStmtKind::Expr(tag_call)));
            for (sym, ty_opt) in binds {
                let Some(ty) = ty_opt else { continue };
                let pe = self.path(sym, ty);
                self.emit_hash_value(&mut arm_stmts, state_e, pe, ty)?;
            }
            let blk = self.block(arm_stmts);
            arms.push(HirMatchArm {
                span: self.span,
                pattern: pat,
                guard: None,
                body: HirMatchArmBody::Block(blk),
            });
        }
        let arms_range = self.pool.alloc_match_arm_list(&arms);
        stmts.push(self.stmt(HirStmtKind::Match {
            value: self_e,
            arms: arms_range,
        }));
        let void_ret = self.pool.alloc_expr_list(&[]);
        stmts.push(self.stmt(HirStmtKind::Return { values: void_ret }));
        Ok(self.block(stmts))
    }

    // ── Debug ────────────────────────────────────────────────────────

    fn emit_fmt_checked(
        &mut self,
        stmts: &mut Vec<HirStmtId>,
        call_expr: HirExprId,
        written_sym: SymbolId,
    ) -> Result<(), Diagnostic> {
        let usize_ty = self.usize_ty();
        let Some(fmt_err_sym) = self.tc.symbols.lang_item_symbol(LangItem::FmtError) else {
            return Err(Diagnostic::ice(
                DiagCode::ICEL001,
                "missing FmtError lang item",
                self.span,
            ));
        };
        let fmt_err_ty = self.intern(ArType::named(
            fmt_err_sym,
            &[],
            &self.tc.type_info.type_interner,
        ));
        let n_sym = self.mint_local("n", usize_ty);
        let e_sym = self.mint_local("e", fmt_err_ty);

        let n_pat = self.pool.alloc_pattern(HirPattern::Bind {
            span: self.span,
            name: SmolStr::new("n"),
            symbol: n_sym,
        });
        let e_pat = self.pool.alloc_pattern(HirPattern::Bind {
            span: self.span,
            name: SmolStr::new("e"),
            symbol: e_sym,
        });

        let ok_payload = self.pool.alloc_pattern_list(&[n_pat]);
        let ok_pat = self.pool.alloc_pattern(HirPattern::TypeTuple {
            span: self.span,
            name: SmolStr::new("Ok"),
            payload: ok_payload,
        });
        let err_payload = self.pool.alloc_pattern_list(&[e_pat]);
        let err_pat = self.pool.alloc_pattern(HirPattern::TypeTuple {
            span: self.span,
            name: SmolStr::new("Err"),
            payload: err_payload,
        });

        let w_expr = self.path(written_sym, usize_ty);
        let n_expr = self.path(n_sym, usize_ty);
        let sum = self.expr(
            HirExprKind::Binary {
                op: BinaryOp::Add,
                left: w_expr,
                right: n_expr,
            },
            usize_ty,
        );
        let places = self.pool.alloc_place_list(&[HirPlace {
            root_symbol: written_sym,
            suffixes: smallvec::SmallVec::new(),
            ty: usize_ty,
            span: self.span,
        }]);
        let add_stmt = self.stmt(HirStmtKind::Set {
            places,
            op: SetOp::Assign,
            value: sum,
        });
        let ok_blk = self.block(vec![add_stmt]);

        let e_expr = self.path(e_sym, fmt_err_ty);
        let err_ctor = self.expr(
            HirExprKind::ResultCtor {
                variant: ResultCtorVariant::Err,
                value: e_expr,
            },
            self.recipe.return_ty,
        );
        let ret_err = self.return_expr(err_ctor);
        let err_blk = self.block(vec![ret_err]);

        let arms = self.pool.alloc_match_arm_list(&[
            HirMatchArm {
                span: self.span,
                pattern: ok_pat,
                guard: None,
                body: HirMatchArmBody::Block(ok_blk),
            },
            HirMatchArm {
                span: self.span,
                pattern: err_pat,
                guard: None,
                body: HirMatchArmBody::Block(err_blk),
            },
        ]);
        stmts.push(self.stmt(HirStmtKind::Match {
            value: call_expr,
            arms,
        }));
        Ok(())
    }

    fn emit_fmt_write_str(
        &mut self,
        stmts: &mut Vec<HirStmtId>,
        f_e: HirExprId,
        text: &str,
        written_sym: SymbolId,
    ) -> Result<(), Diagnostic> {
        let f_ty = self.recipe.explicit_param_tys[0];
        let str_ty = self.str_ty();
        let ret_ty = self.recipe.return_ty;
        let lit = self.str_lit(text);
        let call = self.call_method(f_e, f_ty, "writeStr", vec![lit], vec![str_ty], ret_ty);
        self.emit_fmt_checked(stmts, call, written_sym)
    }

    fn emit_fmt_value(
        &mut self,
        stmts: &mut Vec<HirStmtId>,
        f_e: HirExprId,
        val: HirExprId,
        val_ty: TypeId,
        written_sym: SymbolId,
    ) -> Result<(), Diagnostic> {
        let f_ty = self.recipe.explicit_param_tys[0];
        let ret_ty = self.recipe.return_ty;
        let str_ty = self.str_ty();
        let u8_ty = self.u8_ty();
        let (bare, bare_ty) = self.deref_val(val, val_ty);

        match self.resolve(bare_ty) {
            ArType::Primitive(Primitive::Bool) => {
                let bty = self.bool_ty();
                let call = self.call_method(f_e, f_ty, "writeBool", vec![bare], vec![bty], ret_ty);
                self.emit_fmt_checked(stmts, call, written_sym)?;
            }
            ArType::Primitive(
                Primitive::U8 | Primitive::U16 | Primitive::U32 | Primitive::U64 | Primitive::USize,
            ) => {
                let u64_ty = self.u64_ty();
                let arg = if bare_ty == u64_ty {
                    bare
                } else {
                    self.expr(
                        HirExprKind::Cast {
                            expr: bare,
                            target_ty: u64_ty,
                        },
                        u64_ty,
                    )
                };
                let call =
                    self.call_method(f_e, f_ty, "writeUint", vec![arg], vec![u64_ty], ret_ty);
                self.emit_fmt_checked(stmts, call, written_sym)?;
            }
            ArType::Primitive(p) if p.is_integer() => {
                let i64_ty = self.i64_ty();
                let arg = if bare_ty == i64_ty {
                    bare
                } else {
                    self.expr(
                        HirExprKind::Cast {
                            expr: bare,
                            target_ty: i64_ty,
                        },
                        i64_ty,
                    )
                };
                let call = self.call_method(f_e, f_ty, "writeInt", vec![arg], vec![i64_ty], ret_ty);
                self.emit_fmt_checked(stmts, call, written_sym)?;
            }
            ArType::Primitive(Primitive::Float | Primitive::F32 | Primitive::F64) => {
                let s = self.expr(HirExprKind::ToStr { value: bare }, str_ty);
                let call = self.call_method(f_e, f_ty, "writeStr", vec![s], vec![str_ty], ret_ty);
                self.emit_fmt_checked(stmts, call, written_sym)?;
            }
            ArType::Primitive(Primitive::Char) => {
                let sq = self.expr(HirExprKind::Int(SmolStr::new("39")), u8_ty);
                let c1 = self.call_method(f_e, f_ty, "writeByte", vec![sq], vec![u8_ty], ret_ty);
                self.emit_fmt_checked(stmts, c1, written_sym)?;
                let s = self.expr(HirExprKind::ToStr { value: bare }, str_ty);
                let c2 = self.call_method(f_e, f_ty, "writeStr", vec![s], vec![str_ty], ret_ty);
                self.emit_fmt_checked(stmts, c2, written_sym)?;
                let c3 = self.call_method(f_e, f_ty, "writeByte", vec![sq], vec![u8_ty], ret_ty);
                self.emit_fmt_checked(stmts, c3, written_sym)?;
            }
            ArType::Primitive(Primitive::Str) => {
                let dq = self.expr(HirExprKind::Int(SmolStr::new("34")), u8_ty);
                let c1 = self.call_method(f_e, f_ty, "writeByte", vec![dq], vec![u8_ty], ret_ty);
                self.emit_fmt_checked(stmts, c1, written_sym)?;
                let c2 = self.call_method(f_e, f_ty, "writeStr", vec![bare], vec![str_ty], ret_ty);
                self.emit_fmt_checked(stmts, c2, written_sym)?;
                let c3 = self.call_method(f_e, f_ty, "writeByte", vec![dq], vec![u8_ty], ret_ty);
                self.emit_fmt_checked(stmts, c3, written_sym)?;
            }
            ArType::Named(sym, _)
                if self
                    .tc
                    .symbols
                    .try_get(sym)
                    .is_some_and(|s| s.lang_item == Some(LangItem::String)) =>
            {
                let dq = self.expr(HirExprKind::Int(SmolStr::new("34")), u8_ty);
                let c1 = self.call_method(f_e, f_ty, "writeByte", vec![dq], vec![u8_ty], ret_ty);
                self.emit_fmt_checked(stmts, c1, written_sym)?;
                let ref_str_ty = self.intern(ArType::Ref(str_ty));
                let s_ref = self.as_ref_arg(val, val_ty);
                let s_ref_ty = self.pool.expr(s_ref).ty;
                let as_str_ref =
                    self.call_method(s_ref, s_ref_ty, "asStr", vec![], vec![], ref_str_ty);
                let as_str = self.expr(
                    HirExprKind::Unary {
                        op: UnaryOp::Deref,
                        expr: as_str_ref,
                    },
                    str_ty,
                );
                let c2 =
                    self.call_method(f_e, f_ty, "writeStr", vec![as_str], vec![str_ty], ret_ty);
                self.emit_fmt_checked(stmts, c2, written_sym)?;
                let c3 = self.call_method(f_e, f_ty, "writeByte", vec![dq], vec![u8_ty], ret_ty);
                self.emit_fmt_checked(stmts, c3, written_sym)?;
            }
            ArType::Tuple(elems) => {
                self.emit_fmt_write_str(stmts, f_e, "(", written_sym)?;
                let elem_tys = self.tc.type_info.type_interner.type_args(elems);
                for (i, elem_ty) in elem_tys.into_iter().enumerate() {
                    if i > 0 {
                        self.emit_fmt_write_str(stmts, f_e, ", ", written_sym)?;
                    }
                    let idx_name = i.to_string();
                    let fe = self.field_access(val, &idx_name, elem_ty);
                    self.emit_fmt_value(stmts, f_e, fe, elem_ty, written_sym)?;
                }
                self.emit_fmt_write_str(stmts, f_e, ")", written_sym)?;
            }
            _ => {
                let ref_val = self.as_ref_arg(val, val_ty);
                let ref_ty = self.pool.expr(ref_val).ty;
                let call =
                    self.call_method(ref_val, ref_ty, "fmtDebug", vec![f_e], vec![f_ty], ret_ty);
                self.emit_fmt_checked(stmts, call, written_sym)?;
            }
        }
        Ok(())
    }

    fn build_debug_body(&mut self) -> Result<HirBlockId, Diagnostic> {
        let self_e = self.self_expr();
        let f_e = self.second_param_expr();
        let usize_ty = self.usize_ty();
        let written_sym = self.mint_local("written", usize_ty);
        let zero = self.expr(HirExprKind::Int(SmolStr::new("0")), usize_ty);
        let bindings = self.pool.alloc_binding_list(&[HirBindingItem {
            symbol: written_sym,
            ty: usize_ty,
            span: self.span,
        }]);
        let mut stmts = vec![self.stmt(HirStmtKind::VarDecl {
            bindings,
            value: zero,
        })];

        let raw_ty_name = self.tc.symbols.get(self.recipe.type_symbol).name.clone();
        let type_name = raw_ty_name
            .rsplit('.')
            .next()
            .unwrap_or(&raw_ty_name)
            .to_string();

        if !self.is_enum() {
            let fields: Vec<FieldSpec> = self
                .struct_fields()
                .into_iter()
                .filter(|f| !self.is_phantom_data(f.ty))
                .collect();
            if fields.is_empty() {
                self.emit_fmt_write_str(&mut stmts, f_e, &type_name, written_sym)?;
            } else {
                let header = format!("{type_name} {{ ");
                self.emit_fmt_write_str(&mut stmts, f_e, &header, written_sym)?;
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.emit_fmt_write_str(&mut stmts, f_e, ", ", written_sym)?;
                    }
                    let label = format!("{}: ", f.name);
                    self.emit_fmt_write_str(&mut stmts, f_e, &label, written_sym)?;
                    let fe = self.field_access(self_e, &f.name, f.ty);
                    self.emit_fmt_value(&mut stmts, f_e, fe, f.ty, written_sym)?;
                }
                self.emit_fmt_write_str(&mut stmts, f_e, " }", written_sym)?;
            }
            let w_final = self.path(written_sym, usize_ty);
            let ok = self.expr(
                HirExprKind::ResultCtor {
                    variant: ResultCtorVariant::Ok,
                    value: w_final,
                },
                self.recipe.return_ty,
            );
            stmts.push(self.return_expr(ok));
            return Ok(self.block(stmts));
        }

        let variants = self.enum_variants();
        let mut arms = Vec::with_capacity(variants.len());
        for v in &variants {
            let (pat, binds) = self.build_enum_bind_pattern(v, "db");
            let mut arm_stmts = Vec::new();
            let prefix = format!("{type_name}.{}", v.name);
            let active_binds: Vec<(SymbolId, TypeId)> = binds
                .into_iter()
                .filter_map(|(s, t_opt)| t_opt.map(|t| (s, t)))
                .collect();
            if active_binds.is_empty() {
                self.emit_fmt_write_str(&mut arm_stmts, f_e, &prefix, written_sym)?;
            } else {
                let open = format!("{prefix}(");
                self.emit_fmt_write_str(&mut arm_stmts, f_e, &open, written_sym)?;
                for (i, (sym, ty)) in active_binds.iter().enumerate() {
                    if i > 0 {
                        self.emit_fmt_write_str(&mut arm_stmts, f_e, ", ", written_sym)?;
                    }
                    let pe = self.path(*sym, *ty);
                    self.emit_fmt_value(&mut arm_stmts, f_e, pe, *ty, written_sym)?;
                }
                self.emit_fmt_write_str(&mut arm_stmts, f_e, ")", written_sym)?;
            }
            let blk = self.block(arm_stmts);
            arms.push(HirMatchArm {
                span: self.span,
                pattern: pat,
                guard: None,
                body: HirMatchArmBody::Block(blk),
            });
        }
        let arms_range = self.pool.alloc_match_arm_list(&arms);
        stmts.push(self.stmt(HirStmtKind::Match {
            value: self_e,
            arms: arms_range,
        }));
        let w_final = self.path(written_sym, usize_ty);
        let ok = self.expr(
            HirExprKind::ResultCtor {
                variant: ResultCtorVariant::Ok,
                value: w_final,
            },
            self.recipe.return_ty,
        );
        stmts.push(self.return_expr(ok));
        Ok(self.block(stmts))
    }

    // ── Enum pattern builders ────────────────────────────────────────

    /// Builds an enum variant pattern that skips `PhantomData` fields with `_`
    /// and binds non-Copy/generic payloads as `ref T`.
    fn build_enum_bind_pattern(
        &mut self,
        v: &VariantSpec,
        prefix: &str,
    ) -> (crate::hir::HirPatternId, Vec<(SymbolId, Option<TypeId>)>) {
        let mut pat_ids = Vec::with_capacity(v.payload_tys.len());
        let mut binds = Vec::with_capacity(v.payload_tys.len());
        for (i, &fty) in v.payload_tys.iter().enumerate() {
            if self.is_phantom_data(fty) {
                pat_ids.push(
                    self.pool
                        .alloc_pattern(HirPattern::Wildcard { span: self.span }),
                );
                binds.push((SymbolId::DUMMY, None));
                continue;
            }
            let bind_ty = if self.tc.type_info.is_copy(fty) {
                fty
            } else {
                self.intern(ArType::Ref(fty))
            };
            let bname = format!("{prefix}{i}");
            let bsym = self.mint_local(&bname, bind_ty);
            pat_ids.push(self.pool.alloc_pattern(HirPattern::Bind {
                span: self.span,
                name: SmolStr::new(bname),
                symbol: bsym,
            }));
            binds.push((bsym, Some(bind_ty)));
        }
        let payload = self.pool.alloc_pattern_list(&pat_ids);
        let pat = self.pool.alloc_pattern(HirPattern::Enum {
            span: self.span,
            type_symbol: self.recipe.type_symbol,
            variant: v.name.clone(),
            variant_symbol: Some(v.symbol),
            payload,
        });
        (pat, binds)
    }

    /// Same as `build_enum_bind_pattern` but also binds `PhantomData` slots for `Clone`.
    fn build_enum_bind_pattern_all(
        &mut self,
        v: &VariantSpec,
        prefix: &str,
    ) -> (crate::hir::HirPatternId, Vec<(SymbolId, TypeId)>) {
        let mut pat_ids = Vec::with_capacity(v.payload_tys.len());
        let mut binds = Vec::with_capacity(v.payload_tys.len());
        for (i, &fty) in v.payload_tys.iter().enumerate() {
            let bind_ty = if self.tc.type_info.is_copy(fty) {
                fty
            } else {
                self.intern(ArType::Ref(fty))
            };
            let bname = format!("{prefix}{i}");
            let bsym = self.mint_local(&bname, bind_ty);
            pat_ids.push(self.pool.alloc_pattern(HirPattern::Bind {
                span: self.span,
                name: SmolStr::new(bname),
                symbol: bsym,
            }));
            binds.push((bsym, bind_ty));
        }
        let payload = self.pool.alloc_pattern_list(&pat_ids);
        let pat = self.pool.alloc_pattern(HirPattern::Enum {
            span: self.span,
            type_symbol: self.recipe.type_symbol,
            variant: v.name.clone(),
            variant_symbol: Some(v.symbol),
            payload,
        });
        (pat, binds)
    }

    fn build_enum_wild_pattern(&mut self, v: &VariantSpec) -> crate::hir::HirPatternId {
        let pat_ids: Vec<_> = v
            .payload_tys
            .iter()
            .map(|_| {
                self.pool
                    .alloc_pattern(HirPattern::Wildcard { span: self.span })
            })
            .collect();
        let payload = self.pool.alloc_pattern_list(&pat_ids);
        self.pool.alloc_pattern(HirPattern::Enum {
            span: self.span,
            type_symbol: self.recipe.type_symbol,
            variant: v.name.clone(),
            variant_symbol: Some(v.symbol),
            payload,
        })
    }
}
