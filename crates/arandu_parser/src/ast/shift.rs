//! Composition of item-local ASTs into one file AST (RFC 0042).
//!
//! An item AST is lowered on its own, with arena IDs from zero and spans
//! relative to the item start. Appending it to a file AST moves every ID by
//! the arena lengths before it and every span by the item offset, so the item
//! keeps a fixed correspondence with its slice of the file arena: the file ID
//! of a node is its local ID plus [`ItemBases`], and its span is the local
//! span plus the offset.
//!
//! Every destructuring below is exhaustive: a new field or variant that holds
//! an ID or a span must be shifted here, or the crate stops compiling.

use super::*;
use arandu_lexer::Span;

/// Arena lengths of a file AST before one item was appended to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ItemBases {
    pub offset: u32,
    pub exprs: u32,
    pub stmts: u32,
    pub type_exprs: u32,
    pub blocks: u32,
    pub patterns: u32,
    pub field_patterns: u32,
    pub decls: u32,
    pub field_inits: u32,
    pub lambda_params: u32,
    pub string_parts: u32,
    pub match_arms: u32,
    pub catch_handlers: u32,
    pub expr_ids: u32,
    pub stmt_ids: u32,
    pub type_expr_ids: u32,
    pub field_init_ids: u32,
    pub lambda_param_ids: u32,
    pub string_part_ids: u32,
    pub match_arm_ids: u32,
    pub pattern_ids: u32,
    pub field_pattern_ids: u32,
}

impl ItemBases {
    fn of(pool: &AstPool, offset: u32) -> Self {
        let len = |n: usize| n as u32;
        Self {
            offset,
            exprs: len(pool.exprs.len()),
            stmts: len(pool.stmts.len()),
            type_exprs: len(pool.type_exprs.len()),
            blocks: len(pool.blocks.len()),
            patterns: len(pool.patterns.len()),
            field_patterns: len(pool.field_patterns.len()),
            decls: len(pool.decls.len()),
            field_inits: len(pool.field_inits.len()),
            lambda_params: len(pool.lambda_params.len()),
            string_parts: len(pool.string_parts.len()),
            match_arms: len(pool.match_arms.len()),
            catch_handlers: len(pool.catch_handlers.len()),
            expr_ids: len(pool.expr_ids.len()),
            stmt_ids: len(pool.stmt_ids.len()),
            type_expr_ids: len(pool.type_expr_ids.len()),
            field_init_ids: len(pool.field_init_ids.len()),
            lambda_param_ids: len(pool.lambda_param_ids.len()),
            string_part_ids: len(pool.string_part_ids.len()),
            match_arm_ids: len(pool.match_arm_ids.len()),
            pattern_ids: len(pool.pattern_ids.len()),
            field_pattern_ids: len(pool.field_pattern_ids.len()),
        }
    }

    /// File position of an item-local span. Synthetic nodes carry the
    /// `Span::new(0, 0, 0)` placeholder, which is not a position.
    #[must_use]
    pub fn span(&self, span: Span) -> Span {
        if span == Span::new(0, 0, 0) {
            return span;
        }
        Span::new(
            span.file_id,
            span.start + self.offset,
            span.end + self.offset,
        )
    }

    /// File arena ID of an item-local expression.
    #[must_use]
    pub fn expr(&self, id: ExprId) -> ExprId {
        ExprId::new(id.as_usize() + self.exprs as usize)
    }
}

macro_rules! shift_id {
    ($name:ident, $ty:ident, $field:ident) => {
        fn $name(&self, id: &mut $ty) {
            *id = $ty::new(id.as_usize() + self.0.$field as usize);
        }
    };
}

macro_rules! shift_range {
    ($name:ident, $field:ident) => {
        fn $name(&self, range: &mut IndexRange) {
            // Empty ranges are canonical (`IndexRange::empty()`).
            if !range.is_empty() {
                range.start += self.0.$field;
            }
        }
    };
}

struct Shift(ItemBases);

impl Shift {
    fn span(&self, span: &mut Span) {
        *span = self.0.span(*span);
    }

