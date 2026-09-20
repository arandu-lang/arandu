//! Multi-file HIR linking: append one module's HIR pool into another with ID
//! and TypeId remapping so imported function bodies participate in mono/codegen.

use arandu_middle::hir::{
    HirBlockId, HirCatchHandler, HirCondition, HirDecl, HirDeclId, HirExpr, HirExprId, HirExprKind,
    HirFieldPatternId, HirForClause, HirLambdaBody, HirMatchArm, HirMatchArmBody, HirPattern,
    HirPatternId, HirPlace, HirPlaceSuffix, HirProgram, HirSimpleStmt, HirStmt, HirStmtId,
    HirStmtKind, HirStringPart, IndexRange,
};
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_typeck::{TypeCheckResult, translate_type};
use rustc_hash::FxHashMap;

/// Offsets for every dense arena and list storage in [`HirPool`].
#[derive(Debug, Clone, Copy)]
struct PoolOffsets {
    expr: u32,
    stmt: u32,
    block: u32,
    decl: u32,
    pattern: u32,
    field_pattern: u32,
    params: u32,
    struct_fields: u32,
    enum_variants: u32,
    func_signatures: u32,
    bindings: u32,
    places: u32,
    for_bindings: u32,
    match_arms: u32,
    field_inits: u32,
    lambda_params: u32,
    expr_ids: u32,
    stmt_ids: u32,
    pattern_ids: u32,
    field_pattern_ids: u32,
}

impl PoolOffsets {
    fn capture(dest: &HirProgram) -> Self {
        let p = &dest.pool;
        Self {
            expr: p.exprs.len() as u32,
            stmt: p.stmts.len() as u32,
            block: p.blocks.len() as u32,
            decl: p.decls.len() as u32,
            pattern: p.patterns.len() as u32,
            field_pattern: p.field_patterns.len() as u32,
            params: p.params.len() as u32,
            struct_fields: p.struct_fields.len() as u32,
            enum_variants: p.enum_variants.len() as u32,
            func_signatures: p.func_signatures.len() as u32,
            bindings: p.bindings.len() as u32,
            places: p.places.len() as u32,
            for_bindings: p.for_bindings.len() as u32,
            match_arms: p.match_arms.len() as u32,
            field_inits: p.field_inits.len() as u32,
            lambda_params: p.lambda_params.len() as u32,
            expr_ids: p.expr_ids.len() as u32,
            stmt_ids: p.stmt_ids.len() as u32,
            pattern_ids: p.pattern_ids.len() as u32,
            field_pattern_ids: p.field_pattern_ids.len() as u32,
        }
    }

    fn range(&self, r: IndexRange, list_off: u32) -> IndexRange {
        if r.is_empty() {
            return IndexRange::empty();
        }
        IndexRange {
            start: r.start.saturating_add(list_off),
            len: r.len,
        }
    }

    fn expr_id(&self, id: HirExprId) -> HirExprId {
        HirExprId::from_usize(id.as_usize() + self.expr as usize)
    }

    fn stmt_id(&self, id: HirStmtId) -> HirStmtId {
        HirStmtId::from_usize(id.as_usize() + self.stmt as usize)
    }

    fn block_id(&self, id: HirBlockId) -> HirBlockId {
        HirBlockId::from_usize(id.as_usize() + self.block as usize)
    }

    fn decl_id(&self, id: HirDeclId) -> HirDeclId {
        HirDeclId::from_usize(id.as_usize() + self.decl as usize)
    }

    fn pattern_id(&self, id: HirPatternId) -> HirPatternId {
        HirPatternId::from_usize(id.as_usize() + self.pattern as usize)
    }

    fn field_pattern_id(&self, id: HirFieldPatternId) -> HirFieldPatternId {
        HirFieldPatternId::from_usize(id.as_usize() + self.field_pattern as usize)
    }
}

/// Re-intern a TypeId from `from` into `to`, with a small cache.
fn map_type(
    id: TypeId,
    from: &TypeInterner,
    to: &mut TypeInterner,
    cache: &mut FxHashMap<TypeId, TypeId>,
) -> TypeId {
    if let Some(&mapped) = cache.get(&id) {
        return mapped;
    }
    let ty = from.resolve(id);
    let translated = translate_type(&ty, from, to);
    let mapped = to.intern(translated);
    cache.insert(id, mapped);
    mapped
}

