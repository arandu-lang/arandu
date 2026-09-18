use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use smallvec::SmallVec;

use super::synth_expr;
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{self, ArType, Primitive};
use arandu_middle::types::type_interner::TypeId;

/// Infer `struct` type arguments from field initializers.
///
/// For each type parameter `P`, finds a field whose template type is exactly
/// `Named(P, [])` and uses the field value's type. Returns `None` if any
/// parameter cannot be resolved to a non-error concrete type.
fn infer_struct_type_args(
    checker: &mut TypeChecker<'_>,
    params: &[arandu_middle::SymbolId],
    template_fields: &arandu_middle::layout::StructFields,
    field_ids: &[arandu_parser::ast_pool::FieldInitId],
) -> Option<Vec<TypeId>> {
    let mut out = Vec::with_capacity(params.len());
    for &param in params {
        let mut found: Option<TypeId> = None;
        for &fid in field_ids {
            let field = checker.pool.field_init(fid);
            let Some(tmpl_tid) = template_fields.get(field.name.as_str()).map(|f| f.ty) else {
                continue;
            };
            let tmpl = checker.resolve(tmpl_tid);
            let matches_param = matches!(
                tmpl,
                ArType::Named(id, ref args) if id == param && args.is_empty()
            );
            if !matches_param {
                continue;
            }
            let Some(val_tid) = peek_value_type(checker, field.value) else {
                continue;
            };
            found = Some(val_tid);
            break;
        }
        out.push(found?);
    }
    Some(out)
}

/// Side-effect-free type peek for a struct-literal field value, used only to
/// infer missing generic type arguments.
///
/// Unlike `synth_expr`, this never allocates literal variables, registers
/// constraints or emits diagnostics: the real field synthesis (which checks
/// field values against the instantiated struct type) runs exactly once in
/// the field loop below. Resolving the peek through the full synthesizer
/// caused every matching field to be synthesized twice — duplicated
/// diagnostics from erroneous sub-expressions and orphaned literal variables.
fn peek_value_type(checker: &mut TypeChecker<'_>, value: ExprId) -> Option<TypeId> {
    let pool = checker.pool;
    match pool.expr(value) {
        ExprKind::Int { .. } => Some(checker.intern(ArType::IntLiteral)),
        ExprKind::Float { .. } => Some(checker.intern(ArType::FloatLiteral)),
        ExprKind::Bool { .. } => Some(checker.intern(ArType::Primitive(Primitive::Bool))),
        ExprKind::Char { .. } => Some(checker.intern(ArType::Primitive(Primitive::Char))),
        ExprKind::InterpolatedString { .. } => {
            Some(checker.intern(ArType::Primitive(Primitive::Str)))
        }
        ExprKind::Group { expr } => peek_value_type(checker, *expr),
        ExprKind::Unary {
            op: arandu_parser::UnaryOp::Neg,
            expr,
        } => peek_value_type(checker, *expr),
        // A plain path resolves to a declared/looked-up type without any
        // side effects — mirroring how the value-Path arm synthesizes. This
        // covers function-valued generic fields (`Job { callback: count }`
        // where `count` is a `func(int) int`), which a returning
        // `synth_expr` preserved.
        ExprKind::Path { .. } => {
            let symbol_id = checker.resolved.expr_symbol(value)?;
            checker
                .ctx
                .lookup(symbol_id)
                .or_else(|| checker.decl_type_id(symbol_id))
        }
        _ => None,
    }
}

