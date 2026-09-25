//! AMIR → Cranelift IR function translator.
//!
//! [`FunctionTranslator`] walks an [`AmirFunc`] basic block by block,
//! emitting Cranelift IR instructions via a [`FunctionBuilder`]. The
//! [`AmirVisitor`] trait defines the visit callbacks used during traversal.

mod call;
mod compare;
mod coroutine;
mod expr;
mod memory;
mod operand;
mod place;
mod stmt;
mod string;
mod terminator;

use arandu_base::span::Span;
use arandu_semantics::amir::{
    AmirBasicBlock, AmirConstant, AmirDebugBinding, AmirFunc, AmirOperand, AmirStmt,
    AmirTerminator, BlockId, InstrId, LocalId, TempId,
};
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use arandu_semantics::{DiagCode, Diagnostic, SymbolTable};
use cranelift_codegen::ir::{
    Block, InstBuilder, RelSourceLoc, SourceLoc, StackSlot, Type, Value, ValueLabel,
    ValueLabelAssignments, ValueLabelStart,
};
use cranelift_frontend::{FunctionBuilder, Variable};
use cranelift_module::{FuncId, Module};
use rustc_hash::FxHashMap;

use crate::types::{ClifType, clif_type, clif_types};

/// Visitor callbacks invoked while translating an [`AmirFunc`] to Cranelift IR.
pub trait AmirVisitor {
    /// Called once for each basic block before its statements are visited.
    fn visit_block(&mut self, block: &AmirBasicBlock);
    /// Called for each statement within the current block.
    fn visit_stmt(&mut self, stmt: &AmirStmt);
    /// Called for the block terminator after all statements have been visited.
    fn visit_terminator(&mut self, term: &AmirTerminator);
}

/// Translates a single [`AmirFunc`] into Cranelift IR using a [`FunctionBuilder`].
///
/// Holds all per-function compilation state: block/temp/local mappings,
/// string fat-pointer variables, and a deferred error slot so that
/// translation can continue after the first failure.
pub struct FunctionTranslator<'a, 'b, M: Module> {
    pub builder: FunctionBuilder<'a>,
    pub module: &'b mut M,
    pub symbol_table: &'b SymbolTable,
    pub func_ids: &'b FxHashMap<String, FuncId>,
    pub block_map: FxHashMap<BlockId, Block>,
    pub temp_map: FxHashMap<TempId, Variable>,
    pub local_map: FxHashMap<LocalId, Variable>,
    /// Stack homes for address-taken scalar locals (F2.0 `&`/`&mut`).
    /// Aggregate/heap locals keep their address in `local_map` (pointer SSA).
    pub local_stack_slots: FxHashMap<LocalId, StackSlot>,
    pub str_temp_map: FxHashMap<TempId, (Variable, Variable)>,
    pub str_local_map: FxHashMap<LocalId, (Variable, Variable)>,
    pub ptr_type: Type,
    pub literal_pool: &'b arandu_semantics::literal_pool::AmirLiteralPool,
    pub current_func: &'b AmirFunc,
    pub type_info: &'b arandu_semantics::TypeInfo,
    block_coverage: Option<(FuncId, u32, i64)>,
    pub(crate) debug_locations: Option<&'b mut Vec<Span>>,
    debug_temp_locals: Option<FxHashMap<TempId, Vec<LocalId>>>,
    current_debug_location: Option<u32>,
    pub(crate) error: Option<Diagnostic>,
}

