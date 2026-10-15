//! Interned literal storage for AMIR constants (C2).
//!
//! Lexemes keep source spelling (underscores, base prefixes). Use
//! [`parse_int_literal`] / [`parse_float_literal`] at consume sites (codegen, SCCP).
//!
//! Entries use [`smol_str::SmolStr`]: short strings are stack-inline (no heap);
//! longer ones are refcounted. Callers pass `&str` via [`AmirLiteralPool::intern_str`]
//! etc. so lowering does not need to `String::clone` just to intern.

use rustc_hash::FxHashMap;
use smol_str::SmolStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LiteralId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AmirLiteralEntry {
    Int(SmolStr),
    Float(SmolStr),
    /// Frozen IEEE value: residual CTFE must not reparse a decimal display.
    FloatBits(crate::ctfe::ConstFloat),
    Str(SmolStr),
    Char(SmolStr),
}

#[derive(Debug, Default, Clone)]
pub struct AmirLiteralPool {
    pub entries: Vec<AmirLiteralEntry>,
    pub index: FxHashMap<AmirLiteralEntry, LiteralId>,
}

impl AmirLiteralPool {
    pub fn intern(&mut self, entry: AmirLiteralEntry) -> LiteralId {
        if let Some(&id) = self.index.get(&entry) {
            return id;
        }
        let id = LiteralId(self.entries.len() as u32);
        self.index.insert(entry.clone(), id);
        self.entries.push(entry);
        id
    }

    /// Intern an integer lexeme. Accepts `&str` or owned [`SmolStr`] (HIR path).
    #[inline]
    pub fn intern_int(&mut self, s: impl Into<SmolStr>) -> LiteralId {
        self.intern(AmirLiteralEntry::Int(s.into()))
    }

    /// Intern a float lexeme.
    #[inline]
    pub fn intern_float(&mut self, s: impl Into<SmolStr>) -> LiteralId {
        self.intern(AmirLiteralEntry::Float(s.into()))
    }

    #[inline]
    pub fn intern_float_bits(&mut self, value: crate::ctfe::ConstFloat) -> LiteralId {
        self.intern(AmirLiteralEntry::FloatBits(value))
    }

    /// Intern a string literal body.
    #[inline]
    pub fn intern_str(&mut self, s: impl Into<SmolStr>) -> LiteralId {
        self.intern(AmirLiteralEntry::Str(s.into()))
    }

    /// Intern a char lexeme (source spelling, possibly multi-byte escape form).
    #[inline]
    pub fn intern_char(&mut self, s: impl Into<SmolStr>) -> LiteralId {
        self.intern(AmirLiteralEntry::Char(s.into()))
    }

    #[must_use]
    pub fn get(&self, id: LiteralId) -> &AmirLiteralEntry {
        &self.entries[id.0 as usize]
    }
}

/// Parse an Arandu integer lexeme (`1_000`, `0xFF`, `0b1010_0001`, `0o77`).
#[must_use]
pub fn parse_int_literal(s: &str) -> Option<i128> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (negative, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else {
        (false, s)
    };
    let (radix, digits) =
        if let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16u32, rest)
        } else if let Some(rest) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2, rest)
        } else if let Some(rest) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (8, rest)
        } else {
            (10, body)
        };
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    let value = i128::from_str_radix(&cleaned, radix).ok()?;
    Some(if negative { -value } else { value })
}

/// Parse an Arandu float lexeme (underscores allowed: `3.14_15`).
#[must_use]
pub fn parse_float_literal(s: &str) -> Option<f64> {
    let cleaned: String = s.trim().chars().filter(|&c| c != '_').collect();
    cleaned.parse().ok()
}

/// C-compatible spelling of an int lexeme (decimal, no underscores).
#[must_use]
pub fn int_literal_c_source(s: &str) -> Option<String> {
    parse_int_literal(s).map(|v| {
        if v > i64::MAX as i128 {
            format!("{v}ULL")
        } else if v == i128::from(i64::MIN) {
            // C lexes the positive magnitude before unary minus. Spell the
            // minimum using signed representable operands, not an oversized
            // decimal token with implementation-dependent unsigned semantics.
            "(-9223372036854775807LL - 1LL)".to_owned()
        } else if v < i64::MIN as i128 {
            format!("{v}LL")
        } else {
            v.to_string()
        }
    })
}

/// C-compatible spelling of a float lexeme (no underscores).
#[must_use]
pub fn float_literal_c_source(s: &str) -> Option<String> {
    parse_float_literal(s).map(|v| {
        // Keep a decimal form valid in C.
        let mut out = v.to_string();
        if !out.contains('.') && !out.contains('e') && !out.contains('E') {
            out.push_str(".0");
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_signed_minimum_has_no_unrepresentable_positive_token() {
        assert_eq!(
            int_literal_c_source("-9223372036854775808").as_deref(),
            Some("(-9223372036854775807LL - 1LL)")
        );
        assert_eq!(
            int_literal_c_source("-0x8000_0000_0000_0000").as_deref(),
            Some("(-9223372036854775807LL - 1LL)")
        );
        assert_eq!(
            int_literal_c_source("18446744073709551615").as_deref(),
            Some("18446744073709551615ULL")
        );
    }

    #[test]
    fn test_literal_deduplication() {
        let mut pool = AmirLiteralPool::default();
        let lit1 = pool.intern_int("42");
        let lit2 = pool.intern_int("42");
        let lit3 = pool.intern_int("100");

        assert_eq!(lit1, lit2);
        assert_ne!(lit1, lit3);
        assert_eq!(pool.entries.len(), 2);
    }

    #[test]
    fn intern_str_dedups() {
        let mut pool = AmirLiteralPool::default();
        let a = pool.intern_str("hello");
        let b = pool.intern_str("hello");
        let c = pool.intern_str("world");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn parse_int_accepts_underscores_and_bases() {
        assert_eq!(parse_int_literal("1_000"), Some(1000));
        assert_eq!(parse_int_literal("0xFF"), Some(255));
        assert_eq!(parse_int_literal("0b1010_0001"), Some(0b1010_0001));
        assert_eq!(parse_int_literal("0o77"), Some(63));
        assert_eq!(parse_int_literal("-1_024"), Some(-1024));
        assert!(parse_int_literal("not_an_int").is_none());
        assert!(parse_int_literal("0x").is_none());
    }

    #[test]
    fn parse_float_accepts_underscores() {
        // Use a non-approx value so clippy::approx_constant does not fire.
        assert_eq!(parse_float_literal("2.5"), Some(2.5));
        assert!((parse_float_literal("1_000.5").unwrap() - 1000.5).abs() < f64::EPSILON);
    }
}
