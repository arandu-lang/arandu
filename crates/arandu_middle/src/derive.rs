//! Built-in `@Derive(...)` specification and diagnostics shared across compiler phases.

use crate::Span;
use crate::diagnostics::{DiagCode, Diagnostic};

/// Supported built-in derive traits in `@Derive(...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeriveKind {
    PartialEq,
    Eq,
    Ord,
    Clone,
    Hash,
    Debug,
}

/// Distinct associated methods synthesized by `@Derive(...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeriveMethodKind {
    Eq,
    Cmp,
    Clone,
    Hash,
    Debug,
}

/// Why an argument inside `@Derive(...)` cannot be derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeriveArgIssue<'a> {
    StructuralCapability(&'a str),
    DisplayNotDerivable,
    UnknownTrait,
}

impl DeriveKind {
    /// Canonical order used when expanding implications and checking constraints.
    pub const ALL: [Self; 6] = [
        Self::PartialEq,
        Self::Eq,
        Self::Ord,
        Self::Clone,
        Self::Hash,
        Self::Debug,
    ];

    /// Parses one `@Derive` trait name.
    pub fn parse(name: &str) -> Result<Self, DeriveArgIssue<'_>> {
        match name {
            "PartialEq" => Ok(Self::PartialEq),
            "Eq" => Ok(Self::Eq),
            "Ord" => Ok(Self::Ord),
            "Clone" => Ok(Self::Clone),
            "Hash" => Ok(Self::Hash),
            "Debug" => Ok(Self::Debug),
            "Copy" => Err(DeriveArgIssue::StructuralCapability("Copy")),
            "Send" => Err(DeriveArgIssue::StructuralCapability("Send")),
            "Sync" => Err(DeriveArgIssue::StructuralCapability("Sync")),
            "Display" => Err(DeriveArgIssue::DisplayNotDerivable),
            _ => Err(DeriveArgIssue::UnknownTrait),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PartialEq => "PartialEq",
            Self::Eq => "Eq",
            Self::Ord => "Ord",
            Self::Clone => "Clone",
            Self::Hash => "Hash",
            Self::Debug => "Debug",
        }
    }

    #[must_use]
    pub const fn method_kind(self) -> DeriveMethodKind {
        match self {
            Self::PartialEq | Self::Eq => DeriveMethodKind::Eq,
            Self::Ord => DeriveMethodKind::Cmp,
            Self::Clone => DeriveMethodKind::Clone,
            Self::Hash => DeriveMethodKind::Hash,
            Self::Debug => DeriveMethodKind::Debug,
        }
    }
}

impl DeriveMethodKind {
    /// Canonical synthesis order for derived methods.
    pub const ALL: [Self; 5] = [Self::Eq, Self::Cmp, Self::Clone, Self::Hash, Self::Debug];

    #[must_use]
    pub const fn method_name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Cmp => "cmp",
            Self::Clone => "clone",
            Self::Hash => "hash",
            Self::Debug => "fmtDebug",
        }
    }

    #[must_use]
    pub fn from_method_name(name: &str) -> Option<Self> {
        match name {
            "eq" => Some(Self::Eq),
            "cmp" => Some(Self::Cmp),
            "clone" => Some(Self::Clone),
            "hash" => Some(Self::Hash),
            "fmtDebug" => Some(Self::Debug),
            _ => None,
        }
    }
}

/// Applies built-in `@Derive` implication rules (`Eq -> PartialEq`, `Ord -> Eq`, `Hash -> Eq`)
/// and returns the deduplicated set in deterministic canonical order.
#[must_use]
pub fn expand_derive_implications(kinds: &[DeriveKind]) -> Vec<DeriveKind> {
    let has = |k: DeriveKind| kinds.contains(&k);
    let mut partial_eq = has(DeriveKind::PartialEq);
    let mut eq = has(DeriveKind::Eq);
    let ord = has(DeriveKind::Ord);
    let clone = has(DeriveKind::Clone);
    let hash = has(DeriveKind::Hash);
    let debug = has(DeriveKind::Debug);
    if ord || hash {
        eq = true;
    }
    if eq {
        partial_eq = true;
    }
    let flags = [partial_eq, eq, ord, clone, hash, debug];
    DeriveKind::ALL
        .into_iter()
        .zip(flags)
        .filter_map(|(kind, active)| active.then_some(kind))
        .collect()
}

/// Returns the distinct derived methods needed by `kinds` (already closed under implications)
/// in deterministic canonical order.
#[must_use]
pub fn derived_methods_for_kinds(kinds: &[DeriveKind]) -> Vec<DeriveMethodKind> {
    let expanded = expand_derive_implications(kinds);
    let mut out = Vec::new();
    for method in DeriveMethodKind::ALL {
        if expanded.iter().any(|kind| kind.method_kind() == method) {
            out.push(method);
        }
    }
    out
}

/// Builds a structured, user-friendly `N014` diagnostic for invalid `@Derive(...)` arguments.
#[must_use]
pub fn invalid_derive_arg_diagnostic(
    issue: DeriveArgIssue<'_>,
    raw: &str,
    span: Span,
) -> Diagnostic {
    match issue {
        DeriveArgIssue::StructuralCapability("Copy") => Diagnostic::error(
            DiagCode::N014InvalidAnnotationArguments,
            "`Copy` is inferred structurally in Arandu and cannot be used in `@Derive(...)`",
            span,
        )
        .with_hint("types with all-`Copy` fields are `Copy` automatically; use `@NoCopy` to opt out"),
        DeriveArgIssue::StructuralCapability(name) => Diagnostic::error(
            DiagCode::N014InvalidAnnotationArguments,
            format!("`{name}` is inferred structurally in Arandu and cannot be used in `@Derive(...)`"),
            span,
        )
        .with_hint("`Send` and `Sync` are checked automatically from a type's fields and capabilities"),
        DeriveArgIssue::DisplayNotDerivable => Diagnostic::error(
            DiagCode::N014InvalidAnnotationArguments,
            "`Display` cannot be derived with `@Derive(...)`",
            span,
        )
        .with_hint("implement `Display` manually (`func Type.fmt(self: ref Type, f: mut ref Formatter): Result<usize, FmtError>`) or use `@Derive(Debug)`"),
        DeriveArgIssue::UnknownTrait => Diagnostic::error(
            DiagCode::N014InvalidAnnotationArguments,
            format!("unsupported derive target `{raw}` in `@Derive(...)`"),
            span,
        )
        .with_hint("supported `@Derive` traits are `PartialEq`, `Eq`, `Ord`, `Clone`, `Hash`, and `Debug`"),
    }
}
