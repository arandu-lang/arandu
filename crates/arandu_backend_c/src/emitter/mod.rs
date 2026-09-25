//! C code emitter for the Arandu backend.
//!
//! [`CEmitter`] takes a fully optimized [`AmirProgram`] and produces a
//! single self-contained C translation unit as a `String`. The generated
//! code relies on GCC/Clang GNU extensions (statement expressions `({ })`)
//! and is not standard C99.

use std::fmt::Write;

use arandu_middle::amir::{AmirFunc, AmirProgram};
use arandu_middle::layout::{LayoutEngine, StructLayoutProvider};
use arandu_middle::types::{ArType, TypeInterner};
use arandu_middle::{DiagCode, Diagnostic, Span};
use arandu_semantics::SymbolTable;

pub mod decl;
pub mod expr;
pub mod format;
pub mod func;
pub mod runtime;
pub mod stmt;

pub(super) fn sanitize_c_ident(name: &str) -> String {
    match name {
        "stdin" => return "ar_stdin".to_string(),
        "stdout" => return "ar_stdout".to_string(),
        "stderr" => return "ar_stderr".to_string(),
        "write" => return "ar_write".to_string(),
        "read" => return "ar_read".to_string(),
        "close" => return "ar_close".to_string(),
        "open" => return "ar_open".to_string(),
        "remove" => return "ar_remove".to_string(),
        "rename" => return "ar_rename".to_string(),
        "abort" => return "ar_abort".to_string(),
        "exit" => return "ar_exit".to_string(),
        _ => {}
    }
    let mut out = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if c == '.' {
            out.push_str("__");
        } else {
            out.push('_');
        }
    }
    out
}

/// Emits a full C translation unit from an [`AmirProgram`].
///
/// The emitter is single-use: construct it with [`CEmitter::new`] and call
/// [`CEmitter::emit`] once to obtain the generated source as a `String`.
pub struct CEmitter<'a> {
    pub(super) program: &'a AmirProgram,
    pub(super) symbols: &'a SymbolTable,
    pub(super) layout: &'a LayoutEngine,
    pub(super) provider: &'a dyn StructLayoutProvider,
    pub(super) interner: &'a TypeInterner,
    pub(super) output: String,
    pub(super) emitted_types: rustc_hash::FxHashSet<String>,
    /// A3.3: unique id for `__ar_co_N` stack payload locals (multi-stmt).
    pub(super) co_stack_slot: u32,
    pub(super) error: Option<Diagnostic>,
}

impl<'a> CEmitter<'a> {
    /// Creates a new `CEmitter` bound to the given program and type metadata.
    pub fn new(
        program: &'a AmirProgram,
        symbols: &'a SymbolTable,
        layout: &'a LayoutEngine,
        provider: &'a dyn StructLayoutProvider,
        interner: &'a TypeInterner,
    ) -> Self {
        Self {
            program,
            symbols,
            layout,
            provider,
            interner,
            output: String::new(),
            emitted_types: rustc_hash::FxHashSet::default(),
            co_stack_slot: 0,
            error: None,
        }
    }

    /// Next `__ar_co_N` id for stack-first CoroutineReady multi-stmt emission.
    #[inline]
    pub(super) fn next_co_stack_slot(&mut self) -> u32 {
        let n = self.co_stack_slot;
        self.co_stack_slot = self.co_stack_slot.saturating_add(1);
        n
    }

    /// Resolve an AMIR temp's dense `TypeId` (DoD — no `ArType` on the IR).
    #[inline]
    pub(super) fn temp_ty(&self, func: &AmirFunc, t: arandu_middle::amir::TempId) -> ArType {
        self.interner.resolve(func.temps[t.as_usize()].ty)
    }

    #[inline]
    pub(super) fn local_ty(&self, func: &AmirFunc, local: arandu_middle::amir::LocalId) -> ArType {
        self.interner.resolve(func.locals[local.as_usize()].ty)
    }

    #[inline]
    pub(super) fn operand_ty(
        &self,
        func: &AmirFunc,
        op: &arandu_middle::amir::AmirOperand,
    ) -> ArType {
        match op {
            arandu_middle::amir::AmirOperand::Copy(t)
            | arandu_middle::amir::AmirOperand::Move(t) => self.temp_ty(func, *t),
            arandu_middle::amir::AmirOperand::Constant(c) => match c {
                arandu_middle::amir::AmirConstant::Pool(id) => {
                    match self.program.literal_pool.get(*id) {
                        arandu_middle::literal_pool::AmirLiteralEntry::Str(_) => {
                            ArType::Primitive(arandu_middle::types::Primitive::Str)
                        }
                        arandu_middle::literal_pool::AmirLiteralEntry::Int(_) => {
                            ArType::Primitive(arandu_middle::types::Primitive::Int)
                        }
                        arandu_middle::literal_pool::AmirLiteralEntry::Float(_) => {
                            ArType::Primitive(arandu_middle::types::Primitive::Float)
                        }
                        arandu_middle::literal_pool::AmirLiteralEntry::Char(_) => {
                            ArType::Primitive(arandu_middle::types::Primitive::Char)
                        }
                    }
                }
                arandu_middle::amir::AmirConstant::Bool(_) => {
                    ArType::Primitive(arandu_middle::types::Primitive::Bool)
                }
                arandu_middle::amir::AmirConstant::Nil => ArType::Void,
            },
            _ => ArType::Error,
        }
    }

