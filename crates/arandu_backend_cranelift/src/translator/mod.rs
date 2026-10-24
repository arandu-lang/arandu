//! AMIR → Cranelift IR function translator.
//!
//! [`FunctionTranslator`] walks an [`AmirFunc`] basic block by block,
//! emitting Cranelift IR instructions via a [`FunctionBuilder`]. The
//! [`AmirVisitor`] trait defines the visit callbacks used during traversal.

mod atomic;
mod call;
mod compare;
mod constant_data;
mod coroutine;
mod expr;
mod memory;
mod operand;
mod place;
mod stmt;
mod storage;
mod string;
mod terminator;
mod translate;

use arandu_base::span::Span;
use arandu_middle::amir::{
    AmirBasicBlock, AmirConstant, AmirDebugBinding, AmirFunc, AmirOperand, AmirStmt,
    AmirTerminator, BlockId, InstrId, LocalId, TempId,
};
use arandu_middle::types::{ArType, Primitive};
use arandu_middle::{DiagCode, Diagnostic, SymbolTable};
use cranelift_codegen::ir::{
    Block, InstBuilder, RelSourceLoc, SourceLoc, StackSlot, Type, Value, ValueLabel,
    ValueLabelAssignments, ValueLabelStart,
};
use cranelift_frontend::{FunctionBuilder, Variable};
use cranelift_module::{FuncId, Module};
use rustc_hash::FxHashMap;

use crate::types::{ClifType, clif_type};

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
    runtime_imports: &'b crate::jit::symbols::RuntimeImports,
    pub(crate) gen_shutdown: bool,
    pub block_map: FxHashMap<BlockId, Block>,
    pub temp_map: FxHashMap<TempId, Variable>,
    pub local_map: FxHashMap<LocalId, Variable>,
    /// Stack homes for address-taken scalar locals (F2.0 `&`/`&mut`).
    /// Aggregate/heap locals keep their address in `local_map` (pointer SSA).
    pub local_stack_slots: FxHashMap<LocalId, StackSlot>,
    pub(crate) integer_concat_temps: Vec<Option<arandu_codegen::string_interp::IntegerStringKind>>,
    pub(crate) integer_concat_values: FxHashMap<TempId, (Value, Value, &'static str)>,
    pub str_temp_map: FxHashMap<TempId, (Variable, Variable)>,
    pub str_local_map: FxHashMap<LocalId, (Variable, Variable)>,
    pub ptr_type: Type,
    /// Hidden destination owned by the caller, never a callee-frame address.
    pub(crate) indirect_return_destination: Option<Value>,
    /// Destination-passing for the current top-level result construction.
    pub(crate) aggregate_destination: Option<Value>,
    pub(crate) frame_promotion_safe: bool,
    /// Lazily allocated Copy scratch buffers, owned by this invocation only.
    pub(crate) heap_scratch_slots: Vec<StackSlot>,
    pub(crate) static_initializers: FxHashMap<InstrId, arandu_mir::static_data::StaticInitializer>,
    pub(crate) current_initializer: Option<InstrId>,
    pub literal_pool: &'b arandu_middle::literal_pool::AmirLiteralPool,
    pub current_func: &'b AmirFunc,
    pub type_info: &'b dyn arandu_middle::type_context::CodegenTypeContext,
    block_coverage: Option<(FuncId, u32, i64)>,
    pub(crate) debug_locations: Option<&'b mut Vec<Span>>,
    debug_temp_locals: Option<FxHashMap<TempId, Vec<LocalId>>>,
    current_debug_location: Option<u32>,
    pub(crate) error: Option<Diagnostic>,
}