    shift_id!(expr_id, ExprId, exprs);
    shift_id!(stmt_id, StmtId, stmts);
    shift_id!(type_expr_id, TypeExprId, type_exprs);
    shift_id!(block_id, BlockId, blocks);
    shift_id!(pattern_id, PatternId, patterns);
    shift_id!(field_pattern_id, FieldPatternId, field_patterns);
    shift_id!(field_init_id, FieldInitId, field_inits);
    shift_id!(lambda_param_id, LambdaParamId, lambda_params);
    shift_id!(string_part_id, StringPartId, string_parts);
    shift_id!(match_arm_id, MatchArmId, match_arms);
    shift_id!(catch_handler_id, CatchHandlerId, catch_handlers);
    shift_id!(decl_id, DeclId, decls);

    shift_range!(expr_range, expr_ids);
    shift_range!(stmt_range, stmt_ids);
    shift_range!(type_expr_range, type_expr_ids);
    shift_range!(field_init_range, field_init_ids);
    shift_range!(lambda_param_range, lambda_param_ids);
    shift_range!(string_part_range, string_part_ids);
    shift_range!(match_arm_range, match_arm_ids);
    shift_range!(pattern_range, pattern_ids);
    shift_range!(field_pattern_range, field_pattern_ids);

    fn opt_expr(&self, id: &mut Option<ExprId>) {
        if let Some(id) = id {
            self.expr_id(id);
        }
    }

    fn opt_type_expr(&self, id: &mut Option<TypeExprId>) {
        if let Some(id) = id {
            self.type_expr_id(id);
        }
    }

    fn type_name(&self, name: &mut TypeName) {
        let TypeName { span, path: _ } = name;
        self.span(span);
    }

    fn block(&self, block: &mut Block) {
        let Block { span, statements } = block;
        self.span(span);
        self.stmt_range(statements);
    }

