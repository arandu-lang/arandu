//! Language vocabulary shared by the syntax phases and the semantic contracts.
//!
//! These enums carry no AST structure, so the parser produces them and the
//! symbol table, HIR and AMIR consume them without depending on the parser.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Module,
    Internal,
    Public,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AbiKind {
    C,
    AranduIntrinsic,
    Rust,
    Other,
}

impl AbiKind {
    #[must_use]
    pub fn from_abi_str(abi: &str) -> Self {
        match abi.trim_matches('"') {
            "C" => Self::C,
            "arandu-intrinsic" => Self::AranduIntrinsic,
            "Rust" => Self::Rust,
            _ => Self::Other,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::C => "C",
            Self::AranduIntrinsic => "arandu-intrinsic",
            Self::Rust => "Rust",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for AbiKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Public target-layout expressions, distinct from declaration attributes and
/// ordinary calls (a user function named `sizeOf` is not an intrinsic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutQuery {
    Size,
    Align,
    TargetOS,
    TargetArch,
    TargetPointerWidth,
}

impl LayoutQuery {
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "sizeOf" => Some(Self::Size),
            "alignOf" => Some(Self::Align),
            "targetOS" => Some(Self::TargetOS),
            "targetArch" => Some(Self::TargetArch),
            "targetPointerWidth" => Some(Self::TargetPointerWidth),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Size => "sizeOf",
            Self::Align => "alignOf",
            Self::TargetOS => "targetOS",
            Self::TargetArch => "targetArch",
            Self::TargetPointerWidth => "targetPointerWidth",
        }
    }
}

/// Encode a Unicode scalar as a valid Arandu character literal.
#[must_use]
pub fn char_literal(value: char) -> String {
    let escaped = match value {
        '\n' => "\\n",
        '\t' => "\\t",
        '\r' => "\\r",
        '\0' => "\\0",
        '\\' => "\\\\",
        '\'' => "\\'",
        '"' => "\\\"",
        '$' => "\\$",
        value => return format!("'{value}'"),
    };
    format!("'{escaped}'")
}
