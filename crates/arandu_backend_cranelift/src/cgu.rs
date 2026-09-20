//! Partitioned Ahead-Of-Time (AOT) Code Generation Units (CGUs).
//!
//! A CGU digest is a versioned, canonical encoding of machine-code inputs. It
//! deliberately excludes source spans and diagnostics, while including the
//! compiler identity, target, optimization policy, AMIR, concrete layouts and
//! direct-callee signatures.

use std::collections::BTreeSet;

use arandu_semantics::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirPlace, AmirProgram, AmirProjection, AmirRvalue,
    AmirStmt, AmirTerminator, for_each_place_operand, for_each_rvalue_operand,
    for_each_terminator_operand,
};
use arandu_semantics::literal_pool::AmirLiteralEntry;
use arandu_semantics::types::{
    ArType, BorrowKind, BorrowPath, BorrowPathSegment, ReturnBorrowSummary, TypeId,
};
use arandu_semantics::{Diagnostic, EnumPayloadShape, SymbolId, SymbolTable, TypeInfo};
use target_lexicon::Triple;

use crate::aot::{AotOptimization, CraneliftObjectBackend};
use crate::jit::isa::codegen_ice;

const CGU_HASH_SCHEMA: &[u8] = b"arandu-cgu-input-v2\0";
const CRANELIFT_IDENTITY: &[u8] = b"cranelift-0.134.3\0";

/// A discrete compilation unit corresponding to one function or item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenUnit {
    /// Human-readable identity of this unit (e.g. `main` or `math_add`).
    pub name: String,
    /// Canonical `SymbolId` of the primary function.
    pub symbol: SymbolId,
    /// BLAKE3 digest of every machine-code input represented by this CGU.
    pub hash: String,
}

/// Partition `program` into individual [`CodegenUnit`]s with deterministic hashes.
pub fn partition_program(
    program: &AmirProgram,
    symbols: &SymbolTable,
    type_info: &TypeInfo,
    target: &Triple,
    optimization: AotOptimization,
    toolchain_fingerprint: &str,
) -> Vec<CodegenUnit> {
    let mut units = Vec::with_capacity(program.funcs.len());

    for func in &program.funcs {
        let name = symbols.try_get(func.symbol).map_or_else(
            || format!("symbol_{}_{}", func.symbol.file_id, func.symbol.local_id.0),
            |symbol| symbols.host_func_name(symbol).to_owned(),
        );
        let hash = compute_cgu_hash(
            func,
            program,
            symbols,
            type_info,
            target,
            optimization,
            toolchain_fingerprint,
        );
        units.push(CodegenUnit {
            name,
            symbol: func.symbol,
            hash,
        });
    }

    units
}

/// Compute a stable digest for one function's complete machine-code inputs.
pub fn compute_cgu_hash(
    func: &AmirFunc,
    program: &AmirProgram,
    symbols: &SymbolTable,
    type_info: &TypeInfo,
    target: &Triple,
    optimization: AotOptimization,
    toolchain_fingerprint: &str,
) -> String {
    let mut hash = StableHash::new();
    hash.bytes(CGU_HASH_SCHEMA);
    hash.bytes(CRANELIFT_IDENTITY);
    hash.str(toolchain_fingerprint);
    hash.str(&target.to_string());
    hash.tag(match optimization {
        AotOptimization::Baseline => 0,
        AotOptimization::Speed => 1,
    });

    let mut context = HashContext {
        hash: &mut hash,
        program,
        symbols,
        type_info,
        named_type_stack: Vec::new(),
    };
    context.func(func);
    context.direct_callee_signatures(func);
    // ObjectModule declares the complete module surface before defining this
    // function. Keep that declaration closure in the cache key: removing or
    // changing a declaration must never reuse an object whose relocation or
    // platform metadata was produced against the previous closure.
    context.module_declaration_surface();
    hash.finish()
}

struct StableHash(blake3::Hasher);

