//! AMIR node, dataflow, and IDE-fact hashes.

use arandu_middle::SymbolId;
use blake3::Hasher;
use std::sync::Arc;

use super::symbols::{
    finish, hash_return_borrow_summary, hash_str, hash_symbol_id, u32_le, u64_le, StableHash,
};

pub(super) fn hash_id(hasher: &mut Hasher, id: usize) {
    hasher.update(&u64_le(id as u64));
}

pub(super) fn hash_ar_type(hasher: &mut Hasher, ty: &arandu_middle::types::ArType) {
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

pub(super) fn hash_span(hasher: &mut Hasher, span: arandu_lexer::Span) {
    hasher.update(&u32_le(span.file_id));
    hasher.update(&u32_le(span.start));
    hasher.update(&u32_le(span.end));
}

pub(super) fn hash_optional_span(hasher: &mut Hasher, span: Option<arandu_lexer::Span>) {
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

pub(super) fn hash_optional_symbol_id(hasher: &mut Hasher, symbol: Option<SymbolId>) {
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

pub(super) fn hash_terminator(hasher: &mut Hasher, term: &arandu_middle::amir::AmirTerminator) {
    use arandu_middle::amir::AmirTerminator;
    match term {
        AmirTerminator::Return => {
            hasher.update(&[0]);
        }
        AmirTerminator::Unreachable => {
            hasher.update(&[1]);
        }
        AmirTerminator::Goto { target, args } => {
            hasher.update(&[2]);
            hash_id(hasher, target.as_usize());
            hash_operands(hasher, args);
        }
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, condition);
            hash_id(hasher, if_true.as_usize());
            hash_operands(hasher, true_args);
            hash_id(hasher, if_false.as_usize());
            hash_operands(hasher, false_args);
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            hasher.update(&[4]);
            hash_amir_operand(hasher, discriminant);
            hash_id(hasher, targets.len());
            for (value, target, args) in targets {
                hasher.update(&value.to_le_bytes());
                hash_id(hasher, target.as_usize());
                hash_operands(hasher, args);
            }
            hash_id(hasher, otherwise.0.as_usize());
            hash_operands(hasher, &otherwise.1);
        }
        AmirTerminator::Suspend {
            future,
            resume,
            args,
        } => {
            hasher.update(&[5]);
            hash_amir_operand(hasher, future);
            hash_id(hasher, resume.as_usize());
            hash_operands(hasher, args);
        }
    }
}

fn hash_operands(hasher: &mut Hasher, operands: &[arandu_middle::amir::AmirOperand]) {
    hash_id(hasher, operands.len());
    for operand in operands {
        hash_amir_operand(hasher, operand);
    }
}

fn hash_place(hasher: &mut Hasher, place: &arandu_middle::amir::AmirPlace) {
    use arandu_middle::amir::AmirProjection;
    hash_id(hasher, place.local.as_usize());
    hash_id(hasher, place.projections.len());
    for projection in &place.projections {
        match projection {
            AmirProjection::Field(symbol) => {
                hasher.update(&[0]);
                hash_symbol_id(hasher, *symbol);
            }
            AmirProjection::Index(operand) => {
                hasher.update(&[1]);
                hash_amir_operand(hasher, operand);
            }
            AmirProjection::IndexConstant(index) => {
                hasher.update(&[6]);
                hash_id(hasher, *index);
            }
            AmirProjection::Deref => {
                hasher.update(&[2]);
            }
            AmirProjection::Variant(tag) => {
                hasher.update(&[3]);
                hash_id(hasher, *tag);
            }
            AmirProjection::Payload {
                variant_tag,
                index,
                field_ty,
                tuple_ty,
            } => {
                hasher.update(&[4]);
                hash_id(hasher, *variant_tag);
                hash_id(hasher, *index);
                hash_id(hasher, field_ty.as_usize());
                match tuple_ty {
                    Some(ty) => {
                        hasher.update(&[1]);
                        hash_id(hasher, ty.as_usize());
                    }
                    None => {
                        hasher.update(&[0]);
                    }
                }
            }
            AmirProjection::TupleField(index) => {
                hasher.update(&[5]);
                hash_id(hasher, *index);
            }
        }
    }
}

pub(super) fn hash_stmt(hasher: &mut Hasher, stmt: &arandu_middle::amir::AmirStmt) {
    use arandu_middle::amir::AmirStmt;
    match stmt {
        AmirStmt::Assign { lhs, rhs } => {
            hasher.update(&[0]);
            hash_id(hasher, lhs.as_usize());
            hash_rvalue(hasher, rhs);
        }
        AmirStmt::Store { lhs, rhs } => {
            hasher.update(&[1]);
            hash_place(hasher, lhs);
            hash_amir_operand(hasher, rhs);
        }
        AmirStmt::Call {
            lhs,
            callee,
            args,
            return_borrow,
        } => {
            hasher.update(&[2]);
            match lhs {
                Some(id) => {
                    hasher.update(&[1]);
                    hash_id(hasher, id.as_usize());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
            hash_amir_operand(hasher, callee);
            hash_operands(hasher, args);
            match return_borrow {
                Some(summary) => {
                    hasher.update(&[1]);
                    hash_return_borrow_summary(hasher, summary);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        AmirStmt::Free(value) => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, value);
        }
        AmirStmt::StorageLive(local) => {
            hasher.update(&[4]);
            hash_id(hasher, local.as_usize());
        }
        AmirStmt::StorageDead(local) => {
            hasher.update(&[5]);
            hash_id(hasher, local.as_usize());
        }
        AmirStmt::Destroy(place) => {
            hasher.update(&[6]);
            hash_place(hasher, place);
        }
        AmirStmt::Nop => {
            hasher.update(&[7]);
        }
    }
}

pub(super) fn hash_rvalue(hasher: &mut Hasher, value: &arandu_middle::amir::AmirRvalue) {
    use arandu_middle::amir::AmirRvalue as R;
    match value {
        R::VectorSelect {
            mask,
            if_true,
            if_false,
        } => {
            hasher.update(&[36]);
            hash_amir_operand(hasher, mask);
            hash_amir_operand(hasher, if_true);
            hash_amir_operand(hasher, if_false);
        }
        R::VectorCompare { op, left, right } => {
            hasher.update(&[37, op.stable_tag()]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
        }
        R::VectorOp { op, left, right } => {
            hasher.update(&[31, op.stable_tag()]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
        }
        R::VectorSplat { value } => {
            hasher.update(&[32]);
            hash_amir_operand(hasher, value);
        }
        R::VectorShuffle { left, right, mask } => {
            hasher.update(&[33]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
            hasher.update(mask);
        }
        R::VectorExtractLane { vector, lane } => {
            hasher.update(&[34, *lane]);
            hash_amir_operand(hasher, vector);
        }
        R::VectorInsertLane {
            vector,
            lane,
            value,
        } => {
            hasher.update(&[35, *lane]);
            hash_amir_operand(hasher, vector);
            hash_amir_operand(hasher, value);
        }
        R::Use(op) => {
            hasher.update(&[0]);
            hash_amir_operand(hasher, op);
        }
        R::Binary { op, left, right } => {
            hasher.update(&[1, op.stable_tag()]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
        }
        R::Unary { op, operand } => {
            hasher.update(&[2, op.stable_tag()]);
            hash_amir_operand(hasher, operand);
        }
        R::FieldAccess { base, field } => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, base);
            hash_id(hasher, *field);
        }
        R::StructLiteral {
            struct_symbol,
            fields,
        } => {
            hasher.update(&[4]);
            hash_symbol_id(hasher, *struct_symbol);
            hash_id(hasher, fields.len());
            for (name, operand) in fields {
                hash_str(hasher, name);
                hash_amir_operand(hasher, operand);
            }
        }
        R::IndexAccess { base, index } => {
            hasher.update(&[5]);
            hash_amir_operand(hasher, base);
            hash_amir_operand(hasher, index);
        }
        R::Array { items } => {
            hasher.update(&[6]);
            hash_operands(hasher, items);
        }
        R::Tuple { items } => {
            hasher.update(&[7]);
            hash_operands(hasher, items);
        }
        R::Discriminant { value } => {
            hasher.update(&[8]);
            hash_amir_operand(hasher, value);
        }
        R::EnumPayload {
            value,
            variant,
            index,
            variant_tag,
            field_ty,
            tuple_ty,
        } => {
            hasher.update(&[9]);
            hash_amir_operand(hasher, value);
            hash_symbol_id(hasher, *variant);
            hash_id(hasher, *variant_tag);
            hash_id(hasher, *index);
            hash_id(hasher, field_ty.as_usize());
            match tuple_ty {
                Some(tuple_ty) => {
                    hasher.update(&[1]);
                    hash_id(hasher, tuple_ty.as_usize());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        R::EnumConstruct {
            variant_tag,
            payload,
        } => {
            hasher.update(&[10]);
            hash_id(hasher, *variant_tag);
            match payload {
                Some(op) => {
                    hasher.update(&[1]);
                    hash_amir_operand(hasher, op);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        R::Len(op) => {
            hasher.update(&[11]);
            hash_amir_operand(hasher, op);
        }
        R::SliceData(op) => {
            hasher.update(&[12]);
            hash_amir_operand(hasher, op);
        }
        R::SliceView { owner, data, len } => {
            hasher.update(&[13]);
            hash_amir_operand(hasher, owner);
            hash_amir_operand(hasher, data);
            hash_amir_operand(hasher, len);
        }
        R::SliceSubslice { slice, start, len } => {
            hasher.update(&[14]);
            hash_amir_operand(hasher, slice);
            hash_amir_operand(hasher, start);
            hash_amir_operand(hasher, len);
        }
        R::StrBytes { source } => {
            hasher.update(&[15]);
            hash_amir_operand(hasher, source);
        }
        R::StrView { owner } => {
            hasher.update(&[16]);
            hash_amir_operand(hasher, owner);
        }
        R::Alloc(op) => {
            hasher.update(&[17]);
            hash_amir_operand(hasher, op);
        }
        R::Load(place) => {
            hasher.update(&[18]);
            hash_place(hasher, place);
        }
        R::Borrow(place) => {
            hasher.update(&[19]);
            hash_place(hasher, place);
        }
        R::BorrowMut(place) => {
            hasher.update(&[20]);
            hash_place(hasher, place);
        }
        R::RelativeBorrow { local, mutable } => {
            hasher.update(&[21, u8::from(*mutable)]);
            hash_id(hasher, local.as_usize());
        }
        R::CoroutineReady {
            value,
            payload_ty,
            stack,
        } => {
            hasher.update(&[22, u8::from(*stack)]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, payload_ty.as_usize());
        }
        R::GenInsert {
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[23]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, value);
        }
        R::GenGet {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[24]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
        }
        R::GenSet {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[25]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
            hash_amir_operand(hasher, value);
        }
        R::GenUpsert {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[26]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
            hash_amir_operand(hasher, value);
        }
        R::GenRemove {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[27]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
        }
        R::StringInterp { parts } => {
            hasher.update(&[28]);
            hash_operands(hasher, parts);
        }
        R::ToStr { value, src_ty } => {
            hasher.update(&[29]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, src_ty.as_usize());
        }
        R::BlackBox { value, value_ty } => {
            hasher.update(&[30]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, value_ty.as_usize());
        }
    }
}

fn hash_gen_meta(
    hasher: &mut Hasher,
    ty: arandu_middle::types::TypeId,
    arena: arandu_middle::amir::GenArenaDomain,
    origin: arandu_lexer::Span,
) {
    hasher.update(&[match arena {
        arandu_middle::amir::GenArenaDomain::CompilerManaged => 0,
    }]);
    hash_id(hasher, ty.as_usize());
    hash_span(hasher, origin);
}

fn hash_amir_operand(hasher: &mut Hasher, operand: &arandu_middle::amir::AmirOperand) {
    use arandu_middle::amir::{AmirConstant, AmirOperand};

    match operand {
        AmirOperand::Copy(temp) => {
            hasher.update(&[0]);
            hasher.update(&u64_le(temp.as_usize() as u64));
        }
        AmirOperand::Move(temp) => {
            hasher.update(&[1]);
            hasher.update(&u64_le(temp.as_usize() as u64));
        }
        AmirOperand::Constant(AmirConstant::Pool(id)) => {
            hasher.update(&[2]);
            hasher.update(&u32_le(id.0));
        }
        AmirOperand::Constant(AmirConstant::Bool(value)) => {
            hasher.update(&[3, u8::from(*value)]);
        }
        AmirOperand::Constant(AmirConstant::Nil) => {
            hasher.update(&[4]);
        }
        AmirOperand::FunctionRef(symbol) => {
            hasher.update(&[5]);
            hash_symbol_id(hasher, *symbol);
        }
        AmirOperand::GlobalRef(symbol) => {
            hasher.update(&[6]);
            hash_symbol_id(hasher, *symbol);
        }
    }
}

impl StableHash for crate::dataflow::DataflowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"DataflowFacts/v1");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.live_in_count));
        h.update(&u32_le(self.live_out_count));
        h.update(&u32_le(self.init_in_count));
        h.update(&u32_le(self.moved_in_count));
        h.update(&u32_le(self.stmt_count));
        finish(h)
    }
}

impl StableHash for crate::dataflow::BorrowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BorrowFacts/v2");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.shared_in_count));
        h.update(&u32_le(self.exclusive_in_count));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out_count));
        h.update(&u32_le(self.exclusive_out_count));
        finish(h)
    }
}

impl StableHash for arandu_mir::borrow_facts::BlockBorrowSummary {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BlockBorrowSummary/v2");
        h.update(&u32_le(self.shared_in));
        h.update(&u32_le(self.exclusive_in));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out));
        h.update(&u32_le(self.exclusive_out));
        finish(h)
    }
}

