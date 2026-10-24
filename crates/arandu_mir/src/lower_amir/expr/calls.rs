//! Function calls, method calls, and intrinsic resolution during AMIR lowering.

use super::super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{
    AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, TempId,
};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::{HirExpr, HirExprId, HirExprKind, IndexRange};
use crate::passes::type_checker::types::{ArType, Primitive};

fn resolve_method_target(
    callee: &HirExpr,
    pool: &crate::hir::HirPool,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
) -> Option<crate::SymbolId> {
    let (base_id, field) = match &callee.kind {
        HirExprKind::Field { base, field } | HirExprKind::SafeField { base, field } => {
            (*base, field)
        }
        _ => return None,
    };

    let base_expr = pool.expr(base_id);
    // Peel Nullable / & / &mut / ptr so `shared self: &T` still resolves methods
    // (same family as typeck synth_method_call + PROMOTE-L1 interface via T).
    let mut base_ty = interner.resolve(base_expr.ty);
    for _ in 0..4 {
        base_ty = match base_ty {
            ArType::Nullable(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Ptr(inner) => interner.resolve(inner),
            other => other,
        };
        if matches!(
            base_ty,
            ArType::Named(_, _)
                | ArType::Result(_, _)
                | ArType::Option(_)
                | ArType::Primitive(_)
                | ArType::Slice(_)
                | ArType::MutSlice(_)
        ) {
            break;
        }
    }
    // Named receivers use their type SymbolId; builtin Result/Option/primitive methods
    // live on the prelude type symbols (linked by import re-index).
    let type_id = match base_ty {
        ArType::Named(id, _) => Some(id),
        ArType::Vector(shape) => symbols.vector_type_symbol(shape),
        ArType::Result(_, _) => symbols.builtins.result,
        ArType::Option(_) => symbols.builtins.option,
        ArType::Primitive(Primitive::Str) => symbols.builtins.str_type,
        ArType::Primitive(Primitive::Char) => symbols.builtins.char_type,
        ArType::Primitive(Primitive::Int) | ArType::IntLiteral => symbols.builtins.int_type,
        ArType::Primitive(Primitive::U32) => symbols.builtins.u32_type,
        ArType::Primitive(Primitive::U64) => symbols.builtins.u64_type,
        ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => symbols.builtins.float_type,
        ArType::Slice(_) | ArType::MutSlice(_) => symbols.builtins.slice_type,
        ArType::Ptr(_) => symbols.builtins.ptr_type,
        _ => None,
    }?;

    symbols.lookup_associated_member(type_id, field)
}

impl LowerCtx<'_> {
    pub(super) fn lower_call(
        &mut self,
        callee: HirExprId,
        args: IndexRange,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let start_owned = self.owned_string_temps.clone();
        let result = self.lower_call_inner(callee, args, expr, target, symbols)?;
        // Statement-scoped temporaries, as in Rust, restricted to what is
        // provably unaliased: an owned string created for the arguments
        // (`"${x}".len()`, `str.len("${x}")`) dies after the call when every
        // parameter is a `str`/`[]T` view (by value or shared `ref`) or carries
        // no borrow, and the result carries no borrow. View-returning calls and
        // `mut ref`/aggregate parameters that could keep the string are left
        // alone.
        if self.owned_string_temps.len() > start_owned.len()
            && self.call_cannot_retain_string_args(callee, expr.ty)
        {
            self.free_new_owned_string_temps(&start_owned, None);
        }
        Ok(result)
    }

    fn call_cannot_retain_string_args(
        &self,
        callee: HirExprId,
        result_ty: crate::types::TypeId,
    ) -> bool {
        let interner = &self.tc.type_info.type_interner;
        if result_may_hold_view(result_ty, interner) {
            return false;
        }
        let ArType::Func(params, _) = self.resolve_ty(self.hir.pool.expr(callee).ty) else {
            return false;
        };
        // A view parameter can only outlive the call through the result, a
        // retaining parameter (`mut ref`, aggregates) or unsafe global state.
        let is_view = |ty: Option<ArType>| {
            matches!(
                ty,
                Some(ArType::Primitive(crate::types::Primitive::Str) | ArType::Slice(_))
            )
        };
        interner
            .type_args(params)
            .iter()
            .all(|&param| match interner.try_resolve(param) {
                Some(ArType::Ref(inner)) => is_view(interner.try_resolve(inner)),
                view @ Some(ArType::Primitive(crate::types::Primitive::Str) | ArType::Slice(_)) => {
                    is_view(view)
                }
                _ => !crate::borrow_facts::type_may_carry_borrow(param, interner),
            })
    }

    fn lower_call_inner(
        &mut self,
        callee: HirExprId,
        args: IndexRange,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let callee_expr = self.hir.pool.expr(callee);
        // `std.testing.blackBox<T>(value)` is a compiler-recognized
        // identity barrier. Lower it before generic call expansion so
        // DCE and both backends see the explicit AMIR contract.
        let black_box_symbol = match &callee_expr.kind {
            HirExprKind::Path { symbol } => Some(*symbol),
            HirExprKind::Generic { callee, .. } => match &self.hir.pool.expr(*callee).kind {
                HirExprKind::Path { symbol } => Some(*symbol),
                HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                _ => None,
            },
            HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
            _ => None,
        };
        if black_box_symbol.is_some_and(|symbol| {
            symbols.get(symbol).lang_item == Some(arandu_middle::symbol_table::LangItem::BlackBox)
        }) {
            let args_slice = self.hir.pool.expr_list(args);
            if let Some(&argument) = args_slice.first() {
                let value = self.lower_expr(argument, None, symbols)?;
                let value_ty = self.hir.pool.expr(argument).ty;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::BlackBox { value, value_ty });
                return Ok(AmirOperand::Copy(dest));
            }
        }

        // `mem.sizeOf<T>()` is Call(Generic(sizeOf, [T]), []) — fold before
        // treating Generic as a callable value (L6.1).
        if let HirExprKind::Generic {
            callee: g_cal,
            args: type_args,
        } = &callee_expr.kind
            && let Some(op) = self
                .try_lower_mem_size_align_intrinsic(*g_cal, type_args, expr.ty, target, symbols)?
        {
            return Ok(op);
        }

        let mut is_enum_ctor = None;
        match &callee_expr.kind {
            HirExprKind::Path { symbol } => {
                let enum_sym_from_ty = match self.resolve_ty(callee_expr.ty) {
                    ArType::Named(id, _) => Some(id),
                    ArType::Func(_, ret) => match self.tc.type_info.type_interner.resolve(ret) {
                        ArType::Named(id, _) => Some(id),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(&tag) = self.tc.type_info.enum_variant_tags.get(symbol).or_else(|| {
                    let enum_id = enum_sym_from_ty?;
                    self.tc
                        .type_info
                        .enum_variants
                        .iter()
                        .find(|&(v_sym, (parent_sym, _))| {
                            *parent_sym == enum_id
                                && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                                && {
                                    let lookup_bare =
                                        symbols.get(*symbol).name.rsplit('.').next().unwrap_or("");
                                    let reg_bare =
                                        symbols.get(*v_sym).name.rsplit('.').next().unwrap_or("");
                                    lookup_bare == reg_bare
                                }
                        })
                        .and_then(|(v_sym, _)| self.tc.type_info.enum_variant_tags.get(v_sym))
                }) {
                    is_enum_ctor = Some(tag);
                }
            }
            HirExprKind::TypePath {
                type_symbol,
                member_symbol,
            } => {
                if let Some(&tag) = self
                    .tc
                    .type_info
                    .enum_variant_tags
                    .get(member_symbol)
                    .or_else(|| {
                        let lookup_bare = symbols
                            .get(*member_symbol)
                            .name
                            .rsplit('.')
                            .next()
                            .unwrap_or("");
                        self.tc
                            .type_info
                            .enum_variants
                            .iter()
                            .find(|&(v_sym, (parent_sym, _))| {
                                *parent_sym == *type_symbol
                                    && symbols.get(*v_sym).name.rsplit('.').next().unwrap_or("")
                                        == lookup_bare
                                    && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                            })
                            .and_then(|(v_sym, _)| self.tc.type_info.enum_variant_tags.get(v_sym))
                    })
                {
                    is_enum_ctor = Some(tag);
                }
            }
            _ => {}
        }

        if let Some(tag) = is_enum_ctor {
            let args_slice = self.hir.pool.expr_list(args);
            let payload_op = match args_slice.len() {
                0 => None,
                1 => {
                    let value = self.lower_expr(args_slice[0], None, symbols)?;
                    Some(self.consume_operand(value)?)
                }
                _ => {
                    let mut item_ops = Vec::with_capacity(args_slice.len());
                    for &arg in args_slice {
                        let value = self.lower_expr(arg, None, symbols)?;
                        item_ops.push(self.consume_operand(value)?);
                    }
                    let param_tys = match self.resolve_ty(callee_expr.ty) {
                        ArType::Func(params, _) => {
                            self.tc.type_info.type_interner.type_args(params)
                        }
                        _ => vec![],
                    };
                    let tuple_ty = ArType::tuple(&param_tys, &self.tc.type_info.type_interner);
                    let dest_tuple = self.new_temp(tuple_ty);
                    self.emit_assign_temp(dest_tuple, AmirRvalue::Tuple { items: item_ops });
                    Some(self.consume_operand(AmirOperand::Copy(dest_tuple))?)
                }
            };
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
            self.emit_assign_temp(
                dest,
                AmirRvalue::EnumConstruct {
                    variant_tag: tag,
                    payload: payload_op,
                },
            );
            return Ok(AmirOperand::Copy(dest));
        }

        let method_target = resolve_method_target(
            callee_expr,
            &self.hir.pool,
            symbols,
            &self.tc.type_info.type_interner,
        );
        let callee_symbol = method_target.or_else(|| match &callee_expr.kind {
            HirExprKind::Path { symbol } => Some(*symbol),
            HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
            HirExprKind::Generic { callee, .. } => match &self.hir.pool.expr(*callee).kind {
                HirExprKind::Path { symbol } => Some(*symbol),
                HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                _ => None,
            },
            _ => None,
        });
        if let Some(callee_symbol) = callee_symbol {
            let callee_name = symbols.get(callee_symbol).name.as_str();
            let is_testing_operation = [
                "expect",
                "expectEqualInt",
                "expectEqualFloat",
                "expectEqualBool",
                "expectEqualStr",
                "fail",
                "skip",
                "log",
                "tempDir",
            ]
            .iter()
            .any(|operation| {
                callee_name == *operation || callee_name.ends_with(&format!(".{operation}"))
            });
            if is_testing_operation
                && let Some(set_span_id) = symbols
                    .iter()
                    .find(|symbol| symbol.name == "ar_test_set_span")
                    .map(|symbol| symbol.id)
            {
                let file_id =
                    AmirOperand::Constant(self.intern_literal_int(expr.span.file_id.to_string()));
                let start =
                    AmirOperand::Constant(self.intern_literal_int(expr.span.start.to_string()));
                let end = AmirOperand::Constant(self.intern_literal_int(expr.span.end.to_string()));
                self.push_stmt(AmirStmt::Call {
                    lhs: None,
                    callee: AmirOperand::FunctionRef(set_span_id),
                    args: smallvec::smallvec![file_id, start, end],
                    return_borrow: None,
                });
            }
        }
        let callee_op = if let Some(method_symbol) = method_target {
            AmirOperand::FunctionRef(method_symbol)
        } else {
            self.lower_expr(callee, None, symbols)?
        };
        let args_slice = self.hir.pool.expr_list(args);
        // Method calls: HIR already includes the receiver as arg 0 when
        // typeck rewrites `obj.m(a)` → Call(Field(m), [obj, a]). Only inject
        // `base` when args are short of the formal arity (legacy / incomplete HIR).
        let callee_resolved_ty = self.resolve_ty(callee_expr.ty);
        let callee_decl_ty = callee_symbol.and_then(|sym| self.tc.type_info.decl_type(sym));
        let formal_params: Vec<ArType> =
            match callee_decl_ty.as_ref().unwrap_or(&callee_resolved_ty) {
                ArType::Func(params, _) => self
                    .tc
                    .type_info
                    .type_interner
                    .type_args(*params)
                    .iter()
                    .map(|&id| self.resolve_ty(id))
                    .collect(),
                _ => Vec::new(),
            };
        let mut arg_ops = Vec::with_capacity(args_slice.len() + 1);
        let inject_receiver = method_target.is_some()
            && !formal_params.is_empty()
            && args_slice.len() < formal_params.len();
        // Arg consume modes from the post-mono `CalleeArgModes` table (O(1)).
        let callee_for_modes = callee_symbol.unwrap_or(crate::SymbolId::DUMMY);
        if inject_receiver
            && let HirExprKind::Field { base, .. } | HirExprKind::SafeField { base, .. } =
                &callee_expr.kind
        {
            let formal0 = formal_params.first();
            arg_ops.push(self.lower_call_arg(*base, 0, callee_for_modes, formal0, symbols)?);
        }
        let arg_param_offset = if inject_receiver { 1 } else { 0 };
        for (i, &arg) in args_slice.iter().enumerate() {
            let arg_expr = self.hir.pool.expr(arg);
            let formal_i = i + arg_param_offset;
            let formal = formal_params.get(formal_i);
            let arg_op = self.lower_call_arg(arg, formal_i, callee_for_modes, formal, symbols)?;
            let arg_op = if let Some(param_ty) = formal {
                if matches!(param_ty, ArType::Primitive(Primitive::Str)) {
                    self.maybe_to_str(arg_op, arg_expr.ty)?
                } else {
                    arg_op
                }
            } else {
                arg_op
            };
            arg_ops.push(arg_op);
        }
        let dest = if self.with_ty(expr.ty, |t| matches!(t, ArType::Void)) {
            None
        } else {
            Some(target.unwrap_or_else(|| self.new_temp_id(expr.ty)))
        };
        if let Some(symbol) = callee_symbol
            && symbols.get(symbol).lang_item
                == Some(arandu_middle::symbol_table::LangItem::SimdIntrinsic)
            && let Some(intrinsic) =
                arandu_middle::intrinsics::SimdIntrinsic::from_name(&symbols.get(symbol).name)
        {
            use arandu_middle::intrinsics::SimdOperation;
            // Constants must become typed SSA values: vector validation cannot
            // infer the scalar type of a literal pool entry in isolation.
            for (argument, formal) in arg_ops.iter_mut().zip(&formal_params) {
                if matches!(argument, AmirOperand::Constant(_)) {
                    let temp = self.new_temp(formal.clone());
                    self.emit_assign_temp(temp, AmirRvalue::Use(*argument));
                    *argument = AmirOperand::Copy(temp);
                }
            }
            let value = match (intrinsic.operation, arg_ops.as_slice()) {
                (SimdOperation::Splat, [value]) => AmirRvalue::VectorSplat { value: *value },
                (SimdOperation::Binary(op), [left, right]) => AmirRvalue::VectorOp {
                    op,
                    left: *left,
                    right: *right,
                },
                (SimdOperation::Extract(lane), [vector]) => AmirRvalue::VectorExtractLane {
                    vector: *vector,
                    lane,
                },
                (SimdOperation::Insert(lane), [vector, value]) => AmirRvalue::VectorInsertLane {
                    vector: *vector,
                    lane,
                    value: *value,
                },
                (SimdOperation::Compare(op), [left, right]) => AmirRvalue::VectorCompare {
                    op,
                    left: *left,
                    right: *right,
                },
                (SimdOperation::Select, [mask, if_true, if_false]) => AmirRvalue::VectorSelect {
                    mask: *mask,
                    if_true: *if_true,
                    if_false: *if_false,
                },
                _ => {
                    return Err(Diagnostic::ice(
                        DiagCode::ICEGEN001,
                        "invalid SIMD intrinsic arity after type checking",
                        expr.span,
                    ));
                }
            };
            let Some(dest) = dest else {
                return Err(Diagnostic::ice(
                    DiagCode::ICEGEN001,
                    "SIMD intrinsic has no value destination",
                    expr.span,
                ));
            };
            self.emit_assign_temp(dest, value);
            return Ok(AmirOperand::Copy(dest));
        }
        if let Some(symbol) = callee_symbol {
            let name = symbols.get(symbol).name.as_str();
            let kind = arandu_middle::IntrinsicKind::from_declared_name(
                name,
                symbols.get(symbol).lang_item,
            );
            if kind == Some(arandu_middle::IntrinsicKind::DropInPlace)
                && let (Some(&arg), [pointer]) = (args_slice.first(), arg_ops.as_slice())
            {
                let pointer_ty = self.resolve_ty(self.hir.pool.expr(arg).ty);
                if matches!(
                    pointer_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                ) {
                    let pointer_local = self.new_compiler_local(pointer_ty);
                    self.mark_local_materialized(pointer_local);
                    self.push_stmt(AmirStmt::Store {
                        lhs: AmirPlace {
                            local: pointer_local,
                            projections: smallvec::SmallVec::new(),
                        },
                        rhs: *pointer,
                    });
                    self.push_stmt(AmirStmt::Destroy(AmirPlace {
                        local: pointer_local,
                        projections: smallvec::smallvec![AmirProjection::Deref],
                    }));
                    return Ok(AmirOperand::Constant(AmirConstant::Nil));
                }
            }
            let intrinsic = match (kind, arg_ops.as_slice()) {
                (Some(arandu_middle::IntrinsicKind::AddressOf), [value]) => {
                    Some(AmirRvalue::Use(*value))
                }
                (Some(arandu_middle::IntrinsicKind::SliceFromRaw), [owner, data, len]) => {
                    Some(AmirRvalue::SliceView {
                        owner: *owner,
                        data: *data,
                        len: *len,
                    })
                }
                (Some(arandu_middle::IntrinsicKind::SliceSubslice), [slice, start, len]) => {
                    Some(AmirRvalue::SliceSubslice {
                        slice: *slice,
                        start: *start,
                        len: *len,
                    })
                }
                (Some(arandu_middle::IntrinsicKind::SliceLen), [slice]) => {
                    Some(AmirRvalue::Len(*slice))
                }
                (Some(arandu_middle::IntrinsicKind::SliceData), [slice]) => {
                    Some(AmirRvalue::SliceData(*slice))
                }
                (Some(arandu_middle::IntrinsicKind::StrBytes), [source]) => {
                    Some(AmirRvalue::StrBytes { source: *source })
                }
                (Some(arandu_middle::IntrinsicKind::StrView), [owner]) => {
                    Some(AmirRvalue::StrView { owner: *owner })
                }
                _ => None,
            };
            if let (Some(intrinsic), Some(dest)) = (intrinsic, dest) {
                self.emit_assign_temp(dest, intrinsic);
                return Ok(AmirOperand::Copy(dest));
            }
        }
        self.push_stmt(AmirStmt::Call {
            lhs: dest,
            callee: callee_op,
            args: arg_ops.into(),
            return_borrow: callee_symbol
                .and_then(|symbol| self.tc.type_info.return_borrow_summaries.get(&symbol))
                .cloned(),
        });
        Ok(dest.map_or(AmirOperand::Constant(AmirConstant::Nil), AmirOperand::Copy))
    }

    pub(super) fn try_lower_mem_size_align_intrinsic(
        &mut self,
        callee: HirExprId,
        type_args: &[crate::types::TypeId],
        result_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<Option<AmirOperand>, Diagnostic> {
        if type_args.len() != 1 {
            return Ok(None);
        }
        let callee_expr = self.hir.pool.expr(callee);
        let symbol = match &callee_expr.kind {
            HirExprKind::Path { symbol } => *symbol,
            HirExprKind::TypePath { member_symbol, .. } => *member_symbol,
            _ => return Ok(None),
        };
        // Identity comes from the retained intrinsic declaration, never merely
        // from spelling. Ordinary generic functions may legally share the name.
        let intrinsic_declared = self.hir.decls.iter().any(|&id| {
            let crate::hir::HirDecl::Extern(declaration) = self.hir.pool.decl(id) else {
                return false;
            };
            declaration.is_intrinsic()
                && self
                    .hir
                    .pool
                    .func_signatures_list(declaration.members)
                    .iter()
                    .any(|member| member.symbol == symbol)
        });
        if !intrinsic_declared {
            return Ok(None);
        }
        let name = symbols.get(symbol).name.as_str();
        let kind = arandu_middle::IntrinsicKind::from_name(name);
        let bare = match kind {
            Some(arandu_middle::IntrinsicKind::SizeOf) => "sizeOf",
            Some(arandu_middle::IntrinsicKind::AlignOf) => "alignOf",
            _ => return Ok(None),
        };

        let query = if bare == "sizeOf" {
            crate::hir::LayoutQuery::Size
        } else {
            crate::hir::LayoutQuery::Align
        };
        self.lower_layout_query(query, type_args[0], result_ty, target, callee_expr.span)
            .map(Some)
    }

    pub(crate) fn lower_layout_query(
        &mut self,
        query: crate::hir::LayoutQuery,
        operand_ty: crate::types::TypeId,
        result_ty: crate::types::TypeId,
        target: Option<TempId>,
        span: arandu_lexer::Span,
    ) -> Result<AmirOperand, Diagnostic> {
        let ty = self.resolve_ty(operand_ty);
        let engine = arandu_middle::layout::LayoutEngine::from_data_layout(self.layout);
        let layout = engine
            .layout_of_type(
                &ty,
                &self.tc.type_info.type_interner,
                self.tc.type_info.as_ref(),
            )
            .map_err(|error| {
                Diagnostic::error(
                    DiagCode::T047InvalidTypeLayout,
                    format!("cannot compute @{} for this type: {error}", query.name()),
                    span,
                )
                .with_primary_label("type has no concrete target layout")
            })?;
        let value = if query == crate::hir::LayoutQuery::Size {
            layout.size
        } else {
            layout.align
        };

        let lit = self.intern_literal_int(value.to_string());
        let dest = target.unwrap_or_else(|| self.new_temp_id(result_ty));
        self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Constant(lit)));
        Ok(AmirOperand::Copy(dest))
    }
}

/// Structural refinement of `type_may_carry_borrow` for call results:
/// `Option<usize>` or `Result<bool, int>` cannot alias an argument string.
/// Named aggregates and tuples stay conservative.
fn result_may_hold_view(ty: crate::types::TypeId, interner: &crate::types::TypeInterner) -> bool {
    match interner.try_resolve(ty) {
        Some(
            ArType::Option(inner)
            | ArType::Nullable(inner)
            | ArType::Array(_, inner)
            | ArType::ConstArray(_, inner),
        ) => result_may_hold_view(inner, interner),
        Some(ArType::Result(ok, err)) => {
            result_may_hold_view(ok, interner) || result_may_hold_view(err, interner)
        }
        _ => crate::borrow_facts::type_may_carry_borrow(ty, interner),
    }
}
