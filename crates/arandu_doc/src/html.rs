//! Pure static standalone HTML generator for Arandu documentation.
//!
//! Generates zero-dependency, self-contained HTML pages that work offline
//! via file://, require zero JavaScript to read, support dark/light modes,
//! and feature responsive navigation and contract callouts.

use arandu_middle::docs::{DocItem, DocItemKind, DocModule};

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Serializes a [`DocModule`] into a self-contained static HTML document.
pub fn render_html(module: &DocModule) -> String {
    let mut out = String::new();

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

    out.push_str("<!DOCTYPE html>\n<html lang=\"pt-BR\">\n<head>\n");
    out.push_str("<meta charset=\"UTF-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n");
    out.push_str(&format!(
        "<title>{} — Documentação Arandu</title>\n",
        escape_html(&module.name)
    ));
    out.push_str("<style>\n");
    out.push_str(CSS_STYLES);
    out.push_str("</style>\n");

    let json_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "TechArticle",
        "headline": module.name,
        "description": module.overview.as_deref().unwrap_or("Documentação técnica do módulo Arandu"),
        "inLanguage": "pt-BR",
        "publisher": {
            "@type": "Organization",
            "name": "Arandu Programming Language",
            "url": "https://arandu-lang.dev"
        }
    });
    if let Ok(json_str) = serde_json::to_string(&json_ld) {
        // JSON quoting alone does not escape HTML's script-end token. Doc
        // comments must stay data even when they contain </script> markup.
        let json_str = json_str
            .replace('&', "\\u0026")
            .replace('<', "\\u003c")
            .replace('>', "\\u003e");
        out.push_str(&format!(
            "<script type=\"application/ld+json\">{}</script>\n",
            json_str
        ));
    }

    out.push_str("</head>\n<body>\n");

    out.push_str("<div class=\"doc-layout\">\n");

    // ── Sidebar ──────────────────────────────────────────────────────────
    out.push_str("<aside class=\"doc-sidebar\">\n");
    out.push_str("<div class=\"sidebar-header\">\n");
    out.push_str("<a href=\"#top\" class=\"logo-link\">Arandu Doc</a>\n");
    out.push_str(&format!(
        "<div class=\"module-badge\">{}</div>\n",
        escape_html(&module.name)
    ));
    out.push_str("</div>\n");

    out.push_str("<div class=\"search-wrapper\">\n");
    out.push_str("<input type=\"search\" id=\"symbol-search\" placeholder=\"Filtrar símbolos...\" oninput=\"filterSymbols(this.value)\">\n");
    out.push_str("</div>\n");

    out.push_str("<nav class=\"symbol-tree\" id=\"symbol-tree\">\n");
    render_sidebar_category(&mut out, "Structs", "str", &structs);
    render_sidebar_category(&mut out, "Enums", "enum", &enums);
    render_sidebar_category(&mut out, "Interfaces", "iface", &interfaces);
    render_sidebar_category(&mut out, "Tipos", "type", &type_aliases);
    render_sidebar_category(&mut out, "Constantes", "const", &constants);
    render_sidebar_category(&mut out, "Funções", "fn", &functions);
    out.push_str("</nav>\n");
    out.push_str("</aside>\n");

    // ── Main Content ─────────────────────────────────────────────────────
    out.push_str("<main class=\"doc-main\" id=\"top\">\n");

    out.push_str("<header class=\"module-header\">\n");
    out.push_str(&format!(
        "<div class=\"module-kind\">MÓDULO</div>\n<h1>{}</h1>\n",
        escape_html(&module.name)
    ));
    out.push_str(&format!(
        "<div class=\"module-path\">Origem: <code>{}</code></div>\n",
        escape_html(&module.path)
    ));

    if let Some(overview) = &module.overview {
        out.push_str(&format!(
            "<div class=\"module-overview\">{}</div>\n",
            escape_html(overview)
        ));
    }
    out.push_str("</header>\n");

    render_main_category(&mut out, "Structs", &structs);
    render_main_category(&mut out, "Enums", &enums);
    render_main_category(&mut out, "Interfaces", &interfaces);
    render_main_category(&mut out, "Tipos", &type_aliases);
    render_main_category(&mut out, "Constantes", &constants);
    render_main_category(&mut out, "Funções", &functions);

    out.push_str("<footer class=\"doc-footer\">\n");
    out.push_str("<p>Documentação gerada nativamente pelo <strong>Arandudoc</strong>. Garantias formais verificadas pelo compilador Arandu.</p>\n");
    out.push_str("</footer>\n");

    out.push_str("</main>\n");
    out.push_str("</div>\n");

    // Progressive enhancement search script (runs only if JS enabled)
    out.push_str(
        r#"<script>
function filterSymbols(query) {
  query = query.toLowerCase().trim();
  const links = document.querySelectorAll('.symbol-item-link');
  links.forEach(link => {
    const text = link.getAttribute('data-name') || '';
    if (!query || text.includes(query)) {
      link.style.display = 'flex';
    } else {
      link.style.display = 'none';
    }
  });
}
</script>
</body>
</html>
"#,
    );

    out
}

