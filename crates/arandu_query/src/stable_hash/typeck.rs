//! Type-check and parse-result hashes.

use arandu_middle::SymbolTable;
use arandu_parser::{ParseError, ParseErrorCode, Program};
use arandu_semantics::TypeCheckResult;
use blake3::Hasher;

use super::nodes::{hash_id, hash_span};
use super::symbols::{
    finish, hash_diag, hash_resolved_names, hash_return_borrow_summary, hash_str, hash_symbol_id,
    hash_symbol_table, u32_le, u64_le, StableHash,
};

impl StableHash for TypeCheckResult {
    fn stable_hash(&self) -> blake3::Hash {
        hash_type_check_result(self, true)
    }
}

pub(crate) fn type_signature_hash(result: &TypeCheckResult) -> blake3::Hash {
    hash_type_check_result(result, false)
}

/// Presentation intentionally hides frozen aggregate contents. Memo equality
/// must instead retain the full structural type and canonical constant values.
fn semantic_type_bytes(
    ty: arandu_middle::types::TypeId,
    interner: &arandu_middle::types::TypeInterner,
    symbols: &SymbolTable,
) -> Vec<u8> {
    let canonical = arandu_middle::types::TypeShape::from_id(ty, interner)
        .ok()
        .and_then(|shape| arandu_middle::ctfe::canonical_type_bytes(&shape).ok());
    if let Some(bytes) = canonical {
        let mut identity = vec![0];
        identity.extend(bytes);
        return identity;
    }
    // Preserve recovery for types beyond the admitted structural limits.
    // Such types are diagnosed before lowering; never hash incidental TypeIds.
    let mut identity = vec![1];
    identity.extend(interner.display(ty, symbols).bytes());
    identity
}

fn hash_semantic_type(
    hash: &mut Hasher,
    ty: arandu_middle::types::TypeId,
    interner: &arandu_middle::types::TypeInterner,
    symbols: &SymbolTable,
) {
    let bytes = semantic_type_bytes(ty, interner, symbols);
    hash.update(&u64_le(u64::try_from(bytes.len()).unwrap_or(u64::MAX)));
    hash.update(&bytes);
}

