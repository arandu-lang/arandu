use rustc_hash::FxHashSet;

use arandu_lexer::Span;
use arandu_parser::{ExprKind, MatchArm, Pattern, ast_pool::PatternId};

use super::super::TypeChecker;
use super::super::types::{ArType, Primitive, TypeId, build_subst_ids, substitute_type_id};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Ctor {
    Bool(bool),
    OptionNone,
    OptionSome,
    ResultOk,
    ResultErr,
    PollReady,
    PollPending,
    Tuple(usize),
    EnumVariant(crate::SymbolId),
    Struct(crate::SymbolId),
    SealedImpl(crate::SymbolId),
    Opaque(PatternId),
}

#[derive(Clone, Debug)]
enum Pat {
    Wildcard,
    Ctor { ctor: Ctor, args: Vec<Pat> },
    Or(Vec<Pat>),
}

#[derive(Clone)]
struct CtorSpec {
    ctor: Ctor,
    label: String,
    arg_tys: Vec<TypeId>,
}

enum CtorSpace {
    Finite(Vec<CtorSpec>),
    Infinite,
}

fn peel_ref_type(checker: &TypeChecker<'_>, mut ty: TypeId) -> TypeId {
    for _ in 0..8 {
        match checker.type_info.resolve_type_id(ty) {
            ArType::Ref(inner) | ArType::RefMut(inner) => ty = inner,
            _ => break,
        }
    }
    ty
}

fn enum_variant_list(checker: &TypeChecker<'_>, enum_id: crate::SymbolId) -> Vec<crate::SymbolId> {
    let mut ids: Vec<_> = checker
        .type_info
        .enum_variants
        .iter()
        .filter_map(|(&variant_id, &(parent_enum, _))| {
            if parent_enum != enum_id {
                return None;
            }
            let sym = checker.symbols.try_get(variant_id)?;
            (sym.kind == arandu_middle::SymbolKind::AssociatedFunc).then_some(variant_id)
        })
        .collect();
    ids.sort_by_key(|sym| {
        (
            checker
                .type_info
                .enum_variant_tags
                .get(sym)
                .copied()
                .unwrap_or(usize::MAX),
            sym.file_id,
            sym.local_id.0,
        )
    });
    ids.dedup();
    ids
}

fn variant_short_name(
    checker: &TypeChecker<'_>,
    enum_id: crate::SymbolId,
    sym: crate::SymbolId,
) -> String {
    let enum_name = checker
        .symbols
        .try_get(enum_id)
        .map(|s| s.name.as_str())
        .unwrap_or("enum");
    let prefix = format!("{enum_name}.");
    checker
        .symbols
        .try_get(sym)
        .map(|s| s.name.strip_prefix(&prefix).unwrap_or(&s.name).to_string())
        .unwrap_or_else(|| "variant".to_string())
}

fn enum_variant_field_tys(
    checker: &TypeChecker<'_>,
    enum_id: crate::SymbolId,
    type_args: &[TypeId],
    variant_id: crate::SymbolId,
) -> Vec<TypeId> {
    let generic_params = checker
        .type_info
        .generic_params
        .get(&enum_id)
        .map_or(&[][..], |p| p.as_slice());
    let subst = build_subst_ids(generic_params, type_args, &checker.type_info.type_interner);
    let payload = checker
        .type_info
        .variant_payload_for(variant_id, type_args)
        .or_else(|| {
            checker
                .type_info
                .enum_variants
                .get(&variant_id)
                .map(|(_, shape)| shape)
        });
    match payload {
        Some(crate::type_checker::EnumPayloadShape::Tuple(items)) => items
            .iter()
            .map(|&tid| substitute_type_id(tid, &subst, &checker.type_info.type_interner))
            .collect(),
        _ => Vec::new(),
    }
}

