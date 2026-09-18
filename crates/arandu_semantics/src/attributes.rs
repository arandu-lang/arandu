//! Built-in annotation registry and semantic validation.
//!
//! The parser preserves annotation spelling. This module is the single owner
//! of canonical names, migration aliases, valid targets and argument shapes.

use arandu_diagnostics::{CodeReplacement, DiagCode, Diagnostic, Hint};
use arandu_parser::ast_pool::AstPool;
use arandu_parser::{Attribute, ExprKind, FuncName, TopLevelDecl};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationId {
    Link,
    Test,
    Benchmark,
    Suppress,
    Deny,
    Forbid,
    NoFallback,
    Destructor,
    Effects,
    NoSuspend,
    Specialize,
    Repr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationTarget {
    Const,
    TypeAlias,
    Function,
    Method,
    Parameter,
    Struct,
    Field,
    Enum,
    EnumVariant,
    Interface,
    InterfaceMethod,
    ExternBlock,
    ExternFunction,
}

impl AnnotationTarget {
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Const => "constant",
            Self::TypeAlias => "type alias",
            Self::Function => "function",
            Self::Method => "method",
            Self::Parameter => "parameter",
            Self::Struct => "struct",
            Self::Field => "field",
            Self::Enum => "enum",
            Self::EnumVariant => "enum variant",
            Self::Interface => "interface",
            Self::InterfaceMethod => "interface method",
            Self::ExternBlock => "extern block",
            Self::ExternFunction => "extern function",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationArguments {
    None,
    OneString,
    OneStringOrIdent,
    EffectList,
}

impl AnnotationArguments {
    #[must_use]
    pub const fn synopsis(self) -> &'static str {
        match self {
            Self::None => "no arguments",
            Self::OneString => "one string argument",
            Self::OneStringOrIdent => "one string or identifier argument",
            Self::EffectList => "one or more effect identifiers or strings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationAvailability {
    Implemented,
    Planned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnotationSpec {
    pub id: AnnotationId,
    pub canonical_name: &'static str,
    pub legacy_aliases: &'static [&'static str],
    pub targets: &'static [AnnotationTarget],
    pub arguments: AnnotationArguments,
    pub repeatable: bool,
    pub availability: AnnotationAvailability,
    pub summary: &'static str,
}

const FUNCTION: &[AnnotationTarget] = &[AnnotationTarget::Function, AnnotationTarget::Method];
const FREE_FUNCTION: &[AnnotationTarget] = &[AnnotationTarget::Function];
const EXTERN_BLOCK: &[AnnotationTarget] = &[AnnotationTarget::ExternBlock];
const DECLARATIONS: &[AnnotationTarget] = &[
    AnnotationTarget::Const,
    AnnotationTarget::TypeAlias,
    AnnotationTarget::Function,
    AnnotationTarget::Method,
    AnnotationTarget::Struct,
    AnnotationTarget::Field,
    AnnotationTarget::Enum,
    AnnotationTarget::EnumVariant,
    AnnotationTarget::Interface,
    AnnotationTarget::InterfaceMethod,
    AnnotationTarget::ExternBlock,
    AnnotationTarget::ExternFunction,
];
const STRUCT_OR_ENUM: &[AnnotationTarget] = &[AnnotationTarget::Struct, AnnotationTarget::Enum];

pub static BUILTIN_ANNOTATIONS: &[AnnotationSpec] = &[
    AnnotationSpec {
        id: AnnotationId::Link,
        canonical_name: "Link",
        legacy_aliases: &["link"],
        targets: EXTERN_BLOCK,
        arguments: AnnotationArguments::OneString,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Links an extern block to a native library.",
    },
    AnnotationSpec {
        id: AnnotationId::Test,
        canonical_name: "Test",
        legacy_aliases: &[],
        targets: FREE_FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Marks a function as a test.",
    },
    AnnotationSpec {
        id: AnnotationId::Benchmark,
        canonical_name: "Benchmark",
        legacy_aliases: &[],
        targets: FREE_FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Marks a function as a benchmark.",
    },
    AnnotationSpec {
        id: AnnotationId::Suppress,
        canonical_name: "Suppress",
        legacy_aliases: &[],
        targets: DECLARATIONS,
        arguments: AnnotationArguments::OneString,
        repeatable: true,
        availability: AnnotationAvailability::Planned,
        summary: "Suppresses a named lint in a declaration scope.",
    },
    AnnotationSpec {
        id: AnnotationId::Deny,
        canonical_name: "Deny",
        legacy_aliases: &[],
        targets: DECLARATIONS,
        arguments: AnnotationArguments::OneString,
        repeatable: true,
        availability: AnnotationAvailability::Planned,
        summary: "Promotes a named lint to an error.",
    },
    AnnotationSpec {
        id: AnnotationId::Forbid,
        canonical_name: "Forbid",
        legacy_aliases: &[],
        targets: DECLARATIONS,
        arguments: AnnotationArguments::OneString,
        repeatable: true,
        availability: AnnotationAvailability::Planned,
        summary: "Forbids nested scopes from suppressing a named lint.",
    },
    AnnotationSpec {
        id: AnnotationId::NoFallback,
        canonical_name: "NoFallback",
        legacy_aliases: &["no_fallback", "no_generational_fallback"],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Forbids generational heap fallback in a function.",
    },
    AnnotationSpec {
        id: AnnotationId::Destructor,
        canonical_name: "Destructor",
        legacy_aliases: &[],
        targets: &[AnnotationTarget::Method],
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Associates one consuming cleanup method with its receiver type.",
    },
    AnnotationSpec {
        id: AnnotationId::Effects,
        canonical_name: "Effects",
        legacy_aliases: &["effects"],
        targets: FUNCTION,
        arguments: AnnotationArguments::EffectList,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Declares static effects and capabilities for a function.",
    },
    AnnotationSpec {
        id: AnnotationId::NoSuspend,
        canonical_name: "NoSuspend",
        legacy_aliases: &["nosuspend"],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Planned,
        summary: "Declares that a function cannot suspend.",
    },
    AnnotationSpec {
        id: AnnotationId::Specialize,
        canonical_name: "Specialize",
        legacy_aliases: &["specialize"],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Planned,
        summary: "Requests specialization of a function.",
    },
    AnnotationSpec {
        id: AnnotationId::Repr,
        canonical_name: "Repr",
        legacy_aliases: &["repr"],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::OneStringOrIdent,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Selects a representation contract for a data type.",
    },
];

#[derive(Debug, Default)]
pub struct ValidatedAnnotations {
    ids: Vec<AnnotationId>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidatedAnnotations {
    #[must_use]
    pub fn contains(&self, id: AnnotationId) -> bool {
        self.ids.contains(&id)
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == arandu_diagnostics::Severity::Error)
    }
}

enum NameResolution {
    Canonical(&'static AnnotationSpec),
    Legacy(&'static AnnotationSpec),
    Unknown,
}

/// Look up a canonical name or an explicit migration alias.
#[must_use]
pub fn annotation_spec(name: &str) -> Option<(&'static AnnotationSpec, bool)> {
    match resolve_name(name) {
        NameResolution::Canonical(spec) => Some((spec, false)),
        NameResolution::Legacy(spec) => Some((spec, true)),
        NameResolution::Unknown => None,
    }
}

fn resolve_name(name: &str) -> NameResolution {
    for spec in BUILTIN_ANNOTATIONS {
        if name == spec.canonical_name {
            return NameResolution::Canonical(spec);
        }
        if spec.legacy_aliases.contains(&name) {
            return NameResolution::Legacy(spec);
        }
    }
    NameResolution::Unknown
}

fn arguments_match(spec: &AnnotationSpec, attr: &Attribute, pool: &AstPool) -> bool {
    match spec.arguments {
        AnnotationArguments::None => attr.args.is_empty(),
        AnnotationArguments::OneString => {
            attr.args.len() == 1
                && attr.args.first().is_some_and(|arg| {
                    matches!(pool.expr(*arg), ExprKind::InterpolatedString { .. })
                })
        }
        AnnotationArguments::OneStringOrIdent => {
            attr.args.len() == 1
                && attr.args.first().is_some_and(|arg| {
                    matches!(
                        pool.expr(*arg),
                        ExprKind::Path { .. } | ExprKind::InterpolatedString { .. }
                    )
                })
        }
        AnnotationArguments::EffectList => {
            !attr.args.is_empty()
                && attr.args.iter().all(|arg| {
                    matches!(
                        pool.expr(*arg),
                        ExprKind::Path { .. } | ExprKind::InterpolatedString { .. }
                    )
                })
        }
    }
}

#[must_use]
pub fn validate_attributes(
    attrs: &[Attribute],
    target: AnnotationTarget,
    pool: &AstPool,
) -> ValidatedAnnotations {
    let mut out = ValidatedAnnotations::default();
    for attr in attrs {
        let (spec, legacy) = match resolve_name(&attr.name) {
            NameResolution::Canonical(spec) => (spec, false),
            NameResolution::Legacy(spec) => (spec, true),
            NameResolution::Unknown => {
                out.diagnostics.push(Diagnostic::error(
                    DiagCode::N012UnknownAnnotation,
                    format!("unknown annotation '@{}'", attr.name),
                    attr.name_span,
                ));
                continue;
            }
        };

        if spec.availability == AnnotationAvailability::Planned {
            out.diagnostics.push(
                Diagnostic::error(
                    DiagCode::N012UnknownAnnotation,
                    format!(
                        "annotation '@{}' is planned but not available",
                        spec.canonical_name
                    ),
                    attr.name_span,
                )
                .with_note(
                    "the annotation name is reserved, but its semantics are not implemented",
                ),
            );
            continue;
        }
        if !spec.targets.contains(&target) {
            out.diagnostics.push(Diagnostic::error(
                DiagCode::N013InvalidAnnotationTarget,
                format!(
                    "annotation '@{}' cannot be applied to a {}",
                    spec.canonical_name,
                    target.description()
                ),
                attr.name_span,
            ));
            continue;
        }
        if !arguments_match(spec, attr, pool) {
            out.diagnostics.push(Diagnostic::error(
                DiagCode::N014InvalidAnnotationArguments,
                format!(
                    "annotation '@{}' expects {}",
                    spec.canonical_name,
                    spec.arguments.synopsis()
                ),
                attr.span,
            ));
            continue;
        }
        if spec.arguments == AnnotationArguments::EffectList {
            let mut has_invalid_effect = false;
            for arg in &attr.args {
                let name = match pool.expr(*arg) {
                    ExprKind::Path { path } => path.first().map(|s| s.as_str()),
                    ExprKind::InterpolatedString { parts } => {
                        let part_ids = pool.string_part_list(*parts);
                        part_ids.first().and_then(|&id| match pool.string_part(id) {
                            arandu_parser::StringPart::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                    }
                    _ => None,
                };
                if let Some(eff_name) = name
                    && arandu_middle::EffectFlags::from_name(eff_name).is_none()
                {
                    out.diagnostics.push(Diagnostic::error(
                        DiagCode::N012UnknownAnnotation,
                        format!("unknown effect '{eff_name}' in @{}", spec.canonical_name),
                        attr.span,
                    ));
                    has_invalid_effect = true;
                }
            }
            if has_invalid_effect {
                continue;
            }
        }
        if spec.id == AnnotationId::Repr {
            let repr_val = attr.args.first().and_then(|arg| match pool.expr(*arg) {
                ExprKind::Path { path } => path.first().map(|s| s.as_str()),
                ExprKind::InterpolatedString { parts } => {
                    let part_ids = pool.string_part_list(*parts);
                    part_ids.first().and_then(|&id| match pool.string_part(id) {
                        arandu_parser::StringPart::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                }
                _ => None,
            });
            if let Some(val) = repr_val
                && !val.eq_ignore_ascii_case("c")
            {
                out.diagnostics.push(Diagnostic::error(
                    DiagCode::N014InvalidAnnotationArguments,
                    format!("unsupported representation '{val}' in @Repr; expected 'C'"),
                    attr.span,
                ));
                continue;
            }
        }
        if !spec.repeatable && out.ids.contains(&spec.id) {
            out.diagnostics.push(Diagnostic::error(
                DiagCode::N015DuplicateAnnotation,
                format!("annotation '@{}' cannot be repeated", spec.canonical_name),
                attr.name_span,
            ));
            continue;
        }

        if legacy {
            out.diagnostics.push(
                Diagnostic::warning(
                    DiagCode::W008LegacyAnnotationName,
                    format!(
                        "legacy annotation name '@{}'; use '@{}'",
                        attr.name, spec.canonical_name
                    ),
                    attr.name_span,
                )
                .with_hint_replacement(Hint {
                    message: format!("replace with '{}'", spec.canonical_name),
                    replacement: Some(CodeReplacement {
                        span: attr.name_span,
                        new_text: spec.canonical_name.to_string(),
                    }),
                }),
            );
        }
        out.ids.push(spec.id);
    }
    out
}

/// Validate one top-level declaration and its directly nested annotated parts.
#[must_use]
pub fn validate_decl_attributes(decl: &TopLevelDecl, pool: &AstPool) -> ValidatedAnnotations {
    let mut combined = ValidatedAnnotations::default();
    let mut append = |validated: ValidatedAnnotations| {
        combined.ids.extend(validated.ids);
        combined.diagnostics.extend(validated.diagnostics);
    };
    match decl {
        TopLevelDecl::Const(d) => {
            append(validate_attributes(&d.attrs, AnnotationTarget::Const, pool))
        }
        TopLevelDecl::TypeAlias(d) => {
            append(validate_attributes(
                &d.attrs,
                AnnotationTarget::TypeAlias,
                pool,
            ));
        }
        TopLevelDecl::Func(d) => {
            let target = if matches!(d.name, FuncName::Method { .. }) {
                AnnotationTarget::Method
            } else {
                AnnotationTarget::Function
            };
            append(validate_attributes(&d.attrs, target, pool));
            for param in &d.params {
                append(validate_attributes(
                    &param.attrs,
                    AnnotationTarget::Parameter,
                    pool,
                ));
            }
        }
        TopLevelDecl::Struct(d) => {
            append(validate_attributes(
                &d.attrs,
                AnnotationTarget::Struct,
                pool,
            ));
            for field in &d.fields {
                append(validate_attributes(
                    &field.attrs,
                    AnnotationTarget::Field,
                    pool,
                ));
            }
        }
        TopLevelDecl::Enum(d) => {
            append(validate_attributes(&d.attrs, AnnotationTarget::Enum, pool));
            for variant in &d.variants {
                append(validate_attributes(
                    &variant.attrs,
                    AnnotationTarget::EnumVariant,
                    pool,
                ));
            }
        }
        TopLevelDecl::Interface(d) => {
            append(validate_attributes(
                &d.attrs,
                AnnotationTarget::Interface,
                pool,
            ));
            for member in &d.members {
                append(validate_attributes(
                    &member.attrs,
                    AnnotationTarget::InterfaceMethod,
                    pool,
                ));
                for param in &member.params {
                    append(validate_attributes(
                        &param.attrs,
                        AnnotationTarget::Parameter,
                        pool,
                    ));
                }
            }
        }
        TopLevelDecl::Extern(d) => {
            append(validate_attributes(
                &d.attrs,
                AnnotationTarget::ExternBlock,
                pool,
            ));
            for member in &d.members {
                append(validate_attributes(
                    &member.attrs,
                    AnnotationTarget::ExternFunction,
                    pool,
                ));
                for param in &member.params {
                    append(validate_attributes(
                        &param.attrs,
                        AnnotationTarget::Parameter,
                        pool,
                    ));
                }
            }
        }
        TopLevelDecl::Error(_) => {}
    }
    combined
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_parser::parse;

    fn validate(source: &str) -> ValidatedAnnotations {
        let program = parse(source).expect("fixture parses");
        let decl = program.pool.decl(program.decls[0]);
        validate_decl_attributes(decl, &program.pool)
    }

    #[test]
    fn canonical_no_fallback_is_recognized() {
        let result = validate("@NoFallback\nfunc main() {}\n");
        assert!(result.contains(AnnotationId::NoFallback));
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn legacy_name_keeps_semantics_and_has_exact_replacement() {
        let result = validate("@no_fallback\nfunc main() {}\n");
        assert!(result.contains(AnnotationId::NoFallback));
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, DiagCode::W008LegacyAnnotationName);
        let replacement = diagnostic.hints[0]
            .replacement
            .as_ref()
            .expect("replacement");
        assert_eq!(replacement.span.start, 1);
        assert_eq!(replacement.span.end, 12);
        assert_eq!(replacement.new_text, "NoFallback");
    }

    #[test]
    fn invalid_target_does_not_enable_annotation() {
        let result = validate("@NoFallback\nstruct Main {}\n");
        assert!(!result.contains(AnnotationId::NoFallback));
        assert_eq!(
            result.diagnostics[0].code,
            DiagCode::N013InvalidAnnotationTarget
        );
    }

    #[test]
    fn duplicate_non_repeatable_annotation_is_rejected() {
        let result = validate("@NoFallback\n@NoFallback\nfunc main() {}\n");
        assert_eq!(
            result.diagnostics[0].code,
            DiagCode::N015DuplicateAnnotation
        );
    }

    #[test]
    fn destructor_is_canonical_and_method_only() {
        let result = validate("@Destructor\nfunc Resource.close(own self): void {}\n");
        assert!(result.contains(AnnotationId::Destructor));
        assert!(result.diagnostics.is_empty());

        let result = validate("@Destructor\nfunc close(): void {}\n");
        assert!(!result.contains(AnnotationId::Destructor));
        assert_eq!(
            result.diagnostics[0].code,
            DiagCode::N013InvalidAnnotationTarget
        );
    }

    #[test]
    fn repr_c_string_and_ident_are_accepted() {
        let result_str = validate("@Repr(\"C\")\nstruct Point { x: i32, y: i32 }\n");
        assert!(result_str.contains(AnnotationId::Repr));
        assert!(result_str.diagnostics.is_empty());

        let result_ident = validate("@Repr(C)\nstruct Point { x: i32, y: i32 }\n");
        assert!(result_ident.contains(AnnotationId::Repr));
        assert!(result_ident.diagnostics.is_empty());

        let result_legacy = validate("@repr(\"C\")\nstruct Point { x: i32, y: i32 }\n");
        assert!(result_legacy.contains(AnnotationId::Repr));
        assert_eq!(result_legacy.diagnostics.len(), 1);
        assert_eq!(
            result_legacy.diagnostics[0].code,
            DiagCode::W008LegacyAnnotationName
        );

        let result_invalid_repr = validate("@Repr(\"Rust\")\nstruct Point { x: i32, y: i32 }\n");
        assert!(!result_invalid_repr.contains(AnnotationId::Repr));
        assert_eq!(
            result_invalid_repr.diagnostics[0].code,
            DiagCode::N014InvalidAnnotationArguments
        );

        let result_invalid_target = validate("@Repr(\"C\")\nfunc foo(): void {}\n");
        assert!(!result_invalid_target.contains(AnnotationId::Repr));
        assert_eq!(
            result_invalid_target.diagnostics[0].code,
            DiagCode::N013InvalidAnnotationTarget
        );
    }
}
