use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use arandu_middle::amir::{AmirConstant, AmirFunc, AmirOperand, AmirProgram};
use arandu_middle::intrinsics::IntrinsicKind;
use arandu_middle::layout::LayoutEngine;
use arandu_middle::types::{ArType, Primitive, VectorType};
use arandu_middle::{Diagnostic, SymbolId, SymbolKind, SymbolTable};
use arandu_semantics::TypeInfo;

use super::{FuncEmitter, ModuleEmitter, escape_c_string, ice, quote_name};
use crate::types::{fat_ptr_llvm_ty, llvm_type, ptr_int};

impl<'a> ModuleEmitter<'a> {
    pub(super) fn new(
        program: &'a AmirProgram,
        symbols: &'a SymbolTable,
        info: &'a TypeInfo,
    ) -> Self {
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

        Self {
            program,
            symbols,
            info,
            interner: &info.type_interner,
            layout: LayoutEngine::from_data_layout(info.target_layout),
            string_globals,
            globals,
            declarations: RefCell::new(BTreeMap::new()),
        }
    }

    /// Materialize a declaration the first time emission references it.
    ///
    /// Textual LLVM IR prints every `declare` as a module entity, so unlike a
    /// JIT symbol table an unused declaration is observable output. Emission
    /// sites that write `@name` request the symbol here; [`Self::emit`] prints
    /// only the accumulated set, in deterministic `BTreeMap` order.
    pub(super) fn require_declaration(&self, key: &str, text: impl Into<String>) {
        self.declarations
            .borrow_mut()
            .entry(key.to_owned())
            .or_insert_with(|| text.into());
    }

    pub(super) fn require_llvm_trap(&self) {
        self.require_declaration("llvm.trap", "declare void @llvm.trap()");
    }

    pub(super) fn require_raw_malloc(&self) {
        let len = ptr_int(self.layout.pointer_width());
        self.require_declaration(
            "ar_rt_raw_malloc",
            format!("declare ptr @ar_rt_raw_malloc({len})"),
        );
    }

    pub(super) fn require_raw_free(&self) {
        self.require_declaration("ar_rt_raw_free", "declare void @ar_rt_raw_free(ptr)");
    }

    pub(super) fn require_memcmp(&self) {
        let len = ptr_int(self.layout.pointer_width());
        self.require_declaration("memcmp", format!("declare i32 @memcmp(ptr, ptr, {len})"));
    }

    pub(super) fn require_memcpy(&self) {
        let memcpy = self.memcpy_intrinsic();
        let len = ptr_int(self.layout.pointer_width());
        self.require_declaration(
            memcpy,
            format!("declare void @{memcpy}(ptr, ptr, {len}, i1)"),
        );
    }

    /// `ToStr` helpers keep their fixed host signatures: `bool` crosses as
    /// `i8` and `char` as `i32`, independent of the Arandu facade.
    pub(super) fn require_to_str_helper(&self, name: &str) -> Result<(), Diagnostic> {
        let text = match name {
            "ar_jit_i64_to_str" => "declare ptr @ar_jit_i64_to_str(i64, ptr)",
            "ar_jit_u64_to_str" => "declare ptr @ar_jit_u64_to_str(i64, ptr)",
            "ar_jit_f64_to_str" => "declare ptr @ar_jit_f64_to_str(double, ptr)",
            "ar_jit_bool_to_str" => "declare ptr @ar_jit_bool_to_str(i8, ptr)",
            "ar_jit_char_to_str" => "declare ptr @ar_jit_char_to_str(i32, ptr)",
            "ar_jit_err_to_str" => "declare ptr @ar_jit_err_to_str(ptr, ptr)",
            other => return Err(ice(format!("unknown ToStr helper {other}"))),
        };
        self.require_declaration(name, text);
        Ok(())
    }

    pub(super) fn require_gen_helper(&self, name: &str) -> Result<(), Diagnostic> {
        let text = match name {
            "ar_gen_insert_raw" => "declare i64 @ar_gen_insert_raw(ptr, i64, i64, ptr)",
            "ar_gen_get_raw" => "declare i1 @ar_gen_get_raw(i64, ptr, i64, i64)",
            "ar_gen_set_raw" => "declare i1 @ar_gen_set_raw(i64, ptr, i64, i64, ptr)",
            "ar_gen_upsert_raw" => "declare i64 @ar_gen_upsert_raw(i64, ptr, i64, i64, ptr)",
            "ar_gen_remove_raw" => "declare i1 @ar_gen_remove_raw(i64, ptr, i64, i64)",
            other => return Err(ice(format!("unknown GenRef helper {other}"))),
        };
        self.require_declaration(name, text);
        Ok(())
    }

    pub(super) fn require_bit_intrinsic(&self, name: &str) -> Result<(), Diagnostic> {
        let (intrinsic, width) = name
            .rsplit_once(".i")
            .ok_or_else(|| ice(format!("bit intrinsic {name} has no integer width")))?;
        let width: u32 = width
            .parse()
            .map_err(|_| ice(format!("bit intrinsic {name} has an invalid width")))?;
        let text = match intrinsic {
            "llvm.ctpop" | "llvm.bitreverse" => {
                format!("declare i{width} @{name}(i{width})")
            }
            "llvm.ctlz" | "llvm.cttz" => format!("declare i{width} @{name}(i{width}, i1)"),
            other => return Err(ice(format!("unknown bit intrinsic {other}"))),
        };
        self.require_declaration(name, text);
        Ok(())
    }

