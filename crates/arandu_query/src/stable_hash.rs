//! Structural hashing for Salsa memo equality (RC-HASHEQ).
//!
//! Never uses full `Debug` of IR graphs — only deterministic fields (IDs,
//! spans, diagnostic codes, counts, ordered maps).

use arandu_middle::{Diagnostic, ResolutionResult, SymbolId, SymbolKind, SymbolTable};
use arandu_parser::{ParseError, ParseErrorCode, Program};
use arandu_semantics::amir::AmirProgram;
use arandu_semantics::TypeCheckResult;
use blake3::Hasher;
use std::sync::Arc;

/// Types that can be content-addressed for [`crate::db::HashEq`].
pub trait StableHash {
    fn stable_hash(&self) -> blake3::Hash;
}

fn finish(hasher: Hasher) -> blake3::Hash {
    hasher.finalize()
}

fn u32_le(n: u32) -> [u8; 4] {
    n.to_le_bytes()
}

fn u64_le(n: u64) -> [u8; 8] {
    n.to_le_bytes()
}

fn hash_str(hasher: &mut Hasher, value: &str) {
    hasher.update(&u64_le(value.len() as u64));
    hasher.update(value.as_bytes());
}

fn hash_borrow_path(hasher: &mut Hasher, path: &arandu_middle::types::BorrowPath) {
    use arandu_middle::types::BorrowPathSegment;

    hasher.update(&u64_le(path.0.len() as u64));
    for segment in &path.0 {
        match segment {
            BorrowPathSegment::Tuple(index) => {
                hasher.update(&[0]);
                hasher.update(&u32_le(*index));
            }
            BorrowPathSegment::Field(name) => {
                hasher.update(&[1]);
                hash_str(hasher, name);
            }
            BorrowPathSegment::Variant(index) => {
                hasher.update(&[2]);
                hasher.update(&u32_le(*index));
            }
            BorrowPathSegment::Payload(index) => {
                hasher.update(&[3]);
                hasher.update(&u32_le(*index));
            }
            BorrowPathSegment::OptionSome => {
                hasher.update(&[4]);
            }
            BorrowPathSegment::ResultOk => {
                hasher.update(&[5]);
            }
            BorrowPathSegment::ResultErr => {
                hasher.update(&[6]);
            }
            BorrowPathSegment::ArrayElement => {
                hasher.update(&[7]);
            }
            BorrowPathSegment::NullableValue => {
                hasher.update(&[8]);
            }
            BorrowPathSegment::CoroutinePayload => {
                hasher.update(&[9]);
            }
            BorrowPathSegment::PollReady => {
                hasher.update(&[10]);
            }
            BorrowPathSegment::RangeElement => {
                hasher.update(&[11]);
            }
        };
    }
}

fn hash_return_borrow_summary(
    hasher: &mut Hasher,
    summary: &arandu_middle::types::ReturnBorrowSummary,
) {
    hasher.update(&u64_le(summary.dependencies.len() as u64));
    for dependency in &summary.dependencies {
        hash_borrow_path(hasher, &dependency.result_path);
        hasher.update(&[match dependency.kind {
            arandu_middle::types::BorrowKind::Shared => 0,
            arandu_middle::types::BorrowKind::Exclusive => 1,
        }]);
        hasher.update(&u64_le(dependency.sources.len() as u64));
        for source in &dependency.sources {
            hasher.update(&u32_le(source.parameter_index));
            hash_borrow_path(hasher, &source.parameter_path);
        }
    }
}

fn hash_diag(hasher: &mut Hasher, d: &Diagnostic) {
    // `as_str` is the allocation-free single source of truth for a DiagCode's
    // public code; formatting the variant name would churn a temporary String.
    hash_str(hasher, d.code.as_str());
    hasher.update(&[d.severity as u8]);
    hasher.update(&u32_le(d.span.file_id));
    hasher.update(&u32_le(d.span.start));
    hasher.update(&u32_le(d.span.end));
    hash_str(hasher, &d.message);
    hasher.update(&u64_le(d.labels.len() as u64));
    for label in &d.labels {
        hasher.update(&u32_le(label.span.file_id));
        hasher.update(&u32_le(label.span.start));
        hasher.update(&u32_le(label.span.end));
        hash_str(hasher, &label.message);
    }
    hasher.update(&u64_le(d.notes.len() as u64));
    for note in &d.notes {
        hash_str(hasher, note);
    }
    hasher.update(&u64_le(d.hints.len() as u64));
    for hint in &d.hints {
        hash_str(hasher, &hint.message);
        if let Some(replacement) = &hint.replacement {
            hasher.update(&[1]);
            hasher.update(&u32_le(replacement.span.file_id));
            hasher.update(&u32_le(replacement.span.start));
            hasher.update(&u32_le(replacement.span.end));
            hash_str(hasher, &replacement.new_text);
        } else {
            hasher.update(&[0]);
        }
    }
}

impl StableHash for Vec<Diagnostic> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for diagnostic in self {
            hash_diag(&mut h, diagnostic);
        }
        finish(h)
    }
}

fn hash_symbol_id(hasher: &mut Hasher, id: SymbolId) {
    hasher.update(&u32_le(id.file_id));
    hasher.update(&u32_le(id.local_id.0));
}

fn hash_resolved_names(hasher: &mut Hasher, resolved: &arandu_middle::ResolvedNames) {
    for entries in [
        &resolved.definitions,
        &resolved.value_refs,
        &resolved.type_refs,
    ] {
        let mut entries: Vec<_> = entries.iter().collect();
        entries.sort_by_key(|(key, _)| (key.start, key.end));
        hasher.update(&u64_le(entries.len() as u64));
        for (key, symbol) in entries {
            hasher.update(&u32_le(key.start));
            hasher.update(&u32_le(key.end));
            hash_symbol_id(hasher, *symbol);
        }
    }
    hasher.update(&u64_le(resolved.expr_symbols.len() as u64));
    for slot in &resolved.expr_symbols {
        if let Some(symbol) = slot {
            hasher.update(&[1]);
            hash_symbol_id(hasher, *symbol);
        } else {
            hasher.update(&[0]);
        }
    }
    let mut mutable: Vec<_> = resolved.mutable_symbols.iter().copied().collect();
    mutable.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    hasher.update(&u64_le(mutable.len() as u64));
    for symbol in mutable {
        hash_symbol_id(hasher, symbol);
    }
}