fn ctor_space_for_type(checker: &TypeChecker<'_>, ty: TypeId) -> CtorSpace {
    let ty = peel_ref_type(checker, ty);
    match checker.type_info.resolve_type_id(ty) {
        ArType::Primitive(Primitive::Bool) => CtorSpace::Finite(vec![
            CtorSpec {
                ctor: Ctor::Bool(true),
                label: "true".to_string(),
                arg_tys: Vec::new(),
            },
            CtorSpec {
                ctor: Ctor::Bool(false),
                label: "false".to_string(),
                arg_tys: Vec::new(),
            },
        ]),
        ArType::Option(inner) => CtorSpace::Finite(vec![
            CtorSpec {
                ctor: Ctor::OptionSome,
                label: "Option.Some".to_string(),
                arg_tys: vec![inner],
            },
            CtorSpec {
                ctor: Ctor::OptionNone,
                label: "Option.None".to_string(),
                arg_tys: Vec::new(),
            },
        ]),
        ArType::Result(ok, err) => CtorSpace::Finite(vec![
            CtorSpec {
                ctor: Ctor::ResultOk,
                label: "Result.Ok".to_string(),
                arg_tys: vec![ok],
            },
            CtorSpec {
                ctor: Ctor::ResultErr,
                label: "Result.Err".to_string(),
                arg_tys: vec![err],
            },
        ]),
        ArType::Poll(inner) => CtorSpace::Finite(vec![
            CtorSpec {
                ctor: Ctor::PollReady,
                label: "Poll.Ready".to_string(),
                arg_tys: vec![inner],
            },
            CtorSpec {
                ctor: Ctor::PollPending,
                label: "Poll.Pending".to_string(),
                arg_tys: Vec::new(),
            },
        ]),
        ArType::Tuple(args) => {
            let elem_tys = checker.type_info.type_interner.type_args(args);
            CtorSpace::Finite(vec![CtorSpec {
                ctor: Ctor::Tuple(elem_tys.len()),
                label: String::new(),
                arg_tys: elem_tys,
            }])
        }
        ArType::Named(sym, type_args_id) => {
            if checker
                .type_info
                .interfaces
                .get(&sym)
                .is_some_and(|iface| iface.sealed)
            {
                let mut impls: Vec<_> = checker
                    .symbols
                    .interface_implementations
                    .iter()
                    .filter_map(|&(concrete, implemented)| (implemented == sym).then_some(concrete))
                    .collect();
                impls.sort_by_key(|s| {
                    checker
                        .symbols
                        .try_get(*s)
                        .map(|e| e.name.to_string())
                        .unwrap_or_default()
                });
                if impls.is_empty() {
                    return CtorSpace::Infinite;
                }
                return CtorSpace::Finite(
                    impls
                        .into_iter()
                        .map(|concrete| CtorSpec {
                            ctor: Ctor::SealedImpl(concrete),
                            label: checker
                                .symbols
                                .try_get(concrete)
                                .map(|s| s.name.to_string())
                                .unwrap_or_else(|| "type".to_string()),
                            arg_tys: Vec::new(),
                        })
                        .collect(),
                );
            }
            let variants = enum_variant_list(checker, sym);
            if !variants.is_empty() {
                let type_args = checker.type_info.type_interner.type_args(type_args_id);
                let enum_name = checker
                    .symbols
                    .try_get(sym)
                    .map(|s| s.name.to_string())
                    .unwrap_or_else(|| "enum".to_string());
                let specs = variants
                    .into_iter()
                    .map(|vid| {
                        let short = variant_short_name(checker, sym, vid);
                        let field_tys = enum_variant_field_tys(checker, sym, &type_args, vid);
                        CtorSpec {
                            ctor: Ctor::EnumVariant(vid),
                            label: format!("{enum_name}.{short}"),
                            arg_tys: field_tys,
                        }
                    })
                    .collect();
                return CtorSpace::Finite(specs);
            }
            if let Some(fields) = checker.type_info.struct_fields.get(&sym) {
                let type_args = checker.type_info.type_interner.type_args(type_args_id);
                let generic_params = checker
                    .type_info
                    .generic_params
                    .get(&sym)
                    .map_or(&[][..], |p| p.as_slice());
                let subst =
                    build_subst_ids(generic_params, &type_args, &checker.type_info.type_interner);
                let mut ordered: Vec<_> = fields.iter().collect();
                ordered.sort_by_key(|f| f.index);
                let arg_tys = ordered
                    .into_iter()
                    .map(|f| substitute_type_id(f.ty, &subst, &checker.type_info.type_interner))
                    .collect();
                let name = checker
                    .symbols
                    .try_get(sym)
                    .map(|s| s.name.to_string())
                    .unwrap_or_else(|| "struct".to_string());
                return CtorSpace::Finite(vec![CtorSpec {
                    ctor: Ctor::Struct(sym),
                    label: name,
                    arg_tys,
                }]);
            }
            CtorSpace::Infinite
        }
        _ => CtorSpace::Infinite,
    }
}