fn map_type_opt(
    id: Option<TypeId>,
    from: &TypeInterner,
    to: &mut TypeInterner,
    cache: &mut FxHashMap<TypeId, TypeId>,
) -> Option<TypeId> {
    id.map(|t| map_type(t, from, to, cache))
}

/// Append `src` HIR into `dest`, remapping every pool id and TypeId.
///
/// Also merges type maps and registers `src` symbols into `dest_tc` so AMIR
/// lowering can resolve locals/params from imported modules.
pub fn link_hir_module(
    dest_tc: &mut TypeCheckResult,
    dest: &mut HirProgram,
    src_tc: &TypeCheckResult,
    src: &HirProgram,
) {
    let offs = PoolOffsets::capture(dest);
    let mut ty_cache: FxHashMap<TypeId, TypeId> = FxHashMap::default();
    let from = &src_tc.type_info.type_interner;

    // --- list storages (values with nested ids / types) ---
    for p in &src.pool.params {
        let mut p = p.clone();
        p.ty = map_type(
            p.ty,
            from,
            &mut dest_tc.type_info_mut().type_interner,
            &mut ty_cache,
        );
        dest.pool.params.push(p);
    }
    for f in &src.pool.struct_fields {
        let mut f = f.clone();
        f.ty = map_type(
            f.ty,
            from,
            &mut dest_tc.type_info_mut().type_interner,
            &mut ty_cache,
        );
        dest.pool.struct_fields.push(f);
    }
    for v in &src.pool.enum_variants {
        let mut v = v.clone();
        v.payload = map_type_opt(
            v.payload,
            from,
            &mut dest_tc.type_info_mut().type_interner,
            &mut ty_cache,
        );
        dest.pool.enum_variants.push(v);
    }
    for s in &src.pool.func_signatures {
        let mut s = s.clone();
        s.params = offs.range(s.params, offs.params);
        s.return_type = map_type(
            s.return_type,
            from,
            &mut dest_tc.type_info_mut().type_interner,
            &mut ty_cache,
        );
        dest.pool.func_signatures.push(s);
    }
    for b in &src.pool.bindings {
        let mut b = b.clone();
        b.ty = map_type(
            b.ty,
            from,
            &mut dest_tc.type_info_mut().type_interner,
            &mut ty_cache,
        );
        dest.pool.bindings.push(b);
    }
    for place in &src.pool.places {
        dest.pool
            .places
            .push(remap_place(place, &offs, from, dest_tc, &mut ty_cache));
    }
    for b in &src.pool.for_bindings {
        let mut b = b.clone();
        b.ty = map_type(
            b.ty,
            from,
            &mut dest_tc.type_info_mut().type_interner,
            &mut ty_cache,
        );
        dest.pool.for_bindings.push(b);
    }
    for arm in &src.pool.match_arms {
        dest.pool
            .match_arms
            .push(remap_match_arm(arm, &offs, from, dest_tc, &mut ty_cache));
    }
    for fi in &src.pool.field_inits {
        let mut fi = fi.clone();
        fi.value = offs.expr_id(fi.value);
        dest.pool.field_inits.push(fi);
    }
    for lp in &src.pool.lambda_params {
        let mut lp = lp.clone();
        lp.ty = map_type(
            lp.ty,
            from,
            &mut dest_tc.type_info_mut().type_interner,
            &mut ty_cache,
        );
        dest.pool.lambda_params.push(lp);
    }

    // --- id lists ---
    for &id in &src.pool.expr_ids {
        dest.pool.expr_ids.push(offs.expr_id(id));
    }
    for &id in &src.pool.stmt_ids {
        dest.pool.stmt_ids.push(offs.stmt_id(id));
    }
    for &id in &src.pool.pattern_ids {
        dest.pool.pattern_ids.push(offs.pattern_id(id));
    }
    for &id in &src.pool.field_pattern_ids {
        dest.pool.field_pattern_ids.push(offs.field_pattern_id(id));
    }

    // --- dense nodes ---
    for expr in src.pool.exprs.iter() {
        dest.pool
            .exprs
            .push(remap_expr(expr, &offs, from, dest_tc, &mut ty_cache));
    }
    for stmt in src.pool.stmts.iter() {
        dest.pool
            .stmts
            .push(remap_stmt(stmt, &offs, from, dest_tc, &mut ty_cache));
    }
    for block in src.pool.blocks.iter() {
        let mut block = block.clone();
        block.statements = offs.range(block.statements, offs.stmt_ids);
        dest.pool.blocks.push(block);
    }
    for pat in src.pool.patterns.iter() {
        dest.pool
            .patterns
            .push(remap_pattern(pat, &offs, from, dest_tc, &mut ty_cache));
    }
    for fp in src.pool.field_patterns.iter() {
        let mut fp = fp.clone();
        fp.pattern = fp.pattern.map(|id| offs.pattern_id(id));
        dest.pool.field_patterns.push(fp);
    }
    for decl in src.pool.decls.iter() {
        dest.pool
            .decls
            .push(remap_decl(decl, &offs, from, dest_tc, &mut ty_cache));
    }

    // Top-level decl list
    for &id in &src.decls {
        dest.decls.push(offs.decl_id(id));
    }

    // Type maps (decl_types, struct fields, generics, …)
    dest_tc.type_info_mut().merge_from(&src_tc.type_info);

    // Symbols (params/locals) so AMIR can resolve ids from the imported file.
    for sym in src_tc.symbols.iter() {
        if dest_tc.symbols.try_get(sym.id).is_none() {
            dest_tc.symbols_mut().register_imported_symbol(sym.clone());
        }
    }
    // Module membership remains available after whole-program linking for
    // namespace lookup and diagnostics. Keys retain the source SymbolId,
    // matching the imported symbols registered above.
    for ((module, member), member_id) in &src_tc.symbols.module_members {
        dest_tc
            .symbols_mut()
            .module_members
            .insert((module.clone(), member.clone()), *member_id);
    }
    // Associated methods (type symbol → member) so method dispatch can resolve
    // statically across modules (`self.total.add(...)` → `Stats.add`). Without
    // this map the merged table degrades an `obj.field.method(...)` call to an
    // indirect function call, which neither backend implements.
    for ((ty, member), member_id) in &src_tc.symbols.associated_members {
        dest_tc
            .symbols_mut()
            .associated_members
            .insert((*ty, member.clone()), *member_id);
    }
    for (symbol, name) in &src_tc.symbols.host_function_names {
        dest_tc
            .symbols_mut()
            .host_function_names
            .insert(*symbol, name.clone());
    }
    for (type_id, params) in &src_tc.symbols.type_params {
        dest_tc
            .symbols_mut()
            .type_params
            .entry(*type_id)
            .or_insert_with(|| params.clone());
    }
}