impl<'a, 'b, M: Module> FunctionTranslator<'a, 'b, M> {
    /// Declare host helpers at their actual use, rather than retaining every
    /// runtime archive member through unused undefined object symbols.
    pub(super) fn resolve_function(&mut self, name: &str) -> Option<FuncId> {
        if let Some(&id) = self.func_ids.get(name) {
            return Some(id);
        }
        match self.runtime_imports.resolve(self.module, name) {
            Ok(id) => id,
            Err(error) => {
                self.error.get_or_insert(error);
                None
            }
        }
    }

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
        runtime_imports: &'b crate::jit::symbols::RuntimeImports,
        gen_shutdown: bool,
        ptr_type: Type,
        literal_pool: &'b arandu_middle::literal_pool::AmirLiteralPool,
        current_func: &'b AmirFunc,
        type_info: &'b dyn arandu_middle::type_context::CodegenTypeContext,
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
        let little_endian = module.isa().endianness() == cranelift_codegen::ir::Endianness::Little;
        Self {
            builder,
            module,
            symbol_table,
            func_ids,
            runtime_imports,
            gen_shutdown,
            block_map: FxHashMap::default(),
            temp_map: FxHashMap::default(),
            local_map: FxHashMap::default(),
            local_stack_slots: FxHashMap::default(),
            integer_concat_temps: arandu_codegen::string_interp::integer_concat_temps(
                current_func,
                type_info.type_interner(),
            ),
            integer_concat_values: FxHashMap::default(),
            str_temp_map: FxHashMap::default(),
            str_local_map: FxHashMap::default(),
            ptr_type,
            indirect_return_destination: None,
            aggregate_destination: None,
            frame_promotion_safe: storage::frame_promotion_safe(
                current_func,
                symbol_table,
                type_info,
            ),
            heap_scratch_slots: Vec::new(),
            static_initializers: arandu_mir::static_data::static_initializers(
                current_func,
                type_info.type_interner(),
                type_info,
                literal_pool,
                arandu_middle::layout::DataLayout::ptr_width(u64::from(ptr_type.bytes())),
                little_endian,
            ),
            current_initializer: None,
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
            ArType::Vector(_)
            | ArType::Primitive(_)
            | ArType::IntLiteral
            | ArType::FloatLiteral => true,
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

    pub(crate) fn checked_layout(&mut self, ty: &ArType) -> arandu_middle::layout::TypeLayout {
        let pointer_width = self.ptr_type.bytes() as u64;
        let engine = arandu_middle::layout::LayoutEngine::new(pointer_width);
        match engine.layout_of_type(ty, self.type_info.type_interner(), self.type_info) {
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
                arandu_middle::layout::TypeLayout::simple(0, 1)
            }
            Err(error) => {
                self.record_ice(
                    format!(
                        "Cranelift rejected the layout of '{}': {error}",
                        ty.display(self.symbol_table, self.type_info.type_interner())
                    ),
                    self.func_span(),
                );
                arandu_middle::layout::TypeLayout::simple(0, 1)
            }
        }
    }

    pub(crate) fn classify_arg_abi(&mut self, ty: &ArType) -> arandu_middle::layout::ArgAbi {
        let triple = self.module.isa().triple();
        let target_abi = crate::abi::target_abi_for_triple(triple);
        let pointer_width = self.ptr_type.bytes() as u64;
        let classifier = arandu_middle::layout::TargetAbiClassifier::new(target_abi, pointer_width);
        classifier.classify_type(ty, self.type_info.type_interner(), self.type_info)
    }

    pub(crate) fn poison_i32(&mut self) -> Value {
        self.builder
            .ins()
            .iconst(cranelift_codegen::ir::types::I32, 0)
    }

    pub(crate) fn poison_value(&mut self, clif_ty: cranelift_codegen::ir::Type) -> Value {
        if clif_ty.is_vector() {
            let lane = self.poison_value(clif_ty.lane_type());
            self.builder.ins().splat(clif_ty, lane)
        } else if clif_ty.is_int() {
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
    pub(crate) fn resolve_ty(&self, id: arandu_middle::types::TypeId) -> ArType {
        self.type_info.type_interner().resolve(id)
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
                    arandu_middle::literal_pool::AmirLiteralEntry::Int(_) => ArType::IntLiteral,
                    arandu_middle::literal_pool::AmirLiteralEntry::Float(_) => ArType::FloatLiteral,
                    arandu_middle::literal_pool::AmirLiteralEntry::FloatBits(value) => {
                        ArType::Primitive(value.ty().primitive())
                    }
                    arandu_middle::literal_pool::AmirLiteralEntry::Str(_) => {
                        ArType::Primitive(Primitive::Str)
                    }
                    arandu_middle::literal_pool::AmirLiteralEntry::Char(_) => {
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

    pub(crate) fn is_current_block_terminated(&self) -> bool {
        if let Some(block) = self.builder.current_block()
            && let Some(inst) = self.builder.func.layout.last_inst(block)
        {
            return self.builder.func.dfg.insts[inst].opcode().is_terminator();
        }
        false
    }
}
