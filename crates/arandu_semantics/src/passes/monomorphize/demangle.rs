use arandu_middle::symbol_table::SymbolTable;
use arandu_middle::types::{ArType, TypeInterner};
use std::fmt::Write as _;

use super::graph::InstantiationKey;

pub fn mangle_symbol(
    key: &InstantiationKey<'_>,
    interner: &TypeInterner,
    symbols: &SymbolTable,
) -> String {
    let name = &symbols.get(key.symbol).name;
    let mut mangled = format!("_A${name}$I_");
    for (i, &tid) in key.type_args.iter().enumerate() {
        if i > 0 {
            mangled.push_str("_T_");
        }
        mangle_type_into(&mut mangled, &interner.resolve(tid), symbols, interner);
    }
    mangled.push_str("_$E");
    mangled
}

#[must_use]
pub fn demangle_symbol(mangled: &str) -> Option<String> {
    let inner = mangled.strip_prefix("_A$")?.strip_suffix("$E")?;
    let (name, rest) = inner.split_once("$I")?;
    let types_part = rest.strip_prefix("_")?.strip_suffix("_")?;
    if types_part.is_empty() {
        Some(name.to_string())
    } else {
        let types: Vec<&str> = types_part.split("_T_").collect();
        Some(format!("{}<{}>", name, types.join(", ")))
    }
}

fn mangle_type_into(out: &mut String, ty: &ArType, symbols: &SymbolTable, interner: &TypeInterner) {
    match ty {
        ArType::Primitive(p) => out.push_str(p.as_str()),
        ArType::Named(id, args) => {
            out.push_str(&symbols.get(*id).name);
            for &arg in interner.type_args(*args).iter() {
                out.push('_');
                mangle_type_into(out, &interner.resolve(arg), symbols, interner);
            }
        }
        ArType::Nullable(inner) => {
            out.push_str("opt_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Ptr(inner) => {
            out.push_str("ptr_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Ref(inner) => {
            out.push_str("ref_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::RefMut(inner) => {
            out.push_str("refmut_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::GenRef => out.push_str("genref"),
        ArType::Slice(inner) => {
            out.push_str("slice_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Array(n, inner) => {
            let _ = write!(out, "arr{n}_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::ConstArray(param, inner) => {
            out.push_str("arrparam_");
            out.push_str(&symbols.get(*param).name);
            out.push('_');
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Const(value) => {
            let _ = write!(out, "const{value}");
        }
        ArType::ConstParam(param) => {
            out.push_str("constparam_");
            out.push_str(&symbols.get(*param).name);
        }
        ArType::Tuple(items) => {
            out.push_str("tup");
            for &item in interner.type_args(*items).iter() {
                out.push('_');
                mangle_type_into(out, &interner.resolve(item), symbols, interner);
            }
        }
        ArType::Func(params, ret) => {
            out.push_str("fn");
            for &param in interner.type_args(*params).iter() {
                out.push('_');
                mangle_type_into(out, &interner.resolve(param), symbols, interner);
            }
            out.push_str("_R_");
            mangle_type_into(out, &interner.resolve(*ret), symbols, interner);
        }
        ArType::Result(ok, err) => {
            out.push_str("res_");
            mangle_type_into(out, &interner.resolve(*ok), symbols, interner);
            out.push('_');
            mangle_type_into(out, &interner.resolve(*err), symbols, interner);
        }
        ArType::Option(inner) => {
            out.push_str("option_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Coroutine(inner) => {
            out.push_str("coro_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Poll(inner) => {
            out.push_str("poll_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Range(inner) => {
            out.push_str("range_");
            mangle_type_into(out, &interner.resolve(*inner), symbols, interner);
        }
        ArType::Void => out.push_str("void"),
        ArType::Err => out.push_str("err"),
        ArType::IntLiteral => out.push_str("int"),
        ArType::FloatLiteral => out.push_str("float"),
        ArType::Error => out.push_str("error"),
    }
}
