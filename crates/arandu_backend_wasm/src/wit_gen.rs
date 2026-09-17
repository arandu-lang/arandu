//! WebAssembly Interface Types (WIT) generation from an
//! [`AmirProgram`](arandu_middle::amir::AmirProgram).
//!
//! This module inspects the `public` functions of a compiled
//! [`AmirProgram`](arandu_middle::amir::AmirProgram)
//! and produces a syntactically valid `.wit` text that describes the exported
//! surface of the Arandu module as a WebAssembly Component Model world.
//!
//! # WIT output shape
//!
//! ```wit
//! package arandu:<module-name>@0.1.0;
//!
//! interface exports {
//!   record pixel {
//!     r: u8,
//!     g: u8,
//!     b: u8,
//!     a: u8,
//!   }
//!
//!   apply: func(p: pixel) -> u32;
//!   ...
//! }
//!
//! world <module-name> {
//!   export exports;
//! }
//! ```
//!
//! # Invariants
//!
//! * Output is deterministic: same `AmirProgram` → identical WIT string.
//! * No I/O, no Salsa queries, no `unwrap`/`expect` in production paths.
//! * WIT names are derived from the symbol table in UTF-8 with hyphens
//!   replacing underscores to satisfy WIT identifier rules.
//! * Data-Oriented Design (DoD): zero string-typed models in the intermediate
//!   analysis; types and definitions use strongly-typed IDs and flat pools.

mod emit;
mod lower;
mod model;
mod naming;

pub use emit::generate_wit;
pub use model::{WitPrimitive, WitTypeDef, WitTypeDefKind, WitTypeId, WitTypeKind, WitTypePool};
pub use naming::{extract_method_name, to_wit_ident};

#[cfg(test)]
mod tests {
    use super::lower::map_primitive;
    use super::model::WitPrimitive;
    use super::naming::to_wit_ident;
    use arandu_middle::types::Primitive;

    #[test]
    fn to_wit_ident_replaces_underscores_and_lowercases() {
        assert_eq!(to_wit_ident("my_func"), "my-func");
        assert_eq!(to_wit_ident("MyModule"), "mymodule");
        assert_eq!(to_wit_ident("add_two_numbers"), "add-two-numbers");
        assert_eq!(to_wit_ident("__leading_trailing__"), "leading-trailing");
    }

    #[test]
    fn to_wit_ident_escapes_reserved_keywords() {
        assert_eq!(to_wit_ident("type"), "%type");
        assert_eq!(to_wit_ident("record"), "%record");
        assert_eq!(to_wit_ident("func"), "%func");
        assert_eq!(to_wit_ident("string"), "%string");
    }

    #[test]
    fn map_primitive_covers_all_variants() {
        assert_eq!(map_primitive(Primitive::Bool), WitPrimitive::Bool);
        assert_eq!(map_primitive(Primitive::Str), WitPrimitive::String);
        assert_eq!(map_primitive(Primitive::I32), WitPrimitive::S32);
        assert_eq!(map_primitive(Primitive::U64), WitPrimitive::U64);
        assert_eq!(map_primitive(Primitive::F64), WitPrimitive::F64);
        assert_eq!(map_primitive(Primitive::Byte), WitPrimitive::U8);
    }
}