impl<'a, 'b, M: Module> FunctionTranslator<'a, 'b, M> {
    pub(super) fn materialize_slice_descriptor(&mut self, data: Value, len: Value) -> Value {
        let pointer_bytes = self.ptr_type.bytes();
        let slot = self
            .builder
            .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData {
                kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                size: pointer_bytes * 2,
                align_shift: pointer_bytes.trailing_zeros() as u8,
                key: None,
            });
        self.builder.ins().stack_store(self.ptr_type, data, slot, 0);
        self.builder
            .ins()
            .stack_store(self.ptr_type, len, slot, pointer_bytes as i32);
        self.builder.ins().stack_addr(self.ptr_type, slot, 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        builder: FunctionBuilder<'a>,
        module: &'b mut M,
        symbol_table: &'b SymbolTable,
        func_ids: &'b FxHashMap<String, FuncId>,
        ptr_type: Type,
        literal_pool: &'b arandu_semantics::literal_pool::AmirLiteralPool,
        current_func: &'b AmirFunc,
        type_info: &'b arandu_semantics::TypeInfo,
        debug_locations: Option<&'b mut Vec<Span>>,
        debug_bindings: &'b [AmirDebugBinding],
        block_coverage: Option<(FuncId, u32, i64)>,
    ) -> Self {
        let debug_temp_locals = debug_locations.as_ref().map(|_| {
            let mut by_temp = FxHashMap::<TempId, Vec<LocalId>>::default();
            for binding in debug_bindings {
                if binding.function == current_func.symbol {
                    let locals = by_temp.entry(binding.temp).or_default();
                    if !locals.contains(&binding.local) {
                        locals.push(binding.local);
                    }
                }
            }
            by_temp
        });
        Self {
            builder,
            module,
            symbol_table,
            func_ids,
            block_map: FxHashMap::default(),
            temp_map: FxHashMap::default(),
            local_map: FxHashMap::default(),
            local_stack_slots: FxHashMap::default(),
            str_temp_map: FxHashMap::default(),
            str_local_map: FxHashMap::default(),
            ptr_type,
            literal_pool,
            current_func,
            type_info,
            block_coverage,
            debug_locations,
            debug_temp_locals,
            current_debug_location: None,
            error: None,
        }
    }

    fn set_debug_span(&mut self, span: Span) {
        let Some(locations) = &mut self.debug_locations else {
            return;
        };
        let Ok(index) = u32::try_from(locations.len()) else {
            self.record_ice("DWARF source-location table exceeds u32", span);
            return;
        };
        locations.push(span);
        self.builder.set_srcloc(SourceLoc::new(index));
        self.current_debug_location = Some(index);
    }

    pub(crate) fn label_local_value(&mut self, local: LocalId, value: Value) {
        let Some(location) = self.current_debug_location else {
            return;
        };
        let Ok(label) = u32::try_from(local.as_usize()) else {
            self.record_ice("DWARF local label exceeds u32", self.local_span(local));
            return;
        };
        let from = RelSourceLoc::from_base_offset(
            self.builder.func.params.base_srcloc(),
            SourceLoc::new(location),
        );
        let Some(labels) = self.builder.func.dfg.values_labels.as_mut() else {
            return;
        };
        let starts = labels
            .entry(value)
            .or_insert_with(|| ValueLabelAssignments::Starts(Vec::new()));
        if let ValueLabelAssignments::Starts(starts) = starts {
            starts.push(ValueLabelStart {
                from,
                label: ValueLabel::from_u32(label),
            });
        }
    }

    pub(crate) fn label_temp_value(&mut self, temp: TempId, value: Value) {
        let count = self
            .debug_temp_locals
            .as_ref()
            .and_then(|bindings| bindings.get(&temp))
            .map_or(0, Vec::len);
        for index in 0..count {
            let local = self.debug_temp_locals.as_ref().and_then(|bindings| {
                bindings
                    .get(&temp)
                    .and_then(|locals| locals.get(index))
                    .copied()
            });
            if let Some(local) = local {
                self.label_local_value(local, value);
            }
        }
    }

    fn operand_span(&self, operand: &AmirOperand) -> Span {
        match operand {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => self.temp_span(*temp),
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => {
                self.symbol_table.get(*symbol).span
            }
            AmirOperand::Constant(_) => self.func_span(),
        }
    }

