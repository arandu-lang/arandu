//! Direct lowering from AMIR to textual LLVM IR.
//!
//! This module deliberately keeps LLVM behind a text boundary: the backend is
//! pure Rust and never links `llvm-sys`. The generated module can be validated
//! with `llvm-as`. Execution links through `clang` and the packaged runtime;
//! `lli` does not resolve Arandu runtime symbols, so the CLI refuses it.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt::Write as _;

use arandu_middle::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirProgram, AmirStmt, BlockId, LocalId, TempId,
};
use arandu_middle::intrinsics::IntrinsicKind;
use arandu_middle::layout::LayoutEngine;
use arandu_middle::types::{ArType, Primitive, TypeInterner, VectorType};
use arandu_middle::{DiagCode, Diagnostic, Span, SymbolId, SymbolKind, SymbolTable};
use arandu_semantics::TypeInfo;

use crate::types::{fat_ptr_llvm_ty, llvm_type, ptr_int};
use crate::writer::IrWriter;

mod call;
mod rvalue;
mod stmt;
mod term;
mod value;

/// Emit one textual LLVM module for a validated AMIR program.
pub fn emit_llvm_ir(
    program: &AmirProgram,
    symbols: &SymbolTable,
    info: &TypeInfo,
) -> Result<String, Diagnostic> {
    let diagnostics = arandu_middle::validate_amir_program(program, symbols, &info.type_interner);
    if let Some(error) = diagnostics.into_iter().next() {
        return Err(error);
    }
    ModuleEmitter::new(program, symbols, info).emit()
}

pub(super) fn ice(message: impl Into<String>) -> Diagnostic {
    Diagnostic::ice(DiagCode::ICEGEN001, message, Span::new(0, 0, 0))
}

pub(super) fn temp_name(temp: TempId) -> String {
    format!("%t{}", temp.as_usize())
}

pub(super) fn local_name(local: LocalId) -> String {
    format!("%s{}", local.as_usize())
}

pub(super) fn block_label(block: BlockId) -> String {
    format!("bb{}", block.as_usize())
}

pub(super) struct ModuleEmitter<'a> {
    program: &'a AmirProgram,
    symbols: &'a SymbolTable,
    info: &'a TypeInfo,
    interner: &'a TypeInterner,
    layout: LayoutEngine,
    string_globals: HashMap<u32, (String, usize)>,
    globals: Vec<String>,
    declarations: BTreeMap<String, String>,
}

impl<'a> ModuleEmitter<'a> {
    fn new(program: &'a AmirProgram, symbols: &'a SymbolTable, info: &'a TypeInfo) -> Self {
        let mut string_globals = HashMap::new();
        let mut globals = Vec::new();
        for (index, entry) in program.literal_pool.entries.iter().enumerate() {
            if let arandu_middle::literal_pool::AmirLiteralEntry::Str(value) = entry {
                let name = format!("@.str.{index}");
                let length = value.len();
                globals.push(format!(
                    "{name} = private unnamed_addr constant [{} x i8] c\"{}\\00\", align 1",
                    length + 1,
                    escape_c_string(value.as_bytes())
                ));
                string_globals.insert(index as u32, (name, length));
            }
        }

        let mut this = Self {
            program,
            symbols,
            info,
            interner: &info.type_interner,
            layout: LayoutEngine::from_data_layout(info.target_layout),
            string_globals,
            globals,
            declarations: BTreeMap::new(),
        };
        this.collect_declarations();
        this
    }