fn render_sidebar_category(out: &mut String, title: &str, tag: &str, items: &[&DocItem]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!(
        "<details open class=\"nav-group\">\n<summary class=\"nav-group-title\">{} <span class=\"count\">{}</span></summary>\n<ul class=\"nav-items\">\n",
        title,
        items.len()
    ));

    for item in items {
        let anchor = format!("sym-{}", item.name);
        out.push_str(&format!(
            "<li><a href=\"#{}\" class=\"symbol-item-link\" data-name=\"{}\"><span class=\"kind-pill pill-{}\">{}</span><span class=\"sym-name\">{}</span></a></li>\n",
            anchor,
            escape_html(&item.name.to_lowercase()),
            tag,
            tag,
            escape_html(&item.name)
        ));
    }

    out.push_str("</ul>\n</details>\n");
}

fn render_main_category(out: &mut String, title: &str, items: &[&DocItem]) {
    if items.is_empty() {
        return;
    }

    out.push_str(&format!(
        "<section class=\"category-section\">\n<h2 class=\"category-heading\">{}</h2>\n",
        title
    ));

    for item in items {
        let anchor = format!("sym-{}", item.name);
        out.push_str(&format!(
            "<article class=\"item-card\" id=\"{}\">\n",
            anchor
        ));

        out.push_str("<div class=\"item-card-header\">\n");
        out.push_str(&format!(
            "<div class=\"item-title-group\"><span class=\"item-kind-label\">{}</span><h3 class=\"item-title\">{}</h3></div>\n",
            item.kind.as_str(),
            escape_html(&item.name)
        ));

        if !item.effect_badges.is_empty() {
            out.push_str("<div class=\"effect-badges-strip\">\n");
            for badge in &item.effect_badges {
                out.push_str(&format!(
                    "<span class=\"effect-badge {}\" title=\"{}\">[{}]</span>\n",
                    badge.css_class(),
                    badge.description(),
                    badge.label()
                ));
            }
            out.push_str("</div>\n");
        }
        out.push_str("</div>\n");

        out.push_str("<div class=\"signature-block\">\n<pre><code>");
        out.push_str(&escape_html(&item.signature));
        out.push_str("</code></pre>\n</div>\n");

        if let Some(summary) = &item.summary {
            out.push_str(&format!(
                "<p class=\"item-summary\">{}</p>\n",
                escape_html(summary)
            ));
        }

        if let Some(desc) = &item.sections.description {
            out.push_str(&format!(
                "<div class=\"item-description\"><p>{}</p></div>\n",
                escape_html(desc)
            ));
        }

        // Contracts
        if item.sections.complexity.is_some()
            || item.sections.allocations.is_some()
            || item.sections.safety.is_some()
        {
            out.push_str("<div class=\"contracts-grid\">\n");
            if let Some(comp) = &item.sections.complexity {
                out.push_str(&format!(
                    "<div class=\"contract-box\"><div class=\"contract-label\">COMPLEXIDADE</div><div class=\"contract-value\">{}</div></div>\n",
                    escape_html(comp)
                ));
            }
            if let Some(alloc) = &item.sections.allocations {
                out.push_str(&format!(
                    "<div class=\"contract-box\"><div class=\"contract-label\">ALOCAÇÃO DE MEMÓRIA</div><div class=\"contract-value\">{}</div></div>\n",
                    escape_html(alloc)
                ));
            }
            if let Some(safety) = &item.sections.safety {
                out.push_str(&format!(
                    "<div class=\"contract-box contract-safety\"><div class=\"contract-label\">SEGURANÇA &amp; CONTRATOS</div><div class=\"contract-value\">{}</div></div>\n",
                    escape_html(safety)
                ));
            }
            out.push_str("</div>\n");
        }

        if !item.fields.is_empty() {
            out.push_str(
                "<div class=\"members-block\"><h4>Campos</h4><ul class=\"members-list\">\n",
            );
            for field in &item.fields {
                let doc_html = field
                    .doc
                    .as_deref()
                    .map(|d| format!("<span class=\"member-doc\"> — {}</span>", escape_html(d)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "<li><code>{}: {}</code>{}</li>\n",
                    escape_html(&field.name),
                    escape_html(&field.ty),
                    doc_html
                ));
            }
            out.push_str("</ul></div>\n");
        }

        if !item.variants.is_empty() {
            out.push_str(
                "<div class=\"members-block\"><h4>Variantes</h4><ul class=\"members-list\">\n",
            );
            for variant in &item.variants {
                let payload = variant.payload.as_deref().unwrap_or("");
                let doc_html = variant
                    .doc
                    .as_deref()
                    .map(|d| format!("<span class=\"member-doc\"> — {}</span>", escape_html(d)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "<li><code>{}{}</code>{}</li>\n",
                    escape_html(&variant.name),
                    escape_html(payload),
                    doc_html
                ));
            }
            out.push_str("</ul></div>\n");
        }

        if let Some(borrow) = &item.return_borrow {
            out.push_str(&format!(
                "<div class=\"return-borrow-box\"><strong>Contrato de Empréstimo:</strong> {}</div>\n",
                escape_html(borrow)
            ));
        }

        if !item.sections.examples.is_empty() {
            out.push_str("<div class=\"examples-block\"><h4>Exemplos</h4>\n");
            for ex in &item.sections.examples {
                out.push_str("<pre class=\"example-code\"><code>");
                out.push_str(&escape_html(&ex.code));
                out.push_str("</code></pre>\n");
            }
            out.push_str("</div>\n");
        }

        out.push_str("</article>\n");
    }

    out.push_str("</section>\n");
}

