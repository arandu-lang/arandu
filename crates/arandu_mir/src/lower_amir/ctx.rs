use super::{DeferKind, LowerCtx, MoveState};
use crate::amir::{
    AmirConstant, AmirLocal, AmirOperand, AmirPlace, AmirRvalue, AmirStmt, AmirTemp,
    AmirTerminator, BlockId, LocalId, TempId,
};
use crate::diagnostics::Diagnostic;
use crate::hir::HirBlock;
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;

impl LowerCtx<'_> {
    pub(crate) fn lower_const_operand(
        &mut self,
        symbol: SymbolId,
    ) -> Result<Option<AmirOperand>, Diagnostic> {
        let Some(&expr) = self.const_values.get(&symbol) else {
            return Ok(None);
        };
        let hir_expr = self.hir.pool.expr(expr);
        if let crate::hir::HirExprKind::Layout { query, operand_ty } = hir_expr.kind {
            return self
                .lower_layout_query(query, operand_ty, hir_expr.ty, None, hir_expr.span)
                .map(Some);
        }
        if self.tc.type_info.ctfe_global_values.contains_key(&symbol) {
            // Frozen declarations contain only admitted materialized values.
            // Interpret that value in this unit's literal pool instead of
            // leaving a runtime GlobalRef in a CTFE expression/helper.
            let symbols = std::sync::Arc::clone(&self.tc.symbols);
            return self.lower_expr(expr, None, &symbols).map(Some);
        }
        // Preserve integer consts through a compile-time cast. Returning the
        // literal directly drops its inferred target type in AMIR; materializing
        // it in a typed temp keeps comparisons (e.g. `uint > CONST`) well typed.
        let value = match &hir_expr.kind {
            crate::hir::HirExprKind::Int(value) => value.clone(),
            crate::hir::HirExprKind::Cast { expr, .. } => {
                let inner = self.hir.pool.expr(*expr);
                let crate::hir::HirExprKind::Int(value) = &inner.kind else {
                    return Ok(None);
                };
                value.clone()
            }
            _ => return Ok(None),
        };
        let ty = self
            .tc
            .type_info
            .decl_type_id(symbol)
            .unwrap_or(hir_expr.ty);
        let temp = self.new_temp_id(ty);
        let value = self.intern_literal_int(value);
        self.emit_assign_temp(
            temp,
            crate::amir::AmirRvalue::Use(AmirOperand::Constant(value)),
        );
        Ok(Some(AmirOperand::Copy(temp)))
    }

    pub(crate) fn next_local_id(&self) -> LocalId {
        LocalId::from_usize(self.locals.len())
    }

    pub(crate) fn next_temp_id(&self) -> TempId {
        TempId::from_usize(self.temps.len())
    }

    #[inline]
    pub(crate) fn intern_literal_int(&mut self, s: impl Into<smol_str::SmolStr>) -> AmirConstant {
        AmirConstant::Pool(self.literal_pool.intern_int(s))
    }

    #[inline]
    pub(crate) fn intern_literal_str(&mut self, s: impl Into<smol_str::SmolStr>) -> AmirConstant {
        AmirConstant::Pool(self.literal_pool.intern_str(s))
    }

    #[inline]
    pub(crate) fn intern_literal_char(&mut self, s: impl Into<smol_str::SmolStr>) -> AmirConstant {
        let source = s.into();
        let value = if let Some(value) = arandu_lexer::decode_char_content(&source) {
            let mut encoded = [0; 4];
            smol_str::SmolStr::new(value.encode_utf8(&mut encoded))
        } else {
            source
        };
        AmirConstant::Pool(self.literal_pool.intern_char(value))
    }

    pub(crate) fn intern_ty(&self, ty: ArType) -> crate::types::TypeId {
        self.intern_ty_ref(&ty)
    }

    pub(crate) fn intern_ty_ref(&self, ty: &crate::types::ArType) -> crate::types::TypeId {
        self.tc.type_info.type_interner.intern_ref(ty)
    }

    /// Resolve a HIR/AMIR `TypeId` to an owned `ArType` (clones from the interner).
    #[inline]
    pub(crate) fn resolve_ty(&self, id: crate::types::TypeId) -> ArType {
        self.tc.type_info.type_interner.resolve(id)
    }

    /// Borrow the interned type without cloning.
    #[inline]
    pub(crate) fn with_ty<R>(&self, id: crate::types::TypeId, f: impl FnOnce(&ArType) -> R) -> R {
        self.tc.type_info.type_interner.with_type(id, f)
    }

    /// Allocate a temp from an already-interned type id (no `ArType` clone).
    pub(crate) fn new_temp_id(&mut self, ty: crate::types::TypeId) -> TempId {
        let is_copy = self.tc.type_info.is_copy(ty);
        let is_nullable = self
            .tc
            .type_info
            .type_interner
            .with_type(ty, |t| matches!(t, ArType::Nullable(_)));
        let id = self.next_temp_id();
        let span = if Self::span_is_usable(self.current_span) {
            self.current_span
        } else {
            Span::new(0, 0, 0)
        };
        self.temps.push(AmirTemp {
            id,
            ty,
            is_copy,
            is_nullable,
            span,
        });
        self.temp_states.push(MoveState::Available);
        self.temp_origins.push(None);
        self.temp_place_origins.push(None);
        id
    }

    /// Allocate a local from an already-interned type id.
    pub(crate) fn new_local_id(
        &mut self,
        ty: crate::types::TypeId,
        symbol: SymbolId,
        span: Span,
    ) -> LocalId {
        let is_memory = self
            .tc
            .type_info
            .type_interner
            .with_type(ty, super::is_memory_type);
        let id = self.next_local_id();
        self.locals.push(AmirLocal {
            id,
            ty,
            is_memory,
            symbol: Some(symbol),
            span,
            use_span: None,
        });
        self.local_states.push(MoveState::Available);
        self.symbol_map.insert(symbol, id);
        self.track_local_in_scope(id);
        id
    }

    /// Non-empty source span (start != end). Empty spans are treated as unknown.
    #[inline]
    pub(crate) fn span_is_usable(span: Span) -> bool {
        span.start != span.end
    }

    /// Best available span for diagnostics: prefer non-empty `preferred`, else `current_span`.
    #[inline]
    pub(crate) fn diag_span(&self, preferred: Span) -> Span {
        if Self::span_is_usable(preferred) {
            preferred
        } else if Self::span_is_usable(self.current_span) {
            self.current_span
        } else {
            Span::new(0, 0, 0)
        }
    }

    /// Run `f` with `current_span` set to `span` when usable (restores previous after).
    pub(crate) fn with_span<R>(&mut self, span: Span, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.current_span;
        if Self::span_is_usable(span) {
            self.current_span = span;
        }
        let out = f(self);
        self.current_span = prev;
        out
    }

    pub(crate) fn new_temp(&mut self, ty: ArType) -> TempId {
        self.new_temp_ref(&ty)
    }

    /// Intern from borrow — avoids cloning `ArType` when the caller only has a ref
    /// (e.g. `expr.ty`).
    pub(crate) fn new_temp_ref(&mut self, ty: &ArType) -> TempId {
        let ty = self.intern_ty_ref(ty);
        let is_copy = self.tc.type_info.is_copy(ty);
        let is_nullable = self
            .tc
            .type_info
            .type_interner
            .with_type(ty, |t| matches!(t, ArType::Nullable(_)));
        let id = self.next_temp_id();
        let span = if Self::span_is_usable(self.current_span) {
            self.current_span
        } else {
            Span::new(0, 0, 0)
        };
        self.temps.push(AmirTemp {
            id,
            ty,
            is_copy,
            is_nullable,
            span,
        });
        self.temp_states.push(MoveState::Available);
        self.temp_origins.push(None);
        self.temp_place_origins.push(None);
        id
    }

    pub(crate) fn new_local(&mut self, ty: ArType, symbol: SymbolId, span: Span) -> LocalId {
        self.new_local_ref(&ty, symbol, span)
    }

    pub(crate) fn new_local_ref(&mut self, ty: &ArType, symbol: SymbolId, span: Span) -> LocalId {
        let is_memory = super::is_memory_type(ty);
        let ty = self.intern_ty_ref(ty);
        let id = self.next_local_id();
        self.locals.push(AmirLocal {
            id,
            ty,
            is_memory,
            symbol: Some(symbol),
            span,
            use_span: None,
        });
        self.local_states.push(MoveState::Available);
        self.symbol_map.insert(symbol, id);
        self.track_local_in_scope(id);
        id
    }

    pub(crate) fn new_compiler_local(&mut self, ty: ArType) -> LocalId {
        let ty = self.intern_ty(ty);
        self.new_compiler_local_id(ty, Span::new(0, 0, 0))
    }

    /// Allocate a materialized compiler temporary without associating it with
    /// a user symbol. Keeping the symbol absent prevents the sentinel
    /// [`SymbolId::DUMMY`] from leaking into debug metadata or AMIR printers.
    pub(crate) fn new_compiler_local_id(
        &mut self,
        ty: crate::types::TypeId,
        span: Span,
    ) -> LocalId {
        let is_memory = self
            .tc
            .type_info
            .type_interner
            .with_type(ty, super::is_memory_type);
        let id = self.next_local_id();
        self.locals.push(AmirLocal {
            id,
            ty,
            is_memory,
            symbol: None,
            span,
            use_span: None,
        });
        self.local_states.push(MoveState::Available);
        self.track_local_in_scope(id);
        id
    }

    fn track_local_in_scope(&mut self, local: LocalId) {
        if let Some(scope) = self.local_scopes.last_mut() {
            scope.push(local);
        }
    }

    pub(crate) fn begin_local_scope(&mut self) -> usize {
        self.local_scopes.push(Vec::new());
        self.local_scopes.len() - 1
    }

    /// Ends the innermost lexical scope, emitting StorageDead markers in
    /// reverse declaration order so elaboration can place deterministic drops.
    pub(crate) fn end_local_scope(&mut self) {
        if let Some(scope) = self.local_scopes.pop()
            && !self.local_scopes.is_empty()
            && self.builder.current_block.is_some()
        {
            for local in scope.into_iter().rev() {
                if self.local_needs_scope_drop(local) {
                    self.push_stmt(crate::amir::AmirStmt::StorageDead(local));
                }
            }
        }
    }

    /// Emits scope-exit markers for every lexical scope nested inside `depth`.
    /// The scopes remain on the compile-time stack: another CFG branch may
    /// still be lowered from the same lexical source position.
    pub(crate) fn emit_local_scope_exit_from(&mut self, depth: usize) {
        let locals = self
            .local_scopes
            .iter()
            .skip(depth)
            .rev()
            .flat_map(|scope| scope.iter().rev().copied())
            .collect::<Vec<_>>();
        for local in locals {
            if self.local_needs_scope_drop(local) {
                self.push_stmt(crate::amir::AmirStmt::StorageDead(local));
            }
        }
    }

    fn local_needs_scope_drop(&self, local: LocalId) -> bool {
        let Some(local) = self.locals.get(local.as_usize()) else {
            return false;
        };
        crate::drop_elaborate::type_needs_drop(local.ty, &self.tc.type_info)
            || self.tc.type_info.type_interner.with_type(local.ty, |ty| {
                matches!(ty, ArType::Primitive(Primitive::Str))
            })
    }

    pub(crate) fn operand_type(&self, op: &AmirOperand) -> ArType {
        match op {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                self.resolve_ty(self.temps[temp_id.as_usize()].ty)
            }
            AmirOperand::Constant(c) => match c {
                AmirConstant::Bool(_) => ArType::Primitive(Primitive::Bool),
                AmirConstant::Nil => ArType::Error,
                AmirConstant::Pool(_) => ArType::Error,
            },
            _ => ArType::Error,
        }
    }

    /// A3.1/A3.5: end the current block at an `await` frontier and continue in `resume`.
    ///
    /// **Dense live capture (gold design):** every local that currently has a definition
    /// in this function (`Available` + a `current_def` in the suspend block) is forced
    /// onto the resume block as a block param *before* building terminator args. That
    /// makes `Suspend.args` the explicit coroutine state at the frontier (same shape
    /// as `goto bb_resume(x, y, …)`), not an empty edge relying only on later phis.
    ///
    /// Over-approx vs true liveness is intentional for lower-time (no full dataflow
    /// yet); unused captures are cheap SSA params that later DCE can drop.
    pub(crate) fn emit_suspend(
        &mut self,
        future: AmirOperand,
        resume: BlockId,
    ) -> Result<(), Diagnostic> {
        let Some(curr) = self.builder.current_block else {
            return Err(Diagnostic::ice(
                crate::DiagCode::ICEGEN001,
                "AMIR lower: emit_suspend without current block",
                self.diag_span(self.current_span),
            ));
        };
        // Seed resume params for locals defined on this path (coroutine state slots).
        let n_locals = self.locals.len();
        for i in 0..n_locals {
            let local = LocalId::from_usize(i);
            if !matches!(self.local_states.get(i), Some(MoveState::Available)) {
                continue;
            }
            // Only capture if we have a def reaching this block (skip never-written).
            if !self.current_def.contains_key(&(curr, local)) {
                // Also try recursive read — may still be live via pred.
                // Avoid creating params for completely unused locals: require
                // either current_def here or a use after would need it.
                // Conservative: only current_def at suspend block.
                continue;
            }
            // Unsealed resume → incomplete phi / block param for `local`.
            let _ = self.read_variable(resume, local);
        }
        let args = self.build_target_args(resume);
        self.set_terminator(AmirTerminator::Suspend {
            future,
            resume,
            args,
        });
        Ok(())
    }

    pub(crate) fn set_bool_branch(
        &mut self,
        condition: AmirOperand,
        if_true: BlockId,
        if_false: BlockId,
    ) {
        let true_args = self.build_target_args(if_true);
        let false_args = self.build_target_args(if_false);
        self.set_terminator(AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        });
    }

    pub(crate) fn emit_goto(&mut self, target: BlockId) {
        let args = self.build_target_args(target);
        self.set_terminator(AmirTerminator::Goto { target, args });
    }

    pub(crate) fn emit_switch_int(
        &mut self,
        discriminant: AmirOperand,
        targets: Vec<(i128, BlockId)>,
        otherwise: BlockId,
    ) {
        let mut target_args_list = Vec::new();
        for (val, target) in targets {
            let args = self.build_target_args(target);
            target_args_list.push((val, target, args));
        }
        let otherwise_args = self.build_target_args(otherwise);
        self.set_terminator(AmirTerminator::SwitchInt {
            discriminant,
            targets: target_args_list,
            otherwise: (otherwise, otherwise_args),
        });
    }

    pub(crate) fn lower_defer_block(
        &mut self,
        block: &HirBlock,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        for &stmt_id in self.hir.pool.stmt_list(block.statements) {
            if self.builder.current_block.is_none() {
                break;
            }
            let stmt = self.hir.pool.stmt(stmt_id);
            self.lower_stmt(stmt, symbols)?;
        }
        Ok(())
    }

    pub(crate) fn exit_current_defer_frame(
        &mut self,
        include_errdefer: bool,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if let Some(frame) = self.defer_frames.pop() {
            if include_errdefer {
                for (block, kind) in frame.entries.iter().rev() {
                    if *kind == DeferKind::ErrDefer {
                        self.lower_defer_block(block, symbols)?;
                    }
                }
            }
            for (block, kind) in frame.entries.iter().rev() {
                if *kind == DeferKind::Defer {
                    self.lower_defer_block(block, symbols)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn exit_defer_frames_from(
        &mut self,
        target_depth: usize,
        include_errdefer: bool,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        // Emit cleanup for this CFG edge without consuming lexical schedules
        // needed by sibling paths. Frames contain only block ranges/kinds, not
        // copies of HIR arenas. Pop while emitting to prevent a defer's own
        // control flow from recursively executing that same frame.
        let lexical_frames = self.defer_frames.clone();
        while self.defer_frames.len() > target_depth {
            self.exit_current_defer_frame(include_errdefer, symbols)?;
        }
        self.defer_frames = lexical_frames;
        Ok(())
    }

    pub(crate) fn exit_all_defer_frames(
        &mut self,
        include_errdefer: bool,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.exit_defer_frames_from(0, include_errdefer, symbols)
    }

    pub(crate) fn register_defer(&mut self, block: &HirBlock, kind: DeferKind) {
        if let Some(frame) = self.defer_frames.last_mut() {
            frame.entries.push((block.clone(), kind));
        }
    }

    pub(crate) fn emit_assign_temp(&mut self, temp: TempId, rhs: AmirRvalue) {
        self.push_stmt(AmirStmt::Assign { lhs: temp, rhs });
    }

    pub(crate) fn emit_store_place(
        &mut self,
        lhs: AmirPlace,
        rhs: AmirOperand,
    ) -> Result<(), Diagnostic> {
        let rhs = self.consume_operand(rhs)?;
        // Projection store reads the base local (field/index write).
        if !lhs.projections.is_empty() {
            self.note_local_use(lhs.local, self.current_span);
        }
        let root_local = lhs.projections.is_empty().then_some(lhs.local);
        let needs_owner_reload = root_local
            .is_some_and(|local| !self.tc.type_info.is_copy(self.locals[local.as_usize()].ty));
        if let Some(local) = root_local {
            self.local_states[local.as_usize()] = MoveState::Available;
        }
        self.push_stmt(AmirStmt::Store {
            lhs: lhs.clone(),
            rhs,
        });
        if let (Some(local), Some(block)) = (root_local, self.builder.current_block) {
            let value = if needs_owner_reload {
                // A non-Copy binding now owns the value. Do not keep the RHS
                // temp as its SSA definition: that temp's provenance still
                // names the moved source place and would make later uses of
                // the binding look like repeated moves from that source.
                let ty = self.locals[local.as_usize()].ty;
                let place = AmirPlace {
                    local,
                    projections: smallvec::SmallVec::new(),
                };
                let temp = self.new_temp_id(ty);
                self.emit_assign_temp(temp, AmirRvalue::Load(place.clone()));
                self.temp_origins[temp.as_usize()] = Some(local);
                self.temp_place_origins[temp.as_usize()] = Some(place);
                AmirOperand::Copy(temp)
            } else {
                rhs
            };
            self.write_variable(block, local, value);
        }
        Ok(())
    }

    /// Record the latest source use of a local (S-USE-SPAN). Analyses prefer
    /// `use_span` over declaration span so O008/move point at the use site.
    pub(crate) fn note_local_use(&mut self, local: LocalId, span: Span) {
        // Skip empty spans so synthetic lowers don't wipe a real use site.
        if !Self::span_is_usable(span) {
            return;
        }
        if let Some(loc) = self.locals.get_mut(local.as_usize()) {
            loc.use_span = Some(span);
        }
    }

    /// Note use of the stack origin of a temp, if any (move / free / call args).
    pub(crate) fn note_temp_origin_use(&mut self, temp: TempId) {
        if let Some(Some(local)) = self.temp_origins.get(temp.as_usize()) {
            self.note_local_use(*local, self.current_span);
        }
    }

    pub(crate) fn load_place(
        &mut self,
        place: &AmirPlace,
        ty: crate::types::TypeId,
    ) -> Result<AmirOperand, Diagnostic> {
        self.note_local_use(place.local, self.current_span);
        let temp = self.new_temp_id(ty);
        self.emit_assign_temp(temp, AmirRvalue::Load(place.clone()));
        // Preserve the root for lowering-time diagnostics. The MIR move checker
        // derives the full projection path from the emitted `Load(place)`.
        self.temp_origins[temp.as_usize()] = Some(place.local);
        self.temp_place_origins[temp.as_usize()] = Some(place.clone());
        Ok(AmirOperand::Copy(temp))
    }

    pub(crate) fn consume_operand(&mut self, op: AmirOperand) -> Result<AmirOperand, Diagnostic> {
        let AmirOperand::Copy(temp) = op else {
            return Ok(op);
        };
        let idx = temp.as_usize();
        if self.temp_states[idx] == MoveState::Moved {
            return Err(self.move_diag(format!("use of moved temporary _{idx}")));
        }
        // Consuming a non-copy temp is a use of its origin local at this site.
        self.note_temp_origin_use(temp);
        if self.temps[idx].is_copy {
            return Ok(AmirOperand::Copy(temp));
        }
        self.temp_states[idx] = MoveState::Moved;
        Ok(AmirOperand::Move(temp))
    }

    pub(crate) fn move_diag(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(
            crate::DiagCode::U001FeatureNotSupported,
            message.into(),
            self.diag_span(self.current_span),
        )
    }
}