    fn expr_kind(&self, kind: &mut ExprKind) {
        match kind {
            ExprKind::Layout { query: _, ty } => self.opt_type_expr(ty),
            ExprKind::Path { path: _ }
            | ExprKind::Error
            | ExprKind::Int { value: _ }
            | ExprKind::Float { value: _ }
            | ExprKind::Bool { value: _ }
            | ExprKind::Char { value: _ }
            | ExprKind::Byte { value: _ }
            | ExprKind::ByteString { bytes: _ }
            | ExprKind::Nil => {}
            ExprKind::TypePath {
                type_name,
                member: _,
            } => self.type_name(type_name),
            ExprKind::VariantSugar { name: _, args } => self.expr_range(args),
            ExprKind::Generic { callee, args } => {
                self.expr_id(callee);
                self.type_expr_range(args);
            }
            ExprKind::Field { base, field: _ } | ExprKind::SafeField { base, field: _ } => {
                self.expr_id(base);
            }
            ExprKind::Index { base, index } | ExprKind::SafeIndex { base, index } => {
                self.expr_id(base);
                self.expr_id(index);
            }
            ExprKind::Try { expr }
            | ExprKind::Alloc { expr }
            | ExprKind::Group { expr }
            | ExprKind::Unary { op: _, expr } => self.expr_id(expr),
            ExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                self.expr_id(callee);
                self.expr_range(args);
                if let Some(block) = trailing_block {
                    self.block_id(block);
                }
            }
            ExprKind::StructLiteral { ty, fields } => {
                self.type_expr_id(ty);
                self.field_init_range(fields);
            }
            ExprKind::Array { items } => self.expr_range(items),
            ExprKind::ArrayRepeat { value, count } => {
                self.expr_id(value);
                self.type_expr_id(count);
            }
            ExprKind::Lambda {
                params,
                body,
                is_move: _,
            } => {
                self.lambda_param_range(params);
                match body {
                    LambdaBody::Expr { span, expr } => {
                        self.span(span);
                        self.expr_id(expr);
                    }
                    LambdaBody::Block { span, block } => {
                        self.span(span);
                        self.block(block);
                    }
                }
            }
            ExprKind::AsyncBlock { block } | ExprKind::UnsafeBlock { block } => {
                self.block_id(block);
            }
            ExprKind::Comptime { body } => match body {
                ComptimeBody::Expression(expr) => self.expr_id(expr),
                ComptimeBody::Block(block) => self.block_id(block),
            },
            ExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.condition(condition);
                self.block_id(then_block);
                self.block_id(else_block);
            }
            ExprKind::Match { value, arms } => {
                self.expr_id(value);
                self.match_arm_range(arms);
            }
            ExprKind::Catch { expr, handler } => {
                self.expr_id(expr);
                self.catch_handler_id(handler);
            }
            ExprKind::NullCoalesce { left, right } | ExprKind::Binary { op: _, left, right } => {
                self.expr_id(left);
                self.expr_id(right);
            }
            ExprKind::Cast { expr, ty } => {
                self.expr_id(expr);
                self.type_expr_id(ty);
            }
            ExprKind::InterpolatedString { parts } => self.string_part_range(parts),
        }
    }

    fn condition(&self, condition: &mut Condition) {
        match condition {
            Condition::Expr { span, expr } => {
                self.span(span);
                self.expr_id(expr);
            }
            Condition::Is {
                span,
                expr,
                pattern,
            } => {
                self.span(span);
                self.expr_id(expr);
                self.pattern_id(pattern);
            }
            Condition::And { span, conditions } => {
                self.span(span);
                for condition in conditions.iter_mut() {
                    self.condition(condition);
                }
            }
        }
    }

    fn bindings(&self, bindings: &mut [BindingItem]) {
        for BindingItem {
            span,
            mutable: _,
            name: _,
            ty,
        } in bindings
        {
            self.span(span);
            self.opt_type_expr(ty);
        }
    }

    fn places(&self, places: &mut [Place]) {
        for Place {
            span,
            root: _,
            suffixes,
        } in places
        {
            self.span(span);
            for suffix in suffixes {
                match suffix {
                    PlaceSuffix::Field { span, name: _ } => self.span(span),
                    PlaceSuffix::Index { span, expr } => {
                        self.span(span);
                        self.expr_id(expr);
                    }
                }
            }
        }
    }

    fn simple_stmt(&self, stmt: &mut SimpleStmt) {
        match stmt {
            SimpleStmt::VarDecl {
                span,
                bindings,
                value,
            } => {
                self.span(span);
                self.bindings(bindings);
                self.expr_id(value);
            }
            SimpleStmt::Set {
                span,
                places,
                op: _,
                value,
            } => {
                self.span(span);
                self.places(places);
                self.expr_id(value);
            }
            SimpleStmt::Expr { span, expr } => {
                self.span(span);
                self.expr_id(expr);
            }
        }
    }

    fn defer_body(&self, body: &mut DeferBody) {
        match body {
            DeferBody::Expr { span, expr } => {
                self.span(span);
                self.expr_id(expr);
            }
            DeferBody::Block { span, block } => {
                self.span(span);
                self.block(block);
            }
        }
    }

    fn stmt(&self, stmt: &mut Stmt) {
        match stmt {
            Stmt::VarDecl {
                span,
                bindings,
                value,
            } => {
                self.span(span);
                self.bindings(bindings);
                self.expr_id(value);
            }
            Stmt::Set {
                span,
                places,
                op: _,
                value,
            } => {
                self.span(span);
                self.places(places);
                self.expr_id(value);
            }
            Stmt::Return { span, values } => {
                self.span(span);
                for value in values {
                    self.expr_id(value);
                }
            }
            Stmt::Break { span } | Stmt::Continue { span } | Stmt::Error(span) => self.span(span),
            Stmt::Free { span, expr }
            | Stmt::Match { span, expr }
            | Stmt::Expr {
                span,
                expr,
                has_semi: _,
            } => {
                self.span(span);
                self.expr_id(expr);
            }
            Stmt::If {
                span,
                is_comptime: _,
                condition,
                then_block,
                else_block,
            } => {
                self.span(span);
                self.condition(condition);
                self.block(then_block);
                if let Some(block) = else_block {
                    self.block(block);
                }
            }
            Stmt::For {
                span,
                is_comptime: _,
                clause,
                body,
            } => {
                self.span(span);
                match clause {
                    ForClause::In {
                        span,
                        bindings,
                        iterable,
                    } => {
                        self.span(span);
                        for ForBinding {
                            span,
                            mutable: _,
                            name: _,
                        } in bindings
                        {
                            self.span(span);
                        }
                        self.expr_id(iterable);
                    }
                    ForClause::CStyle {
                        span,
                        init,
                        condition,
                        step,
                    } => {
                        self.span(span);
                        if let Some(init) = init {
                            self.simple_stmt(init);
                        }
                        self.opt_expr(condition);
                        if let Some(step) = step {
                            self.simple_stmt(step);
                        }
                    }
                }
                self.block(body);
            }
            Stmt::While {
                span,
                condition,
                body,
            } => {
                self.span(span);
                self.condition(condition);
                self.block(body);
            }
            Stmt::Defer { span, body } | Stmt::ErrDefer { span, body } => {
                self.span(span);
                self.defer_body(body);
            }
            Stmt::Unsafe { span, block } => {
                self.span(span);
                self.block(block);
            }
        }
    }

    fn result_type(&self, result: &mut ResultType) {
        match result {
            ResultType::Single { span, ty } => {
                self.span(span);
                self.type_expr_id(ty);
            }
            ResultType::Multi { span, types } => {
                self.span(span);
                self.type_expr_range(types);
            }
        }
    }

    fn type_expr(&self, ty: &mut TypeExpr) {
        match ty {
            TypeExpr::Const { span, value: _ } | TypeExpr::Primitive { span, name: _ } => {
                self.span(span);
            }
            TypeExpr::ConstExpression { span, expression } => {
                self.span(span);
                self.expr_id(expression);
            }
            TypeExpr::Named { span, name, args } => {
                self.span(span);
                self.type_name(name);
                self.type_expr_range(args);
            }
            TypeExpr::Nullable { span, inner }
            | TypeExpr::Pointer { span, inner }
            | TypeExpr::Ref { span, inner }
            | TypeExpr::RefMut { span, inner }
            | TypeExpr::Slice { span, inner }
            | TypeExpr::MutSlice { span, inner }
            | TypeExpr::Group { span, inner } => {
                self.span(span);
                self.type_expr_id(inner);
            }
            TypeExpr::Array {
                span,
                size: _,
                size_span,
                size_expression,
                elem,
            } => {
                self.span(span);
                self.span(size_span);
                self.opt_type_expr(size_expression);
                self.type_expr_id(elem);
            }
            TypeExpr::Func {
                span,
                params,
                result,
            } => {
                self.span(span);
                self.type_expr_range(params);
                if let Some(result) = result {
                    self.result_type(result);
                }
            }
        }
    }

    fn pattern(&self, pattern: &mut Pattern) {
        match pattern {
            Pattern::Wildcard { span }
            | Pattern::Bind {
                span,
                name: _,
                mutable: _,
            } => self.span(span),
            Pattern::Literal { span, expr } => {
                self.span(span);
                self.expr_id(expr);
            }
            Pattern::Enum {
                span,
                type_name,
                variant: _,
                payload,
            } => {
                self.span(span);
                self.type_name(type_name);
                self.pattern_range(payload);
            }
            Pattern::TypeTuple {
                span,
                name: _,
                payload,
            } => {
                self.span(span);
                self.pattern_range(payload);
            }
            Pattern::Struct {
                span,
                type_name,
                fields,
            } => {
                self.span(span);
                self.type_name(type_name);
                self.field_pattern_range(fields);
            }
            Pattern::Tuple { span, items } => {
                self.span(span);
                self.pattern_range(items);
            }
            Pattern::Range {
                span,
                start,
                inclusive: _,
                end,
            } => {
                self.span(span);
                self.expr_id(start);
                self.expr_id(end);
            }
            Pattern::Or { span, alts } => {
                self.span(span);
                self.pattern_range(alts);
            }
        }
    }

    fn attributes(&self, attrs: &mut [Attribute]) {
        for Attribute {
            span,
            name_span,
            name: _,
            args,
        } in attrs
        {
            self.span(span);
            self.span(name_span);
            for arg in args {
                self.expr_id(arg);
            }
        }
    }

    fn generics(&self, params: &mut [GenericParam], where_clause: &mut [WhereItem]) {
        for GenericParam {
            span,
            name: _,
            const_ty,
            constraints,
            default,
        } in params
        {
            self.span(span);
            self.opt_type_expr(const_ty);
            for constraint in constraints {
                self.type_expr_id(constraint);
            }
            self.opt_type_expr(default);
        }
        for WhereItem {
            span,
            name: _,
            constraints,
        } in where_clause
        {
            self.span(span);
            for constraint in constraints {
                self.type_expr_id(constraint);
            }
        }
    }

    fn params(&self, params: &mut [Param]) {
        for Param {
            span,
            attrs,
            ownership: _,
            name: _,
            ty,
            is_variadic: _,
            is_receiver: _,
        } in params
        {
            self.span(span);
            self.attributes(attrs);
            self.type_expr_id(ty);
        }
    }

    fn signature(&self, signature: &mut FuncSignature) {
        let FuncSignature {
            span,
            attrs,
            name: _,
            generic_params,
            params,
            result,
            where_clause,
        } = signature;
        self.span(span);
        self.attributes(attrs);
        self.generics(generic_params, where_clause);
        self.params(params);
        if let Some(result) = result {
            self.result_type(result);
        }
    }

    fn fields(&self, fields: &mut [FieldDecl]) {
        for FieldDecl {
            span,
            attrs,
            visibility: _,
            name: _,
            ty,
            default,
        } in fields
        {
            self.span(span);
            self.attributes(attrs);
            self.type_expr_id(ty);
            self.opt_expr(default);
        }
    }

    fn decl(&self, decl: &mut TopLevelDecl) {
        match decl {
            TopLevelDecl::Const(ConstDecl {
                span,
                attrs,
                visibility: _,
                name: _,
                ty,
                value,
            }) => {
                self.span(span);
                self.attributes(attrs);
                self.opt_type_expr(ty);
                self.expr_id(value);
            }
            TopLevelDecl::TypeAlias(TypeAliasDecl {
                span,
                attrs,
                visibility: _,
                name: _,
                generic_params,
                ty,
            }) => {
                self.span(span);
                self.attributes(attrs);
                self.generics(generic_params, &mut []);
                self.type_expr_id(ty);
            }
            TopLevelDecl::Func(FuncDecl {
                span,
                attrs,
                visibility: _,
                is_async: _,
                name,
                generic_params,
                params,
                result,
                where_clause,
                body,
            }) => {
                self.span(span);
                self.attributes(attrs);
                match name {
                    FuncName::Free { span, name: _ } => self.span(span),
                    FuncName::Method {
                        span,
                        receiver,
                        name: _,
                    } => {
                        self.span(span);
                        self.type_name(receiver);
                    }
                }
                self.generics(generic_params, where_clause);
                self.params(params);
                if let Some(result) = result {
                    self.result_type(result);
                }
                self.block(body);
            }
            TopLevelDecl::Struct(StructDecl {
                span,
                attrs,
                visibility: _,
                name: _,
                generic_params,
                where_clause,
                fields,
            }) => {
                self.span(span);
                self.attributes(attrs);
                self.generics(generic_params, where_clause);
                self.fields(fields);
            }
            TopLevelDecl::Enum(EnumDecl {
                span,
                attrs,
                visibility: _,
                name: _,
                generic_params,
                where_clause,
                variants,
            }) => {
                self.span(span);
                self.attributes(attrs);
                self.generics(generic_params, where_clause);
                for EnumVariant {
                    span,
                    attrs,
                    name: _,
                    payload,
                } in variants
                {
                    self.span(span);
                    self.attributes(attrs);
                    match payload {
                        Some(EnumPayload::Tuple { span, types }) => {
                            self.span(span);
                            self.type_expr_range(types);
                        }
                        Some(EnumPayload::Struct { span, fields }) => {
                            self.span(span);
                            self.fields(fields);
                        }
                        None => {}
                    }
                }
            }
            TopLevelDecl::Interface(InterfaceDecl {
                span,
                attrs,
                visibility: _,
                sealed: _,
                name: _,
                generic_params,
                where_clause,
                members,
            }) => {
                self.span(span);
                self.attributes(attrs);
                self.generics(generic_params, where_clause);
                for member in members {
                    self.signature(member);
                }
            }
            TopLevelDecl::Extern(ExternDecl {
                span,
                attrs,
                abi: _,
                members,
            }) => {
                self.span(span);
                self.attributes(attrs);
                for member in members {
                    self.signature(member);
                }
            }
            TopLevelDecl::Submodule(SubmoduleDecl {
                span,
                attrs,
                visibility: _,
                name: _,
                decls,
            }) => {
                self.span(span);
                self.attributes(attrs);
                for decl in decls {
                    self.decl_id(decl);
                }
            }
            TopLevelDecl::Error(span) => self.span(span),
        }
    }

    fn import(&self, import: &mut ImportDecl) {
        let items = match import {
            ImportDecl::ModuleAlias {
                span,
                path: _,
                alias: _,
            }
            | ImportDecl::ExternalAlias {
                span,
                source: _,
                alias: _,
            } => {
                self.span(span);
                return;
            }
            ImportDecl::Named {
                span,
                items,
                path: _,
            }
            | ImportDecl::ReExport {
                span,
                visibility: _,
                items,
                path: _,
            }
            | ImportDecl::ExternalNamed {
                span,
                items,
                source: _,
            } => {
                self.span(span);
                items
            }
        };
        for ImportItem {
            span,
            name: _,
            alias: _,
        } in items
        {
            self.span(span);
        }
    }

    fn pool(&self, pool: &mut AstPool) {
        let AstPool {
            exprs,
            expr_spans,
            stmts,
            stmt_spans,
            type_exprs,
            type_expr_spans,
            blocks,
            patterns,
            field_patterns,
            decls,
            decl_spans,
            field_inits,
            lambda_params,
            string_parts,
            match_arms,
            catch_handlers,
            expr_ids,
            stmt_ids,
            type_expr_ids,
            field_init_ids,
            lambda_param_ids,
            string_part_ids,
            match_arm_ids,
            pattern_ids,
            field_pattern_ids,
        } = pool;
        exprs.iter_mut().for_each(|kind| self.expr_kind(kind));
        stmts.iter_mut().for_each(|stmt| self.stmt(stmt));
        type_exprs.iter_mut().for_each(|ty| self.type_expr(ty));
        blocks.iter_mut().for_each(|block| self.block(block));
        patterns
            .iter_mut()
            .for_each(|pattern| self.pattern(pattern));
        decls.iter_mut().for_each(|decl| self.decl(decl));
        for span in expr_spans
            .iter_mut()
            .chain(stmt_spans.iter_mut())
            .chain(type_expr_spans.iter_mut())
            .chain(decl_spans.iter_mut())
        {
            self.span(span);
        }
        for FieldPattern {
            span,
            name: _,
            pattern,
        } in field_patterns
        {
            self.span(span);
            if let Some(pattern) = pattern {
                self.pattern_id(pattern);
            }
        }
        for FieldInit {
            span,
            name: _,
            value,
        } in field_inits
        {
            self.span(span);
            self.expr_id(value);
        }
        for LambdaParam { span, name: _, ty } in lambda_params {
            self.span(span);
            self.opt_type_expr(ty);
        }
        for part in string_parts {
            match part {
                StringPart::Text { span, text: _ } => self.span(span),
                StringPart::Expr { span, expr } => {
                    self.span(span);
                    self.expr_id(expr);
                }
            }
        }
        for MatchArm {
            span,
            pattern,
            guard,
            body,
        } in match_arms
        {
            self.span(span);
            self.pattern_id(pattern);
            self.opt_expr(guard);
            match body {
                MatchArmBody::Expr { span, expr } => {
                    self.span(span);
                    self.expr_id(expr);
                }
                MatchArmBody::Block { span, block } => {
                    self.span(span);
                    self.block(block);
                }
            }
        }
        for handler in catch_handlers {
            match handler {
                CatchHandler::Expr { span, expr } => {
                    self.span(span);
                    self.expr_id(expr);
                }
                CatchHandler::Block {
                    span,
                    error: _,
                    block,
                } => {
                    self.span(span);
                    self.block(block);
                }
            }
        }
        expr_ids.iter_mut().for_each(|id| self.expr_id(id));
        stmt_ids.iter_mut().for_each(|id| self.stmt_id(id));
        type_expr_ids
            .iter_mut()
            .for_each(|id| self.type_expr_id(id));
        field_init_ids
            .iter_mut()
            .for_each(|id| self.field_init_id(id));
        lambda_param_ids
            .iter_mut()
            .for_each(|id| self.lambda_param_id(id));
        string_part_ids
            .iter_mut()
            .for_each(|id| self.string_part_id(id));
        match_arm_ids
            .iter_mut()
            .for_each(|id| self.match_arm_id(id));
        pattern_ids.iter_mut().for_each(|id| self.pattern_id(id));
        field_pattern_ids
            .iter_mut()
            .for_each(|id| self.field_pattern_id(id));
    }
}