impl StableHash for Vec<arandu_mir::borrow_facts::BlockBorrowSummary> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"Vec<BlockBorrowSummary>/v1");
        h.update(&u64_le(self.len() as u64));
        for s in self {
            h.update(s.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::LivenessMap {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"LivenessMap/v1");
        h.update(&u64_le(self.live_in_counts.len() as u64));
        for &c in &self.live_in_counts {
            h.update(&u32_le(c));
        }
        for &c in &self.live_out_counts {
            h.update(&u32_le(c));
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::IdeDiagnostic {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(self.code.as_bytes());
        h.update(&[self.severity]);
        h.update(self.message.as_bytes());
        if let Some(primary_label) = &self.primary_label {
            h.update(&[1]);
            hash_str(&mut h, primary_label);
        } else {
            h.update(&[0]);
        }
        h.update(&u32_le(self.file_id));
        h.update(&u32_le(self.start));
        h.update(&u32_le(self.end));
        h.update(&u64_le(self.labels.len() as u64));
        for label in &self.labels {
            h.update(&u32_le(label.file_id));
            h.update(&u32_le(label.start));
            h.update(&u32_le(label.end));
            hash_str(&mut h, &label.message);
        }
        h.update(&u64_le(self.notes.len() as u64));
        for note in &self.notes {
            hash_str(&mut h, note);
        }
        h.update(&u64_le(self.hints.len() as u64));
        for hint in &self.hints {
            hash_str(&mut h, &hint.message);
            if let Some(replacement) = &hint.replacement {
                h.update(&[1]);
                h.update(&u32_le(replacement.file_id));
                h.update(&u32_le(replacement.start));
                h.update(&u32_le(replacement.end));
                hash_str(&mut h, &replacement.new_text);
            } else {
                h.update(&[0]);
            }
        }
        if let Some(f) = self.func {
            h.update(&[1]);
            hash_symbol_id(&mut h, f);
        } else {
            h.update(&[0]);
        }
        if let Some(b) = self.block {
            h.update(&[1]);
            h.update(&u32_le(b.as_usize() as u32));
        } else {
            h.update(&[0]);
        }
        finish(h)
    }
}

impl StableHash for Vec<crate::dataflow::IdeDiagnostic> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for d in self {
            h.update(d.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for Vec<arandu_middle::SymbolId> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for id in self {
            hash_symbol_id(&mut h, *id);
        }
        finish(h)
    }
}

impl StableHash for crate::passes::ItemSourceInput {
    fn stable_hash(&self) -> blake3::Hash {
        // Content-address only this item's source fingerprint — not the whole Program.
        let mut h = Hasher::new();
        h.update(b"ItemSourceInput/v3");
        hash_symbol_id(&mut h, self.item_sym);
        h.update(&u32_le(self.item_start));
        h.update(self.body_fp.as_bytes());
        finish(h)
    }
}

impl StableHash for Arc<[crate::highlight::HlToken]> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"HlTokenSlice/v1");
        h.update(&u64_le(self.len() as u64));
        for t in self.iter() {
            h.update(&u32_le(t.start));
            h.update(&u32_le(t.end));
            h.update(&[t.kind as u8]);
            h.update(&u32_le(u32::from(t.mods)));
        }
        finish(h)
    }
}

impl StableHash for arandu_parser::SyntaxTree {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"SyntaxTree/v2");
        h.update(self.text().as_bytes());
        // Hash ranges (no per-item String alloc).
        let ranges = self.item_ranges();
        h.update(&u64_le(ranges.len() as u64));
        let text = self.text();
        let bytes = text.as_bytes();
        for (s, e) in ranges {
            h.update(&u32_le(s));
            h.update(&u32_le(e));
            let s = (s as usize).min(bytes.len());
            let e = (e as usize).min(bytes.len()).max(s);
            h.update(&bytes[s..e]);
        }
        finish(h)
    }
}
