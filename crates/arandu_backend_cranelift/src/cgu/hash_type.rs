use super::HashContext;
use arandu_middle::SymbolId;
use arandu_middle::types::{ArType, TypeId};

impl HashContext<'_> {
    pub(super) fn symbol(&mut self, id: SymbolId) {
        if let Some(symbol) = self.symbols.try_get(id) {
            self.hash.tag(0);
            self.hash.str(self.symbols.host_func_name(symbol));
            self.hash.str(&symbol.name);
        } else {
            // Invalid semantic metadata must not crash codegen or collide with a
            // valid symbol. The numeric identity gives the diagnostic path a
            // deterministic cache miss until validation reports the ICE.
            self.hash.tag(1);
            self.hash.u32(id.file_id);
            self.hash.u32(id.local_id.0);
        }
    }

    pub(super) fn type_id(&mut self, id: TypeId) {
        let Some(ty) = self.type_info.type_interner().try_resolve(id) else {
            self.hash.tag(u8::MAX);
            self.hash.usize(id.as_usize());
            return;
        };
        self.ar_type(&ty);
        if let Some(destructor) = self.type_info.destructor_for_type(id) {
            self.hash.tag(1);
            self.symbol(destructor);
        } else {
            self.hash.tag(0);
        }
    }

    pub(super) fn type_args(&mut self, range: arandu_middle::hir::pool::IndexRange) {
        self.hash.u32(range.len);
        for index in 0..range.len as usize {
            match self.type_info.type_interner().type_arg(range, index) {
                Some(id) => {
                    self.hash.tag(0);
                    self.type_id(id);
                }
                None => self.hash.tag(1),
            }
        }
    }

    pub(super) fn ar_type(&mut self, ty: &ArType) {
        match ty {
            ArType::Vector(vector) => {
                self.hash.tag(25);
                self.hash.str(vector.name());
            }
            ArType::Primitive(primitive) => {
                self.hash.tag(0);
                self.hash.str(primitive.as_str());
            }
            ArType::Named(symbol, args) => {
                self.hash.tag(1);
                self.symbol(*symbol);
                self.type_args(*args);
                self.named_layout(*symbol, &self.type_info.type_interner().type_args(*args));
            }
            ArType::Func(params, result) => {
                self.hash.tag(2);
                self.type_args(*params);
                self.type_id(*result);
            }
            ArType::Nullable(inner) => {
                self.hash.tag(3);
                self.type_id(*inner);
            }
            ArType::Slice(inner) => {
                self.hash.tag(4);
                self.type_id(*inner);
            }
            ArType::MutSlice(inner) => {
                self.hash.tag(25);
                self.type_id(*inner);
            }
            ArType::Array(len, inner) => {
                self.hash.tag(5);
                self.hash.u64(*len);
                self.type_id(*inner);
            }
            ArType::Ptr(inner) => {
                self.hash.tag(6);
                self.type_id(*inner);
            }
            ArType::Ref(inner) => {
                self.hash.tag(7);
                self.type_id(*inner);
            }
            ArType::RefMut(inner) => {
                self.hash.tag(8);
                self.type_id(*inner);
            }
            ArType::GenRef => self.hash.tag(9),
            ArType::Tuple(items) => {
                self.hash.tag(10);
                self.type_args(*items);
            }
            ArType::Result(ok, error) => {
                self.hash.tag(11);
                self.type_id(*ok);
                self.type_id(*error);
            }
            ArType::Option(inner) => {
                self.hash.tag(12);
                self.type_id(*inner);
            }
            ArType::Coroutine(inner) => {
                self.hash.tag(13);
                self.type_id(*inner);
            }
            ArType::Poll(inner) => {
                self.hash.tag(14);
                self.type_id(*inner);
            }
            ArType::Range(inner) => {
                self.hash.tag(15);
                self.type_id(*inner);
            }
            ArType::Err => self.hash.tag(16),
            ArType::Void => self.hash.tag(17),
            ArType::IntLiteral => self.hash.tag(18),
            ArType::FloatLiteral => self.hash.tag(19),
            ArType::Error => self.hash.tag(20),
            ArType::ConstArray(param, inner) => {
                self.hash.tag(21);
                self.symbol(*param);
                self.type_id(*inner);
            }
            ArType::FrozenConst(value) => {
                self.hash.tag(24);
                for byte in value.canonical_bytes() {
                    self.hash.tag(byte);
                }
            }
            ArType::Const(value) => {
                self.hash.tag(22);
                self.hash.u64(*value);
            }
            ArType::ConstParam(param) => {
                self.hash.tag(23);
                self.symbol(*param);
            }
        }
    }

    pub(super) fn named_layout(
        &mut self,
        symbol: SymbolId,
        arguments: &[arandu_middle::types::TypeId],
    ) {
        if self.named_type_stack.contains(&symbol) {
            self.hash.tag(0);
            return;
        }
        self.hash.tag(1);
        self.named_type_stack.push(symbol);
        self.hash.bool(self.type_info.is_repr_c(symbol));
        if let Some(params) = self.type_info.get_generic_params(symbol) {
            self.hash.usize(params.len());
            for &param in params.iter() {
                self.symbol(param);
            }
        } else {
            self.hash.usize(0);
        }

        if let Some(fields) = self.type_info.fields_for(symbol, arguments) {
            self.hash.tag(0);
            self.hash.usize(fields.fields.len());
            for field in &fields.fields {
                self.hash.str(&field.name);
                self.hash.usize(field.index);
                if let Some(field_symbol) = field.symbol {
                    self.hash.tag(1);
                    self.symbol(field_symbol);
                } else {
                    self.hash.tag(0);
                }
                self.type_id(field.ty);
            }
        } else {
            self.hash.tag(1);
        }

        // Copy the `'a` reference so the payload slices do not borrow `self`.
        let type_info = self.type_info;
        let mut variants = type_info.enum_variant_payloads(symbol, arguments);
        variants.sort_by_key(|variant| {
            (
                variant.tag.unwrap_or(usize::MAX),
                variant.variant.file_id,
                variant.variant.local_id.0,
            )
        });
        self.hash.usize(variants.len());
        for variant in variants {
            self.hash.usize(variant.tag.unwrap_or(usize::MAX));
            self.symbol(variant.variant);
            match variant.payload {
                None => self.hash.tag(0),
                Some(types) => {
                    self.hash.tag(1);
                    self.hash.usize(types.len());
                    for &ty in types {
                        self.type_id(ty);
                    }
                }
            }
        }

        let removed = self.named_type_stack.pop();
        debug_assert_eq!(removed, Some(symbol));
    }
}
