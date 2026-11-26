//! Documentation models, structured section parser, and effect badge derivation.

use crate::SymbolId;
use crate::effects::EffectFlags;
use serde::{Deserialize, Serialize};

/// High-level documentation model for an Arandu source module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocModule {
    pub name: String,
    pub path: String,
    pub file_id: u32,
    pub overview: Option<String>,
    pub items: Vec<DocItem>,
}

/// Category of a documented language item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocItemKind {
    Function,
    Struct,
    Enum,
    Interface,
    TypeAlias,
    Constant,
}

impl DocItemKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Interface => "interface",
            Self::TypeAlias => "type_alias",
            Self::Constant => "constant",
        }
    }
}

/// Compiler-proven effect badge derived from the A2 effect system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectBadge {
    Pure,
    ZeroAlloc,
    NoThrow,
    ThreadSafe,
    AsyncSuspend,
}

impl EffectBadge {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pure => "Pure",
            Self::ZeroAlloc => "Zero-Alloc",
            Self::NoThrow => "No-Throw",
            Self::ThreadSafe => "Thread-Safe",
            Self::AsyncSuspend => "Async / Suspend",
        }
    }

    #[must_use]
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Pure => "badge-pure",
            Self::ZeroAlloc => "badge-zero-alloc",
            Self::NoThrow => "badge-no-throw",
            Self::ThreadSafe => "badge-thread-safe",
            Self::AsyncSuspend => "badge-async",
        }
    }

    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Pure => {
                "Compilador provou ausência de efeitos colaterais e mutação de estado externo."
            }
            Self::ZeroAlloc => {
                "Garantia formal de tempo de compilação: a chamada nunca aloca memória no heap."
            }
            Self::NoThrow => {
                "Garantia estrita de que a execução nunca dispara erros irrecuperáveis ou pânico."
            }
            Self::ThreadSafe => "Livre de condições de corrida e seguro para execução concorrente.",
            Self::AsyncSuspend => "Ponto de suspensão cooperativo / corrotina.",
        }
    }
}

/// Structured documentation sections extracted from Markdown doc-comments.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSections {
    pub description: Option<String>,
    pub complexity: Option<String>,
    pub allocations: Option<String>,
    pub safety: Option<String>,
    pub examples: Vec<DoctestSnippet>,
    pub errors: Option<String>,
}

/// Runnable code snippet extracted from `# Examples` for doctest execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctestSnippet {
    pub code: String,
    pub line_offset: u32,
    pub should_panic: bool,
    pub no_run: bool,
}

/// Struct field documentation and type signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocField {
    pub name: String,
    pub ty: String,
    pub doc: Option<String>,
}

/// Enum variant documentation and optional payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocVariant {
    pub name: String,
    pub payload: Option<String>,
    pub doc: Option<String>,
}

/// Fully documented item (function, struct, enum, interface, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocItem {
    pub symbol_id: SymbolId,
    pub name: String,
    pub kind: DocItemKind,
    pub signature: String,
    pub effect_badges: Vec<EffectBadge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub summary: Option<String>,
    pub sections: DocSections,
    pub fields: Vec<DocField>,
    pub variants: Vec<DocVariant>,
    pub return_borrow: Option<String>,
}

/// Derive compiler-proven effect badges from [`EffectFlags`].
#[must_use]
pub fn derive_effect_badges(effects: EffectFlags) -> Vec<EffectBadge> {
    let mut badges = Vec::new();

    let side_effects_mask = EffectFlags::NET.0
        | EffectFlags::FILE_READ.0
        | EffectFlags::FILE_WRITE.0
        | EffectFlags::ENVIRONMENT.0
        | EffectFlags::PROCESS.0
        | EffectFlags::FOREIGN.0
        | EffectFlags::HEAP.0
        | EffectFlags::BLOCKING.0
        | EffectFlags::THREAD.0;

    let is_pure = effects.contains(EffectFlags::PURE) || (effects.0 & side_effects_mask) == 0;
    if is_pure {
        badges.push(EffectBadge::Pure);
    }

    let is_zero_alloc = effects.contains(EffectFlags::NO_ALLOC)
        || (!effects.contains(EffectFlags::HEAP) && !effects.contains(EffectFlags::FOREIGN));
    if is_zero_alloc {
        badges.push(EffectBadge::ZeroAlloc);
    }

    if effects.contains(EffectFlags::NO_THROW) {
        badges.push(EffectBadge::NoThrow);
    }

    let is_thread_safe =
        !effects.contains(EffectFlags::THREAD) && !effects.contains(EffectFlags::FOREIGN);
    if is_thread_safe {
        badges.push(EffectBadge::ThreadSafe);
    }

    if effects.contains(EffectFlags::SUSPEND) {
        badges.push(EffectBadge::AsyncSuspend);
    }

    badges
}

