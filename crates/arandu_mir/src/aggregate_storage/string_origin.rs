//! Bounded provenance proof for str results independent of this invocation.
//! This does not prove arbitrary borrowed returns or opaque retention contracts.
use arandu_middle::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, LocalId, TempId,
    for_each_rvalue_place,
};
use arandu_middle::types::{ArType, Primitive, TypeInterner};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Temp(TempId),
    Local(LocalId),
}

/// Literal pool data is static; value parameters belong to the caller. Memory
/// locals qualify only when every direct store forwards such data and their
/// descriptor's address is never exposed. Reject cycles and unproved producers.
pub(super) fn independent_string_result(function: &AmirFunc, interner: &TypeInterner) -> bool {
    fn operand(
        op: &AmirOperand,
        function: &AmirFunc,
        interner: &TypeInterner,
        path: &mut [Source; 64],
        depth: usize,
    ) -> bool {
        match op {
            AmirOperand::Constant(AmirConstant::Pool(_)) => true,
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                visit(Source::Temp(*temp), function, interner, path, depth)
            }
            AmirOperand::Constant(AmirConstant::Bool(_) | AmirConstant::Nil)
            | AmirOperand::FunctionRef(_)
            | AmirOperand::GlobalRef(_) => false,
        }
    }
    fn visit(
        source: Source,
        function: &AmirFunc,
        interner: &TypeInterner,
        path: &mut [Source; 64],
        depth: usize,
    ) -> bool {
        if depth >= path.len() || path[..depth].contains(&source) {
            return false;
        }
        let ty = match source {
            Source::Temp(temp) => function.temps.get(temp.as_usize()).map(|value| value.ty),
            Source::Local(local) => function.locals.get(local.as_usize()).map(|value| value.ty),
        };
        if !ty.is_some_and(|ty| matches!(interner.resolve(ty), ArType::Primitive(Primitive::Str))) {
            return false;
        }
        if let Source::Temp(temp) = source {
            if function
                .block_params
                .iter()
                .any(|parameter| parameter.id == temp)
            {
                return false;
            }
            if function.params.contains(&temp) {
                return true;
            }
        }
        path[depth] = source;
        let mut defined = false;
        for id in function.stmts.iter_ids() {
            match function.stmts.get(id) {
                Some(AmirStmt::Assign { lhs, rhs }) => {
                    if let Source::Local(local) = source {
                        // Shared visitors cover address-bearing places. A value
                        // load reads the descriptor without exposing its address.
                        let mut exposed = false;
                        if !matches!(rhs, AmirRvalue::Load(_)) {
                            for_each_rvalue_place(rhs, |place| exposed |= place.local == local);
                        }
                        if exposed
                            || matches!(rhs, AmirRvalue::RelativeBorrow { local: borrowed, .. } if *borrowed == local)
                        {
                            return false;
                        }
                        continue;
                    }
                    if source != Source::Temp(*lhs) {
                        continue;
                    }
                    defined = true;
                    let proven = match rhs {
                        AmirRvalue::Use(op) => operand(op, function, interner, path, depth + 1),
                        AmirRvalue::Load(place) => {
                            place.projections.is_empty()
                                && visit(
                                    Source::Local(place.local),
                                    function,
                                    interner,
                                    path,
                                    depth + 1,
                                )
                        }
                        AmirRvalue::VectorSelect { .. }
                        | AmirRvalue::VectorCompare { .. }
                        | AmirRvalue::VectorOp { .. }
                        | AmirRvalue::VectorSplat { .. }
                        | AmirRvalue::VectorShuffle { .. }
                        | AmirRvalue::VectorExtractLane { .. }
                        | AmirRvalue::VectorInsertLane { .. }
                        | AmirRvalue::Binary { .. }
                        | AmirRvalue::Unary { .. }
                        | AmirRvalue::FieldAccess { .. }
                        | AmirRvalue::IndexAccess { .. }
                        | AmirRvalue::StructLiteral { .. }
                        | AmirRvalue::Array { .. }
                        | AmirRvalue::Tuple { .. }
                        | AmirRvalue::Discriminant { .. }
                        | AmirRvalue::EnumPayload { .. }
                        | AmirRvalue::EnumConstruct { .. }
                        | AmirRvalue::Len(_)
                        | AmirRvalue::SliceData(_)
                        | AmirRvalue::SliceView { .. }
                        | AmirRvalue::SliceSubslice { .. }
                        | AmirRvalue::StrBytes { .. }
                        | AmirRvalue::StrView { .. }
                        | AmirRvalue::Alloc(_)
                        | AmirRvalue::Borrow(_)
                        | AmirRvalue::BorrowMut(_)
                        | AmirRvalue::ToStr { .. }
                        | AmirRvalue::BlackBox { .. }
                        | AmirRvalue::StringInterp { .. }
                        | AmirRvalue::RelativeBorrow { .. }
                        | AmirRvalue::CoroutineReady { .. }
                        | AmirRvalue::GenInsert { .. }
                        | AmirRvalue::GenGet { .. }
                        | AmirRvalue::GenSet { .. }
                        | AmirRvalue::GenUpsert { .. }
                        | AmirRvalue::GenRemove { .. } => false,
                    };
                    if !proven {
                        return false;
                    }
                }
                Some(AmirStmt::Store { lhs, rhs }) => {
                    if source == Source::Local(lhs.local) {
                        defined = true;
                        if !lhs.projections.is_empty()
                            || !operand(rhs, function, interner, path, depth + 1)
                        {
                            return false;
                        }
                    }
                }
                Some(AmirStmt::Call { lhs, .. }) => {
                    if lhs.is_some_and(|temp| source == Source::Temp(temp)) {
                        return false;
                    }
                }
                Some(AmirStmt::Destroy(place)) => {
                    if source == Source::Local(place.local) {
                        return false;
                    }
                }
                Some(
                    AmirStmt::Free(_)
                    | AmirStmt::StorageLive(_)
                    | AmirStmt::StorageDead(_)
                    | AmirStmt::Nop,
                ) => {}
                None => return false,
            }
        }
        defined
    }
    visit(
        Source::Temp(TempId::from_usize(0)),
        function,
        interner,
        &mut [Source::Temp(TempId::from_usize(0)); 64],
        0,
    )
}
