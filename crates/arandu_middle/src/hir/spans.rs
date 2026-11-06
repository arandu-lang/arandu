//! Every source position a HIR program carries, for moving it between item
//! and file coordinates (RFC 0042). Every struct is destructured and every
//! enum matched without a wildcard, so a new field or variant that carries a
//! span does not compile until it is visited here.

use super::{
    HirBindingItem, HirBlock, HirConst, HirDecl, HirEnum, HirEnumVariant, HirExpr, HirExtern,
    HirFieldInit, HirFieldPattern, HirForBinding, HirForClause, HirFunc, HirFuncSignature,
    HirInterface, HirLambdaParam, HirMatchArm, HirParam, HirPattern, HirPlace, HirPlaceSuffix,
    HirPool, HirProgram, HirStmt, HirStmtKind, HirStruct, HirStructField, HirTypeAlias,
};
use crate::Span;

impl HirProgram {
    /// Replaces every span with `f(span)`.
    pub fn map_spans(&mut self, f: &mut impl FnMut(Span) -> Span) {
        let HirProgram {
            span,
            module: _,
            decls: _,
            pool,
        } = self;
        *span = f(*span);
        pool.map_spans(f);
    }
}

impl HirPool {
    /// Replaces every span with `f(span)`.
    pub fn map_spans(&mut self, f: &mut impl FnMut(Span) -> Span) {
        let HirPool {
            ctfe_lowering: _,
            exprs,
            stmts,
            blocks,
            decls,
            patterns,
            field_patterns,
            params,
            struct_fields,
            enum_variants,
            func_signatures,
            bindings,
            places,
            for_bindings,
            match_arms,
            field_inits,
            lambda_params,
            expr_ids: _,
            stmt_ids: _,
            pattern_ids: _,
            field_pattern_ids: _,
        } = self;
        for HirExpr {
            kind: _,
            ty: _,
            span,
        } in exprs.iter_mut()
        {
            *span = f(*span);
        }
        for HirStmt { kind, span } in stmts.iter_mut() {
            *span = f(*span);
            if let HirStmtKind::For { clause, .. } = kind {
                match clause {
                    HirForClause::In { span, .. } | HirForClause::CStyle { span, .. } => {
                        *span = f(*span);
                    }
                }
            }
        }
        for HirBlock {
            statements: _,
            span,
        } in blocks.iter_mut()
        {
            *span = f(*span);
        }
        for decl in decls.iter_mut() {
            let span = match decl {
                HirDecl::Const(HirConst { span, .. })
                | HirDecl::TypeAlias(HirTypeAlias { span, .. })
                | HirDecl::Func(HirFunc { span, .. })
                | HirDecl::Struct(HirStruct { span, .. })
                | HirDecl::Enum(HirEnum { span, .. })
                | HirDecl::Interface(HirInterface { span, .. })
                | HirDecl::Extern(HirExtern { span, .. }) => span,
            };
            *span = f(*span);
        }
        for pattern in patterns.iter_mut() {
            let span = match pattern {
                HirPattern::Wildcard { span }
                | HirPattern::Bind { span, .. }
                | HirPattern::Literal { span, .. }
                | HirPattern::Enum { span, .. }
                | HirPattern::TypeTuple { span, .. }
                | HirPattern::Struct { span, .. }
                | HirPattern::Tuple { span, .. }
                | HirPattern::Range { span, .. }
                | HirPattern::Or { span, .. } => span,
            };
            *span = f(*span);
        }
        for HirFieldPattern {
            span,
            name: _,
            pattern: _,
        } in field_patterns.iter_mut()
        {
            *span = f(*span);
        }
        for HirParam {
            symbol: _,
            ty: _,
            span,
            is_receiver: _,
            receiver_kind: _,
        } in params.iter_mut()
        {
            *span = f(*span);
        }
        for HirStructField {
            symbol: _,
            ty: _,
            span,
        } in struct_fields.iter_mut()
        {
            *span = f(*span);
        }
        for HirEnumVariant {
            symbol: _,
            payload: _,
            span,
        } in enum_variants.iter_mut()
        {
            *span = f(*span);
        }
        for HirFuncSignature {
            symbol: _,
            params: _,
            return_type: _,
            span,
        } in func_signatures.iter_mut()
        {
            *span = f(*span);
        }
        for HirBindingItem {
            symbol: _,
            ty: _,
            span,
        } in bindings.iter_mut()
        {
            *span = f(*span);
        }
        for HirPlace {
            root_symbol: _,
            suffixes,
            ty: _,
            span,
        } in places.iter_mut()
        {
            *span = f(*span);
            for suffix in suffixes.iter_mut() {
                match suffix {
                    HirPlaceSuffix::Field { span, .. } | HirPlaceSuffix::Index { span, .. } => {
                        *span = f(*span);
                    }
                }
            }
        }
        for HirForBinding {
            symbol: _,
            ty: _,
            span,
        } in for_bindings.iter_mut()
        {
            *span = f(*span);
        }
        for HirMatchArm {
            span,
            pattern: _,
            guard: _,
            body: _,
        } in match_arms.iter_mut()
        {
            *span = f(*span);
        }
        for HirFieldInit {
            span,
            name: _,
            value: _,
        } in field_inits.iter_mut()
        {
            *span = f(*span);
        }
        for HirLambdaParam {
            span,
            symbol: _,
            ty: _,
        } in lambda_params.iter_mut()
        {
            *span = f(*span);
        }
    }
}