/// Clean a single doc-comment line (strip leading `/// ` or `///` or `* `).
#[must_use]
pub fn clean_doc_line(line: &str) -> &str {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("/// ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("///") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("//! ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("//!") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("* ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix('*') {
        rest
    } else {
        trimmed
    }
}

/// Clean stored doc-comment lines, including `/** … */` block comments.
///
/// Block comments arrive as one token with embedded newlines and decorative
/// `* ` prefixes. Fences are dropped and each embedded line becomes one output
/// line; blank lines from `///` are preserved so Markdown paragraphs survive.
#[must_use]
pub fn clean_doc_lines(raw_lines: &[String]) -> Vec<String> {
    let mut cleaned = Vec::new();
    for raw in raw_lines {
        let mut in_block = false;
        for subline in raw.lines() {
            let trimmed = subline.trim();
            if in_block && trimmed == "*/" {
                in_block = false;
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("/**") {
                in_block = !trimmed.ends_with("*/");
                let content = rest.strip_suffix("*/").unwrap_or(rest).trim();
                if !content.is_empty() {
                    cleaned.push(clean_doc_line(content).to_owned());
                }
                continue;
            }
            let content = if in_block {
                let content = trimmed.strip_suffix("*/").unwrap_or(trimmed);
                in_block = !trimmed.ends_with("*/");
                content.trim()
            } else {
                trimmed
            };
            cleaned.push(clean_doc_line(content).to_owned());
        }
    }
    cleaned
}

enum ParseSection {
    Preamble,
    Description,
    Complexity,
    Allocations,
    Safety,
    Examples,
    Errors,
}

/// Parse raw doc-comment lines into a summary and structured [`DocSections`].
#[must_use]
pub fn parse_doc_comment_lines(raw_lines: &[String]) -> (Option<String>, DocSections) {
    let cleaned_lines = clean_doc_lines(raw_lines);

    let mut summary = None;
    let mut description_lines = Vec::new();
    let mut complexity_lines = Vec::new();
    let mut allocations_lines = Vec::new();
    let mut safety_lines = Vec::new();
    let mut examples_lines = Vec::new();
    let mut errors_lines = Vec::new();

    let mut section = ParseSection::Preamble;
    let mut in_code_block = false;

    for (idx, line) in cleaned_lines.iter().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
        }

        if !in_code_block && trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if heading.eq_ignore_ascii_case("complexity") {
                section = ParseSection::Complexity;
                continue;
            } else if heading.eq_ignore_ascii_case("allocations")
                || heading.eq_ignore_ascii_case("allocation")
            {
                section = ParseSection::Allocations;
                continue;
            } else if heading.eq_ignore_ascii_case("safety") {
                section = ParseSection::Safety;
                continue;
            } else if heading.eq_ignore_ascii_case("examples")
                || heading.eq_ignore_ascii_case("example")
            {
                section = ParseSection::Examples;
                continue;
            } else if heading.eq_ignore_ascii_case("errors")
                || heading.eq_ignore_ascii_case("error")
            {
                section = ParseSection::Errors;
                continue;
            } else {
                section = ParseSection::Description;
            }
        }

        match section {
            ParseSection::Preamble => {
                if summary.is_none() {
                    if !trimmed.is_empty() {
                        summary = Some(trimmed.to_string());
                    }
                } else if trimmed.is_empty() {
                    section = ParseSection::Description;
                } else {
                    description_lines.push(line.clone());
                }
            }
            ParseSection::Description => {
                description_lines.push(line.clone());
            }
            ParseSection::Complexity => {
                complexity_lines.push(line.clone());
            }
            ParseSection::Allocations => {
                allocations_lines.push(line.clone());
            }
            ParseSection::Safety => {
                safety_lines.push(line.clone());
            }
            ParseSection::Examples => {
                examples_lines.push((idx as u32, line.clone()));
            }
            ParseSection::Errors => {
                errors_lines.push(line.clone());
            }
        }
    }

    // Extract doctest snippets from examples
    let mut examples = Vec::new();
    let mut current_code = Vec::new();
    let mut current_start_offset = 0u32;
    let mut current_should_panic = false;
    let mut current_no_run = false;
    let mut inside_example_block = false;

    for (offset, line) in examples_lines {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if inside_example_block {
                examples.push(DoctestSnippet {
                    code: current_code.join("\n"),
                    line_offset: current_start_offset,
                    should_panic: current_should_panic,
                    no_run: current_no_run,
                });
                current_code.clear();
                inside_example_block = false;
            } else {
                inside_example_block = true;
                current_start_offset = offset;
                let tags = trimmed.trim_start_matches('`').trim();
                current_should_panic = tags.contains("should_panic");
                current_no_run = tags.contains("no_run") || tags.contains("ignore");
            }
        } else if inside_example_block {
            current_code.push(line);
        }
    }

    let finish_text = |lines: Vec<String>| {
        let text = lines.join("\n").trim().to_string();
        if text.is_empty() { None } else { Some(text) }
    };

    (
        summary,
        DocSections {
            description: finish_text(description_lines),
            complexity: finish_text(complexity_lines),
            allocations: finish_text(allocations_lines),
            safety: finish_text(safety_lines),
            examples,
            errors: finish_text(errors_lines),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_structured_doc_comments() {
        let lines = vec![
            "/// Pushes an element onto the back of the collection.".to_string(),
            "///".to_string(),
            "/// Detailed paragraph explaining capacity growth and memory layout.".to_string(),
            "///".to_string(),
            "/// # Complexity".to_string(),
            "/// O(1) amortized.".to_string(),
            "///".to_string(),
            "/// # Allocations".to_string(),
            "/// Zero-Alloc if capacity > len; otherwise doubles buffer capacity.".to_string(),
            "///".to_string(),
            "/// # Safety".to_string(),
            "/// Safe to call on initialized collections.".to_string(),
            "///".to_string(),
            "/// # Examples".to_string(),
            "/// ```arandu".to_string(),
            "/// let mut v = Vec::new<int>()".to_string(),
            "/// v.push(42)".to_string(),
            "/// assert(v.len() == 1)".to_string(),
            "/// ```".to_string(),
        ];

        let (summary, sections) = parse_doc_comment_lines(&lines);

        assert_eq!(
            summary.as_deref(),
            Some("Pushes an element onto the back of the collection.")
        );
        assert_eq!(
            sections.description.as_deref(),
            Some("Detailed paragraph explaining capacity growth and memory layout.")
        );
        assert_eq!(sections.complexity.as_deref(), Some("O(1) amortized."));
        assert_eq!(
            sections.allocations.as_deref(),
            Some("Zero-Alloc if capacity > len; otherwise doubles buffer capacity.")
        );
        assert_eq!(
            sections.safety.as_deref(),
            Some("Safe to call on initialized collections.")
        );
        assert_eq!(sections.examples.len(), 1);
        assert!(sections.examples[0].code.contains("v.push(42)"));
        assert!(!sections.examples[0].should_panic);
        assert!(!sections.examples[0].no_run);
    }

    #[test]
    fn clean_doc_lines_handles_block_and_line_forms() {
        let block = vec![
            concat!(
                "/**\n",
                " * Adds two values.\n",
                " *\n",
                " * Keeps integer precision.\n",
                " */\n",
            )
            .to_owned(),
        ];
        assert_eq!(
            clean_doc_lines(&block),
            ["Adds two values.", "", "Keeps integer precision."]
        );

        let embedded = vec!["/** heading\ncontinued */".to_owned()];
        assert_eq!(clean_doc_lines(&embedded), ["heading", "continued"]);

        let single = vec!["/** A raw handle with */ marker. */".to_owned()];
        assert_eq!(clean_doc_lines(&single), ["A raw handle with */ marker."]);

        let lines = vec![
            "/// Adds two values.".to_owned(),
            "///".to_owned(),
            "//! Keeps integer precision.".to_owned(),
        ];
        assert_eq!(
            clean_doc_lines(&lines),
            ["Adds two values.", "", "Keeps integer precision."]
        );
    }

    #[test]
    fn derive_badges_from_effect_flags() {
        let pure_flags = EffectFlags::NONE;
        let badges = derive_effect_badges(pure_flags);
        assert!(badges.contains(&EffectBadge::Pure));
        assert!(badges.contains(&EffectBadge::ZeroAlloc));
        assert!(badges.contains(&EffectBadge::ThreadSafe));
        assert!(!badges.contains(&EffectBadge::AsyncSuspend));

        let heap_flags = EffectFlags::HEAP;
        let badges = derive_effect_badges(heap_flags);
        assert!(!badges.contains(&EffectBadge::Pure));
        assert!(!badges.contains(&EffectBadge::ZeroAlloc));
        assert!(badges.contains(&EffectBadge::ThreadSafe));

        let async_flags = EffectFlags::SUSPEND;
        let badges = derive_effect_badges(async_flags);
        assert!(badges.contains(&EffectBadge::AsyncSuspend));
    }
}