    /// Declare an extern callee with the exact signature the call site emits.
    ///
    /// `signature_for` is the single source of truth for extern ABIs, so the
    /// declaration cannot drift from the `call` instruction.
    pub(super) fn require_extern_signature(
        &self,
        name: &str,
        args: &[ArType],
        ret: &ArType,
    ) -> Result<(), Diagnostic> {
        let mut flattened = Vec::new();
        for arg in args {
            flattened.extend(self.host_abi_param_llvm_tys(arg)?);
        }
        let ret = self.llvm_ty(ret)?;
        let params = flattened.join(", ");
        let calling_convention = self.runtime_calling_convention(name);
        self.require_declaration(
            name,
            format!(
                "declare {calling_convention}{ret} @{}({params})",
                quote_name(name)
            ),
        );
        Ok(())
    }

    pub(super) fn emit(self) -> Result<String, Diagnostic> {
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
                profile.llvm_triple(),
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
        for declaration in self.declarations.borrow().values() {
            output.push_str(declaration);
            output.push('\n');
        }
        output.push('\n');
        output.push_str(&body);
        Ok(output)
    }

    pub(super) fn function_name(&self, symbol: SymbolId) -> String {
        self.symbols.try_get(symbol).map_or_else(
            || format!("sym{}", symbol.local_id.0),
            |symbol| self.symbols.host_func_name(symbol).to_owned(),
        )
    }

    pub(super) fn is_internal(&self, symbol: SymbolId) -> bool {
        self.program.funcs.iter().any(|func| func.symbol == symbol)
    }

    /// Runtime fat-string helpers use `extern "sysv64"` on Windows so their
    /// ABI matches Cranelift JIT. LLVM's default convention on Windows is Win64,
    /// so preserve the runtime convention explicitly for these imports.
    pub(super) fn runtime_calling_convention(&self, name: &str) -> &'static str {
        if self.info.target_identity.os == "windows"
            && self.info.target_identity.arch == "x86_64"
            && super::WINDOWS_SYSV_RUNTIME_SYMBOLS.contains(&name)
        {
            "x86_64_sysvcc "
        } else {
            ""
        }
    }

    pub(super) fn intrinsic_kind(&self, symbol: SymbolId) -> Option<IntrinsicKind> {
        let symbol = self.symbols.try_get(symbol)?;
        if symbol.kind != SymbolKind::ExternFunc {
            return None;
        }
        IntrinsicKind::from_declared_name(&symbol.name, symbol.lang_item)
    }

    pub(super) fn signature_for(
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

    pub(super) fn operand_type(&self, operand: &AmirOperand, func: &AmirFunc) -> ArType {
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

    pub(super) fn llvm_ty(&self, ty: &ArType) -> Result<String, Diagnostic> {
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

    pub(super) fn alloca_type(&self, ty: &ArType) -> Result<(String, u64), Diagnostic> {
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

    pub(super) fn align_of(&self, ty: &ArType) -> u64 {
        self.layout
            .layout_of_type(ty, self.interner, self.info)
            .map_or(1, |layout| layout.align.max(1))
    }

    pub(super) fn pointer_align(&self) -> u64 {
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

    pub(super) fn usize_ty(&self) -> ArType {
        ArType::Primitive(Primitive::USize)
    }

    pub(super) fn i64_ty(&self) -> ArType {
        ArType::Primitive(Primitive::I64)
    }

    pub(super) fn void_ptr_ty(&self) -> ArType {
        ArType::Ptr(self.interner.intern(ArType::Void))
    }

    pub(super) fn is_signed(&self, ty: &ArType) -> bool {
        match ty {
            ArType::Primitive(primitive) => primitive.is_signed(),
            ArType::Vector(vector) => vector.lane().is_signed(),
            _ => false,
        }
    }

    pub(super) fn integer_width(&self, ty: &ArType) -> Option<u64> {
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

    pub(super) fn is_pointer(&self, ty: &ArType) -> bool {
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

    pub(super) fn is_fat_pointer(&self, ty: &ArType) -> bool {
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

    pub(super) fn is_float(&self, ty: &ArType) -> bool {
        matches!(ty, ArType::Primitive(p) if p.is_float())
    }

    pub(super) fn is_void(&self, ty: &ArType) -> bool {
        matches!(ty, ArType::Void)
    }

    pub(super) fn vector_info(&self, ty: &ArType) -> Option<(VectorType, u64, Primitive)> {
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

#[cfg(test)]
mod tests {
    use super::ModuleEmitter;
    use arandu_middle::amir::AmirProgram;

    #[test]
    fn windows_runtime_fat_string_helpers_use_sysv_calling_convention() {
        let program = AmirProgram {
            funcs: Vec::new(),
            literal_pool: Default::default(),
            extern_funcs: Default::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        let symbols = arandu_semantics::SymbolTable::default();
        let mut info = arandu_semantics::TypeInfo::new();
        info.target_identity =
            arandu_middle::db::TargetIdentity::from_triple("x86_64-pc-windows-msvc")
                .expect("canonical Windows triple");
        info.target_layout = arandu_middle::DataLayout::ptr_width(8);
        let module = ModuleEmitter::new(&program, &symbols, &info);

        assert_eq!(
            module.runtime_calling_convention("ar_env_arg"),
            "x86_64_sysvcc "
        );
        assert_eq!(module.runtime_calling_convention("ar_jit_i64_to_str"), "");
        module
            .require_extern_signature(
                "ar_str_concat",
                &[
                    arandu_middle::types::ArType::Primitive(arandu_middle::types::Primitive::Str),
                    arandu_middle::types::ArType::Primitive(arandu_middle::types::Primitive::Str),
                ],
                &arandu_middle::types::ArType::Primitive(arandu_middle::types::Primitive::Str),
            )
            .expect("ar_str_concat declaration");
        let ir = module.emit().expect("empty Windows module");
        assert!(
            ir.contains("declare x86_64_sysvcc { ptr, i64 } @ar_str_concat(ptr, i64, ptr, i64)")
        );
    }
}
