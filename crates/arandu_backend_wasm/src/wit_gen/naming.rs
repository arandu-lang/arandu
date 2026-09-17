/// Keywords reserved in the WebAssembly Interface Types (WIT) specification.
const WIT_KEYWORDS: &[&str] = &[
    "record",
    "enum",
    "variant",
    "flags",
    "interface",
    "world",
    "package",
    "func",
    "type",
    "export",
    "import",
    "static",
    "constructor",
    "method",
    "use",
    "as",
    "include",
    "borrow",
    "own",
    "result",
    "option",
    "list",
    "tuple",
    "string",
    "bool",
    "char",
    "u8",
    "u16",
    "u32",
    "u64",
    "s8",
    "s16",
    "s32",
    "s64",
    "f32",
    "f64",
];

/// If the function name is in `Type.method` format, return `Some(method)`.
#[must_use]
pub fn extract_method_name(name: &str) -> Option<&str> {
    name.rsplit_once('.').map(|(_, method)| method)
}

/// Convert an Arandu identifier to a valid WIT-compatible identifier (kebab-case).
///
/// Underscores are replaced with hyphens. Reserved WIT keywords are escaped with `%`.
#[must_use]
pub fn to_wit_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    let mut prev_hyphen = false;
    for c in name.chars() {
        if c == '_' || c == '-' || c == '.' {
            if !prev_hyphen && !out.is_empty() {
                out.push('-');
                prev_hyphen = true;
            }
        } else if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        }
    }
    // Trim any trailing hyphen
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("item");
    }
    if WIT_KEYWORDS.contains(&out.as_str()) {
        format!("%{out}")
    } else {
        out
    }
}
