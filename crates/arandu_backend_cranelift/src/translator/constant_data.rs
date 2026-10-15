//! Read-only sources from the shared bounded AMIR serializer.
//! Runtime values always retain independent writable backing.
use super::FunctionTranslator;
use cranelift_codegen::ir::{InstBuilder, Value};

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn initialize_aggregate_from_rodata(&mut self, destination: Value) -> bool {
        let Some(statement) = self.current_initializer else {
            return false;
        };
        let Some(initializer) = self.static_initializers.remove(&statement) else {
            return false;
        };
        let Ok(size) = u64::try_from(initializer.bytes.len()) else {
            return false;
        };
        let mut hash = blake3::Hasher::new();
        hash.update(b"arandu-static-aggregate/v1");
        hash.update(&initializer.alignment.to_le_bytes());
        hash.update(&initializer.bytes);
        let name = format!("__ar_static_{}", hash.finalize().to_hex());
        let existing = self.module.declarations().get_name(&name);
        let id =
            match self
                .module
                .declare_data(&name, cranelift_module::Linkage::Local, false, false)
            {
                Ok(id) => id,
                Err(error) => {
                    self.record_ice(
                        format!("failed to declare aggregate constant data: {error}"),
                        self.func_span(),
                    );
                    return true;
                }
            };
        if existing.is_none() {
            let mut data = cranelift_module::DataDescription::new();
            data.define(initializer.bytes.into_boxed_slice());
            data.set_align(initializer.alignment);
            if let Err(error) = self.module.define_data(id, &data) {
                self.record_ice(
                    format!("failed to define aggregate constant data: {error}"),
                    self.func_span(),
                );
                return true;
            }
        }
        let reference = self.module.declare_data_in_func(id, self.builder.func);
        let source = self.builder.ins().symbol_value(self.ptr_type, reference);
        self.copy_aggregate_bytes(destination, source, size);
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn native_object_keeps_scalar_initializers_in_read_only_data() {
        use cranelift_object::object::{Object, ObjectSection, SectionKind};
        let ast = arandu_parser::parse(
            "func main(): int { let a: [8]i32 = [1,2,3,4,5,6,7,8]; return a[1] as int }",
        )
        .unwrap();
        let mut checked = arandu_semantics::type_check(
            arandu_semantics::resolve_for_test(0, &ast),
            &ast,
            arandu_semantics::TargetInfo { pointer_width: 64 },
        );
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let hir = arandu_semantics::lower_to_hir(&mut checked, &ast).unwrap();
        let (program, _) =
            arandu_semantics::lower_to_amir_with_interfaces(&mut checked, &hir, 8).unwrap();
        let artifact = crate::aot::CraneliftObjectBackend::host_baseline()
            .unwrap()
            .compile(&program, &checked.symbols, &checked.type_info)
            .unwrap();
        let object = cranelift_object::object::File::parse(artifact.bytes()).unwrap();
        let expected: Vec<u8> = (1_i32..=8)
            .flat_map(|value| {
                if object.is_little_endian() {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                }
            })
            .collect();
        assert!(
            object
                .sections()
                .any(|section| section.kind() == SectionKind::ReadOnlyData
                    && section.data().is_ok_and(|bytes| bytes
                        .windows(expected.len())
                        .any(|window| window == expected)))
        );
    }
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn equal_aggregate_sources_are_shared_between_functions() {
        use cranelift_object::object::{Object, ObjectSection, SectionKind};
        let ast = arandu_parser::parse("func first(): int { let mut a: [8]i32 = [1,2,3,4,5,6,7,8]; a[0] = 42; return a[0] as int }\nfunc second(): int { let a: [8]i32 = [1,2,3,4,5,6,7,8]; return a[0] as int }\nfunc main(): int { return first() + second() }").unwrap();
        let mut checked = arandu_semantics::type_check(
            arandu_semantics::resolve_for_test(0, &ast),
            &ast,
            arandu_semantics::TargetInfo { pointer_width: 64 },
        );
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let hir = arandu_semantics::lower_to_hir(&mut checked, &ast).unwrap();
        let (program, _) =
            arandu_semantics::lower_to_amir_with_interfaces(&mut checked, &hir, 8).unwrap();
        let artifact = crate::aot::CraneliftObjectBackend::host_baseline()
            .unwrap()
            .compile(&program, &checked.symbols, &checked.type_info)
            .unwrap();
        let object = cranelift_object::object::File::parse(artifact.bytes()).unwrap();
        let expected: Vec<_> = (1_i32..=8)
            .flat_map(|value| {
                if object.is_little_endian() {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                }
            })
            .collect();
        let copies: usize = object
            .sections()
            .filter(|section| section.kind() == SectionKind::ReadOnlyData)
            .map(|section| {
                section
                    .data()
                    .unwrap()
                    .windows(expected.len())
                    .filter(|window| *window == expected)
                    .count()
            })
            .sum();
        assert_eq!(copies, 1);
    }
}
