//! Pure Markdown serializer for Arandu documentation models.

use arandu_middle::docs::{DocItem, DocItemKind, DocModule};

/// Serializes a [`DocModule`] into GitHub-Flavored Markdown.
pub fn render_markdown(module: &DocModule) -> String {
    let mut out = String::new();

    out.push_str(&format!("# Módulo `{}`\n\n", module.name));
    out.push_str(&format!("**Arquivo**: `{}`\n\n", module.path));

    if let Some(overview) = &module.overview {
        out.push_str(overview);
        out.push_str("\n\n---\n\n");
    }

    let structs: Vec<&DocItem> = module
        .items
        .iter()
        .filter(|i| i.kind == DocItemKind::Struct)
        .collect();
    let enums: Vec<&DocItem> = module
        .items
        .iter()
        .filter(|i| i.kind == DocItemKind::Enum)
        .collect();
    let interfaces: Vec<&DocItem> = module
        .items
        .iter()
        .filter(|i| i.kind == DocItemKind::Interface)
        .collect();
    let type_aliases: Vec<&DocItem> = module
        .items
        .iter()
        .filter(|i| i.kind == DocItemKind::TypeAlias)
        .collect();
    let constants: Vec<&DocItem> = module
        .items
        .iter()
        .filter(|i| i.kind == DocItemKind::Constant)
        .collect();
    let functions: Vec<&DocItem> = module
        .items
        .iter()
        .filter(|i| i.kind == DocItemKind::Function)
        .collect();

    render_category(&mut out, "Structs", &structs);
    render_category(&mut out, "Enums", &enums);
    render_category(&mut out, "Interfaces", &interfaces);
    render_category(&mut out, "Tipos", &type_aliases);
    render_category(&mut out, "Constantes", &constants);
    render_category(&mut out, "Funções", &functions);

    out
}

fn render_category(out: &mut String, title: &str, items: &[&DocItem]) {
    if items.is_empty() {
        return;
    }

    out.push_str(&format!("## {title}\n\n"));

    for item in items {
        out.push_str(&format!("### `{}`\n\n", item.name));

        if !item.effect_badges.is_empty() {
            let badges = item
                .effect_badges
                .iter()
                .map(|b| format!("`[{}]`", b.label()))
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("**Efeitos**: {badges}\n\n"));
        }

        if !item.capabilities.is_empty() {
            let caps = item
                .capabilities
                .iter()
                .map(|c| format!("`[{c}]`"))
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("**Capacidades**: {caps}\n\n"));
        }

        out.push_str("```arandu\n");
        out.push_str(&item.signature);
        out.push_str("\n```\n\n");

        if let Some(summary) = &item.summary {
            out.push_str(summary);
            out.push_str("\n\n");
        }

        if let Some(description) = &item.sections.description {
            out.push_str(description);
            out.push_str("\n\n");
        }

        if let Some(complexity) = &item.sections.complexity {
            out.push_str(&format!("#### Complexidade\n\n{complexity}\n\n"));
        }

        if let Some(allocations) = &item.sections.allocations {
            out.push_str(&format!("#### Alocações\n\n{allocations}\n\n"));
        }

        if let Some(safety) = &item.sections.safety {
            out.push_str(&format!("#### Segurança\n\n{safety}\n\n"));
        }

        if !item.fields.is_empty() {
            out.push_str("#### Campos\n\n");
            for field in &item.fields {
                let doc = field
                    .doc
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                out.push_str(&format!("- `{}: {}`{}\n", field.name, field.ty, doc));
            }
            out.push('\n');
        }

        if !item.variants.is_empty() {
            out.push_str("#### Variantes\n\n");
            for variant in &item.variants {
                let payload = variant.payload.as_deref().unwrap_or("");
                let doc = variant
                    .doc
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                out.push_str(&format!("- `{}{}`{}\n", variant.name, payload, doc));
            }
            out.push('\n');
        }

        if let Some(borrow) = &item.return_borrow {
            out.push_str(&format!("*Contrato de Retorno*: {borrow}\n\n"));
        }

        if !item.sections.examples.is_empty() {
            out.push_str("#### Exemplos\n\n");
            for example in &item.sections.examples {
                out.push_str("```arandu\n");
                out.push_str(&example.code);
                out.push_str("\n```\n\n");
            }
        }

        out.push_str("---\n\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_middle::SymbolId;
    use arandu_middle::docs::{DocSections, EffectBadge};

    #[test]
    fn render_markdown_item() {
        let module = DocModule {
            name: "std.core.math".to_string(),
            path: "stdlib/core/math.aru".to_string(),
            file_id: 1,
            overview: Some("Math operations.".to_string()),
            items: vec![DocItem {
                symbol_id: SymbolId::new(1, 0),
                name: "abs".to_string(),
                kind: DocItemKind::Function,
                signature: "func abs(val: int): int".to_string(),
                effect_badges: vec![EffectBadge::Pure, EffectBadge::ZeroAlloc],
                capabilities: vec![],
                summary: Some("Returns the absolute value.".to_string()),
                sections: DocSections {
                    description: None,
                    complexity: Some("O(1)".to_string()),
                    allocations: Some("Zero".to_string()),
                    safety: None,
                    examples: Vec::new(),
                    errors: None,
                },
                fields: Vec::new(),
                variants: Vec::new(),
                return_borrow: None,
            }],
        };

        let md = render_markdown(&module);
        assert!(md.contains("# Módulo `std.core.math`"));
        assert!(md.contains("`[Pure]` `[Zero-Alloc]`"));
        assert!(md.contains("#### Complexidade\n\nO(1)"));
    }
}
