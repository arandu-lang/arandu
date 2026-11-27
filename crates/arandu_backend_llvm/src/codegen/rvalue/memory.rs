use super::super::{
    FuncEmitter, TypedValue, ice, local_name, parse_byte_aggregate_size, quote_name,
};
use arandu_middle::Diagnostic;
use arandu_middle::amir::AmirRvalue;
use arandu_middle::types::ArType;

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_memory_rvalue(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
    ) -> Result<(), Diagnostic> {
        let fat = self.module.fat_ptr_llvm_ty();
        match rvalue {
            AmirRvalue::Load(place) => {
                let (address, place_ty) = self.place(place)?;
                let loaded_ty = self.module.llvm_ty(&place_ty)?;
                let align = self.module.align_of(&place_ty);
                let raw = self.new_value();
                self.writer.line(format!(
                    "{raw} = load {loaded_ty}, ptr {address}, align {align}"
                ));
                self.assign_coerced(
                    result,
                    self.typed_from_ar(loaded_ty, raw, &place_ty),
                    lhs_ty,
                )
            }
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                let (address, _) = self.place(place)?;
                self.writer.line(format!(
                    "{} = getelementptr i8, ptr {address}, i64 0",
                    result
                ));
                Ok(())
            }
            AmirRvalue::RelativeBorrow { local, .. } => {
                self.writer.line(format!(
                    "{} = getelementptr i8, ptr {}, i64 0",
                    result,
                    local_name(*local)
                ));
                Ok(())
            }
            AmirRvalue::Alloc(size) => {
                let size_ty = self.module.operand_type(size, self.func);
                let size = self.operand(size, &size_ty)?;
                let size = self.coerce(size, &self.module.usize_ty())?;
                self.module.require_raw_malloc();
                self.writer.line(format!(
                    "{} = call ptr @ar_rt_raw_malloc({})",
                    result,
                    size.render()
                ));
                Ok(())
            }
            AmirRvalue::ArenaAlloc { size, align, .. } => {
                self.module.require_task_arena_alloc();
                let len_ty = self.module.ptr_int_ty();
                let alloc_size = (*size).max(1);
                let alloc_align = (*align).max(1).next_power_of_two();
                let has_suspend = self.func.blocks.iter().any(|block| {
                    matches!(
                        block.terminator,
                        arandu_middle::amir::AmirTerminator::Suspend { .. }
                    )
                });
                if has_suspend {
                    self.writer.line(format!(
                        "{result} = call ptr @ar_task_arena_alloc({len_ty} {alloc_size}, {len_ty} {alloc_align})"
                    ));
                    return Ok(());
                }
                // Inline bump-pointer fast path: loads thread-local `[cur, end]`
                // once (hoistable by LLVM LICM) and only calls `ar_task_arena_alloc`
                // on the cold slab-exhaustion path.
                self.module.require_task_arena_fast_state();
                let ptr_bytes = u64::from(self.module.layout.pointer_width());
                let mask = alloc_align - 1;
                let state = self.new_value();
                let cur = self.new_value();
                let end_ptr = self.new_value();
                let end = self.new_value();
                let aligned = if mask == 0 {
                    cur.clone()
                } else {
                    let bumped = self.new_value();
                    let masked = self.new_value();
                    self.writer
                        .line(format!("{state} = call ptr @ar_task_arena_fast_state()"));
                    self.writer.line(format!(
                        "{cur} = load {len_ty}, ptr {state}, align {ptr_bytes}"
                    ));
                    self.writer.line(format!(
                        "{end_ptr} = getelementptr inbounds i8, ptr {state}, {len_ty} {ptr_bytes}"
                    ));
                    self.writer.line(format!(
                        "{end} = load {len_ty}, ptr {end_ptr}, align {ptr_bytes}"
                    ));
                    self.writer
                        .line(format!("{bumped} = add {len_ty} {cur}, {mask}"));
                    self.writer.line(format!(
                        "{masked} = and {len_ty} {bumped}, -{}",
                        alloc_align
                    ));
                    masked
                };
                if mask == 0 {
                    self.writer
                        .line(format!("{state} = call ptr @ar_task_arena_fast_state()"));
                    self.writer.line(format!(
                        "{cur} = load {len_ty}, ptr {state}, align {ptr_bytes}"
                    ));
                    self.writer.line(format!(
                        "{end_ptr} = getelementptr inbounds i8, ptr {state}, {len_ty} {ptr_bytes}"
                    ));
                    self.writer.line(format!(
                        "{end} = load {len_ty}, ptr {end_ptr}, align {ptr_bytes}"
                    ));
                }
                let next = self.new_value();
                let ok1 = self.new_value();
                let ok2 = self.new_value();
                let ok3 = self.new_value();
                let ok12 = self.new_value();
                let ok = self.new_value();
                self.writer
                    .line(format!("{next} = add {len_ty} {aligned}, {alloc_size}"));
                self.writer
                    .line(format!("{ok1} = icmp uge {len_ty} {aligned}, {cur}"));
                self.writer
                    .line(format!("{ok2} = icmp uge {len_ty} {next}, {aligned}"));
                self.writer
                    .line(format!("{ok3} = icmp ule {len_ty} {next}, {end}"));
                self.writer.line(format!("{ok12} = and i1 {ok1}, {ok2}"));
                self.writer.line(format!("{ok} = and i1 {ok12}, {ok3}"));

                let fast_bb = self.new_block();
                let slow_bb = self.new_block();
                let join_bb = self.new_block();
                self.writer
                    .line(format!("br i1 {ok}, label %{fast_bb}, label %{slow_bb}"));

                self.enter_llvm_label(&fast_bb);
                self.writer.line(format!(
                    "store {len_ty} {next}, ptr {state}, align {ptr_bytes}"
                ));
                let fast_ptr = self.new_value();
                self.writer
                    .line(format!("{fast_ptr} = inttoptr {len_ty} {aligned} to ptr"));
                self.writer.line(format!("br label %{join_bb}"));

                self.enter_llvm_label(&slow_bb);
                let slow_ptr = self.new_value();
                self.writer.line(format!(
                    "{slow_ptr} = call ptr @ar_task_arena_alloc({len_ty} {alloc_size}, {len_ty} {alloc_align})"
                ));
                self.writer.line(format!("br label %{join_bb}"));

                self.enter_llvm_label(&join_bb);
                self.writer.line(format!(
                    "{result} = phi ptr [ {fast_ptr}, %{fast_bb} ], [ {slow_ptr}, %{slow_bb} ]"
                ));
                Ok(())
            }
            AmirRvalue::StaticAddr(symbol) => {
                let Some(item) = self
                    .module
                    .program
                    .statics
                    .iter()
                    .find(|item| item.symbol == *symbol)
                else {
                    return Err(ice("`static mut` storage is not in the program"));
                };
                let address = TypedValue::new("ptr", format!("@{}", quote_name(&item.name)));
                self.assign_coerced(result, address, lhs_ty)
            }
            AmirRvalue::ArenaCheckpoint => {
                self.module.require_task_arena_checkpoint();
                self.writer
                    .line(format!("{result} = call i64 @ar_task_arena_checkpoint()"));
                Ok(())
            }
            AmirRvalue::Len(value) => {
                let ty = self.module.operand_type(value, self.func);
                if let ArType::Array(length, _) = &ty {
                    let value = length.to_string();
                    return self.assign_coerced(
                        result,
                        TypedValue::new(self.module.llvm_ty(lhs_ty)?, value),
                        lhs_ty,
                    );
                }
                let value = self.operand(value, &ty)?;
                let aggregate = self.module.llvm_ty(&ty)?;
                if aggregate == fat {
                    // The length is pointer-sized; the destination may be a
                    // narrower integer (`for` counts with `int`).
                    let length = self.new_value();
                    self.writer.line(format!(
                        "{length} = extractvalue {aggregate} {}, 1",
                        value.value
                    ));
                    let len_ty = self.module.ptr_int_ty();
                    self.assign_coerced(result, TypedValue::new(len_ty.to_owned(), length), lhs_ty)
                } else if value.ty == "ptr" {
                    // `ref String` / `ref str` / `ref []T`: length sits at
                    // pointer-width offset in the shared fat-descriptor layout.
                    let len_addr = self.byte_gep(&value.value, self.module.layout.pointer_width());
                    let loaded = self.new_value();
                    let align = self.module.pointer_align();
                    let len_ty = self.module.ptr_int_ty();
                    self.writer.line(format!(
                        "{loaded} = load {len_ty}, ptr {len_addr}, align {align}"
                    ));
                    self.assign_coerced(result, TypedValue::new(len_ty.to_owned(), loaded), lhs_ty)
                } else {
                    Err(ice(format!(
                        "LLVM lowering does not implement len for {ty:?}"
                    )))
                }
            }
            AmirRvalue::StrView { owner } => {
                // Intrinsic returns `ref str`: a thin pointer to a `{data,len}`
                // descriptor. Owned String/Path layouts share that prefix, so a
                // `ref Owner` is already a valid `ref str` (Cranelift pass-through).
                let ty = self.module.operand_type(owner, self.func);
                let value = self.operand(owner, &ty)?;
                if value.ty == "ptr" || self.module.is_pointer(&ty) {
                    return self.assign_coerced(
                        result,
                        TypedValue::new("ptr".to_owned(), value.value),
                        lhs_ty,
                    );
                }
                if value.ty == fat || parse_byte_aggregate_size(&value.ty).is_some() {
                    // By-value descriptor / opaque String: spill and return its address.
                    let slot = self.new_value();
                    let align = self.module.pointer_align();
                    self.emit_entry_alloca(&slot, &value.ty, align);
                    self.writer.line(format!(
                        "store {} {}, ptr {slot}, align {align}",
                        value.ty, value.value
                    ));
                    return self.assign_coerced(
                        result,
                        TypedValue::new("ptr".to_owned(), slot),
                        lhs_ty,
                    );
                }
                Err(ice(format!(
                    "LLVM lowering does not implement strView for {ty:?}"
                )))
            }
            AmirRvalue::SliceView { data, len, .. } => {
                let pointer_ty = self.module.void_ptr_ty();
                let data = self.operand(data, &pointer_ty)?;
                let length_ty = self.module.usize_ty();
                let len = self.operand(len, &length_ty)?;
                let len = self.coerce(len, &self.module.usize_ty())?;
                let first = self.new_value();
                self.writer.line(format!(
                    "{first} = insertvalue {fat} undef, ptr {}, 0",
                    data.value
                ));
                self.writer.line(format!(
                    "{result} = insertvalue {fat} {first}, {} {}, 1",
                    len.ty, len.value
                ));
                Ok(())
            }
            AmirRvalue::SliceData(slice) => {
                let slice_ty = self.module.operand_type(slice, self.func);
                let slice = self.operand(slice, &slice_ty)?;
                self.writer
                    .line(format!("{result} = extractvalue {fat} {}, 0", slice.value));
                Ok(())
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                let slice_ty = self.module.operand_type(slice, self.func);
                let slice = self.operand(slice, &slice_ty)?;
                let pointer = self.new_value();
                self.writer
                    .line(format!("{pointer} = extractvalue {fat} {}, 0", slice.value));
                let start_ty = self.module.usize_ty();
                let start = self.operand(start, &start_ty)?;
                let start = self.coerce(start, &self.module.usize_ty())?;
                let index_ll = start.ty.clone();
                let element_ty = match &slice_ty {
                    ArType::Slice(inner) | ArType::MutSlice(inner) => {
                        self.module.interner.resolve(*inner)
                    }
                    other => {
                        return Err(ice(format!(
                            "slice subslice requires a slice, got {other:?}"
                        )));
                    }
                };
                let stride = self
                    .module
                    .layout
                    .layout_of_type(&element_ty, self.module.interner, self.module.info)
                    .map_err(|error| {
                        ice(format!("LLVM layout failed for {element_ty:?}: {error}"))
                    })?
                    .size;
                let offset = self.new_value();
                self.writer.line(format!(
                    "{offset} = mul {index_ll} {}, {stride}",
                    start.value
                ));
                let data = self.new_value();
                self.writer.line(format!(
                    "{data} = getelementptr inbounds i8, ptr {pointer}, {index_ll} {offset}"
                ));
                let length_ty = self.module.usize_ty();
                let len = self.operand(len, &length_ty)?;
                let len = self.coerce(len, &self.module.usize_ty())?;
                let first = self.new_value();
                self.writer
                    .line(format!("{first} = insertvalue {fat} undef, ptr {data}, 0"));
                self.writer.line(format!(
                    "{result} = insertvalue {fat} {first}, {} {}, 1",
                    len.ty, len.value
                ));
                Ok(())
            }
            AmirRvalue::StrBytes { source } => {
                let source_ty = self.module.operand_type(source, self.func);
                let source = self.operand(source, &source_ty)?;
                self.assign_coerced(result, source, lhs_ty)
            }
            _ => unreachable!(),
        }
    }
}
