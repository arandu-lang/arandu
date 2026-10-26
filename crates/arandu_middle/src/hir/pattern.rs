//! HIR pattern types (B7): stable IR decoupled from parser `Pattern`.

#[cfg(test)]
mod tests {
    use super::*;
    const S: Span = Span::new(0, 0, 0);

    #[test]
    fn wildcard_span() {
        let p = HirPattern::Wildcard { span: S };
        assert_eq!(p.span(), S);
    }

    #[test]
    fn bind_span() {
        let p = HirPattern::Bind {
            span: S,
            name: "x".into(),
            symbol: SymbolId::new(0, 1),
        };
        assert_eq!(p.span(), S);
    }

    #[test]
    fn literal_span() {
        let p = HirPattern::Literal {
            span: S,
            expr: super::super::pool::HirExprId::from_usize(0),
        };
        assert_eq!(p.span(), S);
    }

    #[test]
    fn enum_span() {
        let p = HirPattern::Enum {
            span: S,
            type_symbol: SymbolId::new(0, 0),
            variant: "V".into(),
            variant_symbol: Some(SymbolId::new(0, 1)),
            payload: super::super::pool::IndexRange::empty(),
        };
        assert_eq!(p.span(), S);
    }

    #[test]
    fn struct_span() {
        let p = HirPattern::Struct {
            span: S,
            struct_symbol: SymbolId::new(0, 0),
            fields: super::super::pool::IndexRange::empty(),
        };
        assert_eq!(p.span(), S);
    }

    #[test]
    fn field_pattern_basic() {
        let fp = HirFieldPattern {
            span: S,
            name: "f".into(),
            pattern: None,
        };
        assert_eq!(fp.name, "f");
        assert!(fp.pattern.is_none());
    }
}

use crate::Span;
use crate::SymbolId;
use smol_str::SmolStr;

#[derive(Debug, Clone)]
pub enum HirPattern {
    Wildcard {
        span: Span,
    },
    Bind {
        span: Span,
        name: SmolStr,
        symbol: SymbolId,
    },
    Literal {
        span: Span,
        expr: super::pool::HirExprId,
    },
    Enum {
        span: Span,
        type_symbol: SymbolId,
        variant: SmolStr,
        variant_symbol: Option<SymbolId>,
        payload: super::pool::IndexRange,
    },
    TypeTuple {
        span: Span,
        name: SmolStr,
        payload: super::pool::IndexRange,
    },
    Struct {
        span: Span,
        struct_symbol: SymbolId,
        fields: super::pool::IndexRange,
    },
    Tuple {
        span: Span,
        items: super::pool::IndexRange,
    },
    Range {
        span: Span,
        start: super::pool::HirExprId,
        inclusive: bool,
        end: super::pool::HirExprId,
    },
    /// SYN.4: ordered alternatives `p1 | p2 | …`.
    Or {
        span: Span,
        alts: super::pool::IndexRange,
    },
}

#[derive(Debug, Clone)]
pub struct HirFieldPattern {
    pub span: Span,
    pub name: SmolStr,
    pub pattern: Option<super::pool::HirPatternId>,
}

impl HirPattern {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Wildcard { span }
            | Self::Bind { span, .. }
            | Self::Literal { span, .. }
            | Self::Enum { span, .. }
            | Self::TypeTuple { span, .. }
            | Self::Struct { span, .. }
            | Self::Tuple { span, .. }
            | Self::Range { span, .. }
            | Self::Or { span, .. } => *span,
        }
    }
}