    fn collect_declarations(&mut self) {
        // LLVM intrinsics and libc helpers are cheap to declare even when a
        // particular module does not call them.
        self.add_declaration("llvm.trap", "declare void @llvm.trap()");
        // Match Cranelift: owned heap buffers use the runtime allocator pair,
        // never libc malloc/free (ToStr / str concat / Free share this ABI).
        let pointer_bytes = self.layout.pointer_width();
        let len = ptr_int(pointer_bytes);
        let fat = fat_ptr_llvm_ty(pointer_bytes);
        let memcpy = self.memcpy_intrinsic();
        self.add_declaration("ar_rt_raw_free", "declare void @ar_rt_raw_free(ptr)");
        self.add_declaration(
            "ar_rt_raw_malloc",
            &format!("declare ptr @ar_rt_raw_malloc({len})"),
        );
        self.add_declaration("memcmp", &format!("declare i32 @memcmp(ptr, ptr, {len})"));
        self.add_declaration(
            "ar_str_concat",
            &format!("declare {fat} @ar_str_concat(ptr, {len}, ptr, {len})"),
        );
        self.add_declaration(
            memcpy,
            &format!("declare void @{memcpy}(ptr, ptr, {len}, i1)"),
        );
        for (name, ty) in [
            ("ar_jit_i64_to_str", "i64"),
            ("ar_jit_u64_to_str", "i64"),
            ("ar_jit_f64_to_str", "double"),
            ("ar_jit_bool_to_str", "i8"),
            ("ar_jit_char_to_str", "i32"),
        ] {
            self.add_declaration(name, &format!("declare ptr @{name}({ty}, ptr)"));
        }
        self.add_declaration(
            "ar_jit_err_to_str",
            "declare ptr @ar_jit_err_to_str(ptr, ptr)",
        );
        self.add_declaration(
            "ar_co_make_ready_i64",
            "declare ptr @ar_co_make_ready_i64(i64)",
        );
        self.add_declaration("ar_co_poll_i64", "declare i32 @ar_co_poll_i64(ptr, ptr)");
        self.add_declaration("ar_co_block_on_i64", "declare i64 @ar_co_block_on_i64(ptr)");
        // Host executor bridges are always i64 regardless of the Arandu
        // `int` facade in `std.runtime.executor` (matches Cranelift).
        self.add_declaration("ar_rt_spawn_i64", "declare i64 @ar_rt_spawn_i64(ptr)");
        self.add_declaration("ar_rt_join_i64", "declare i64 @ar_rt_join_i64(i64)");
        self.add_declaration("ar_rt_block_on_i64", "declare i64 @ar_rt_block_on_i64(ptr)");
        self.add_declaration(
            "ar_gen_insert_raw",
            "declare i64 @ar_gen_insert_raw(ptr, i64, i64, ptr)",
        );
        self.add_declaration(
            "ar_gen_get_raw",
            "declare i1 @ar_gen_get_raw(i64, ptr, i64, i64)",
        );
        self.add_declaration(
            "ar_gen_set_raw",
            "declare i1 @ar_gen_set_raw(i64, ptr, i64, i64, ptr)",
        );
        self.add_declaration(
            "ar_gen_upsert_raw",
            "declare i64 @ar_gen_upsert_raw(i64, ptr, i64, i64, ptr)",
        );
        self.add_declaration(
            "ar_gen_remove_raw",
            "declare i1 @ar_gen_remove_raw(i64, ptr, i64, i64)",
        );
        for width in [8, 16, 32, 64] {
            self.add_declaration(
                &format!("llvm.ctpop.i{width}"),
                &format!("declare i{width} @llvm.ctpop.i{width}(i{width})"),
            );
            self.add_declaration(
                &format!("llvm.ctlz.i{width}"),
                &format!("declare i{width} @llvm.ctlz.i{width}(i{width}, i1)"),
            );
            self.add_declaration(
                &format!("llvm.cttz.i{width}"),
                &format!("declare i{width} @llvm.cttz.i{width}(i{width}, i1)"),
            );
            self.add_declaration(
                &format!("llvm.bitreverse.i{width}"),
                &format!("declare i{width} @llvm.bitreverse.i{width}(i{width})"),
            );
        }

        let mut signatures = Vec::new();
        for func in &self.program.funcs {
            for block in &func.blocks {
                for id in func.block_stmt_ids(block.id) {
                    if let AmirStmt::Call {
                        callee: AmirOperand::FunctionRef(symbol),
                        args,
                        ..
                    } = func.stmt(id)
                        && self.intrinsic_kind(*symbol).is_none()
                        && !self.is_internal(*symbol)
                    {
                        let (args, ret) = self.signature_for(*symbol, args, func);
                        signatures.push((*symbol, args, ret));
                    }
                }
            }
        }
        for (symbol, args, ret) in signatures {
            let name = self.function_name(symbol);
            if self.declarations.contains_key(&name) {
                // Prefer the explicit host ABI declaration when present.
                continue;
            }
            // Host/C ABI flattens fat `str`/slice to separate ptr+len slots,
            // matching Cranelift and the runtime's `extern "C"` surface.
            let mut flattened = Vec::new();
            let mut ok = true;
            for arg in &args {
                match self.host_abi_param_llvm_tys(arg) {
                    Ok(parts) => flattened.extend(parts),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            let parsed_ret = self.llvm_ty(&ret);
            if ok && let Ok(ret) = parsed_ret {
                let params = flattened.join(", ");
                self.add_declaration(
                    &name,
                    &format!("declare {ret} @{}({params})", quote_name(&name)),
                );
            }
        }
    }

    fn add_declaration(&mut self, key: &str, text: &str) {
        self.declarations
            .entry(key.to_owned())
            .or_insert_with(|| text.to_owned());
    }

    fn emit(self) -> Result<String, Diagnostic> {
        let mut body = String::new();
        for func in &self.program.funcs {
            let function = FuncEmitter::new(&self, func)?;
            body.push_str(&function.emit()?);
            body.push('\n');
        }

        let mut output = String::new();
        output.push_str("; ModuleID = 'arandu'\n");
        output.push_str("source_filename = \"arandu\"\n");
        if let Some(profile) = arandu_middle::embedded_profile(&self.info.target_identity.triple) {
            let _ = write!(
                output,
                "target triple = \"{}\"\ntarget datalayout = \"{}\"\n",
                profile.triple(),
                profile.llvm_datalayout()
            );
        }
        output.push('\n');
        for global in &self.globals {
            output.push_str(global);
            output.push('\n');
        }
        if !self.globals.is_empty() {
            output.push('\n');
        }
        for declaration in self.declarations.values() {
            output.push_str(declaration);
            output.push('\n');
        }
        output.push('\n');
        output.push_str(&body);
        Ok(output)
    }

    fn function_name(&self, symbol: SymbolId) -> String {
        self.symbols.try_get(symbol).map_or_else(
            || format!("sym{}", symbol.local_id.0),
            |symbol| self.symbols.host_func_name(symbol).to_owned(),
        )
    }

    fn is_internal(&self, symbol: SymbolId) -> bool {
        self.program.funcs.iter().any(|func| func.symbol == symbol)
    }

    fn intrinsic_kind(&self, symbol: SymbolId) -> Option<IntrinsicKind> {
        let symbol = self.symbols.try_get(symbol)?;
        if symbol.kind != SymbolKind::ExternFunc {
            return None;
        }
        IntrinsicKind::from_declared_name(&symbol.name, symbol.lang_item)
    }

    fn signature_for(
        &self,
        symbol: SymbolId,
        call_args: &[AmirOperand],
        caller: &AmirFunc,
    ) -> (Vec<ArType>, ArType) {
        let name = self.function_name(symbol);
        if let Some(signature) = self.host_bridge_signature(&name) {
            return signature;
        }
        if let Some((args, ret)) = self.program.extern_funcs.get(&symbol) {
            return (args.clone(), ret.clone());
        }
        if let Some(ty) = self.info.decl_types.get(&symbol)
            && let ArType::Func(args, ret) = self.interner.resolve(*ty)
        {
            let args = self
                .interner
                .type_args(args)
                .iter()
                .map(|id| self.interner.resolve(*id))
                .collect();
            return (args, self.interner.resolve(ret));
        }
        if let Some(func) = self.program.funcs.iter().find(|func| func.symbol == symbol) {
            let args = func
                .params
                .iter()
                .map(|temp| self.interner.resolve(func.temps[temp.as_usize()].ty))
                .collect();
            return (args, self.interner.resolve(func.return_type));
        }
        let args = call_args
            .iter()
            .map(|arg| self.operand_type(arg, caller))
            .collect();
        (args, ArType::Void)
    }

    /// Fixed host ABIs that must not follow the Arandu `int` facade.
    fn host_bridge_signature(&self, name: &str) -> Option<(Vec<ArType>, ArType)> {
        let void_ptr = self.void_ptr_ty();
        let i64_ty = ArType::Primitive(Primitive::I64);
        match name {
            "ar_rt_spawn_i64" => Some((vec![void_ptr], i64_ty)),
            "ar_rt_join_i64" => Some((vec![i64_ty.clone()], i64_ty)),
            "ar_rt_block_on_i64" | "ar_co_block_on_i64" => Some((vec![void_ptr], i64_ty)),
            "ar_co_make_ready_i64" => Some((vec![i64_ty], void_ptr)),
            _ => None,
        }
    }

    fn operand_type(&self, operand: &AmirOperand, func: &AmirFunc) -> ArType {
        match operand {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                self.interner.resolve(func.temps[temp.as_usize()].ty)
            }
            AmirOperand::Constant(constant) => self.constant_type(constant),
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => {
                if let Some(ty) = self.info.decl_types.get(symbol) {
                    let ty = self.interner.resolve(*ty);
                    if matches!(ty, ArType::Func(..)) {
                        return ty;
                    }
                }
                ArType::Ptr(self.interner.intern(ArType::Void))
            }
        }
    }

    fn constant_type(&self, constant: &AmirConstant) -> ArType {
        match constant {
            AmirConstant::Pool(id) => match self.program.literal_pool.get(*id) {
                arandu_middle::literal_pool::AmirLiteralEntry::Int(_) => {
                    ArType::Primitive(Primitive::Int)
                }
                arandu_middle::literal_pool::AmirLiteralEntry::Float(_) => {
                    ArType::Primitive(Primitive::Float)
                }
                arandu_middle::literal_pool::AmirLiteralEntry::FloatBits(value) => {
                    ArType::Primitive(match value.ty().bit_width() {
                        32 => Primitive::F32,
                        _ => Primitive::F64,
                    })
                }
                arandu_middle::literal_pool::AmirLiteralEntry::Str(_) => {
                    ArType::Primitive(Primitive::Str)
                }
                arandu_middle::literal_pool::AmirLiteralEntry::Char(_) => {
                    ArType::Primitive(Primitive::Char)
                }
            },
            AmirConstant::Bool(_) => ArType::Primitive(Primitive::Bool),
            AmirConstant::Nil => ArType::Ptr(self.interner.intern(ArType::Void)),
        }
    }

    fn llvm_ty(&self, ty: &ArType) -> Result<String, Diagnostic> {
        if self.is_byte_aggregate(ty) {
            return self.aggregate_ty(ty);
        }
        llvm_type(ty, self.interner, self.layout.pointer_width()).map_err(ice)
    }

    /// Aggregates use the target layout as an opaque byte array. Their AMIR
    /// values are still first-class LLVM values, but fields are addressed by
    /// byte offset instead of relying on LLVM's own struct layout rules.
    fn is_byte_aggregate(&self, ty: &ArType) -> bool {
        matches!(
            ty,
            ArType::Named(..)
                | ArType::Tuple(..)
                | ArType::Array(..)
                | ArType::Option(_)
                | ArType::Result(_, _)
                | ArType::Poll(_)
                | ArType::Range(_)
        )
    }

    fn aggregate_ty(&self, ty: &ArType) -> Result<String, Diagnostic> {
        let layout = self
            .layout
            .layout_of_type(ty, self.interner, self.info)
            .map_err(|error| ice(format!("LLVM layout failed for {ty:?}: {error}")))?;
        Ok(format!("[{} x i8]", layout.size.max(1)))
    }

    fn alloca_type(&self, ty: &ArType) -> Result<(String, u64), Diagnostic> {
        let layout = self
            .layout
            .layout_of_type(ty, self.interner, self.info)
            .map_err(|error| ice(format!("LLVM layout failed for {ty:?}: {error}")))?;
        let size = layout.size;
        let align = layout.align.max(1);
        if size == 0 {
            return Ok(("[1 x i8]".to_owned(), align));
        }
        let text = match self.llvm_ty(ty) {
            Ok(text) if text != "void" => text,
            _ => format!("[{size} x i8]"),
        };
        Ok((text, align))
    }

    fn align_of(&self, ty: &ArType) -> u64 {
        self.layout
            .layout_of_type(ty, self.interner, self.info)
            .map_or(1, |layout| layout.align.max(1))
    }

    fn pointer_align(&self) -> u64 {
        self.layout.data_layout.pointer_align().max(1)
    }

    /// LLVM integer for pointer-sized lengths and indices. Bytes, not bits.
    pub(super) fn ptr_int_ty(&self) -> &'static str {
        ptr_int(self.layout.pointer_width())
    }

