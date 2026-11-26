use std::sync::Arc;

use arandu_middle::derive::{
    DeriveKind, DeriveMethodKind, derived_methods_for_kinds, expand_derive_implications,
    invalid_derive_arg_diagnostic,
};
use arandu_middle::symbol_table::LangItem;
use arandu_middle::types::TypeShape;
use arandu_middle::{DiagCode, Diagnostic, Symbol, SymbolId, SymbolKind};
use arandu_parser::{Attribute, Program, TopLevelDecl, ast_pool::ExprKind};
use rustc_hash::FxHashSet;
use smallvec::SmallVec;
use smol_str::SmolStr;

use crate::type_checker::TypeChecker;
use crate::type_checker::info::{DeriveRecipe, EnumPayloadShape, InterfaceConstraint};
use crate::type_checker::types::interfaces::type_satisfies_interface;
use crate::type_checker::types::{ArType, GenericSubst, Primitive, TypeId, substitute_type};

struct DerivedTypeTarget {
    type_sym: SymbolId,
    type_name: SmolStr,
    span: arandu_lexer::Span,
    expanded: Vec<DeriveKind>,
    methods: Vec<DeriveMethodKind>,
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, program))]
pub(crate) fn collect_derive_methods(checker: &mut TypeChecker<'_>, program: &Program) {
    let mut targets = Vec::new();
    program.for_each_decl_recursive(|_, decl| {
        let (type_name, decl_span, attrs) = match decl {
            TopLevelDecl::Struct(s) => (&s.name, s.span, &s.attrs),
            TopLevelDecl::Enum(e) => (&e.name, e.span, &e.attrs),
            _ => return,
        };
        let Some(type_sym) = checker
            .resolved
            .definitions
            .get(&crate::NodeKey::from(decl_span))
            .copied()
        else {
            return;
        };
        let Some(expanded) = parse_derive_attrs(checker, attrs) else {
            return;
        };
        let methods = derived_methods_for_kinds(&expanded);
        targets.push(DerivedTypeTarget {
            type_sym,
            type_name: type_name.clone(),
            span: decl_span,
            expanded,
            methods,
        });
    });

    if targets.is_empty() {
        return;
    }

    // Pass 1: register method signatures, method-scoped generic bounds, and recipes
    // so sibling/nested derived types see every derived method before Pass 2 validation.
    for target in &targets {
        register_target_derives(checker, target);
    }

    // Pass 2: validate concrete field capabilities and float bans on Eq/Ord/Hash.
    for target in &targets {
        validate_target_derives(checker, target);
    }
}

fn parse_derive_attrs(
    checker: &mut TypeChecker<'_>,
    attrs: &[Attribute],
) -> Option<Vec<DeriveKind>> {
    let mut has_derive = false;
    let mut requested = Vec::new();
    for attr in attrs {
        if attr.name != "Derive" {
            continue;
        }
        has_derive = true;
        if attr.args.is_empty() {
            checker.diagnostics.push(
                Diagnostic::error(
                    DiagCode::N014InvalidAnnotationArguments,
                    "annotation '@Derive' expects one or more trait identifiers",
                    attr.span,
                )
                .with_hint(
                    "supported `@Derive` traits are `PartialEq`, `Eq`, `Ord`, `Clone`, `Hash`, and `Debug`",
                ),
            );
            continue;
        }
        for &arg in &attr.args {
            let arg_span = checker.pool.expr_span(arg);
            let ExprKind::Path { path } = checker.pool.expr(arg) else {
                checker.diagnostics.push(Diagnostic::error(
                    DiagCode::N014InvalidAnnotationArguments,
                    "annotation '@Derive' expects trait identifiers",
                    arg_span,
                ));
                continue;
            };
            if path.len() != 1 {
                checker.diagnostics.push(Diagnostic::error(
                    DiagCode::N014InvalidAnnotationArguments,
                    "annotation '@Derive' expects simple trait identifiers",
                    arg_span,
                ));
                continue;
            }
            let name = path[0].as_str();
            match DeriveKind::parse(name) {
                Ok(kind) => requested.push(kind),
                Err(issue) => checker
                    .diagnostics
                    .push(invalid_derive_arg_diagnostic(issue, name, arg_span)),
            }
        }
    }
    if !has_derive || requested.is_empty() {
        return None;
    }
    Some(expand_derive_implications(&requested))
}

