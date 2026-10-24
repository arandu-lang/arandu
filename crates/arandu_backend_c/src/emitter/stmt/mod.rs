use super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirStmt, AmirTerminator};
use arandu_middle::types::{ArType, Primitive};
use std::fmt::Write;

mod r#gen;
mod intrinsic;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_stmt(&mut self, stmt: &AmirStmt, func: &AmirFunc) {
        match stmt {
            AmirStmt::Assign { lhs, rhs } => {
                if self.rotation_elided[lhs.as_usize()] {
                    return;
                }
                if let Some(rotation) = self.rotations[lhs.as_usize()] {
                    let value = self.format_operand(&rotation.value, func);
                    let expression =
                        format!("ar_rotr{}({value}, {})", rotation.width, rotation.count);
                    if self.expression_candidates[lhs.as_usize()] {
                        self.temp_expressions[lhs.as_usize()] = Some(expression);
                    } else {
                        let _ = writeln!(
                            self.output,
                            "    t{} = {expression};",
                            self.temp_slots[lhs.as_usize()]
                        );
                    }
                    return;
                }
                if self.emit_integer_concat_statement(*lhs, rhs, func) {
                    return;
                }
                let lhs_ty = self.temp_ty(func, *lhs);
                if matches!(lhs_ty, ArType::Void) {
                    return;
                }
                let lhs_c_ty = self.format_type(&lhs_ty);
                if self.expression_candidates[lhs.as_usize()] {
                    // Reuse the canonical expression emitter and retain the
                    // assignment conversion (notably fixed-width narrowing).
                    let surrounding = std::mem::take(&mut self.output);
                    self.emit_rvalue(rhs, func, &lhs_ty, &lhs_c_ty);
                    let expression = std::mem::replace(&mut self.output, surrounding);
                    self.temp_expressions[lhs.as_usize()] =
                        Some(format!("(({lhs_c_ty})({expression}))"));
                    return;
                }
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
                    // `ar_co_poll_i64`/`ar_co_await_i64` read an 8-byte payload word.
                    let payload_size = self.checked_layout(&payload_ar).size.max(8);
                    let size = 8 + payload_size;
                    let _ = writeln!(
                        &mut self.output,
                        "    uint8_t __ar_co_{slot}[{size}] __attribute__((aligned(8))) = {{0}};"
                    );
                    let _ = writeln!(&mut self.output, "    *(uint32_t*)__ar_co_{slot} = 0;");
                    let _ = writeln!(
                        &mut self.output,
                        "    *({payload_c}*)(__ar_co_{slot} + 8) = ({payload_c})({v});"
                    );
                    let _ = writeln!(
                        &mut self.output,
                        "    t{} = (void*)__ar_co_{slot};",
                        self.temp_slots[lhs.as_usize()]
                    );
                    return;
                }
                if let Some(source) = self
                    .current_initializer
                    .and_then(|id| self.static_sources.get(&(func.symbol, id)))
                {
                    let _ = writeln!(
                        self.output,
                        "    memcpy(&t{}, __ar_static_{source}, sizeof(t{}));",
                        self.temp_slots[lhs.as_usize()],
                        self.temp_slots[lhs.as_usize()]
                    );
                    return;
                }
                let _ = write!(
                    &mut self.output,
                    "    t{} = ",
                    self.temp_slots[lhs.as_usize()]
                );
                self.emit_rvalue(rhs, func, &lhs_ty, &lhs_c_ty);
                let _ = writeln!(&mut self.output, ";");
            }
            AmirStmt::Store { lhs, rhs } => {
                if lhs.projections.is_empty()
                    && matches!(self.local_ty(func, lhs.local), ArType::Void)
                {
                    return;
                }
                let lhs_str = self.format_place(lhs, func);
                let rhs_str = self.format_operand(rhs, func);
                if lhs.projections.is_empty()
                    && self.is_str_ref_to_str(
                        &self.operand_ty(func, rhs),
                        &self.local_ty(func, lhs.local),
                    )
                {
                    let _ = writeln!(&mut self.output, "    {lhs_str} = *({rhs_str});");
                } else {
                    let _ = writeln!(&mut self.output, "    {} = {};", lhs_str, rhs_str);
                }
            }
            AmirStmt::Call {
                lhs, callee, args, ..
            } => {
                if self.try_emit_mem_intrinsic(lhs, callee, args, func) {
                    return;
                }
                if let AmirOperand::FunctionRef(symbol) = callee
                    && self.symbols.get(*symbol).kind
                        == arandu_middle::symbol_table::SymbolKind::ExternFunc
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
                            self.temp_slots[dest.as_usize()],
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
                if let AmirOperand::FunctionRef(symbol) = callee
                    && self.symbols.get(*symbol).kind == arandu_middle::SymbolKind::ExternFunc
                    && self
                        .symbols
                        .get(*symbol)
                        .name
                        .rsplit('.')
                        .next()
                        .is_some_and(|bare| bare == "ar_rt_copy_value")
                    && let [dest, source, size] = args.as_slice()
                {
                    let dest_str = self.format_operand(dest, func);
                    let source_str = self.format_operand(source, func);
                    let size_str = self.format_operand(size, func);
                    let source_ty = self.operand_ty(func, source);
                    let source_ptr = if source_ty.is_borrowed_slice_abi(self.interner) {
                        format!("(const void*)&({source_str})")
                    } else {
                        format!("(const void*)({source_str})")
                    };
                    let _ = writeln!(
                        &mut self.output,
                        "    ar_rt_copy_value((void*)({dest_str}), {source_ptr}, (size_t)({size_str}));"
                    );
                    return;
                }
                let callee_str = self.format_operand(callee, func);
                let callee_func = if let AmirOperand::FunctionRef(symbol) = callee {
                    self.program
                        .funcs
                        .iter()
                        .find(|candidate| candidate.symbol == *symbol)
                } else {
                    None
                };
                let external_params = if let AmirOperand::FunctionRef(symbol) = callee {
                    self.program
                        .extern_funcs
                        .get(symbol)
                        .map(|(params, _)| params)
                } else {
                    None
                };
                let args_str: Vec<_> = args
                    .iter()
                    .enumerate()
                    .filter_map(|(index, arg)| {
                        let ty = callee_func
                            .and_then(|callee| {
                                callee
                                    .params
                                    .get(index)
                                    .map(|param| self.temp_ty(callee, *param))
                            })
                            .or_else(|| {
                                external_params
                                    .and_then(|params| params.get(index))
                                    .cloned()
                            })
                            .unwrap_or_else(|| self.operand_ty(func, arg));
                        // Nil also represents nullable values; only omit a bare nil
                        // when the callee's declared parameter is actually void.
                        let empty = matches!(ty, ArType::Void)
                            && (callee_func.is_some()
                                || external_params.is_some()
                                || matches!(arg, AmirOperand::Copy(_) | AmirOperand::Move(_)));
                        (!empty).then(|| self.format_operand(arg, func))
                    })
                    .collect();
                if let Some(dest) =
                    lhs.filter(|dest| !matches!(self.temp_ty(func, *dest), ArType::Void))
                {
                    let _ = write!(
                        &mut self.output,
                        "    t{} = ",
                        self.temp_slots[dest.as_usize()]
                    );
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
                if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = op
                    && self.integer_concat_temps[temp.as_usize()].is_some()
                {
                    return;
                }
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
                        arandu_middle::amir::AmirProjection::Variant(_) => {}
                        arandu_middle::amir::AmirProjection::Payload { field_ty, .. } => {
                            current_ty = self.interner.resolve(*field_ty);
                        }
                        arandu_middle::amir::AmirProjection::TupleField(index) => {
                            current_ty = match &current_ty {
                                ArType::Tuple(args) => self
                                    .interner
                                    .type_args(*args)
                                    .get(*index)
                                    .copied()
                                    .map(|field_ty| self.interner.resolve(field_ty))
                                    .unwrap_or(ArType::Error),
                                _ => ArType::Error,
                            };
                        }
                        arandu_middle::amir::AmirProjection::Index(_)
                        | arandu_middle::amir::AmirProjection::IndexConstant(_) => {
                            current_ty = match &current_ty {
                                ArType::Array(_, inner)
                                | ArType::Slice(inner)
                                | ArType::MutSlice(inner)
                                | ArType::ConstArray(_, inner) => self.interner.resolve(*inner),
                                other => other.clone(),
                            };
                        }
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

    pub(super) fn emit_terminator(&mut self, term: &AmirTerminator, func: &AmirFunc) {
        match term {
            AmirTerminator::Return => {
                let name = super::sanitize_c_ident(&self.symbols.get(func.symbol).name);
                let ret = self.interner.resolve(func.return_type);
                if name == "main" {
                    if self.arena_runtime {
                        let _ = writeln!(&mut self.output, "    ar_gen_shutdown_raw();");
                    }
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
        if args.is_empty() {
            return;
        }
        // SSA edges perform simultaneous assignments. Snapshot every input
        // before writing any destination, including fat values and structs.
        // The edge-local scope makes names unique across different branches.
        let _ = writeln!(&mut self.output, "{indent}{{");
        for (index, (param, arg)) in target_params.iter().zip(args).enumerate() {
            let ty = self.interner.resolve(param.ty);
            if matches!(ty, ArType::Void) {
                continue;
            }
            let c_ty = self.format_type(&ty);
            let arg_str = self.format_operand(arg, func);
            let _ = writeln!(
                &mut self.output,
                "{indent}    {c_ty} ar_edge_{index} = {arg_str};"
            );
        }
        for (index, param) in target_params.iter().take(args.len()).enumerate() {
            if matches!(self.interner.resolve(param.ty), ArType::Void) {
                continue;
            }
            let _ = writeln!(
                &mut self.output,
                "{indent}    t{} = ar_edge_{index};",
                self.temp_slots[param.id.as_usize()]
            );
        }
        let _ = writeln!(&mut self.output, "{indent}}}");
    }
}