/// Stable, allocation-free discriminant for [`SymbolKind`].
fn symbol_kind_discriminant(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Module => 0,
        SymbolKind::ImportValue => 1,
        SymbolKind::ImportType => 2,
        SymbolKind::Func => 3,
        SymbolKind::Const => 4,
        SymbolKind::TypeAlias => 5,
        SymbolKind::Struct => 6,
        SymbolKind::Enum => 7,
        SymbolKind::Interface => 8,
        SymbolKind::ExternFunc => 9,
        SymbolKind::Param => 10,
        SymbolKind::Local => 11,
        SymbolKind::Field => 12,
        SymbolKind::EnumVariant => 13,
        SymbolKind::TypeParam => 14,
        SymbolKind::NamespaceMember => 15,
        SymbolKind::AssociatedFunc => 16,
        SymbolKind::ConstParam => 17,
    }
}

fn lang_item_discriminant(item: arandu_middle::symbol_table::LangItem) -> u8 {
    use arandu_middle::symbol_table::LangItem;
    match item {
        LangItem::Option => 0,
        LangItem::OptionSome => 1,
        LangItem::OptionNone => 2,
        LangItem::Result => 3,
        LangItem::ResultOk => 4,
        LangItem::ResultErr => 5,
        LangItem::Poll => 6,
        LangItem::PollReady => 7,
        LangItem::PollPending => 8,
        LangItem::Coroutine => 9,
        LangItem::String => 10,
        LangItem::Vec => 11,
        LangItem::Copy => 12,
        LangItem::Send => 13,
        LangItem::Sync => 14,
        LangItem::TaskHandle => 15,
    }
}

fn hash_symbol(hasher: &mut Hasher, symbol: &arandu_middle::Symbol, include_spans: bool) {
    hash_symbol_id(hasher, symbol.id);
    hash_str(hasher, symbol.name.as_str());
    hasher.update(&[symbol_kind_discriminant(symbol.kind)]);
    if include_spans {
        hasher.update(&u32_le(symbol.span.file_id));
        hasher.update(&u32_le(symbol.span.start));
        hasher.update(&u32_le(symbol.span.end));
    }
    hasher.update(&u32_le(symbol.scope.0));
    hasher.update(&[u8::from(symbol.is_public)]);
    if include_spans {
        if let Some(item) = symbol.lang_item {
            hasher.update(&[1, lang_item_discriminant(item)]);
        } else {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_symbol(hasher: &mut Hasher, symbol: Option<SymbolId>) {
    if let Some(symbol) = symbol {
        hasher.update(&[1]);
        hash_symbol_id(hasher, symbol);
    } else {
        hasher.update(&[0]);
    }
}

fn hash_symbol_table(hasher: &mut Hasher, table: &SymbolTable, include_spans: bool) {
    hasher.update(&u64_le(table.unresolved_module_aliases.len() as u64));
    for alias in &table.unresolved_module_aliases {
        hash_str(hasher, alias);
    }
    let mut symbols: Vec<_> = table.iter().collect();
    symbols.sort_by_key(|symbol| (symbol.id.file_id, symbol.id.local_id.0));
    hasher.update(&u64_le(symbols.len() as u64));
    for symbol in symbols {
        hash_symbol(hasher, symbol, include_spans);
    }
    let mut host_names: Vec<_> = table.host_function_names.iter().collect();
    host_names.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    hasher.update(&u64_le(host_names.len() as u64));
    for (symbol, name) in host_names {
        hash_symbol_id(hasher, *symbol);
        hash_str(hasher, name);
    }

    if include_spans {
        hasher.update(&u32_le(table.file_id));
        hasher.update(&u32_le(table.global_scope().0));

        let scopes: Vec<_> = table.scope_layout().collect();
        hasher.update(&u64_le(scopes.len() as u64));
        for (scope, parent, members) in scopes {
            hasher.update(&u32_le(scope.0));
            if let Some(parent) = parent {
                hasher.update(&[1]);
                hasher.update(&u32_le(parent.0));
            } else {
                hasher.update(&[0]);
            }
            hasher.update(&u64_le(members.len() as u64));
            for member in members {
                hash_symbol_id(hasher, *member);
            }
        }

        let mut imported: Vec<_> = table.imported_symbols.iter().collect();
        imported.sort_by_key(|(id, _)| (id.file_id, id.local_id.0));
        hasher.update(&u64_le(imported.len() as u64));
        for (id, symbol) in imported {
            hash_symbol_id(hasher, *id);
            hash_symbol(hasher, symbol, true);
        }

        let mut module_members: Vec<_> = table.module_members.iter().collect();
        module_members
            .sort_by_key(|((module, member), _)| (module.to_string(), member.to_string()));
        hasher.update(&u64_le(module_members.len() as u64));
        for ((module, member), symbol) in module_members {
            hash_str(hasher, module);
            hash_str(hasher, member);
            hash_symbol_id(hasher, *symbol);
        }

        let mut associated_members: Vec<_> = table.associated_members.iter().collect();
        associated_members
            .sort_by_key(|((owner, name), _)| (owner.file_id, owner.local_id.0, name.to_string()));
        hasher.update(&u64_le(associated_members.len() as u64));
        for ((owner, name), symbol) in associated_members {
            hash_symbol_id(hasher, *owner);
            hash_str(hasher, name);
            hash_symbol_id(hasher, *symbol);
        }

        let mut type_params: Vec<_> = table.type_params.iter().collect();
        type_params.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        hasher.update(&u64_le(type_params.len() as u64));
        for (symbol, params) in type_params {
            hash_symbol_id(hasher, *symbol);
            hasher.update(&u64_le(params.len() as u64));
            for param in params {
                hash_symbol_id(hasher, *param);
            }
        }

        for builtin in [
            table.builtins.option,
            table.builtins.result,
            table.builtins.poll,
            table.builtins.coroutine,
            table.builtins.string,
            table.builtins.vec,
            table.builtin_alloc,
            table.builtin_free,
        ] {
            hash_optional_symbol(hasher, builtin);
        }
    }
}

impl StableHash for ResolutionResult {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&[u8::from(self.is_cycle_fallback)]);
        hash_symbol_table(&mut h, &self.symbols, true);
        h.update(&u64_le(self.diagnostics.len() as u64));
        for d in &self.diagnostics {
            hash_diag(&mut h, d);
        }
        hash_resolved_names(&mut h, &self.resolved);
        let mut docs: Vec<_> = self.docs.iter().collect();
        docs.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, lines) in docs {
            h.update(&u32_le(key.start));
            h.update(&u32_le(key.end));
            for line in lines {
                hash_str(&mut h, line);
            }
        }
        finish(h)
    }
}

