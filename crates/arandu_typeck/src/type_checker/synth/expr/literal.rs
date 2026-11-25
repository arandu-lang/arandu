use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use smallvec::SmallVec;

use super::synth_expr;
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{self, ArType, Primitive};
use crate::{CodeReplacement, Hint};
use arandu_middle::types::type_interner::TypeId;

/// Infer `struct` type arguments from field initializers.
///
/// Each field's template type is matched structurally against the peeked
/// type of its value, so `Pair { first: Fresh {..} }` binds `T` = `Fresh` and
/// `Wrap { items: vec }` binds `T` from `Vec<T>`. Returns `None` if any
/// parameter stays unbound.
fn infer_struct_type_args(
    checker: &mut TypeChecker<'_>,
    params: &[arandu_middle::SymbolId],
    template_fields: &arandu_middle::layout::StructFields,
    field_ids: &[arandu_parser::ast_pool::FieldInitId],
) -> Option<Vec<TypeId>> {
    let mut bindings = rustc_hash::FxHashMap::default();
    for &fid in field_ids {
        let field = checker.pool.field_init(fid);
        let Some(template) = template_fields.get(field.name.as_str()).map(|f| f.ty) else {
            continue;
        };
        let Some(value) = peek_value_type(checker, field.value) else {
            continue;
        };
        if checker.resolve(value).is_error() {
            continue;
        }
        let template = checker.resolve(template);
        super::call::bind_type_params(checker, params, &template, value, &mut bindings);
    }
    params
        .iter()
        .map(|param| bindings.get(param).copied())
        .collect()
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
        ExprKind::Byte { .. } => Some(checker.intern(ArType::Primitive(Primitive::U8))),
        ExprKind::ByteString { .. } => {
            let u8_id = checker.intern(ArType::Primitive(Primitive::U8));
            Some(checker.intern(ArType::Slice(u8_id)))
        }
        ExprKind::CString { .. } => {
            let u8_id = checker.intern(ArType::Primitive(Primitive::U8));
            Some(checker.intern(ArType::Ptr(u8_id)))
        }
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
        // A nested literal names its type, or infers it the same way.
        ExprKind::StructLiteral { ty, fields } => {
            let (ty, fields) = (*ty, *fields);
            // Lowering a type may report; the real synthesis reports once.
            let mark = checker.diagnostics.len();
            let lowered = checker.lower_type_expr(ty, checker.type_scope());
            checker.diagnostics.truncate(mark);
            let ArType::Named(symbol, args) = lowered else {
                return None;
            };
            if checker.type_info.enum_variants.contains_key(&symbol) {
                return None;
            }
            if !checker.type_info.type_interner.type_args(args).is_empty() {
                return Some(checker.intern(ArType::Named(symbol, args)));
            }
            let Some(params) = checker
                .type_info
                .generic_params
                .get(&symbol)
                .cloned()
                .filter(|params| !params.is_empty())
            else {
                return Some(checker.intern(ArType::Named(symbol, args)));
            };
            let template = checker.type_info.struct_fields.get(&symbol).cloned()?;
            let field_ids = checker.pool.field_init_list(fields).to_vec();
            let inferred = infer_struct_type_args(checker, &params, &template, &field_ids)?;
            let named = ArType::named(symbol, &inferred, &checker.type_info.type_interner);
            Some(checker.intern(named))
        }
        // A call to a plain function has its declared result.
        ExprKind::Call { callee, .. } => {
            let symbol = checker.resolved.expr_symbol(*callee)?;
            if checker
                .type_info
                .generic_params
                .get(&symbol)
                .is_some_and(|params| !params.is_empty())
            {
                return None;
            }
            match checker.decl_type(symbol)? {
                ArType::Func(_, ret) => Some(ret),
                _ => None,
            }
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
            let expected_prim = expected.and_then(|exp_id| match checker.resolve(exp_id) {
                ArType::Primitive(p) => Some((exp_id, p)),
                ArType::Option(inner) => match checker.resolve(inner) {
                    ArType::Primitive(p) => Some((inner, p)),
                    _ => None,
                },
                _ => None,
            });
            if let Some((exp_id, p)) = expected_prim {
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
                            // int is fixed 32-bit signed (RFC 0023).
                            (checker.target_info.int_min()..=checker.target_info.int_max())
                                .contains(&parsed)
                        }
                        Primitive::ISize => {
                            // isize is pointer-width signed (RFC 0023).
                            (checker.target_info.isize_min()..=checker.target_info.isize_max())
                                .contains(&parsed)
                        }
                        Primitive::U8 | Primitive::Byte => (0..=u8::MAX as i128).contains(&parsed),
                        Primitive::U16 => (0..=u16::MAX as i128).contains(&parsed),
                        Primitive::U32 => (0..=u32::MAX as i128).contains(&parsed),
                        Primitive::U64 => parsed >= 0 && (parsed as u128 <= u64::MAX as u128),
                        Primitive::Uint => {
                            // uint is fixed 32-bit unsigned (RFC 0023).
                            parsed >= 0 && (parsed as u128 <= checker.target_info.uint_max())
                        }
                        Primitive::USize => {
                            // usize is pointer-width unsigned (RFC 0023).
                            parsed >= 0 && (parsed as u128 <= checker.target_info.usize_max())
                        }
                        _ => true,
                    };
                    if !fits {
                        let mut diag = crate::Diagnostic::error(
                            crate::DiagCode::T038IntegerLiteralOutOfRange,
                            format!("integer literal `{value}` does not fit in `{}`", p.as_str()),
                            span,
                        )
                        .with_label(
                            span,
                            format!("value is outside the range of `{}`", p.as_str()),
                        );
                        // Only a direct literal under a known annotation gets a
                        // replacement: changing `int` elsewhere would not fix
                        // literals nested in calls or aggregates.
                        if p == Primitive::Int
                            && let Some(annotation) = checker.expected_type_span
                            && annotation.file_id == span.file_id
                            && annotation.start != annotation.end
                        {
                            let wider = if parsed <= i64::MAX as i128 {
                                "i64"
                            } else {
                                "u64"
                            };
                            diag = diag.with_hint_replacement(Hint {
                                message: format!(
                                    "change the type annotation from `int` to `{wider}`"
                                ),
                                replacement: Some(CodeReplacement {
                                    span: annotation,
                                    new_text: wider.to_owned(),
                                }),
                            });
                        } else {
                            diag = diag
                                .with_hint("use a wider integer type or change the literal value");
                        }
                        checker.diagnostics.push(diag);
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
            let expected_float = expected.and_then(|exp_id| match checker.resolve(exp_id) {
                ArType::Primitive(p) if p.is_float() => Some(exp_id),
                ArType::Option(inner)
                    if matches!(checker.resolve(inner), ArType::Primitive(p) if p.is_float()) =>
                {
                    Some(inner)
                }
                _ => None,
            });
            if let Some(exp_id) = expected_float {
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
        ExprKind::Byte { .. } => Some(checker.intern(ArType::Primitive(Primitive::U8))),
        ExprKind::ByteString { .. } => {
            let u8_id = checker.intern(ArType::Primitive(Primitive::U8));
            Some(checker.intern(ArType::Slice(u8_id)))
        }
        ExprKind::CString { .. } => {
            let u8_id = checker.intern(ArType::Primitive(Primitive::U8));
            Some(checker.intern(ArType::Ptr(u8_id)))
        }
        ExprKind::InterpolatedString { parts } => {
            // ToStr v0.1: formatable primitives are accepted; lower inserts
            // AmirRvalue::ToStr. Non-formatable types get T034 (not silent Any).
            let part_ids = checker.pool.string_part_list(*parts).to_vec();
            let str_ty = checker.intern(ArType::Primitive(Primitive::Str));
            let mut built = false;
            for part_id in part_ids {
                if let arandu_parser::StringPart::Expr {
                    expr: inner_expr, ..
                } = checker.pool.string_part(part_id)
                {
                    built = true;
                    let part_ty_id = synth_expr(checker, *inner_expr);
                    let part_ty = checker.resolve(part_ty_id);
                    let is_ref_str = match &part_ty {
                        ArType::Ref(inner) | ArType::RefMut(inner) => {
                            matches!(checker.resolve(*inner), ArType::Primitive(Primitive::Str))
                        }
                        _ => false,
                    };
                    let has_to_str = {
                        let mut base = part_ty_id;
                        for _ in 0..4 {
                            match checker.resolve(base) {
                                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                                    base = inner;
                                }
                                _ => break,
                            }
                        }
                        match checker.resolve(base) {
                            ArType::Named(id, _) => {
                                checker
                                    .symbols
                                    .lookup_associated_member(id, "to_str")
                                    .is_some()
                                    || checker
                                        .symbols
                                        .lookup_associated_member(id, "toStr")
                                        .is_some()
                            }
                            _ => false,
                        }
                    };
                    if part_ty.is_error()
                        || part_ty.is_to_str_v01()
                        || is_ref_str
                        || super::text::is_string(checker, &part_ty)
                        || has_to_str
                    {
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
                            "only bool, integers, floats, char, str, String, and types implementing `toStr` are supported"
                                .to_string(),
                        ),
                    );
                }
            }
            // Text built at run time is owned: `String`, freed once (as Rust's
            // `format!`). A literal with no expression stays a `str`.
            if built && let Some(string) = super::text::string_type(checker) {
                return Some(string);
            }
            // A literal takes its type from context, as Swift's string
            // literals do: where a `String` is expected it is one, copied
            // from module data, without `String.from(..)` at every use.
            if let Some(expected) = expected
                && super::text::is_owned_string(checker, &checker.resolve(expected))
            {
                return Some(expected);
            }
            Some(str_ty)
        }
        ExprKind::Nil => {
            // `nil` requires an expected nullable/optional/pointer type (it does
            // not fill `Result` or non-nullable types, and does not steal the
            // enclosing function's return type when unannotated).
            if let Some(ctx_id) = expected {
                let ctx = checker.resolve(ctx_id);
                if ctx.is_error()
                    || matches!(ctx, ArType::Option(_) | ArType::Ptr(_))
                    || matches!(
                        ctx,
                        ArType::Result(ok, _) if matches!(checker.resolve(ok), ArType::Void)
                    )
                {
                    Some(ctx_id)
                } else if types::is_err_type(&ctx, &checker.type_info.type_interner) {
                    let err_id = checker.intern(ArType::Error);
                    Some(checker.intern(ArType::Option(err_id)))
                } else {
                    let ty_disp = ctx.display(&checker.symbols, &checker.type_info.type_interner);
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T006NotNullable,
                            format!("type '{ty_disp}' is not nullable"),
                            span,
                        )
                        .with_label(
                            span,
                            format!("cannot assign `nil` to non-nullable type '{ty_disp}'"),
                        ),
                    );
                    Some(checker.intern(ArType::Error))
                }
            } else {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T001CannotInferType,
                        "cannot infer type for `nil`",
                        span,
                    )
                    .with_label(span, "type annotation needed for `nil`")
                    .with_hint("add an explicit optional or pointer type annotation"),
                );
                Some(checker.intern(ArType::Error))
            }
        }
        ExprKind::StructLiteral { ty, fields } => {
            let ty_id = *ty;
            let fields_range = *fields;
            let struct_ty = checker.lower_type_expr(ty_id, checker.type_scope());
            if matches!(struct_ty, ArType::Vector(_)) {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::U001FeatureNotSupported,
                    "native vector storage is opaque; use its constructors",
                    checker.pool.expr_span(expr),
                ));
                return Some(checker.intern(ArType::Error));
            }
            let mut struct_ty_id = checker.intern(struct_ty);
            let struct_info = match checker.resolve(struct_ty_id) {
                ArType::Named(symbol_id, generic_args) => Some((symbol_id, generic_args)),
                _ => None,
            };
            if let Some((symbol_id, generic_args)) = struct_info {
                let enum_parent = checker
                    .type_info
                    .enum_variants
                    .get(&symbol_id)
                    .map(|&(enum_id, _)| enum_id);
                let result_type_sym = enum_parent.unwrap_or(symbol_id);
                let mut generic_args = checker.type_info.type_interner.type_args(generic_args);
                let field_ids = checker.pool.field_init_list(fields_range).to_vec();

                // Infer missing type args from field values: `BoxG { v: 42 }` → `BoxG<int>`.
                if generic_args.is_empty()
                    && let Some(params) = checker.type_info.generic_params.get(&symbol_id).cloned()
                    && !params.is_empty()
                {
                    if let Some(template_fields) =
                        checker.type_info.struct_fields.get(&symbol_id).cloned()
                        && let Some(inferred) = infer_struct_type_args(
                            checker,
                            &params,
                            template_fields.as_ref(),
                            &field_ids,
                        )
                    {
                        generic_args = inferred;
                    } else if let Some(exp_id) = expected
                        && let ArType::Named(exp_sym, exp_args) = checker.resolve(exp_id)
                        && exp_sym == result_type_sym
                    {
                        let exp_vec = checker.type_info.type_interner.type_args(exp_args);
                        if !exp_vec.is_empty() {
                            generic_args = exp_vec;
                        }
                    }
                }
                if enum_parent.is_some() || !generic_args.is_empty() {
                    let concrete = ArType::named(
                        result_type_sym,
                        &generic_args,
                        &checker.type_info.type_interner,
                    );
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
                            checker.check_private_field_update(symbol_id, field.span);
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
                        checker.check_field_visibility(symbol_id, field.name.as_str(), field.span);
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
                                && matches!(
                                    checker.resolve(expected_id),
                                    ArType::Option(_) | ArType::Ptr(_)
                                )
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
                            let unifies = types::unify(
                                &defined_field_ty,
                                &field_val_ty,
                                &checker.type_info.type_interner,
                            ) || match &defined_field_ty {
                                ArType::Option(inner) => types::unify(
                                    &checker.type_info.type_interner.resolve(*inner),
                                    &field_val_ty,
                                    &checker.type_info.type_interner,
                                ),
                                _ => false,
                            };
                            if !unifies {
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

                    if !has_update_base && !checker.type_info.struct_unions.contains(&symbol_id) {
                        let mut missing_fields = Vec::new();
                        for def_name in fields_def.keys() {
                            if !seen_fields.iter().any(|(name, _)| name == def_name) {
                                if let Some(&default_expr) = checker
                                    .type_info
                                    .struct_field_defaults
                                    .get(&(symbol_id, def_name.as_str().into()))
                                {
                                    let expected_id_opt = fields_def
                                        .get(def_name)
                                        .map(|f_ty| checker.intern(f_ty.clone()));
                                    let def_val_ty_id = super::super::synth_expr_expected(
                                        checker,
                                        default_expr,
                                        expected_id_opt,
                                    );
                                    if let Some(defined_field_ty) = fields_def.get(def_name)
                                        && let Some(var_id) =
                                            checker.literal_table.var_for_expr(default_expr)
                                    {
                                        let exp_id = checker.intern(defined_field_ty.clone());
                                        checker.constrain_literal_var(
                                            var_id,
                                            exp_id,
                                            ConstraintOrigin::FieldInit {
                                                struct_span: span,
                                                field_name: def_name.to_string(),
                                                field_span: span,
                                                value_span: checker.pool.expr_span(default_expr),
                                            },
                                        );
                                    }
                                    let _ = def_val_ty_id;
                                    continue;
                                }
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
        ExprKind::ArrayRepeat { value, count } => {
            let expected_element = expected.and_then(|ty| match checker.resolve(ty) {
                ArType::Array(_, element)
                | ArType::ConstArray(_, element)
                | ArType::Slice(element)
                | ArType::MutSlice(element) => Some(element),
                _ => None,
            });
            let element = super::synth_expr_expected(checker, *value, expected_element);
            let length = checker.lower_type_expr(*count, checker.type_scope());
            let array =
                match (length.as_const_u64(), length) {
                    (Some(n), _) if n <= arandu_middle::types::MAX_ARRAY_REPEAT_ELEMENTS => {
                        ArType::Array(n, element)
                    }
                    (_, ArType::ConstParam(parameter)) => ArType::ConstArray(parameter, element),
                    (_, ArType::Error)
                        if !matches!(
                            checker.pool.type_expr(*count),
                            arandu_parser::TypeExpr::Const { .. }
                        ) =>
                    {
                        ArType::Error
                    }
                    _ => {
                        checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T048InvalidArrayRepeat,
                        "array repetition requires a static integer length of at most 65536",
                        checker.pool.type_expr_span(*count),
                    ).with_primary_label("invalid repetition length"));
                        ArType::Error
                    }
                };
            if matches!(array, ArType::Array(n, _) if n > 1)
                && !checker.resolve(element).is_error()
                && !checker.resolve(element).is_literal()
                && !checker.type_info.is_copy(element)
            {
                let element_ty = checker.resolve(element);
                let generic_params_all: Vec<_> = checker
                    .type_info
                    .generic_params
                    .values()
                    .flat_map(|p| p.iter().copied())
                    .collect();
                let copy_bounded = |sym: arandu_middle::SymbolId| {
                    checker
                        .type_info
                        .param_constraints
                        .get(&sym)
                        .is_some_and(|cs| {
                            cs.iter().any(|b| {
                                checker.symbols.try_get(b.iface_sym).is_some_and(|s| {
                                    s.lang_item == Some(arandu_middle::symbol_table::LangItem::Copy)
                                })
                            })
                        })
                };
                let all_params_copy_bounded = super::super::method::contains_generic_params(
                    &element_ty,
                    &generic_params_all,
                    &checker.type_info.type_interner,
                ) && {
                    let Ok(shape) = arandu_middle::types::TypeShape::from_id(
                        element,
                        &checker.type_info.type_interner,
                    ) else {
                        return Some(checker.intern(array));
                    };
                    let mut all_copy = true;
                    let _ = shape.for_each_symbol(|sym| {
                        if checker
                            .symbols
                            .try_get(sym)
                            .is_some_and(|s| s.kind == crate::SymbolKind::TypeParam)
                            && !copy_bounded(sym)
                        {
                            all_copy = false;
                        }
                    });
                    all_copy
                };
                if !all_params_copy_bounded {
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T048InvalidArrayRepeat,
                            "repeating this value would duplicate ownership",
                            span,
                        )
                        .with_primary_label("this element is not Copy")
                        .with_note("Construct each owning element independently or require `T: Copy`; no implicit clone is performed."),
                    );
                }
            }
            Some(checker.intern(array))
        }
        ExprKind::Tuple { items } => {
            let item_ids = checker.pool.expr_list(*items).to_vec();
            let expected_items = expected.and_then(|expected| match checker.resolve(expected) {
                ArType::Tuple(items) => Some(checker.type_info.type_interner.type_args(items)),
                _ => None,
            });
            let mut types = Vec::with_capacity(item_ids.len());
            for (index, &item) in item_ids.iter().enumerate() {
                let expected_item = expected_items
                    .as_ref()
                    .and_then(|items| items.get(index).copied());
                let mut ty = super::synth_expr_expected(checker, item, expected_item);
                // An element is a value of its own: a literal no context
                // types takes its default now (`int`, `float`), since the
                // tuple's type is fixed here and not by a later use.
                let default = match checker.resolve(ty) {
                    ArType::IntLiteral => Some(Primitive::Int),
                    ArType::FloatLiteral => Some(Primitive::Float),
                    _ => None,
                };
                if let Some(default) = default {
                    let concrete = expected_item
                        .filter(|&expected| checker.resolve(expected).is_numeric())
                        .unwrap_or_else(|| checker.intern(ArType::Primitive(default)));
                    if let Some(var) = checker.literal_table.var_for_expr(item) {
                        checker.constrain_literal_var(
                            var,
                            concrete,
                            ConstraintOrigin::ArrayLiteral {
                                array_span: span,
                                item_span: checker.pool.expr_span(item),
                                item_index: index,
                            },
                        );
                    }
                    checker.record_expr_type(item, concrete);
                    ty = concrete;
                }
                types.push(ty);
            }
            Some(checker.intern(ArType::tuple(&types, &checker.type_info.type_interner)))
        }
        ExprKind::Array { items } => {
            let items_range = *items;
            let expected_elem_id = expected.and_then(|exp_id| match checker.resolve(exp_id) {
                ArType::Array(_, elem_id) | ArType::Slice(elem_id) | ArType::MutSlice(elem_id) => {
                    Some(elem_id)
                }
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
                let literal_target = match checker.resolve(concrete_id) {
                    ArType::Option(inner) => inner,
                    _ => concrete_id,
                };
                checker.constrain_literal_var(
                    var_id,
                    literal_target,
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
                let compatible = array_element_types_compatible(
                    &elem_ty,
                    &item_ty,
                    &checker.type_info.type_interner,
                ) || matches!(
                    elem_ty,
                    ArType::Option(inner_id)
                        if array_element_types_compatible(
                            &checker.type_info.type_interner.resolve(inner_id),
                            &item_ty,
                            &checker.type_info.type_interner,
                        )
                );
                if !compatible {
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