fn lower_pattern(checker: &TypeChecker<'_>, pat_id: PatternId, ty: TypeId) -> Pat {
    let ty = peel_ref_type(checker, ty);
    let resolved_ty = checker.type_info.resolve_type_id(ty);
    match checker.pool.pattern(pat_id).clone() {
        Pattern::Wildcard { .. } | Pattern::Bind { .. } => Pat::Wildcard,
        Pattern::Or { alts, .. } => {
            let items = checker
                .pool
                .pattern_list(alts)
                .iter()
                .map(|&alt| lower_pattern(checker, alt, ty))
                .collect();
            Pat::Or(items)
        }
        Pattern::Literal { expr, .. } => match checker.pool.expr(expr) {
            ExprKind::Bool { value } => Pat::Ctor {
                ctor: Ctor::Bool(*value),
                args: Vec::new(),
            },
            ExprKind::Nil if matches!(resolved_ty, ArType::Option(_)) => Pat::Ctor {
                ctor: Ctor::OptionNone,
                args: Vec::new(),
            },
            _ => Pat::Ctor {
                ctor: Ctor::Opaque(pat_id),
                args: Vec::new(),
            },
        },
        Pattern::Range { .. } => Pat::Ctor {
            ctor: Ctor::Opaque(pat_id),
            args: Vec::new(),
        },
        Pattern::Tuple { items, .. } => {
            let pat_ids = checker.pool.pattern_list(items).to_vec();
            let elem_tys = match resolved_ty {
                ArType::Tuple(args) => checker.type_info.type_interner.type_args(args),
                _ => Vec::new(),
            };
            if pat_ids.len() != elem_tys.len() {
                return Pat::Ctor {
                    ctor: Ctor::Opaque(pat_id),
                    args: Vec::new(),
                };
            }
            let args: Vec<_> = pat_ids
                .into_iter()
                .zip(elem_tys)
                .map(|(pid, ety)| lower_pattern(checker, pid, ety))
                .collect();
            Pat::Ctor {
                ctor: Ctor::Tuple(args.len()),
                args,
            }
        }
        Pattern::Enum {
            variant, payload, ..
        }
        | Pattern::TypeTuple {
            name: variant,
            payload,
            ..
        } => {
            let short = variant
                .rsplit_once('.')
                .map_or(variant.as_str(), |(_, s)| s);
            let payload_ids = checker.pool.pattern_list(payload).to_vec();
            match resolved_ty {
                ArType::Option(inner) => match short {
                    "None" => Pat::Ctor {
                        ctor: Ctor::OptionNone,
                        args: Vec::new(),
                    },
                    "Some" => {
                        let sub = payload_ids
                            .first()
                            .map(|&pid| lower_pattern(checker, pid, inner))
                            .unwrap_or(Pat::Wildcard);
                        Pat::Ctor {
                            ctor: Ctor::OptionSome,
                            args: vec![sub],
                        }
                    }
                    _ => Pat::Ctor {
                        ctor: Ctor::Opaque(pat_id),
                        args: Vec::new(),
                    },
                },
                ArType::Result(ok, err) => match short {
                    "Ok" => {
                        let sub = payload_ids
                            .first()
                            .map(|&pid| lower_pattern(checker, pid, ok))
                            .unwrap_or(Pat::Wildcard);
                        Pat::Ctor {
                            ctor: Ctor::ResultOk,
                            args: vec![sub],
                        }
                    }
                    "Err" => {
                        let sub = payload_ids
                            .first()
                            .map(|&pid| lower_pattern(checker, pid, err))
                            .unwrap_or(Pat::Wildcard);
                        Pat::Ctor {
                            ctor: Ctor::ResultErr,
                            args: vec![sub],
                        }
                    }
                    _ => Pat::Ctor {
                        ctor: Ctor::Opaque(pat_id),
                        args: Vec::new(),
                    },
                },
                ArType::Poll(inner) => match short {
                    "Ready" => {
                        let sub = payload_ids
                            .first()
                            .map(|&pid| lower_pattern(checker, pid, inner))
                            .unwrap_or(Pat::Wildcard);
                        Pat::Ctor {
                            ctor: Ctor::PollReady,
                            args: vec![sub],
                        }
                    }
                    "Pending" => Pat::Ctor {
                        ctor: Ctor::PollPending,
                        args: Vec::new(),
                    },
                    _ => Pat::Ctor {
                        ctor: Ctor::Opaque(pat_id),
                        args: Vec::new(),
                    },
                },
                ArType::Named(sym, type_args_id) => {
                    if checker
                        .type_info
                        .interfaces
                        .get(&sym)
                        .is_some_and(|iface| iface.sealed)
                    {
                        let global = checker.symbols.global_scope();
                        if let Some(concrete) = checker.symbols.lookup_type(global, short) {
                            return Pat::Ctor {
                                ctor: Ctor::SealedImpl(concrete),
                                args: Vec::new(),
                            };
                        }
                    }
                    if let Some(vid) = checker.symbols.lookup_associated_member(sym, short) {
                        let type_args = checker.type_info.type_interner.type_args(type_args_id);
                        let field_tys = enum_variant_field_tys(checker, sym, &type_args, vid);
                        let args = if payload_ids.len() == field_tys.len() {
                            payload_ids
                                .into_iter()
                                .zip(field_tys)
                                .map(|(pid, fty)| lower_pattern(checker, pid, fty))
                                .collect()
                        } else {
                            vec![Pat::Wildcard; field_tys.len()]
                        };
                        return Pat::Ctor {
                            ctor: Ctor::EnumVariant(vid),
                            args,
                        };
                    }
                    Pat::Ctor {
                        ctor: Ctor::Opaque(pat_id),
                        args: Vec::new(),
                    }
                }
                _ => Pat::Ctor {
                    ctor: Ctor::Opaque(pat_id),
                    args: Vec::new(),
                },
            }
        }
        Pattern::Struct {
            type_name, fields, ..
        } => {
            if let ArType::Named(sym, type_args_id) = resolved_ty {
                if checker
                    .type_info
                    .interfaces
                    .get(&sym)
                    .is_some_and(|iface| iface.sealed)
                {
                    let global = checker.symbols.global_scope();
                    if let Some(short) = type_name.path.last()
                        && let Some(concrete) = checker.symbols.lookup_type(global, short.as_str())
                    {
                        return Pat::Ctor {
                            ctor: Ctor::SealedImpl(concrete),
                            args: Vec::new(),
                        };
                    }
                }
                if let Some(short) = type_name.path.last()
                    && let Some(vid) = checker
                        .symbols
                        .lookup_associated_member(sym, short.as_str())
                {
                    let type_args = checker.type_info.type_interner.type_args(type_args_id);
                    let field_tys = enum_variant_field_tys(checker, sym, &type_args, vid);
                    return Pat::Ctor {
                        ctor: Ctor::EnumVariant(vid),
                        args: vec![Pat::Wildcard; field_tys.len()],
                    };
                }
                if let Some(struct_fields) = checker.type_info.struct_fields.get(&sym) {
                    let type_args = checker.type_info.type_interner.type_args(type_args_id);
                    let generic_params = checker
                        .type_info
                        .generic_params
                        .get(&sym)
                        .map_or(&[][..], |p| p.as_slice());
                    let subst = build_subst_ids(
                        generic_params,
                        &type_args,
                        &checker.type_info.type_interner,
                    );
                    let field_pats: Vec<_> = checker
                        .pool
                        .field_pattern_list(fields)
                        .iter()
                        .map(|&fid| checker.pool.field_pattern(fid))
                        .collect();
                    let mut ordered: Vec<_> = struct_fields.iter().collect();
                    ordered.sort_by_key(|f| f.index);
                    let args = ordered
                        .into_iter()
                        .map(|f| {
                            let fty =
                                substitute_type_id(f.ty, &subst, &checker.type_info.type_interner);
                            field_pats
                                .iter()
                                .find(|fp| fp.name.as_str() == f.name.as_str())
                                .and_then(|fp| fp.pattern)
                                .map_or(Pat::Wildcard, |pid| lower_pattern(checker, pid, fty))
                        })
                        .collect();
                    return Pat::Ctor {
                        ctor: Ctor::Struct(sym),
                        args,
                    };
                }
            }
            Pat::Ctor {
                ctor: Ctor::Opaque(pat_id),
                args: Vec::new(),
            }
        }
    }
}

