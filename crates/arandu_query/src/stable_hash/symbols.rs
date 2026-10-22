//! Symbol-table, diagnostic, and resolution hashes.

use arandu_middle::{Diagnostic, ResolutionResult, SymbolId, SymbolKind, SymbolTable};
use blake3::Hasher;

/// Types that can be content-addressed for [`crate::db::HashEq`].
pub trait StableHash {
    fn stable_hash(&self) -> blake3::Hash;
}

pub(super) fn finish(hasher: Hasher) -> blake3::Hash {
    hasher.finalize()
}

pub(super) fn u32_le(n: u32) -> [u8; 4] {
    n.to_le_bytes()
}

pub(super) fn u64_le(n: u64) -> [u8; 8] {
    n.to_le_bytes()
}

pub(super) fn hash_str(hasher: &mut Hasher, value: &str) {
    hasher.update(&u64_le(value.len() as u64));
    hasher.update(value.as_bytes());
}

pub(crate) fn hash_literal_entry(
    hasher: &mut Hasher,
    entry: &arandu_middle::literal_pool::AmirLiteralEntry,
) {
    use arandu_middle::literal_pool::AmirLiteralEntry;
    let (tag, text) = match entry {
        AmirLiteralEntry::Int(text) => (0, text),
        AmirLiteralEntry::Float(text) => (1, text),
        AmirLiteralEntry::Str(text) => (2, text),
        AmirLiteralEntry::Char(text) => (3, text),
        AmirLiteralEntry::FloatBits(value) => {
            hasher.update(&[4]);
            hasher.update(&value.canonical_bytes());
            return;
        }
    };
    hasher.update(&[tag]);
    hash_str(hasher, text);
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

pub(crate) fn hash_return_borrow_summary(
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

pub(super) fn hash_diag(hasher: &mut Hasher, d: &Diagnostic) {
    // `as_str` is the allocation-free single source of truth for a DiagCode's
    // public code; formatting the variant name would churn a temporary String.
    hash_str(hasher, d.code.as_str());
    hasher.update(&[d.severity as u8]);
    hasher.update(&u32_le(d.span.file_id));
    hasher.update(&u32_le(d.span.start));
    hasher.update(&u32_le(d.span.end));
    hash_str(hasher, &d.message);
    if let Some(primary_label) = &d.primary_label {
        hasher.update(&[1]);
        hash_str(hasher, primary_label);
    } else {
        hasher.update(&[0]);
    }
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

pub(super) fn hash_symbol_id(hasher: &mut Hasher, id: SymbolId) {
    hasher.update(&u32_le(id.file_id));
    hasher.update(&u32_le(id.local_id.0));
}

pub(super) fn hash_resolved_names(hasher: &mut Hasher, resolved: &arandu_middle::ResolvedNames) {
    let mut loops: Vec<_> = resolved.comptime_loops.iter().collect();
    loops.sort_by_key(|(key, _)| (key.start, key.end));
    hasher.update(&u64_le(u64::try_from(loops.len()).unwrap_or(u64::MAX)));
    for (key, (lower, upper)) in loops {
        hasher.update(&u32_le(key.start));
        hasher.update(&u32_le(key.end));
        hasher.update(&arandu_middle::ctfe::ConstValue::Integer(*lower).canonical_bytes());
        hasher.update(&arandu_middle::ctfe::ConstValue::Integer(*upper).canonical_bytes());
    }
    let mut typed: Vec<_> = resolved.typed_comptime_arguments.iter().collect();
    typed.sort_by_key(|(key, _)| (key.start, key.end));
    hasher.update(&u64_le(u64::try_from(typed.len()).unwrap_or(u64::MAX)));
    for (key, value) in typed {
        hasher.update(&key.start.to_le_bytes());
        hasher.update(&key.end.to_le_bytes());
        hasher.update(&value.canonical_bytes());
    }
    let mut arguments: Vec<_> = resolved.comptime_arguments.iter().collect();
    arguments.sort_by_key(|(key, _)| (key.start, key.end));
    hasher.update(&u64_le(u64::try_from(arguments.len()).unwrap_or(u64::MAX)));
    for (key, value) in arguments {
        hasher.update(&u32_le(key.start));
        hasher.update(&u32_le(key.end));
        hasher.update(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            hasher.update(&u64_le(*value));
        }
    }
    let mut deferred: Vec<_> = resolved
        .deferred_comptime_functions
        .iter()
        .copied()
        .collect();
    deferred.sort_unstable_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    hasher.update(&u64_le(u64::try_from(deferred.len()).unwrap_or(u64::MAX)));
    for symbol in deferred {
        hash_symbol_id(hasher, symbol);
    }
    let mut loops: Vec<_> = resolved.deferred_loop_bodies.iter().collect();
    loops.sort_unstable_by_key(|key| (key.start, key.end));
    hasher.update(&u64_le(u64::try_from(loops.len()).unwrap_or(u64::MAX)));
    for key in loops {
        hasher.update(&u32_le(key.start));
        hasher.update(&u32_le(key.end));
    }
    let mut branches: Vec<_> = resolved.comptime_branches.iter().collect();
    branches.sort_by_key(|(key, _)| (key.start, key.end));
    hasher.update(&u64_le(u64::try_from(branches.len()).unwrap_or(u64::MAX)));
    for (key, selected) in branches {
        hasher.update(&u32_le(key.start));
        hasher.update(&u32_le(key.end));
        hasher.update(&[u8::from(*selected)]);
    }
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
        LangItem::Alloc => 16,
        LangItem::Free => 17,
        LangItem::Str => 18,
        LangItem::Slice => 19,
        LangItem::Char => 20,
        LangItem::Int => 21,
        LangItem::Float => 22,
        LangItem::Ptr => 23,
        LangItem::U32 => 24,
        LangItem::U64 => 25,
        LangItem::BitIntrinsic => 26,
        LangItem::AtomicIntrinsic => 27,
        LangItem::CompilerIntrinsic => 49,
        LangItem::BlackBox => 50,
        LangItem::VectorF32x4 => 28,
        LangItem::VectorF64x2 => 29,
        LangItem::VectorI32x4 => 30,
        LangItem::VectorI16x8 => 31,
        LangItem::VectorU8x16 => 32,
        LangItem::VectorI64x2 => 33,
        LangItem::SimdIntrinsic => 34,
        LangItem::UnsafeCell => 35,
        LangItem::RwLock => 36,
        LangItem::Mutex => 37,
        LangItem::RwLockReadGuard => 38,
        LangItem::RwLockWriteGuard => 39,
        LangItem::MutexGuard => 40,
        LangItem::Condvar => 41,
        LangItem::Arc => 42,
        LangItem::ArcWeak => 43,
        LangItem::ThreadJob => 44,
        LangItem::ThreadSpawn => 45,
        LangItem::ThreadParkTimeout => 46,
        LangItem::ThreadPark => 47,
        LangItem::ThreadUnpark => 48,
        LangItem::StdThread => 51,
        LangItem::StdFs => 52,
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
    hasher.update(&[match symbol.visibility {
        arandu_parser::Visibility::Private => 0,
        arandu_parser::Visibility::Module => 1,
        arandu_parser::Visibility::Internal => 2,
        arandu_parser::Visibility::Public => 3,
    }]);
    // Declaration identity changes typing and transfer proofs even when a
    // signature projection deliberately omits incidental source spans.
    if let Some(item) = symbol.lang_item {
        hasher.update(&[1, lang_item_discriminant(item)]);
    } else {
        hasher.update(&[0]);
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

pub(super) fn hash_symbol_table(hasher: &mut Hasher, table: &SymbolTable, include_spans: bool) {
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
    let mut tooling_roots: Vec<_> = table.tooling_entrypoints.iter().copied().collect();
    tooling_roots.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    hasher.update(&u64_le(tooling_roots.len() as u64));
    for symbol in tooling_roots {
        hash_symbol_id(hasher, symbol);
    }

    // `@Export` controls linkage and the flat symbol name, so the set belongs
    // to the codegen-relevant identity of the program.
    let mut exported: Vec<_> = table.exported_functions.iter().copied().collect();
    exported.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    hasher.update(b"ExportedFunctions/v1");
    hasher.update(&u64_le(exported.len() as u64));
    for symbol in exported {
        hash_symbol_id(hasher, symbol);
    }

    // Sealed-interface status and implementation edges affect exported
    // surfaces and downstream type checking, so they belong to both the full
    // resolution hash and the span-independent module-signature hash.
    let mut sealed_interfaces: Vec<_> = table.sealed_interfaces.iter().copied().collect();
    sealed_interfaces.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    hasher.update(b"SealedInterfaces/v1");
    hasher.update(&u64_le(sealed_interfaces.len() as u64));
    for symbol in sealed_interfaces {
        hash_symbol_id(hasher, symbol);
    }
    let mut implementations: Vec<_> = table.interface_implementations.iter().copied().collect();
    implementations.sort_by_key(|(ty, interface)| {
        (
            ty.file_id,
            ty.local_id.0,
            interface.file_id,
            interface.local_id.0,
        )
    });
    hasher.update(b"InterfaceImplementations/v1");
    hasher.update(&u64_le(implementations.len() as u64));
    for (ty, interface) in implementations {
        hash_symbol_id(hasher, ty);
        hash_symbol_id(hasher, interface);
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
            table.builtins.str_type,
            table.builtins.slice_type,
            table.builtins.char_type,
            table.builtins.int_type,
            table.builtins.float_type,
            table.builtins.ptr_type,
            table.builtins.u32_type,
            table.builtins.u64_type,
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

impl StableHash for arandu_resolve::HeaderResolution {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"HeaderResolution/v1");
        h.update(self.declarations.stable_hash().as_bytes());
        let mut scopes: Vec<_> = self.body_scopes.iter().collect();
        scopes.sort_by_key(|(key, _)| (key.start, key.end));
        h.update(&u64_le(scopes.len() as u64));
        for (key, scope) in scopes {
            h.update(&u32_le(key.start));
            h.update(&u32_le(key.end));
            h.update(&u32_le(scope.0));
        }
        let mut aliases: Vec<_> = self.import_aliases.iter().collect();
        aliases.sort_by(|a, b| a.0.cmp(b.0));
        h.update(&u64_le(aliases.len() as u64));
        for (alias, module) in aliases {
            hash_str(&mut h, alias);
            hash_str(&mut h, module);
        }
        match &self.current_module {
            Some(module) => {
                h.update(&[1]);
                hash_str(&mut h, module);
            }
            None => {
                h.update(&[0]);
            }
        }
        let mut imports: Vec<_> = self.imported_symbols.iter().collect();
        imports.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        h.update(&u64_le(imports.len() as u64));
        for (symbol, (name, span)) in imports {
            hash_symbol_id(&mut h, *symbol);
            hash_str(&mut h, name);
            h.update(&u32_le(span.file_id));
            h.update(&u32_le(span.start));
            h.update(&u32_le(span.end));
        }
        let mut used: Vec<_> = self.used_symbols.iter().copied().collect();
        used.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
        h.update(&u64_le(used.len() as u64));
        for symbol in used {
            hash_symbol_id(&mut h, symbol);
        }
        finish(h)
    }
}