const CSS_STYLES: &str = r#"
:root {
  --bg-page: #0f141c;
  --bg-sidebar: #131923;
  --bg-card: #18202e;
  --bg-code: #0b0f17;
  --border-color: #242f42;
  --text-main: #e2e8f0;
  --text-muted: #94a3b8;
  --text-heading: #f8fafc;
  --accent: #38bdf8;
  --accent-rgb: 56, 189, 248;
  --badge-pure: #10b981;
  --badge-zero: #3b82f6;
  --badge-nothrow: #8b5cf6;
  --badge-thread: #f59e0b;
  --badge-async: #ec4899;
}

* { box-sizing: border-box; margin: 0; padding: 0; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  background-color: var(--bg-page);
  color: var(--text-main);
  line-height: 1.6;
}

.doc-layout { display: flex; min-height: 100vh; }

.doc-sidebar {
  width: 290px;
  background-color: var(--bg-sidebar);
  border-right: 1px solid var(--border-color);
  padding: 24px 16px;
  position: sticky;
  top: 0;
  height: 100vh;
  overflow-y: auto;
  flex-shrink: 0;
}

.sidebar-header { margin-bottom: 20px; }
.logo-link {
  font-size: 1.25rem;
  font-weight: 700;
  color: var(--text-heading);
  text-decoration: none;
  letter-spacing: -0.02em;
}
.module-badge {
  font-family: ui-monospace, monospace;
  font-size: 0.8rem;
  color: var(--accent);
  margin-top: 4px;
}

.search-wrapper input {
  width: 100%;
  padding: 8px 12px;
  background-color: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: 6px;
  color: var(--text-main);
  font-size: 0.85rem;
  margin-bottom: 20px;
}
.search-wrapper input:focus {
  outline: none;
  border-color: var(--accent);
}

