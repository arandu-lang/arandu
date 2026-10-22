use super::super::{FuncEmitter, TypedValue, ice, local_name, parse_byte_aggregate_size};
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
                    self.writer.line(format!(
                        "{} = extractvalue {aggregate} {}, 1",
                        result, value.value
                    ));
                    Ok(())
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
                    self.writer
                        .line(format!("{slot} = alloca {}, align {align}", value.ty));
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
