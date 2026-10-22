use super::{CompletionItem, CompletionKind, RANK_MEMBER, accepted, finish_ranked, value_kind};
use crate::presentation::{display_type, symbol_at, symbol_presentation};
use arandu_middle::types::{ArType, Primitive};
use arandu_middle::{SymbolId, SymbolKind};
use arandu_query::{AnalysisSnapshot, SourceFile};
use arandu_semantics::TypeCheckResult;

// ── Member completion ───────────────────────────────────────────────

/// `(receiver, receiver_start_offset)` when the cursor follows `receiver.`.
fn receiver_before_prefix(text: &str, offset: u32, prefix_len: usize) -> Option<(String, u32)> {
    let end = (offset as usize).min(text.len()).checked_sub(prefix_len)?;
    let before = text.get(..end)?.trim_end();
    let without_dot = before.strip_suffix('.')?;
    let bytes = without_dot.as_bytes();
    let mut start = without_dot.len();
    while start > 0 {
        let byte = bytes[start - 1];
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    let receiver = &without_dot[start..];
    if receiver.is_empty() {
        return None;
    }
    Some((receiver.to_string(), u32::try_from(start).ok()?))
}

pub(super) fn member_completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &TypeCheckResult,
    text: &str,
    offset: u32,
    prefix: &str,
    prefix_l: &str,
) -> Option<Vec<CompletionItem>> {
    let (receiver, receiver_start) = receiver_before_prefix(text, offset, prefix.len())?;
    let global = tc.symbols.global_scope();

    if tc.symbols.lookup_module(global, &receiver).is_some() {
        return module_member_completions(snap, tc, &receiver, prefix, prefix_l);
    }

    let symbol = symbol_at(tc, receiver_start)
        .or_else(|| {
            // Name resolution does not always publish a reference entry for a
            // plain receiver use, so fall back to the most recent visible
            // declaration with the same name (a shadowing local wins).
            tc.symbols
                .iter()
                .filter(|candidate| candidate.name.as_str() == receiver)
                .filter(|candidate| candidate.span.start <= receiver_start)
                .filter(|candidate| {
                    matches!(
                        candidate.kind,
                        SymbolKind::Local | SymbolKind::Param | SymbolKind::Const
                    )
                })
                .max_by_key(|candidate| candidate.span.start)
                .map(|candidate| candidate.id)
        })
        .or_else(|| tc.symbols.lookup_value(global, &receiver))
        .or_else(|| tc.symbols.lookup_type(global, &receiver))?;
    let parent = parent_type_symbol(tc, symbol)?;
    let owner_kind = tc.symbols.try_get(parent).map(|symbol| symbol.kind);

    let mut items: Vec<(u8, CompletionItem)> = Vec::new();

    if owner_kind == Some(SymbolKind::Struct)
        && let Some(fields) = tc.type_info.struct_fields.get(&parent)
    {
        let private_access = has_owner_method_at(snap, source, receiver_start, tc, parent);
        for field in fields.iter() {
            if !private_access
                && field
                    .symbol
                    .is_some_and(|symbol| tc.type_info.private_fields.contains(&symbol))
            {
                continue;
            }
            if !accepted(prefix, prefix_l, field.name.as_str()) {
                continue;
            }
            let ty = tc.type_info.type_interner.resolve(field.ty);
            let mut item = CompletionItem::new(field.name.to_string(), CompletionKind::Field);
            item.detail = Some(format!("{}: {}", field.name, display_type(tc, &ty)));
            items.push((RANK_MEMBER, item));
        }
    }

    if owner_kind == Some(SymbolKind::Enum) {
        for (variant, (owner, _shape)) in &tc.type_info.enum_variants {
            if *owner != parent {
                continue;
            }
            let Some(name) = tc.type_info.enum_variant_name(&tc.symbols, *variant) else {
                continue;
            };
            if !accepted(prefix, prefix_l, &name) {
                continue;
            }
            let mut item = CompletionItem::new(name, CompletionKind::EnumMember);
            item.detail = Some("variant".to_string());
            let tag = tc
                .type_info
                .enum_variant_tags
                .get(variant)
                .copied()
                .unwrap_or(0);
            items.push((u8::try_from(tag).unwrap_or(RANK_MEMBER), item));
        }
    }

    if owner_kind == Some(SymbolKind::Interface)
        && let Some(info) = tc.type_info.interfaces.get(&parent)
    {
        for method in &info.methods {
            if !accepted(prefix, prefix_l, method.name.as_str()) {
                continue;
            }
            let signature = tc.type_info.type_interner.resolve(method.sig_id);
            let mut item = CompletionItem::new(method.name.to_string(), CompletionKind::Method);
            item.detail = Some(display_type(tc, &signature));
            items.push((RANK_MEMBER, item));
        }
    }

    for ((owner, name), &member) in &tc.symbols.associated_members {
        if *owner != parent || !accepted(prefix, prefix_l, name.as_str()) {
            continue;
        }
        let mut item = CompletionItem::new(name.to_string(), CompletionKind::Method);
        if let Some(symbol) = tc.symbols.try_get(member) {
            let presentation = symbol_presentation(snap, source, tc, symbol);
            item.detail = Some(presentation.signature);
            item.documentation = presentation.documentation;
        }
        items.push((RANK_MEMBER + 1, item));
    }

    if items.is_empty() {
        return None;
    }
    Some(finish_ranked(items))
}