    pub(super) fn fat_ptr_llvm_ty(&self) -> String {
        fat_ptr_llvm_ty(self.layout.pointer_width())
    }

    pub(super) fn memcpy_intrinsic(&self) -> &'static str {
        if self.layout.pointer_width() == 4 {
            "llvm.memcpy.p0.p0.i32"
        } else {
            "llvm.memcpy.p0.p0.i64"
        }
    }

    fn usize_ty(&self) -> ArType {
        ArType::Primitive(Primitive::USize)
    }

    fn i64_ty(&self) -> ArType {
        ArType::Primitive(Primitive::I64)
    }

    fn void_ptr_ty(&self) -> ArType {
        ArType::Ptr(self.interner.intern(ArType::Void))
    }

    fn is_signed(&self, ty: &ArType) -> bool {
        match ty {
            ArType::Primitive(primitive) => primitive.is_signed(),
            ArType::Vector(vector) => vector.lane().is_signed(),
            _ => false,
        }
    }

    fn integer_width(&self, ty: &ArType) -> Option<u64> {
        match ty {
            // `llvm_type` represents an unconstrained integer literal as i32;
            // keep coercion's width model aligned with that ABI choice.
            ArType::IntLiteral => Some(32),
            ArType::Primitive(primitive) => Some(match primitive {
                Primitive::Bool => 1,
                Primitive::Int | Primitive::Uint | Primitive::I32 | Primitive::U32 => 32,
                // `DataLayout::pointer_width` is bytes; LLVM integer widths are bits.
                Primitive::ISize | Primitive::USize => self.layout.pointer_width() * 8,
                Primitive::I8 | Primitive::U8 | Primitive::Byte => 8,
                Primitive::I16 | Primitive::U16 => 16,
                Primitive::I64 | Primitive::U64 => 64,
                Primitive::Char => 32,
                _ => return None,
            }),
            _ => None,
        }
    }

    fn is_pointer(&self, ty: &ArType) -> bool {
        matches!(
            ty,
            ArType::Ptr(_)
                | ArType::Ref(_)
                | ArType::RefMut(_)
                | ArType::Nullable(_)
                | ArType::GenRef
                | ArType::Coroutine(_)
                | ArType::Err
                | ArType::Func(..)
        )
    }

    fn is_fat_pointer(&self, ty: &ArType) -> bool {
        matches!(
            ty,
            ArType::Primitive(Primitive::Str) | ArType::Slice(_) | ArType::MutSlice(_)
        )
    }

    /// LLVM types for one logical parameter at the host/C ABI boundary.
    ///
    /// Fat `str` and slices become two scalar slots (`ptr`, pointer integer),
    /// matching Cranelift and runtime helpers such as `ar_fs_read_all`.
    /// Void parameters contribute no slots. Internal Arandu→Arandu calls keep
    /// the first-class fat value ABI from [`Self::fat_ptr_llvm_ty`].
    fn host_abi_param_llvm_tys(&self, ty: &ArType) -> Result<Vec<String>, Diagnostic> {
        if self.is_void(ty) {
            return Ok(Vec::new());
        }
        if self.is_fat_pointer(ty) {
            return Ok(vec!["ptr".to_owned(), self.ptr_int_ty().to_owned()]);
        }
        Ok(vec![self.llvm_ty(ty)?])
    }

    fn is_float(&self, ty: &ArType) -> bool {
        matches!(ty, ArType::Primitive(p) if p.is_float())
    }

    fn is_void(&self, ty: &ArType) -> bool {
        matches!(ty, ArType::Void)
    }

    fn vector_info(&self, ty: &ArType) -> Option<(VectorType, u64, Primitive)> {
        let ArType::Vector(vector) = ty else {
            return None;
        };
        let lanes = match vector {
            VectorType::F32x4 => 4,
            VectorType::F64x2 => 2,
            VectorType::I32x4 => 4,
            VectorType::I16x8 => 8,
            VectorType::U8x16 => 16,
            VectorType::I64x2 => 2,
        };
        Some((*vector, lanes, vector.lane()))
    }
}

