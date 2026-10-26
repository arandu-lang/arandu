//! The C surface of an Arandu package.
//!
//! [`header`] renders the `@Export` contract as a C header. It depends only on
//! type information and the symbol table, never on a backend, so the same
//! header describes the object whether the caller chose LLVM, Cranelift, C or
//! WebAssembly. Type mapping itself lives in
//! [`arandu_middle::layout::ffi_type_of`], the function `@Export` validation
//! uses, so the header cannot accept a type the compiler rejects or reject a
//! type the compiler emits.

pub mod header;
