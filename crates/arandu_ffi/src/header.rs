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

use arandu_middle::layout::{
    FfiPosition, FfiType, LayoutEngine, StructLayoutProvider, TypeLayout, c_primitive_name,
    ffi_type_of,
};
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use arandu_middle::{DiagCode, Diagnostic, SymbolId, SymbolTable};
use arandu_typeck::TypeInfo;

/// Renders the header for every function marked `@Export`.
///
/// # Errors
///
/// A signature that has no C representation, a nominal type without
/// `@Repr("C")`, two Arandu types competing for one C name, or an exported
/// function without a recorded signature all become `T049` diagnostics.
pub fn emit_c_header(info: &TypeInfo, symbols: &SymbolTable) -> Result<String, Diagnostic> {
    let context = Context {
        symbols,
        interner: &info.type_interner,
        provider: info,
        pointer_width: u64::from(info.target_pointer_width) / 8,
        layout: LayoutEngine::new(u64::from(info.target_pointer_width) / 8),
    };
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
        let params = context
            .interner
            .type_args(signature.0)
            .iter()
            .map(|id| {
                let ty = context.interner.resolve(*id);
                exports.collect(&context, &ty, FfiPosition::Parameter, symbol)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let return_ty = context.interner.resolve(signature.1);
        let ret = exports.collect(&context, &return_ty, FfiPosition::Return, symbol)?;
        exports.functions.push(Function { name, params, ret });
    }

    exports.render(&context)
}

struct Context<'a> {
    symbols: &'a SymbolTable,
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

struct Function {
    name: String,
    params: Vec<FfiType>,
    ret: FfiType,
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
                let c_name = context.struct_name(*symbol);
                if c_name == "AranduStr"
                    || c_name.starts_with("AranduSlice")
                    || c_name.starts_with("AranduMutSlice")
                {
                    return Err(context.error(
                        owner,
                        format!(
                            "type name '{c_name}' is reserved: the C boundary generates `AranduStr` and `AranduSlice*`/`AranduMutSlice*` typedefs with that prefix"
                        ),
                    ));
                }
                self.structs.push(StructDecl {
                    symbol: *symbol,
                    ty: ty.clone(),
                });
                let fields = context
                    .provider
                    .get_struct_fields_for_type(ty, context.interner)
                    .ok_or_else(|| context.error(owner, "struct has no field table"))?;
                let fields = fields.clone();
                for field in fields.iter() {
                    let field_ty = context.interner.resolve(field.ty);
                    self.collect(context, &field_ty, FfiPosition::StructField, owner)?;
                }
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
            (
                ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner),
                FfiType::Ptr(pointee),
            )
            | (
                ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner),
                FfiType::Ref { pointee, .. },
            ) if !pointee.is_void() => {
                let pointee_ty = context.interner.resolve(*inner);
                if let ArType::Named(..) = pointee_ty {
                    self.collect(context, &pointee_ty, FfiPosition::Parameter, owner)?;
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
        for decl in &self.structs {
            let name = context.struct_name(decl.symbol).to_owned();
            if let Some(existing) = names.insert(name.clone(), decl.symbol)
                && existing != decl.symbol
            {
                return Err(context.error(
                    decl.symbol,
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
            prototypes.push(format!(
                "{} {}({params});",
                context.render(&function.ret)?,
                function.name
            ));
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

        for name in names.keys() {
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