pub(super) struct FuncEmitter<'a, 'm> {
    module: &'m ModuleEmitter<'a>,
    func: &'a AmirFunc,
    writer: IrWriter,
    defined: Vec<Option<&'static str>>,
    temp_slots: Vec<Option<String>>,
    /// LLVM type actually written for a temp when it differs from the AMIR view
    /// (full enum-variant payloads that AMIR projects thinly).
    temp_llvm_ty: Vec<Option<String>>,
    next_value: usize,
    current_return: Option<String>,
    return_counter: usize,
    /// Block currently being lowered (for edge-arg materialization).
    current_block: BlockId,
    /// LLVM label of the basic block currently being written. AMIR blocks may
    /// be split by helper `locN` labels (fat-pointer compares, traps, …); PHI
    /// predecessors must name the label that actually branches.
    current_llvm_label: String,
    /// AMIR block → LLVM label containing that block's terminator.
    terminator_labels: HashMap<BlockId, String>,
    /// Values passed across CFG edges, materialized in the predecessor so
    /// successor PHI nodes never insert instructions before/between phis.
    edge_args: HashMap<(BlockId, BlockId), Vec<TypedValue>>,
}

impl<'a, 'm> FuncEmitter<'a, 'm> {
    fn new(module: &'m ModuleEmitter<'a>, func: &'a AmirFunc) -> Result<Self, Diagnostic> {
        let mut this = Self {
            module,
            func,
            writer: IrWriter::default(),
            defined: vec![None; func.temps.len()],
            temp_slots: vec![None; func.temps.len()],
            temp_llvm_ty: vec![None; func.temps.len()],
            next_value: 0,
            current_return: None,
            return_counter: 0,
            current_block: BlockId::from_usize(0),
            current_llvm_label: String::new(),
            terminator_labels: HashMap::new(),
            edge_args: HashMap::new(),
        };
        if !func.temps.is_empty() {
            this.temp_slots[0] = Some(String::new());
        }
        for temp in &func.params {
            this.define(*temp, "function parameter")?;
        }
        let block_params = func
            .blocks
            .iter()
            .flat_map(|block| func.block_params(block.params).iter().cloned())
            .collect::<Vec<_>>();
        let assignments = func
            .blocks
            .iter()
            .flat_map(|block| func.block_stmts(block.id))
            .filter_map(|statement| match statement {
                AmirStmt::Assign { lhs, .. } => Some(*lhs),
                AmirStmt::Call { lhs, .. } => *lhs,
                _ => None,
            })
            .collect::<Vec<_>>();
        for param in &block_params {
            if assignments.contains(&param.id) {
                return Err(ice(format!(
                    "AMIR block parameter %t{} is also assigned, which the LLVM backend cannot represent as SSA",
                    param.id.as_usize()
                )));
            }
            this.define(param.id, "block parameter")?;
        }
        let mut counts = vec![0usize; func.temps.len()];
        for temp in &assignments {
            counts[temp.as_usize()] += 1;
        }
        for temp in assignments {
            if counts[temp.as_usize()] > 1 {
                this.temp_slots[temp.as_usize()] = Some(String::new());
            } else if temp.as_usize() != 0 {
                this.define(temp, "assignment")?;
            }
        }
        Ok(this)
    }

