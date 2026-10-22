use super::{CompletionItem, CompletionKind};

// ── Annotations ─────────────────────────────────────────────────────

pub(super) fn is_annotation_completion(text: &str, offset: u32, prefix: &str) -> bool {
    usize::try_from(offset)
        .ok()
        .and_then(|offset| offset.checked_sub(prefix.len()))
        .and_then(|name_start| name_start.checked_sub(1))
        .is_some_and(|at| text.as_bytes().get(at) == Some(&b'@'))
}

fn annotation_target_after(
    text: &str,
    offset: u32,
) -> Option<arandu_semantics::attributes::AnnotationTarget> {
    let offset = usize::try_from(offset).ok()?.min(text.len());
    let tail = &text[offset..];
    let keyword = tail
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|part| !part.is_empty() && !matches!(*part, "public" | "async"))?;
    match keyword {
        "func" => Some(arandu_semantics::attributes::AnnotationTarget::Function),
        "extern" => Some(arandu_semantics::attributes::AnnotationTarget::ExternBlock),
        "struct" => Some(arandu_semantics::attributes::AnnotationTarget::Struct),
        "enum" => Some(arandu_semantics::attributes::AnnotationTarget::Enum),
        "interface" => Some(arandu_semantics::attributes::AnnotationTarget::Interface),
        "const" => Some(arandu_semantics::attributes::AnnotationTarget::Const),
        "type" => Some(arandu_semantics::attributes::AnnotationTarget::TypeAlias),
        _ => None,
    }
}

pub(super) fn annotation_completions(text: &str, offset: u32, prefix: &str) -> Vec<CompletionItem> {
    use arandu_semantics::attributes as attrs;

    let target = annotation_target_after(text, offset);
    let mut items: Vec<CompletionItem> = attrs::BUILTIN_ANNOTATIONS
        .iter()
        .filter(|spec| {
            spec.availability == attrs::AnnotationAvailability::Implemented
                && target.is_none_or(|target| spec.targets.contains(&target))
                && (prefix.is_empty()
                    || spec
                        .canonical_name
                        .to_ascii_lowercase()
                        .starts_with(&prefix.to_ascii_lowercase()))
        })
        .map(|spec| {
            let (insert_text, is_snippet) = match spec.arguments {
                attrs::AnnotationArguments::None => (None, false),
                attrs::AnnotationArguments::OneString => (
                    Some(format!("{}(\"${{1:library}}\")", spec.canonical_name)),
                    true,
                ),
                attrs::AnnotationArguments::OneStringOrIdent => {
                    (Some(format!("{}(\"${{1:C}}\")", spec.canonical_name)), true)
                }
                attrs::AnnotationArguments::EffectList => {
                    (Some(format!("{}(${{1:Pure}})", spec.canonical_name)), true)
                }
                // Bare form is the common case (`@Export`); the optional C
                // symbol can be typed as `@Export("name")` when needed.
                attrs::AnnotationArguments::OptionalString => (None, false),
            };
            let mut item = CompletionItem::new(spec.canonical_name, CompletionKind::Property);
            item.detail = Some(format!("@{} — {}", spec.canonical_name, spec.summary));
            item.documentation = Some(spec.summary.to_string());
            item.insert_text = insert_text;
            item.is_snippet = is_snippet;
            item
        })
        .collect();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items
}