    /// Resolve a field through the concrete arguments of a named type.
    ///
    /// Layout already substitutes generic parameters, so code generation must
    /// do the same when it chooses the C lvalue type.  Returning the template
    /// field (`T*`) for `Vec<int>` produces a translation unit whose temporary
    /// declarations and field accesses disagree.
    pub(super) fn instantiated_field_ty(&self, named_ty: &ArType, field_name: &str) -> ArType {
        arandu_middle::layout::instantiated_field_type(
            named_ty,
            field_name,
            self.interner,
            self.provider,
        )
        .map_or(ArType::Error, |id| self.interner.resolve(id))
    }

    pub(super) fn record_codegen_ice(&mut self, func: &AmirFunc, message: impl Into<String>) {
        if self.error.is_none() {
            let span = func
                .temps
                .first()
                .map(|temp| temp.span)
                .or_else(|| func.locals.first().map(|local| local.span))
                .unwrap_or_else(|| Span::new(func.symbol.file_id, 0, 0));
            self.error = Some(Diagnostic::ice(DiagCode::ICEGEN001, message, span));
        }
    }

    pub(super) fn checked_layout(&mut self, ty: &ArType) -> arandu_middle::layout::TypeLayout {
        match self.layout.layout_of_type(ty, self.interner, self.provider) {
            Ok(layout) => layout,
            Err(error) => {
                if self.error.is_none() {
                    self.error = Some(Diagnostic::ice(
                        DiagCode::ICEGEN001,
                        format!("C code generation rejected an invalid type layout: {error}"),
                        Span::new(0, 0, 0),
                    ));
                }
                arandu_middle::layout::TypeLayout::simple(0, 1)
            }
        }
    }

    /// Emits all type definitions, string literal globals, and function bodies,
    /// then returns the complete C source as a `String`.
    pub fn emit(mut self) -> Result<String, Diagnostic> {
        let needs_str = self.program_uses_str();
        let needs_println = self.program_uses_println();
        let needs_eprint = self.program_uses_eprint();
        // I/O prelude functions require the ArStr runtime even without literals.
        let needs_str = needs_str || needs_println || needs_eprint;
        self.emit_headers(needs_str);
        if needs_str {
            self.emit_str_literals();
        }
        if needs_println {
            self.emit_prelude_println();
        }
        if needs_eprint {
            self.emit_prelude_eprint();
        }

        for func in &self.program.funcs {
            let ret = self.interner.resolve(func.return_type);
            self.ensure_type_emitted(&ret);
            for local in &func.locals {
                let ty = self.interner.resolve(local.ty);
                self.ensure_type_emitted(&ty);
            }
            for temp in &func.temps {
                let ty = self.interner.resolve(temp.ty);
                self.ensure_type_emitted(&ty);
            }
            self.emit_func_decl(func);
        }
        for (symbol, (params, ret)) in &self.program.extern_funcs {
            let name = sanitize_c_ident(&self.symbols.get(*symbol).name);
            // Provided as static helpers in this TU (path + pure-buffer alloc).
            if matches!(
                name.as_str(),
                "ar_path_is_absolute"
                    | "ar_path_is_empty"
                    | "ar_path_join"
                    | "ar_path_join_owned"
                    | "ar_path_file_name"
                    | "ar_vec_malloc"
                    | "ar_vec_buf_free"
                    | "ar_vec_realloc"
                    | "ar_str_concat"
                    | "ar_str_split_last"
                    | "ar_string_push_str"
                    | "exists"
                    | "ar_fs_open"
                    | "ar_fs_read"
                    | "ar_fs_write"
                    | "ar_fs_close"
                    | "ar_fs_read_all"
                    | "ar_fs_readdir"
                    | "ar_env_args_len"
                    | "ar_env_arg"
                    | "ar_env_var_is_set"
                    | "ar_rt_block_on_i64"
                    | "ar_rt_spawn_i64"
                    | "ar_rt_join_i64"
                    | "ar_rt_cancel_i64"
                    | "ar_rt_parallel_fold_run"
                    | "io__eprint"
            ) {
                continue;
            }
            self.ensure_type_emitted(ret);
            for param in params {
                self.ensure_type_emitted(param);
            }
            let ret_str = self.format_type(ret);
            let _ = write!(&mut self.output, "{} {}(", ret_str, name);
            for (i, param) in params.iter().enumerate() {
                if i > 0 {
                    let _ = write!(&mut self.output, ", ");
                }
                let ty_str = self.format_type(param);
                let _ = write!(&mut self.output, "{}", ty_str);
            }
            if params.is_empty() {
                let _ = write!(&mut self.output, "void");
            }
            let _ = writeln!(&mut self.output, ");");
        }
        self.emit_gen_drop_glues();
        for func in &self.program.funcs {
            self.emit_func(func);
        }
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.output),
        }
    }
}