    fn temp_slot(&self, temp: TempId) -> Option<&str> {
        self.temp_slots[temp.as_usize()].as_deref()
    }

    fn destination(&mut self, lhs: TempId) -> String {
        if self.temp_slots[lhs.as_usize()].is_some() {
            let name = self.new_value();
            if lhs.as_usize() == 0 {
                self.current_return = Some(name.clone());
            }
            return name;
        }
        if lhs.as_usize() == 0 {
            let name = format!("%ret{}", self.return_counter);
            self.return_counter += 1;
            self.current_return = Some(name.clone());
            name
        } else {
            temp_name(lhs)
        }
    }

    fn return_value(&self) -> Option<String> {
        self.current_return.clone()
    }

    fn define(&mut self, temp: TempId, source: &'static str) -> Result<(), Diagnostic> {
        if let Some(previous) = self.defined[temp.as_usize()] {
            return Err(ice(format!(
                "AMIR temp %t{} is defined twice ({previous} and {source})",
                temp.as_usize()
            )));
        }
        self.defined[temp.as_usize()] = Some(source);
        Ok(())
    }

    fn ensure_defined(&self, temp: TempId) -> Result<(), Diagnostic> {
        if self.defined[temp.as_usize()].is_none() && self.temp_slots[temp.as_usize()].is_none() {
            return Err(ice(format!(
                "AMIR temp %t{} is used without a definition",
                temp.as_usize()
            )));
        }
        Ok(())
    }