impl StableHash {
    fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    fn tag(&mut self, value: u8) {
        self.0.update(&[value]);
    }

    fn bool(&mut self, value: bool) {
        self.tag(u8::from(value));
    }

    fn u32(&mut self, value: u32) {
        self.0.update(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(&value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        match u64::try_from(value) {
            Ok(value) => {
                self.tag(0);
                self.u64(value);
            }
            Err(_) => self.tag(1),
        }
    }

    fn i128(&mut self, value: i128) {
        self.0.update(&value.to_le_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.usize(value.len());
        self.0.update(value);
    }

    fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn finish(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

struct HashContext<'a> {
    hash: &'a mut StableHash,
    program: &'a AmirProgram,
    symbols: &'a SymbolTable,
    type_info: &'a TypeInfo,
    named_type_stack: Vec<SymbolId>,
}

impl HashContext<'_> {
    fn module_declaration_surface(&mut self) {
        self.hash.usize(self.program.funcs.len());
        for func in &self.program.funcs {
            self.symbol(func.symbol);
            self.func_signature(func);
        }

        let mut externs: Vec<_> = self.program.extern_funcs.iter().collect();
        externs.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        self.hash.usize(externs.len());
        for (&symbol, (params, result)) in externs {
            self.symbol(symbol);
            self.hash.usize(params.len());
            for param in params {
                self.ar_type(param);
            }
            self.ar_type(result);
        }
    }

    fn symbol(&mut self, id: SymbolId) {
        if let Some(symbol) = self.symbols.try_get(id) {
            self.hash.tag(0);
            self.hash.str(self.symbols.host_func_name(symbol));
            self.hash.str(&symbol.name);
        } else {
            // Invalid semantic metadata must not crash codegen or collide with a
            // valid symbol. The numeric identity gives the diagnostic path a
            // deterministic cache miss until validation reports the ICE.
            self.hash.tag(1);
        }
        self.hash.u32(id.file_id);
        self.hash.u32(id.local_id.0);
    }

    fn type_id(&mut self, id: TypeId) {
        let Some(ty) = self.type_info.type_interner.try_resolve(id) else {
            self.hash.tag(u8::MAX);
            self.hash.usize(id.as_usize());
            return;
        };
        self.ar_type(&ty);
        if let Some(&destructor) = self.type_info.destructor_instances.get(&id) {
            self.hash.tag(1);
            self.symbol(destructor);
        } else {
            self.hash.tag(0);
        }
    }

    fn type_args(&mut self, range: arandu_semantics::hir::pool::IndexRange) {
        self.hash.u32(range.len);
        for index in 0..range.len as usize {
            match self.type_info.type_interner.type_arg(range, index) {
                Some(id) => {
                    self.hash.tag(0);
                    self.type_id(id);
                }
                None => self.hash.tag(1),
            }
        }
    }

    fn ar_type(&mut self, ty: &ArType) {
        match ty {
            ArType::Primitive(primitive) => {
                self.hash.tag(0);
                self.hash.str(primitive.as_str());
            }
            ArType::Named(symbol, args) => {
                self.hash.tag(1);
                self.symbol(*symbol);
                self.type_args(*args);
                self.named_layout(*symbol);
            }
            ArType::Func(params, result) => {
                self.hash.tag(2);
                self.type_args(*params);
                self.type_id(*result);
            }
            ArType::Nullable(inner) => {
                self.hash.tag(3);
                self.type_id(*inner);
            }
            ArType::Slice(inner) => {
                self.hash.tag(4);
                self.type_id(*inner);
            }
            ArType::Array(len, inner) => {
                self.hash.tag(5);
                self.hash.u64(*len);
                self.type_id(*inner);
            }
            ArType::Ptr(inner) => {
                self.hash.tag(6);
                self.type_id(*inner);
            }
            ArType::Ref(inner) => {
                self.hash.tag(7);
                self.type_id(*inner);
            }
            ArType::RefMut(inner) => {
                self.hash.tag(8);
                self.type_id(*inner);
            }
            ArType::GenRef => self.hash.tag(9),
            ArType::Tuple(items) => {
                self.hash.tag(10);
                self.type_args(*items);
            }
            ArType::Result(ok, error) => {
                self.hash.tag(11);
                self.type_id(*ok);
                self.type_id(*error);
            }
            ArType::Option(inner) => {
                self.hash.tag(12);
                self.type_id(*inner);
            }
            ArType::Coroutine(inner) => {
                self.hash.tag(13);
                self.type_id(*inner);
            }
            ArType::Poll(inner) => {
                self.hash.tag(14);
                self.type_id(*inner);
            }
            ArType::Range(inner) => {
                self.hash.tag(15);
                self.type_id(*inner);
            }
            ArType::Err => self.hash.tag(16),
            ArType::Void => self.hash.tag(17),
            ArType::IntLiteral => self.hash.tag(18),
            ArType::FloatLiteral => self.hash.tag(19),
            ArType::Error => self.hash.tag(20),
            ArType::ConstArray(param, inner) => {
                self.hash.tag(21);
                self.symbol(*param);
                self.type_id(*inner);
            }
            ArType::Const(value) => {
                self.hash.tag(22);
                self.hash.u64(*value);
            }
            ArType::ConstParam(param) => {
                self.hash.tag(23);
                self.symbol(*param);
            }
        }
    }

    fn named_layout(&mut self, symbol: SymbolId) {
        if self.named_type_stack.contains(&symbol) {
            self.hash.tag(0);
            return;
        }
        self.hash.tag(1);
        self.named_type_stack.push(symbol);

        if let Some(fields) = self.type_info.struct_fields.get(&symbol) {
            self.hash.tag(0);
            self.hash.usize(fields.fields.len());
            for field in &fields.fields {
                self.hash.str(&field.name);
                self.hash.usize(field.index);
                if let Some(field_symbol) = field.symbol {
                    self.hash.tag(1);
                    self.symbol(field_symbol);
                } else {
                    self.hash.tag(0);
                }
                self.type_id(field.ty);
            }
        } else {
            self.hash.tag(1);
        }

        let mut variants: Vec<_> = self
            .type_info
            .enum_variants
            .iter()
            .filter(|(_, (parent, _))| *parent == symbol)
            .map(|(&variant, (_, shape))| {
                (
                    self.type_info
                        .enum_variant_tags
                        .get(&variant)
                        .copied()
                        .unwrap_or(usize::MAX),
                    variant.file_id,
                    variant.local_id.0,
                    variant,
                    shape,
                )
            })
            .collect();
        variants.sort_by_key(|&(tag, file_id, local_id, _, _)| (tag, file_id, local_id));
        self.hash.usize(variants.len());
        for (tag, _, _, variant, shape) in variants {
            self.hash.usize(tag);
            self.symbol(variant);
            match shape {
                EnumPayloadShape::Unit => self.hash.tag(0),
                EnumPayloadShape::Tuple(types) => {
                    self.hash.tag(1);
                    self.hash.usize(types.len());
                    for &ty in types {
                        self.type_id(ty);
                    }
                }
            }
        }

        let removed = self.named_type_stack.pop();
        debug_assert_eq!(removed, Some(symbol));
    }

    fn func_signature(&mut self, func: &AmirFunc) {
        self.symbol(func.symbol);
        self.type_id(func.return_type);
        self.hash.usize(func.params.len());
        for &param in &func.params {
            self.hash.usize(param.as_usize());
            if let Some(temp) = func.temps.get(param.as_usize()) {
                self.hash.tag(0);
                self.type_id(temp.ty);
            } else {
                self.hash.tag(1);
            }
        }
        if let Some(receiver) = func.receiver {
            self.hash.tag(1);
            self.hash.usize(receiver.temp.as_usize());
            self.hash.tag(match receiver.kind {
                arandu_semantics::hir::ReceiverKind::Shared => 0,
                arandu_semantics::hir::ReceiverKind::Mut => 1,
                arandu_semantics::hir::ReceiverKind::Own => 2,
            });
        } else {
            self.hash.tag(0);
        }
        self.hash.u32(
            self.type_info
                .function_effects
                .get(&func.symbol)
                .copied()
                .unwrap_or_default()
                .0,
        );
    }

    fn func(&mut self, func: &AmirFunc) {
        self.func_signature(func);

        self.hash.usize(func.locals.len());
        for local in &func.locals {
            self.hash.usize(local.id.as_usize());
            self.type_id(local.ty);
            self.hash.bool(local.is_memory);
            if let Some(symbol) = local.symbol {
                self.hash.tag(1);
                self.symbol(symbol);
            } else {
                self.hash.tag(0);
            }
        }

        self.hash.usize(func.temps.len());
        for temp in &func.temps {
            self.hash.usize(temp.id.as_usize());
            self.type_id(temp.ty);
            self.hash.bool(temp.is_copy);
            self.hash.bool(temp.is_nullable);
        }

        self.hash.usize(func.block_params.len());
        for param in &func.block_params {
            self.hash.usize(param.id.as_usize());
            self.hash.usize(param.local.as_usize());
            self.type_id(param.ty);
            self.hash.bool(param.moved);
        }

        self.hash.usize(func.blocks.len());
        for block in &func.blocks {
            self.hash.usize(block.id.as_usize());
            let params = func.block_params(block.params);
            self.hash.usize(params.len());
            for param in params {
                self.hash.usize(param.id.as_usize());
            }

            self.hash.u32(block.statements.len);
            for statement_id in block.statements.iter_ids() {
                if let Some(statement) = func.try_stmt(statement_id) {
                    self.hash.tag(0);
                    self.stmt(statement);
                } else {
                    self.hash.tag(1);
                    self.hash.usize(statement_id.as_usize());
                }
            }
            self.terminator(&block.terminator);
        }
    }

    fn place(&mut self, place: &AmirPlace) {
        self.hash.usize(place.local.as_usize());
        self.hash.usize(place.projections.len());
        for projection in &place.projections {
            match projection {
                AmirProjection::Field(symbol) => {
                    self.hash.tag(0);
                    self.symbol(*symbol);
                }
                AmirProjection::Index(operand) => {
                    self.hash.tag(1);
                    self.operand(operand);
                }
                AmirProjection::Deref => self.hash.tag(2),
            }
        }
    }

    fn operand(&mut self, operand: &AmirOperand) {
        match operand {
            AmirOperand::Copy(temp) => {
                self.hash.tag(0);
                self.hash.usize(temp.as_usize());
            }
            AmirOperand::Move(temp) => {
                self.hash.tag(1);
                self.hash.usize(temp.as_usize());
            }
            AmirOperand::Constant(constant) => {
                self.hash.tag(2);
                self.constant(constant);
            }
            AmirOperand::FunctionRef(symbol) => {
                self.hash.tag(3);
                self.symbol(*symbol);
            }
            AmirOperand::GlobalRef(symbol) => {
                self.hash.tag(4);
                self.symbol(*symbol);
            }
        }
    }

    fn constant(&mut self, constant: &AmirConstant) {
        match constant {
            AmirConstant::Pool(id) => {
                self.hash.tag(0);
                self.hash.u32(id.0);
                match self.program.literal_pool.entries.get(id.0 as usize) {
                    Some(literal) => {
                        self.hash.tag(0);
                        self.literal(literal);
                    }
                    None => self.hash.tag(1),
                }
            }
            AmirConstant::Bool(value) => {
                self.hash.tag(1);
                self.hash.bool(*value);
            }
            AmirConstant::Nil => self.hash.tag(2),
        }
    }

    fn literal(&mut self, literal: &AmirLiteralEntry) {
        match literal {
            AmirLiteralEntry::Int(value) => {
                self.hash.tag(0);
                self.hash.str(value);
            }
            AmirLiteralEntry::Float(value) => {
                self.hash.tag(1);
                self.hash.str(value);
            }
            AmirLiteralEntry::Str(value) => {
                self.hash.tag(2);
                self.hash.str(value);
            }
            AmirLiteralEntry::Char(value) => {
                self.hash.tag(3);
                self.hash.str(value);
            }
        }
    }

    fn operands(&mut self, operands: &[AmirOperand]) {
        self.hash.usize(operands.len());
        for operand in operands {
            self.operand(operand);
        }
    }

    fn rvalue(&mut self, rvalue: &AmirRvalue) {
        match rvalue {
            AmirRvalue::Use(operand) => {
                self.hash.tag(0);
                self.operand(operand);
            }
            AmirRvalue::Binary { op, left, right } => {
                self.hash.tag(1);
                self.hash.tag(op.stable_tag());
                self.operand(left);
                self.operand(right);
            }
            AmirRvalue::Unary { op, operand } => {
                self.hash.tag(2);
                self.hash.tag(op.stable_tag());
                self.operand(operand);
            }
            AmirRvalue::FieldAccess { base, field } => {
                self.hash.tag(3);
                self.operand(base);
                self.hash.usize(*field);
            }
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                self.hash.tag(4);
                self.symbol(*struct_symbol);
                self.hash.usize(fields.len());
                for (name, operand) in fields {
                    self.hash.str(name);
                    self.operand(operand);
                }
            }
            AmirRvalue::IndexAccess { base, index } => {
                self.hash.tag(5);
                self.operand(base);
                self.operand(index);
            }
            AmirRvalue::Array { items } => {
                self.hash.tag(6);
                self.operands(items);
            }
            AmirRvalue::Tuple { items } => {
                self.hash.tag(7);
                self.operands(items);
            }
            AmirRvalue::Discriminant { value } => {
                self.hash.tag(8);
                self.operand(value);
            }
            AmirRvalue::EnumPayload {
                value,
                variant,
                index,
            } => {
                self.hash.tag(9);
                self.operand(value);
                self.symbol(*variant);
                self.hash.usize(*index);
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                self.hash.tag(10);
                self.hash.usize(*variant_tag);
                if let Some(payload) = payload {
                    self.hash.tag(1);
                    self.operand(payload);
                } else {
                    self.hash.tag(0);
                }
            }
            AmirRvalue::Len(operand) => {
                self.hash.tag(11);
                self.operand(operand);
            }
            AmirRvalue::SliceView { owner, data, len } => {
                self.hash.tag(12);
                self.operand(owner);
                self.operand(data);
                self.operand(len);
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                self.hash.tag(13);
                self.operand(slice);
                self.operand(start);
                self.operand(len);
            }
            AmirRvalue::SliceData(operand) => {
                self.hash.tag(29);
                self.operand(operand);
            }
            AmirRvalue::StrView { owner } => {
                self.hash.tag(14);
                self.operand(owner);
            }
            AmirRvalue::Alloc(operand) => {
                self.hash.tag(15);
                self.operand(operand);
            }
            AmirRvalue::Load(place) => {
                self.hash.tag(16);
                self.place(place);
            }
            AmirRvalue::Borrow(place) => {
                self.hash.tag(17);
                self.place(place);
            }
            AmirRvalue::BorrowMut(place) => {
                self.hash.tag(18);
                self.place(place);
            }
            AmirRvalue::RelativeBorrow { local, mutable } => {
                self.hash.tag(19);
                self.hash.usize(local.as_usize());
                self.hash.bool(*mutable);
            }
            AmirRvalue::CoroutineReady {
                value,
                payload_ty,
                stack,
            } => {
                self.hash.tag(20);
                self.operand(value);
                self.type_id(*payload_ty);
                self.hash.bool(*stack);
            }
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => {
                self.hash.tag(21);
                self.operand(value);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            } => {
                self.hash.tag(22);
                self.operand(gen_ref);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                self.hash.tag(23);
                self.operand(gen_ref);
                self.operand(value);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                self.hash.tag(24);
                self.operand(gen_ref);
                self.operand(value);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => {
                self.hash.tag(25);
                self.operand(gen_ref);
                self.type_id(*payload_ty);
            }
            AmirRvalue::StringInterp { parts } => {
                self.hash.tag(26);
                self.operands(parts);
            }
            AmirRvalue::ToStr { value, src_ty } => {
                self.hash.tag(27);
                self.operand(value);
                self.type_id(*src_ty);
            }
            AmirRvalue::BlackBox { value, value_ty } => {
                self.hash.tag(28);
                self.operand(value);
                self.type_id(*value_ty);
            }
        }
    }

    fn stmt(&mut self, stmt: &AmirStmt) {
        match stmt {
            AmirStmt::Assign { lhs, rhs } => {
                self.hash.tag(0);
                self.hash.usize(lhs.as_usize());
                self.rvalue(rhs);
            }
            AmirStmt::Store { lhs, rhs } => {
                self.hash.tag(1);
                self.place(lhs);
                self.operand(rhs);
            }
            AmirStmt::Call {
                lhs,
                callee,
                args,
                return_borrow,
            } => {
                self.hash.tag(2);
                if let Some(lhs) = lhs {
                    self.hash.tag(1);
                    self.hash.usize(lhs.as_usize());
                } else {
                    self.hash.tag(0);
                }
                self.operand(callee);
                self.operands(args);
                if let Some(summary) = return_borrow {
                    self.hash.tag(1);
                    self.borrow_summary(summary);
                } else {
                    self.hash.tag(0);
                }
            }
            AmirStmt::Free(operand) => {
                self.hash.tag(3);
                self.operand(operand);
            }
            AmirStmt::StorageLive(local) => {
                self.hash.tag(4);
                self.hash.usize(local.as_usize());
            }
            AmirStmt::StorageDead(local) => {
                self.hash.tag(5);
                self.hash.usize(local.as_usize());
            }
            AmirStmt::Destroy(place) => {
                self.hash.tag(6);
                self.place(place);
            }
            AmirStmt::Nop => self.hash.tag(7),
        }
    }

    fn terminator(&mut self, terminator: &AmirTerminator) {
        match terminator {
            AmirTerminator::Return => self.hash.tag(0),
            AmirTerminator::Goto { target, args } => {
                self.hash.tag(1);
                self.hash.usize(target.as_usize());
                self.operands(args);
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
            } => {
                self.hash.tag(2);
                self.operand(condition);
                self.hash.usize(if_true.as_usize());
                self.operands(true_args);
                self.hash.usize(if_false.as_usize());
                self.operands(false_args);
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                self.hash.tag(3);
                self.operand(discriminant);
                self.hash.usize(targets.len());
                for (value, target, args) in targets {
                    self.hash.i128(*value);
                    self.hash.usize(target.as_usize());
                    self.operands(args);
                }
                self.hash.usize(otherwise.0.as_usize());
                self.operands(&otherwise.1);
            }
            AmirTerminator::Suspend {
                future,
                resume,
                args,
            } => {
                self.hash.tag(4);
                self.operand(future);
                self.hash.usize(resume.as_usize());
                self.operands(args);
            }
            AmirTerminator::Unreachable => self.hash.tag(5),
        }
    }

    fn borrow_summary(&mut self, summary: &ReturnBorrowSummary) {
        self.hash.usize(summary.dependencies.len());
        for dependency in &summary.dependencies {
            self.borrow_path(&dependency.result_path);
            self.hash.tag(match dependency.kind {
                BorrowKind::Shared => 0,
                BorrowKind::Exclusive => 1,
            });
            self.hash.usize(dependency.sources.len());
            for source in &dependency.sources {
                self.hash.u32(source.parameter_index);
                self.borrow_path(&source.parameter_path);
            }
        }
    }

    fn borrow_path(&mut self, path: &BorrowPath) {
        self.hash.usize(path.len());
        for segment in path.iter() {
            match segment {
                BorrowPathSegment::Tuple(index) => {
                    self.hash.tag(0);
                    self.hash.u32(*index);
                }
                BorrowPathSegment::Field(name) => {
                    self.hash.tag(1);
                    self.hash.str(name);
                }
                BorrowPathSegment::Variant(index) => {
                    self.hash.tag(2);
                    self.hash.u32(*index);
                }
                BorrowPathSegment::Payload(index) => {
                    self.hash.tag(3);
                    self.hash.u32(*index);
                }
                BorrowPathSegment::OptionSome => self.hash.tag(4),
                BorrowPathSegment::ResultOk => self.hash.tag(5),
                BorrowPathSegment::ResultErr => self.hash.tag(6),
                BorrowPathSegment::ArrayElement => self.hash.tag(7),
                BorrowPathSegment::NullableValue => self.hash.tag(8),
                BorrowPathSegment::CoroutinePayload => self.hash.tag(9),
                BorrowPathSegment::PollReady => self.hash.tag(10),
                BorrowPathSegment::RangeElement => self.hash.tag(11),
            }
        }
    }

    fn direct_callee_signatures(&mut self, func: &AmirFunc) {
        let mut callees = BTreeSet::new();
        {
            let mut collect = |operand: &AmirOperand| {
                if let AmirOperand::FunctionRef(symbol) = operand {
                    callees.insert((symbol.file_id, symbol.local_id.0));
                }
            };

            for block in &func.blocks {
                for statement_id in block.statements.iter_ids() {
                    let Some(statement) = func.try_stmt(statement_id) else {
                        continue;
                    };
                    match statement {
                        AmirStmt::Assign { rhs, .. } => for_each_rvalue_operand(rhs, &mut collect),
                        AmirStmt::Store { lhs, rhs } => {
                            collect(rhs);
                            for_each_place_operand(lhs, &mut collect);
                        }
                        AmirStmt::Call { callee, args, .. } => {
                            collect(callee);
                            for operand in args {
                                collect(operand);
                            }
                        }
                        AmirStmt::Free(operand) => collect(operand),
                        AmirStmt::Destroy(place) => for_each_place_operand(place, &mut collect),
                        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
                    }
                }
                for_each_terminator_operand(&block.terminator, &mut collect);
            }
        }

        self.hash.usize(callees.len());
        for (file_id, local_id) in callees {
            let symbol = SymbolId::new(file_id, local_id);
            self.symbol(symbol);
            if let Some(callee) = self.program.funcs.iter().find(|func| func.symbol == symbol) {
                self.hash.tag(0);
                self.func_signature(callee);
            } else if let Some((params, result)) = self.program.extern_funcs.get(&symbol) {
                self.hash.tag(1);
                self.hash.usize(params.len());
                for param in params {
                    self.ar_type(param);
                }
                self.ar_type(result);
            } else if let Some(&ty) = self.type_info.decl_types.get(&symbol) {
                self.hash.tag(2);
                self.type_id(ty);
            } else {
                self.hash.tag(3);
            }
        }
    }
}

/// Compile a single [`CodegenUnit`] into an in-memory relocatable object.
pub fn compile_cgu(
    unit: &CodegenUnit,
    program: &AmirProgram,
    symbols: &SymbolTable,
    type_info: &TypeInfo,
    target: &Triple,
    optimization: AotOptimization,
) -> Result<Vec<u8>, Diagnostic> {
    let backend =
        CraneliftObjectBackend::for_target_with_optimization(target.clone(), optimization)?;
    let mut compiler = backend.into_compiler();

    compiler.compile_filtered_module(program, symbols, type_info, Some(&[unit.symbol]))?;

    let product = compiler.module.finish();
    product.emit().map_err(|error| {
        codegen_ice(format!(
            "failed to emit CGU object '{}' for target '{target}': {error}",
            unit.name
        ))
    })
}
