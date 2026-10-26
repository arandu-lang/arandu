//! Structural mapping from Arandu types to the C boundary.
//!
//! `@Export` validation and the generated header must agree on exactly which
//! types cross the boundary: if one accepted a type the other could not spell,
//! the header would describe an object the compiler never emitted. Both sides
//! therefore go through [`ffi_type_of`] and differ only in how they render the
//! result. Layout is deliberately *not* consulted here; sizes, offsets and ABI
//! classes stay in [`super::export_abi`] and [`crate::LayoutEngine`].

use crate::SymbolId;
use crate::layout::{StructLayoutProvider, c_primitive_name};
use crate::types::{ArType, Primitive, TypeInterner};

/// Where a value appears on the boundary. C arrays only exist as struct
/// fields; a parameter array would silently decay to a pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiPosition {
    Parameter,
    Return,
    StructField,
    /// The consuming receiver of an exported `@Destructor`.
    ///
    /// C cannot pass an opaque type by value, so the receiver crosses as
    /// `T *` even though the Arandu body consumes a `T`. The backend loads the
    /// value from that pointer at the call boundary; the allocation itself
    /// belongs to whoever created the handle.
    OwnedReceiver,
}

/// One value as C sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfiType {
    Void,
    Primitive(Primitive),
    /// `str` crosses as `{const uint8_t *ptr; size_t len;}` with a documented
    /// UTF-8 contract; it is not NUL terminated and is never validated here.
    Str,
    Struct(SymbolId),
    Slice {
        element: Box<FfiType>,
        mutable: bool,
    },
    Ptr(Box<FfiType>),
    Ref {
        pointee: Box<FfiType>,
        mutable: bool,
    },
    Array {
        element: Box<FfiType>,
        len: u64,
    },
}

impl FfiType {
    /// Aggregates are passed under a C ABI rule (lanes, `byval`, `sret`);
    /// pointers and scalars are single registers.
    #[must_use]
    pub fn is_aggregate(&self) -> bool {
        matches!(
            self,
            Self::Str | Self::Struct(_) | Self::Slice { .. } | Self::Array { .. }
        )
    }

    /// Whether the value is the unit type.
    #[must_use]
    pub fn is_void(&self) -> bool {
        matches!(self, Self::Void)
    }
}

/// Maximum nesting of aggregates at the boundary. The limit keeps a recursive
/// `@Repr("C")` graph from turning a diagnostic into a stack overflow.
const MAX_DEPTH: usize = 8;