impl StableHash for TypeCheckResult {
    fn stable_hash(&self) -> blake3::Hash {
        hash_type_check_result(self, true)
    }
}

pub(crate) fn type_signature_hash(result: &TypeCheckResult) -> blake3::Hash {
    hash_type_check_result(result, false)
}

fn hash_type_check_result(result: &TypeCheckResult, include_spans: bool) -> blake3::Hash {
    let mut h = Hasher::new();
    h.update(b"TypeCheckResult/v2");
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
                    let ty = result.type_info.type_interner.resolve(*tid);
                    hash_str(
                        &mut h,
                        &ty.display(&result.symbols, &result.type_info.type_interner),
                    );
                }
                None => {
                    h.update(&[0]);
                }
            }
        }
    }
    h.update(&u64_le(result.type_info.decl_types.len() as u64));
    let mut decls: Vec<_> = result.type_info.decl_types.iter().collect();
    decls.sort_by_key(|(id, _)| (id.file_id, id.local_id.0));
    for (sid, tid) in decls {
        hash_symbol_id(&mut h, *sid);
        let ty = result.type_info.type_interner.resolve(*tid);
        hash_str(
            &mut h,
            &ty.display(&result.symbols, &result.type_info.type_interner),
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
            hash_str(&mut h, &interner.display(field.ty, symbols));
            h.update(&u64_le(field.index as u64));
        }
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
                    hash_str(&mut h, &interner.display(*ty, symbols));
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
        destructor_instances.sort_by_key(|(ty, _)| ty.as_usize());
        h.update(&u64_le(destructor_instances.len() as u64));
        for (ty, destructor) in destructor_instances {
            hash_str(&mut h, &interner.display(*ty, symbols));
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
        hash_str(&mut h, &interner.display(*ty, symbols));
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
                hash_str(&mut h, &interner.display(*ty, symbols));
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
            hash_str(&mut h, &interner.display(method.sig_id, symbols));
            h.update(&u64_le(method.generic_params.len() as u64));
            for param in &method.generic_params {
                hash_symbol_id(&mut h, *param);
            }
        }
    }

    if include_spans {
        let mut variant_instantiations: Vec<_> = info.variant_instantiations.iter().collect();
        variant_instantiations.sort_by_key(|((symbol, args), _)| {
            (
                symbol.file_id,
                symbol.local_id.0,
                args.iter().map(|arg| arg.as_usize()).collect::<Vec<_>>(),
            )
        });
        h.update(&u64_le(variant_instantiations.len() as u64));
        for ((symbol, args), (params, result_ty)) in variant_instantiations {
            hash_symbol_id(&mut h, *symbol);
            h.update(&u64_le(args.len() as u64));
            for ty in args {
                hash_str(&mut h, &interner.display(*ty, symbols));
            }
            h.update(&u64_le(params.len() as u64));
            for ty in params {
                hash_str(&mut h, &interner.display(*ty, symbols));
            }
            hash_str(&mut h, &interner.display(*result_ty, symbols));
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

impl StableHash for AmirProgram {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"AmirProgram/v3");
        h.update(&u64_le(self.funcs.len() as u64));
        for f in &self.funcs {
            h.update(f.stable_hash().as_bytes());
        }
        h.update(&u64_le(self.literal_pool.entries.len() as u64));
        for entry in &self.literal_pool.entries {
            match entry {
                arandu_middle::literal_pool::AmirLiteralEntry::Int(s) => {
                    h.update(&[0]);
                    hash_str(&mut h, s);
                }
                arandu_middle::literal_pool::AmirLiteralEntry::Float(s) => {
                    h.update(&[1]);
                    hash_str(&mut h, s);
                }
                arandu_middle::literal_pool::AmirLiteralEntry::Str(s) => {
                    h.update(&[2]);
                    hash_str(&mut h, s);
                }
                arandu_middle::literal_pool::AmirLiteralEntry::Char(s) => {
                    h.update(&[3]);
                    hash_str(&mut h, s);
                }
            }
        }
        // HashMap iteration order must not influence the content hash.
        let mut externs: Vec<_> = self.extern_funcs.iter().collect();
        externs.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        h.update(&u64_le(externs.len() as u64));
        for (symbol, (params, result)) in externs {
            hash_symbol_id(&mut h, *symbol);
            h.update(&u64_le(params.len() as u64));
            for ty in params {
                hash_ar_type(&mut h, ty);
            }
            hash_ar_type(&mut h, result);
        }
        h.update(&u64_le(self.debug_bindings.len() as u64));
        for binding in &self.debug_bindings {
            hash_symbol_id(&mut h, binding.function);
            hash_id(&mut h, binding.temp.as_usize());
            hash_id(&mut h, binding.local.as_usize());
        }
        h.update(&u64_le(self.debug_blocks.len() as u64));
        for block in &self.debug_blocks {
            hash_symbol_id(&mut h, block.function);
            hash_id(&mut h, block.block.as_usize());
            h.update(&u32_le(block.span.file_id));
            h.update(&u32_le(block.span.start));
            h.update(&u32_le(block.span.end));
        }
        finish(h)
    }
}

impl StableHash for crate::passes::LowerAmirArtifacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(self.amir.stable_hash().as_bytes());
        h.update(self.type_check.stable_hash().as_bytes());
        finish(h)
    }
}

impl StableHash for crate::passes::BorrowInterfaces {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hasher = Hasher::new();
        hasher.update(&u64_le(self.entries.len() as u64));
        for (symbol, summary) in &self.entries {
            hash_symbol_id(&mut hasher, *symbol);
            hash_return_borrow_summary(&mut hasher, summary);
        }
        finish(hasher)
    }
}

impl StableHash for petgraph::Graph<u32, ()> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.node_count() as u64));
        h.update(&u64_le(self.edge_count() as u64));
        let mut nodes: Vec<u32> = self.node_weights().copied().collect();
        nodes.sort_unstable();
        for n in nodes {
            h.update(&u32_le(n));
        }
        let mut edges: Vec<(u32, u32)> = self
            .edge_indices()
            .filter_map(|e| {
                let (source, target) = self.edge_endpoints(e)?;
                let s_weight = *self.node_weight(source)?;
                let t_weight = *self.node_weight(target)?;
                Some((s_weight, t_weight))
            })
            .collect();
        edges.sort_unstable();
        for (s, t) in edges {
            h.update(&u32_le(s));
            h.update(&u32_le(t));
        }
        finish(h)
    }
}