    fn temp_type(&self, temp: TempId) -> ArType {
        self.module
            .interner
            .resolve(self.func.temps[temp.as_usize()].ty)
    }

    fn emit(mut self) -> Result<String, Diagnostic> {
        let return_ty = self.module.interner.resolve(self.func.return_type);
        let function_name = self.module.function_name(self.func.symbol);
        let is_main = function_name == "main";
        let return_text = if is_main && self.module.is_void(&return_ty) {
            "i32".to_owned()
        } else {
            self.module.llvm_ty(&return_ty)?
        };

        let params = self
            .func
            .params
            .iter()
            .map(|temp| {
                let ty = self.temp_type(*temp);
                let attribute = if matches!(ty, ArType::Ref(_) | ArType::RefMut(_)) {
                    "nonnull "
                } else {
                    ""
                };
                Ok(format!(
                    "{} {attribute}{}",
                    self.module.llvm_ty(&ty)?,
                    temp_name(*temp)
                ))
            })
            .collect::<Result<Vec<_>, Diagnostic>>()?;

        self.writer.line(format!(
            "define {return_text} @{}({}) nounwind {{",
            quote_name(&function_name),
            params.join(", ")
        ));

        let Some(entry) = self.func.blocks.first() else {
            return Err(ice("AMIR function has no basic blocks"));
        };
        let entry_id = entry.id;
        if !self.func.block_params(entry.params).is_empty() {
            return Err(ice("AMIR entry block must not have block parameters"));
        }
        self.writer.line(format!("{}:", block_label(entry_id)));

        for local in &self.func.locals {
            let ty = self.module.interner.resolve(local.ty);
            let (alloca, align) = self.module.alloca_type(&ty)?;
            self.writer.line(format!(
                "{} = alloca {alloca}, align {align}",
                local_name(local.id)
            ));
        }
        for temp in 0..self.func.temps.len() {
            if self.temp_slots[temp].is_some() {
                let ty = self.module.interner.resolve(self.func.temps[temp].ty);
                let (alloca, align) = self.module.alloca_type(&ty)?;
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {alloca}, align {align}"));
                self.temp_slots[temp] = Some(slot);
            }
        }

        // Pass 1: lower each block body + terminator into a private buffer so
        // edge arguments are materialized in predecessors before any successor
        // PHI is written (LLVM requires phis to be grouped at the block top).
        let blocks = self.func.blocks.clone();
        let mut bodies = Vec::with_capacity(blocks.len());
        for block in &blocks {
            self.current_block = block.id;
            self.current_llvm_label = block_label(block.id);
            self.current_return = None;
            let mut body = IrWriter::default();
            std::mem::swap(&mut self.writer, &mut body);
            let statements = self.func.block_stmts(block.id).cloned().collect::<Vec<_>>();
            for statement in &statements {
                self.emit_statement(statement)?;
            }
            let terminator = block.terminator.clone();
            self.terminator_labels
                .insert(block.id, self.current_llvm_label.clone());
            self.emit_terminator(&terminator)?;
            std::mem::swap(&mut self.writer, &mut body);
            bodies.push(body.finish());
        }

        // Pass 2: emit labels, PHI nodes from recorded edge args, then bodies.
        for (block, body) in blocks.iter().zip(bodies) {
            if block.id != entry_id {
                self.writer.blank();
                self.writer.line(format!("{}:", block_label(block.id)));
            }
            self.emit_block_phis(block)?;
            self.writer.append_raw(&body);
        }

        self.writer.line("}");
        Ok(self.writer.finish())
    }
}