fn append<T>(dest: &mut Vec<T>, source: Vec<T>) {
    dest.extend(source);
}

impl Program {
    /// Appends an item-local AST lowered at `offset` in this file, returning
    /// the bases that map the item's IDs into this AST.
    pub fn append_item(&mut self, item: &Program, offset: u32) -> ItemBases {
        let bases = ItemBases::of(&self.pool, offset);
        let shift = Shift(bases);
        let Program {
            span: _,
            module,
            imports,
            interface_impls,
            decls,
            docs,
            pool,
        } = item.clone();
        let mut pool = pool;
        shift.pool(&mut pool);
        if let Some(mut module) = module {
            let ModuleDecl { span, path: _ } = &mut module;
            shift.span(span);
            self.module = Some(module);
        }
        for mut import in imports {
            shift.import(&mut import);
            self.imports.push(import);
        }
        for mut implementation in interface_impls {
            let InterfaceImplDecl {
                span,
                for_type,
                interface,
            } = &mut implementation;
            shift.span(span);
            shift.type_name(for_type);
            shift.type_name(interface);
            self.interface_impls.push(implementation);
        }
        for mut decl in decls {
            shift.decl_id(&mut decl);
            self.decls.push(decl);
        }
        for mut doc in docs {
            let DocCommentAttachment {
                span,
                text: _,
                target_span,
            } = &mut doc;
            shift.span(span);
            shift.span(target_span);
            self.docs.push(doc);
        }
        let AstPool {
            exprs,
            expr_spans,
            stmts,
            stmt_spans,
            type_exprs,
            type_expr_spans,
            blocks,
            patterns,
            field_patterns,
            decls,
            decl_spans,
            field_inits,
            lambda_params,
            string_parts,
            match_arms,
            catch_handlers,
            expr_ids,
            stmt_ids,
            type_expr_ids,
            field_init_ids,
            lambda_param_ids,
            string_part_ids,
            match_arm_ids,
            pattern_ids,
            field_pattern_ids,
        } = pool;
        let dest = &mut self.pool;
        append(&mut dest.exprs, exprs);
        append(&mut dest.expr_spans, expr_spans);
        append(&mut dest.stmts, stmts);
        append(&mut dest.stmt_spans, stmt_spans);
        append(&mut dest.type_exprs, type_exprs);
        append(&mut dest.type_expr_spans, type_expr_spans);
        append(&mut dest.blocks, blocks);
        append(&mut dest.patterns, patterns);
        append(&mut dest.field_patterns, field_patterns);
        append(&mut dest.decls, decls);
        append(&mut dest.decl_spans, decl_spans);
        append(&mut dest.field_inits, field_inits);
        append(&mut dest.lambda_params, lambda_params);
        append(&mut dest.string_parts, string_parts);
        append(&mut dest.match_arms, match_arms);
        append(&mut dest.catch_handlers, catch_handlers);
        append(&mut dest.expr_ids, expr_ids);
        append(&mut dest.stmt_ids, stmt_ids);
        append(&mut dest.type_expr_ids, type_expr_ids);
        append(&mut dest.field_init_ids, field_init_ids);
        append(&mut dest.lambda_param_ids, lambda_param_ids);
        append(&mut dest.string_part_ids, string_part_ids);
        append(&mut dest.match_arm_ids, match_arm_ids);
        append(&mut dest.pattern_ids, pattern_ids);
        append(&mut dest.field_pattern_ids, field_pattern_ids);
        bases
    }
}
