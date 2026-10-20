use arandu_middle::types::{ArType, Primitive, TypeInterner, VectorType};

pub(super) fn llvm_type(ty: &ArType, interner: &TypeInterner) -> Result<String, String> {
    match ty {
        ArType::Primitive(primitive) => primitive_type(*primitive),
        ArType::Vector(vector) => vector_type(*vector),
        ArType::Ptr(_)
        | ArType::Ref(_)
        | ArType::RefMut(_)
        | ArType::Nullable(_)
        | ArType::GenRef
        | ArType::Coroutine(_)
        | ArType::Err => Ok("ptr".to_owned()),
        ArType::Void => Ok("void".to_owned()),
        ArType::Tuple(args) => {
            let items = interner.type_args(*args);
            let fields = items
                .iter()
                .map(|id| llvm_type(&interner.resolve(*id), interner))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("{{ {} }}", fields.join(", ")))
        }
        ArType::Array(length, element) => Ok(format!(
            "[{} x {}]",
            length,
            llvm_type(&interner.resolve(*element), interner)?
        )),
        ArType::Slice(_) | ArType::MutSlice(_) => Ok("{ ptr, i64 }".to_owned()),
        ArType::IntLiteral => Ok("i32".to_owned()),
        ArType::FloatLiteral => Ok("double".to_owned()),
        other => Err(format!(
            "LLVM type mapping is not implemented for {other:?}"
        )),
    }
}

fn primitive_type(ty: Primitive) -> Result<String, String> {
    let mapped = match ty {
        Primitive::Bool => "i1",
        Primitive::Int | Primitive::Uint => "i32",
        Primitive::ISize | Primitive::USize => "i64",
        Primitive::I8 | Primitive::U8 | Primitive::Byte => "i8",
        Primitive::I16 | Primitive::U16 => "i16",
        Primitive::I32 | Primitive::U32 | Primitive::Char => "i32",
        Primitive::I64 | Primitive::U64 => "i64",
        Primitive::Float | Primitive::F64 => "double",
        Primitive::F32 => "float",
        Primitive::Str => return Ok("{ ptr, i64 }".to_owned()),
        Primitive::Any => return Ok("ptr".to_owned()),
    };
    Ok(mapped.to_owned())
}

fn vector_type(ty: VectorType) -> Result<String, String> {
    let (lanes, lane_ty) = match ty {
        VectorType::F32x4 => (4, "float"),
        VectorType::F64x2 => (2, "double"),
        VectorType::I32x4 => (4, "i32"),
        VectorType::I16x8 => (8, "i16"),
        VectorType::U8x16 => (16, "i8"),
        VectorType::I64x2 => (2, "i64"),
    };
    Ok(format!("<{lanes} x {lane_ty}>"))
}