/// Maps one Arandu type to its C counterpart, rejecting anything whose C
/// representation would require a transformation the backend does not perform.
///
/// # Errors
///
/// Returns a human-readable reason suitable for `T049`; `@Export` validation
/// and the header generator surface the same text, so a user never learns
/// about a boundary rule only after asking for the header.
pub fn ffi_type_of(
    ty: &ArType,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    position: FfiPosition,
    depth: usize,
) -> Result<FfiType, String> {
    if depth > MAX_DEPTH {
        return Err("aggregate nesting is too deep for a C boundary".to_owned());
    }
    match ty {
        ArType::Primitive(Primitive::Str) => Ok(FfiType::Str),
        ArType::Primitive(Primitive::Char) => Err(
            "`char` is a Unicode scalar value, not a C character; use `u32` or `u8`".to_owned(),
        ),
        ArType::Primitive(Primitive::Any) => {
            Err("`any` is a dynamic host value and has no C representation".to_owned())
        }
        // `int`, `uint` and `float` have fixed widths in Arandu (32, 32 and 64
        // bits), and `isize`/`usize` map to the C pointer-width types, so all
        // of them have a deterministic spelling. `c_primitive_name` is the
        // single source of truth for that spelling.
        ArType::Primitive(primitive) => c_primitive_name(*primitive, 8)
            .map(|_| FfiType::Primitive(*primitive))
            .ok_or_else(|| format!("`{primitive:?}` has no C representation")),
        ArType::Void => match position {
            FfiPosition::Return => Ok(FfiType::Void),
            _ => Err("`void` is only valid as the return type".to_owned()),
        },
        ArType::Ptr(inner) => {
            let pointee = pointer_target(*inner, interner, provider, depth)?;
            Ok(FfiType::Ptr(Box::new(pointee)))
        }
        ArType::Ref(inner) | ArType::RefMut(inner) => {
            let pointee = pointer_target(*inner, interner, provider, depth)?;
            Ok(FfiType::Ref {
                pointee: Box::new(pointee),
                mutable: matches!(ty, ArType::RefMut(_)),
            })
        }
        ArType::Slice(element) | ArType::MutSlice(element) => {
            let element_ty = interner.resolve(*element);
            let mapped = ffi_type_of(
                &element_ty,
                interner,
                provider,
                FfiPosition::Parameter,
                depth + 1,
            )
            .map_err(|reason| format!("a slice of {element_ty:?} has no C representation: {reason}"))?;
            if !matches!(mapped, FfiType::Primitive(_) | FfiType::Struct(_)) {
                return Err(
                    "a slice element must be a fixed-width scalar or a `@Repr(\"C\")` struct"
                        .to_owned(),
                );
            }
            Ok(FfiType::Slice {
                element: Box::new(mapped),
                mutable: matches!(ty, ArType::MutSlice(_)),
            })
        }
        ArType::Array(len, element) => {
            if position != FfiPosition::StructField {
                return Err(
                    "arrays decay to pointers outside a struct field; pass a slice or a pointer"
                        .to_owned(),
                );
            }
            if *len == 0 {
                return Err("a zero-length array field has no C representation".to_owned());
            }
            let element_ty = interner.resolve(*element);
            let mapped = ffi_type_of(
                &element_ty,
                interner,
                provider,
                FfiPosition::StructField,
                depth + 1,
            )?;
            if !matches!(mapped, FfiType::Primitive(_) | FfiType::Struct(_)) {
                return Err("an array field must hold scalars or `@Repr(\"C\")` structs".to_owned());
            }
            Ok(FfiType::Array {
                element: Box::new(mapped),
                len: *len,
            })
        }
        ArType::Named(symbol, _) => {
            if position == FfiPosition::OwnedReceiver {
                // An exported destructor receives the handle, not its layout:
                // the C side only ever sees `T *`. Fields may hold anything the
                // Arandu side owns, because C never spells them.
                return Ok(FfiType::Ptr(Box::new(FfiType::Struct(*symbol))));
            }
            if !provider.is_repr_c(*symbol) {
                return Err(
                    "nominal types by value need `@Repr(\"C\")` so their layout is the C layout; add it or pass `ptr T`"
                        .to_owned(),
                );
            }
            let Some(fields) = provider.get_struct_fields_for_type(ty, interner) else {
                return Err(
                    "this nominal type has no concrete field table; instantiate it or pass `ptr T`"
                        .to_owned(),
                );
            };
            if fields.is_empty() {
                return Err("an empty struct has no C representation".to_owned());
            }
            let fields = fields.clone();
            for field in fields.iter() {
                let field_ty = interner.resolve(field.ty);
                ffi_type_of(
                    &field_ty,
                    interner,
                    provider,
                    FfiPosition::StructField,
                    depth + 1,
                )
                .map_err(|reason| format!("field `{}`: {reason}", field.name))?;
            }
            Ok(FfiType::Struct(*symbol))
        }
        ArType::Result(_, _) => Err(
            "the C ABI has no sum types; return a status code (`u32`) and write the payload through `mut ref T`"
                .to_owned(),
        ),
        ArType::Option(_) => Err(
            "the C ABI has no optional values; use a null pointer or a status-code parameter"
                .to_owned(),
        ),
        ArType::Tuple(_) => {
            Err("tuples have no C representation; declare a `@Repr(\"C\")` struct".to_owned())
        }
        ArType::Func(_, _) => Err(
            "function pointers are not supported across the boundary yet; the AMIR call graph is direct"
                .to_owned(),
        ),
        ArType::ConstArray(_, _) => {
            Err("const-generic arrays must be monomorphized before `@Export`".to_owned())
        }
        other => Err(format!("{other:?} has no C representation")),
    }
}

/// Pointee rules for `ptr T`, `ref T` and `mut ref T`.
///
/// A pointer is opaque to C, so it accepts any nominal type without
/// `@Repr("C")`; only the pointee *spelling* matters.
fn pointer_target(
    inner: crate::types::TypeId,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    depth: usize,
) -> Result<FfiType, String> {
    let inner_ty = interner.resolve(inner);
    match &inner_ty {
        ArType::Void => Ok(FfiType::Void),
        ArType::Named(symbol, _) => Ok(FfiType::Struct(*symbol)),
        ArType::Primitive(Primitive::Char) => {
            Err("`char` is a Unicode scalar value, not a C character; use `u32` or `u8`".to_owned())
        }
        ArType::Primitive(primitive) => c_primitive_name(*primitive, 8)
            .map(|_| FfiType::Primitive(*primitive))
            .ok_or_else(|| format!("pointer to `{primitive:?}` has no stable C representation")),
        ArType::Slice(_) | ArType::MutSlice(_) => {
            Err("`ref []T` points at a fat pointer; pass `[]T` by value instead".to_owned())
        }
        other => {
            let _ = (provider, depth);
            Err(format!(
                "pointer to {other:?} has no stable C representation; point at a scalar or a nominal type"
            ))
        }
    }
}

#[cfg(test)]
mod tests;
