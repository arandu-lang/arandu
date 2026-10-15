use arandu_middle::symbol_table::SymbolTable;
use arandu_middle::types::{ArType, TypeInterner};
use std::fmt::Write as _;

use super::graph::InstantiationKey;

pub fn mangle_symbol(
    key: &InstantiationKey<'_>,
    interner: &TypeInterner,
    symbols: &SymbolTable,
) -> String {
    // A function's source name is only unique inside its module. Generic
    // instances from different modules must remain distinct in the shared HIR
    // and in emitted backends even when both declarations are named `len`.
    let name = symbols.host_func_name(symbols.get(key.symbol));
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
            // Nominal arguments need their defining module identity too:
            // identity<a.User> and identity<b.User> are different instances.
            let name = symbols.host_func_name(symbols.get(*id));
            let arguments = interner.type_args(*args);
            // Length/arity delimiters make nominal names and nested generic
            // arguments unambiguous, including identifiers containing `_`.
            let _ = write!(out, "n{}_{}_g{}", name.len(), name, arguments.len());
            for &arg in &arguments {
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
        ArType::FrozenConst(value) => {
            out.push_str("frozen_");
            // Artifact names must stay bounded when Copy arguments contain
            // large immutable strings. The instantiation graph still compares
            // full TypeIds/values; this digest is only the external spelling.
            let mut digest = blake3::Hasher::new();
            digest.update(b"arandu-frozen-argument-symbol/v1");
            digest.update(&value.canonical_bytes());
            out.push_str(digest.finalize().to_hex().as_str());
        }
        ArType::Const(value) => {
            let _ = write!(out, "const{value}");
        }
        ArType::ConstParam(param) => {
            out.push_str("constparam_");
            out.push_str(&symbols.get(*param).name);
        }
        ArType::Tuple(items) => {
            let items = interner.type_args(*items);
            let _ = write!(out, "tup{}", items.len());
            for &item in &items {
                out.push('_');
                mangle_type_into(out, &interner.resolve(item), symbols, interner);
            }
        }
        ArType::Func(params, ret) => {
            let params = interner.type_args(*params);
            let _ = write!(out, "fn{}", params.len());
            for &param in &params {
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use arandu_middle::ctfe::{ConstAggregate, ConstString, ConstValue};
    use arandu_middle::types::{Primitive, TypeShape};

    #[test]
    fn aggregate_argument_symbols_are_bounded_and_keep_full_value_identity() {
        fn spelling(text: &str) -> String {
            let value = ConstAggregate::new(
                TypeShape::Array(1, Box::new(TypeShape::Primitive(Primitive::Str))),
                vec![ConstValue::String(ConstString::new(text))],
            )
            .expect("closed immutable array");
            let ty = ArType::FrozenConst(std::sync::Arc::new(ConstValue::Aggregate(value)));
            let mut name = String::new();
            mangle_type_into(&mut name, &ty, &SymbolTable::new(0), &TypeInterner::new());
            name
        }
        let text = "a".repeat(16_384);
        let first = spelling(&text);
        assert_eq!(first.len(), "frozen_".len() + 64);
        assert_eq!(first, spelling(&text));
        let mut different = text;
        different.pop();
        different.push('b');
        assert_ne!(first, spelling(&different));
    }
}