fn remap_place(
    place: &HirPlace,
    offs: &PoolOffsets,
    from: &TypeInterner,
    dest_tc: &mut TypeCheckResult,
    ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirPlace {
    let mut place = place.clone();
    place.ty = map_type(
        place.ty,
        from,
        &mut dest_tc.type_info_mut().type_interner,
        ty_cache,
    );
    for suffix in &mut place.suffixes {
        match suffix {
            HirPlaceSuffix::Field { ty, .. } => {
                *ty = map_type(
                    *ty,
                    from,
                    &mut dest_tc.type_info_mut().type_interner,
                    ty_cache,
                );
            }
            HirPlaceSuffix::Index { expr, ty, .. } => {
                *expr = offs.expr_id(*expr);
                *ty = map_type(
                    *ty,
                    from,
                    &mut dest_tc.type_info_mut().type_interner,
                    ty_cache,
                );
            }
        }
    }
    place
}

fn remap_match_arm(
    arm: &HirMatchArm,
    offs: &PoolOffsets,
    _from: &TypeInterner,
    _dest_tc: &mut TypeCheckResult,
    _ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirMatchArm {
    HirMatchArm {
        span: arm.span,
        pattern: offs.pattern_id(arm.pattern),
        guard: arm.guard.map(|g| offs.expr_id(g)),
        body: match &arm.body {
            HirMatchArmBody::Expr(e) => HirMatchArmBody::Expr(offs.expr_id(*e)),
            HirMatchArmBody::Block(b) => HirMatchArmBody::Block(offs.block_id(*b)),
        },
    }
}

fn remap_condition(cond: &HirCondition, offs: &PoolOffsets) -> HirCondition {
    match cond {
        HirCondition::Expr(e) => HirCondition::Expr(offs.expr_id(*e)),
        HirCondition::Is { expr, pattern } => HirCondition::Is {
            expr: offs.expr_id(*expr),
            pattern: offs.pattern_id(*pattern),
        },
        HirCondition::And(conditions) => HirCondition::And(
            conditions
                .iter()
                .map(|condition| remap_condition(condition, offs))
                .collect(),
        ),
    }
}

fn remap_simple_stmt(stmt: &HirSimpleStmt, offs: &PoolOffsets) -> HirSimpleStmt {
    match stmt {
        HirSimpleStmt::VarDecl { bindings, value } => HirSimpleStmt::VarDecl {
            bindings: offs.range(*bindings, offs.bindings),
            value: offs.expr_id(*value),
        },
        HirSimpleStmt::Set { places, op, value } => HirSimpleStmt::Set {
            places: offs.range(*places, offs.places),
            op: *op,
            value: offs.expr_id(*value),
        },
        HirSimpleStmt::Expr(e) => HirSimpleStmt::Expr(offs.expr_id(*e)),
    }
}

fn remap_expr(
    expr: &HirExpr,
    offs: &PoolOffsets,
    from: &TypeInterner,
    dest_tc: &mut TypeCheckResult,
    ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirExpr {
    let ty = map_type(
        expr.ty,
        from,
        &mut dest_tc.type_info_mut().type_interner,
        ty_cache,
    );
    let kind = match &expr.kind {
        HirExprKind::Path { symbol } => HirExprKind::Path { symbol: *symbol },
        HirExprKind::TypePath {
            type_symbol,
            member_symbol,
        } => HirExprKind::TypePath {
            type_symbol: *type_symbol,
            member_symbol: *member_symbol,
        },
        HirExprKind::Generic { callee, args } => HirExprKind::Generic {
            callee: offs.expr_id(*callee),
            args: args
                .iter()
                .map(|&a| {
                    map_type(
                        a,
                        from,
                        &mut dest_tc.type_info_mut().type_interner,
                        ty_cache,
                    )
                })
                .collect(),
        },
        HirExprKind::Field { base, field } => HirExprKind::Field {
            base: offs.expr_id(*base),
            field: field.clone(),
        },
        HirExprKind::SafeField { base, field } => HirExprKind::SafeField {
            base: offs.expr_id(*base),
            field: field.clone(),
        },
        HirExprKind::Index { base, index } => HirExprKind::Index {
            base: offs.expr_id(*base),
            index: offs.expr_id(*index),
        },
        HirExprKind::SafeIndex { base, index } => HirExprKind::SafeIndex {
            base: offs.expr_id(*base),
            index: offs.expr_id(*index),
        },
        HirExprKind::Try { expr: inner } => HirExprKind::Try {
            expr: offs.expr_id(*inner),
        },
        HirExprKind::Call {
            callee,
            args,
            trailing_block,
        } => HirExprKind::Call {
            callee: offs.expr_id(*callee),
            args: offs.range(*args, offs.expr_ids),
            trailing_block: trailing_block.map(|b| offs.block_id(b)),
        },
        HirExprKind::ResultCtor { variant, value } => HirExprKind::ResultCtor {
            variant: *variant,
            value: offs.expr_id(*value),
        },
        HirExprKind::StructLiteral {
            struct_symbol,
            fields,
        } => HirExprKind::StructLiteral {
            struct_symbol: *struct_symbol,
            fields: offs.range(*fields, offs.field_inits),
        },
        HirExprKind::Array { items } => HirExprKind::Array {
            items: offs.range(*items, offs.expr_ids),
        },
        HirExprKind::Lambda { params, body } => HirExprKind::Lambda {
            params: offs.range(*params, offs.lambda_params),
            body: match body {
                HirLambdaBody::Expr(e) => HirLambdaBody::Expr(offs.expr_id(*e)),
                HirLambdaBody::Block(b) => HirLambdaBody::Block(offs.block_id(*b)),
            },
        },
        HirExprKind::Alloc { expr: inner } => HirExprKind::Alloc {
            expr: offs.expr_id(*inner),
        },
        HirExprKind::AsyncBlock { block } => HirExprKind::AsyncBlock {
            block: offs.block_id(*block),
        },
        HirExprKind::UnsafeBlock { block } => HirExprKind::UnsafeBlock {
            block: offs.block_id(*block),
        },
        HirExprKind::If {
            condition,
            then_block,
            else_block,
        } => HirExprKind::If {
            condition: remap_condition(condition, offs),
            then_block: offs.block_id(*then_block),
            else_block: offs.block_id(*else_block),
        },
        HirExprKind::Match { value, arms } => HirExprKind::Match {
            value: offs.expr_id(*value),
            arms: offs.range(*arms, offs.match_arms),
        },
        HirExprKind::Catch {
            expr: inner,
            handler,
        } => HirExprKind::Catch {
            expr: offs.expr_id(*inner),
            handler: match handler {
                HirCatchHandler::Expr(e) => HirCatchHandler::Expr(offs.expr_id(*e)),
                HirCatchHandler::Block {
                    error_symbol,
                    error_name,
                    block,
                } => HirCatchHandler::Block {
                    error_symbol: *error_symbol,
                    error_name: error_name.clone(),
                    block: offs.block_id(*block),
                },
            },
        },
        HirExprKind::NullCoalesce { left, right } => HirExprKind::NullCoalesce {
            left: offs.expr_id(*left),
            right: offs.expr_id(*right),
        },
        HirExprKind::Cast {
            expr: inner,
            target_ty,
        } => HirExprKind::Cast {
            expr: offs.expr_id(*inner),
            target_ty: map_type(
                *target_ty,
                from,
                &mut dest_tc.type_info_mut().type_interner,
                ty_cache,
            ),
        },
        HirExprKind::Unary { op, expr: inner } => HirExprKind::Unary {
            op: *op,
            expr: offs.expr_id(*inner),
        },
        HirExprKind::Binary { op, left, right } => HirExprKind::Binary {
            op: *op,
            left: offs.expr_id(*left),
            right: offs.expr_id(*right),
        },
        HirExprKind::Int(s) => HirExprKind::Int(s.clone()),
        HirExprKind::Float(s) => HirExprKind::Float(s.clone()),
        HirExprKind::Bool(b) => HirExprKind::Bool(*b),
        HirExprKind::Char(s) => HirExprKind::Char(s.clone()),
        HirExprKind::Str(s) => HirExprKind::Str(s.clone()),
        HirExprKind::StringInterp { parts } => HirExprKind::StringInterp {
            parts: parts
                .iter()
                .map(|p| match p {
                    HirStringPart::Text(t) => HirStringPart::Text(t.clone()),
                    HirStringPart::Expr(e) => HirStringPart::Expr(offs.expr_id(*e)),
                })
                .collect(),
        },
        HirExprKind::ToStr { value } => HirExprKind::ToStr {
            value: offs.expr_id(*value),
        },
        HirExprKind::Nil => HirExprKind::Nil,
        HirExprKind::Error => HirExprKind::Error,
    };
    HirExpr {
        kind,
        ty,
        span: expr.span,
    }
}

fn remap_stmt(
    stmt: &HirStmt,
    offs: &PoolOffsets,
    _from: &TypeInterner,
    _dest_tc: &mut TypeCheckResult,
    _ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirStmt {
    let kind = match &stmt.kind {
        HirStmtKind::VarDecl { bindings, value } => HirStmtKind::VarDecl {
            bindings: offs.range(*bindings, offs.bindings),
            value: offs.expr_id(*value),
        },
        HirStmtKind::Set { places, op, value } => HirStmtKind::Set {
            places: offs.range(*places, offs.places),
            op: *op,
            value: offs.expr_id(*value),
        },
        HirStmtKind::Return { values } => HirStmtKind::Return {
            values: offs.range(*values, offs.expr_ids),
        },
        HirStmtKind::Break => HirStmtKind::Break,
        HirStmtKind::Continue => HirStmtKind::Continue,
        HirStmtKind::Free(e) => HirStmtKind::Free(offs.expr_id(*e)),
        HirStmtKind::Expr(e) => HirStmtKind::Expr(offs.expr_id(*e)),
        HirStmtKind::If {
            condition,
            then_block,
            else_block,
        } => HirStmtKind::If {
            condition: remap_condition(condition, offs),
            then_block: offs.block_id(*then_block),
            else_block: else_block.map(|b| offs.block_id(b)),
        },
        HirStmtKind::For { clause, body } => HirStmtKind::For {
            clause: match clause {
                HirForClause::In {
                    span,
                    bindings,
                    iterable,
                } => HirForClause::In {
                    span: *span,
                    bindings: offs.range(*bindings, offs.for_bindings),
                    iterable: offs.expr_id(*iterable),
                },
                HirForClause::CStyle {
                    span,
                    init,
                    condition,
                    step,
                } => HirForClause::CStyle {
                    span: *span,
                    init: init.as_ref().map(|s| remap_simple_stmt(s, offs)),
                    condition: condition.map(|c| offs.expr_id(c)),
                    step: step.as_ref().map(|s| remap_simple_stmt(s, offs)),
                },
            },
            body: offs.block_id(*body),
        },
        HirStmtKind::While { condition, body } => HirStmtKind::While {
            condition: remap_condition(condition, offs),
            body: offs.block_id(*body),
        },
        HirStmtKind::Match { value, arms } => HirStmtKind::Match {
            value: offs.expr_id(*value),
            arms: offs.range(*arms, offs.match_arms),
        },
        HirStmtKind::Defer(b) => HirStmtKind::Defer(offs.block_id(*b)),
        HirStmtKind::ErrDefer(b) => HirStmtKind::ErrDefer(offs.block_id(*b)),
        HirStmtKind::Unsafe(b) => HirStmtKind::Unsafe(offs.block_id(*b)),
        HirStmtKind::Error => HirStmtKind::Error,
    };
    HirStmt {
        kind,
        span: stmt.span,
    }
}

fn remap_pattern(
    pat: &HirPattern,
    offs: &PoolOffsets,
    _from: &TypeInterner,
    _dest_tc: &mut TypeCheckResult,
    _ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirPattern {
    match pat {
        HirPattern::Wildcard { span } => HirPattern::Wildcard { span: *span },
        HirPattern::Bind { span, name, symbol } => HirPattern::Bind {
            span: *span,
            name: name.clone(),
            symbol: *symbol,
        },
        HirPattern::Literal { span, expr } => HirPattern::Literal {
            span: *span,
            expr: offs.expr_id(*expr),
        },
        HirPattern::Enum {
            span,
            type_symbol,
            variant,
            variant_symbol,
            payload,
        } => HirPattern::Enum {
            span: *span,
            type_symbol: *type_symbol,
            variant: variant.clone(),
            variant_symbol: *variant_symbol,
            payload: offs.range(*payload, offs.pattern_ids),
        },
        HirPattern::TypeTuple {
            span,
            name,
            payload,
        } => HirPattern::TypeTuple {
            span: *span,
            name: name.clone(),
            payload: offs.range(*payload, offs.pattern_ids),
        },
        HirPattern::Struct {
            span,
            struct_symbol,
            fields,
        } => HirPattern::Struct {
            span: *span,
            struct_symbol: *struct_symbol,
            fields: offs.range(*fields, offs.field_pattern_ids),
        },
        HirPattern::Tuple { span, items } => HirPattern::Tuple {
            span: *span,
            items: offs.range(*items, offs.pattern_ids),
        },
        HirPattern::Range {
            span,
            start,
            inclusive,
            end,
        } => HirPattern::Range {
            span: *span,
            start: offs.expr_id(*start),
            inclusive: *inclusive,
            end: offs.expr_id(*end),
        },
        HirPattern::Or { span, alts } => HirPattern::Or {
            span: *span,
            alts: offs.range(*alts, offs.pattern_ids),
        },
    }
}

fn remap_decl(
    decl: &HirDecl,
    offs: &PoolOffsets,
    from: &TypeInterner,
    dest_tc: &mut TypeCheckResult,
    ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirDecl {
    match decl {
        HirDecl::Const(c) => {
            let mut c = c.clone();
            c.ty = map_type(
                c.ty,
                from,
                &mut dest_tc.type_info_mut().type_interner,
                ty_cache,
            );
            c.value = offs.expr_id(c.value);
            HirDecl::Const(c)
        }
        HirDecl::TypeAlias(a) => {
            let mut a = a.clone();
            a.target = map_type(
                a.target,
                from,
                &mut dest_tc.type_info_mut().type_interner,
                ty_cache,
            );
            HirDecl::TypeAlias(a)
        }
        HirDecl::Func(f) => {
            let mut f = f.clone();
            f.params = offs.range(f.params, offs.params);
            f.return_type = map_type(
                f.return_type,
                from,
                &mut dest_tc.type_info_mut().type_interner,
                ty_cache,
            );
            f.body = f.body.map(|b| offs.block_id(b));
            HirDecl::Func(f)
        }
        HirDecl::Struct(s) => {
            let mut s = s.clone();
            s.fields = offs.range(s.fields, offs.struct_fields);
            HirDecl::Struct(s)
        }
        HirDecl::Enum(e) => {
            let mut e = e.clone();
            e.variants = offs.range(e.variants, offs.enum_variants);
            HirDecl::Enum(e)
        }
        HirDecl::Interface(i) => HirDecl::Interface(i.clone()),
        HirDecl::Extern(ex) => {
            let mut ex = ex.clone();
            ex.members = offs.range(ex.members, offs.func_signatures);
            HirDecl::Extern(ex)
        }
    }
}
