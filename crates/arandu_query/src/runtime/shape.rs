use arandu_middle::types::TypeShape;

pub(super) fn hash_shape(hash: &mut blake3::Hasher, shape: &TypeShape) {
    use TypeShape::*;
    let tag = match shape {
        Primitive(_) => 0,
        Vector(_) => 25,
        Named(_, _) => 1,
        Func(_, _) => 2,
        Nullable(_) => 3,
        Slice(_) => 4,
        Array(_, _) => 5,
        ConstArray(_, _) => 6,
        Const(_) => 7,
        FrozenConst(_) => 24,
        ConstParam(_) => 8,
        Ptr(_) => 9,
        Ref(_) => 10,
        RefMut(_) => 11,
        GenRef => 12,
        Tuple(_) => 13,
        Result(_, _) => 14,
        Option(_) => 15,
        Coroutine(_) => 16,
        Poll(_) => 17,
        Range(_) => 18,
        Err => 19,
        Void => 20,
        IntLiteral => 21,
        FloatLiteral => 22,
        Error => 23,
        MutSlice(_) => 26,
    };
    hash.update(&[tag]);
    match shape {
        Vector(vector) => {
            hash.update(vector.name().as_bytes());
        }
        Primitive(primitive) => {
            hash.update(&[*primitive as u8]);
        }
        Named(symbol, arguments) => {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
            hash.update(&(arguments.len() as u64).to_le_bytes());
            for argument in arguments {
                hash_shape(hash, argument);
            }
        }
        Func(arguments, result) => {
            hash.update(&(arguments.len() as u64).to_le_bytes());
            for argument in arguments {
                hash_shape(hash, argument);
            }
            hash_shape(hash, result);
        }
        Tuple(arguments) => {
            hash.update(&(arguments.len() as u64).to_le_bytes());
            for argument in arguments {
                hash_shape(hash, argument);
            }
        }
        Array(length, inner) => {
            hash.update(&length.to_le_bytes());
            hash_shape(hash, inner);
        }
        ConstArray(symbol, inner) => {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
            hash_shape(hash, inner);
        }
        FrozenConst(value) => {
            hash.update(&value.canonical_bytes());
        }
        Const(value) => {
            hash.update(&value.to_le_bytes());
        }
        ConstParam(symbol) => {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
        }
        Nullable(inner) | Slice(inner) | MutSlice(inner) | Ptr(inner) | Ref(inner)
        | RefMut(inner) | Option(inner) | Coroutine(inner) | Poll(inner) | Range(inner) => {
            hash_shape(hash, inner)
        }
        Result(ok, error) => {
            hash_shape(hash, ok);
            hash_shape(hash, error);
        }
        GenRef | Err | Void | IntLiteral | FloatLiteral | Error => {}
    }
}
