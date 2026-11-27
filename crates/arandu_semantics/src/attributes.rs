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
    Export,
    Test,
    Benchmark,
    Suppress,
    Deny,
    Forbid,
    NoFallback,
    NoCopy,
    NoSend,
    NoSync,
    UnsafeSend,
    UnsafeSync,
    Destructor,
    Unsafe,
    Effects,
    NoSuspend,
    Specialize,
    Repr,
    Union,
    Cold,
    Derive,
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
    /// One or more string arguments (e.g. `@Suppress("unused_variable", "shadowing")`).
    OneOrMoreStrings,
    /// No argument or exactly one string: the declaration name is the default,
    /// and the string overrides it (e.g. `@Export` / `@Export("c_name")`).
    OptionalString,
    OneStringOrIdent,
    EffectList,
    OneOrMoreIdents,
}

impl AnnotationArguments {
    #[must_use]
    pub const fn synopsis(self) -> &'static str {
        match self {
            Self::None => "no arguments",
            Self::OneString => "one string argument",
            Self::OneOrMoreStrings => "one or more string arguments",
            Self::OptionalString => "no arguments or one string argument",
            Self::OneStringOrIdent => "one string or identifier argument",
            Self::EffectList => "one or more effect identifiers or strings",
            Self::OneOrMoreIdents => {
                "one or more trait identifiers (PartialEq, Eq, Ord, Clone, Hash, Debug)"
            }
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
    /// Spellings accepted before 0.2.0 (RFC 0002). They are unknown names
    /// now and only suggest the canonical one.
    pub renamed_from: &'static [&'static str],
    pub targets: &'static [AnnotationTarget],
    pub arguments: AnnotationArguments,
    pub repeatable: bool,
    pub availability: AnnotationAvailability,
    pub summary: &'static str,
}

const FUNCTION: &[AnnotationTarget] = &[AnnotationTarget::Function, AnnotationTarget::Method];
const FREE_FUNCTION: &[AnnotationTarget] = &[AnnotationTarget::Function];
const EXTERN_BLOCK: &[AnnotationTarget] = &[AnnotationTarget::ExternBlock];
const STRUCT_OR_ENUM: &[AnnotationTarget] = &[AnnotationTarget::Struct, AnnotationTarget::Enum];