fn expand_head_rows(rows: Vec<Vec<Pat>>) -> Vec<Vec<Pat>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match row.first() {
            Some(Pat::Or(alts)) => {
                let tail = &row[1..];
                let mut stack: Vec<Pat> = alts.iter().cloned().rev().collect();
                while let Some(pat) = stack.pop() {
                    if let Pat::Or(nested) = pat {
                        for n in nested.into_iter().rev() {
                            stack.push(n);
                        }
                    } else {
                        let mut new_row = Vec::with_capacity(1 + tail.len());
                        new_row.push(pat);
                        new_row.extend_from_slice(tail);
                        out.push(new_row);
                    }
                }
            }
            _ => out.push(row),
        }
    }
    out
}

fn specialize_matrix(rows: &[Vec<Pat>], ctor: &Ctor, arity: usize) -> Vec<Vec<Pat>> {
    let mut out = Vec::new();
    for row in rows {
        let Some(head) = row.first() else {
            continue;
        };
        let tail = &row[1..];
        match head {
            Pat::Wildcard => {
                let mut new_row = vec![Pat::Wildcard; arity];
                new_row.extend_from_slice(tail);
                out.push(new_row);
            }
            Pat::Ctor {
                ctor: row_ctor,
                args,
                ..
            } if row_ctor == ctor => {
                let mut new_row = Vec::with_capacity(arity + tail.len());
                if args.len() == arity {
                    new_row.extend_from_slice(args);
                } else {
                    new_row.resize(arity, Pat::Wildcard);
                }
                new_row.extend_from_slice(tail);
                out.push(new_row);
            }
            Pat::Ctor { .. } | Pat::Or(_) => {}
        }
    }
    out
}