/// Stricter than `unify` for array literals: int and float literals must not mix.
pub(super) fn array_element_types_compatible(
    a: &ArType,
    b: &ArType,
    interner: &arandu_middle::types::TypeInterner,
) -> bool {
    if matches!(
        (a, b),
        (ArType::IntLiteral, ArType::FloatLiteral) | (ArType::FloatLiteral, ArType::IntLiteral)
    ) {
        return false;
    }
    types::unify(a, b, interner)
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, expr))]
pub(super) fn synth_literal_expr(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    kind: &ExprKind,
    span: Span,
    expected: Option<TypeId>,
) -> Option<TypeId> {
    match kind {
        ExprKind::Int { value, .. } => {
            if let Some(exp_id) = expected
                && let ArType::Primitive(p) = checker.resolve(exp_id)
            {
                if p.is_float() {
                    return Some(exp_id);
                }
                if p.is_integer()
                    && let Some(parsed) = arandu_middle::literal_pool::parse_int_literal(value)
                {
                    let fits = match p {
                        Primitive::I8 => (i8::MIN as i128..=i8::MAX as i128).contains(&parsed),
                        Primitive::I16 => (i16::MIN as i128..=i16::MAX as i128).contains(&parsed),
                        Primitive::I32 => (i32::MIN as i128..=i32::MAX as i128).contains(&parsed),
                        Primitive::I64 => (i64::MIN as i128..=i64::MAX as i128).contains(&parsed),
                        Primitive::Int => {
                            // int é pointer-width signed; o range depende do target.
                            (checker.target_info.int_min()..=checker.target_info.int_max())
                                .contains(&parsed)
                        }
                        Primitive::U8 | Primitive::Byte => (0..=u8::MAX as i128).contains(&parsed),
                        Primitive::U16 => (0..=u16::MAX as i128).contains(&parsed),
                        Primitive::U32 => (0..=u32::MAX as i128).contains(&parsed),
                        Primitive::U64 => parsed >= 0 && (parsed as u128 <= u64::MAX as u128),
                        Primitive::Uint => {
                            // uint é pointer-width unsigned; o range depende do target.
                            parsed >= 0 && (parsed as u128 <= checker.target_info.uint_max())
                        }
                        _ => true,
                    };
                    if !fits {
                        checker.diagnostics.push(
                            crate::Diagnostic::error(
                                crate::DiagCode::T038IntegerLiteralOutOfRange,
                                format!(
                                    "integer literal `{value}` does not fit in `{}`",
                                    p.as_str()
                                ),
                                span,
                            )
                            .with_label(
                                span,
                                format!("value is outside the range of `{}`", p.as_str()),
                            )
                            .with_hint("use a wider integer type or change the literal value"),
                        );
                    }
                    // Keep the contextual type after reporting. Falling back to
                    // `IntLiteral` would make generic literal unification accept
                    // the same out-of-range value silently.
                    return Some(exp_id);
                }
            }
            let var_id = checker.literal_table.alloc(
                crate::type_checker::solver::LiteralKind::Int,
                Some(crate::type_checker::solver::LiteralOccurrence {
                    expr,
                    span,
                    raw: value.to_string(),
                }),
            );
            checker.literal_table.bind_expr(expr, var_id);
            Some(checker.intern(ArType::IntLiteral))
        }
        ExprKind::Float { value, .. } => {
            if let Some(exp_id) = expected
                && let ArType::Primitive(p) = checker.resolve(exp_id)
                && p.is_float()
            {
                return Some(exp_id);
            }
            let var_id = checker.literal_table.alloc(
                crate::type_checker::solver::LiteralKind::Float,
                Some(crate::type_checker::solver::LiteralOccurrence {
                    expr,
                    span,
                    raw: value.to_string(),
                }),
            );
            checker.literal_table.bind_expr(expr, var_id);
            Some(checker.intern(ArType::FloatLiteral))
        }
        ExprKind::Bool { .. } => Some(checker.intern(ArType::Primitive(Primitive::Bool))),
        ExprKind::Char { .. } => Some(checker.intern(ArType::Primitive(Primitive::Char))),
        ExprKind::InterpolatedString { parts } => {
            // ToStr v0.1: formatable primitives are accepted; lower inserts
            // AmirRvalue::ToStr. Non-formatable types get T034 (not silent Any).
            let part_ids = checker.pool.string_part_list(*parts).to_vec();
            let str_ty = checker.intern(ArType::Primitive(Primitive::Str));
            for part_id in part_ids {
                if let arandu_parser::StringPart::Expr {
                    expr: inner_expr, ..
                } = checker.pool.string_part(part_id)
                {
                    let part_ty_id = synth_expr(checker, *inner_expr);
                    let part_ty = checker.resolve(part_ty_id);
                    if part_ty.is_error() || part_ty.is_to_str_v01() {
                        continue;
                    }
                    let interner = &checker.type_info.type_interner;
                    let found = part_ty.display(&checker.symbols, interner);
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T034CannotFormat,
                            format!("cannot format value of type `{found}` as `str`"),
                            checker.pool.expr_span(*inner_expr),
                        )
                        .with_note(
                            "only bool, integers, floats, char, and str are supported in v0.1"
                                .to_string(),
                        ),
                    );
                }
            }
            Some(str_ty)
        }
        ExprKind::Nil => {
            // SYN.3: `nil` as Option.None / Nullable empty when context is known.
            // Prefer expected type (assignment, arg, return via synth_expr_expected),
            // then the current function return type.
            let context_id = expected.or_else(|| checker.ctx.current_return());
            if let Some(ctx_id) = context_id {
                let ctx = checker.resolve(ctx_id);
                if types::is_err_type(&ctx, &checker.type_info.type_interner) {
                    let err_id = checker.intern(ArType::Error);
                    Some(checker.intern(ArType::Nullable(err_id)))
                } else if types::is_tryable_type(&ctx, &checker.type_info.type_interner)
                    || matches!(ctx, ArType::Option(_) | ArType::Nullable(_))
                {
                    // Option / Result / Nullable / tryable → nil fills the context type
                    // (AMIR lowers Option-typed nil to EnumConstruct None).
                    Some(ctx_id)
                } else {
                    Some(checker.intern(ArType::Nullable(ctx_id)))
                }
            } else {
                let err_id = checker.intern(ArType::Error);
                Some(checker.intern(ArType::Nullable(err_id)))
            }
        }
        ExprKind::StructLiteral { ty, fields } => {
            let ty_id = *ty;
            let fields_range = *fields;
            let struct_ty = checker.lower_type_expr(ty_id, checker.type_scope());
            let mut struct_ty_id = checker.intern(struct_ty);
            let struct_info = match checker.resolve(struct_ty_id) {
                ArType::Named(symbol_id, generic_args) => Some((symbol_id, generic_args)),
                _ => None,
            };
            if let Some((symbol_id, generic_args)) = struct_info {
                let mut generic_args = checker.type_info.type_interner.type_args(generic_args);
                let field_ids = checker.pool.field_init_list(fields_range).to_vec();

                // Infer missing type args from field values: `BoxG { v: 42 }` → `BoxG<int>`.
                if generic_args.is_empty()
                    && let Some(params) = checker.type_info.generic_params.get(&symbol_id).cloned()
                    && !params.is_empty()
                    && let Some(template_fields) =
                        checker.type_info.struct_fields.get(&symbol_id).cloned()
                    && let Some(inferred) = infer_struct_type_args(
                        checker,
                        &params,
                        template_fields.as_ref(),
                        &field_ids,
                    )
                {
                    generic_args = inferred;
                    let concrete =
                        ArType::named(symbol_id, &generic_args, &checker.type_info.type_interner);
                    struct_ty_id = checker.intern(concrete);
                }

                let resolved_args: Vec<ArType> = generic_args
                    .iter()
                    .map(|&arg_id| checker.resolve(arg_id))
                    .collect();
                let field_map =
                    types::struct_fields_instantiated(checker, symbol_id, &resolved_args).or_else(
                        || {
                            checker
                                .type_info
                                .struct_fields
                                .get(&symbol_id)
                                .map(|fields| {
                                    fields
                                        .iter()
                                        .map(|f| (f.name.to_string(), checker.resolve(f.ty)))
                                        .collect()
                                })
                        },
                    );

                let mut seen_fields = SmallVec::<[(&str, Span); 8]>::new();
                for &fid in &field_ids {
                    let field = checker.pool.field_init(fid);
                    if let Some((_, prev_span)) =
                        seen_fields.iter().find(|(name, _)| *name == field.name)
                    {
                        let diag = crate::Diagnostic::error(
                            crate::DiagCode::T028DuplicateFieldInit,
                            format!("field '{}' initialized more than once", field.name),
                            field.span,
                        )
                        .with_label(*prev_span, "first initialization here")
                        .with_label(field.span, "duplicate initialization");
                        checker.diagnostics.push(diag);
                    } else {
                        seen_fields.push((&field.name, field.span));
                    }
                }

                if let Some(fields_def) = field_map {
                    let mut has_update_base = false;
                    for fid in &field_ids {
                        let field = checker.pool.field_init(*fid);
                        if field.name == ".." {
                            has_update_base = true;
                            let base_ty_id = super::super::synth_expr_expected(
                                checker,
                                field.value,
                                Some(struct_ty_id),
                            );
                            let base_ty = checker.resolve(base_ty_id);
                            let expected_struct_ty = checker.resolve(struct_ty_id);
                            if !types::unify(
                                &expected_struct_ty,
                                &base_ty,
                                &checker.type_info.type_interner,
                            ) {
                                checker.add_constraint(
                                    expected_struct_ty,
                                    base_ty_id,
                                    ConstraintOrigin::FieldInit {
                                        struct_span: span,
                                        field_name: "..".to_string(),
                                        field_span: field.span,
                                        value_span: checker.pool.expr_span(field.value),
                                    },
                                );
                            }
                            continue;
                        }
                        let defined_field_ty_opt = fields_def.get(field.name.as_str()).cloned();
                        // `nil` in a field needs the field's expected type (`ptr[T]`, `T?`),
                        // not the enclosing function return (which produced bogus `int?` /
                        // `Vec?` for `data: nil` in Vec / BoxG).
                        let expected_id_opt = defined_field_ty_opt
                            .as_ref()
                            .map(|ty| checker.intern(ty.clone()));
                        let field_val_ty_id =
                            if matches!(checker.pool.expr(field.value), ExprKind::Nil)
                                && let Some(expected_id) = expected_id_opt
                            {
                                checker.type_info.record_expr_type(field.value, expected_id);
                                expected_id
                            } else {
                                super::super::synth_expr_expected(
                                    checker,
                                    field.value,
                                    expected_id_opt,
                                )
                            };
                        if let Some(defined_field_ty) = defined_field_ty_opt {
                            if let Some(var_id) = checker.literal_table.var_for_expr(field.value) {
                                let exp_id = checker.intern(defined_field_ty.clone());
                                checker.constrain_literal_var(
                                    var_id,
                                    exp_id,
                                    ConstraintOrigin::FieldInit {
                                        struct_span: span,
                                        field_name: field.name.to_string(),
                                        field_span: field.span,
                                        value_span: checker.pool.expr_span(field.value),
                                    },
                                );
                            }
                            let field_val_ty = checker.resolve(field_val_ty_id);
                            if !types::unify(
                                &defined_field_ty,
                                &field_val_ty,
                                &checker.type_info.type_interner,
                            ) {
                                checker.add_constraint(
                                    defined_field_ty,
                                    field_val_ty_id,
                                    ConstraintOrigin::FieldInit {
                                        struct_span: span,
                                        field_name: field.name.to_string(),
                                        field_span: field.span,
                                        value_span: checker.pool.expr_span(field.value),
                                    },
                                );
                            }
                        } else {
                            checker.add_constraint(
                                struct_ty_id,
                                ArType::Error,
                                ConstraintOrigin::UndefinedField {
                                    base_span: checker.pool.type_expr_span(ty_id),
                                    field_span: field.span,
                                    field_name: field.name.to_string(),
                                },
                            );
                        }
                    }

                    if !has_update_base {
                        let mut missing_fields = Vec::new();
                        for def_name in fields_def.keys() {
                            if !seen_fields.iter().any(|(name, _)| name == def_name) {
                                missing_fields.push(format!("`{def_name}`"));
                            }
                        }
                        if !missing_fields.is_empty() {
                            missing_fields.sort();
                            let missing_str = missing_fields.join(", ");
                            let struct_name = checker.symbols.get(symbol_id).name.clone();
                            let diag = crate::Diagnostic::error(
                                crate::DiagCode::T027MissingStructFields,
                                format!("missing fields {missing_str} in struct initializer"),
                                span,
                            )
                            .with_label(span, format!("instantiating struct '{struct_name}' here"));
                            checker.diagnostics.push(diag);
                        }
                    }
                } else {
                    for fid in field_ids {
                        let field = checker.pool.field_init(fid);
                        let _ = synth_expr(checker, field.value);
                    }
                }
            } else {
                let field_ids = checker.pool.field_init_list(fields_range).to_vec();
                for fid in field_ids {
                    let field = checker.pool.field_init(fid);
                    let _ = synth_expr(checker, field.value);
                }
            }
            Some(struct_ty_id)
        }
        ExprKind::Array { items } => {
            let items_range = *items;
            let expected_elem_id = expected.and_then(|exp_id| match checker.resolve(exp_id) {
                ArType::Array(_, elem_id) | ArType::Slice(elem_id) => Some(elem_id),
                _ => None,
            });
            let error_id = checker.intern(ArType::Error);
            let item_ids = checker.pool.expr_list(items_range).to_vec();

            if item_ids.is_empty() {
                let elem_id = expected_elem_id.unwrap_or(error_id);
                return Some(checker.intern(ArType::Array(0, elem_id)));
            }

            // Pass 1: synthesize each element with expected_elem_id (or discovered concrete type).
            let mut item_ty_ids = Vec::with_capacity(item_ids.len());
            let mut discovered_concrete =
                expected_elem_id.filter(|&id| !checker.resolve(id).is_literal());

            for &item_id in &item_ids {
                let item_ty_id = super::synth_expr_expected(checker, item_id, discovered_concrete);
                let item_ty = checker.resolve(item_ty_id);
                if discovered_concrete.is_none() && !item_ty.is_literal() && !item_ty.is_error() {
                    discovered_concrete = Some(item_ty_id);
                }
                item_ty_ids.push(item_ty_id);
            }

            // Unify literal variables across array elements
            let mut first_var: Option<crate::type_checker::solver::TypeVarId> = None;
            // Set when merging the groups widens a promoted integer literal to
            // float; the compatibility pass below must not report the same
            // int/float mismatch again as T002.
            let mut reported_widening = false;
            for &item_id in &item_ids {
                if let Some(var_id) = checker.literal_table.var_for_expr(item_id) {
                    if let Some(fv) = first_var {
                        let item_span = checker.pool.expr_span(item_id);
                        if checker.report_promoted_widening(fv, var_id, item_span) {
                            reported_widening = true;
                        }
                        first_var = Some(checker.literal_table.unify(fv, var_id));
                    } else {
                        first_var = Some(var_id);
                    }
                }
            }
            if let Some(var_id) = first_var
                && let Some(concrete_id) = discovered_concrete
            {
                checker.constrain_literal_var(
                    var_id,
                    concrete_id,
                    ConstraintOrigin::ArrayLiteral {
                        array_span: span,
                        item_span: span,
                        item_index: 0,
                    },
                );
            }

            // Determine element type and validate compatibility
            let target_elem_id = discovered_concrete.unwrap_or_else(|| item_ty_ids[0]);
            let mut elem_ty_id = target_elem_id;

            for (i, (&item_id, &item_ty_id)) in item_ids.iter().zip(&item_ty_ids).enumerate() {
                let elem_ty = checker.resolve(elem_ty_id);
                let item_ty = checker.resolve(item_ty_id);
                if !array_element_types_compatible(
                    &elem_ty,
                    &item_ty,
                    &checker.type_info.type_interner,
                ) {
                    // A promoted int/float pair was already reported as an
                    // implicit widening when the literal groups were merged.
                    let already_reported = reported_widening
                        && matches!(
                            (&elem_ty, &item_ty),
                            (ArType::IntLiteral, ArType::FloatLiteral)
                                | (ArType::FloatLiteral, ArType::IntLiteral)
                        );
                    if !already_reported {
                        checker.add_constraint(
                            elem_ty_id,
                            item_ty_id,
                            ConstraintOrigin::ArrayLiteral {
                                array_span: span,
                                item_span: checker.pool.expr_span(item_id),
                                item_index: i,
                            },
                        );
                    }
                    elem_ty_id = error_id;
                }
            }

            Some(checker.intern(ArType::Array(items_range.len as u64, elem_ty_id)))
        }
        _ => None,
    }
}
