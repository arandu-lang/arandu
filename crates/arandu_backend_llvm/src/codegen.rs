//! Direct lowering from AMIR to textual LLVM IR.
//!
//! This module deliberately keeps LLVM behind a text boundary: the backend is
//! pure Rust and never links `llvm-sys`. The generated module can be validated
//! with `llvm-as` and executed with `lli` or compiled with `clang`/`llc`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use std::collections::HashMap;

use arandu_middle::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirOperand, AmirPlace, AmirProgram, AmirProjection,
    AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId, TempId, VectorCompareOp, VectorOp,
};
use arandu_middle::intrinsics::{AtomicOp, AtomicWidth, IntrinsicKind};
use arandu_middle::layout::{LayoutEngine, StructLayoutProvider, TagEncoding};
use arandu_middle::literal_pool::LiteralId;
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{ArType, Primitive, TypeInterner, VectorType};
use arandu_middle::{DiagCode, Diagnostic, Span, SymbolId, SymbolKind, SymbolTable};
use arandu_semantics::TypeInfo;

use crate::types::llvm_type;
use crate::writer::IrWriter;

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

fn ice(message: impl Into<String>) -> Diagnostic {
    Diagnostic::ice(DiagCode::ICEGEN001, message, Span::new(0, 0, 0))
}

fn temp_name(temp: TempId) -> String {
    format!("%t{}", temp.as_usize())
}

fn local_name(local: LocalId) -> String {
    format!("%s{}", local.as_usize())
}

fn block_label(block: BlockId) -> String {
    format!("bb{}", block.as_usize())
}

