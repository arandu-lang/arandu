use super::super::symbols::{hash_symbol_id, u32_le, u64_le};
use arandu_middle::SymbolId;
use blake3::Hasher;

pub(in crate::stable_hash) fn hash_id(hasher: &mut Hasher, id: usize) {
    hasher.update(&u64_le(id as u64));
}

pub(in crate::stable_hash) fn hash_ar_type(hasher: &mut Hasher, ty: &arandu_middle::types::ArType) {
    use arandu_middle::types::{ArType, Primitive};

    match ty {
        ArType::Vector(vector) => {
            hasher.update(&[27]);
            hasher.update(vector.name().as_bytes());
        }
        ArType::Primitive(primitive) => {
            hasher.update(&[
                0,
                match primitive {
                    Primitive::Int => 0,
                    Primitive::Uint => 1,
                    Primitive::Float => 2,
                    Primitive::I8 => 3,
                    Primitive::I16 => 4,
                    Primitive::I32 => 5,
                    Primitive::I64 => 6,
                    Primitive::U8 => 7,
                    Primitive::U16 => 8,
                    Primitive::U32 => 9,
                    Primitive::U64 => 10,
                    Primitive::F32 => 11,
                    Primitive::F64 => 12,
                    Primitive::Bool => 13,
                    Primitive::Byte => 14,
                    Primitive::Char => 15,
                    Primitive::Str => 16,
                    Primitive::Any => 17,
                    Primitive::ISize => 18,
                    Primitive::USize => 19,
                },
            ]);
        }
        ArType::Named(symbol, args) => {
            hasher.update(&[1]);
            hash_symbol_id(hasher, *symbol);
            hash_index_range(hasher, *args);
        }
        ArType::Func(params, result) => {
            hasher.update(&[2]);
            hash_index_range(hasher, *params);
            hash_id(hasher, result.as_usize());
        }
        ArType::Nullable(inner) => {
            hasher.update(&[3]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Slice(inner) => {
            hasher.update(&[4]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::MutSlice(inner) => {
            hasher.update(&[26]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Array(len, inner) => {
            hasher.update(&[5]);
            hasher.update(&len.to_le_bytes());
            hash_id(hasher, inner.as_usize());
        }
        ArType::ConstArray(symbol, inner) => {
            hasher.update(&[6]);
            hash_symbol_id(hasher, *symbol);
            hash_id(hasher, inner.as_usize());
        }
        ArType::FrozenConst(value) => {
            hasher.update(&[24]);
            hasher.update(&value.canonical_bytes());
        }
        ArType::Const(value) => {
            hasher.update(&[7]);
            hasher.update(&value.to_le_bytes());
        }
        ArType::ConstParam(symbol) => {
            hasher.update(&[8]);
            hash_symbol_id(hasher, *symbol);
        }
        ArType::Ptr(inner) => {
            hasher.update(&[9]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Ref(inner) => {
            hasher.update(&[10]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::RefMut(inner) => {
            hasher.update(&[11]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::GenRef => {
            hasher.update(&[12]);
        }
        ArType::Tuple(items) => {
            hasher.update(&[13]);
            hash_index_range(hasher, *items);
        }
        ArType::Result(ok, err) => {
            hasher.update(&[14]);
            hash_id(hasher, ok.as_usize());
            hash_id(hasher, err.as_usize());
        }
        ArType::Option(inner) => {
            hasher.update(&[15]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Coroutine(inner) => {
            hasher.update(&[16]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Poll(inner) => {
            hasher.update(&[17]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Range(inner) => {
            hasher.update(&[18]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Err => {
            hasher.update(&[19]);
        }
        ArType::Void => {
            hasher.update(&[20]);
        }
        ArType::IntLiteral => {
            hasher.update(&[21]);
        }
        ArType::FloatLiteral => {
            hasher.update(&[22]);
        }
        ArType::Error => {
            hasher.update(&[23]);
        }
    }
}

fn hash_index_range(hasher: &mut Hasher, range: arandu_middle::hir::IndexRange) {
    hasher.update(&u32_le(range.start));
    hasher.update(&u32_le(range.len));
}

pub(in crate::stable_hash) fn hash_span(hasher: &mut Hasher, span: arandu_lexer::Span) {
    hasher.update(&u32_le(span.file_id));
    hasher.update(&u32_le(span.start));
    hasher.update(&u32_le(span.end));
}

pub(in crate::stable_hash) fn hash_optional_span(
    hasher: &mut Hasher,
    span: Option<arandu_lexer::Span>,
) {
    match span {
        Some(span) => {
            hasher.update(&[1]);
            hash_span(hasher, span);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

pub(in crate::stable_hash) fn hash_optional_symbol_id(
    hasher: &mut Hasher,
    symbol: Option<SymbolId>,
) {
    match symbol {
        Some(symbol) => {
            hasher.update(&[1]);
            hash_symbol_id(hasher, symbol);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}