fn has_owner_method_at(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    offset: u32,
    tc: &TypeCheckResult,
    owner: arandu_middle::SymbolId,
) -> bool {
    let tree = arandu_query::passes::syntax_tree(&snap.db, source);
    let output = arandu_parser::syntax::lower_syntax_to_program_recovering(
        tree.value.as_ref(),
        *source.file_id(&snap.db),
    );
    output.program.decls.iter().any(|decl_id| {
        let arandu_parser::TopLevelDecl::Func(func) = output.program.pool.decl(*decl_id) else {
            return false;
        };
        if !(func.span.start <= offset && offset <= func.span.end) {
            return false;
        }
        let arandu_parser::FuncName::Method { receiver, .. } = &func.name else {
            return false;
        };
        receiver
            .path
            .last()
            .and_then(|name| tc.symbols.lookup_type(tc.symbols.global_scope(), name))
            == Some(owner)
    })
}

fn module_member_completions(
    snap: &AnalysisSnapshot,
    tc: &TypeCheckResult,
    alias: &str,
    prefix: &str,
    prefix_l: &str,
) -> Option<Vec<CompletionItem>> {
    let mut items: Vec<(u8, CompletionItem)> = Vec::new();
    for ((module, name), &member) in &tc.symbols.module_members {
        if module.as_str() != alias {
            continue;
        }
        let name_s = name.as_str();
        // Associated method keys are `Type.method`; only plain members apply here.
        if name_s.contains('.') || !accepted(prefix, prefix_l, name_s) {
            continue;
        }
        let symbol = tc.symbols.try_get(member);
        let mut item = CompletionItem::new(
            name_s,
            symbol.map_or(CompletionKind::Text, |symbol| value_kind(symbol.kind)),
        );
        let presentation = symbol.and_then(|symbol| {
            let member_source = snap.db.source_file_by_id(symbol.id.file_id)?;
            Some(symbol_presentation(snap, member_source, tc, symbol))
        });
        item.detail = Some(presentation.as_ref().map_or_else(
            || format!("from `{alias}`"),
            |presentation| format!("{} — from `{alias}`", presentation.signature),
        ));
        item.documentation = presentation.and_then(|presentation| presentation.documentation);
        items.push((RANK_MEMBER, item));
    }
    if items.is_empty() {
        return None;
    }
    Some(finish_ranked(items))
}

/// Resolve the nominal type symbol behind a receiver.
fn parent_type_symbol(tc: &TypeCheckResult, symbol: SymbolId) -> Option<SymbolId> {
    if let Some(definition) = tc.symbols.try_get(symbol)
        && matches!(
            definition.kind,
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface
        )
    {
        return Some(symbol);
    }
    let ty = tc.type_info.decl_type(symbol)?;
    named_symbol_of(tc, &ty)
}

fn named_symbol_of(tc: &TypeCheckResult, ty: &ArType) -> Option<SymbolId> {
    match ty {
        ArType::Named(symbol, _) => Some(*symbol),
        ArType::Ptr(_) => tc.symbols.builtins.ptr_type,
        ArType::Nullable(inner) => match tc.type_info.type_interner.resolve(*inner) {
            ArType::Ptr(_) => tc.symbols.builtins.ptr_type,
            other => named_symbol_of(tc, &other),
        },
        ArType::Ref(inner) | ArType::RefMut(inner) => {
            named_symbol_of(tc, &tc.type_info.type_interner.resolve(*inner))
        }
        ArType::Result(_, _) => tc.symbols.builtins.result,
        ArType::Option(_) => tc.symbols.builtins.option,
        ArType::Primitive(Primitive::Str) => tc.symbols.builtins.str_type,
        ArType::Primitive(Primitive::Char) => tc.symbols.builtins.char_type,
        ArType::Primitive(Primitive::U32) => tc.symbols.builtins.u32_type,
        ArType::Primitive(Primitive::U64) => tc.symbols.builtins.u64_type,
        ArType::Primitive(Primitive::Int) | ArType::IntLiteral => tc.symbols.builtins.int_type,
        ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => {
            tc.symbols.builtins.float_type
        }
        ArType::Slice(_) => tc.symbols.builtins.slice_type,
        _ => None,
    }
}
