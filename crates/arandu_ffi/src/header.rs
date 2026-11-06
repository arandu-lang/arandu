//! C header emission for `@Export`.
//!
//! Everything here is derived from the same inputs the compiler used: the
//! target pointer width, [`arandu_middle::layout::ffi_type_of`] for the type
//! mapping, and [`LayoutEngine`] for sizes and offsets. No backend participates,
//! so `--emit=header` describes the object regardless of the chosen backend.
//! Every aggregate carries `_Static_assert`s on size, alignment and field
//! offsets: a C translation unit that disagrees with Arandu fails to compile
//! instead of corrupting memory at run time.

use std::fmt::Write;

use arandu_middle::docs::clean_doc_line;
use arandu_middle::layout::{
    FfiPosition, FfiType, LayoutEngine, StructLayoutProvider, TypeLayout, c_primitive_name,
    ffi_type_of,
};
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use arandu_middle::{DiagCode, Diagnostic, DocCommentMap, NodeKey, SymbolId, SymbolTable};
use arandu_typeck::TypeInfo;

/// Renders the header for every function marked `@Export`.
///
/// # Errors
///
/// A signature that has no C representation, a nominal type without
/// `@Repr("C")`, two Arandu types competing for one C name, or an exported
/// function without a recorded signature all become `T049` diagnostics.
pub fn emit_c_header(
    info: &TypeInfo,
    symbols: &SymbolTable,
    docs: &DocCommentMap,
) -> Result<String, Diagnostic> {
    let context = Context {
        symbols,
        info,
        docs,
        interner: &info.type_interner,
        provider: info,
        pointer_width: u64::from(info.target_pointer_width) / 8,
        layout: LayoutEngine::new(u64::from(info.target_pointer_width) / 8),
    };
    if let Some((_, second, name)) = symbols.exported_symbol_collision() {
        return Err(context.error(
            second,
            format!(
                "exported symbol '{name}' is claimed by two functions; C has a single namespace, so give one of them a distinct `@Export(\"...\")` name"
            ),
        ));
    }
    let mut exports = Exports::default();

    let mut exported = symbols
        .exported_functions
        .iter()
        .copied()
        .collect::<Vec<_>>();
    exported.sort_by(|left, right| {
        context
            .c_name(*left)
            .cmp(context.c_name(*right))
            .then_with(|| left.cmp(right))
    });

    for symbol in exported {
        let name = context.c_name(symbol).to_owned();
        let signature = info
            .decl_types
            .get(&symbol)
            .map(|id| context.interner.resolve(*id))
            .and_then(|ty| match ty {
                ArType::Func(params, ret) => Some((params, ret)),
                _ => None,
            })
            .ok_or_else(|| {
                context.error(
                    symbol,
                    format!("cannot describe exported function '{name}'"),
                )
            })?;
        let is_destructor = info.destructors.values().any(|d| *d == symbol);
        let params = context
            .interner
            .type_args(signature.0)
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let ty = context.interner.resolve(*id);
                // `@Destructor` consumes its receiver, and C holds a handle:
                // the first parameter is always `T *`, never a by-value copy.
                let position = if index == 0 && is_destructor {
                    FfiPosition::OwnedReceiver
                } else {
                    FfiPosition::Parameter
                };
                exports.collect(&context, &ty, position, symbol)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let return_ty = context.interner.resolve(signature.1);
        // `Result<T, E>` crosses as a C status code with the payload written
        // through a trailing out-parameter — the same rule the AMIR pass
        // applies to the body, derived from the same `ArType`.
        let mut params = params;
        let status = matches!(return_ty, ArType::Result(..));
        let ret = match &return_ty {
            ArType::Result(ok, err) => {
                if !matches!(context.interner.resolve(*ok), ArType::Void) {
                    let out = ArType::Ptr(*ok);
                    params.push(exports.collect(&context, &out, FfiPosition::Parameter, symbol)?);
                }
                let status_ty = context.interner.resolve(*err);
                exports.collect(&context, &status_ty, FfiPosition::Return, symbol)?
            }
            _ => exports.collect(&context, &return_ty, FfiPosition::Return, symbol)?,
        };
        // An owned handle is exactly what `ffi_type_of` already mapped: a
        // pointer to a named type that is not `@Repr("C")`. Reading the rule
        // back from the mapping keeps the header from inventing a second one.
        let owned_handle = match (&return_ty, &ret) {
            (ArType::Ptr(inner), FfiType::Ptr(pointee)) => {
                match (context.interner.resolve(*inner), pointee.as_ref()) {
                    (ArType::Named(named, _), FfiType::Struct(pointer_target))
                        if named == *pointer_target && !context.provider.is_repr_c(named) =>
                    {
                        Some(named)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        exports.functions.push(Function {
            symbol,
            name,
            params,
            ret,
            owned_handle,
            status,
        });
    }

    exports.render(&context)
}

struct Context<'a> {
    symbols: &'a SymbolTable,
    info: &'a TypeInfo,
    docs: &'a DocCommentMap,
    interner: &'a TypeInterner,
    provider: &'a dyn StructLayoutProvider,
    pointer_width: u64,
    layout: LayoutEngine,
}

impl Context<'_> {
    fn c_name(&self, symbol: SymbolId) -> &str {
        self.symbols.host_func_name(self.symbols.get(symbol))
    }

    fn error(&self, symbol: SymbolId, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(
            DiagCode::T049InvalidFfiExport,
            message.into(),
            self.symbols.get(symbol).span,
        )
    }

    fn struct_name(&self, symbol: SymbolId) -> &str {
        self.symbols.get(symbol).name.as_str()
    }

    fn c_name_of(&self, symbol: SymbolId) -> Option<&str> {
        self.symbols
            .try_get(symbol)
            .map(|symbol| self.symbols.host_func_name(symbol))
    }

    /// Doc comments are keyed by the span the parser attached (`func` keeps its
    /// signature, a struct its type declaration), which is wider than
    /// `Symbol::span`. Look the entry up by containment and prefer the
    /// innermost match so a field doc wins over its struct's doc. Ties are
    /// broken by position because the map iterates in hash order.
    ///
    /// `DocCommentMap` carries no file id, so only symbols from the table's own
    /// file are eligible; imported spans with equal offsets must not borrow
    /// local docs.
    fn doc_lines(&self, symbol: SymbolId) -> Vec<String> {
        let Some(symbol) = self.symbols.try_get(symbol) else {
            return Vec::new();
        };
        let span = symbol.span;
        if span.file_id != self.symbols.file_id {
            return Vec::new();
        }
        let mut best: Option<(&NodeKey, &Vec<String>)> = None;
        for (key, lines) in self.docs {
            if key.start > span.start || key.end < span.end {
                continue;
            }
            let better = match best {
                None => true,
                Some((best_key, _)) => {
                    let width = key.end - key.start;
                    let best_width = best_key.end - best_key.start;
                    width < best_width
                        || (width == best_width
                            && (key.start, key.end) < (best_key.start, best_key.end))
                }
            };
            if better {
                best = Some((key, lines));
            }
        }
        best.map_or_else(Vec::new, |(_, lines)| lines.clone())
    }

    /// Writes a C block comment for the docs attached to `symbol`, if any.
    fn push_doc_comment(&self, out: &mut String, symbol: SymbolId) {
        let lines = self
            .doc_lines(symbol)
            .iter()
            .flat_map(|raw| raw.lines())
            .filter_map(clean_header_doc_line)
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return;
        }
        out.push_str("/**\n");
        for line in lines {
            let _ = writeln!(out, " * {}", line.replace("*/", "*\\/"));
        }
        out.push_str(" */\n");
    }

    /// The exported `@Destructor` for `symbol`, when one exists. `destructors`
    /// is semantic metadata; no method name is inspected.
    fn exported_destructor(&self, symbol: SymbolId) -> Option<&str> {
        let &destructor = self.info.destructors.get(&symbol)?;
        if !self.symbols.is_exported(destructor) {
            return None;
        }
        self.c_name_of(destructor)
    }

    /// Documents the release contract of an opaque handle.
    fn push_opaque_handle_comment(&self, out: &mut String, symbol: SymbolId) {
        let Some(destructor) = self.exported_destructor(symbol) else {
            return;
        };
        let name = self.struct_name(symbol);
        let _ = writeln!(
            out,
            "/* Opaque handle. Release with {destructor}({name} *) — the exported @Destructor of {name}. */"
        );
    }

    /// Documents who owns a handle returned across the boundary. This is not a
    /// diagnostic: a missing destructor is a contract the caller must honor,
    /// not a reason to reject an otherwise valid export.
    fn push_owned_handle_comment(&self, out: &mut String, symbol: SymbolId) {
        let name = self.struct_name(symbol);
        if let Some(destructor) = self.exported_destructor(symbol) {
            let _ = writeln!(
                out,
                "/* Returns an owned handle: the caller must release it with {destructor}({name} *). */"
            );
        } else {
            out.push_str("/* Returns an owned handle: no exported @Destructor releases it. */\n");
        }
    }

    fn render(&self, ty: &FfiType) -> Result<String, Diagnostic> {
        let text = match ty {
            FfiType::Void => "void".to_owned(),
            FfiType::Primitive(primitive) => c_primitive_name(*primitive, self.pointer_width)
                .ok_or_else(|| self.ice(format!("{primitive:?} has no C spelling")))?,
            FfiType::Str => "AranduStr".to_owned(),
            FfiType::Struct(symbol) => self.struct_name(*symbol).to_owned(),
            FfiType::Slice { element, mutable } => self.slice_name(element, *mutable),
            FfiType::Ptr(inner) if inner.is_void() => "void *".to_owned(),
            FfiType::Ptr(inner) => format!("{} *", self.render(inner)?),
            FfiType::Ref { pointee, mutable } => format!(
                "{}{} *",
                if *mutable { "" } else { "const " },
                self.render(pointee)?
            ),
            FfiType::Array { .. } => {
                return Err(self.ice("array types are only valid as struct field declarators"));
            }
        };
        Ok(text)
    }

    /// `type name` for plain values, `type name[N]` for array fields.
    fn declarator(&self, ty: &FfiType, name: &str) -> Result<String, Diagnostic> {
        match ty {
            FfiType::Array { element, len } => {
                Ok(format!("{} {name}[{len}]", self.render(element)?))
            }
            other => Ok(format!("{} {name}", self.render(other)?)),
        }
    }

    /// Slice typedefs are keyed by element *and* mutability. Collapsing
    /// `[]u8` and `mut []u8` into one `const`-qualified typedef is the kind of
    /// silent const-cast bug the boundary must not have.
    fn slice_name(&self, element: &FfiType, mutable: bool) -> String {
        let prefix = if mutable {
            "AranduMutSlice"
        } else {
            "AranduSlice"
        };
        let label = match element {
            FfiType::Primitive(primitive) => primitive_label(*primitive),
            FfiType::Struct(symbol) => self.struct_name(*symbol).to_owned(),
            FfiType::Ptr(_) | FfiType::Ref { .. } => "Ptr".to_owned(),
            _ => "Value".to_owned(),
        };
        format!("{prefix}{label}")
    }

    fn ice(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::ice(
            arandu_middle::DiagCode::ICEGEN001,
            message.into(),
            arandu_middle::Span::new(0, 0, 0),
        )
    }
}

fn primitive_label(primitive: Primitive) -> String {
    let label = match primitive {
        Primitive::Bool => "Bool",
        Primitive::I8 => "I8",
        Primitive::I16 => "I16",
        Primitive::I32 => "I32",
        Primitive::I64 => "I64",
        Primitive::U8 | Primitive::Byte => "U8",
        Primitive::U16 => "U16",
        Primitive::U32 => "U32",
        Primitive::U64 => "U64",
        Primitive::Int => "Int",
        Primitive::Uint => "Uint",
        Primitive::ISize => "Isize",
        Primitive::USize => "Usize",
        Primitive::F32 => "F32",
        Primitive::F64 | Primitive::Float => "F64",
        other => return format!("{other:?}"),
    };
    label.to_owned()
}

/// Normalizes one stored doc line for the generated header.
///
/// The lexer keeps the source marker (`///`, `//!` or `/** … */`), and
/// `clean_doc_line` already strips the line forms. Block forms are handled
/// here so their fences never leak as `*` lines.
fn clean_header_doc_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let text = if let Some(rest) = trimmed.strip_prefix("/**") {
        rest.strip_suffix("*/").unwrap_or(rest)
    } else if trimmed == "*/" {
        ""
    } else {
        clean_doc_line(trimmed)
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

struct Function {
    symbol: SymbolId,
    name: String,
    params: Vec<FfiType>,
    ret: FfiType,
    /// Pointee of an owned opaque handle returned by this function, if any.
    owned_handle: Option<SymbolId>,
    /// Returning a C status code (`Result` lowered): 0 means success.
    status: bool,
}

struct StructDecl {
    symbol: SymbolId,
    ty: ArType,
}

struct SliceDecl {
    element: FfiType,
    mutable: bool,
}

#[derive(Default)]
struct Exports {
    functions: Vec<Function>,
    structs: Vec<StructDecl>,
    /// Types that only ever appear behind a pointer. C needs the name, not the
    /// layout: an opaque handle keeps the Arandu fields private.
    forwards: Vec<SymbolId>,
    slices: Vec<SliceDecl>,
    uses_str: bool,
}

impl Exports {
    /// Validates one boundary value and records every declaration it needs,
    /// dependencies first.
    fn collect(
        &mut self,
        context: &Context<'_>,
        ty: &ArType,
        position: FfiPosition,
        owner: SymbolId,
    ) -> Result<FfiType, Diagnostic> {
        let mapped = ffi_type_of(ty, context.interner, context.provider, position, 0)
            .map_err(|reason| context.error(owner, reason))?;
        self.collect_declarations(context, ty, &mapped, owner)?;
        Ok(mapped)
    }

    fn collect_declarations(
        &mut self,
        context: &Context<'_>,
        ty: &ArType,
        mapped: &FfiType,
        owner: SymbolId,
    ) -> Result<(), Diagnostic> {
        match (ty, mapped) {
            (ArType::Primitive(Primitive::Str), _) => {
                self.uses_str = true;
            }
            (_, FfiType::Struct(symbol)) => {
                if let Some(existing) = self.structs.iter().find(|decl| decl.symbol == *symbol) {
                    if existing.ty != *ty {
                        return Err(context.error(
                            owner,
                            format!(
                                "two instantiations of '{}' map to the same C name; export concrete wrappers with distinct `@Repr(\"C\")` types",
                                context.struct_name(*symbol)
                            ),
                        ));
                    }
                    return Ok(());
                }
                let fields = context
                    .provider
                    .get_struct_fields_for_type(ty, context.interner)
                    .ok_or_else(|| context.error(owner, "struct has no field table"))?;
                let fields = fields.clone();
                for field in fields.iter() {
                    let field_ty = context.interner.resolve(field.ty);
                    self.collect(context, &field_ty, FfiPosition::StructField, owner)?;
                }
                // Post-order: C needs a complete type for a by-value field, so
                // every nested struct is defined before the struct using it.
                self.structs.push(StructDecl {
                    symbol: *symbol,
                    ty: ty.clone(),
                });
            }
            (ArType::Slice(element) | ArType::MutSlice(element), FfiType::Slice { .. }) => {
                let element_ty = context.interner.resolve(*element);
                let element = self.collect(context, &element_ty, FfiPosition::Parameter, owner)?;
                let mutable = matches!(ty, ArType::MutSlice(_));
                if !self
                    .slices
                    .iter()
                    .any(|decl| decl.mutable == mutable && decl.element == element)
                {
                    self.slices.push(SliceDecl { element, mutable });
                }
            }
            (ty, FfiType::Ptr(pointee) | FfiType::Ref { pointee, .. }) if !pointee.is_void() => {
                // A type that only crosses behind a pointer keeps its fields
                // private. `@Destructor` receivers arrive here as `Named` +
                // `Ptr` because C holds the handle, not the value.
                let pointee_symbol = match (ty, pointee.as_ref()) {
                    (ArType::Named(symbol, _), FfiType::Struct(_)) => Some(*symbol),
                    (
                        ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner),
                        FfiType::Struct(_),
                    ) => match context.interner.resolve(*inner) {
                        ArType::Named(symbol, _) => Some(symbol),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(symbol) = pointee_symbol
                    && !self.forwards.contains(&symbol)
                {
                    self.forwards.push(symbol);
                }
            }
            (ArType::Array(_, element), FfiType::Array { .. }) => {
                let element_ty = context.interner.resolve(*element);
                self.collect(context, &element_ty, FfiPosition::StructField, owner)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn render(&self, context: &Context<'_>) -> Result<String, Diagnostic> {
        let mut names = std::collections::BTreeMap::<String, SymbolId>::new();
        for symbol in self
            .structs
            .iter()
            .map(|decl| decl.symbol)
            .chain(self.forwards.iter().copied())
        {
            let name = context.struct_name(symbol).to_owned();
            if name == "AranduStr"
                || name.starts_with("AranduSlice")
                || name.starts_with("AranduMutSlice")
            {
                return Err(context.error(
                    symbol,
                    format!(
                        "type name '{name}' is reserved: the C boundary generates `AranduStr` and `AranduSlice*`/`AranduMutSlice*` typedefs with that prefix"
                    ),
                ));
            }
            if let Some(existing) = names.insert(name.clone(), symbol)
                && existing != symbol
            {
                return Err(context.error(
                    symbol,
                    format!(
                        "two Arandu types map to the C name '{name}'; rename one of them, because C has a single namespace"
                    ),
                ));
            }
        }

        let mut prototypes = Vec::with_capacity(self.functions.len());
        for function in &self.functions {
            let params = function
                .params
                .iter()
                .map(|ty| context.render(ty))
                .collect::<Result<Vec<_>, _>>()?;
            let params = if params.is_empty() {
                "void".to_owned()
            } else {
                params.join(", ")
            };
            let mut prototype = String::new();
            context.push_doc_comment(&mut prototype, function.symbol);
            if let Some(handle) = function.owned_handle {
                context.push_owned_handle_comment(&mut prototype, handle);
            }
            if function.status {
                prototype.push_str("/* Status code: 0 on success, non-zero on failure. */\n");
            }
            let _ = writeln!(
                prototype,
                "{} {}({params});",
                context.render(&function.ret)?,
                function.name
            );
            prototypes.push(prototype);
        }

        let hash = {
            let mut hasher = Fnv::new();
            for prototype in &prototypes {
                hasher.write(prototype.as_bytes());
            }
            for decl in &self.structs {
                hasher.write(context.struct_name(decl.symbol).as_bytes());
            }
            hasher.finish()
        };
        let guard = format!("ARANDU_EXPORT_{hash:016X}_H");

        let mut header = String::new();
        header.push_str("/* Generated by Arandu from `@Export`. Do not edit. */\n");
        header.push_str(
            "/* Layouts match the object file; the static asserts below enforce it. */\n",
        );
        let _ = writeln!(header, "#ifndef {guard}\n#define {guard}\n");
        header.push_str("#include <stdbool.h>\n#include <stddef.h>\n#include <stdint.h>\n\n");
        let _ = writeln!(
            header,
            "_Static_assert(sizeof(void *) == {}, \"this header describes a {}-bit target\");\n",
            context.pointer_width,
            context.pointer_width * 8
        );
        header.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");

        for (name, symbol) in &names {
            // A `@Repr("C")` type that also crosses behind a pointer is not
            // opaque: its fields are spelled below, so its docs belong there.
            let opaque = !self.structs.iter().any(|decl| decl.symbol == *symbol);
            if opaque {
                context.push_doc_comment(&mut header, *symbol);
                context.push_opaque_handle_comment(&mut header, *symbol);
            }
            let _ = writeln!(header, "typedef struct {name} {name};");
        }
        if !names.is_empty() {
            header.push('\n');
        }

        if self.uses_str {
            header.push_str(
                "/* UTF-8 bytes. Not NUL terminated. Arandu does not validate at the boundary. */\n",
            );
            header.push_str("typedef struct { const uint8_t *ptr; size_t len; } AranduStr;\n\n");
        }

        let mut slices = self
            .slices
            .iter()
            .map(|decl| (context.slice_name(&decl.element, decl.mutable), decl))
            .collect::<Vec<_>>();
        slices.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, decl) in &slices {
            let element = context.render(&decl.element)?;
            let qualifier = if decl.mutable { "" } else { "const " };
            let _ = writeln!(
                header,
                "typedef struct {{ {qualifier}{element} *ptr; size_t len; }} {name};"
            );
        }
        if !slices.is_empty() {
            header.push('\n');
        }

        for decl in &self.structs {
            self.render_struct(&mut header, context, decl)?;
        }

        for prototype in &prototypes {
            let _ = writeln!(header, "{prototype}");
        }

        let _ = write!(
            header,
            "\n#ifdef __cplusplus\n}}\n#endif\n\n#endif /* {guard} */\n"
        );
        Ok(header)
    }

    fn render_struct(
        &self,
        header: &mut String,
        context: &Context<'_>,
        decl: &StructDecl,
    ) -> Result<(), Diagnostic> {
        let name = context.struct_name(decl.symbol).to_owned();
        let fields = context
            .provider
            .get_struct_fields_for_type(&decl.ty, context.interner)
            .ok_or_else(|| context.error(decl.symbol, format!("struct {name} has no fields")))?;
        let layout: TypeLayout = context
            .layout
            .layout_of_type(&decl.ty, context.interner, context.provider)
            .map_err(|error| {
                context.error(decl.symbol, format!("struct {name} has no layout: {error}"))
            })?;

        context.push_doc_comment(header, decl.symbol);
        let _ = writeln!(header, "struct {name} {{");
        for field in fields.iter() {
            let field_ty = context.interner.resolve(field.ty);
            let mapped = ffi_type_of(
                &field_ty,
                context.interner,
                context.provider,
                FfiPosition::StructField,
                0,
            )
            .map_err(|reason| {
                context.error(decl.symbol, format!("field `{}`: {reason}", field.name))
            })?;
            let _ = writeln!(header, "    {};", context.declarator(&mapped, &field.name)?);
        }
        header.push_str("};\n");
        let _ = writeln!(
            header,
            "_Static_assert(sizeof({name}) == {}, \"{name} must match Arandu's layout\");",
            layout.size.max(1)
        );
        let _ = writeln!(
            header,
            "_Static_assert(_Alignof({name}) == {}, \"{name} alignment must match Arandu's layout\");",
            layout.align.max(1)
        );
        for (field, offset) in fields.iter().zip(&layout.field_offsets) {
            let _ = writeln!(
                header,
                "_Static_assert(offsetof({name}, {}) == {offset}, \"{name}.{} must match Arandu's layout\");",
                field.name, field.name
            );
        }
        header.push('\n');
        Ok(())
    }
}

/// FNV-1a. The guard only needs to be stable for a given export set, not
/// cryptographic.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use arandu_middle::{Span, SymbolKind};

    fn define(
        symbols: &mut SymbolTable,
        name: &str,
        kind: SymbolKind,
        start: u32,
        end: u32,
    ) -> SymbolId {
        symbols
            .define(symbols.global_scope(), name, kind, Span::new(0, start, end))
            .expect("unique test symbol")
    }

    fn fixture() -> (TypeInfo, SymbolTable, DocCommentMap) {
        let mut info = TypeInfo::new();
        info.target_pointer_width = 64;
        let mut symbols = SymbolTable::new(0);

        let handled = define(&mut symbols, "Handled", SymbolKind::Struct, 0, 10);
        let raw = define(&mut symbols, "Raw", SymbolKind::Struct, 40, 50);
        // A @Destructor is semantically just a method symbol; the header reads
        // the association from `TypeInfo::destructors`. Force the exported flag
        // here because `@Export` validation currently rejects methods, which
        // keeps this regression focused on the rendering contract.
        let release = define(&mut symbols, "release", SymbolKind::AssociatedFunc, 60, 70);
        let open = define(&mut symbols, "handled_open", SymbolKind::Func, 100, 110);
        let make_raw = define(&mut symbols, "raw_make", SymbolKind::Func, 130, 140);
        let consume = define(&mut symbols, "consume", SymbolKind::Func, 160, 170);

        let handled_ty =
            info.type_interner
                .intern(ArType::named(handled, &[], &info.type_interner));
        let raw_ty = info
            .type_interner
            .intern(ArType::named(raw, &[], &info.type_interner));
        let handled_ptr = info.type_interner.intern(ArType::Ptr(handled_ty));
        let raw_ptr = info.type_interner.intern(ArType::Ptr(raw_ty));
        let void = info.type_interner.intern(ArType::Void);

        for (symbol, ty) in [
            (open, ArType::func(&[], handled_ptr, &info.type_interner)),
            (make_raw, ArType::func(&[], raw_ptr, &info.type_interner)),
            (
                consume,
                ArType::func(&[handled_ptr], void, &info.type_interner),
            ),
            (
                release,
                ArType::func(&[handled_ptr], void, &info.type_interner),
            ),
        ] {
            let ty = info.type_interner.intern(ty);
            info.decl_types.insert(symbol, ty);
        }

        symbols
            .exported_functions
            .extend([open, make_raw, consume, release]);
        symbols
            .host_function_names
            .insert(release, arandu_middle::SmolStr::new("handled_release"));
        info.destructors.insert(handled, release);

        let mut docs = DocCommentMap::default();
        // The parser keys a function by its signature span (100..110) and a
        // struct by its declaration span (0..10); both are wider than
        // `Symbol::span`, so containment must be what matches.
        docs.insert(
            NodeKey {
                start: 90,
                end: 120,
            },
            vec![
                "/// Opens a handle.".to_owned(),
                "/// Second line.".to_owned(),
            ],
        );
        docs.insert(
            NodeKey { start: 0, end: 20 },
            vec!["/// A handled resource.".to_owned()],
        );
        docs.insert(
            NodeKey {
                start: 120,
                end: 150,
            },
            vec!["/** A raw handle with */ marker. */".to_owned()],
        );

        (info, symbols, docs)
    }

    #[test]
    fn renders_docs_and_opaque_ownership_contracts() {
        let (info, symbols, docs) = fixture();
        let header = emit_c_header(&info, &symbols, &docs).expect("valid header");

        assert!(
            header.contains(
                "/**\n * A handled resource.\n */\n\
                 /* Opaque handle. Release with handled_release(Handled *) — the exported @Destructor of Handled. */\n\
                 typedef struct Handled Handled;"
            ),
            "{header}"
        );
        assert!(
            header.contains(
                "/**\n * Opens a handle.\n * Second line.\n */\n\
                 /* Returns an owned handle: the caller must release it with handled_release(Handled *). */\n\
                 Handled * handled_open(void);"
            ),
            "{header}"
        );
        assert!(
            header.contains(
                "/**\n * A raw handle with *\\/ marker.\n */\n\
                 /* Returns an owned handle: no exported @Destructor releases it. */\n\
                 Raw * raw_make(void);"
            ),
            "{header}"
        );
        // No destructor was registered for `Raw`, so it must not claim one.
        assert!(
            !header.contains("Release with handled_release(Raw *)"),
            "{header}"
        );
        // `consume` has no docs and returns void: no comment at all above it.
        assert!(header.contains("\nvoid consume(Handled *);"), "{header}");
        assert!(!header.contains("/**\n */"), "{header}");
    }

    #[test]
    fn docs_for_an_imported_symbol_do_not_borrow_local_entries() {
        let (mut info, mut symbols, docs) = fixture();
        // Same structural span, different file: the doc map has no file id, so
        // the containment search must refuse it instead of misattributing text.
        let source = symbols
            .iter()
            .next()
            .expect("fixture defines symbols")
            .clone();
        let imported = SymbolId::new(7, 0);
        let mut symbol = source;
        symbol.id = imported;
        symbol.name = arandu_middle::SmolStr::new("external");
        symbol.kind = SymbolKind::Func;
        symbol.span = Span::new(7, 95, 105);
        symbols.imported_symbols_mut().insert(imported, symbol);

        let void = info.type_interner.intern(ArType::Void);
        let ty = info
            .type_interner
            .intern(ArType::func(&[], void, &info.type_interner));
        info.decl_types.insert(imported, ty);
        symbols.exported_functions.insert(imported);

        let header = emit_c_header(&info, &symbols, &docs).expect("valid header");
        assert!(header.contains("void external(void);"), "{header}");
        assert!(
            !header.contains("Second line.\n */\nvoid external"),
            "{header}"
        );
    }
}