fn hash_type_check_result(result: &TypeCheckResult, include_spans: bool) -> blake3::Hash {
    let mut h = Hasher::new();
    h.update(b"TypeCheckResult/v5");
    h.update(result.type_info.target_identity.os.as_bytes());
    h.update(&[0]);
    h.update(result.type_info.target_identity.arch.as_bytes());
    h.update(&[0, result.type_info.target_pointer_width]);
    // Empty host and wasm identities keep the v5 bytes. A supplied triple is
    // hashed only when the driver stored one.
    let triple = result.type_info.target_identity.triple.as_bytes();
    if !triple.is_empty() {
        h.update(b"\0triple:");
        h.update(triple);
    }
    let layout = result.type_info.target_layout;
    for class in [layout.pointer, layout.float, layout.i64, layout.f64] {
        h.update(&class.size.to_le_bytes());
        h.update(&class.abi_align.to_le_bytes());
    }
    hash_symbol_table(&mut h, &result.symbols, include_spans);
    // `ModuleSignatures` deliberately excludes body references so a private or
    // implementation-only edit does not invalidate importing files.
    if include_spans {
        hash_resolved_names(&mut h, &result.resolved);
    }
    h.update(&u64_le(result.diagnostics.len() as u64));
    for d in &result.diagnostics {
        hash_diag(&mut h, d);
    }
    if include_spans {
        h.update(&u64_le(result.type_info.expr_types.len() as u64));
        for slot in &result.type_info.expr_types {
            match slot {
                Some(tid) => {
                    h.update(&[1]);
                    hash_semantic_type(
                        &mut h,
                        *tid,
                        &result.type_info.type_interner,
                        &result.symbols,
                    );
                }
                None => {
                    h.update(&[0]);
                }
            }
        }
        let mut globals: Vec<_> = result.type_info.ctfe_global_values.iter().collect();
        globals.sort_unstable_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        h.update(b"CtfeGlobals/v1");
        h.update(&u64_le(globals.len() as u64));
        for (symbol, value) in globals {
            hash_symbol_id(&mut h, *symbol);
            h.update(&value.canonical_bytes());
        }
        let mut values: Vec<_> = result.type_info.ctfe_values.iter().collect();
        values.sort_unstable_by_key(|(span, _)| (span.file_id, span.start, span.end));
        h.update(b"CtfeResidual/v2");
        h.update(&u64_le(result.type_info.ctfe_roots.len() as u64));
        for expr in &result.type_info.ctfe_roots {
            hash_id(&mut h, expr.as_usize());
        }
        h.update(&u64_le(values.len() as u64));
        for (span, (value, layout)) in values {
            hash_span(&mut h, *span);
            h.update(&value.canonical_bytes());
            for class in [layout.pointer, layout.float, layout.i64, layout.f64] {
                h.update(&class.size.to_le_bytes());
                h.update(&class.abi_align.to_le_bytes());
            }
        }
    }
    h.update(&u64_le(result.type_info.decl_types.len() as u64));
    let mut decls: Vec<_> = result.type_info.decl_types.iter().collect();
    decls.sort_by_key(|(id, _)| (id.file_id, id.local_id.0));
    for (sid, tid) in decls {
        hash_symbol_id(&mut h, *sid);
        hash_semantic_type(
            &mut h,
            *tid,
            &result.type_info.type_interner,
            &result.symbols,
        );
    }
    let mut borrow_summaries: Vec<_> = result.type_info.return_borrow_summaries.iter().collect();
    borrow_summaries.sort_by_key(|(id, _)| (id.file_id, id.local_id.0));
    h.update(&u64_le(borrow_summaries.len() as u64));
    for (symbol, summary) in borrow_summaries {
        hash_symbol_id(&mut h, *symbol);
        hash_return_borrow_summary(&mut h, summary);
    }

    // These tables are consumed by HIR lowering, monomorphization, layout,
    // ownership checks and codegen. Omitting them lets Salsa treat semantically
    // different type-check results as equal even when names and expression
    // types happen to stay unchanged.
    let info = &result.type_info;
    let symbols = &result.symbols;
    let interner = &info.type_interner;

    let mut deferred: Vec<_> = info.deferred_headers.iter().copied().collect();
    deferred.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(u64::try_from(deferred.len()).unwrap_or(u64::MAX)));
    for symbol in deferred {
        hash_symbol_id(&mut h, symbol);
    }
    let mut headers: Vec<_> = info.concrete_headers.iter().collect();
    headers.sort_by_key(|(key, _)| key.stable_hash().as_bytes().to_owned());
    h.update(&u64_le(u64::try_from(headers.len()).unwrap_or(u64::MAX)));
    for (key, header) in headers {
        h.update(key.stable_hash().as_bytes());
        hash_semantic_type(&mut h, header.signature, interner, symbols);
        h.update(&[u8::from(header.fields.is_some())]);
        if let Some(fields) = &header.fields {
            h.update(&u64_le(u64::try_from(fields.len()).unwrap_or(u64::MAX)));
            for field in fields.iter() {
                hash_str(&mut h, &field.name);
                h.update(&[u8::from(field.symbol.is_some())]);
                if let Some(symbol) = field.symbol {
                    hash_symbol_id(&mut h, symbol);
                }
                h.update(&u64_le(u64::try_from(field.index).unwrap_or(u64::MAX)));
                hash_semantic_type(&mut h, field.ty, interner, symbols);
            }
        }
        h.update(&u64_le(
            u64::try_from(header.variants.len()).unwrap_or(u64::MAX),
        ));
        for (symbol, tag, payload) in &header.variants {
            hash_symbol_id(&mut h, *symbol);
            h.update(&u64_le(u64::try_from(*tag).unwrap_or(u64::MAX)));
            match payload {
                arandu_typeck::type_checker::info::EnumPayloadShape::Unit => {
                    h.update(&[0]);
                }
                arandu_typeck::type_checker::info::EnumPayloadShape::Tuple(items) => {
                    h.update(&[1]);
                    h.update(&u64_le(u64::try_from(items.len()).unwrap_or(u64::MAX)));
                    for &ty in items {
                        hash_semantic_type(&mut h, ty, interner, symbols);
                    }
                }
            }
        }
    }
    let mut requests: Vec<_> = info.header_requests.iter().collect();
    requests.sort_by_key(|(key, _)| key.stable_hash().as_bytes().to_owned());
    h.update(&u64_le(u64::try_from(requests.len()).unwrap_or(u64::MAX)));
    for (key, span) in requests {
        h.update(key.stable_hash().as_bytes());
        hash_span(&mut h, *span);
    }

    let mut struct_fields: Vec<_> = info.struct_fields.iter().collect();
    struct_fields.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(struct_fields.len() as u64));
    for (symbol, fields) in struct_fields {
        hash_symbol_id(&mut h, *symbol);
        h.update(&u64_le(fields.fields.len() as u64));
        for field in &fields.fields {
            hash_str(&mut h, &field.name);
            if let Some(field_symbol) = field.symbol {
                h.update(&[1]);
                hash_symbol_id(&mut h, field_symbol);
            } else {
                h.update(&[0]);
            }
            hash_semantic_type(&mut h, field.ty, interner, symbols);
            h.update(&u64_le(field.index as u64));
        }
    }

    let mut private_fields: Vec<_> = info.private_fields.iter().copied().collect();
    private_fields.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    h.update(b"PrivateFields/v1");
    h.update(&u64_le(private_fields.len() as u64));
    for symbol in private_fields {
        hash_symbol_id(&mut h, symbol);
    }

    let mut enum_variants: Vec<_> = info.enum_variants.iter().collect();
    enum_variants.sort_by_key(|(variant, _)| (variant.file_id, variant.local_id.0));
    h.update(&u64_le(enum_variants.len() as u64));
    for (variant, (enum_symbol, shape)) in enum_variants {
        hash_symbol_id(&mut h, *variant);
        hash_symbol_id(&mut h, *enum_symbol);
        match shape {
            arandu_typeck::type_checker::info::EnumPayloadShape::Unit => {
                h.update(&[0]);
            }
            arandu_typeck::type_checker::info::EnumPayloadShape::Tuple(types) => {
                h.update(&[1]);
                h.update(&u64_le(types.len() as u64));
                for ty in types {
                    hash_semantic_type(&mut h, *ty, interner, symbols);
                }
            }
        };
    }

    let mut enum_tags: Vec<_> = info.enum_variant_tags.iter().collect();
    enum_tags.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(enum_tags.len() as u64));
    for (symbol, tag) in enum_tags {
        hash_symbol_id(&mut h, *symbol);
        h.update(&u64_le(u64::try_from(*tag).unwrap_or(u64::MAX)));
    }

    let mut destructors: Vec<_> = info.destructors.iter().collect();
    destructors.sort_by_key(|(ty, _)| (ty.file_id, ty.local_id.0));
    h.update(&u64_le(destructors.len() as u64));
    for (ty, destructor) in destructors {
        hash_symbol_id(&mut h, *ty);
        hash_symbol_id(&mut h, *destructor);
    }

    if include_spans {
        let mut destructor_instances: Vec<_> = info.destructor_instances.iter().collect();
        destructor_instances
            .sort_by_cached_key(|(ty, _)| semantic_type_bytes(**ty, interner, symbols));
        h.update(&u64_le(destructor_instances.len() as u64));
        for (ty, destructor) in destructor_instances {
            hash_semantic_type(&mut h, *ty, interner, symbols);
            hash_symbol_id(&mut h, *destructor);
        }
    }

    let mut generic_params: Vec<_> = info.generic_params.iter().collect();
    generic_params.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(generic_params.len() as u64));
    for (symbol, params) in generic_params {
        hash_symbol_id(&mut h, *symbol);
        h.update(&u64_le(params.len() as u64));
        for param in params.iter() {
            hash_symbol_id(&mut h, *param);
        }
    }

    let mut generic_defaults: Vec<_> = info.generic_defaults.iter().collect();
    generic_defaults.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(generic_defaults.len() as u64));
    for (symbol, ty) in generic_defaults {
        hash_symbol_id(&mut h, *symbol);
        hash_semantic_type(&mut h, *ty, interner, symbols);
    }

    let mut constraints: Vec<_> = info.param_constraints.iter().collect();
    constraints.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(constraints.len() as u64));
    for (symbol, entries) in constraints {
        hash_symbol_id(&mut h, *symbol);
        h.update(&u64_le(entries.len() as u64));
        for constraint in entries.iter() {
            hash_symbol_id(&mut h, constraint.iface_sym);
            h.update(&u64_le(constraint.type_args.len() as u64));
            for ty in &constraint.type_args {
                hash_semantic_type(&mut h, *ty, interner, symbols);
            }
        }
    }

    let mut interfaces: Vec<_> = info.interfaces.iter().collect();
    interfaces.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(interfaces.len() as u64));
    for (symbol, interface) in interfaces {
        hash_symbol_id(&mut h, *symbol);
        if let Some(self_param) = interface.self_param {
            h.update(&[1]);
            hash_symbol_id(&mut h, self_param);
        } else {
            h.update(&[0]);
        }
        h.update(&u64_le(interface.methods.len() as u64));
        for method in &interface.methods {
            hash_str(&mut h, &method.name);
            hash_semantic_type(&mut h, method.sig_id, interner, symbols);
            h.update(&u64_le(method.generic_params.len() as u64));
            for param in &method.generic_params {
                hash_symbol_id(&mut h, *param);
            }
        }
    }

    if include_spans {
        let mut variant_instantiations: Vec<_> = info.variant_instantiations.iter().collect();
        variant_instantiations.sort_by_cached_key(|((symbol, args), _)| {
            (
                symbol.file_id,
                symbol.local_id.0,
                args.iter()
                    .map(|arg| semantic_type_bytes(*arg, interner, symbols))
                    .collect::<Vec<_>>(),
            )
        });
        h.update(&u64_le(variant_instantiations.len() as u64));
        for ((symbol, args), (params, result_ty)) in variant_instantiations {
            hash_symbol_id(&mut h, *symbol);
            h.update(&u64_le(args.len() as u64));
            for ty in args {
                hash_semantic_type(&mut h, *ty, interner, symbols);
            }
            h.update(&u64_le(params.len() as u64));
            for ty in params {
                hash_semantic_type(&mut h, *ty, interner, symbols);
            }
            hash_semantic_type(&mut h, *result_ty, interner, symbols);
        }
    }

    let mut effects: Vec<_> = info.function_effects.iter().collect();
    effects.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(effects.len() as u64));
    for (symbol, effect) in effects {
        hash_symbol_id(&mut h, *symbol);
        h.update(&effect.0.to_le_bytes());
    }

    let mut unsafe_functions: Vec<_> = info.unsafe_functions.iter().copied().collect();
    unsafe_functions.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(unsafe_functions.len() as u64));
    for symbol in unsafe_functions {
        hash_symbol_id(&mut h, symbol);
    }

    let mut interior_mutable: Vec<_> = info.interior_mutable_types.iter().copied().collect();
    interior_mutable.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(interior_mutable.len() as u64));
    for symbol in interior_mutable {
        hash_symbol_id(&mut h, symbol);
    }

    let mut repr_c: Vec<_> = info.struct_repr_c.iter().copied().collect();
    repr_c.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    h.update(&u64_le(repr_c.len() as u64));
    for symbol in repr_c {
        hash_symbol_id(&mut h, symbol);
    }

    finish(h)
}

