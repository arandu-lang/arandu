//! Direct lowering from AMIR to textual LLVM IR.
//!
//! This module deliberately keeps LLVM behind a text boundary: the backend is
//! pure Rust and never links `llvm-sys`. The generated module can be validated
//! with `llvm-as`. Execution links through `clang` and the packaged runtime;
//! `lli` does not resolve Arandu runtime symbols, so the CLI refuses it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use arandu_middle::amir::{AmirFunc, AmirProgram, BlockId, LocalId, TempId};
use arandu_middle::layout::LayoutEngine;
use arandu_middle::type_context::CodegenTypeContext;
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use arandu_middle::{DiagCode, Diagnostic, Span, SymbolId, SymbolTable};

use crate::writer::IrWriter;

mod call;
mod export;
mod func_emit;
mod module_emit;
mod rvalue;
mod stmt;
mod term;
mod value;

const WINDOWS_SYSV_RUNTIME_SYMBOLS: &[&str] = &[
    "ar_env_arg",
    "ar_path_join",
    "ar_path_file_name",
    "ar_str_concat",
    "ar_str_split_last",
    "ar_test_temp_dir",
];

/// Emit one textual LLVM module for a validated AMIR program.
pub fn emit_llvm_ir(
    program: &AmirProgram,
    symbols: &SymbolTable,
    info: &dyn CodegenTypeContext,
) -> Result<String, Diagnostic> {
    let diagnostics = arandu_middle::validate_amir_program(program, symbols, info.type_interner());
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
    info: &'a dyn CodegenTypeContext,
    interner: &'a TypeInterner,
    layout: LayoutEngine,
    string_globals: HashMap<u32, (String, usize)>,
    globals: Vec<String>,
    /// Declarations requested by emission sites that actually reference the
    /// symbol. A declaration is a visible module entity in textual LLVM IR,
    /// unlike a JIT symbol table, so unused entries must never be materialized.
    declarations: RefCell<BTreeMap<String, String>>,
    /// Extern symbols each function body references, keyed by the emitting
    /// function. The freestanding gate walks this map to reject an `@Export`
    /// whose reachable code needs the host runtime.
    host_runtime_refs: RefCell<BTreeMap<SymbolId, BTreeSet<String>>>,
}

pub(super) struct FuncEmitter<'a, 'm> {
    module: &'m ModuleEmitter<'a>,
    func: &'a AmirFunc,
    writer: IrWriter,
    defined: Vec<Option<&'static str>>,
    temp_slots: Vec<Option<String>>,
    /// SSA value bound to a temp that was assigned by aliasing an existing
    /// value instead of materializing a copy instruction.
    temp_values: Vec<Option<TypedValue>>,
    /// Temp whose statement destination was just allocated by `destination()`,
    /// so the value emitter can bind it without a fake identity instruction.
    pending_assignment: Option<TempId>,
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
    /// C ABI shape of an `@Export` aggregate return, consumed by the return
    /// terminator. `None` for ordinary Arandu functions.
    pub(super) export_return: Option<ExportReturn>,
}

/// Boundary return shape of an exported function.
pub(super) struct ExportReturn {
    pub info: arandu_middle::layout::ExportAbiInfo,
    /// Opaque byte representation of the aggregate.
    pub aggregate: String,
    pub align: u64,
}

#[derive(Clone)]
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
    use arandu_middle::SymbolId;
    use arandu_semantics::TypeInfo;

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