    fn stmt_span(&self, stmt: &AmirStmt) -> Span {
        match stmt {
            AmirStmt::Assign { lhs, .. } => self.temp_span(*lhs),
            AmirStmt::Store { lhs, rhs } => {
                let rhs_span = self.operand_span(rhs);
                if rhs_span.start != rhs_span.end {
                    rhs_span
                } else {
                    self.local_span(lhs.local)
                }
            }
            AmirStmt::Call { lhs, callee, .. } => lhs
                .map(|temp| self.temp_span(temp))
                .unwrap_or_else(|| self.operand_span(callee)),
            AmirStmt::Free(operand) => self.operand_span(operand),
            AmirStmt::StorageLive(local)
            | AmirStmt::StorageDead(local)
            | AmirStmt::Destroy(arandu_semantics::amir::AmirPlace { local, .. }) => {
                self.local_span(*local)
            }
            AmirStmt::Nop => self.func_span(),
        }
    }

    /// Scalars that normally live in registers need a stack home when
    /// address-taken (`is_memory` from F2.0 `&`/`&mut` lower).
    ///
    /// Pointer/reference locals are scalar **cells**: when address-taken for an
    /// out-param or `&x` the borrowed address is the cell itself, so `Ptr`,
    /// `Ref` and `RefMut` locals also need a home (`is_memory` is only set when
    /// a real cell address is wanted, never for BC.4a `Deref` borrows).
    /// Aggregate locals keep their object address in SSA (identity borrow), so
    /// they need no cell. `Str` has its own dedicated two-slot home.
    pub(crate) fn needs_scalar_stack_home(ty: &ArType) -> bool {
        match ty {
            ArType::Primitive(Primitive::Str) => false,
            ArType::Primitive(_) | ArType::IntLiteral | ArType::FloatLiteral => true,
            ArType::Ptr(_) => true,
            _ => false,
        }
    }

    pub(crate) fn get_temp_clif_type(&self, temp_id: TempId) -> Option<Type> {
        self.current_func
            .temps
            .get(temp_id.as_usize())
            .and_then(|t| {
                let ty = self.resolve_ty(t.ty);
                match clif_type(&ty, self.ptr_type) {
                    ClifType::Concrete(ty) => Some(ty),
                    ClifType::Void => None,
                }
            })
    }