fn default_matrix(rows: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    let mut out = Vec::new();
    for row in rows {
        if matches!(row.first(), Some(Pat::Wildcard)) {
            out.push(row[1..].to_vec());
        }
    }
    out
}

fn format_witness(spec: &CtorSpec, sub: &[String]) -> String {
    match &spec.ctor {
        Ctor::Bool(_) | Ctor::OptionNone | Ctor::PollPending | Ctor::SealedImpl(_) => {
            spec.label.clone()
        }
        Ctor::OptionSome | Ctor::ResultOk | Ctor::ResultErr | Ctor::PollReady => {
            format!("{}({})", spec.label, sub.join(", "))
        }
        Ctor::Tuple(_) => format!("({})", sub.join(", ")),
        Ctor::EnumVariant(_) | Ctor::Struct(_) => {
            if sub.is_empty() {
                spec.label.clone()
            } else {
                format!("{}({})", spec.label, sub.join(", "))
            }
        }
        Ctor::Opaque(_) => "_".to_string(),
    }
}

fn missing_witness(
    checker: &TypeChecker<'_>,
    cols: &[TypeId],
    rows: Vec<Vec<Pat>>,
    depth: usize,
) -> Option<Vec<String>> {
    if cols.is_empty() {
        return rows.is_empty().then(Vec::new);
    }
    if depth >= 32 {
        return None;
    }
    let rows = expand_head_rows(rows);
    if rows.is_empty() {
        return Some(vec!["_".to_string(); cols.len()]);
    }
    let head_ty = cols[0];
    let tail_cols = &cols[1..];
    match ctor_space_for_type(checker, head_ty) {
        CtorSpace::Finite(specs) => {
            for spec in &specs {
                let spec_rows = specialize_matrix(&rows, &spec.ctor, spec.arg_tys.len());
                let mut next_cols = Vec::with_capacity(spec.arg_tys.len() + tail_cols.len());
                next_cols.extend_from_slice(&spec.arg_tys);
                next_cols.extend_from_slice(tail_cols);
                if let Some(sub) = missing_witness(checker, &next_cols, spec_rows, depth + 1) {
                    let arg_len = spec.arg_tys.len();
                    let head_str = format_witness(spec, &sub[..arg_len]);
                    let mut full = Vec::with_capacity(1 + sub.len() - arg_len);
                    full.push(head_str);
                    full.extend_from_slice(&sub[arg_len..]);
                    return Some(full);
                }
            }
            None
        }
        CtorSpace::Infinite => {
            let def_rows = default_matrix(&rows);
            missing_witness(checker, tail_cols, def_rows, depth + 1).map(|mut sub| {
                let mut full = Vec::with_capacity(1 + sub.len());
                full.push("_".to_string());
                full.append(&mut sub);
                full
            })
        }
    }
}

