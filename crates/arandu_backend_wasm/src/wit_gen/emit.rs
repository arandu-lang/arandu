use arandu_middle::SymbolId;
use arandu_middle::amir::AmirProgram;
use arandu_middle::layout::StructLayoutProvider;
use arandu_middle::symbol_table::SymbolKind;
use arandu_middle::types::TypeInterner;
use arandu_semantics::SymbolTable;
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

use super::lower::TypeLowerer;
use super::model::{WitTypeDef, WitTypeDefKind, WitTypeId, WitTypeKind, WitTypePool};
use super::naming::{extract_method_name, to_wit_ident};

/// Write a [`WitTypeId`] into `out` using the type definitions in `named_map`.
fn write_type(
    id: WitTypeId,
    pool: &WitTypePool,
    named_map: &FxHashMap<SymbolId, SmolStr>,
    out: &mut String,
) {
    let Some(kind) = pool.get(id) else {
        // Defensive: an unknown handle cannot be rendered as a WIT type. Emit
        // `u32` (the fallback used for unregistered nominal types) rather than
        // aborting the whole generation.
        out.push_str("u32");
        return;
    };
    match kind {
        WitTypeKind::Primitive(prim) => {
            out.push_str(prim.wit_name());
        }
        WitTypeKind::Named(sym) => {
            if let Some(name) = named_map.get(sym) {
                out.push_str(name);
            } else {
                out.push_str("u32");
            }
        }
        WitTypeKind::List(inner) => {
            out.push_str("list<");
            write_type(*inner, pool, named_map, out);
            out.push('>');
        }
        WitTypeKind::Tuple(elems) => {
            out.push_str("tuple<");
            for (i, &elem) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_type(elem, pool, named_map, out);
            }
            out.push('>');
        }
        WitTypeKind::Option(inner) => {
            out.push_str("option<");
            write_type(*inner, pool, named_map, out);
            out.push('>');
        }
        WitTypeKind::Result { ok, err } => match (ok, err) {
            (Some(o), Some(e)) => {
                out.push_str("result<");
                write_type(*o, pool, named_map, out);
                out.push_str(", ");
                write_type(*e, pool, named_map, out);
                out.push('>');
            }
            (Some(o), None) => {
                out.push_str("result<");
                write_type(*o, pool, named_map, out);
                out.push('>');
            }
            (None, Some(e)) => {
                out.push_str("result<_, ");
                write_type(*e, pool, named_map, out);
                out.push('>');
            }
            (None, None) => {
                out.push_str("result");
            }
        },
    }
}