pub static BUILTIN_ANNOTATIONS: &[AnnotationSpec] = &[
    AnnotationSpec {
        id: AnnotationId::Link,
        canonical_name: "Link",
        renamed_from: &["link"],
        targets: EXTERN_BLOCK,
        arguments: AnnotationArguments::OneString,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Links an extern block to a native library.",
    },
    AnnotationSpec {
        id: AnnotationId::Export,
        canonical_name: "Export",
        renamed_from: &["FfiExport"],
        // Methods reach `lower_export` so the rejection for a non-destructor
        // method names the real rule (`T049`) instead of a generic target
        // error. Silently ignoring the annotation would be the `#[no_mangle]`
        // trap this design exists to avoid.
        targets: FUNCTION,
        arguments: AnnotationArguments::OptionalString,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Exports a function to the linker with a flat C symbol and the target C ABI.",
    },
    AnnotationSpec {
        id: AnnotationId::Test,
        canonical_name: "Test",
        renamed_from: &[],
        targets: FREE_FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Marks a function as a test.",
    },
    AnnotationSpec {
        id: AnnotationId::Benchmark,
        canonical_name: "Benchmark",
        renamed_from: &[],
        targets: FREE_FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Marks a function as a benchmark.",
    },
    AnnotationSpec {
        id: AnnotationId::Suppress,
        canonical_name: "Suppress",
        renamed_from: &[],
        targets: FUNCTION,
        arguments: AnnotationArguments::OneOrMoreStrings,
        repeatable: true,
        availability: AnnotationAvailability::Implemented,
        summary: "Suppresses named warnings in a function body.",
    },
    AnnotationSpec {
        id: AnnotationId::Deny,
        canonical_name: "Deny",
        renamed_from: &[],
        targets: FUNCTION,
        arguments: AnnotationArguments::OneOrMoreStrings,
        repeatable: true,
        availability: AnnotationAvailability::Implemented,
        summary: "Makes named warnings an error in a function body.",
    },
    AnnotationSpec {
        id: AnnotationId::Forbid,
        canonical_name: "Forbid",
        renamed_from: &[],
        targets: FUNCTION,
        arguments: AnnotationArguments::OneOrMoreStrings,
        repeatable: true,
        availability: AnnotationAvailability::Implemented,
        summary: "Makes named warnings an error that cannot be suppressed.",
    },
    AnnotationSpec {
        id: AnnotationId::NoFallback,
        canonical_name: "NoFallback",
        renamed_from: &["no_fallback", "no_generational_fallback"],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Forbids generational heap fallback in a function.",
    },
    AnnotationSpec {
        id: AnnotationId::NoCopy,
        canonical_name: "NoCopy",
        renamed_from: &[],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Opts a struct or enum out of structural Copy, giving it move-only semantics.",
    },
    AnnotationSpec {
        id: AnnotationId::NoSend,
        canonical_name: "NoSend",
        renamed_from: &[],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Opts a struct or enum out of structural Send, preventing cross-thread transfer.",
    },
    AnnotationSpec {
        id: AnnotationId::NoSync,
        canonical_name: "NoSync",
        renamed_from: &[],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Opts a struct or enum out of structural Sync, preventing cross-thread sharing.",
    },
    AnnotationSpec {
        id: AnnotationId::UnsafeSend,
        canonical_name: "UnsafeSend",
        renamed_from: &[],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Unsafely asserts structural Send for a type whose fields include raw pointers while still propagating generic T: Send bounds.",
    },
    AnnotationSpec {
        id: AnnotationId::UnsafeSync,
        canonical_name: "UnsafeSync",
        renamed_from: &[],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Unsafely asserts structural Sync for a type whose fields include raw pointers while still propagating generic T: Sync bounds.",
    },
    AnnotationSpec {
        id: AnnotationId::Destructor,
        canonical_name: "Destructor",
        renamed_from: &[],
        targets: &[AnnotationTarget::Method],
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Associates one consuming cleanup method with its receiver type.",
    },
    AnnotationSpec {
        id: AnnotationId::Unsafe,
        canonical_name: "Unsafe",
        renamed_from: &["unsafe"],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Requires callers to uphold documented preconditions in an unsafe block.",
    },
    AnnotationSpec {
        id: AnnotationId::Effects,
        canonical_name: "Effects",
        renamed_from: &["effects"],
        targets: FUNCTION,
        arguments: AnnotationArguments::EffectList,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Declares static effects and capabilities for a function.",
    },
    AnnotationSpec {
        id: AnnotationId::NoSuspend,
        canonical_name: "NoSuspend",
        renamed_from: &["nosuspend"],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Declares that a function cannot suspend.",
    },
    AnnotationSpec {
        id: AnnotationId::Specialize,
        canonical_name: "Specialize",
        renamed_from: &["specialize"],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Planned,
        summary: "Requests specialization of a function.",
    },
    AnnotationSpec {
        id: AnnotationId::Repr,
        canonical_name: "Repr",
        renamed_from: &["repr"],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::OneStringOrIdent,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Selects a representation contract for a data type.",
    },
    AnnotationSpec {
        id: AnnotationId::Union,
        canonical_name: "Union",
        renamed_from: &["union"],
        targets: &[AnnotationTarget::Struct],
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Marks a nominal aggregate as an untagged union with overlapping fields.",
    },
    AnnotationSpec {
        id: AnnotationId::Cold,
        canonical_name: "Cold",
        renamed_from: &[],
        targets: FUNCTION,
        arguments: AnnotationArguments::None,
        repeatable: false,
        availability: AnnotationAvailability::Implemented,
        summary: "Marks a function or method as infrequently executed (suppresses inlining and emits cold hints).",
    },
    AnnotationSpec {
        id: AnnotationId::Derive,
        canonical_name: "Derive",
        renamed_from: &[],
        targets: STRUCT_OR_ENUM,
        arguments: AnnotationArguments::OneOrMoreIdents,
        repeatable: true,
        availability: AnnotationAvailability::Implemented,
        summary: "Generates built-in trait implementations (PartialEq, Eq, Ord, Clone, Hash, Debug).",
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

/// The built-in annotation spelled `name`.
#[must_use]
pub fn annotation_spec(name: &str) -> Option<&'static AnnotationSpec> {
    BUILTIN_ANNOTATIONS
        .iter()
        .find(|spec| spec.canonical_name == name)
}

/// The annotation a spelling removed in 0.2.0 was renamed to.
#[must_use]
pub fn renamed_annotation(name: &str) -> Option<&'static AnnotationSpec> {
    BUILTIN_ANNOTATIONS
        .iter()
        .find(|spec| spec.renamed_from.contains(&name))
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
        AnnotationArguments::OneOrMoreStrings => {
            !attr.args.is_empty()
                && attr
                    .args
                    .iter()
                    .all(|arg| matches!(pool.expr(*arg), ExprKind::InterpolatedString { .. }))
        }
        AnnotationArguments::OptionalString => {
            attr.args.is_empty()
                || (attr.args.len() == 1
                    && attr.args.first().is_some_and(|arg| {
                        matches!(pool.expr(*arg), ExprKind::InterpolatedString { .. })
                    }))
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
        AnnotationArguments::OneOrMoreIdents => {
            !attr.args.is_empty()
                && attr.args.iter().all(
                    |arg| matches!(pool.expr(*arg), ExprKind::Path { path } if path.len() == 1),
                )
        }
    }
}