pub fn check_match_exhaustiveness(
    checker: &mut TypeChecker<'_>,
    value_ty: TypeId,
    arms: &[MatchArm],
    match_span: Span,
) {
    let resolved_ty = checker.type_info.resolve_type_id(value_ty);
    // Error-typed match values must not trigger exhaustiveness diagnostics.
    if resolved_ty.is_error() {
        return;
    }
    let peeled_ty = peel_ref_type(checker, value_ty);
    let rows: Vec<Vec<Pat>> = arms
        .iter()
        .filter(|arm| arm.guard.is_none())
        .map(|arm| vec![lower_pattern(checker, arm.pattern, peeled_ty)])
        .collect();

    let Some(_) = missing_witness(checker, &[peeled_ty], rows.clone(), 0) else {
        return;
    };

    // Preserve clear, multi-variant diagnostics for top-level enums and sealed interfaces.
    if let ArType::Named(sym, _) = checker.type_info.resolve_type_id(peeled_ty) {
        if checker
            .type_info
            .interfaces
            .get(&sym)
            .is_some_and(|iface| iface.sealed)
            && let CtorSpace::Finite(specs) = ctor_space_for_type(checker, peeled_ty)
        {
            let expanded = expand_head_rows(rows.clone());
            let mut missing = Vec::new();
            for spec in specs {
                let spec_rows = specialize_matrix(&expanded, &spec.ctor, spec.arg_tys.len());
                if missing_witness(checker, &spec.arg_tys, spec_rows, 1).is_some() {
                    missing.push(spec.label);
                }
            }
            if !missing.is_empty() {
                missing.sort_unstable();
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T024NonExhaustiveMatch,
                    format!(
                        "non-exhaustive match on sealed interface: missing implementer(s): {}",
                        missing.join(", ")
                    ),
                    match_span,
                ));
                return;
            }
        }
        let all_variants = enum_variant_list(checker, sym);
        if !all_variants.is_empty()
            && let CtorSpace::Finite(specs) = ctor_space_for_type(checker, peeled_ty)
        {
            let expanded = expand_head_rows(rows.clone());
            let mut missing_short: Vec<String> = Vec::new();
            let mut covered_top: FxHashSet<crate::SymbolId> = FxHashSet::default();
            for row in &expanded {
                if let Some(Pat::Ctor {
                    ctor: Ctor::EnumVariant(vid),
                    ..
                }) = row.first()
                {
                    covered_top.insert(*vid);
                } else if matches!(row.first(), Some(Pat::Wildcard)) {
                    for vid in &all_variants {
                        covered_top.insert(*vid);
                    }
                }
            }
            for &vid in &all_variants {
                if !covered_top.contains(&vid) {
                    missing_short.push(variant_short_name(checker, sym, vid));
                }
            }
            if !missing_short.is_empty() {
                missing_short.sort();
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T024NonExhaustiveMatch,
                    format!(
                        "non-exhaustive match: missing variant(s): {}",
                        missing_short.join(", ")
                    ),
                    match_span,
                ));
                return;
            }
            let mut nested_missing = Vec::new();
            for spec in specs {
                let spec_rows = specialize_matrix(&expanded, &spec.ctor, spec.arg_tys.len());
                if let Some(sub) = missing_witness(checker, &spec.arg_tys, spec_rows, 1) {
                    nested_missing.push(format_witness(&spec, &sub));
                }
            }
            if !nested_missing.is_empty() {
                nested_missing.sort();
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T024NonExhaustiveMatch,
                    format!(
                        "non-exhaustive match: missing pattern(s): {}",
                        nested_missing.join(", ")
                    ),
                    match_span,
                ));
                return;
            }
        }
    }

    let mut missing = Vec::new();
    if let CtorSpace::Finite(specs) = ctor_space_for_type(checker, peeled_ty) {
        let expanded = expand_head_rows(rows);
        for spec in specs {
            let spec_rows = specialize_matrix(&expanded, &spec.ctor, spec.arg_tys.len());
            if let Some(sub) = missing_witness(checker, &spec.arg_tys, spec_rows, 1) {
                missing.push(format_witness(&spec, &sub));
            }
        }
    }
    if missing.is_empty() {
        missing.push("_".to_string());
    }
    missing.sort();
    checker.diagnostics.push(crate::Diagnostic::error(
        crate::DiagCode::T024NonExhaustiveMatch,
        format!(
            "non-exhaustive match: missing pattern(s): {}",
            missing.join(", ")
        ),
        match_span,
    ));
}