impl StableHash for arandu_middle::amir::AmirFunc {
    fn stable_hash(&self) -> blake3::Hash {
        use arandu_middle::amir::AmirReceiver;
        use arandu_middle::hir::ReceiverKind;

        let mut h = Hasher::new();
        h.update(b"AmirFunc/v2");
        hash_symbol_id(&mut h, self.symbol);
        hash_id(&mut h, self.return_type.as_usize());
        match self.receiver {
            None => {
                h.update(&[0]);
            }
            Some(AmirReceiver { temp, kind }) => {
                h.update(&[
                    1,
                    match kind {
                        ReceiverKind::Shared => 0,
                        ReceiverKind::Mut => 1,
                        ReceiverKind::Own => 2,
                    },
                ]);
                hash_id(&mut h, temp.as_usize());
            }
        };
        h.update(&u64_le(self.params.len() as u64));
        for param in &self.params {
            hash_id(&mut h, param.as_usize());
        }
        h.update(&u64_le(self.blocks.len() as u64));
        h.update(&u64_le(self.locals.len() as u64));
        h.update(&u64_le(self.temps.len() as u64));
        h.update(&u64_le(self.stmts.kinds.len() as u64));
        for kind in self.stmts.kinds.raw.iter() {
            h.update(&[match kind {
                arandu_middle::amir::AmirStmtKind::Assign => 0,
                arandu_middle::amir::AmirStmtKind::Store => 1,
                arandu_middle::amir::AmirStmtKind::Call => 2,
                arandu_middle::amir::AmirStmtKind::Free => 3,
                arandu_middle::amir::AmirStmtKind::StorageLive => 4,
                arandu_middle::amir::AmirStmtKind::StorageDead => 5,
                arandu_middle::amir::AmirStmtKind::Destroy => 6,
                arandu_middle::amir::AmirStmtKind::Nop => 7,
            }]);
        }
        h.update(&u64_le(self.stmts.payloads.len() as u64));
        for local in &self.locals {
            hash_id(&mut h, local.id.as_usize());
            hash_id(&mut h, local.ty.as_usize());
            h.update(&[u8::from(local.is_memory)]);
            hash_optional_symbol_id(&mut h, local.symbol);
            hash_span(&mut h, local.span);
            hash_optional_span(&mut h, local.use_span);
        }
        for temp in &self.temps {
            hash_id(&mut h, temp.id.as_usize());
            hash_id(&mut h, temp.ty.as_usize());
            h.update(&[u8::from(temp.is_copy), u8::from(temp.is_nullable)]);
            hash_span(&mut h, temp.span);
        }
        h.update(&u64_le(self.block_params.len() as u64));
        for param in &self.block_params {
            hash_id(&mut h, param.id.as_usize());
            hash_id(&mut h, param.local.as_usize());
            hash_id(&mut h, param.ty.as_usize());
            match &param.from {
                Some(from) => {
                    h.update(&[1]);
                    hash_str(&mut h, from);
                }
                None => {
                    h.update(&[0]);
                }
            }
            h.update(&[u8::from(param.moved)]);
        }
        for b in &self.blocks {
            hash_id(&mut h, b.id.as_usize());
            hash_id(&mut h, b.statements.start as usize);
            hash_id(&mut h, b.statements.len as usize);
            // Block params live in the dense `block_params` pool; hash the
            // range so a change in block signature invalidates the hash.
            hash_id(&mut h, b.params.start as usize);
            hash_id(&mut h, b.params.len as usize);
            hash_terminator(&mut h, &b.terminator);
        }
        for id in self.stmts.iter_ids() {
            if let Some(stmt) = self.stmts.get(id) {
                hash_stmt(&mut h, stmt);
            }
        }
        hash_id(&mut h, self.cfg.successors.len());
        for successors in &self.cfg.successors {
            hash_id(&mut h, successors.len());
            for block in successors {
                hash_id(&mut h, block.as_usize());
            }
        }
        hash_id(&mut h, self.cfg.predecessors.len());
        for predecessors in &self.cfg.predecessors {
            hash_id(&mut h, predecessors.len());
            for block in predecessors {
                hash_id(&mut h, block.as_usize());
            }
        }
        finish(h)
    }
}

fn hash_id(hasher: &mut Hasher, id: usize) {
    hasher.update(&u64_le(id as u64));
}