/// Extracts the single static text argument of an annotation.
///
/// Returns `None` when the annotation has no argument or when the argument is
/// not a purely textual string literal (interpolated parts are rejected).
#[must_use]
pub fn attribute_string_arg(attr: &Attribute, pool: &AstPool) -> Option<String> {
    let [arg] = attr.args.as_slice() else {
        return None;
    };
    let ExprKind::InterpolatedString { parts } = pool.expr(*arg) else {
        return None;
    };
    let mut value = String::new();
    for &part in pool.string_part_list(*parts) {
        match pool.string_part(part) {
            arandu_parser::StringPart::Text { text, .. } => value.push_str(text),
            _ => return None,
        }
    }
    Some(value)
}

/// Extracts static text arguments of an annotation.
///
/// Returns `None` when any argument is not a purely textual string literal.
#[must_use]
pub fn attribute_string_args(attr: &Attribute, pool: &AstPool) -> Option<Vec<String>> {
    let mut values = Vec::with_capacity(attr.args.len());
    for &arg in &attr.args {
        let ExprKind::InterpolatedString { parts } = pool.expr(arg) else {
            return None;
        };
        let mut value = String::new();
        for &part in pool.string_part_list(*parts) {
            match pool.string_part(part) {
                arandu_parser::StringPart::Text { text, .. } => value.push_str(text),
                _ => return None,
            }
        }
        values.push(value);
    }
    Some(values)
}