struct ModuleEmitter<'a> {
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
        self.add_declaration("ar_rt_raw_free", "declare void @ar_rt_raw_free(ptr)");
        self.add_declaration("ar_rt_raw_malloc", "declare ptr @ar_rt_raw_malloc(i64)");
        self.add_declaration("memcmp", "declare i32 @memcmp(ptr, ptr, i64)");
        self.add_declaration(
            "ar_str_concat",
            "declare { ptr, i64 } @ar_str_concat(ptr, i64, ptr, i64)",
        );
        self.add_declaration(
            "llvm.memcpy.p0.p0.i64",
            "declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)",
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
        output.push_str("source_filename = \"arandu\"\n\n");
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
        llvm_type(ty, self.interner).map_err(ice)
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
    /// Fat `str` and slices become two scalar slots (`ptr`, `i64`), matching
    /// Cranelift and runtime helpers such as `ar_fs_read_all(path_ptr, path_len, ...)`.
    /// Void parameters contribute no slots. Internal Arandu→Arandu calls keep
    /// the first-class `{ ptr, i64 }` value ABI.
    fn host_abi_param_llvm_tys(&self, ty: &ArType) -> Result<Vec<String>, Diagnostic> {
        if self.is_void(ty) {
            return Ok(Vec::new());
        }
        if self.is_fat_pointer(ty) {
            return Ok(vec!["ptr".to_owned(), "i64".to_owned()]);
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

struct FuncEmitter<'a, 'm> {
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

    fn emit_block_phis(&mut self, block: &AmirBasicBlock) -> Result<(), Diagnostic> {
        let params = self.func.block_params(block.params).to_vec();
        if params.is_empty() {
            return Ok(());
        }
        let incoming = self.incoming(block.id);
        if incoming.is_empty() {
            return Err(ice(format!(
                "block {} has parameters but no predecessors",
                block_label(block.id)
            )));
        }
        for (index, param) in params.iter().enumerate() {
            let ty = self.module.interner.resolve(param.ty);
            let ty_text = self.module.llvm_ty(&ty)?;
            let mut entries = Vec::with_capacity(incoming.len());
            let mut seen = Vec::new();
            for (predecessor, args) in &incoming {
                if seen.contains(predecessor) {
                    return Err(ice(format!(
                        "block {} has multiple edges from {} with parameters",
                        block_label(block.id),
                        block_label(*predecessor)
                    )));
                }
                seen.push(*predecessor);
                if args.len() != params.len() {
                    return Err(ice(format!(
                        "block {} expects {} arguments, predecessor {} provides {}",
                        block_label(block.id),
                        params.len(),
                        block_label(*predecessor),
                        args.len()
                    )));
                }
                let Some(edge) = self.edge_args.get(&(*predecessor, block.id)) else {
                    return Err(ice(format!(
                        "missing materialized edge args from {} to {}",
                        block_label(*predecessor),
                        block_label(block.id)
                    )));
                };
                if edge.len() != params.len() {
                    return Err(ice(format!(
                        "edge {} -> {} has {} args, expected {}",
                        block_label(*predecessor),
                        block_label(block.id),
                        edge.len(),
                        params.len()
                    )));
                }
                let llvm_pred = self
                    .terminator_labels
                    .get(predecessor)
                    .cloned()
                    .unwrap_or_else(|| block_label(*predecessor));
                entries.push(format!("[ {}, %{llvm_pred} ]", edge[index].value));
            }
            self.writer.line(format!(
                "{} = phi {ty_text} {}",
                temp_name(param.id),
                entries.join(", ")
            ));
        }
        Ok(())
    }

    fn enter_llvm_label(&mut self, name: &str) {
        self.writer.line(format!("{name}:"));
        self.current_llvm_label = name.to_owned();
    }

    fn record_edge_args(
        &mut self,
        target: BlockId,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let target_block = self
            .func
            .blocks
            .iter()
            .find(|block| block.id == target)
            .ok_or_else(|| ice(format!("unknown block {}", block_label(target))))?;
        let params = self.func.block_params(target_block.params).to_vec();
        if args.len() != params.len() {
            return Err(ice(format!(
                "block {} expects {} arguments, edge from {} provides {}",
                block_label(target),
                params.len(),
                block_label(self.current_block),
                args.len()
            )));
        }
        let mut values = Vec::with_capacity(args.len());
        for (arg, param) in args.iter().zip(&params) {
            let ty = self.module.interner.resolve(param.ty);
            values.push(self.materialize_block_arg(arg, &ty)?);
        }
        self.edge_args.insert((self.current_block, target), values);
        Ok(())
    }

    /// Evaluate a block-argument operand in the *predecessor*, emitting any
    /// loads/coercions before the branch so the successor can use a pure PHI.
    fn materialize_block_arg(
        &mut self,
        operand: &AmirOperand,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        self.operand(operand, expected)
    }

    fn incoming(&self, target: BlockId) -> Vec<(BlockId, Vec<AmirOperand>)> {
        let mut incoming = Vec::new();
        for block in &self.func.blocks {
            match &block.terminator {
                AmirTerminator::Goto { target: edge, args } if *edge == target => {
                    incoming.push((block.id, args.clone()))
                }
                AmirTerminator::Branch {
                    if_true,
                    true_args,
                    if_false,
                    false_args,
                    ..
                } => {
                    if *if_true == target {
                        incoming.push((block.id, true_args.clone()));
                    }
                    if *if_false == target {
                        incoming.push((block.id, false_args.clone()));
                    }
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    for (_, edge, args) in targets {
                        if *edge == target {
                            incoming.push((block.id, args.clone()));
                        }
                    }
                    if otherwise.0 == target {
                        incoming.push((block.id, otherwise.1.clone()));
                    }
                }
                AmirTerminator::Suspend { resume, args, .. } if *resume == target => {
                    incoming.push((block.id, args.clone()));
                }
                _ => {}
            }
        }
        incoming
    }

    fn emit_statement(&mut self, statement: &AmirStmt) -> Result<(), Diagnostic> {
        match statement {
            AmirStmt::Assign { lhs, rhs } => {
                let ty = self.temp_type(*lhs);
                let result = self.destination(*lhs);
                self.emit_rvalue(&result, &ty, rhs, Some(*lhs))?;
                if let Some(slot) = self.temp_slot(*lhs).map(str::to_owned) {
                    let ty_text = self.temp_llvm_ty[lhs.as_usize()]
                        .clone()
                        .map_or_else(|| self.module.llvm_ty(&ty), Ok)?;
                    let align = self.module.align_of(&ty);
                    self.writer.line(format!(
                        "store {ty_text} {result}, ptr {slot}, align {align}"
                    ));
                }
                Ok(())
            }
            AmirStmt::Store { lhs, rhs } => {
                let (address, ty) = self.place(lhs)?;
                let value = self.operand(rhs, &ty)?;
                let align = self.module.align_of(&ty);
                self.writer.line(format!(
                    "store {}, ptr {address}, align {align}",
                    value.render()
                ));
                Ok(())
            }
            AmirStmt::Call {
                lhs, callee, args, ..
            } => {
                let destination = lhs.map(|lhs| self.destination(lhs));
                self.emit_call(*lhs, destination.as_deref(), callee, args)?;
                if let (Some(lhs), Some(value)) = (lhs, destination)
                    && let Some(slot) = self.temp_slot(*lhs).map(str::to_owned)
                {
                    let ty = self.temp_type(*lhs);
                    let ty_text = self.module.llvm_ty(&ty)?;
                    let align = self.module.align_of(&ty);
                    self.writer.line(format!(
                        "store {ty_text} {value}, ptr {slot}, align {align}"
                    ));
                }
                Ok(())
            }
            AmirStmt::Free(operand) => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = if matches!(
                    operand_ty,
                    ArType::Primitive(Primitive::Str) | ArType::Slice(_) | ArType::MutSlice(_)
                ) {
                    let value = self.operand(operand, &operand_ty)?;
                    let pointer = self.new_value();
                    self.writer.line(format!(
                        "{pointer} = extractvalue {{ ptr, i64 }} {}, 0",
                        value.value
                    ));
                    TypedValue::new("ptr".to_owned(), pointer)
                } else {
                    let pointer_ty = self.module.void_ptr_ty();
                    self.operand(operand, &pointer_ty)?
                };
                self.writer
                    .line(format!("call void @ar_rt_raw_free({})", pointer.render()));
                Ok(())
            }
            AmirStmt::Destroy(place) => self.emit_destroy(place),
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => Ok(()),
        }
    }

    fn emit_destroy(&mut self, place: &AmirPlace) -> Result<(), Diagnostic> {
        let (address, ty) = self.place(place)?;
        if matches!(ty, ArType::Primitive(Primitive::Str)) {
            let value_ty = self.module.llvm_ty(&ty)?;
            let value = self.new_value();
            self.writer.line(format!(
                "{value} = load {value_ty}, ptr {address}, align {}",
                self.module.align_of(&ty)
            ));
            let pointer = self.new_value();
            self.writer
                .line(format!("{pointer} = extractvalue {value_ty} {value}, 0"));
            self.writer
                .line(format!("call void @ar_rt_raw_free(ptr {pointer})"));
            self.writer.line(format!(
                "store {value_ty} zeroinitializer, ptr {address}, align {}",
                self.module.align_of(&ty)
            ));
            return Ok(());
        }

        let type_id = self.module.interner.intern(ty.clone());
        let Some(&destructor) = self.module.info.destructor_instances.get(&type_id) else {
            return Ok(());
        };
        let destructor_func = self
            .module
            .program
            .funcs
            .iter()
            .find(|func| func.symbol == destructor)
            .ok_or_else(|| {
                ice(format!(
                    "destructor {} for {ty:?} is not present in the LLVM module",
                    self.module.function_name(destructor)
                ))
            })?;
        let Some(&receiver) = destructor_func.params.first() else {
            return Err(ice(format!(
                "destructor {} has no receiver parameter",
                self.module.function_name(destructor)
            )));
        };
        let receiver_ty = self
            .module
            .interner
            .resolve(destructor_func.temps[receiver.as_usize()].ty);
        let argument = if matches!(
            receiver_ty,
            ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)
        ) {
            TypedValue::new("ptr".to_owned(), address)
        } else {
            let value_ty = self.module.llvm_ty(&receiver_ty)?;
            let value = self.new_value();
            self.writer.line(format!(
                "{value} = load {value_ty}, ptr {address}, align {}",
                self.module.align_of(&receiver_ty)
            ));
            TypedValue::new(value_ty, value)
        };
        self.writer.line(format!(
            "call void @{}({})",
            quote_name(&self.module.function_name(destructor)),
            argument.render()
        ));
        Ok(())
    }

    fn emit_rvalue(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
        lhs: Option<TempId>,
    ) -> Result<(), Diagnostic> {
        match rvalue {
            AmirRvalue::Use(operand) => {
                let value = self.operand(operand, lhs_ty)?;
                let rendered = value.render();
                self.writer
                    .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
                Ok(())
            }
            AmirRvalue::Binary { op, left, right } => {
                self.emit_binary(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::Unary { op, operand } => self.emit_unary(result, lhs_ty, *op, operand),
            AmirRvalue::Load(place) => {
                let (address, place_ty) = self.place(place)?;
                let loaded_ty = self.module.llvm_ty(&place_ty)?;
                let align = self.module.align_of(&place_ty);
                let raw = self.new_value();
                self.writer.line(format!(
                    "{raw} = load {loaded_ty}, ptr {address}, align {align}"
                ));
                self.assign_coerced(
                    result,
                    self.typed_from_ar(loaded_ty, raw, &place_ty),
                    lhs_ty,
                )
            }
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                let (address, _) = self.place(place)?;
                self.writer.line(format!(
                    "{} = getelementptr i8, ptr {address}, i64 0",
                    result
                ));
                Ok(())
            }
            AmirRvalue::RelativeBorrow { local, .. } => {
                self.writer.line(format!(
                    "{} = getelementptr i8, ptr {}, i64 0",
                    result,
                    local_name(*local)
                ));
                Ok(())
            }
            AmirRvalue::Alloc(size) => {
                let size_ty = self.module.operand_type(size, self.func);
                let size = self.operand(size, &size_ty)?;
                let i64_ty = self.module.i64_ty();
                let size = self.coerce(size, &i64_ty)?;
                self.writer.line(format!(
                    "{} = call ptr @ar_rt_raw_malloc({})",
                    result,
                    size.render()
                ));
                Ok(())
            }
            AmirRvalue::Len(value) => {
                let ty = self.module.operand_type(value, self.func);
                if let ArType::Array(length, _) = &ty {
                    let value = length.to_string();
                    return self.assign_coerced(
                        result,
                        TypedValue::new(self.module.llvm_ty(lhs_ty)?, value),
                        lhs_ty,
                    );
                }
                let value = self.operand(value, &ty)?;
                let aggregate = self.module.llvm_ty(&ty)?;
                if aggregate == "{ ptr, i64 }" {
                    self.writer.line(format!(
                        "{} = extractvalue {aggregate} {}, 1",
                        result, value.value
                    ));
                    Ok(())
                } else if value.ty == "ptr" {
                    // `ref String` / `ref str` / `ref []T`: length sits at
                    // pointer-width offset in the shared fat-descriptor layout.
                    let len_addr = self.byte_gep(&value.value, self.module.layout.pointer_width());
                    let loaded = self.new_value();
                    let align = self.module.pointer_align();
                    self.writer.line(format!(
                        "{loaded} = load i64, ptr {len_addr}, align {align}"
                    ));
                    self.assign_coerced(result, TypedValue::new("i64".to_owned(), loaded), lhs_ty)
                } else {
                    Err(ice(format!(
                        "LLVM lowering does not implement len for {ty:?}"
                    )))
                }
            }
            AmirRvalue::StrView { owner } => {
                // Intrinsic returns `ref str`: a thin pointer to a `{data,len}`
                // descriptor. Owned String/Path layouts share that prefix, so a
                // `ref Owner` is already a valid `ref str` (Cranelift pass-through).
                let ty = self.module.operand_type(owner, self.func);
                let value = self.operand(owner, &ty)?;
                if value.ty == "ptr" || self.module.is_pointer(&ty) {
                    return self.assign_coerced(
                        result,
                        TypedValue::new("ptr".to_owned(), value.value),
                        lhs_ty,
                    );
                }
                if value.ty == "{ ptr, i64 }" || parse_byte_aggregate_size(&value.ty).is_some() {
                    // By-value descriptor / opaque String: spill and return its address.
                    let slot = self.new_value();
                    self.writer
                        .line(format!("{slot} = alloca {}, align 8", value.ty));
                    self.writer.line(format!(
                        "store {} {}, ptr {slot}, align 8",
                        value.ty, value.value
                    ));
                    return self.assign_coerced(
                        result,
                        TypedValue::new("ptr".to_owned(), slot),
                        lhs_ty,
                    );
                }
                Err(ice(format!(
                    "LLVM lowering does not implement strView for {ty:?}"
                )))
            }
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let struct_ty = match lhs_ty {
                    ArType::Named(id, _) if *id == *struct_symbol => lhs_ty.clone(),
                    _ => ArType::named(*struct_symbol, &[], self.module.interner),
                };
                let field_defs = self
                    .module
                    .info
                    .get_struct_fields_for_type(&struct_ty, self.module.interner)
                    .ok_or_else(|| {
                        eprintln!(
                            "LLVM StructLiteral missing fields: lhs={lhs_ty:?} struct={struct_ty:?} symbol={struct_symbol:?} fields={fields:?}"
                        );
                        ice(format!("no struct fields for {struct_ty:?}"))
                    })?;
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&struct_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {struct_ty:?}: {error}"))
                    })?;
                let mut resolved = Vec::with_capacity(fields.len());
                for (name, operand) in fields {
                    let field = field_defs
                        .get(name.as_str())
                        .ok_or_else(|| ice(format!("unknown struct field {name:?}")))?;
                    let offset = layout
                        .field_offsets
                        .get(field.index)
                        .copied()
                        .ok_or_else(|| ice(format!("field {name:?} is outside {struct_ty:?}")))?;
                    let field_ty = arandu_middle::layout::instantiated_field_type(
                        &struct_ty,
                        name.as_str(),
                        self.module.interner,
                        self.module.info,
                    )
                    .map(|id| self.module.interner.resolve(id))
                    .ok_or_else(|| ice(format!("cannot instantiate field {name:?}")))?;
                    resolved.push((offset, field_ty, operand));
                }
                self.construct_aggregate(result, &struct_ty, &resolved)
            }
            AmirRvalue::Tuple { items } => {
                let ArType::Tuple(types) = lhs_ty else {
                    return Err(ice(format!(
                        "tuple literal requires a tuple result type, got {lhs_ty:?}"
                    )));
                };
                let types = self.module.interner.type_args(*types);
                if types.len() != items.len() {
                    return Err(ice(format!(
                        "tuple literal has {} items for {} types",
                        items.len(),
                        types.len()
                    )));
                }
                let layout = self
                    .module
                    .layout
                    .layout_of_type(lhs_ty, self.module.interner, self.module.info)
                    .map_err(|error| ice(format!("LLVM layout failed for {lhs_ty:?}: {error}")))?;
                let mut resolved = Vec::with_capacity(items.len());
                for (index, operand) in items.iter().enumerate() {
                    let offset = layout.field_offsets.get(index).copied().unwrap_or(0);
                    let field_ty = self.module.interner.resolve(types[index]);
                    resolved.push((offset, field_ty, operand));
                }
                self.construct_aggregate(result, lhs_ty, &resolved)
            }
            AmirRvalue::Array { items } => {
                let ArType::Array(length, element) = lhs_ty else {
                    return Err(ice(format!(
                        "array literal requires an array result type, got {lhs_ty:?}"
                    )));
                };
                if items.len() > *length as usize {
                    return Err(ice(format!(
                        "array literal has {} items for length {length}",
                        items.len()
                    )));
                }
                let element_ty = self.module.interner.resolve(*element);
                let stride = self
                    .module
                    .layout
                    .layout_of_type(&element_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {element_ty:?}: {error}"))
                    })?
                    .size;
                let mut resolved = Vec::with_capacity(items.len());
                for (index, operand) in items.iter().enumerate() {
                    resolved.push((index as u64 * stride, element_ty.clone(), operand));
                }
                self.construct_aggregate(result, lhs_ty, &resolved)
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => self.emit_enum_construct(result, lhs_ty, *variant_tag, payload.as_ref()),
            AmirRvalue::Discriminant { value } => self.emit_discriminant(result, lhs_ty, value),
            AmirRvalue::EnumPayload {
                value,
                field_ty,
                tuple_ty,
                index,
                ..
            } => self.emit_enum_payload(result, lhs_ty, value, *field_ty, *tuple_ty, *index, lhs),
            AmirRvalue::FieldAccess { base, field } => {
                let base_ty = self.module.operand_type(base, self.func);
                let (owner, address) = match &base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        let owner = self.module.interner.resolve(*inner);
                        let pointer = self.operand(base, &base_ty)?;
                        (owner, pointer.value)
                    }
                    _ => (base_ty.clone(), self.aggregate_address(base, &base_ty)?),
                };
                let (address, field_ty) = self.aggregate_field_address(&owner, &address, *field)?;
                // Result/Option/enum payload fields often lower as `.1` FieldAccess
                // (not EnumPayload). Non-scalar payloads use pointer-repr so a
                // later coerce can copy the full destination layout.
                let owner_is_enum = matches!(
                    &owner,
                    ArType::Option(_) | ArType::Result(_, _) | ArType::Poll(_) | ArType::Named(..)
                ) && self
                    .module
                    .layout
                    .layout_of_type(&owner, self.module.interner, self.module.info)
                    .ok()
                    .and_then(|l| l.tag_encoding)
                    .is_some();
                if owner_is_enum
                    && !self.module.is_pointer(&field_ty)
                    && !self.module.is_pointer(lhs_ty)
                    && self.module.integer_width(&field_ty).is_none()
                    && self.module.integer_width(lhs_ty).is_none()
                    && !self.module.is_float(&field_ty)
                    && !self.module.is_float(lhs_ty)
                    && !matches!(field_ty, ArType::Primitive(Primitive::Bool))
                    && !matches!(lhs_ty, ArType::Primitive(Primitive::Bool))
                {
                    self.writer.line("; enum_field_ptr_repr");
                    self.writer
                        .line(format!("{result} = getelementptr i8, ptr {address}, i64 0"));
                    if let Some(temp) = lhs {
                        self.temp_llvm_ty[temp.as_usize()] = Some("ptr".to_owned());
                    }
                    return Ok(());
                }
                let field_text = self.module.llvm_ty(&field_ty)?;
                let align = self.module.align_of(&field_ty);
                let raw = self.new_value();
                self.writer.line(format!(
                    "{raw} = load {field_text}, ptr {address}, align {align}"
                ));
                self.assign_coerced(
                    result,
                    self.typed_from_ar(field_text, raw, &field_ty),
                    lhs_ty,
                )
            }
            AmirRvalue::IndexAccess { base, index } => {
                let base_ty = self.module.operand_type(base, self.func);
                let deref_ty = match &base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        self.module.interner.resolve(*inner)
                    }
                    other => other.clone(),
                };
                if let Some((_, _, _)) = self.module.vector_info(&deref_ty) {
                    let vector = self.operand(base, &base_ty)?;
                    let index_ty = ArType::Primitive(Primitive::I32);
                    let index = self.operand(index, &index_ty)?;
                    self.writer.line(format!(
                        "{result} = extractelement {} {}, i32 {}",
                        vector.ty, vector.value, index.value
                    ));
                    return Ok(());
                }
                let element_id = arandu_middle::types::index_elem_type(
                    &deref_ty,
                    self.module.symbols,
                    self.module.interner,
                )
                .ok_or_else(|| {
                    ice(format!(
                        "index access requires an indexable type, got {deref_ty:?}"
                    ))
                })?;
                let element_ty = self.module.interner.resolve(element_id);
                let stride = self
                    .module
                    .layout
                    .layout_of_type(&element_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {element_ty:?}: {error}"))
                    })?
                    .size;
                let index_ty = self.module.usize_ty();
                let index = self.operand(index, &index_ty)?;
                let index = self.coerce(index, &self.module.i64_ty())?;
                let base_address = if matches!(
                    &base_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                ) {
                    let pointer = self.operand(base, &base_ty)?;
                    pointer.value
                } else if matches!(&deref_ty, ArType::Array(..)) {
                    self.aggregate_address(base, &deref_ty)?
                } else if matches!(&deref_ty, ArType::Slice(_) | ArType::MutSlice(_)) {
                    let descriptor = self.operand(base, &deref_ty)?;
                    let pointer = self.new_value();
                    self.writer.line(format!(
                        "{pointer} = extractvalue {{ ptr, i64 }} {}, 0",
                        descriptor.value
                    ));
                    pointer
                } else {
                    return Err(ice(format!(
                        "LLVM backend does not implement index access for {deref_ty:?}"
                    )));
                };
                let offset = self.new_value();
                self.writer
                    .line(format!("{offset} = mul i64 {}, {stride}", index.value));
                let address = self.new_value();
                self.writer.line(format!(
                    "{address} = getelementptr inbounds i8, ptr {base_address}, i64 {offset}"
                ));
                let element_text = self.module.llvm_ty(&element_ty)?;
                let align = self.module.align_of(&element_ty);
                let raw = self.new_value();
                self.writer.line(format!(
                    "{raw} = load {element_text}, ptr {address}, align {align}"
                ));
                self.assign_coerced(
                    result,
                    self.typed_from_ar(element_text, raw, &element_ty),
                    lhs_ty,
                )
            }
            AmirRvalue::SliceView { data, len, .. } => {
                let pointer_ty = self.module.void_ptr_ty();
                let data = self.operand(data, &pointer_ty)?;
                let length_ty = self.module.usize_ty();
                let len = self.operand(len, &length_ty)?;
                let len = self.coerce(len, &self.module.i64_ty())?;
                let first = self.new_value();
                self.writer.line(format!(
                    "{first} = insertvalue {{ ptr, i64 }} undef, ptr {}, 0",
                    data.value
                ));
                self.writer.line(format!(
                    "{result} = insertvalue {{ ptr, i64 }} {first}, i64 {}, 1",
                    len.value
                ));
                Ok(())
            }
            AmirRvalue::SliceData(slice) => {
                let slice_ty = self.module.operand_type(slice, self.func);
                let slice = self.operand(slice, &slice_ty)?;
                self.writer.line(format!(
                    "{result} = extractvalue {{ ptr, i64 }} {}, 0",
                    slice.value
                ));
                Ok(())
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                let slice_ty = self.module.operand_type(slice, self.func);
                let slice = self.operand(slice, &slice_ty)?;
                let pointer = self.new_value();
                self.writer.line(format!(
                    "{pointer} = extractvalue {{ ptr, i64 }} {}, 0",
                    slice.value
                ));
                let start_ty = self.module.usize_ty();
                let start = self.operand(start, &start_ty)?;
                let start = self.coerce(start, &self.module.i64_ty())?;
                let element_ty = match &slice_ty {
                    ArType::Slice(inner) | ArType::MutSlice(inner) => {
                        self.module.interner.resolve(*inner)
                    }
                    other => {
                        return Err(ice(format!(
                            "slice subslice requires a slice, got {other:?}"
                        )));
                    }
                };
                let stride = self
                    .module
                    .layout
                    .layout_of_type(&element_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {element_ty:?}: {error}"))
                    })?
                    .size;
                let offset = self.new_value();
                self.writer
                    .line(format!("{offset} = mul i64 {}, {stride}", start.value));
                let data = self.new_value();
                self.writer.line(format!(
                    "{data} = getelementptr inbounds i8, ptr {pointer}, i64 {offset}"
                ));
                let length_ty = self.module.usize_ty();
                let len = self.operand(len, &length_ty)?;
                let len = self.coerce(len, &self.module.i64_ty())?;
                let first = self.new_value();
                self.writer.line(format!(
                    "{first} = insertvalue {{ ptr, i64 }} undef, ptr {data}, 0"
                ));
                self.writer.line(format!(
                    "{result} = insertvalue {{ ptr, i64 }} {first}, i64 {}, 1",
                    len.value
                ));
                Ok(())
            }
            AmirRvalue::StrBytes { source } => {
                let source_ty = self.module.operand_type(source, self.func);
                let source = self.operand(source, &source_ty)?;
                self.assign_coerced(result, source, lhs_ty)
            }
            AmirRvalue::CoroutineReady {
                value, payload_ty, ..
            } => self.emit_coroutine_ready(result, value, *payload_ty),
            AmirRvalue::GenInsert { .. }
            | AmirRvalue::GenGet { .. }
            | AmirRvalue::GenSet { .. }
            | AmirRvalue::GenUpsert { .. }
            | AmirRvalue::GenRemove { .. } => self.emit_gen_operation(result, lhs_ty, rvalue),
            AmirRvalue::StringInterp { parts } => self.emit_string_interp(result, lhs_ty, parts),
            AmirRvalue::ToStr { value, src_ty } => self.emit_to_str(result, lhs_ty, value, *src_ty),
            AmirRvalue::VectorOp { op, left, right } => {
                self.emit_vector_op(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                self.emit_vector_compare(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => self.emit_vector_select(result, lhs_ty, mask, if_true, if_false),
            AmirRvalue::VectorSplat { value } => self.emit_vector_splat(result, lhs_ty, value),
            AmirRvalue::VectorShuffle { left, right, mask } => {
                self.emit_vector_shuffle(result, lhs_ty, left, right, mask)
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                let ty = self.module.operand_type(vector, self.func);
                let vector = self.operand(vector, &ty)?;
                let (_, _, lane_ty) = self.module.vector_info(&ty).ok_or_else(|| {
                    ice(format!(
                        "vector lane extraction requires a vector, got {ty:?}"
                    ))
                })?;
                let lane_ty = ArType::Primitive(lane_ty);
                let lane_text = self.module.llvm_ty(&lane_ty)?;
                self.writer.line(format!(
                    "{} = extractelement {} {}, i32 {lane}",
                    result, vector.ty, vector.value
                ));
                let _ = lane_text;
                Ok(())
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                let ty = self.module.operand_type(vector, self.func);
                let vector = self.operand(vector, &ty)?;
                let (_, _, lane_ty) = self.module.vector_info(&ty).ok_or_else(|| {
                    ice(format!("vector lane insert requires a vector, got {ty:?}"))
                })?;
                let lane_ty = ArType::Primitive(lane_ty);
                let value = self.operand(value, &lane_ty)?;
                self.writer.line(format!(
                    "{} = insertelement {} {}, {} {}, i32 {lane}",
                    result, vector.ty, vector.value, value.ty, value.value
                ));
                Ok(())
            }
            AmirRvalue::BlackBox { value, .. } => {
                let value = self.operand(value, lhs_ty)?;
                let rendered = value.render();
                self.writer
                    .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
                Ok(())
            }
        }
    }

    fn emit_binary(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: BinaryOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        if matches!(
            op,
            BinaryOp::NullCoalesce | BinaryOp::RangeExclusive | BinaryOp::RangeInclusive
        ) {
            return Err(ice(format!(
                "LLVM backend does not yet lower binary operator {op:?}"
            )));
        }

        let comparison = matches!(
            op,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::LtEqual
                | BinaryOp::GtEqual
        );
        let operand_ty = if comparison {
            self.module.operand_type(left, self.func)
        } else {
            lhs_ty.clone()
        };
        if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
            && self.module.is_fat_pointer(&operand_ty)
        {
            return self.emit_fat_pointer_eq(
                result,
                left,
                right,
                &operand_ty,
                matches!(op, BinaryOp::NotEqual),
            );
        }
        let left = self.operand(left, &operand_ty)?;
        let right = self.operand(right, &operand_ty)?;
        let signed = self.module.is_signed(&operand_ty);
        let float = self.module.is_float(&operand_ty);

        let expression = match op {
            BinaryOp::Equal => {
                if float {
                    format!("fcmp oeq {}, {}", left.render(), right.value)
                } else {
                    format!("icmp eq {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::NotEqual => {
                if float {
                    format!("fcmp une {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ne {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Lt => {
                if float {
                    format!("fcmp olt {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp slt {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ult {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Gt => {
                if float {
                    format!("fcmp ogt {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sgt {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ugt {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::LtEqual => {
                if float {
                    format!("fcmp ole {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sle {}, {}", left.render(), right.value)
                } else {
                    format!("icmp ule {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::GtEqual => {
                if float {
                    format!("fcmp oge {}, {}", left.render(), right.value)
                } else if signed {
                    format!("icmp sge {}, {}", left.render(), right.value)
                } else {
                    format!("icmp uge {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Add => self.arithmetic("add", "fadd", &left, &right, float),
            BinaryOp::Sub => self.arithmetic("sub", "fsub", &left, &right, float),
            BinaryOp::Mul => self.arithmetic("mul", "fmul", &left, &right, float),
            BinaryOp::Div => {
                if float {
                    format!("fdiv {}, {}", left.render(), right.value)
                } else if signed {
                    format!("sdiv {}, {}", left.render(), right.value)
                } else {
                    format!("udiv {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::Mod => {
                if float {
                    format!("frem {}, {}", left.render(), right.value)
                } else if signed {
                    format!("srem {}, {}", left.render(), right.value)
                } else {
                    format!("urem {}, {}", left.render(), right.value)
                }
            }
            BinaryOp::BitOr => self.arithmetic("or", "or", &left, &right, false),
            BinaryOp::BitXor => self.arithmetic("xor", "xor", &left, &right, false),
            BinaryOp::BitAnd => self.arithmetic("and", "and", &left, &right, false),
            BinaryOp::Or | BinaryOp::And => {
                let instruction = if op == BinaryOp::Or { "or" } else { "and" };
                format!("{instruction} {}, {}", left.render(), right.value)
            }
            BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                let Some(width) = self.module.integer_width(&operand_ty) else {
                    return Err(ice(format!(
                        "shift requires an integer, got {operand_ty:?}"
                    )));
                };
                let shift_ty = left.ty.clone();
                let mask = self.new_value();
                self.writer.line(format!(
                    "{mask} = and {shift_ty} {}, {}",
                    right.value,
                    width.saturating_sub(1)
                ));
                let instruction = match op {
                    BinaryOp::ShiftLeft => "shl",
                    BinaryOp::ShiftRight if signed => "ashr",
                    BinaryOp::ShiftRight => "lshr",
                    _ => unreachable!(),
                };
                format!("{instruction} {shift_ty} {}, {mask}", left.value)
            }
            _ => {
                return Err(ice(format!(
                    "LLVM backend does not yet lower binary operator {op:?}"
                )));
            }
        };
        self.writer.line(format!("{} = {expression}", result));
        Ok(())
    }

    fn arithmetic(
        &self,
        integer: &str,
        float: &str,
        left: &TypedValue,
        right: &TypedValue,
        is_float: bool,
    ) -> String {
        let instruction = if is_float { float } else { integer };
        format!("{instruction} {}, {}", left.render(), right.value)
    }

    fn emit_unary(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: UnaryOp,
        operand: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        match op {
            UnaryOp::Neg => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let value = self.operand(operand, &operand_ty)?;
                if self.module.is_float(&operand_ty) {
                    self.writer
                        .line(format!("{} = fneg {}", result, value.render()));
                } else {
                    self.writer
                        .line(format!("{} = sub {} 0, {}", result, value.ty, value.value));
                }
                Ok(())
            }
            UnaryOp::Not | UnaryOp::BitNot => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let value = self.operand(operand, &operand_ty)?;
                let all_ones = if value.ty == "i1" {
                    "true".to_owned()
                } else {
                    "-1".to_owned()
                };
                self.writer.line(format!(
                    "{} = xor {} {}, {all_ones}",
                    result, value.ty, value.value
                ));
                Ok(())
            }
            UnaryOp::Await => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = self.operand(operand, &operand_ty)?;
                let payload_ty = match &operand_ty {
                    ArType::Coroutine(inner) => self.module.interner.resolve(*inner),
                    _ => lhs_ty.clone(),
                };
                let address = self.byte_gep(&pointer.value, 8);
                let payload_text = self.module.llvm_ty(&payload_ty)?;
                let align = self.module.align_of(&payload_ty);
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {payload_text}, ptr {address}, align {align}"
                ));
                self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty)
            }
            UnaryOp::Deref => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = self.operand(operand, &operand_ty)?;
                let pointee = match &operand_ty {
                    ArType::Ptr(inner)
                    | ArType::Ref(inner)
                    | ArType::RefMut(inner)
                    | ArType::Nullable(inner) => self.module.interner.resolve(*inner),
                    other => {
                        return Err(ice(format!("LLVM deref requires a pointer, got {other:?}")));
                    }
                };
                let pointee_text = self.module.llvm_ty(&pointee)?;
                let align = self.module.align_of(&pointee);
                self.writer.line(format!(
                    "{} = load {pointee_text}, ptr {}, align {align}",
                    result, pointer.value
                ));
                let _ = lhs_ty;
                Ok(())
            }
            other => Err(ice(format!(
                "LLVM backend does not yet lower unary operator {other:?}"
            ))),
        }
    }

    fn emit_vector_op(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: VectorOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (_, _, lane) = self.module.vector_info(lhs_ty).ok_or_else(|| {
            ice(format!(
                "vector operation requires a vector, got {lhs_ty:?}"
            ))
        })?;
        let left = self.operand(left, lhs_ty)?;
        let right = self.operand(right, lhs_ty)?;
        let float = lane.is_float();
        let signed = lane.is_signed();
        let instruction = match op {
            VectorOp::Add => {
                if float {
                    "fadd"
                } else {
                    "add"
                }
            }
            VectorOp::Sub => {
                if float {
                    "fsub"
                } else {
                    "sub"
                }
            }
            VectorOp::Mul => {
                if float {
                    "fmul"
                } else {
                    "mul"
                }
            }
            VectorOp::Div => {
                if float {
                    "fdiv"
                } else if signed {
                    "sdiv"
                } else {
                    "udiv"
                }
            }
            VectorOp::BitAnd => "and",
            VectorOp::BitOr => "or",
            VectorOp::BitXor => "xor",
        };
        self.writer.line(format!(
            "{} = {instruction} {}, {}",
            result,
            left.render(),
            right.value
        ));
        Ok(())
    }

    fn emit_vector_compare(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: VectorCompareOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let ty = self.module.operand_type(left, self.func);
        let (_, lanes, lane) = self
            .module
            .vector_info(&ty)
            .ok_or_else(|| ice(format!("vector comparison requires a vector, got {ty:?}")))?;
        let left = self.operand(left, &ty)?;
        let right = self.operand(right, &ty)?;
        let lane_ty = ArType::Primitive(lane);
        let lane_text = self.module.llvm_ty(&lane_ty)?;
        let predicate = if lane.is_float() {
            match op {
                VectorCompareOp::Eq => "oeq",
                VectorCompareOp::Ne => "une",
                VectorCompareOp::Lt => "olt",
                VectorCompareOp::Le => "ole",
                VectorCompareOp::Gt => "ogt",
                VectorCompareOp::Ge => "oge",
            }
        } else {
            match op {
                VectorCompareOp::Eq => "eq",
                VectorCompareOp::Ne => "ne",
                VectorCompareOp::Lt => {
                    if lane.is_signed() {
                        "slt"
                    } else {
                        "ult"
                    }
                }
                VectorCompareOp::Le => {
                    if lane.is_signed() {
                        "sle"
                    } else {
                        "ule"
                    }
                }
                VectorCompareOp::Gt => {
                    if lane.is_signed() {
                        "sgt"
                    } else {
                        "ugt"
                    }
                }
                VectorCompareOp::Ge => {
                    if lane.is_signed() {
                        "sge"
                    } else {
                        "uge"
                    }
                }
            }
        };
        let instruction = if lane.is_float() { "fcmp" } else { "icmp" };
        let mask = self.new_value();
        self.writer.line(format!(
            "{mask} = {instruction} {predicate} {}, {}",
            left.render(),
            right.value
        ));
        let extended = self.new_value();
        self.writer.line(format!(
            "{extended} = zext <{lanes} x i1> {mask} to <{lanes} x {lane_text}>"
        ));
        let negated = self.new_value();
        self.writer.line(format!(
            "{negated} = sub <{lanes} x {lane_text}> zeroinitializer, {extended}"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <{lanes} x {lane_text}> {negated} to {result_ty}"
        ));
        Ok(())
    }

    fn emit_vector_select(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        mask: &AmirOperand,
        if_true: &AmirOperand,
        if_false: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let mask_ty = ArType::Vector(VectorType::U8x16);
        let mask = self.operand(mask, &mask_ty)?;
        let if_true = self.operand(if_true, lhs_ty)?;
        let if_false = self.operand(if_false, lhs_ty)?;
        let mask_bytes = mask.value;
        let true_bytes = self.new_value();
        self.writer.line(format!(
            "{true_bytes} = bitcast {} to <16 x i8>",
            if_true.render()
        ));
        let false_bytes = self.new_value();
        self.writer.line(format!(
            "{false_bytes} = bitcast {} to <16 x i8>",
            if_false.render()
        ));
        let inverse = self.new_value();
        self.writer.line(format!(
            "{inverse} = xor <16 x i8> {mask_bytes}, <i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1>"
        ));
        let selected_true = self.new_value();
        self.writer.line(format!(
            "{selected_true} = and <16 x i8> {mask_bytes}, {true_bytes}"
        ));
        let selected_false = self.new_value();
        self.writer.line(format!(
            "{selected_false} = and <16 x i8> {inverse}, {false_bytes}"
        ));
        let combined = self.new_value();
        self.writer.line(format!(
            "{combined} = or <16 x i8> {selected_true}, {selected_false}"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <16 x i8> {combined} to {result_ty}"
        ));
        Ok(())
    }

    fn emit_vector_splat(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (_, lanes, lane) = self
            .module
            .vector_info(lhs_ty)
            .ok_or_else(|| ice(format!("vector splat requires a vector, got {lhs_ty:?}")))?;
        let lane_ty = ArType::Primitive(lane);
        let value = self.operand(value, &lane_ty)?;
        let vector = self.module.llvm_ty(lhs_ty)?;
        let inserted = self.new_value();
        self.writer.line(format!(
            "{inserted} = insertelement {vector} undef, {}, i32 0",
            value.render()
        ));
        self.writer.line(format!(
            "{} = shufflevector {vector} {inserted}, {vector} undef, <{lanes} x i32> zeroinitializer",
            result
        ));
        Ok(())
    }

    fn emit_vector_shuffle(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        left: &AmirOperand,
        right: &AmirOperand,
        mask: &[u8; 16],
    ) -> Result<(), Diagnostic> {
        let ty = self.module.operand_type(left, self.func);
        let left = self.operand(left, &ty)?;
        let right = self.operand(right, &ty)?;
        let left_bytes = self.new_value();
        self.writer.line(format!(
            "{left_bytes} = bitcast {} to <16 x i8>",
            left.render()
        ));
        let right_bytes = self.new_value();
        self.writer.line(format!(
            "{right_bytes} = bitcast {} to <16 x i8>",
            right.render()
        ));
        let mask = mask
            .iter()
            .map(|index| format!("i32 {index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let shuffled = self.new_value();
        self.writer.line(format!(
            "{shuffled} = shufflevector <16 x i8> {left_bytes}, <16 x i8> {right_bytes}, <16 x i32> <{mask}>"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <16 x i8> {shuffled} to {result_ty}"
        ));
        Ok(())
    }

    fn emit_call(
        &mut self,
        lhs: Option<TempId>,
        result: Option<&str>,
        callee: &AmirOperand,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let symbol = match callee {
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => *symbol,
            other => {
                return Err(ice(format!(
                    "LLVM backend only lowers direct calls, got {other:?}"
                )));
            }
        };
        if let Some(kind) = self.module.intrinsic_kind(symbol) {
            return self.emit_intrinsic(lhs, result, kind, args);
        }
        let (param_types, return_ty) = self.module.signature_for(symbol, args, self.func);
        if param_types.len() != args.len() {
            return Err(ice(format!(
                "call to {} expects {} arguments, got {}",
                self.module.function_name(symbol),
                param_types.len(),
                args.len()
            )));
        }
        // Extern/host calls use the flattened C ABI; Arandu-defined callees keep
        // first-class fat pointers as `{ ptr, i64 }` values.
        let flatten_host_abi = !self.module.is_internal(symbol);
        let mut rendered = Vec::with_capacity(args.len());
        for (argument, expected) in args.iter().zip(&param_types) {
            if self.module.is_void(expected) {
                continue;
            }
            if flatten_host_abi && self.module.is_fat_pointer(expected) {
                let fat = self.operand(argument, expected)?;
                let ptr = self.new_value();
                let len = self.new_value();
                self.writer
                    .line(format!("{ptr} = extractvalue {} {}, 0", fat.ty, fat.value));
                self.writer
                    .line(format!("{len} = extractvalue {} {}, 1", fat.ty, fat.value));
                rendered.push(format!("ptr {ptr}"));
                rendered.push(format!("i64 {len}"));
                continue;
            }
            rendered.push(self.operand(argument, expected)?.render());
        }
        let return_text = self.module.llvm_ty(&return_ty)?;
        let name = quote_name(&self.module.function_name(symbol));
        let call = format!("call {return_text} @{name}({})", rendered.join(", "));
        match lhs {
            Some(lhs) if !self.module.is_void(&return_ty) => {
                let Some(result) = result else {
                    return Err(ice("call destination has no LLVM name"));
                };
                let expected = self.temp_type(lhs);
                let expected_text = self.module.llvm_ty(&expected)?;
                if expected_text == return_text {
                    self.writer.line(format!("{result} = {call}"));
                    return Ok(());
                }
                let raw = self.new_value();
                self.writer.line(format!("{raw} = {call}"));
                let coerced = self.coerce(TypedValue::new(return_text, raw), &expected)?;
                let rendered = coerced.render();
                self.writer
                    .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
                Ok(())
            }
            Some(_) => Err(ice(format!(
                "call to {} has a void return but an assigned destination",
                self.module.function_name(symbol)
            ))),
            None => {
                self.writer.line(call);
                Ok(())
            }
        }
    }

    fn emit_intrinsic(
        &mut self,
        lhs: Option<TempId>,
        result: Option<&str>,
        kind: IntrinsicKind,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        match kind {
            IntrinsicKind::Atomic(op, width) => self.emit_atomic(result, op, width, args),
            IntrinsicKind::AtomicFence | IntrinsicKind::AtomicCompilerFence => {
                self.writer.line("fence seq_cst");
                Ok(())
            }
            IntrinsicKind::Abort => {
                self.writer.line("call void @llvm.trap()");
                Ok(())
            }
            IntrinsicKind::CountOnes
            | IntrinsicKind::LeadingZeros
            | IntrinsicKind::TrailingZeros
            | IntrinsicKind::ReverseBits => self.emit_bit_intrinsic(result, kind, args),
            IntrinsicKind::PtrRead => {
                let Some(lhs) = lhs else {
                    return Err(ice("ptrRead requires a destination"));
                };
                let ty = self.temp_type(lhs);
                let Some(result) = result else {
                    return Err(ice("ptrRead destination has no LLVM name"));
                };
                let pointer_ty = self.module.void_ptr_ty();
                let pointer = self.operand(&args[0], &pointer_ty)?;
                let text = self.module.llvm_ty(&ty)?;
                let align = self.module.align_of(&ty);
                self.writer.line(format!(
                    "{result} = load {text}, ptr {}, align {align}",
                    pointer.value
                ));
                Ok(())
            }
            IntrinsicKind::PtrWrite => {
                let ty = self.module.operand_type(&args[1], self.func);
                let pointer_ty = self.module.void_ptr_ty();
                let pointer = self.operand(&args[0], &pointer_ty)?;
                let value = self.operand(&args[1], &ty)?;
                let align = self.module.align_of(&ty);
                self.writer.line(format!(
                    "store {}, ptr {}, align {align}",
                    value.render(),
                    pointer.value
                ));
                Ok(())
            }
            IntrinsicKind::PtrOffset => {
                let Some(lhs) = lhs else {
                    return Err(ice("ptrOffset requires a destination"));
                };
                let target = self.temp_type(lhs);
                let Some(result) = result else {
                    return Err(ice("ptrOffset destination has no LLVM name"));
                };
                let pointee = match &target {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        Some(self.module.interner.resolve(*inner))
                    }
                    _ => None,
                };
                let stride = pointee.as_ref().map_or(1, |ty| {
                    self.module
                        .layout
                        .layout_of_type(ty, self.module.interner, self.module.info)
                        .map_or(1, |layout| layout.size.max(1))
                });
                let pointer_ty = self.module.void_ptr_ty();
                let pointer = self.operand(&args[0], &pointer_ty)?;
                let index_ty = self.module.operand_type(&args[1], self.func);
                let index = self.operand(&args[1], &index_ty)?;
                let i64_ty = self.module.i64_ty();
                let index = self.coerce(index, &i64_ty)?;
                let offset = self.new_value();
                self.writer
                    .line(format!("{offset} = mul i64 {}, {stride}", index.value));
                self.writer.line(format!(
                    "{result} = getelementptr i8, ptr {}, i64 {offset}",
                    pointer.value
                ));
                Ok(())
            }
            IntrinsicKind::AddressOf => {
                if lhs.is_none() {
                    return Ok(());
                }
                let Some(result) = result else {
                    return Err(ice("addressOf destination has no LLVM name"));
                };
                let ty = self.module.operand_type(&args[0], self.func);
                let value = self.operand(&args[0], &ty)?;
                self.writer.line(format!(
                    "{result} = select i1 true, {}, {}",
                    value.render(),
                    value.render()
                ));
                Ok(())
            }
            other => Err(ice(format!(
                "LLVM backend does not yet lower intrinsic {other:?}"
            ))),
        }
    }

    fn emit_atomic(
        &mut self,
        result: Option<&str>,
        op: AtomicOp,
        width: AtomicWidth,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        if args.len() != op.argument_count() {
            return Err(ice(format!(
                "atomic {op:?} expects {} arguments, got {}",
                op.argument_count(),
                args.len()
            )));
        }
        let ty_text = match width {
            AtomicWidth::U8 => "i8".to_owned(),
            AtomicWidth::U32 => "i32".to_owned(),
            AtomicWidth::U64 => "i64".to_owned(),
            AtomicWidth::Usize => self.module.llvm_ty(&self.module.usize_ty())?,
            AtomicWidth::Pointer => "ptr".to_owned(),
        };
        let value_ty = match width {
            AtomicWidth::U8 => ArType::Primitive(Primitive::U8),
            AtomicWidth::U32 => ArType::Primitive(Primitive::U32),
            AtomicWidth::U64 => ArType::Primitive(Primitive::U64),
            AtomicWidth::Usize => self.module.usize_ty(),
            AtomicWidth::Pointer => ArType::Ptr(self.module.interner.intern(ArType::Void)),
        };
        let align = match width {
            AtomicWidth::U8 => 1,
            AtomicWidth::U32 => 4,
            AtomicWidth::U64 => 8,
            AtomicWidth::Usize => self.module.pointer_align(),
            AtomicWidth::Pointer => self.module.pointer_align(),
        };
        let pointer_ty = self.module.void_ptr_ty();
        let pointer = self.operand(&args[0], &pointer_ty)?;
        let pointer_value = pointer.value;
        match op {
            AtomicOp::Load => {
                if let Some(result) = result {
                    self.writer.line(format!(
                        "{result} = load atomic {ty_text}, ptr {pointer_value} seq_cst, align {align}"
                    ));
                } else {
                    self.writer.line(format!(
                        "load atomic {ty_text}, ptr {pointer_value} seq_cst, align {align}"
                    ));
                }
            }
            AtomicOp::Store => {
                let value = self.operand(&args[1], &value_ty)?;
                self.writer.line(format!(
                    "store atomic {ty_text} {}, ptr {pointer_value} seq_cst, align {align}",
                    value.value
                ));
            }
            AtomicOp::CompareExchange => {
                let expected = self.operand(&args[1], &value_ty)?;
                let desired = self.operand(&args[2], &value_ty)?;
                let pair = self.new_value();
                self.writer.line(format!(
                    "{pair} = cmpxchg ptr {pointer_value}, {} {}, {} {} seq_cst seq_cst",
                    expected.ty, expected.value, desired.ty, desired.value
                ));
                if let Some(result) = result {
                    self.writer.line(format!(
                        "{result} = extractvalue {{ {ty_text}, i1 }} {pair}, 0"
                    ));
                }
            }
            AtomicOp::Swap
            | AtomicOp::Add
            | AtomicOp::Sub
            | AtomicOp::And
            | AtomicOp::Or
            | AtomicOp::Xor => {
                let value = self.operand(&args[1], &value_ty)?;
                let instruction = match op {
                    AtomicOp::Swap => "xchg",
                    AtomicOp::Add => "add",
                    AtomicOp::Sub => "sub",
                    AtomicOp::And => "and",
                    AtomicOp::Or => "or",
                    AtomicOp::Xor => "xor",
                    AtomicOp::Load | AtomicOp::Store | AtomicOp::CompareExchange => unreachable!(),
                };
                let text = format!(
                    "atomicrmw {instruction} ptr {pointer_value}, {} {} seq_cst",
                    value.ty, value.value
                );
                if let Some(result) = result {
                    self.writer.line(format!("{result} = {text}"));
                } else {
                    self.writer.line(text);
                }
            }
        }
        Ok(())
    }

    fn emit_bit_intrinsic(
        &mut self,
        result: Option<&str>,
        kind: IntrinsicKind,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let Some(argument) = args.first() else {
            return Err(ice(format!("bit intrinsic {kind:?} requires one argument")));
        };
        let ty = self.module.operand_type(argument, self.func);
        let width = self
            .module
            .integer_width(&ty)
            .ok_or_else(|| ice(format!("bit intrinsic requires an integer, got {ty:?}")))?;
        let argument = self.operand(argument, &ty)?;
        let name = match kind {
            IntrinsicKind::CountOnes => format!("llvm.ctpop.i{width}"),
            IntrinsicKind::LeadingZeros => format!("llvm.ctlz.i{width}"),
            IntrinsicKind::TrailingZeros => format!("llvm.cttz.i{width}"),
            IntrinsicKind::ReverseBits => format!("llvm.bitreverse.i{width}"),
            _ => return Err(ice(format!("unexpected bit intrinsic {kind:?}"))),
        };
        let extra = if matches!(
            kind,
            IntrinsicKind::LeadingZeros | IntrinsicKind::TrailingZeros
        ) {
            ", i1 false"
        } else {
            ""
        };
        let call = format!("call {} @{name}({}{extra})", argument.ty, argument.value);
        if let Some(result) = result {
            self.writer.line(format!("{result} = {call}"));
        } else {
            self.writer.line(call);
        }
        Ok(())
    }

    fn emit_terminator(&mut self, terminator: &AmirTerminator) -> Result<(), Diagnostic> {
        match terminator {
            AmirTerminator::Return => {
                let return_ty = self.module.interner.resolve(self.func.return_type);
                let function_name = self.module.function_name(self.func.symbol);
                if self.module.is_void(&return_ty) {
                    if function_name == "main" {
                        self.writer.line("ret i32 0");
                    } else {
                        self.writer.line("ret void");
                    }
                    return Ok(());
                }
                let Some(value) = ({
                    if let Some(slot) = self.temp_slot(TempId::from_usize(0)).map(str::to_owned) {
                        let ty_text = self.module.llvm_ty(&return_ty)?;
                        let align = self.module.align_of(&return_ty);
                        let loaded = self.new_value();
                        self.writer.line(format!(
                            "{loaded} = load {ty_text}, ptr {slot}, align {align}"
                        ));
                        Some(loaded)
                    } else {
                        self.return_value()
                    }
                }) else {
                    return Err(ice(
                        "AMIR return terminator has no preceding assignment to the return register",
                    ));
                };
                let ty_text = self.module.llvm_ty(&return_ty)?;
                self.writer.line(format!("ret {ty_text} {value}"));
                Ok(())
            }
            AmirTerminator::Goto { target, args } => {
                self.record_edge_args(*target, args)?;
                self.writer
                    .line(format!("br label %{}", block_label(*target)));
                Ok(())
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                let condition = self.operand(condition, &ArType::Primitive(Primitive::Bool))?;
                self.record_edge_args(*if_true, true_args)?;
                self.record_edge_args(*if_false, false_args)?;
                self.writer.line(format!(
                    "br i1 {}, label %{}, label %{}",
                    condition.value,
                    block_label(*if_true),
                    block_label(*if_false)
                ));
                Ok(())
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                let ty = self.module.operand_type(discriminant, self.func);
                let value = self.operand(discriminant, &ty)?;
                let mut cases = Vec::with_capacity(targets.len());
                for (case, target, args) in targets {
                    self.record_edge_args(*target, args)?;
                    cases.push(format!(
                        "{} {}, label %{}",
                        value.ty,
                        case,
                        block_label(*target)
                    ));
                }
                self.record_edge_args(otherwise.0, &otherwise.1)?;
                self.writer.line(format!(
                    "switch {} {}, label %{} [ {} ]",
                    value.ty,
                    value.value,
                    block_label(otherwise.0),
                    cases.join(" ")
                ));
                Ok(())
            }
            AmirTerminator::Unreachable => {
                self.writer.line("call void @llvm.trap()");
                self.writer.line("unreachable");
                Ok(())
            }
            AmirTerminator::Suspend { resume, args, .. } => {
                // Ready-only coroutines: suspension is a direct edge to the
                // resume block, matching the C backend's A3.1 lowering.
                self.record_edge_args(*resume, args)?;
                self.writer
                    .line(format!("br label %{}", block_label(*resume)));
                Ok(())
            }
        }
    }

    fn place(&mut self, place: &AmirPlace) -> Result<(String, ArType), Diagnostic> {
        let local = &self.func.locals[place.local.as_usize()];
        let mut address = local_name(local.id);
        let mut ty = self.module.interner.resolve(local.ty);
        for projection in &place.projections {
            match projection {
                AmirProjection::Deref => {
                    let pointee = match &ty {
                        ArType::Ptr(inner)
                        | ArType::Ref(inner)
                        | ArType::RefMut(inner)
                        | ArType::Nullable(inner) => self.module.interner.resolve(*inner),
                        other => {
                            return Err(ice(format!(
                                "deref projection requires a pointer, got {other:?}"
                            )));
                        }
                    };
                    let loaded = self.new_value();
                    self.writer.line(format!(
                        "{loaded} = load ptr, ptr {address}, align {}",
                        self.module.pointer_align()
                    ));
                    address = loaded;
                    ty = pointee;
                }
                AmirProjection::Field(symbol) => {
                    let (offset, field_ty) = self.field_projection(&ty, *symbol)?;
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = field_ty;
                }
                AmirProjection::TupleField(index) => {
                    let layout = self
                        .module
                        .layout
                        .layout_of_type(&ty, self.module.interner, self.module.info)
                        .map_err(|error| ice(format!("LLVM layout failed for {ty:?}: {error}")))?;
                    let offset = layout.field_offsets.get(*index).copied().ok_or_else(|| {
                        ice(format!("tuple field {index} is out of range for {ty:?}"))
                    })?;
                    let ArType::Tuple(args) = &ty else {
                        return Err(ice(format!(
                            "tuple projection requires a tuple, got {ty:?}"
                        )));
                    };
                    let field_ty = self
                        .module
                        .interner
                        .resolve(self.module.interner.type_args(*args)[*index]);
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = field_ty;
                }
                AmirProjection::Payload {
                    index,
                    field_ty,
                    tuple_ty,
                    ..
                } => {
                    // Result/Option/enum layout: field_offsets[0] = tag,
                    // field_offsets[1] = payload. `index` selects a field
                    // inside the payload tuple when `tuple_ty` is set — never
                    // the tag slot. Matches Cranelift/Wasm place lowering.
                    let owner_ty = match &ty {
                        ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                            self.module.interner.resolve(*inner)
                        }
                        other => other.clone(),
                    };
                    let layout = self
                        .module
                        .layout
                        .layout_of_type(&owner_ty, self.module.interner, self.module.info)
                        .map_err(|error| {
                            ice(format!("LLVM layout failed for {owner_ty:?}: {error}"))
                        })?;
                    let mut offset = layout.field_offsets.get(1).copied().unwrap_or(0);
                    if let Some(tuple_ty) = tuple_ty {
                        let payload_ty = self.module.interner.resolve(*tuple_ty);
                        let payload_layout = self
                            .module
                            .layout
                            .layout_of_type(&payload_ty, self.module.interner, self.module.info)
                            .map_err(|error| {
                                ice(format!("LLVM layout failed for {payload_ty:?}: {error}"))
                            })?;
                        offset = offset.saturating_add(
                            payload_layout
                                .field_offsets
                                .get(*index)
                                .copied()
                                .unwrap_or(0),
                        );
                    }
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = self.module.interner.resolve(*field_ty);
                }
                AmirProjection::IndexConstant(index) => {
                    let element = match &ty {
                        ArType::Array(_, element) => self.module.interner.resolve(*element),
                        ArType::Tuple(args) => self
                            .module
                            .interner
                            .resolve(self.module.interner.type_args(*args)[*index]),
                        other => {
                            return Err(ice(format!(
                                "index projection requires an array or tuple, got {other:?}"
                            )));
                        }
                    };
                    let layout = self
                        .module
                        .layout
                        .layout_of_type(&ty, self.module.interner, self.module.info)
                        .map_err(|error| ice(format!("LLVM layout failed for {ty:?}: {error}")))?;
                    let offset = layout
                        .field_offsets
                        .get(*index)
                        .copied()
                        .unwrap_or_else(|| {
                            self.module
                                .layout
                                .layout_of_type(&element, self.module.interner, self.module.info)
                                .map_or(0, |layout| layout.size * *index as u64)
                        });
                    if offset != 0 {
                        let next = self.new_value();
                        self.writer.line(format!(
                            "{next} = getelementptr inbounds i8, ptr {address}, i64 {offset}"
                        ));
                        address = next;
                    }
                    ty = element;
                }
                AmirProjection::Index(index) => {
                    let (base_ptr, element) = match &ty {
                        ArType::Array(_, element) => {
                            (address.clone(), self.module.interner.resolve(*element))
                        }
                        ArType::Slice(element) | ArType::MutSlice(element) => {
                            // `address` points at a fat `{data,len}` descriptor.
                            let element = self.module.interner.resolve(*element);
                            let data = self.new_value();
                            let align = self.module.pointer_align();
                            self.writer
                                .line(format!("{data} = load ptr, ptr {address}, align {align}"));
                            (data, element)
                        }
                        ArType::Primitive(Primitive::Str) => {
                            let data = self.new_value();
                            let align = self.module.pointer_align();
                            self.writer
                                .line(format!("{data} = load ptr, ptr {address}, align {align}"));
                            (data, ArType::Primitive(Primitive::U8))
                        }
                        other => {
                            return Err(ice(format!(
                                "dynamic index projection requires an array or slice, got {other:?}"
                            )));
                        }
                    };
                    let stride = self
                        .module
                        .layout
                        .layout_of_type(&element, self.module.interner, self.module.info)
                        .map_err(|error| {
                            ice(format!("LLVM layout failed for {element:?}: {error}"))
                        })?
                        .size;
                    let index_ty = self.module.operand_type(index, self.func);
                    let index = self.operand(index, &index_ty)?;
                    let index = self.coerce(index, &self.module.i64_ty())?;
                    let offset = self.new_value();
                    self.writer
                        .line(format!("{offset} = mul i64 {}, {stride}", index.value));
                    let next = self.new_value();
                    self.writer.line(format!(
                        "{next} = getelementptr inbounds i8, ptr {base_ptr}, i64 {offset}"
                    ));
                    address = next;
                    ty = element;
                }
                AmirProjection::Variant(_) => {}
            }
        }
        Ok((address, ty))
    }

    fn field_projection(
        &self,
        base: &ArType,
        field: SymbolId,
    ) -> Result<(u64, ArType), Diagnostic> {
        let fields = self
            .module
            .info
            .get_struct_fields_for_type(base, self.module.interner)
            .ok_or_else(|| {
                eprintln!(
                    "LLVM field projection missing fields: base={base:?} field={field:?} name={:?}",
                    self.module.symbols.try_get(field).map(|s| s.name.as_str())
                );
                ice(format!("no struct fields for {base:?}"))
            })?;
        let field_symbol = self.module.symbols.try_get(field);
        let field_name = field_symbol.map(|symbol| symbol.name.as_str());
        let info = fields
            .iter()
            .find(|candidate| {
                candidate.symbol == Some(field)
                    || field_name.is_some_and(|name| candidate.name.as_str() == name)
            })
            .ok_or_else(|| ice(format!("field {field:?} is not present in {base:?}")))?;
        let layout = self
            .module
            .layout
            .layout_of_type(base, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {base:?}: {error}")))?;
        let offset = layout
            .field_offsets
            .get(info.index)
            .copied()
            .ok_or_else(|| {
                ice(format!(
                    "field index {} is out of range for {base:?}",
                    info.index
                ))
            })?;
        Ok((offset, self.module.interner.resolve(info.ty)))
    }

    fn operand(
        &mut self,
        operand: &AmirOperand,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        match operand {
            AmirOperand::Constant(constant) => self.constant(constant, expected),
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                let actual_ty = self.temp_type(*temp);
                let override_ty = self.temp_llvm_ty[temp.as_usize()].clone();
                if let Some(slot) = self.temp_slot(*temp).map(str::to_owned) {
                    let ty_text = override_ty
                        .clone()
                        .map_or_else(|| self.module.llvm_ty(&actual_ty), Ok)?;
                    let align = self.module.align_of(&actual_ty);
                    let loaded = self.new_value();
                    self.writer.line(format!(
                        "{loaded} = load {ty_text}, ptr {slot}, align {align}"
                    ));
                    let mut loaded_value = TypedValue::new(ty_text, loaded);
                    if self.module.integer_width(&actual_ty).is_some() {
                        loaded_value =
                            loaded_value.with_source_signed(self.module.is_signed(&actual_ty));
                    }
                    return self.coerce(loaded_value, expected);
                }
                let value = if temp.as_usize() == 0 {
                    self.return_value()
                        .ok_or_else(|| ice("return register is read before it is assigned"))?
                } else {
                    self.ensure_defined(*temp)?;
                    temp_name(*temp)
                };
                let llvm_ty = override_ty.map_or_else(|| self.module.llvm_ty(&actual_ty), Ok)?;
                let mut actual = TypedValue::new(llvm_ty, value);
                // Prefer AMIR temp signedness: `u8 as u32` must zext, not sext.
                if self.module.integer_width(&actual_ty).is_some() {
                    actual = actual.with_source_signed(self.module.is_signed(&actual_ty));
                }
                self.coerce(actual, expected)
            }
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => {
                let value = TypedValue::new(
                    "ptr".to_owned(),
                    format!("@{}", quote_name(&self.module.function_name(*symbol))),
                );
                self.coerce(value, expected)
            }
        }
    }

    fn constant(
        &self,
        constant: &AmirConstant,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        let expected_text = self.module.llvm_ty(expected)?;
        match constant {
            AmirConstant::Bool(value) => {
                // `true`/`false` are i1 tokens; integer projections (ToStr →
                // `ar_jit_bool_to_str(i8, ...)`) need numeric 0/1 literals.
                let literal = if expected_text == "i1" {
                    if *value { "true" } else { "false" }
                } else if *value {
                    "1"
                } else {
                    "0"
                };
                Ok(TypedValue::new(expected_text, literal.to_owned()))
            }
            AmirConstant::Nil => {
                if expected_text == "ptr" {
                    Ok(TypedValue::new("ptr".to_owned(), "null".to_owned()))
                } else {
                    Ok(TypedValue::new(expected_text, "0".to_owned()))
                }
            }
            AmirConstant::Pool(id) => self.literal(*id, expected, &expected_text),
        }
    }

    fn literal(
        &self,
        id: LiteralId,
        expected: &ArType,
        expected_text: &str,
    ) -> Result<TypedValue, Diagnostic> {
        use arandu_middle::literal_pool::AmirLiteralEntry;
        match self.module.program.literal_pool.get(id) {
            AmirLiteralEntry::Int(text) => {
                let value = arandu_middle::literal_pool::parse_int_literal(text)
                    .ok_or_else(|| ice(format!("invalid integer literal {text:?}")))?;
                let width = self
                    .module
                    .integer_width(expected)
                    .or_else(|| {
                        if expected_text == "ptr" {
                            Some(self.module.layout.pointer_width())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(32);
                let masked = if width >= 128 {
                    value as u128
                } else {
                    (value as u128) & ((1u128 << width) - 1)
                };
                let rendered = if expected_text == "float" || expected_text == "double" {
                    format!("{masked}.0")
                } else {
                    masked.to_string()
                };
                Ok(TypedValue::new(expected_text.to_owned(), rendered))
            }
            AmirLiteralEntry::Float(text) => {
                let value = arandu_middle::literal_pool::parse_float_literal(text)
                    .ok_or_else(|| ice(format!("invalid float literal {text:?}")))?;
                let rendered = if expected_text == "float" {
                    format!("{:?}", value as f32)
                } else {
                    format!("{value:?}")
                };
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    ensure_float_decimal(&rendered),
                ))
            }
            AmirLiteralEntry::FloatBits(value) => {
                let rendered = if value.ty().bit_width() == 32 {
                    format!("{:?}", f32::from_bits(value.bits() as u32))
                } else {
                    format!("{:?}", f64::from_bits(value.bits()))
                };
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    ensure_float_decimal(&rendered),
                ))
            }
            AmirLiteralEntry::Char(text) => {
                let ch = parse_char_literal(text)
                    .ok_or_else(|| ice(format!("invalid char literal {text:?}")))?;
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    u32::from(ch).to_string(),
                ))
            }
            AmirLiteralEntry::Str(_) => {
                let index = id.0;
                let (name, length) = self
                    .module
                    .string_globals
                    .get(&index)
                    .ok_or_else(|| ice("string literal has no LLVM global"))?;
                if expected_text == "ptr" {
                    Ok(TypedValue::new(
                        "ptr",
                        format!(
                            "getelementptr inbounds ([{} x i8], ptr {name}, i64 0, i64 0)",
                            length + 1
                        ),
                    ))
                } else if expected_text == "{ ptr, i64 }" {
                    Ok(TypedValue::new(
                        expected_text.to_owned(),
                        format!("{{ ptr {name}, i64 {length} }}"),
                    ))
                } else if self.module.is_pointer(expected) {
                    Ok(TypedValue::new(
                        "ptr",
                        format!(
                            "getelementptr inbounds ([{} x i8], ptr {name}, i64 0, i64 0)",
                            length + 1
                        ),
                    ))
                } else {
                    Err(ice(format!(
                        "LLVM backend cannot use a string literal as {expected:?}"
                    )))
                }
            }
        }
    }

    fn coerce(&mut self, value: TypedValue, expected: &ArType) -> Result<TypedValue, Diagnostic> {
        let expected_text = self.module.llvm_ty(expected)?;
        if value.ty == expected_text {
            return Ok(TypedValue::new(expected_text, value.value));
        }
        let actual_width = self.module.integer_width_from_llvm(&value.ty);
        let expected_width = self.module.integer_width(expected);
        let actual_float = value.ty == "float" || value.ty == "double";
        let expected_float = self.module.is_float(expected);

        let instruction = if value.ty == "ptr" && expected_text == "ptr" {
            None
        } else if value.ty == "ptr" {
            if expected_width.is_some() {
                Some(format!("ptrtoint ptr {} to {expected_text}", value.value))
            } else if expected_text == "{ ptr, i64 }" || self.module.is_fat_pointer(expected) {
                // Load a fat `{data,len}` descriptor from behind the pointer
                // (e.g. `ref String` / `ref str` → by-value `str` / `[]T`).
                let loaded = self.new_value();
                let align = self.module.pointer_align();
                self.writer.line(format!(
                    "{loaded} = load {{ ptr, i64 }}, ptr {}, align {align}",
                    value.value
                ));
                return Ok(TypedValue::new("{ ptr, i64 }".to_owned(), loaded));
            } else if let Some(bytes) = parse_byte_aggregate_size(&expected_text) {
                // Pointer-repr aggregate (Cranelift-style EnumPayload of a
                // Named struct): materialize by copying `bytes` from the
                // pointee into a first-class opaque value.
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {expected_text}, align 1"));
                self.writer.line(format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {slot}, ptr {}, i64 {bytes}, i1 false)",
                    value.value
                ));
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {expected_text}, ptr {slot}, align 1"
                ));
                return Ok(TypedValue::new(expected_text, loaded));
            } else {
                return Err(ice(format!("cannot cast LLVM pointer to {expected_text}")));
            }
        } else if expected_text == "ptr" {
            if actual_width.is_some() {
                Some(format!("inttoptr {} {} to ptr", value.ty, value.value))
            } else if let Some(bytes) = parse_byte_aggregate_size(&value.ty) {
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {}, align 1", value.ty));
                self.writer.line(format!(
                    "store {} {}, ptr {slot}, align 1",
                    value.ty, value.value
                ));
                // Pointer-width opaque payloads (e.g. `Result.Ok(ptr)`) store a
                // pointer *value* as `[W x i8]`. Reinterpret those bytes as
                // `ptr`. Larger/smaller aggregates decay to the alloca address.
                if bytes == self.module.layout.pointer_width() {
                    let loaded = self.new_value();
                    self.writer.line(format!(
                        "{loaded} = load ptr, ptr {slot}, align {}",
                        self.module.pointer_align()
                    ));
                    return Ok(TypedValue::new("ptr".to_owned(), loaded));
                }
                return Ok(TypedValue::new("ptr".to_owned(), slot));
            } else if value.ty.starts_with('[') {
                // Non-byte array → pointer: spill and decay to the alloca.
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {}, align 1", value.ty));
                self.writer.line(format!(
                    "store {} {}, ptr {slot}, align 1",
                    value.ty, value.value
                ));
                return Ok(TypedValue::new("ptr".to_owned(), slot));
            } else {
                return Err(ice(format!("cannot cast {} to LLVM pointer", value.ty)));
            }
        } else if let (Some(from_bytes), Some(to_bytes)) = (
            parse_byte_aggregate_size(&value.ty),
            parse_byte_aggregate_size(&expected_text),
        ) {
            // Opaque layout aggregates (`[N x i8]`) of different sizes are
            // rewritten through memory: zero the destination, copy the overlap.
            let src = self.new_value();
            self.writer
                .line(format!("{src} = alloca {}, align 1", value.ty));
            self.writer.line(format!(
                "store {} {}, ptr {src}, align 1",
                value.ty, value.value
            ));
            let dst = self.new_value();
            self.writer
                .line(format!("{dst} = alloca {expected_text}, align 1"));
            self.writer.line(format!(
                "store {expected_text} zeroinitializer, ptr {dst}, align 1"
            ));
            let copy = from_bytes.min(to_bytes);
            if copy > 0 {
                self.writer.line(format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {copy}, i1 false)"
                ));
            }
            let loaded = self.new_value();
            self.writer.line(format!(
                "{loaded} = load {expected_text}, ptr {dst}, align 1"
            ));
            return Ok(TypedValue::new(expected_text, loaded));
        } else if actual_float && expected_float {
            if value.ty == expected_text {
                None
            } else if value.ty == "float" {
                Some(format!("fpext float {} to double", value.value))
            } else {
                Some(format!("fptrunc double {} to float", value.value))
            }
        } else if actual_float && expected_width.is_some() {
            let op = if self.module.is_signed(expected) {
                "fptosi"
            } else {
                "fptoui"
            };
            Some(format!(
                "{op} {} {} to {expected_text}",
                value.ty, value.value
            ))
        } else if expected_float {
            let Some(width) = actual_width else {
                return Err(ice(format!("cannot cast {} to {expected_text}", value.ty)));
            };
            let source_signed = value
                .source_signed
                .unwrap_or_else(|| self.module.is_signed(&value_ar_type(&value.ty)));
            let op = if width == 1 || !source_signed {
                "uitofp"
            } else {
                "sitofp"
            };
            Some(format!(
                "{op} {} {} to {expected_text}",
                value.ty, value.value
            ))
        } else if let (Some(from), Some(to)) = (actual_width, expected_width) {
            if from == to {
                None
            } else if from == 1 {
                Some(format!(
                    "zext {} {} to {expected_text}",
                    value.ty, value.value
                ))
            } else if from < to {
                // Use AMIR source signedness when known. Never infer from the
                // LLVM type string alone (`i8` is shared by `i8` and `u8`).
                let source_signed = value
                    .source_signed
                    .unwrap_or_else(|| self.module.is_signed(&value_ar_type(&value.ty)));
                let op = if source_signed { "sext" } else { "zext" };
                Some(format!(
                    "{op} {} {} to {expected_text}",
                    value.ty, value.value
                ))
            } else {
                Some(format!(
                    "trunc {} {} to {expected_text}",
                    value.ty, value.value
                ))
            }
        } else {
            return Err(ice(format!(
                "cannot cast LLVM value of type {} to {expected_text}",
                value.ty
            )));
        };
        let Some(instruction) = instruction else {
            return Ok(TypedValue::new(expected_text, value.value));
        };
        let result = self.new_value();
        self.writer.line(format!("{result} = {instruction}"));
        Ok(TypedValue::new(expected_text, result))
    }

    fn assign_coerced(
        &mut self,
        result: &str,
        value: TypedValue,
        lhs_ty: &ArType,
    ) -> Result<(), Diagnostic> {
        let value = self.coerce(value, lhs_ty)?;
        let rendered = value.render();
        self.writer
            .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
        Ok(())
    }

    /// Build a typed LLVM value, recording AMIR integer signedness for casts.
    fn typed_from_ar(&self, llvm_ty: String, value: String, ar: &ArType) -> TypedValue {
        let mut typed = TypedValue::new(llvm_ty, value);
        if self.module.integer_width(ar).is_some() {
            typed = typed.with_source_signed(self.module.is_signed(ar));
        }
        typed
    }

    fn scratch_alloca(&mut self, ty: &ArType) -> Result<String, Diagnostic> {
        let (text, align) = self.module.alloca_type(ty)?;
        let slot = self.new_value();
        self.writer
            .line(format!("{slot} = alloca {text}, align {align}"));
        Ok(slot)
    }

    fn byte_gep(&mut self, base: &str, offset: u64) -> String {
        if offset == 0 {
            return base.to_owned();
        }
        let next = self.new_value();
        self.writer.line(format!(
            "{next} = getelementptr inbounds i8, ptr {base}, i64 {offset}"
        ));
        next
    }

    fn aggregate_address(
        &mut self,
        operand: &AmirOperand,
        ty: &ArType,
    ) -> Result<String, Diagnostic> {
        if self.module.is_pointer(ty) {
            let value = self.operand(operand, ty)?;
            return Ok(value.value);
        }
        let value = self.operand(operand, ty)?;
        let slot = self.scratch_alloca(ty)?;
        self.store_value(&slot, &value, ty)?;
        Ok(slot)
    }

    fn store_value(
        &mut self,
        address: &str,
        value: &TypedValue,
        ty: &ArType,
    ) -> Result<(), Diagnostic> {
        let align = self.module.align_of(ty);
        self.writer.line(format!(
            "store {}, ptr {address}, align {align}",
            value.render()
        ));
        Ok(())
    }

    fn field_type_by_index(&self, owner: &ArType, index: usize) -> Result<ArType, Diagnostic> {
        let Some(fields) = self
            .module
            .info
            .get_struct_fields_for_type(owner, self.module.interner)
        else {
            return match owner {
                ArType::Tuple(args) => self
                    .module
                    .interner
                    .type_args(*args)
                    .get(index)
                    .copied()
                    .map(|id| self.module.interner.resolve(id))
                    .ok_or_else(|| ice(format!("tuple field index {index} is outside {owner:?}"))),
                ArType::Result(ok, err) => {
                    Ok(self
                        .module
                        .interner
                        .resolve(if index == 0 { *ok } else { *err }))
                }
                ArType::Option(inner) | ArType::Poll(inner) => {
                    Ok(self.module.interner.resolve(*inner))
                }
                other => Err(ice(format!("no struct fields for {other:?}"))),
            };
        };
        let field = fields
            .iter()
            .find(|candidate| candidate.index == index)
            .ok_or_else(|| ice(format!("field index {index} is not present in {owner:?}")))?;
        arandu_middle::layout::instantiated_field_type(
            owner,
            field.name.as_str(),
            self.module.interner,
            self.module.info,
        )
        .map(|id| self.module.interner.resolve(id))
        .ok_or_else(|| ice(format!("cannot instantiate field {index} of {owner:?}")))
    }

    fn aggregate_field_address(
        &mut self,
        owner: &ArType,
        base: &str,
        index: usize,
    ) -> Result<(String, ArType), Diagnostic> {
        let layout = self
            .module
            .layout
            .layout_of_type(owner, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {owner:?}: {error}")))?;
        let offset = layout
            .field_offsets
            .get(index)
            .copied()
            .ok_or_else(|| ice(format!("field index {index} is outside {owner:?}")))?;
        let field_ty = self.field_type_by_index(owner, index)?;
        Ok((self.byte_gep(base, offset), field_ty))
    }

    fn enum_payload_type(
        &self,
        owner: &ArType,
        variant_tag: usize,
    ) -> Result<Option<ArType>, Diagnostic> {
        let payload = match owner {
            ArType::Named(..) => arandu_middle::layout::instantiated_enum_variant_payload_type(
                owner,
                variant_tag,
                self.module.interner,
                self.module.info,
            )
            .map(|id| self.module.interner.resolve(id)),
            ArType::Result(ok, err) => Some(if variant_tag == 0 {
                self.module.interner.resolve(*ok)
            } else {
                self.module.interner.resolve(*err)
            }),
            ArType::Option(inner) if variant_tag == 1 => Some(self.module.interner.resolve(*inner)),
            ArType::Poll(inner) if variant_tag == 0 => Some(self.module.interner.resolve(*inner)),
            _ => None,
        };
        Ok(payload)
    }

    fn store_enum_integer(
        &mut self,
        base: &str,
        offset: u64,
        width: u64,
        value: u64,
    ) -> Result<(), Diagnostic> {
        let (ty, align) = match width {
            1 => ("i8", 1),
            2 => ("i16", 2),
            4 => ("i32", 4),
            8 => ("i64", 8),
            other => {
                return Err(ice(format!(
                    "LLVM enum tag width {other} is not a power-of-two byte width"
                )));
            }
        };
        let address = self.byte_gep(base, offset);
        self.writer
            .line(format!("store {ty} {value}, ptr {address}, align {align}"));
        Ok(())
    }

    fn emit_enum_construct(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        variant_tag: usize,
        payload: Option<&AmirOperand>,
    ) -> Result<(), Diagnostic> {
        let layout = self
            .module
            .layout
            .layout_of_type(lhs_ty, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {lhs_ty:?}: {error}")))?;
        let slot = self.scratch_alloca(lhs_ty)?;
        let aggregate = self.module.llvm_ty(lhs_ty)?;
        let align = self.module.align_of(lhs_ty);
        self.writer.line(format!(
            "store {aggregate} zeroinitializer, ptr {slot}, align {align}"
        ));
        match layout.tag_encoding {
            Some(TagEncoding::PointerTag {
                tag_mask,
                pointer_offset,
                ..
            }) => {
                let pointer_width = self.module.layout.pointer_width();
                let tag = (variant_tag as u64) & tag_mask;
                let stored = if let Some(payload) = payload {
                    let payload_ty = self.module.operand_type(payload, self.func);
                    if !self.module.is_pointer(&payload_ty) {
                        return Err(ice(format!(
                            "pointer-tagged enum payload must be a pointer, got {payload_ty:?}"
                        )));
                    }
                    let value = self.operand(payload, &payload_ty)?;
                    let raw = self.new_value();
                    let width = if pointer_width == 32 { "i32" } else { "i64" };
                    self.writer
                        .line(format!("{raw} = ptrtoint ptr {} to {width}", value.value));
                    let clean = self.new_value();
                    self.writer.line(format!(
                        "{clean} = and {width} {raw}, {}",
                        !(tag_mask as i128)
                    ));
                    let tagged = self.new_value();
                    self.writer
                        .line(format!("{tagged} = or {width} {clean}, {tag}"));
                    tagged
                } else {
                    let width = if pointer_width == 32 { "i32" } else { "i64" };
                    let value = self.new_value();
                    self.writer.line(format!("{value} = add {width} 0, {tag}"));
                    value
                };
                let address = self.byte_gep(&slot, pointer_offset);
                let width = if pointer_width == 32 { 4 } else { 8 };
                self.writer.line(format!(
                    "store i{width} {stored}, ptr {address}, align {width}"
                ));
            }
            Some(TagEncoding::Niche {
                niche_offset,
                niche_size,
                niche_value,
                tagged_variant,
                ..
            }) => {
                if variant_tag == tagged_variant {
                    let width = if niche_size == 0 {
                        self.module.layout.pointer_width() / 8
                    } else {
                        niche_size
                    };
                    self.store_enum_integer(&slot, niche_offset, width, niche_value)?;
                } else if let Some(payload) = payload {
                    let payload_ty = self
                        .enum_payload_type(lhs_ty, variant_tag)?
                        .unwrap_or_else(|| self.module.operand_type(payload, self.func));
                    if !matches!(payload_ty, ArType::Void | ArType::Error) {
                        let value = self.operand(payload, &payload_ty)?;
                        self.store_value(&slot, &value, &payload_ty)?;
                    }
                }
            }
            Some(TagEncoding::Direct {
                tag_size,
                payload_offset,
            }) => {
                self.store_enum_integer(&slot, 0, tag_size, variant_tag as u64)?;
                if let Some(payload) = payload {
                    let payload_ty = self
                        .enum_payload_type(lhs_ty, variant_tag)?
                        .unwrap_or_else(|| self.module.operand_type(payload, self.func));
                    if !matches!(payload_ty, ArType::Void | ArType::Error) {
                        let value = self.operand(payload, &payload_ty)?;
                        let address = self.byte_gep(&slot, payload_offset);
                        self.store_value(&address, &value, &payload_ty)?;
                    }
                }
            }
            None => {
                return Err(ice(format!(
                    "enum construction requires tag encoding metadata, got none for {lhs_ty:?}"
                )));
            }
        }
        self.writer.line(format!(
            "{result} = load {aggregate}, ptr {slot}, align {align}"
        ));
        Ok(())
    }

    fn enum_storage(&mut self, value: &AmirOperand) -> Result<(String, ArType), Diagnostic> {
        let base_ty = self.module.operand_type(value, self.func);
        match &base_ty {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                let owner = self.module.interner.resolve(*inner);
                let pointer = self.operand(value, &base_ty)?;
                Ok((pointer.value, owner))
            }
            _ => {
                let address = self.aggregate_address(value, &base_ty)?;
                Ok((address, base_ty))
            }
        }
    }

    fn emit_discriminant(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (address, owner) = self.enum_storage(value)?;
        let layout = self
            .module
            .layout
            .layout_of_type(&owner, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {owner:?}: {error}")))?;
        let raw = match layout.tag_encoding {
            Some(TagEncoding::PointerTag {
                tag_mask,
                pointer_offset,
                ..
            }) => {
                let width = self.module.layout.pointer_width();
                let loaded = self.new_value();
                let tag_address = self.byte_gep(&address, pointer_offset);
                self.writer.line(format!(
                    "{loaded} = load i{width}, ptr {tag_address}, align {}",
                    width / 8
                ));
                let masked = self.new_value();
                self.writer
                    .line(format!("{masked} = and i{width} {loaded}, {}", tag_mask));
                TypedValue::new(format!("i{width}"), masked)
            }
            Some(TagEncoding::Niche {
                niche_offset,
                niche_size,
                niche_value,
                untagged_variant,
                tagged_variant,
            }) => {
                let width = if niche_size == 0 {
                    self.module.layout.pointer_width()
                } else {
                    niche_size * 8
                };
                let loaded = self.new_value();
                let niche_address = self.byte_gep(&address, niche_offset);
                self.writer.line(format!(
                    "{loaded} = load i{width}, ptr {niche_address}, align {}",
                    (width / 8).max(1)
                ));
                let is_niche = self.new_value();
                self.writer.line(format!(
                    "{is_niche} = icmp eq i{width} {loaded}, {niche_value}"
                ));
                let selected = self.new_value();
                self.writer.line(format!(
                    "{selected} = select i1 {is_niche}, i{width} {tagged_variant}, i{width} {untagged_variant}"
                ));
                TypedValue::new(format!("i{width}"), selected)
            }
            Some(TagEncoding::Direct { tag_size, .. }) => {
                let width = tag_size * 8;
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load i{width}, ptr {address}, align {}",
                    tag_size.max(1)
                ));
                TypedValue::new(format!("i{width}"), loaded)
            }
            None => {
                return Err(ice(format!(
                    "discriminant requires tag encoding metadata, got none for {owner:?}"
                )));
            }
        };
        self.assign_coerced(result, raw, lhs_ty)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_enum_payload(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
        field_ty: arandu_middle::types::TypeId,
        tuple_ty: Option<arandu_middle::types::TypeId>,
        index: usize,
        lhs: Option<TempId>,
    ) -> Result<(), Diagnostic> {
        let (address, owner) = self.enum_storage(value)?;
        let layout = self
            .module
            .layout
            .layout_of_type(&owner, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {owner:?}: {error}")))?;
        let payload_ty = self.module.interner.resolve(field_ty);
        if let Some(TagEncoding::PointerTag {
            tag_mask,
            pointer_offset,
            ..
        }) = layout.tag_encoding
        {
            let width = self.module.layout.pointer_width();
            let loaded = self.new_value();
            let tag_address = self.byte_gep(&address, pointer_offset);
            self.writer.line(format!(
                "{loaded} = load i{width}, ptr {tag_address}, align {}",
                width / 8
            ));
            let clean = self.new_value();
            self.writer.line(format!(
                "{clean} = and i{width} {loaded}, {}",
                !(tag_mask as i128) as u64
            ));
            return self.assign_coerced(
                result,
                TypedValue::new(format!("i{width}"), clean),
                &payload_ty,
            );
        }
        let tuple_offset = if let Some(tuple_ty) = tuple_ty {
            let payload_ty = self.module.interner.resolve(tuple_ty);
            let payload_layout = self
                .module
                .layout
                .layout_of_type(&payload_ty, self.module.interner, self.module.info)
                .map_err(|error| ice(format!("LLVM layout failed for {payload_ty:?}: {error}")))?;
            payload_layout
                .field_offsets
                .get(index)
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        let base_offset = layout.field_offsets.get(1).copied().unwrap_or(0);
        let offset = base_offset + tuple_offset;
        let payload_address = self.byte_gep(&address, offset);
        // Scalars / pointers load by value. Everything else uses Cranelift's
        // pointer-repr into the enum payload so a later coerce can copy the
        // full destination layout (e.g. thin AMIR projection → BumpArena).
        if self.module.is_pointer(lhs_ty) {
            let align = self.module.pointer_align();
            let loaded = self.new_value();
            self.writer.line(format!(
                "{loaded} = load ptr, ptr {payload_address}, align {align}"
            ));
            return self.assign_coerced(result, TypedValue::new("ptr".to_owned(), loaded), lhs_ty);
        }
        if self.module.integer_width(lhs_ty).is_some()
            || self.module.is_float(lhs_ty)
            || matches!(lhs_ty, ArType::Primitive(Primitive::Bool))
        {
            let payload_text = self.module.llvm_ty(lhs_ty)?;
            let align = self.module.align_of(lhs_ty);
            let loaded = self.new_value();
            self.writer.line(format!(
                "{loaded} = load {payload_text}, ptr {payload_address}, align {align}"
            ));
            return self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty);
        }
        self.writer.line("; enum_payload_ptr_repr");
        self.writer.line(format!(
            "{result} = getelementptr i8, ptr {payload_address}, i64 0"
        ));
        if let Some(temp) = lhs {
            self.temp_llvm_ty[temp.as_usize()] = Some("ptr".to_owned());
        }
        Ok(())
    }

    fn emit_to_str(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
        src_ty: arandu_middle::types::TypeId,
    ) -> Result<(), Diagnostic> {
        let src = self.module.interner.resolve(src_ty);
        if matches!(src, ArType::Primitive(Primitive::Str)) {
            let value = self.operand(value, lhs_ty)?;
            return self.assign_coerced(result, value, lhs_ty);
        }
        let (helper, argument_ty) = match &src {
            ArType::Primitive(Primitive::Bool) => {
                ("ar_jit_bool_to_str", ArType::Primitive(Primitive::U8))
            }
            ArType::Primitive(Primitive::Char) => {
                ("ar_jit_char_to_str", ArType::Primitive(Primitive::U32))
            }
            ArType::FloatLiteral => ("ar_jit_f64_to_str", ArType::Primitive(Primitive::F64)),
            ArType::Primitive(p) if p.is_float() => {
                ("ar_jit_f64_to_str", ArType::Primitive(Primitive::F64))
            }
            ArType::IntLiteral => ("ar_jit_i64_to_str", ArType::Primitive(Primitive::I64)),
            ArType::Primitive(p) if p.is_integer() && p.is_signed() => {
                ("ar_jit_i64_to_str", ArType::Primitive(Primitive::I64))
            }
            ArType::Primitive(p) if p.is_integer() => {
                ("ar_jit_u64_to_str", ArType::Primitive(Primitive::U64))
            }
            ArType::Err => (
                "ar_jit_err_to_str",
                ArType::Ptr(self.module.interner.intern(ArType::Void)),
            ),
            other => {
                return Err(ice(format!(
                    "LLVM ToStr does not support source type {other:?}"
                )));
            }
        };
        let value = self.operand(value, &argument_ty)?;
        let len_slot = self.scratch_alloca(&ArType::Primitive(Primitive::I64))?;
        let pointer = self.new_value();
        self.writer.line(format!(
            "{pointer} = call ptr @{helper}({}, ptr {len_slot})",
            value.render()
        ));
        let length = self.new_value();
        self.writer
            .line(format!("{length} = load i64, ptr {len_slot}, align 8"));
        let first = self.new_value();
        self.writer.line(format!(
            "{first} = insertvalue {{ ptr, i64 }} undef, ptr {pointer}, 0"
        ));
        self.writer.line(format!(
            "{result} = insertvalue {{ ptr, i64 }} {first}, i64 {length}, 1"
        ));
        Ok(())
    }

    fn emit_string_interp(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        parts: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        // Match Cranelift: always return a fresh malloc buffer. AMIR frees each
        // input part *and* the interp result; aliasing a single part (or leaking
        // intermediate `ar_str_concat` buffers) is a double-free / leak.
        if parts.is_empty() {
            let first = self.new_value();
            self.writer.line(format!(
                "{first} = insertvalue {{ ptr, i64 }} undef, ptr null, 0"
            ));
            self.writer.line(format!(
                "{result} = insertvalue {{ ptr, i64 }} {first}, i64 0, 1"
            ));
            return Ok(());
        }
        let string_ty = ArType::Primitive(Primitive::Str);
        let mut part_vals = Vec::with_capacity(parts.len());
        for part in parts {
            part_vals.push(self.operand(part, &string_ty)?);
        }
        let mut total = self.new_value();
        self.writer.line(format!("{total} = add i64 0, 0"));
        let mut ptrs = Vec::with_capacity(part_vals.len());
        let mut lens = Vec::with_capacity(part_vals.len());
        for part in &part_vals {
            let ptr = self.new_value();
            self.writer.line(format!(
                "{ptr} = extractvalue {{ ptr, i64 }} {}, 0",
                part.value
            ));
            let len = self.new_value();
            self.writer.line(format!(
                "{len} = extractvalue {{ ptr, i64 }} {}, 1",
                part.value
            ));
            let next_total = self.new_value();
            self.writer
                .line(format!("{next_total} = add i64 {total}, {len}"));
            total = next_total;
            ptrs.push(ptr);
            lens.push(len);
        }
        let alloc_size = self.new_value();
        self.writer
            .line(format!("{alloc_size} = add i64 {total}, 1"));
        let buf = self.new_value();
        self.writer.line(format!(
            "{buf} = call ptr @ar_rt_raw_malloc(i64 {alloc_size})"
        ));
        let mut offset = self.new_value();
        self.writer.line(format!("{offset} = add i64 0, 0"));
        for (ptr, len) in ptrs.iter().zip(&lens) {
            let dest = self.new_value();
            self.writer.line(format!(
                "{dest} = getelementptr inbounds i8, ptr {buf}, i64 {offset}"
            ));
            self.writer.line(format!(
                "call void @llvm.memcpy.p0.p0.i64(ptr {dest}, ptr {ptr}, i64 {len}, i1 false)"
            ));
            let next_offset = self.new_value();
            self.writer
                .line(format!("{next_offset} = add i64 {offset}, {len}"));
            offset = next_offset;
        }
        let nul_addr = self.new_value();
        self.writer.line(format!(
            "{nul_addr} = getelementptr inbounds i8, ptr {buf}, i64 {total}"
        ));
        self.writer
            .line(format!("store i8 0, ptr {nul_addr}, align 1"));
        let first = self.new_value();
        self.writer.line(format!(
            "{first} = insertvalue {{ ptr, i64 }} undef, ptr {buf}, 0"
        ));
        let owned = self.new_value();
        self.writer.line(format!(
            "{owned} = insertvalue {{ ptr, i64 }} {first}, i64 {total}, 1"
        ));
        self.assign_coerced(
            result,
            TypedValue::new("{ ptr, i64 }".to_owned(), owned),
            lhs_ty,
        )
    }

    /// Compare fat pointers (`str` / slices) by length then `memcmp`.
    ///
    /// LLVM rejects `icmp` on `{ ptr, i64 }` aggregates; this mirrors the
    /// Cranelift / C backend lowering.
    fn emit_fat_pointer_eq(
        &mut self,
        result: &str,
        left: &AmirOperand,
        right: &AmirOperand,
        operand_ty: &ArType,
        negate: bool,
    ) -> Result<(), Diagnostic> {
        let left = self.operand(left, operand_ty)?;
        let right = self.operand(right, operand_ty)?;
        let left_ptr = self.new_value();
        self.writer.line(format!(
            "{left_ptr} = extractvalue {{ ptr, i64 }} {}, 0",
            left.value
        ));
        let left_len = self.new_value();
        self.writer.line(format!(
            "{left_len} = extractvalue {{ ptr, i64 }} {}, 1",
            left.value
        ));
        let right_ptr = self.new_value();
        self.writer.line(format!(
            "{right_ptr} = extractvalue {{ ptr, i64 }} {}, 0",
            right.value
        ));
        let right_len = self.new_value();
        self.writer.line(format!(
            "{right_len} = extractvalue {{ ptr, i64 }} {}, 1",
            right.value
        ));
        let len_eq = self.new_value();
        self.writer
            .line(format!("{len_eq} = icmp eq i64 {left_len}, {right_len}"));
        let same_length = self.new_block();
        let different_lengths = self.new_block();
        let empty_strings = self.new_block();
        let compare_bytes = self.new_block();
        let join = self.new_block();
        self.writer.line(format!(
            "br i1 {len_eq}, label %{same_length}, label %{different_lengths}"
        ));

        self.enter_llvm_label(&different_lengths);
        self.writer.line(format!("br label %{join}"));

        self.enter_llvm_label(&same_length);
        let is_empty = self.new_value();
        self.writer
            .line(format!("{is_empty} = icmp eq i64 {left_len}, 0"));
        self.writer.line(format!(
            "br i1 {is_empty}, label %{empty_strings}, label %{compare_bytes}"
        ));

        self.enter_llvm_label(&empty_strings);
        self.writer.line(format!("br label %{join}"));

        self.enter_llvm_label(&compare_bytes);
        let cmp = self.new_value();
        self.writer.line(format!(
            "{cmp} = call i32 @memcmp(ptr {left_ptr}, ptr {right_ptr}, i64 {left_len})"
        ));
        let bytes_eq = self.new_value();
        self.writer
            .line(format!("{bytes_eq} = icmp eq i32 {cmp}, 0"));
        self.writer.line(format!("br label %{join}"));

        self.enter_llvm_label(&join);
        let eq = self.new_value();
        self.writer.line(format!(
            "{eq} = phi i1 [ false, %{different_lengths} ], [ true, %{empty_strings} ], [ {bytes_eq}, %{compare_bytes} ]"
        ));
        if negate {
            self.writer.line(format!("{result} = xor i1 {eq}, true"));
        } else {
            self.writer
                .line(format!("{result} = select i1 true, i1 {eq}, i1 {eq}"));
        }
        Ok(())
    }

    fn emit_coroutine_ready(
        &mut self,
        result: &str,
        value: &AmirOperand,
        payload_ty: arandu_middle::types::TypeId,
    ) -> Result<(), Diagnostic> {
        let payload_ar = self.module.interner.resolve(payload_ty);
        let payload_layout = self
            .module
            .layout
            .layout_of_type(&payload_ar, self.module.interner, self.module.info)
            .map_err(|error| ice(format!("LLVM layout failed for {payload_ar:?}: {error}")))?;
        // Header is 8 bytes; host poll/block_on helpers read an i64 payload, so
        // keep at least 16 bytes (matches Cranelift).
        let size = (8 + payload_layout.size.max(1)).max(16);
        let pointer = self.new_value();
        self.writer.line(format!(
            "{pointer} = call ptr @ar_rt_raw_malloc(i64 {size})"
        ));
        let null = self.new_value();
        self.writer
            .line(format!("{null} = icmp eq ptr {pointer}, null"));
        let ok = self.new_value();
        self.writer.line(format!("{ok} = xor i1 {null}, true"));
        let trap = self.new_block();
        let continue_block = self.new_block();
        self.writer.line(format!(
            "br i1 {ok}, label %{continue_block}, label %{trap}"
        ));
        self.enter_llvm_label(&trap);
        self.writer.line("call void @llvm.trap()");
        self.writer.line("unreachable");
        self.enter_llvm_label(&continue_block);
        self.writer
            .line(format!("store i32 0, ptr {pointer}, align 4"));
        let magic = self.byte_gep(&pointer, 4);
        // CO_MAGIC = 0x4152434F ("ARCO"); must match `poll_runtime::CO_MAGIC`.
        self.writer.line(format!(
            "store i32 {}, ptr {magic}, align 4",
            0x4152_434fu32
        ));
        if !matches!(payload_ar, ArType::Void) {
            let payload = self.operand(value, &payload_ar)?;
            let payload_address = self.byte_gep(&pointer, 8);
            // Widen integer payloads to i64 so `ar_co_block_on_i64` can read them.
            let stored = if self.module.integer_width(&payload_ar).is_some() {
                self.coerce(payload, &ArType::Primitive(Primitive::I64))?
            } else {
                payload
            };
            let stored_ty = if self.module.integer_width(&payload_ar).is_some() {
                ArType::Primitive(Primitive::I64)
            } else {
                payload_ar.clone()
            };
            self.store_value(&payload_address, &stored, &stored_ty)?;
        }
        self.assign_coerced(
            result,
            TypedValue::new("ptr".to_owned(), pointer),
            &ArType::Ptr(self.module.interner.intern(ArType::Void)),
        )
    }

    fn emit_gen_operation(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
    ) -> Result<(), Diagnostic> {
        match rvalue {
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => {
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let value = self.operand(value, &payload_ar)?;
                self.store_value(&slot, &value, &payload_ar)?;
                let inserted = self.new_value();
                self.writer.line(format!(
                    "{inserted} = call i64 @ar_gen_insert_raw(ptr {slot}, i64 {}, i64 {}, ptr null)",
                    layout.size, layout.align
                ));
                self.assign_coerced(result, TypedValue::new("i64".to_owned(), inserted), lhs_ty)
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            }
            | AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => {
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let handle = self.operand(gen_ref, &ArType::Primitive(Primitive::I64))?;
                let name = if matches!(rvalue, AmirRvalue::GenGet { .. }) {
                    "ar_gen_get_raw"
                } else {
                    "ar_gen_remove_raw"
                };
                let ok = self.new_value();
                self.writer.line(format!(
                    "{ok} = call i1 @{name}(i64 {}, ptr {slot}, i64 {}, i64 {})",
                    handle.value, layout.size, layout.align
                ));
                let fail = self.new_block();
                let cont = self.new_block();
                self.writer
                    .line(format!("br i1 {ok}, label %{cont}, label %{fail}"));
                self.enter_llvm_label(&fail);
                self.writer.line("call void @llvm.trap()");
                self.writer.line("unreachable");
                self.enter_llvm_label(&cont);
                let payload_text = self.module.llvm_ty(&payload_ar)?;
                let align = self.module.align_of(&payload_ar);
                let loaded = self.new_value();
                self.writer.line(format!(
                    "{loaded} = load {payload_text}, ptr {slot}, align {align}"
                ));
                self.assign_coerced(result, TypedValue::new(payload_text, loaded), lhs_ty)
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            }
            | AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                let upsert = matches!(rvalue, AmirRvalue::GenUpsert { .. });
                let payload_ar = self.module.interner.resolve(*payload_ty);
                let layout = self
                    .module
                    .layout
                    .layout_of_type(&payload_ar, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {payload_ar:?}: {error}"))
                    })?;
                let slot = self.scratch_alloca(&payload_ar)?;
                let value = self.operand(value, &payload_ar)?;
                self.store_value(&slot, &value, &payload_ar)?;
                let handle = self.operand(gen_ref, &ArType::Primitive(Primitive::I64))?;
                if upsert {
                    let updated = self.new_value();
                    self.writer.line(format!(
                        "{updated} = call i64 @ar_gen_upsert_raw(i64 {}, ptr {slot}, i64 {}, i64 {}, ptr null)",
                        handle.value, layout.size, layout.align
                    ));
                    self.assign_coerced(result, TypedValue::new("i64".to_owned(), updated), lhs_ty)
                } else {
                    let ok = self.new_value();
                    self.writer.line(format!(
                        "{ok} = call i1 @ar_gen_set_raw(i64 {}, ptr {slot}, i64 {}, i64 {}, ptr null)",
                        handle.value, layout.size, layout.align
                    ));
                    let fail = self.new_block();
                    let cont = self.new_block();
                    self.writer
                        .line(format!("br i1 {ok}, label %{cont}, label %{fail}"));
                    self.enter_llvm_label(&fail);
                    self.writer.line("call void @llvm.trap()");
                    self.writer.line("unreachable");
                    self.enter_llvm_label(&cont);
                    self.assign_coerced(result, handle, lhs_ty)
                }
            }
            _ => Err(ice("unexpected GenRef operation")),
        }
    }

    fn construct_aggregate(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        fields: &[(u64, ArType, &AmirOperand)],
    ) -> Result<(), Diagnostic> {
        let slot = self.scratch_alloca(lhs_ty)?;
        let aggregate = self.module.llvm_ty(lhs_ty)?;
        let align = self.module.align_of(lhs_ty);
        self.writer.line(format!(
            "store {aggregate} zeroinitializer, ptr {slot}, align {align}"
        ));
        for (offset, field_ty, operand) in fields {
            let address = self.byte_gep(&slot, *offset);
            let value = self.operand(operand, field_ty)?;
            self.store_value(&address, &value, field_ty)?;
        }
        self.writer.line(format!(
            "{result} = load {aggregate}, ptr {slot}, align {align}"
        ));
        Ok(())
    }

    fn new_value(&mut self) -> String {
        let value = format!("%v{}", self.next_value);
        self.next_value += 1;
        value
    }

    /// Fresh basic-block label without a leading `%`.
    ///
    /// Callers must write `label %{name}` / `{name}:`. Using [`Self::new_value`]
    /// here would emit `%%vN` because that helper already includes `%`.
    fn new_block(&mut self) -> String {
        let name = format!("loc{}", self.next_value);
        self.next_value += 1;
        name
    }
}

struct TypedValue {
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

fn value_ar_type(llvm: &str) -> ArType {
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
fn parse_byte_aggregate_size(llvm: &str) -> Option<u64> {
    let rest = llvm.strip_prefix('[')?.strip_suffix(" x i8]")?;
    rest.parse().ok()
}

fn ensure_float_decimal(value: &str) -> String {
    // LLVM rejects bare scientific forms like `1e-6` (no decimal point).
    if value.contains('.') {
        value.to_owned()
    } else if let Some(idx) = value.find(['e', 'E']) {
        format!("{}.0{}", &value[..idx], &value[idx..])
    } else {
        format!("{value}.0")
    }
}

fn parse_char_literal(text: &str) -> Option<char> {
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

fn escape_c_string(bytes: &[u8]) -> String {
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

fn quote_name(name: &str) -> String {
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

trait IntegerWidthFromLlvm {
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