pub(super) struct TypedValue {
    ty: String,
    value: String,
    /// Signedness of the AMIR source integer, when known.
    ///
    /// LLVM integer types are sign-agnostic (`i8` serves both `i8` and `u8`).
    /// Widening casts must consult this flag: `u8 as u32` is `zext`, not `sext`.
    /// Inferring signedness from the LLVM type string incorrectly sign-extends
    /// unsigned bytes (breaks SHA-256 padding `0x80`, Softwares/ita digests).
    source_signed: Option<bool>,
}

impl TypedValue {
    fn new(ty: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            ty: ty.into(),
            value: value.into(),
            source_signed: None,
        }
    }

    fn with_source_signed(mut self, signed: bool) -> Self {
        self.source_signed = Some(signed);
        self
    }

    fn render(&self) -> String {
        format!("{} {}", self.ty, self.value)
    }
}

pub(super) fn value_ar_type(llvm: &str) -> ArType {
    match llvm {
        "i1" => ArType::Primitive(Primitive::Bool),
        "i8" => ArType::Primitive(Primitive::I8),
        "i16" => ArType::Primitive(Primitive::I16),
        "i32" => ArType::Primitive(Primitive::I32),
        "i64" => ArType::Primitive(Primitive::I64),
        "float" => ArType::Primitive(Primitive::F32),
        "double" => ArType::Primitive(Primitive::F64),
        _ => ArType::Primitive(Primitive::Int),
    }
}

