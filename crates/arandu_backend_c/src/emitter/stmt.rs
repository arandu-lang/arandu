use super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirStmt, AmirTerminator, TempId};
use arandu_middle::types::{ArType, Primitive};
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    /// Emit `std.core.mem` intrinsics as C loads/stores/pointer arithmetic.
    ///
    /// Parity with Cranelift JIT: `sizeOf` is normally folded in AMIR lower;
    /// residual calls and `ptrOffset` / `ptrRead` / `ptrWrite` must not become
    /// unresolved external symbols in emit-c.
    fn try_emit_mem_intrinsic(
        &mut self,
        lhs: &Option<TempId>,
        callee: &AmirOperand,
        args: &[AmirOperand],
        func: &AmirFunc,
    ) -> bool {
        let name = match callee {
            AmirOperand::FunctionRef(id) | AmirOperand::GlobalRef(id) => {
                self.symbols.get(*id).name.as_str()
            }
            _ => return false,
        };
        let kind = arandu_middle::IntrinsicKind::from_name(name);

        match kind {
            Some(arandu_middle::IntrinsicKind::Abort) => {
                let _ = writeln!(&mut self.output, "    abort();");
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrRead) => {
                if args.is_empty() {
                    return true;
                }
                let p = self.format_operand(&args[0], func);
                if let Some(dest) = lhs {
                    let _ = writeln!(&mut self.output, "    t{} = *({});", dest.as_usize(), p);
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrWrite) => {
                if args.len() < 2 {
                    return true;
                }
                let p = self.format_operand(&args[0], func);
                let v = self.format_operand(&args[1], func);
                let _ = writeln!(&mut self.output, "    *({}) = {};", p, v);
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrOffset) => {
                if args.len() < 2 {
                    return true;
                }
                let p = self.format_operand(&args[0], func);
                let i = self.format_operand(&args[1], func);
                // C pointer arithmetic scales by pointee size when `p` is a typed pointer.
                if let Some(dest) = lhs {
                    let _ = writeln!(
                        &mut self.output,
                        "    t{} = ({}) + ({});",
                        dest.as_usize(),
                        p,
                        i
                    );
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::SizeOf | arandu_middle::IntrinsicKind::AlignOf) => {
                // Residual only — prefer AMIR fold. Host pointer width for `int`.
                let n = if kind == Some(arandu_middle::IntrinsicKind::SizeOf) {
                    self.layout.pointer_width()
                } else {
                    self.layout.pointer_width().min(8)
                };
                if let Some(dest) = lhs {
                    let _ = writeln!(&mut self.output, "    t{} = {}ULL;", dest.as_usize(), n);
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn emit_stmt(&mut self, stmt: &AmirStmt, func: &AmirFunc) {
        match stmt {
            AmirStmt::Assign { lhs, rhs } => {
                let lhs_ty = self.temp_ty(func, *lhs);
                let lhs_c_ty = self.format_type(&lhs_ty);
                match rhs {
                    arandu_middle::amir::AmirRvalue::GenInsert {
                        value, payload_ty, ..
                    } => {
                        self.emit_gen_write_assign(*lhs, None, value, *payload_ty, func, false);
                        return;
                    }
                    arandu_middle::amir::AmirRvalue::GenSet {
                        gen_ref,
                        value,
                        payload_ty,
                        ..
                    } => {
                        self.emit_gen_write_assign(
                            *lhs,
                            Some(gen_ref),
                            value,
                            *payload_ty,
                            func,
                            false,
                        );
                        return;
                    }
                    arandu_middle::amir::AmirRvalue::GenUpsert {
                        gen_ref,
                        value,
                        payload_ty,
                        ..
                    } => {
                        self.emit_gen_write_assign(
                            *lhs,
                            Some(gen_ref),
                            value,
                            *payload_ty,
                            func,
                            true,
                        );
                        return;
                    }
                    arandu_middle::amir::AmirRvalue::GenGet {
                        gen_ref,
                        payload_ty,
                        ..
                    } => {
                        self.emit_gen_read_assign(
                            *lhs,
                            "ar_gen_get_raw",
                            gen_ref,
                            *payload_ty,
                            func,
                        );
                        return;
                    }
                    arandu_middle::amir::AmirRvalue::GenRemove {
                        gen_ref,
                        payload_ty,
                        ..
                    } => {
                        self.emit_gen_read_assign(
                            *lhs,
                            "ar_gen_remove_raw",
                            gen_ref,
                            *payload_ty,
                            func,
                        );
                        return;
                    }
                    _ => {}
                }
                // A3.3 multi-stmt stack: declare payload local, store value, take address.
                // (Statement-expression `&local` would dangle after the expression ends.)
                if let arandu_middle::amir::AmirRvalue::CoroutineReady {
                    value,
                    payload_ty,
                    stack: true,
                } = rhs
                {
                    // A3.6 stack blob: disc@0 + payload@8 (aligned buffer).
                    let payload_ar = self.interner.resolve(*payload_ty);
                    let payload_c = self.format_type(&payload_ar);
                    let v = self.format_operand(value, func);
                    let slot = self.next_co_stack_slot();
                    let payload_size = self.checked_layout(&payload_ar).size.max(1);
                    let size = 8 + payload_size;
                    let _ = writeln!(
                        &mut self.output,
                        "    uint8_t __ar_co_{slot}[{size}] __attribute__((aligned(8)));"
                    );
                    let _ = writeln!(&mut self.output, "    *(uint32_t*)__ar_co_{slot} = 0;");
                    let _ = writeln!(
                        &mut self.output,
                        "    *({payload_c}*)(__ar_co_{slot} + 8) = ({payload_c})({v});"
                    );
                    let _ = writeln!(
                        &mut self.output,
                        "    t{} = (void*)__ar_co_{slot};",
                        lhs.as_usize()
                    );
                    return;
                }
                let _ = write!(&mut self.output, "    t{} = ", lhs.as_usize());
                self.emit_rvalue(rhs, func, &lhs_ty, &lhs_c_ty);
                let _ = writeln!(&mut self.output, ";");
            }
            AmirStmt::Store { lhs, rhs } => {
                let lhs_str = self.format_place(lhs, func);
                let rhs_str = self.format_operand(rhs, func);
                let _ = writeln!(&mut self.output, "    {} = {};", lhs_str, rhs_str);
            }
            AmirStmt::Call {
                lhs, callee, args, ..
            } => {
                if self.try_emit_mem_intrinsic(lhs, callee, args, func) {
                    return;
                }
                if let AmirOperand::FunctionRef(symbol) = callee
                    && self
                        .symbols
                        .get(*symbol)
                        .name
                        .contains("ar_string_push_str")
                    && let [owner, value] = args.as_slice()
                {
                    let owner = self.format_operand(owner, func);
                    let value = self.format_operand(value, func);
                    if let Some(dest) = lhs {
                        let _ = writeln!(
                            &mut self.output,
                            "    t{} = ar_string_push_str({}, ({}).ptr, ({}).len);",
                            dest.as_usize(),
                            owner,
                            value,
                            value
                        );
                    } else {
                        let _ = writeln!(
                            &mut self.output,
                            "    ar_string_push_str({}, ({}).ptr, ({}).len);",
                            owner, value, value
                        );
                    }
                    return;
                }
                let callee_str = self.format_operand(callee, func);
                let args_str: Vec<_> = args.iter().map(|a| self.format_operand(a, func)).collect();
                if let Some(dest) = lhs {
                    let _ = write!(&mut self.output, "    t{} = ", dest.as_usize());
                } else {
                    let _ = write!(&mut self.output, "    ");
                }
                let _ = write!(&mut self.output, "{callee_str}(");
                for (i, arg_str) in args_str.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(&mut self.output, ", ");
                    }
                    let _ = write!(&mut self.output, "{}", arg_str);
                }
                let _ = writeln!(&mut self.output, ");");
            }
            AmirStmt::Free(op) => {
                let op_ty = self.operand_ty(func, op);
                let op_str = self.format_operand(op, func);
                if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                    let _ = writeln!(
                        &mut self.output,
                        "    if ({op_str}.ptr) {{ free((void*){op_str}.ptr); }}"
                    );
                } else {
                    let _ = writeln!(&mut self.output, "    free({});", op_str);
                }
            }
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) => {}
            AmirStmt::Destroy(place) => {
                let local_ty = self.local_ty(func, place.local);
                let mut current_ty = local_ty;
                for proj in &place.projections {
                    match proj {
                        arandu_middle::amir::AmirProjection::Deref => {
                            current_ty = match &current_ty {
                                ArType::Ptr(inner)
                                | ArType::Ref(inner)
                                | ArType::RefMut(inner)
                                | ArType::Nullable(inner) => self.interner.resolve(*inner),
                                other => other.clone(),
                            };
                        }
                        arandu_middle::amir::AmirProjection::Field(field_sym) => {
                            let struct_ty = match &current_ty {
                                ArType::Ptr(inner)
                                | ArType::Ref(inner)
                                | ArType::RefMut(inner)
                                | ArType::Nullable(inner) => self.interner.resolve(*inner),
                                other => other.clone(),
                            };
                            let field_name = self
                                .symbols
                                .get(*field_sym)
                                .name
                                .rsplit('.')
                                .next()
                                .unwrap_or("");
                            current_ty = self.instantiated_field_ty(&struct_ty, field_name);
                        }
                        arandu_middle::amir::AmirProjection::Index(_) => {}
                    }
                }
                if matches!(current_ty, ArType::Primitive(Primitive::Str)) {
                    let value = self.format_place(place, func);
                    let _ = writeln!(
                        &mut self.output,
                        "    if ({value}.ptr) {{ free((void*){value}.ptr); {value}.ptr = NULL; }}"
                    );
                } else {
                    let ty_id = self.interner.intern(current_ty.clone());
                    if let Some((_, destructor)) = self.gen_drop_glue(ty_id, &current_ty) {
                        let destructor_symbol = self.symbols.get(destructor);
                        let destructor =
                            super::sanitize_c_ident(self.symbols.host_func_name(destructor_symbol));
                        let value = self.format_place(place, func);
                        let _ = writeln!(&mut self.output, "    {destructor}({value});");
                    }
                }
            }
            AmirStmt::Nop => {}
        }
    }

    fn emit_gen_write_assign(
        &mut self,
        lhs: TempId,
        handle: Option<&AmirOperand>,
        value: &AmirOperand,
        payload_ty: arandu_middle::types::TypeId,
        func: &AmirFunc,
        upsert: bool,
    ) {
        let payload_ar = self.interner.resolve(payload_ty);
        let drop_glue = self
            .gen_drop_glue(payload_ty, &payload_ar)
            .map(|(name, _)| name);
        if !self
            .provider
            .is_copy_type(payload_ty)
            .unwrap_or_else(|| payload_ar.is_copy_v01())
            && drop_glue.is_none()
        {
            self.record_codegen_ice(
                func,
                "non-Copy GenRef payload has no explicit @Destructor contract",
            );
            let _ = writeln!(&mut self.output, "    t{} = 0;", lhs.as_usize());
            return;
        }
        let payload_c = self.format_type(&payload_ar);
        let layout = self.checked_layout(&payload_ar);
        let value = self.format_operand(value, func);
        let glue = drop_glue.as_deref().unwrap_or("NULL");
        let slot = self.next_co_stack_slot();
        let _ = writeln!(
            &mut self.output,
            "    {payload_c} __ar_gen_payload_{slot} = ({payload_c})({value});"
        );
        match handle {
            None => {
                let _ = writeln!(
                    &mut self.output,
                    "    t{} = (int64_t)ar_gen_insert_raw(&__ar_gen_payload_{slot}, {}, {}, {glue});",
                    lhs.as_usize(),
                    layout.size,
                    layout.align
                );
            }
            Some(handle) if upsert => {
                let handle = self.format_operand(handle, func);
                let _ = writeln!(
                    &mut self.output,
                    "    t{} = (int64_t)ar_gen_upsert_raw((uint64_t)({handle}), &__ar_gen_payload_{slot}, {}, {}, {glue});",
                    lhs.as_usize(),
                    layout.size,
                    layout.align
                );
            }
            Some(handle) => {
                let handle = self.format_operand(handle, func);
                let _ = writeln!(
                    &mut self.output,
                    "    if (!ar_gen_set_raw((uint64_t)({handle}), &__ar_gen_payload_{slot}, {}, {}, {glue})) abort();",
                    layout.size, layout.align
                );
                let _ = writeln!(
                    &mut self.output,
                    "    t{} = (int64_t)({handle});",
                    lhs.as_usize()
                );
            }
        }
        let _ = writeln!(
            &mut self.output,
            "    if (t{} == 0) abort();",
            lhs.as_usize()
        );
    }

    fn emit_gen_read_assign(
        &mut self,
        lhs: TempId,
        name: &str,
        handle: &AmirOperand,
        payload_ty: arandu_middle::types::TypeId,
        func: &AmirFunc,
    ) {
        let payload_ar = self.interner.resolve(payload_ty);
        if !self
            .provider
            .is_copy_type(payload_ty)
            .unwrap_or_else(|| payload_ar.is_copy_v01())
            && self.gen_drop_glue(payload_ty, &payload_ar).is_none()
        {
            self.record_codegen_ice(
                func,
                "non-Copy GenRef payload has no explicit @Destructor contract",
            );
            return;
        }
        let layout = self.checked_layout(&payload_ar);
        let handle = self.format_operand(handle, func);
        let _ = writeln!(
            &mut self.output,
            "    if (!{name}((uint64_t)({handle}), &t{}, {}, {})) abort();",
            lhs.as_usize(),
            layout.size,
            layout.align
        );
    }

    pub(super) fn emit_terminator(&mut self, term: &AmirTerminator, func: &AmirFunc) {
        match term {
            AmirTerminator::Return => {
                let name = super::sanitize_c_ident(&self.symbols.get(func.symbol).name);
                let ret = self.interner.resolve(func.return_type);
                if name == "main" {
                    let _ = writeln!(&mut self.output, "    ar_gen_shutdown_raw();");
                    // ISO C requires `int main`; void Arandu main becomes `return 0`.
                    if matches!(ret, ArType::Void) {
                        let _ = writeln!(&mut self.output, "    return 0;");
                    } else {
                        let _ = writeln!(&mut self.output, "    return (int)t0;");
                    }
                } else if matches!(ret, ArType::Void) {
                    let _ = writeln!(&mut self.output, "    return;");
                } else {
                    let _ = writeln!(&mut self.output, "    return t0;");
                }
            }
            AmirTerminator::Goto { target, args } => {
                self.emit_block_arguments(*target, args, func, "    ");
                let _ = writeln!(&mut self.output, "    goto bb{};", target.as_usize());
            }
            // A3.1 ready-only: suspend = jump to resume (await load is in resume BB).
            AmirTerminator::Suspend {
                future: _,
                resume,
                args,
            } => {
                self.emit_block_arguments(*resume, args, func, "    ");
                let _ = writeln!(&mut self.output, "    goto bb{};", resume.as_usize());
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
            } => {
                let cond_str = self.format_operand(condition, func);
                let _ = writeln!(&mut self.output, "    if ({}) {{", cond_str);
                self.emit_block_arguments(*if_true, true_args, func, "        ");
                let _ = writeln!(&mut self.output, "        goto bb{};", if_true.as_usize());
                let _ = writeln!(&mut self.output, "    }} else {{");
                self.emit_block_arguments(*if_false, false_args, func, "        ");
                let _ = writeln!(&mut self.output, "        goto bb{};", if_false.as_usize());
                let _ = writeln!(&mut self.output, "    }}");
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                let discr_str = self.format_operand(discriminant, func);
                let _ = writeln!(&mut self.output, "    switch ({}) {{", discr_str);
                for (val, target, args) in targets.iter() {
                    let _ = writeln!(&mut self.output, "        case {}:", val);
                    self.emit_block_arguments(*target, args, func, "            ");
                    let _ = writeln!(
                        &mut self.output,
                        "            goto bb{};",
                        target.as_usize()
                    );
                }
                let _ = writeln!(&mut self.output, "        default:");
                self.emit_block_arguments(otherwise.0, &otherwise.1, func, "            ");
                let _ = writeln!(
                    &mut self.output,
                    "            goto bb{};",
                    otherwise.0.as_usize()
                );
                let _ = writeln!(&mut self.output, "    }}");
            }
            AmirTerminator::Unreachable => {
                let _ = writeln!(&mut self.output, "    AR_UNREACHABLE();");
            }
        }
    }

    pub(super) fn emit_block_arguments(
        &mut self,
        target: arandu_middle::amir::BlockId,
        args: &[AmirOperand],
        func: &AmirFunc,
        indent: &str,
    ) {
        let target_block = &func.blocks[target.as_usize()];
        let target_params = func.block_params(target_block.params);
        for (param, arg) in target_params.iter().zip(args.iter()) {
            let arg_str = self.format_operand(arg, func);
            let _ = writeln!(
                &mut self.output,
                "{}t{} = {};",
                indent,
                param.id.as_usize(),
                arg_str
            );
        }
    }
}