fn hash_ar_type(hasher: &mut Hasher, ty: &arandu_middle::types::ArType) {
    use arandu_middle::types::{ArType, Primitive};

    match ty {
        ArType::Primitive(primitive) => {
            hasher.update(&[
                0,
                match primitive {
                    Primitive::Int => 0,
                    Primitive::Uint => 1,
                    Primitive::Float => 2,
                    Primitive::I8 => 3,
                    Primitive::I16 => 4,
                    Primitive::I32 => 5,
                    Primitive::I64 => 6,
                    Primitive::U8 => 7,
                    Primitive::U16 => 8,
                    Primitive::U32 => 9,
                    Primitive::U64 => 10,
                    Primitive::F32 => 11,
                    Primitive::F64 => 12,
                    Primitive::Bool => 13,
                    Primitive::Byte => 14,
                    Primitive::Char => 15,
                    Primitive::Str => 16,
                    Primitive::Any => 17,
                },
            ]);
        }
        ArType::Named(symbol, args) => {
            hasher.update(&[1]);
            hash_symbol_id(hasher, *symbol);
            hash_index_range(hasher, *args);
        }
        ArType::Func(params, result) => {
            hasher.update(&[2]);
            hash_index_range(hasher, *params);
            hash_id(hasher, result.as_usize());
        }
        ArType::Nullable(inner) => {
            hasher.update(&[3]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Slice(inner) => {
            hasher.update(&[4]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Array(len, inner) => {
            hasher.update(&[5]);
            hasher.update(&len.to_le_bytes());
            hash_id(hasher, inner.as_usize());
        }
        ArType::ConstArray(symbol, inner) => {
            hasher.update(&[6]);
            hash_symbol_id(hasher, *symbol);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Const(value) => {
            hasher.update(&[7]);
            hasher.update(&value.to_le_bytes());
        }
        ArType::ConstParam(symbol) => {
            hasher.update(&[8]);
            hash_symbol_id(hasher, *symbol);
        }
        ArType::Ptr(inner) => {
            hasher.update(&[9]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Ref(inner) => {
            hasher.update(&[10]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::RefMut(inner) => {
            hasher.update(&[11]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::GenRef => {
            hasher.update(&[12]);
        }
        ArType::Tuple(items) => {
            hasher.update(&[13]);
            hash_index_range(hasher, *items);
        }
        ArType::Result(ok, err) => {
            hasher.update(&[14]);
            hash_id(hasher, ok.as_usize());
            hash_id(hasher, err.as_usize());
        }
        ArType::Option(inner) => {
            hasher.update(&[15]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Coroutine(inner) => {
            hasher.update(&[16]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Poll(inner) => {
            hasher.update(&[17]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Range(inner) => {
            hasher.update(&[18]);
            hash_id(hasher, inner.as_usize());
        }
        ArType::Err => {
            hasher.update(&[19]);
        }
        ArType::Void => {
            hasher.update(&[20]);
        }
        ArType::IntLiteral => {
            hasher.update(&[21]);
        }
        ArType::FloatLiteral => {
            hasher.update(&[22]);
        }
        ArType::Error => {
            hasher.update(&[23]);
        }
    }
}

fn hash_index_range(hasher: &mut Hasher, range: arandu_middle::hir::IndexRange) {
    hasher.update(&u32_le(range.start));
    hasher.update(&u32_le(range.len));
}

fn hash_span(hasher: &mut Hasher, span: arandu_lexer::Span) {
    hasher.update(&u32_le(span.file_id));
    hasher.update(&u32_le(span.start));
    hasher.update(&u32_le(span.end));
}

fn hash_optional_span(hasher: &mut Hasher, span: Option<arandu_lexer::Span>) {
    match span {
        Some(span) => {
            hasher.update(&[1]);
            hash_span(hasher, span);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_symbol_id(hasher: &mut Hasher, symbol: Option<SymbolId>) {
    match symbol {
        Some(symbol) => {
            hasher.update(&[1]);
            hash_symbol_id(hasher, symbol);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_terminator(hasher: &mut Hasher, term: &arandu_middle::amir::AmirTerminator) {
    use arandu_middle::amir::AmirTerminator;
    match term {
        AmirTerminator::Return => {
            hasher.update(&[0]);
        }
        AmirTerminator::Unreachable => {
            hasher.update(&[1]);
        }
        AmirTerminator::Goto { target, args } => {
            hasher.update(&[2]);
            hash_id(hasher, target.as_usize());
            hash_operands(hasher, args);
        }
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, condition);
            hash_id(hasher, if_true.as_usize());
            hash_operands(hasher, true_args);
            hash_id(hasher, if_false.as_usize());
            hash_operands(hasher, false_args);
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            hasher.update(&[4]);
            hash_amir_operand(hasher, discriminant);
            hash_id(hasher, targets.len());
            for (value, target, args) in targets {
                hasher.update(&value.to_le_bytes());
                hash_id(hasher, target.as_usize());
                hash_operands(hasher, args);
            }
            hash_id(hasher, otherwise.0.as_usize());
            hash_operands(hasher, &otherwise.1);
        }
        AmirTerminator::Suspend {
            future,
            resume,
            args,
        } => {
            hasher.update(&[5]);
            hash_amir_operand(hasher, future);
            hash_id(hasher, resume.as_usize());
            hash_operands(hasher, args);
        }
    }
}

fn hash_operands(hasher: &mut Hasher, operands: &[arandu_middle::amir::AmirOperand]) {
    hash_id(hasher, operands.len());
    for operand in operands {
        hash_amir_operand(hasher, operand);
    }
}

fn hash_place(hasher: &mut Hasher, place: &arandu_middle::amir::AmirPlace) {
    use arandu_middle::amir::AmirProjection;
    hash_id(hasher, place.local.as_usize());
    hash_id(hasher, place.projections.len());
    for projection in &place.projections {
        match projection {
            AmirProjection::Field(symbol) => {
                hasher.update(&[0]);
                hash_symbol_id(hasher, *symbol);
            }
            AmirProjection::Index(operand) => {
                hasher.update(&[1]);
                hash_amir_operand(hasher, operand);
            }
            AmirProjection::Deref => {
                hasher.update(&[2]);
            }
        }
    }
}

fn hash_stmt(hasher: &mut Hasher, stmt: &arandu_middle::amir::AmirStmt) {
    use arandu_middle::amir::AmirStmt;
    match stmt {
        AmirStmt::Assign { lhs, rhs } => {
            hasher.update(&[0]);
            hash_id(hasher, lhs.as_usize());
            hash_rvalue(hasher, rhs);
        }
        AmirStmt::Store { lhs, rhs } => {
            hasher.update(&[1]);
            hash_place(hasher, lhs);
            hash_amir_operand(hasher, rhs);
        }
        AmirStmt::Call {
            lhs,
            callee,
            args,
            return_borrow,
        } => {
            hasher.update(&[2]);
            match lhs {
                Some(id) => {
                    hasher.update(&[1]);
                    hash_id(hasher, id.as_usize());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
            hash_amir_operand(hasher, callee);
            hash_operands(hasher, args);
            match return_borrow {
                Some(summary) => {
                    hasher.update(&[1]);
                    hash_return_borrow_summary(hasher, summary);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        AmirStmt::Free(value) => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, value);
        }
        AmirStmt::StorageLive(local) => {
            hasher.update(&[4]);
            hash_id(hasher, local.as_usize());
        }
        AmirStmt::StorageDead(local) => {
            hasher.update(&[5]);
            hash_id(hasher, local.as_usize());
        }
        AmirStmt::Destroy(place) => {
            hasher.update(&[6]);
            hash_place(hasher, place);
        }
        AmirStmt::Nop => {
            hasher.update(&[7]);
        }
    }
}

fn hash_rvalue(hasher: &mut Hasher, value: &arandu_middle::amir::AmirRvalue) {
    use arandu_middle::amir::AmirRvalue as R;
    match value {
        R::Use(op) => {
            hasher.update(&[0]);
            hash_amir_operand(hasher, op);
        }
        R::Binary { op, left, right } => {
            hasher.update(&[1, op.stable_tag()]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
        }
        R::Unary { op, operand } => {
            hasher.update(&[2, op.stable_tag()]);
            hash_amir_operand(hasher, operand);
        }
        R::FieldAccess { base, field } => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, base);
            hash_id(hasher, *field);
        }
        R::StructLiteral {
            struct_symbol,
            fields,
        } => {
            hasher.update(&[4]);
            hash_symbol_id(hasher, *struct_symbol);
            hash_id(hasher, fields.len());
            for (name, operand) in fields {
                hash_str(hasher, name);
                hash_amir_operand(hasher, operand);
            }
        }
        R::IndexAccess { base, index } => {
            hasher.update(&[5]);
            hash_amir_operand(hasher, base);
            hash_amir_operand(hasher, index);
        }
        R::Array { items } => {
            hasher.update(&[6]);
            hash_operands(hasher, items);
        }
        R::Tuple { items } => {
            hasher.update(&[7]);
            hash_operands(hasher, items);
        }
        R::Discriminant { value } => {
            hasher.update(&[8]);
            hash_amir_operand(hasher, value);
        }
        R::EnumPayload {
            value,
            variant,
            index,
        } => {
            hasher.update(&[9]);
            hash_amir_operand(hasher, value);
            hash_symbol_id(hasher, *variant);
            hash_id(hasher, *index);
        }
        R::EnumConstruct {
            variant_tag,
            payload,
        } => {
            hasher.update(&[10]);
            hash_id(hasher, *variant_tag);
            match payload {
                Some(op) => {
                    hasher.update(&[1]);
                    hash_amir_operand(hasher, op);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        R::Len(op) => {
            hasher.update(&[11]);
            hash_amir_operand(hasher, op);
        }
        R::SliceData(op) => {
            hasher.update(&[12]);
            hash_amir_operand(hasher, op);
        }
        R::SliceView { owner, data, len } => {
            hasher.update(&[13]);
            hash_amir_operand(hasher, owner);
            hash_amir_operand(hasher, data);
            hash_amir_operand(hasher, len);
        }
        R::SliceSubslice { slice, start, len } => {
            hasher.update(&[14]);
            hash_amir_operand(hasher, slice);
            hash_amir_operand(hasher, start);
            hash_amir_operand(hasher, len);
        }
        R::StrBytes { source } => {
            hasher.update(&[15]);
            hash_amir_operand(hasher, source);
        }
        R::StrView { owner } => {
            hasher.update(&[16]);
            hash_amir_operand(hasher, owner);
        }
        R::Alloc(op) => {
            hasher.update(&[17]);
            hash_amir_operand(hasher, op);
        }
        R::Load(place) => {
            hasher.update(&[18]);
            hash_place(hasher, place);
        }
        R::Borrow(place) => {
            hasher.update(&[19]);
            hash_place(hasher, place);
        }
        R::BorrowMut(place) => {
            hasher.update(&[20]);
            hash_place(hasher, place);
        }
        R::RelativeBorrow { local, mutable } => {
            hasher.update(&[21, u8::from(*mutable)]);
            hash_id(hasher, local.as_usize());
        }
        R::CoroutineReady {
            value,
            payload_ty,
            stack,
        } => {
            hasher.update(&[22, u8::from(*stack)]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, payload_ty.as_usize());
        }
        R::GenInsert {
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[23]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, value);
        }
        R::GenGet {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[24]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
        }
        R::GenSet {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[25]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
            hash_amir_operand(hasher, value);
        }
        R::GenUpsert {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[26]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
            hash_amir_operand(hasher, value);
        }
        R::GenRemove {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[27]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
        }
        R::StringInterp { parts } => {
            hasher.update(&[28]);
            hash_operands(hasher, parts);
        }
        R::ToStr { value, src_ty } => {
            hasher.update(&[29]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, src_ty.as_usize());
        }
        R::BlackBox { value, value_ty } => {
            hasher.update(&[30]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, value_ty.as_usize());
        }
    }
}

fn hash_gen_meta(
    hasher: &mut Hasher,
    ty: arandu_middle::types::TypeId,
    arena: arandu_middle::amir::GenArenaDomain,
    origin: arandu_lexer::Span,
) {
    hasher.update(&[match arena {
        arandu_middle::amir::GenArenaDomain::CompilerManaged => 0,
    }]);
    hash_id(hasher, ty.as_usize());
    hash_span(hasher, origin);
}

fn hash_amir_operand(hasher: &mut Hasher, operand: &arandu_middle::amir::AmirOperand) {
    use arandu_middle::amir::{AmirConstant, AmirOperand};

    match operand {
        AmirOperand::Copy(temp) => {
            hasher.update(&[0]);
            hasher.update(&u64_le(temp.as_usize() as u64));
        }
        AmirOperand::Move(temp) => {
            hasher.update(&[1]);
            hasher.update(&u64_le(temp.as_usize() as u64));
        }
        AmirOperand::Constant(AmirConstant::Pool(id)) => {
            hasher.update(&[2]);
            hasher.update(&u32_le(id.0));
        }
        AmirOperand::Constant(AmirConstant::Bool(value)) => {
            hasher.update(&[3, u8::from(*value)]);
        }
        AmirOperand::Constant(AmirConstant::Nil) => {
            hasher.update(&[4]);
        }
        AmirOperand::FunctionRef(symbol) => {
            hasher.update(&[5]);
            hash_symbol_id(hasher, *symbol);
        }
        AmirOperand::GlobalRef(symbol) => {
            hasher.update(&[6]);
            hash_symbol_id(hasher, *symbol);
        }
    }
}

impl StableHash for crate::dataflow::DataflowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"DataflowFacts/v1");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.live_in_count));
        h.update(&u32_le(self.live_out_count));
        h.update(&u32_le(self.init_in_count));
        h.update(&u32_le(self.moved_in_count));
        h.update(&u32_le(self.stmt_count));
        finish(h)
    }
}

impl StableHash for crate::dataflow::BorrowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BorrowFacts/v2");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.shared_in_count));
        h.update(&u32_le(self.exclusive_in_count));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out_count));
        h.update(&u32_le(self.exclusive_out_count));
        finish(h)
    }
}

impl StableHash for arandu_mir::borrow_facts::BlockBorrowSummary {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BlockBorrowSummary/v2");
        h.update(&u32_le(self.shared_in));
        h.update(&u32_le(self.exclusive_in));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out));
        h.update(&u32_le(self.exclusive_out));
        finish(h)
    }
}

impl StableHash for Vec<arandu_mir::borrow_facts::BlockBorrowSummary> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"Vec<BlockBorrowSummary>/v1");
        h.update(&u64_le(self.len() as u64));
        for s in self {
            h.update(s.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::LivenessMap {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"LivenessMap/v1");
        h.update(&u64_le(self.live_in_counts.len() as u64));
        for &c in &self.live_in_counts {
            h.update(&u32_le(c));
        }
        for &c in &self.live_out_counts {
            h.update(&u32_le(c));
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::IdeDiagnostic {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(self.code.as_bytes());
        h.update(&[self.severity]);
        h.update(self.message.as_bytes());
        h.update(&u32_le(self.file_id));
        h.update(&u32_le(self.start));
        h.update(&u32_le(self.end));
        h.update(&u64_le(self.labels.len() as u64));
        for label in &self.labels {
            h.update(&u32_le(label.file_id));
            h.update(&u32_le(label.start));
            h.update(&u32_le(label.end));
            hash_str(&mut h, &label.message);
        }
        h.update(&u64_le(self.notes.len() as u64));
        for note in &self.notes {
            hash_str(&mut h, note);
        }
        h.update(&u64_le(self.hints.len() as u64));
        for hint in &self.hints {
            hash_str(&mut h, &hint.message);
            if let Some(replacement) = &hint.replacement {
                h.update(&[1]);
                h.update(&u32_le(replacement.file_id));
                h.update(&u32_le(replacement.start));
                h.update(&u32_le(replacement.end));
                hash_str(&mut h, &replacement.new_text);
            } else {
                h.update(&[0]);
            }
        }
        if let Some(f) = self.func {
            h.update(&[1]);
            hash_symbol_id(&mut h, f);
        } else {
            h.update(&[0]);
        }
        if let Some(b) = self.block {
            h.update(&[1]);
            h.update(&u32_le(b.as_usize() as u32));
        } else {
            h.update(&[0]);
        }
        finish(h)
    }
}

impl StableHash for Vec<crate::dataflow::IdeDiagnostic> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for d in self {
            h.update(d.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for Vec<arandu_middle::SymbolId> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for id in self {
            hash_symbol_id(&mut h, *id);
        }
        finish(h)
    }
}

impl StableHash for crate::passes::ItemSourceInput {
    fn stable_hash(&self) -> blake3::Hash {
        // Content-address only this item's source fingerprint — not the whole Program.
        let mut h = Hasher::new();
        h.update(b"ItemSourceInput/v3");
        hash_symbol_id(&mut h, self.item_sym);
        h.update(&u32_le(self.item_start));
        h.update(self.body_fp.as_bytes());
        finish(h)
    }
}

impl StableHash for Arc<[crate::highlight::HlToken]> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"HlTokenSlice/v1");
        h.update(&u64_le(self.len() as u64));
        for t in self.iter() {
            h.update(&u32_le(t.start));
            h.update(&u32_le(t.end));
            h.update(&[t.kind as u8]);
            h.update(&u32_le(u32::from(t.mods)));
        }
        finish(h)
    }
}

impl StableHash for arandu_parser::SyntaxTree {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"SyntaxTree/v2");
        h.update(self.text().as_bytes());
        // Hash ranges (no per-item String alloc).
        let ranges = self.item_ranges();
        h.update(&u64_le(ranges.len() as u64));
        let text = self.text();
        let bytes = text.as_bytes();
        for (s, e) in ranges {
            h.update(&u32_le(s));
            h.update(&u32_le(e));
            let s = (s as usize).min(bytes.len());
            let e = (e as usize).min(bytes.len()).max(s);
            h.update(&bytes[s..e]);
        }
        finish(h)
    }
}

#[cfg(test)]
mod tests {
    use super::{type_signature_hash, StableHash};

    fn amir_func(terminator: arandu_middle::amir::AmirTerminator) -> arandu_middle::amir::AmirFunc {
        use arandu_middle::amir::{AmirFunc, AmirStmtTable};
        use arandu_middle::layout::DenseRange;
        use arandu_middle::types::{ArType, TypeInterner};

        let interner = TypeInterner::new();
        AmirFunc {
            symbol: arandu_middle::SymbolId::new(0, 0),
            return_type: interner.intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks: vec![arandu_middle::amir::AmirBasicBlock {
                id: arandu_middle::amir::BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator,
            }],
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg: arandu_middle::cfg::ControlFlowGraph::default(),
        }
    }

    #[test]
    fn resolution_hash_distinguishes_symbol_kind_with_identical_name_and_span() {
        use arandu_middle::{ResolutionResult, Span, SymbolKind};
        let result = |kind| {
            let mut result = ResolutionResult::cycle_fallback();
            let scope = result.symbols.global_scope();
            std::sync::Arc::make_mut(&mut result.symbols)
                .define(scope, "value", kind, Span::new(0, 0, 5))
                .unwrap();
            result
        };
        let local = result(SymbolKind::Local);
        let parameter = result(SymbolKind::Param);
        assert_ne!(local.stable_hash(), parameter.stable_hash());
        assert_eq!(local.stable_hash(), result(SymbolKind::Local).stable_hash());
    }

    #[test]
    fn program_hash_changes_when_only_a_literal_changes() {
        let first = arandu_parser::parse("func answer(): int { return 1 }").unwrap();
        let second = arandu_parser::parse("func answer(): int { return 2 }").unwrap();
        assert_ne!(Ok(first).stable_hash(), Ok(second).stable_hash());
    }

    #[test]
    fn graph_hash_changes_when_edge_changes() {
        let mut g1 = petgraph::Graph::<u32, ()>::new();
        let n0 = g1.add_node(1);
        let n1 = g1.add_node(2);
        let n2 = g1.add_node(3);
        g1.add_edge(n0, n1, ());
        g1.add_edge(n1, n2, ());

        let mut g2 = petgraph::Graph::<u32, ()>::new();
        let n0 = g2.add_node(1);
        let n1 = g2.add_node(2);
        let n2 = g2.add_node(3);
        g2.add_edge(n0, n2, ());
        g2.add_edge(n1, n2, ());

        assert_ne!(g1.stable_hash(), g2.stable_hash());
    }

    #[test]
    fn amir_program_hash_changes_when_literal_pool_changes() {
        let mut p1 = arandu_middle::amir::AmirProgram {
            funcs: Vec::new(),
            literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
            extern_funcs: rustc_hash::FxHashMap::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        let mut p2 = arandu_middle::amir::AmirProgram {
            funcs: Vec::new(),
            literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
            extern_funcs: rustc_hash::FxHashMap::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        p1.literal_pool.intern_int("42");
        p2.literal_pool.intern_int("43");
        assert_ne!(p1.stable_hash(), p2.stable_hash());
    }

    #[test]
    fn amir_hash_tracks_statement_operands_and_terminator_arguments() {
        use arandu_middle::amir::{AmirConstant, AmirOperand, AmirStmt, AmirTerminator, BlockId};

        let mut first = amir_func(AmirTerminator::Goto {
            target: BlockId::from_usize(0),
            args: vec![AmirOperand::Constant(AmirConstant::Bool(false))],
        });
        let second = amir_func(AmirTerminator::Goto {
            target: BlockId::from_usize(0),
            args: vec![AmirOperand::Constant(AmirConstant::Bool(true))],
        });
        assert_ne!(first.stable_hash(), second.stable_hash());

        first
            .stmts
            .push(AmirStmt::Free(AmirOperand::Constant(AmirConstant::Bool(
                false,
            ))));
        let mut changed_stmt = first.clone();
        *changed_stmt
            .stmts
            .get_mut(arandu_middle::amir::InstrId::from_usize(0))
            .unwrap() = AmirStmt::Free(AmirOperand::Constant(AmirConstant::Bool(true)));
        assert_ne!(first.stable_hash(), changed_stmt.stable_hash());
    }

    #[test]
    fn amir_program_hash_tracks_debug_bindings() {
        use arandu_middle::amir::{AmirDebugBinding, AmirProgram, LocalId, TempId};

        let make_program = |local| AmirProgram {
            funcs: Vec::new(),
            literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
            extern_funcs: rustc_hash::FxHashMap::default(),
            debug_bindings: vec![AmirDebugBinding {
                function: arandu_middle::SymbolId::new(0, 1),
                temp: TempId::from_usize(2),
                local: LocalId::from_usize(local),
            }],
            debug_blocks: Vec::new(),
        };
        assert_ne!(make_program(3).stable_hash(), make_program(4).stable_hash());
    }

    #[test]
    fn amir_program_hash_tracks_extern_signatures_independent_of_map_order() {
        use arandu_middle::amir::AmirProgram;
        use arandu_middle::types::{ArType, Primitive};
        use arandu_middle::SymbolId;

        let first_symbol = SymbolId::new(0, 1);
        let second_symbol = SymbolId::new(0, 2);
        let make_program = |reverse: bool, second_param: Primitive| {
            let mut extern_funcs = rustc_hash::FxHashMap::default();
            let first = (
                first_symbol,
                (vec![ArType::Primitive(Primitive::Int)], ArType::Void),
            );
            let second = (
                second_symbol,
                (vec![ArType::Primitive(second_param)], ArType::Void),
            );
            if reverse {
                extern_funcs.insert(second.0, second.1);
                extern_funcs.insert(first.0, first.1);
            } else {
                extern_funcs.insert(first.0, first.1);
                extern_funcs.insert(second.0, second.1);
            }
            AmirProgram {
                funcs: Vec::new(),
                literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
                extern_funcs,
                debug_bindings: Vec::new(),
                debug_blocks: Vec::new(),
            }
        };

        assert_eq!(
            make_program(false, Primitive::Bool).stable_hash(),
            make_program(true, Primitive::Bool).stable_hash()
        );
        assert_ne!(
            make_program(false, Primitive::Bool).stable_hash(),
            make_program(false, Primitive::Char).stable_hash()
        );
    }

    #[test]
    fn type_check_hash_tracks_codegen_and_safety_metadata() {
        use arandu_middle::{EffectFlags, SymbolId};
        use std::sync::Arc;

        let baseline = arandu_semantics::TypeCheckResult::empty();
        let baseline_hash = baseline.stable_hash();
        let symbol = SymbolId::new(100, 7);

        let mut with_effect = baseline.clone();
        Arc::make_mut(&mut with_effect.type_info)
            .function_effects
            .insert(symbol, EffectFlags::FILE_READ);
        assert_ne!(baseline_hash, with_effect.stable_hash());

        let mut with_repr_c = baseline.clone();
        Arc::make_mut(&mut with_repr_c.type_info)
            .struct_repr_c
            .insert(symbol);
        assert_ne!(baseline_hash, with_repr_c.stable_hash());

        let mut with_destructor = baseline.clone();
        Arc::make_mut(&mut with_destructor.type_info)
            .destructors
            .insert(symbol, SymbolId::new(100, 8));
        assert_ne!(baseline_hash, with_destructor.stable_hash());

        let mut with_builtin = baseline.clone();
        Arc::make_mut(&mut with_builtin.symbols).builtin_alloc = Some(symbol);
        assert_ne!(baseline_hash, with_builtin.stable_hash());

        let mut with_scope = baseline.clone();
        Arc::make_mut(&mut with_scope.symbols).new_scope(arandu_middle::ScopeId(0));
        assert_ne!(baseline_hash, with_scope.stable_hash());

        let mut with_resolution = baseline.clone();
        Arc::make_mut(&mut with_resolution.resolved)
            .value_ref(arandu_middle::Span::new(100, 4, 9), symbol);
        assert_ne!(baseline_hash, with_resolution.stable_hash());
        assert_eq!(
            type_signature_hash(&baseline),
            type_signature_hash(&with_resolution),
            "implementation references must not invalidate a module signature"
        );

        let another_symbol = SymbolId::new(100, 3);
        let mut first_order = baseline.clone();
        let first_effects = &mut Arc::make_mut(&mut first_order.type_info).function_effects;
        first_effects.insert(symbol, EffectFlags::FILE_READ);
        first_effects.insert(another_symbol, EffectFlags::THREAD);

        let mut reverse_order = baseline;
        let reverse_effects = &mut Arc::make_mut(&mut reverse_order.type_info).function_effects;
        reverse_effects.insert(another_symbol, EffectFlags::THREAD);
        reverse_effects.insert(symbol, EffectFlags::FILE_READ);
        assert_eq!(first_order.stable_hash(), reverse_order.stable_hash());
    }
}