/// Parse `N` from an opaque LLVM aggregate type `[N x i8]`.
pub(super) fn parse_byte_aggregate_size(llvm: &str) -> Option<u64> {
    let rest = llvm.strip_prefix('[')?.strip_suffix(" x i8]")?;
    rest.parse().ok()
}

pub(super) fn ensure_float_decimal(value: &str) -> String {
    // LLVM rejects bare scientific forms like `1e-6` (no decimal point).
    if value.contains('.') {
        value.to_owned()
    } else if let Some(idx) = value.find(['e', 'E']) {
        format!("{}.0{}", &value[..idx], &value[idx..])
    } else {
        format!("{value}.0")
    }
}

pub(super) fn parse_char_literal(text: &str) -> Option<char> {
    // Literal pools store either the full source lexeme (`'\n'`) or the
    // unquoted escape payload (`\n`). Accept both forms.
    let content = text
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(text);
    if let Some(escaped) = content.strip_prefix('\\') {
        return match escaped {
            "n" => Some('\n'),
            "t" => Some('\t'),
            "r" => Some('\r'),
            "0" => Some('\0'),
            "\\" => Some('\\'),
            "\"" => Some('"'),
            "'" => Some('\''),
            "$" => Some('$'),
            unicode if unicode.starts_with("u{") && unicode.ends_with('}') => {
                let digits = &unicode[2..unicode.len() - 1];
                if digits.is_empty() {
                    return None;
                }
                u32::from_str_radix(digits, 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            hex if hex.starts_with('x') => u32::from_str_radix(&hex[1..], 16)
                .ok()
                .and_then(char::from_u32),
            _ => None,
        };
    }
    let mut chars = content.chars();
    let value = chars.next()?;
    chars.next().is_none().then_some(value)
}

pub(super) fn escape_c_string(bytes: &[u8]) -> String {
    let mut escaped = String::new();
    for &byte in bytes {
        if byte.is_ascii_graphic() || byte == b' ' {
            if byte == b'"' || byte == b'\\' {
                escaped.push('\\');
            }
            escaped.push(byte as char);
        } else {
            let _ = write!(escaped, "\\{byte:02X}");
        }
    }
    escaped
}

pub(super) fn quote_name(name: &str) -> String {
    let starts_well = name
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || matches!(ch, '_' | '.' | '$'));
    if starts_well
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '$'))
    {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\22"))
    }
}

pub(super) trait IntegerWidthFromLlvm {
    fn integer_width_from_llvm(&self, ty: &str) -> Option<u64>;
}

impl IntegerWidthFromLlvm for ModuleEmitter<'_> {
    fn integer_width_from_llvm(&self, ty: &str) -> Option<u64> {
        match ty {
            "i1" => Some(1),
            "i8" => Some(8),
            "i16" => Some(16),
            "i32" => Some(32),
            "i64" => Some(64),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_literal_coercion_uses_its_llvm_integer_width() {
        let program = AmirProgram {
            funcs: Vec::new(),
            literal_pool: Default::default(),
            extern_funcs: Default::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        let symbols = SymbolTable::new(0);
        let info = TypeInfo::default();
        let module = ModuleEmitter::new(&program, &symbols, &info);
        let blocks = Vec::new();
        let func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: info.type_interner.intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks: blocks.clone(),
            block_params: Vec::new(),
            stmts: Default::default(),
            cfg: arandu_middle::cfg::compute_cfg_edges(&blocks),
        };
        let mut emitter = FuncEmitter::new(&module, &func).expect("empty function emitter");

        let coerced = emitter
            .coerce(
                TypedValue::new("i64".to_owned(), "42".to_owned()),
                &ArType::IntLiteral,
            )
            .expect("i64 must coerce to the i32 representation of IntLiteral");

        assert_eq!(coerced.ty, "i32");
        assert!(emitter.writer.finish().contains("trunc i64 42 to i32"));
    }
}