    pub(crate) fn get_operand_clif_type(&self, operand: &AmirOperand) -> Option<Type> {
        match operand {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                self.get_temp_clif_type(*temp_id)
            }
            _ => match clif_type(&self.get_operand_ar_type(operand), self.ptr_type) {
                ClifType::Concrete(ty) => Some(ty),
                ClifType::Void => None,
            },
        }
    }

    pub(crate) fn func_span(&self) -> Span {
        self.symbol_table.get(self.current_func.symbol).span
    }

    pub(crate) fn record_ice(&mut self, message: impl Into<String>, span: Span) {
        if self.error.is_none() {
            self.error = Some(Diagnostic::ice(DiagCode::ICEGEN001, message, span));
        }
    }

    pub(crate) fn record_error(&mut self, code: DiagCode, message: impl Into<String>, span: Span) {
        if self.error.is_none() {
            self.error = Some(Diagnostic::error(code, message, span));
        }
    }

    pub(crate) fn checked_layout(&mut self, ty: &ArType) -> arandu_semantics::layout::TypeLayout {
        let pointer_width = self.ptr_type.bytes() as u64;
        let engine = arandu_semantics::layout::LayoutEngine::new(pointer_width);
        match engine.layout_of_type(ty, &self.type_info.type_interner, self.type_info) {
            Ok(layout)
                if layout.size <= i32::MAX as u64
                    && layout
                        .field_offsets
                        .iter()
                        .all(|offset| *offset <= i32::MAX as u64) =>
            {
                layout
            }
            Ok(layout) => {
                self.record_ice(
                    format!(
                        "type layout exceeds Cranelift's signed 32-bit offset limit: size={}, offsets={:?}",
                        layout.size, layout.field_offsets
                    ),
                    self.func_span(),
                );
                arandu_semantics::layout::TypeLayout::simple(0, 1)
            }
            Err(error) => {
                self.record_ice(
                    format!("Cranelift rejected an invalid type layout: {error}"),
                    self.func_span(),
                );
                arandu_semantics::layout::TypeLayout::simple(0, 1)
            }
        }
    }

    pub(crate) fn classify_arg_abi(&mut self, ty: &ArType) -> arandu_semantics::layout::ArgAbi {
        let triple = self.module.isa().triple();
        let target_abi = crate::abi::target_abi_for_triple(triple);
        let pointer_width = self.ptr_type.bytes() as u64;
        let classifier =
            arandu_semantics::layout::TargetAbiClassifier::new(target_abi, pointer_width);
        classifier.classify_type(ty, &self.type_info.type_interner, self.type_info)
    }

    pub(crate) fn poison_i32(&mut self) -> Value {
        self.builder.ins().iconst(self.ptr_type, 0)
    }

    pub(crate) fn poison_value(&mut self, clif_ty: cranelift_codegen::ir::Type) -> Value {
        if clif_ty.is_int() {
            self.builder.ins().iconst(clif_ty, 0)
        } else if clif_ty == cranelift_codegen::ir::types::F32 {
            self.builder.ins().f32const(0.0)
        } else if clif_ty == cranelift_codegen::ir::types::F64 {
            self.builder.ins().f64const(0.0)
        } else {
            self.builder.ins().iconst(clif_ty, 0)
        }
    }

    pub(crate) fn temp_span(&self, temp_id: TempId) -> Span {
        self.current_func
            .temps
            .get(temp_id.as_usize())
            .map(|temp| temp.span)
            .unwrap_or_else(|| self.func_span())
    }

    pub(crate) fn local_span(&self, local_id: LocalId) -> Span {
        self.current_func
            .locals
            .get(local_id.as_usize())
            .map(|local| local.span)
            .unwrap_or_else(|| self.func_span())
    }

    #[inline]
    pub(crate) fn resolve_ty(&self, id: arandu_semantics::types::TypeId) -> ArType {
        self.type_info.type_interner.resolve(id)
    }

    #[inline]
    pub(crate) fn temp_ar_ty(&self, temp_id: TempId) -> ArType {
        self.resolve_ty(self.current_func.temps[temp_id.as_usize()].ty)
    }

    #[inline]
    pub(crate) fn local_ar_ty(&self, local_id: LocalId) -> ArType {
        self.resolve_ty(self.current_func.locals[local_id.as_usize()].ty)
    }

    pub(crate) fn get_operand_ar_type(&self, op: &AmirOperand) -> ArType {
        match op {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => self.temp_ar_ty(*temp_id),
            AmirOperand::Constant(c) => match c {
                AmirConstant::Bool(_) => ArType::Primitive(Primitive::Bool),
                AmirConstant::Nil => ArType::Void,
                AmirConstant::Pool(lit_id) => match self.literal_pool.get(*lit_id) {
                    arandu_semantics::literal_pool::AmirLiteralEntry::Int(_) => ArType::IntLiteral,
                    arandu_semantics::literal_pool::AmirLiteralEntry::Float(_) => {
                        ArType::FloatLiteral
                    }
                    arandu_semantics::literal_pool::AmirLiteralEntry::Str(_) => {
                        ArType::Primitive(Primitive::Str)
                    }
                    arandu_semantics::literal_pool::AmirLiteralEntry::Char(_) => {
                        ArType::Primitive(Primitive::Char)
                    }
                },
            },
            AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => {
                if let AmirOperand::GlobalRef(symbol) = op
                    && let Some(ty) = self.type_info.decl_type(*symbol)
                {
                    return ty;
                }
                ArType::Error
            }
        }
    }

    #[tracing::instrument(level = "trace", target = "arandu_backend_cranelift", skip(self))]

    pub fn translate(&mut self) -> Result<(), Diagnostic> {
        self.set_debug_span(self.func_span());
        for (idx, _block) in self.current_func.blocks.iter().enumerate() {
            let block_id = BlockId::from_usize(idx);
            let clif_block = self.builder.create_block();
            self.block_map.insert(block_id, clif_block);
        }

        for (idx, block) in self.current_func.blocks.iter().enumerate() {
            let block_id = BlockId::from_usize(idx);
            let clif_block = self.block_map[&block_id];
            if block_id.as_usize() > 0 {
                for param in self.current_func.block_params(block.params) {
                    let pty = self.resolve_ty(param.ty);
                    match self.fat_operand_kind(&pty) {
                        operand::FatOperandKind::Str | operand::FatOperandKind::Slice => {
                            self.builder.append_block_param(clif_block, self.ptr_type);
                            self.builder.append_block_param(clif_block, self.ptr_type);
                        }
                        operand::FatOperandKind::None => {
                            for &clif_ty in &clif_types(&pty, self.ptr_type) {
                                self.builder.append_block_param(clif_block, clif_ty);
                            }
                        }
                    }
                }
            }
        }

        let entry_clif = self.block_map[&BlockId::from_usize(0)];
        self.builder
            .append_block_params_for_function_params(entry_clif);

        for local in &self.current_func.locals {
            let lty = self.resolve_ty(local.ty);
            if matches!(lty, ArType::Primitive(Primitive::Str)) {
                let var_ptr = self.builder.declare_var(self.ptr_type);
                let var_len = self.builder.declare_var(self.ptr_type);
                self.str_local_map.insert(local.id, (var_ptr, var_len));
                if local.is_memory {
                    let size = 2 * self.ptr_type.bytes();
                    let align_shift = self.ptr_type.bytes().trailing_zeros() as u8;
                    let slot = self.builder.create_sized_stack_slot(
                        cranelift_codegen::ir::StackSlotData {
                            kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                            size,
                            align_shift,
                            key: None,
                        },
                    );
                    self.local_stack_slots.insert(local.id, slot);
                }
            } else if let ClifType::Concrete(clif_ty) = clif_type(&lty, self.ptr_type) {
                let var = self.builder.declare_var(clif_ty);
                self.local_map.insert(local.id, var);
                // F2.0: address-taken scalars get a real stack slot so `&x` is valid.
                if local.is_memory && Self::needs_scalar_stack_home(&lty) {
                    let layout = self.checked_layout(&lty);
                    let size = u32::try_from(layout.size.max(1)).unwrap_or_else(|_| {
                        self.record_ice(
                            "type layout exceeds Cranelift's u32 stack-slot limit",
                            local.span,
                        );
                        1
                    });
                    let align = layout.align.max(1);
                    let align_shift = align.trailing_zeros() as u8;
                    let slot = self.builder.create_sized_stack_slot(
                        cranelift_codegen::ir::StackSlotData {
                            kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                            size,
                            align_shift,
                            key: None,
                        },
                    );
                    self.local_stack_slots.insert(local.id, slot);
                }
            }
        }

        for temp in &self.current_func.temps {
            let tty = self.resolve_ty(temp.ty);
            if matches!(tty, ArType::Primitive(Primitive::Str)) {
                let var_ptr = self.builder.declare_var(self.ptr_type);
                let var_len = self.builder.declare_var(self.ptr_type);
                self.str_temp_map.insert(temp.id, (var_ptr, var_len));
            } else if let ClifType::Concrete(clif_ty) = clif_type(&tty, self.ptr_type) {
                let var = self.builder.declare_var(clif_ty);
                self.temp_map.insert(temp.id, var);
            }
        }

        let rpo = arandu_semantics::amir::reverse_post_order(self.current_func);
        for &block_id in &rpo {
            let block = self.current_func.block(block_id);
            self.visit_block(block);
        }

        self.builder.seal_all_blocks();

        if let Some(error) = self.error.take() {
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn is_current_block_terminated(&self) -> bool {
        if let Some(block) = self.builder.current_block()
            && let Some(inst) = self.builder.func.layout.last_inst(block)
        {
            return self.builder.func.dfg.insts[inst].opcode().is_terminator();
        }
        false
    }
}

impl<'a, 'b, M: Module> AmirVisitor for FunctionTranslator<'a, 'b, M> {
    fn visit_block(&mut self, block: &AmirBasicBlock) {
        if self.error.is_some() {
            return;
        }
        let clif_block = self.block_map[&block.id];
        self.builder.switch_to_block(clif_block);
        if let Some((coverage_func, function_index, session_id)) = self.block_coverage {
            let Ok(block_index) = i64::try_from(block.id.as_usize()) else {
                self.record_ice(
                    "AMIR block index exceeds the JIT coverage ABI",
                    self.func_span(),
                );
                return;
            };
            let func_ref = self
                .module
                .declare_func_in_func(coverage_func, self.builder.func);
            let session_id = self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, session_id);
            let function_index = self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, i64::from(function_index));
            let block_index = self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, block_index);
            self.builder
                .ins()
                .call(func_ref, &[session_id, function_index, block_index]);
        }

        if block.id.as_usize() == 0 {
            for local in &self.current_func.locals {
                let lty = self.resolve_ty(local.ty);
                if matches!(lty, ArType::Primitive(Primitive::Str)) {
                    let &(var_ptr, var_len) = &self.str_local_map[&local.id];
                    let zero_ptr = self.builder.ins().iconst(self.ptr_type, 0);
                    let zero_len = self.builder.ins().iconst(self.ptr_type, 0);
                    self.builder.def_var(var_ptr, zero_ptr);
                    self.builder.def_var(var_len, zero_len);
                    if let Some(&slot) = self.local_stack_slots.get(&local.id) {
                        let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            zero_ptr,
                            addr,
                            0,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            zero_len,
                            addr,
                            self.ptr_type.bytes() as i32,
                        );
                    }
                } else if let Some(&var) = self.local_map.get(&local.id) {
                    let Some(clif_ty) = clif_type(&lty, self.ptr_type).concrete() else {
                        continue;
                    };
                    let zero = if clif_ty == cranelift_codegen::ir::types::F32 {
                        self.builder.ins().f32const(0.0)
                    } else if clif_ty == cranelift_codegen::ir::types::F64 {
                        self.builder.ins().f64const(0.0)
                    } else {
                        self.builder.ins().iconst(clif_ty, 0)
                    };
                    self.builder.def_var(var, zero);
                }
            }

            let clif_params = self.builder.block_params(clif_block).to_vec();
            let mut clif_slot_idx = 0;
            for &param_temp_id in &self.current_func.params {
                let param_ty = self.temp_ar_ty(param_temp_id);
                if matches!(&param_ty, ArType::Primitive(Primitive::Str)) {
                    let ptr_val = clif_params[clif_slot_idx];
                    let len_val = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(&param_temp_id) {
                        self.builder.def_var(var_ptr, ptr_val);
                        self.builder.def_var(var_len, len_val);
                    }
                } else if matches!(
                    self.fat_operand_kind(&param_ty),
                    operand::FatOperandKind::Slice
                ) {
                    let data = clif_params[clif_slot_idx];
                    let len = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    let descriptor = self.materialize_slice_descriptor(data, len);
                    if let Some(&var) = self.temp_map.get(&param_temp_id) {
                        self.builder.def_var(var, descriptor);
                    }
                } else if matches!(&param_ty, ArType::Named(_, _) | ArType::Tuple(_)) {
                    let arg_abi = self.classify_arg_abi(&param_ty);
                    match arg_abi {
                        arandu_semantics::layout::ArgAbi::ZeroSized => {
                            let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
                            if let Some(&var) = self.temp_map.get(&param_temp_id) {
                                self.builder.def_var(var, null_ptr);
                            }
                        }
                        arandu_semantics::layout::ArgAbi::Direct(direct) => {
                            let layout = self.checked_layout(&param_ty);
                            let size = u32::try_from(layout.size.max(1)).unwrap_or(1);
                            let align_shift = layout.align.max(1).trailing_zeros() as u8;
                            let slot = self.builder.create_sized_stack_slot(
                                cranelift_codegen::ir::StackSlotData {
                                    kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                                    size,
                                    align_shift,
                                    key: None,
                                },
                            );
                            let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                            for abi_slot in &direct.slots {
                                let chunk_val = clif_params[clif_slot_idx];
                                clif_slot_idx += 1;
                                self.builder.ins().store(
                                    cranelift_codegen::ir::MemFlagsData::new(),
                                    chunk_val,
                                    addr,
                                    abi_slot.offset as i32,
                                );
                            }
                            if let Some(&var) = self.temp_map.get(&param_temp_id) {
                                self.builder.def_var(var, addr);
                            }
                        }
                        arandu_semantics::layout::ArgAbi::Indirect => {
                            let ptr_val = clif_params[clif_slot_idx];
                            clif_slot_idx += 1;
                            if let Some(&var) = self.temp_map.get(&param_temp_id) {
                                self.builder.def_var(var, ptr_val);
                            }
                        }
                    }
                } else if let ClifType::Concrete(_) = clif_type(&param_ty, self.ptr_type) {
                    let val = clif_params[clif_slot_idx];
                    clif_slot_idx += 1;
                    if let Some(&var) = self.temp_map.get(&param_temp_id) {
                        self.builder.def_var(var, val);
                    }
                }
                if let Some(var) = self.temp_map.get(&param_temp_id).copied() {
                    let value = self.builder.use_var(var);
                    self.label_temp_value(param_temp_id, value);
                }
            }
        } else {
            let clif_params = self.builder.block_params(clif_block).to_vec();
            let mut clif_slot_idx = 0;
            for param in self.current_func.block_params(block.params) {
                let pty = self.resolve_ty(param.ty);
                if matches!(pty, ArType::Primitive(Primitive::Str)) {
                    let ptr_val = clif_params[clif_slot_idx];
                    let len_val = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(&param.id) {
                        self.builder.def_var(var_ptr, ptr_val);
                        self.builder.def_var(var_len, len_val);
                    }
                } else if matches!(self.fat_operand_kind(&pty), operand::FatOperandKind::Slice) {
                    let data = clif_params[clif_slot_idx];
                    let len = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    let descriptor = self.materialize_slice_descriptor(data, len);
                    if let Some(&var) = self.temp_map.get(&param.id) {
                        self.builder.def_var(var, descriptor);
                    }
                } else if let ClifType::Concrete(_) = clif_type(&pty, self.ptr_type) {
                    let val = clif_params[clif_slot_idx];
                    clif_slot_idx += 1;
                    if let Some(&var) = self.temp_map.get(&param.id) {
                        self.builder.def_var(var, val);
                    }
                }
                if let Some(var) = self.temp_map.get(&param.id).copied() {
                    let value = self.builder.use_var(var);
                    self.label_temp_value(param.id, value);
                }
            }
        }

        for stmt_id in block.statements.iter_ids::<InstrId>() {
            let stmt = self.current_func.stmt(stmt_id);
            self.visit_stmt(stmt);
            if self.is_current_block_terminated() {
                break;
            }
        }

        if !self.is_current_block_terminated() {
            self.visit_terminator(&block.terminator);
        }
    }

    fn visit_stmt(&mut self, stmt: &AmirStmt) {
        self.set_debug_span(self.stmt_span(stmt));
        self.translate_stmt(stmt);
    }

    fn visit_terminator(&mut self, term: &AmirTerminator) {
        self.translate_terminator(term);
    }
}