/// Whether `name` is a valid flat symbol for a C toolchain.
#[must_use]
pub fn is_c_symbol_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[must_use]
pub fn validate_attributes(
    attrs: &[Attribute],
    target: AnnotationTarget,
    pool: &AstPool,
) -> ValidatedAnnotations {
    let mut out = ValidatedAnnotations::default();
    for attr in attrs {
        let Some(spec) = annotation_spec(&attr.name) else {
            let mut diagnostic = Diagnostic::error(
                DiagCode::N012UnknownAnnotation,
                format!("unknown annotation '@{}'", attr.name),
                attr.name_span,
            );
            // A removed spelling names its replacement, exactly where it
            // stands, so editors apply the rename without parsing text.
            if let Some(renamed) = renamed_annotation(&attr.name) {
                diagnostic = diagnostic
                    .with_note(format!(
                        "'@{}' was renamed to '@{}' in 0.2.0",
                        attr.name, renamed.canonical_name
                    ))
                    .with_hint_replacement(Hint {
                        message: format!("replace with '{}'", renamed.canonical_name),
                        replacement: Some(CodeReplacement {
                            span: attr.name_span,
                            new_text: renamed.canonical_name.to_string(),
                        }),
                    });
            }
            out.diagnostics.push(diagnostic);
            continue;
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
        if spec.id == AnnotationId::Derive {
            let mut has_invalid_derive = false;
            for &arg in &attr.args {
                let Some(name) = (match pool.expr(arg) {
                    ExprKind::Path { path } if path.len() == 1 => Some(path[0].as_str()),
                    _ => None,
                }) else {
                    continue;
                };
                if let Err(issue) = arandu_middle::derive::DeriveKind::parse(name) {
                    out.diagnostics
                        .push(arandu_middle::derive::invalid_derive_arg_diagnostic(
                            issue,
                            name,
                            pool.expr_span(arg),
                        ));
                    has_invalid_derive = true;
                }
            }
            if has_invalid_derive {
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
        if spec.id == AnnotationId::Export && !attr.args.is_empty() {
            match attribute_string_arg(attr, pool) {
                Some(name) if is_c_symbol_name(&name) => {}
                Some(name) => {
                    out.diagnostics.push(Diagnostic::error(
                        DiagCode::N014InvalidAnnotationArguments,
                        format!(
                            "invalid exported symbol '{name}'; expected a C identifier ([A-Za-z_][A-Za-z0-9_]*)"
                        ),
                        attr.span,
                    ));
                    continue;
                }
                None => {
                    out.diagnostics.push(Diagnostic::error(
                        DiagCode::N014InvalidAnnotationArguments,
                        "@Export expects a static string literal naming the C symbol".to_owned(),
                        attr.span,
                    ));
                    continue;
                }
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
        let conflict = match spec.id {
            AnnotationId::NoSend if out.ids.contains(&AnnotationId::UnsafeSend) => {
                Some("UnsafeSend")
            }
            AnnotationId::UnsafeSend if out.ids.contains(&AnnotationId::NoSend) => Some("NoSend"),
            AnnotationId::NoSync if out.ids.contains(&AnnotationId::UnsafeSync) => {
                Some("UnsafeSync")
            }
            AnnotationId::UnsafeSync if out.ids.contains(&AnnotationId::NoSync) => Some("NoSync"),
            _ => None,
        };
        if let Some(other) = conflict {
            out.diagnostics.push(Diagnostic::error(
                DiagCode::N015DuplicateAnnotation,
                format!(
                    "conflicting concurrency annotations '@{}' and '@{other}' on the same declaration",
                    spec.canonical_name
                ),
                attr.name_span,
            ));
            continue;
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
        TopLevelDecl::Submodule(d) => {
            for &inner_id in &d.decls {
                append(validate_decl_attributes(pool.decl(inner_id), pool));
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
    fn removed_name_is_unknown_with_an_exact_replacement() {
        let result = validate("@no_fallback\nfunc main() {}\n");
        assert!(!result.contains(AnnotationId::NoFallback));
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, DiagCode::N012UnknownAnnotation);
        assert!(
            diagnostic
                .notes
                .iter()
                .any(|note| note.contains("renamed to '@NoFallback'"))
        );
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
    fn export_accepts_bare_and_named_forms() {
        for source in [
            "@Export\nfunc apply(raw: u32): u32 { return raw }\n",
            "@Export(\"arandu_core_apply\")\nfunc apply(raw: u32): u32 { return raw }\n",
        ] {
            let result = validate(source);
            assert!(result.contains(AnnotationId::Export));
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        }
    }

    #[test]
    fn export_rejects_invalid_or_dynamic_symbol_names() {
        for name in ["\"bad-name\"", "\"1bad\"", "\"\"", "\"${symbol}\""] {
            let source = format!("@Export({name})\nfunc apply(raw: u32): u32 {{ return raw }}\n");
            let result = validate(&source);
            assert!(
                !result.contains(AnnotationId::Export),
                "{name} must be rejected"
            );
            assert_eq!(
                result.diagnostics[0].code,
                DiagCode::N014InvalidAnnotationArguments
            );
        }
    }
    #[test]
    fn repr_c_string_and_ident_are_accepted() {
        let result_str = validate("@Repr(\"C\")\nstruct Point { x: i32, y: i32 }\n");
        assert!(result_str.contains(AnnotationId::Repr));
        assert!(result_str.diagnostics.is_empty());

        let result_ident = validate("@Repr(C)\nstruct Point { x: i32, y: i32 }\n");
        assert!(result_ident.contains(AnnotationId::Repr));
        assert!(result_ident.diagnostics.is_empty());

        let result_removed = validate("@repr(\"C\")\nstruct Point { x: i32, y: i32 }\n");
        assert!(!result_removed.contains(AnnotationId::Repr));
        assert_eq!(result_removed.diagnostics.len(), 1);
        assert_eq!(
            result_removed.diagnostics[0].code,
            DiagCode::N012UnknownAnnotation
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

    #[test]
    fn cold_is_canonical_and_func_only() {
        let valid = validate("@Cold\nfunc slow(): void {}\n");
        assert!(valid.contains(AnnotationId::Cold));
        assert!(valid.diagnostics.is_empty(), "{:?}", valid.diagnostics);

        let wrong_target = validate("@Cold\nstruct Slow {}\n");
        assert!(!wrong_target.contains(AnnotationId::Cold));
        assert_eq!(
            wrong_target.diagnostics[0].code,
            DiagCode::N013InvalidAnnotationTarget
        );

        let extra_args = validate("@Cold(1)\nfunc slow(): void {}\n");
        assert!(!extra_args.contains(AnnotationId::Cold));
        assert_eq!(
            extra_args.diagnostics[0].code,
            DiagCode::N014InvalidAnnotationArguments
        );
    }
}