fn register_target_derives(checker: &mut TypeChecker<'_>, target: &DerivedTypeTarget) {
    let struct_gps = checker
        .type_info
        .generic_params
        .get(&target.type_sym)
        .map(|arc| arc.as_ref().clone())
        .unwrap_or_default();
    let non_phantom_gps = collect_non_phantom_type_params(checker, target.type_sym, &struct_gps);

    // Compute structural Copy on the nominal type before synthesizing methods.
    let self_for_copy = if struct_gps.is_empty() {
        let named = ArType::named(target.type_sym, &[], &checker.type_info.type_interner);
        let id = checker.intern(named);
        checker.type_info.is_copy(id)
    } else if non_phantom_gps.is_empty() {
        // Generic type whose type params only live in PhantomData<T> — all non-phantom fields concrete.
        all_concrete_non_phantom_fields_copy(checker, target.type_sym)
    } else {
        false
    };

    for &method_kind in &target.methods {
        let Some(method_sym) = checker
            .symbols
            .lookup_associated_member(target.type_sym, method_kind.method_name())
        else {
            continue;
        };

        let mut method_gp = Vec::with_capacity(struct_gps.len() + 1);
        let mut subst = GenericSubst::default();
        let mut m_tp_ids = Vec::with_capacity(struct_gps.len());

        for (idx, &orig_tp) in struct_gps.iter().enumerate() {
            let orig_name = checker
                .symbols
                .try_get(orig_tp)
                .map_or_else(|| format!("T{idx}"), |s| s.name.to_string());
            let m_tp = mint_derive_symbol(
                checker,
                target.type_sym,
                method_kind,
                0x100 + idx as u32,
                &orig_name,
                SymbolKind::TypeParam,
                target.span,
            );
            let m_tp_ty = ArType::named(m_tp, &[], &checker.type_info.type_interner);
            let m_tp_id = checker.intern(m_tp_ty.clone());
            method_gp.push(m_tp);
            m_tp_ids.push(m_tp_id);
            subst.push((orig_tp, m_tp_ty));
        }

        // Attach carried-over constraints + derived trait bounds (only for non-PhantomData type params).
        for (idx, &orig_tp) in struct_gps.iter().enumerate() {
            let m_tp = method_gp[idx];
            let existing = checker.type_info.param_constraints.get(&orig_tp).cloned();
            let mut constraints: Vec<InterfaceConstraint> = existing
                .map(|list| {
                    list.iter()
                        .map(|c| InterfaceConstraint {
                            iface_sym: c.iface_sym,
                            type_args: c
                                .type_args
                                .iter()
                                .map(|&a| {
                                    let resolved = checker.resolve(a);
                                    let s = substitute_type(
                                        &resolved,
                                        &subst,
                                        &checker.type_info.type_interner,
                                    );
                                    checker.intern(s)
                                })
                                .collect(),
                        })
                        .collect()
                })
                .unwrap_or_default();

            if non_phantom_gps.contains(&orig_tp)
                && let Some(bound_iface) =
                    required_interface_for_derive_method(checker, method_kind, &target.expanded)
                && !constraints
                    .iter()
                    .any(|c| c.iface_sym == bound_iface && c.type_args.is_empty())
            {
                constraints.push(InterfaceConstraint {
                    iface_sym: bound_iface,
                    type_args: SmallVec::new(),
                });
            }
            if !constraints.is_empty() {
                checker
                    .type_info
                    .param_constraints
                    .insert(m_tp, Arc::new(constraints));
            }
        }

        let self_bare = {
            let ty = ArType::named(target.type_sym, &m_tp_ids, &checker.type_info.type_interner);
            checker.intern(ty)
        };
        let self_ref = checker.intern(ArType::Ref(self_bare));

        let (explicit_param_tys, return_ty, param_names): (
            Vec<TypeId>,
            TypeId,
            SmallVec<[&'static str; 3]>,
        ) = match method_kind {
            DeriveMethodKind::Eq => {
                let bool_ty = checker.intern(ArType::Primitive(Primitive::Bool));
                (
                    vec![self_ref],
                    bool_ty,
                    SmallVec::from_slice(&["self", "other"]),
                )
            }
            DeriveMethodKind::Cmp => {
                let Some(ord_sym) = checker.symbols.lang_item_symbol(LangItem::Ordering) else {
                    continue;
                };
                let ord_ty = {
                    let ty = ArType::named(ord_sym, &[], &checker.type_info.type_interner);
                    checker.intern(ty)
                };
                (
                    vec![self_ref],
                    ord_ty,
                    SmallVec::from_slice(&["self", "other"]),
                )
            }
            DeriveMethodKind::Clone => (vec![], self_bare, SmallVec::from_slice(&["self"])),
            DeriveMethodKind::Hash => {
                let Some(hasher_sym) = checker.symbols.lang_item_symbol(LangItem::Hasher) else {
                    continue;
                };
                let h_sym = mint_derive_symbol(
                    checker,
                    target.type_sym,
                    method_kind,
                    0x200,
                    "H",
                    SymbolKind::TypeParam,
                    target.span,
                );
                checker.type_info.param_constraints.insert(
                    h_sym,
                    Arc::new(vec![InterfaceConstraint {
                        iface_sym: hasher_sym,
                        type_args: SmallVec::new(),
                    }]),
                );
                method_gp.push(h_sym);
                let h_ty = {
                    let ty = ArType::named(h_sym, &[], &checker.type_info.type_interner);
                    checker.intern(ty)
                };
                let state_ty = checker.intern(ArType::RefMut(h_ty));
                let void_ty = checker.intern(ArType::Void);
                (
                    vec![state_ty],
                    void_ty,
                    SmallVec::from_slice(&["self", "state"]),
                )
            }
            DeriveMethodKind::Debug => {
                let (Some(fmt_sym), Some(fmt_err_sym)) = (
                    checker.symbols.lang_item_symbol(LangItem::Formatter),
                    checker.symbols.lang_item_symbol(LangItem::FmtError),
                ) else {
                    continue;
                };
                let fmt_ty = {
                    let ty = ArType::named(fmt_sym, &[], &checker.type_info.type_interner);
                    checker.intern(ty)
                };
                let f_ty = checker.intern(ArType::RefMut(fmt_ty));
                let usize_ty = checker.intern(ArType::Primitive(Primitive::USize));
                let fmt_err_ty = {
                    let ty = ArType::named(fmt_err_sym, &[], &checker.type_info.type_interner);
                    checker.intern(ty)
                };
                let ret_ty = checker.intern(ArType::Result(usize_ty, fmt_err_ty));
                (vec![f_ty], ret_ty, SmallVec::from_slice(&["self", "f"]))
            }
        };

        let mut all_param_tys = Vec::with_capacity(1 + explicit_param_tys.len());
        all_param_tys.push(self_ref);
        all_param_tys.extend(explicit_param_tys.iter().copied());

        let func_ty = ArType::func(&all_param_tys, return_ty, &checker.type_info.type_interner);
        let func_id = checker.intern(func_ty);
        checker.record_decl_type(method_sym, func_id);
        if !method_gp.is_empty() {
            checker
                .type_info
                .generic_params
                .insert(method_sym, Arc::new(method_gp));
        }

        let mut param_symbols = Vec::with_capacity(param_names.len());
        for (idx, (&name, &ty)) in param_names.iter().zip(all_param_tys.iter()).enumerate() {
            let sym = mint_derive_symbol(
                checker,
                target.type_sym,
                method_kind,
                0x300 + idx as u32,
                name,
                SymbolKind::Param,
                target.span,
            );
            checker.record_decl_type(sym, ty);
            param_symbols.push(sym);
        }

        checker.type_info.derived_methods.insert(
            method_sym,
            DeriveRecipe {
                type_symbol: target.type_sym,
                method_symbol: method_sym,
                kind: method_kind,
                self_param_ty: self_ref,
                explicit_param_tys,
                return_ty,
                param_symbols,
                is_copy: self_for_copy && method_kind == DeriveMethodKind::Clone,
                span: target.span,
            },
        );
    }
}

fn validate_target_derives(checker: &mut TypeChecker<'_>, target: &DerivedTypeTarget) {
    let field_types = collect_non_phantom_field_types(checker, target.type_sym);
    let mut reported_float_traits = FxHashSet::default();

    for &kind in &target.expanded {
        if matches!(kind, DeriveKind::Eq | DeriveKind::Ord | DeriveKind::Hash) {
            for &fty in &field_types {
                let mut visited = FxHashSet::default();
                if contains_float_recursive(checker, fty, &mut visited)
                    && reported_float_traits.insert(kind)
                {
                    let field_disp = checker
                        .resolve(fty)
                        .display(&checker.symbols, &checker.type_info.type_interner);
                    checker.diagnostics.push(
                        Diagnostic::error(
                            DiagCode::T025InterfaceNotSatisfied,
                            format!(
                                "type `{}` cannot derive `{}` because field type `{field_disp}` contains a floating-point value",
                                target.type_name,
                                kind.name()
                            ),
                            target.span,
                        )
                        .with_hint(
                            "floating-point values (such as `f32`/`f64`/`float`) do not have total equality or ordering due to `NaN`; use `@Derive(PartialEq)` instead",
                        ),
                    );
                    break;
                }
            }
        }

        for &fty in &field_types {
            if field_contains_type_param(checker, fty) {
                continue;
            }
            if !concrete_field_supports_derive(checker, fty, kind, target.span) {
                let field_disp = checker
                    .resolve(fty)
                    .display(&checker.symbols, &checker.type_info.type_interner);
                checker.diagnostics.push(
                    Diagnostic::error(
                        DiagCode::T025InterfaceNotSatisfied,
                        format!(
                            "type `{}` cannot derive `{}`: field type `{field_disp}` does not implement `{}`",
                            target.type_name,
                            kind.name(),
                            kind.name()
                        ),
                        target.span,
                    )
                    .with_hint(format!(
                        "add `@Derive({})` or implement `{}` on `{field_disp}`",
                        kind.name(),
                        kind.name()
                    )),
                );
            }
        }
    }
}

fn required_interface_for_derive_method(
    checker: &TypeChecker<'_>,
    method: DeriveMethodKind,
    expanded: &[DeriveKind],
) -> Option<SymbolId> {
    match method {
        DeriveMethodKind::Eq => {
            if expanded.contains(&DeriveKind::Eq) {
                checker
                    .symbols
                    .lang_item_symbol(LangItem::Eq)
                    .or_else(|| checker.symbols.lang_item_symbol(LangItem::PartialEq))
            } else {
                checker
                    .symbols
                    .lang_item_symbol(LangItem::PartialEq)
                    .or_else(|| checker.symbols.lang_item_symbol(LangItem::Eq))
            }
        }
        DeriveMethodKind::Cmp => checker.symbols.lang_item_symbol(LangItem::Ord),
        DeriveMethodKind::Clone => checker.symbols.lang_item_symbol(LangItem::Clone),
        DeriveMethodKind::Hash => checker.symbols.lang_item_symbol(LangItem::Hash),
        DeriveMethodKind::Debug => checker.symbols.lang_item_symbol(LangItem::Debug),
    }
}

fn collect_non_phantom_field_types(checker: &TypeChecker<'_>, type_sym: SymbolId) -> Vec<TypeId> {
    let mut out = Vec::new();
    if let Some(fields) = checker.type_info.struct_fields.get(&type_sym) {
        for field in fields.iter() {
            if !is_phantom_data_type(checker, field.ty) {
                out.push(field.ty);
            }
        }
    }
    for (parent, shape) in checker.type_info.enum_variants.values() {
        if *parent != type_sym {
            continue;
        }
        if let EnumPayloadShape::Tuple(items) = shape {
            for &item_ty in items {
                if !is_phantom_data_type(checker, item_ty) {
                    out.push(item_ty);
                }
            }
        }
    }
    out
}

fn collect_non_phantom_type_params(
    checker: &TypeChecker<'_>,
    type_sym: SymbolId,
    struct_gps: &[SymbolId],
) -> FxHashSet<SymbolId> {
    let mut used = FxHashSet::default();
    if struct_gps.is_empty() {
        return used;
    }
    let gp_set: FxHashSet<SymbolId> = struct_gps.iter().copied().collect();
    for fty in collect_non_phantom_field_types(checker, type_sym) {
        if let Ok(shape) = TypeShape::from_id(fty, &checker.type_info.type_interner) {
            let _ = shape.for_each_symbol(|sym| {
                if gp_set.contains(&sym) {
                    used.insert(sym);
                }
            });
        }
    }
    used
}

fn all_concrete_non_phantom_fields_copy(checker: &TypeChecker<'_>, type_sym: SymbolId) -> bool {
    collect_non_phantom_field_types(checker, type_sym)
        .into_iter()
        .all(|fty| checker.type_info.is_copy(fty))
}

fn is_phantom_data_type(checker: &TypeChecker<'_>, ty_id: TypeId) -> bool {
    matches!(
        checker.resolve(ty_id),
        ArType::Named(sym, _) if is_phantom_data_sym(checker, sym)
    )
}

fn is_phantom_data_sym(checker: &TypeChecker<'_>, sym: SymbolId) -> bool {
    checker
        .symbols
        .try_get(sym)
        .is_some_and(|s| s.lang_item == Some(LangItem::PhantomData))
}

fn field_contains_type_param(checker: &TypeChecker<'_>, ty_id: TypeId) -> bool {
    let mut has_tp = false;
    if let Ok(shape) = TypeShape::from_id(ty_id, &checker.type_info.type_interner) {
        let _ = shape.for_each_symbol(|sym| {
            if checker
                .symbols
                .try_get(sym)
                .is_some_and(|s| s.kind == SymbolKind::TypeParam)
            {
                has_tp = true;
            }
        });
    }
    has_tp
}

fn contains_float_recursive(
    checker: &TypeChecker<'_>,
    ty_id: TypeId,
    visited: &mut FxHashSet<SymbolId>,
) -> bool {
    match checker.resolve(ty_id) {
        ArType::Primitive(Primitive::Float | Primitive::F32 | Primitive::F64)
        | ArType::FloatLiteral => true,
        ArType::Array(_, elem)
        | ArType::ConstArray(_, elem)
        | ArType::Slice(elem)
        | ArType::MutSlice(elem) => contains_float_recursive(checker, elem, visited),
        ArType::Tuple(args) => {
            let items = checker.type_info.type_interner.type_args(args);
            items
                .into_iter()
                .any(|item| contains_float_recursive(checker, item, visited))
        }
        ArType::Option(inner) => contains_float_recursive(checker, inner, visited),
        ArType::Result(ok, err) => {
            contains_float_recursive(checker, ok, visited)
                || contains_float_recursive(checker, err, visited)
        }
        ArType::Named(sym, args) => {
            if is_phantom_data_sym(checker, sym) {
                return false;
            }
            if !visited.insert(sym) {
                return false;
            }
            let arg_ids = checker.type_info.type_interner.type_args(args);
            if arg_ids
                .iter()
                .any(|&a| contains_float_recursive(checker, a, visited))
            {
                return true;
            }
            if let Some(fields) = checker.type_info.struct_fields.get(&sym).cloned() {
                for f in fields.iter() {
                    if contains_float_recursive(checker, f.ty, visited) {
                        return true;
                    }
                }
            }
            let variants: Vec<_> = checker
                .type_info
                .enum_variants
                .values()
                .filter(|(parent, _)| *parent == sym)
                .cloned()
                .collect();
            for (_, shape) in variants {
                if let EnumPayloadShape::Tuple(items) = shape {
                    for item in items {
                        if contains_float_recursive(checker, item, visited) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        _ => false,
    }
}

fn concrete_field_supports_derive(
    checker: &mut TypeChecker<'_>,
    ty_id: TypeId,
    kind: DeriveKind,
    span: arandu_lexer::Span,
) -> bool {
    let resolved = checker.resolve(ty_id);
    if matches!(resolved, ArType::Error) {
        return true;
    }
    match kind {
        DeriveKind::PartialEq | DeriveKind::Eq => {
            if let ArType::Tuple(args) = resolved {
                let items = checker.type_info.type_interner.type_args(args);
                return items
                    .into_iter()
                    .all(|item| concrete_field_supports_derive(checker, item, kind, span));
            }
            if let ArType::Array(_, elem) | ArType::ConstArray(_, elem) = resolved {
                return concrete_field_supports_derive(checker, elem, kind, span);
            }
            if let Some(iface) =
                required_interface_for_derive_method(checker, DeriveMethodKind::Eq, &[kind])
            {
                type_satisfies_interface(checker, &resolved, iface, &[], span)
            } else {
                matches!(
                    resolved,
                    ArType::Primitive(_)
                        | ArType::IntLiteral
                        | ArType::FloatLiteral
                        | ArType::Ptr(_)
                        | ArType::Ref(_)
                        | ArType::RefMut(_)
                )
            }
        }
        DeriveKind::Ord => {
            if resolved.is_orderable() {
                return true;
            }
            if let ArType::Tuple(args) = resolved {
                let items = checker.type_info.type_interner.type_args(args);
                return items
                    .into_iter()
                    .all(|item| concrete_field_supports_derive(checker, item, kind, span));
            }
            checker
                .symbols
                .lang_item_symbol(LangItem::Ord)
                .is_some_and(|ord| type_satisfies_interface(checker, &resolved, ord, &[], span))
        }
        DeriveKind::Clone => {
            if checker.type_info.is_copy(ty_id) {
                return true;
            }
            checker
                .symbols
                .lang_item_symbol(LangItem::Clone)
                .is_some_and(|clone| type_satisfies_interface(checker, &resolved, clone, &[], span))
        }
        DeriveKind::Hash => {
            if matches!(
                resolved,
                ArType::Primitive(p) if !matches!(p, Primitive::Float | Primitive::F32 | Primitive::F64)
            ) {
                return true;
            }
            if let ArType::Tuple(args) = resolved {
                let items = checker.type_info.type_interner.type_args(args);
                return items
                    .into_iter()
                    .all(|item| concrete_field_supports_derive(checker, item, kind, span));
            }
            checker
                .symbols
                .lang_item_symbol(LangItem::Hash)
                .is_some_and(|hash| type_satisfies_interface(checker, &resolved, hash, &[], span))
        }
        DeriveKind::Debug => {
            if matches!(resolved, ArType::Primitive(_)) {
                return true;
            }
            if let ArType::Named(sym, _) = resolved
                && checker
                    .symbols
                    .try_get(sym)
                    .is_some_and(|s| s.lang_item == Some(LangItem::String))
            {
                return true;
            }
            if let ArType::Tuple(args) = resolved {
                let items = checker.type_info.type_interner.type_args(args);
                return items
                    .into_iter()
                    .all(|item| concrete_field_supports_derive(checker, item, kind, span));
            }
            checker
                .symbols
                .lang_item_symbol(LangItem::Debug)
                .is_some_and(|dbg| type_satisfies_interface(checker, &resolved, dbg, &[], span))
        }
    }
}

fn mint_derive_symbol(
    checker: &mut TypeChecker<'_>,
    owner_sym: SymbolId,
    method: DeriveMethodKind,
    role: u32,
    name: &str,
    kind: SymbolKind,
    span: arandu_lexer::Span,
) -> SymbolId {
    let file_id = checker.symbols.file_id;
    let mut mixed = owner_sym.local_id.0.wrapping_mul(0x9E37_79B1)
        ^ ((method as u32).wrapping_add(1)).wrapping_mul(0x85EB_CA6B)
        ^ role.wrapping_mul(0xC2B2_AE35);
    loop {
        let local = SymbolId::SYNTHETIC_LOCAL_BIT | (mixed & 0x7FFF_FFFF);
        let id = SymbolId::new(file_id, local);
        let global_scope = checker.symbols.global_scope();
        if checker.symbols.try_get(id).is_none() {
            Arc::make_mut(&mut checker.symbols).register_synthetic(Symbol {
                id,
                name: SmolStr::new(name),
                kind,
                span,
                scope: global_scope,
                visibility: arandu_middle::Visibility::Private,
                lang_item: None,
            });
            return id;
        }
        mixed = mixed.wrapping_add(0x0001_0000);
    }
}