.nav-group { margin-bottom: 16px; }
.nav-group-title {
  font-size: 0.75rem;
  font-weight: 700;
  text-transform: uppercase;
  color: var(--text-muted);
  cursor: pointer;
  padding: 4px 0;
}
.nav-group-title .count {
  font-size: 0.7rem;
  background: var(--border-color);
  padding: 1px 6px;
  border-radius: 10px;
  margin-left: 6px;
}
.nav-items { list-style: none; margin-top: 6px; }
.symbol-item-link {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 8px;
  color: var(--text-main);
  text-decoration: none;
  font-size: 0.85rem;
  border-radius: 4px;
}
.symbol-item-link:hover { background-color: rgba(var(--accent-rgb), 0.1); color: var(--accent); }

.kind-pill {
  font-size: 0.65rem;
  font-weight: 700;
  text-transform: uppercase;
  padding: 2px 4px;
  border-radius: 3px;
  line-height: 1;
}
.pill-fn { background: rgba(56, 189, 248, 0.2); color: #38bdf8; }
.pill-str { background: rgba(168, 85, 247, 0.2); color: #c084fc; }
.pill-enum { background: rgba(234, 179, 8, 0.2); color: #facc15; }
.pill-iface { background: rgba(34, 197, 94, 0.2); color: #4ade80; }
.pill-type { background: rgba(148, 163, 184, 0.2); color: #94a3b8; }
.pill-const { background: rgba(244, 63, 94, 0.2); color: #fb7185; }

.doc-main { flex: 1; padding: 40px 60px; max-width: 1000px; }

.module-header { margin-bottom: 40px; border-bottom: 1px solid var(--border-color); padding-bottom: 24px; }
.module-kind { font-size: 0.75rem; font-weight: 700; color: var(--accent); letter-spacing: 0.05em; }
.module-header h1 { font-size: 2.2rem; color: var(--text-heading); margin-top: 4px; }
.module-path { font-size: 0.85rem; color: var(--text-muted); margin-top: 6px; }
.module-path code { font-family: ui-monospace, monospace; }
.module-overview { margin-top: 16px; font-size: 1.05rem; color: var(--text-main); }

.category-heading {
  font-size: 1.4rem;
  color: var(--text-heading);
  margin-top: 40px;
  margin-bottom: 20px;
  border-bottom: 1px solid var(--border-color);
  padding-bottom: 8px;
}

.item-card {
  background-color: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 24px;
  margin-bottom: 24px;
}
.item-card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 12px;
  margin-bottom: 14px;
}
.item-title-group { display: flex; align-items: baseline; gap: 10px; }
.item-kind-label {
  font-size: 0.7rem;
  font-weight: 700;
  text-transform: uppercase;
  color: var(--accent);
}
.item-title { font-size: 1.3rem; color: var(--text-heading); }

.effect-badges-strip { display: flex; gap: 6px; }
.effect-badge {
  font-size: 0.72rem;
  font-weight: 600;
  font-family: ui-monospace, monospace;
  padding: 3px 8px;
  border-radius: 12px;
}
.badge-pure { background: rgba(16, 185, 129, 0.15); color: #34d399; border: 1px solid rgba(16, 185, 129, 0.3); }
.badge-zero-alloc { background: rgba(59, 130, 246, 0.15); color: #60a5fa; border: 1px solid rgba(59, 130, 246, 0.3); }
.badge-no-throw { background: rgba(139, 92, 246, 0.15); color: #a78bfa; border: 1px solid rgba(139, 92, 246, 0.3); }
.badge-thread-safe { background: rgba(245, 158, 11, 0.15); color: #fbbf24; border: 1px solid rgba(245, 158, 11, 0.3); }
.badge-async { background: rgba(236, 72, 153, 0.15); color: #f472b6; border: 1px solid rgba(236, 72, 153, 0.3); }

.signature-block {
  background-color: var(--bg-code);
  border: 1px solid var(--border-color);
  border-radius: 6px;
  padding: 12px 16px;
  margin-bottom: 16px;
}
.signature-block code {
  font-family: ui-monospace, monospace;
  font-size: 0.95rem;
  color: #7dd3fc;
}

.item-summary { font-size: 1rem; color: var(--text-main); margin-bottom: 12px; font-weight: 500; }
.item-description { font-size: 0.95rem; color: var(--text-muted); margin-bottom: 16px; }

.contracts-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: 12px;
  margin-bottom: 16px;
}
.contract-box {
  background-color: rgba(255, 255, 255, 0.02);
  border: 1px solid var(--border-color);
  border-radius: 6px;
  padding: 10px 14px;
}
.contract-label {
  font-size: 0.65rem;
  font-weight: 700;
  text-transform: uppercase;
  color: var(--text-muted);
  letter-spacing: 0.05em;
  margin-bottom: 4px;
}
.contract-value {
  font-size: 0.85rem;
  color: var(--text-main);
  font-weight: 500;
}
.contract-safety {
  border-color: rgba(245, 158, 11, 0.3);
  background-color: rgba(245, 158, 11, 0.03);
}

.members-block, .examples-block { margin-top: 16px; }
.members-block h4, .examples-block h4 {
  font-size: 0.85rem;
  text-transform: uppercase;
  color: var(--text-muted);
  margin-bottom: 8px;
}
.members-list { list-style: disc inside; font-size: 0.9rem; }
.members-list li { padding: 3px 0; }
.members-list code { font-family: ui-monospace, monospace; color: var(--accent); }
.member-doc { color: var(--text-muted); font-style: italic; }

.return-borrow-box {
  margin-top: 12px;
  font-size: 0.85rem;
  background-color: rgba(56, 189, 248, 0.05);
  border-left: 3px solid var(--accent);
  padding: 8px 12px;
  border-radius: 0 4px 4px 0;
}

.example-code {
  background-color: var(--bg-code);
  border: 1px solid var(--border-color);
  border-radius: 6px;
  padding: 12px;
  font-family: ui-monospace, monospace;
  font-size: 0.85rem;
  color: #a5f3fc;
  overflow-x: auto;
}

.doc-footer {
  margin-top: 60px;
  padding-top: 20px;
  border-top: 1px solid var(--border-color);
  font-size: 0.85rem;
  color: var(--text-muted);
  text-align: center;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_middle::SymbolId;
    use arandu_middle::docs::{DocSections, EffectBadge};

    #[test]
    fn render_html_page() {
        let module = DocModule {
            name: "std.core.mem".to_string(),
            path: "stdlib/core/mem.aru".to_string(),
            file_id: 1,
            overview: Some("Low-level memory management.".to_string()),
            items: vec![DocItem {
                symbol_id: SymbolId::new(1, 0),
                name: "copy".to_string(),
                kind: DocItemKind::Function,
                signature: "func copy(dst: ptr[u8], src: ptr[u8], count: uint)".to_string(),
                effect_badges: vec![EffectBadge::Pure, EffectBadge::ZeroAlloc],
                summary: Some("Copies memory buffer.".to_string()),
                sections: DocSections {
                    description: None,
                    complexity: Some("O(n)".to_string()),
                    allocations: Some("Zero-Alloc".to_string()),
                    safety: Some("Buffers must not overlap.".to_string()),
                    examples: Vec::new(),
                    errors: None,
                },
                fields: Vec::new(),
                variants: Vec::new(),
                return_borrow: None,
            }],
        };

        let html = render_html(&module);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("std.core.mem — Documentação Arandu"));
        assert!(html.contains("[Pure]"));
        assert!(html.contains("[Zero-Alloc]"));
        assert!(html.contains("O(n)"));
        assert!(html.contains("Buffers must not overlap."));
        assert!(html.contains("application/ld+json"));
        assert!(html.contains("TechArticle"));
    }
    #[test]
    #[allow(clippy::unwrap_used)]
    fn structured_metadata_cannot_close_its_script_element() {
        let text = "</script><script>alert('doc')</script>&";
        let module = DocModule {
            name: text.into(),
            path: "untrusted.aru".into(),
            file_id: 1,
            overview: Some(text.into()),
            items: vec![],
        };
        let html = render_html(&module);
        let prefix = "<script type=\"application/ld+json\">";
        let data = html
            .split_once(prefix)
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        assert!(!data.contains('<'));
        let metadata: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(metadata["headline"], text);
        assert_eq!(metadata["description"], text);
    }
}