impl StableHash for Result<Program, ParseError> {
    fn stable_hash(&self) -> blake3::Hash {
        match self {
            Ok(program) => hash_program(program),
            Err(err) => hash_parse_err(err),
        }
    }
}

impl StableHash for Result<std::sync::Arc<Program>, ParseError> {
    fn stable_hash(&self) -> blake3::Hash {
        match self {
            Ok(program) => hash_program(program),
            Err(err) => hash_parse_err(err),
        }
    }
}

fn hash_program(program: &Program) -> blake3::Hash {
    let mut h = Hasher::new();
    h.update(b"Program/v2");
    h.update(&u32_le(program.span.file_id));
    // The old count-only hash treated equal-shaped programs as equal even when
    // names, literals or operators changed. The canonical AST dump covers every
    // semantic node without relying on unstable `Debug` output.
    h.update(program.dump("").as_bytes());
    // `Program::dump` deliberately presents executable syntax and omits doc
    // attachments, but docs are observable by resolve/IDE queries.
    for doc in &program.docs {
        h.update(&u32_le(doc.span.start));
        h.update(&u32_le(doc.span.end));
        hash_str(&mut h, doc.text.as_str());
        h.update(&u32_le(doc.target_span.start));
        h.update(&u32_le(doc.target_span.end));
    }
    finish(h)
}

fn hash_parse_err(err: &ParseError) -> blake3::Hash {
    let mut h = Hasher::new();
    h.update(&[0]);
    h.update(&[match err.code {
        ParseErrorCode::Lex => 0,
        ParseErrorCode::ExpectedToken => 1,
        ParseErrorCode::ExpectedTopLevelDecl => 2,
        ParseErrorCode::ExpectedExpression => 3,
        ParseErrorCode::ExpectedType => 4,
        ParseErrorCode::ExpectedPlace => 5,
        ParseErrorCode::InvalidResultReturn => 6,
    }]);
    h.update(&u32_le(err.span.start));
    h.update(&u32_le(err.span.end));
    h.update(err.message.as_bytes());
    finish(h)
}