/// Generate a complete WIT text for `program`.
///
/// `pkg_name` is used as the WIT package name (e.g. `"my-app"`). It must be
/// a valid WIT identifier (lowercase, hyphens allowed, no spaces).
///
/// # Errors
///
/// Returns `None` if the program has no public functions (nothing to export).
/// The caller may choose to skip component encoding in that case.
#[must_use]
pub fn generate_wit(
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    layout_provider: &dyn StructLayoutProvider,
    pkg_name: &str,
) -> Option<String> {
    let public_funcs: Vec<_> = program
        .funcs
        .iter()
        .filter(|f| symbols.try_get(f.symbol).is_some_and(|s| s.is_public))
        .collect();

    if public_funcs.is_empty() {
        return None;
    }

    let mut lowerer = TypeLowerer::new(symbols, interner, layout_provider);

    // Pre-pass: lower all public function parameter and return types to collect
    // referenced nominal types into `type_defs`.
    struct FuncSigInfo {
        fn_name: String,
        params: Vec<(String, WitTypeId)>,
        result: Option<WitTypeId>,
        symbol: SymbolId,
    }

    // Check for explicit public interfaces in the symbol table
    let mut public_ifaces: Vec<&arandu_middle::symbol_table::Symbol> = Vec::new();
    for sym in symbols.iter() {
        if sym.kind == SymbolKind::Interface && sym.is_public {
            public_ifaces.push(sym);
        }
    }
    public_ifaces.sort_by_key(|s| (s.id.file_id, s.id.local_id.0));

    let mut func_sigs = Vec::with_capacity(public_funcs.len());
    for func in &public_funcs {
        let Some(sym) = symbols.try_get(func.symbol) else {
            continue;
        };
        let fn_name = to_wit_ident(&sym.name);
        let mut params = Vec::new();

        // The receiver is materialized as a member of `func.params` (AMIR keeps
        // `params[0]` as the receiver). Expose it once as `self` and skip the
        // duplicate entry in the loop below so the WIT signature does not gain a
        // spurious extra parameter.
        let receiver_temp = func.receiver.map(|recv| recv.temp);
        if let Some(recv) = func.receiver {
            let ty = func.temps[recv.temp.as_usize()].ty;
            if let Some(wit_ty) = lowerer.lower_type(ty) {
                params.push(("self".to_owned(), wit_ty));
            }
        }
        for (i, &param_tid) in func.params.iter().enumerate() {
            if Some(param_tid) == receiver_temp {
                continue;
            }
            let ty = func.temps[param_tid.as_usize()].ty;
            if let Some(wit_ty) = lowerer.lower_type(ty) {
                params.push((format!("p{i}"), wit_ty));
            }
        }

        let result = lowerer.lower_type(func.return_type);
        func_sigs.push(FuncSigInfo {
            fn_name,
            params,
            result,
            symbol: func.symbol,
        });
    }

    // Partition functions into named interfaces vs default `exports`.
    let mut iface_groups: Vec<(&arandu_middle::symbol_table::Symbol, Vec<FuncSigInfo>)> =
        Vec::new();
    let mut default_exports: Vec<FuncSigInfo> = Vec::new();

    for sig in func_sigs {
        let sym = symbols.try_get(sig.symbol);
        let iface_match = sym.and_then(|s| {
            public_ifaces.iter().find(|iface| {
                s.name.starts_with(&format!("{}.", iface.name))
                    || symbols
                        .associated_members
                        .iter()
                        .any(|((parent, _), &member)| *parent == iface.id && member == sig.symbol)
            })
        });

        if let Some(iface) = iface_match {
            let method_name = sym
                .and_then(|s| extract_method_name(&s.name))
                .map(to_wit_ident)
                .unwrap_or_else(|| sig.fn_name.clone());

            let updated_sig = FuncSigInfo {
                fn_name: method_name,
                params: sig.params,
                result: sig.result,
                symbol: sig.symbol,
            };

            if let Some((_, group)) = iface_groups.iter_mut().find(|(i, _)| i.id == iface.id) {
                group.push(updated_sig);
            } else {
                iface_groups.push((iface, vec![updated_sig]));
            }
        } else {
            default_exports.push(sig);
        }
    }

    // Now format the WIT text into a single allocated String buffer.
    let mut wit = String::with_capacity(1024);

    // Package header
    wit.push_str("package arandu:");
    wit.push_str(&to_wit_ident(pkg_name));
    wit.push_str("@0.1.0;\n\n");

    let mut exported_interfaces = Vec::new();

    // Helper closure to collect nominal types referenced by a slice of functions
    let collect_iface_types = |sigs: &[FuncSigInfo]| -> Vec<WitTypeDef> {
        let mut needed = FxHashSet::default();
        for sig in sigs {
            let mut stack = Vec::new();
            for (_, pty) in &sig.params {
                stack.push(*pty);
            }
            if let Some(rty) = sig.result {
                stack.push(rty);
            }
            while let Some(tid) = stack.pop() {
                let Some(kind) = lowerer.pool.get(tid) else {
                    continue;
                };
                match kind {
                    WitTypeKind::Primitive(_) => {}
                    WitTypeKind::Named(sym) => {
                        if needed.insert(*sym)
                            && let Some(def) = lowerer.type_defs.iter().find(|d| d.symbol == *sym)
                        {
                            match &def.kind {
                                WitTypeDefKind::Record { fields } => {
                                    for (_, fty) in fields {
                                        stack.push(*fty);
                                    }
                                }
                                WitTypeDefKind::Enum { .. } => {}
                                WitTypeDefKind::Variant { cases } => {
                                    for (_, pty) in cases {
                                        if let Some(pt) = pty {
                                            stack.push(*pt);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    WitTypeKind::List(inner) | WitTypeKind::Option(inner) => stack.push(*inner),
                    WitTypeKind::Tuple(elems) => stack.extend(elems.iter().copied()),
                    WitTypeKind::Result { ok, err } => {
                        if let Some(o) = ok {
                            stack.push(*o);
                        }
                        if let Some(e) = err {
                            stack.push(*e);
                        }
                    }
                }
            }
        }
        lowerer
            .type_defs
            .iter()
            .filter(|d| needed.contains(&d.symbol))
            .cloned()
            .collect()
    };

    // Helper closure to emit nominal types
    let emit_types = |wit: &mut String, defs: &[WitTypeDef], lowerer: &TypeLowerer| {
        for def in defs {
            match &def.kind {
                WitTypeDefKind::Record { fields } => {
                    wit.push_str("  record ");
                    wit.push_str(&def.name);
                    wit.push_str(" {\n");
                    for (fname, fty) in fields {
                        wit.push_str("    ");
                        wit.push_str(fname);
                        wit.push_str(": ");
                        write_type(*fty, &lowerer.pool, &lowerer.named_map, wit);
                        wit.push_str(",\n");
                    }
                    wit.push_str("  }\n\n");
                }
                WitTypeDefKind::Enum { cases } => {
                    wit.push_str("  enum ");
                    wit.push_str(&def.name);
                    wit.push_str(" {\n");
                    for case in cases {
                        wit.push_str("    ");
                        wit.push_str(case);
                        wit.push_str(",\n");
                    }
                    wit.push_str("  }\n\n");
                }
                WitTypeDefKind::Variant { cases } => {
                    wit.push_str("  variant ");
                    wit.push_str(&def.name);
                    wit.push_str(" {\n");
                    for (case, payload) in cases {
                        wit.push_str("    ");
                        wit.push_str(case);
                        if let Some(pty) = payload {
                            wit.push('(');
                            write_type(*pty, &lowerer.pool, &lowerer.named_map, wit);
                            wit.push(')');
                        }
                        wit.push_str(",\n");
                    }
                    wit.push_str("  }\n\n");
                }
            }
        }
    };

    // Helper closure to emit function signatures
    let emit_funcs = |wit: &mut String, sigs: &[FuncSigInfo], lowerer: &TypeLowerer| {
        for sig in sigs {
            wit.push_str("  ");
            wit.push_str(&sig.fn_name);
            wit.push_str(": func(");
            for (i, (pname, pty)) in sig.params.iter().enumerate() {
                if i > 0 {
                    wit.push_str(", ");
                }
                wit.push_str(pname);
                wit.push_str(": ");
                write_type(*pty, &lowerer.pool, &lowerer.named_map, wit);
            }
            wit.push(')');

            if let Some(res) = sig.result {
                wit.push_str(" -> ");
                write_type(res, &lowerer.pool, &lowerer.named_map, wit);
            }
            wit.push_str(";\n");
        }
    };

    // 1. Emit named interfaces
    for (iface, sigs) in &iface_groups {
        let iface_wit_name = to_wit_ident(&iface.name);
        wit.push_str("interface ");
        wit.push_str(&iface_wit_name);
        wit.push_str(" {\n");

        let iface_types = collect_iface_types(sigs);
        emit_types(&mut wit, &iface_types, &lowerer);
        emit_funcs(&mut wit, sigs, &lowerer);
        wit.push_str("}\n\n");
        exported_interfaces.push(iface_wit_name);
    }

    // 2. Emit default exports interface if needed
    if !default_exports.is_empty() || iface_groups.is_empty() {
        wit.push_str("interface exports {\n");
        let exports_types = collect_iface_types(&default_exports);
        emit_types(&mut wit, &exports_types, &lowerer);
        emit_funcs(&mut wit, &default_exports, &lowerer);
        wit.push_str("}\n\n");
        exported_interfaces.push("exports".to_owned());
    }

    // Collect imported extern functions deterministically
    let mut extern_list: Vec<_> = program.extern_funcs.iter().collect();
    extern_list.sort_by_key(|(sym, _)| (sym.file_id, sym.local_id.0));

    let mut extern_sigs = Vec::with_capacity(extern_list.len());
    for (sym_id, (params, ret)) in &extern_list {
        let Some(sym) = symbols.try_get(**sym_id) else {
            continue;
        };
        let fn_name = to_wit_ident(&sym.name);
        let mut param_wits = Vec::with_capacity(params.len());
        for (i, p_ty) in params.iter().enumerate() {
            if let Some(wit_ty) = lowerer.lower_ar_type(p_ty) {
                param_wits.push((format!("p{i}"), wit_ty));
            }
        }
        let result = lowerer.lower_ar_type(ret);
        extern_sigs.push(FuncSigInfo {
            fn_name,
            params: param_wits,
            result,
            symbol: **sym_id,
        });
    }

    // World definition
    let world_name = to_wit_ident(pkg_name);
    wit.push_str("world ");
    wit.push_str(&world_name);
    wit.push_str(" {\n");
    for sig in &extern_sigs {
        wit.push_str("  import ");
        wit.push_str(&sig.fn_name);
        wit.push_str(": func(");
        for (i, (pname, pty)) in sig.params.iter().enumerate() {
            if i > 0 {
                wit.push_str(", ");
            }
            wit.push_str(pname);
            wit.push_str(": ");
            write_type(*pty, &lowerer.pool, &lowerer.named_map, &mut wit);
        }
        wit.push(')');
        if let Some(res) = sig.result {
            wit.push_str(" -> ");
            write_type(res, &lowerer.pool, &lowerer.named_map, &mut wit);
        }
        wit.push_str(";\n");
    }
    for iface_name in &exported_interfaces {
        wit.push_str("  export ");
        wit.push_str(iface_name);
        wit.push_str(";\n");
    }
    wit.push_str("}\n");

    Some(wit)
}
