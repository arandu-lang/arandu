use arandu_middle::SymbolId;
use arandu_middle::layout::StructLayoutProvider;
use arandu_middle::symbol_table::SymbolKind;
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};
use arandu_semantics::SymbolTable;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use smol_str::SmolStr;

use super::model::{WitPrimitive, WitTypeDef, WitTypeDefKind, WitTypeId, WitTypeKind, WitTypePool};
use super::naming::to_wit_ident;

/// Helper context for lowering types and accumulating nominal definitions.
pub(super) struct TypeLowerer<'a> {
    symbols: &'a SymbolTable,
    interner: &'a TypeInterner,
    layout_provider: &'a dyn StructLayoutProvider,
    pub(super) pool: WitTypePool,
    pub(super) type_defs: Vec<WitTypeDef>,
    defined_symbols: FxHashSet<SymbolId>,
    visiting_symbols: FxHashSet<SymbolId>,
    pub(super) named_map: FxHashMap<SymbolId, SmolStr>,
}

impl<'a> TypeLowerer<'a> {
    pub(super) fn new(
        symbols: &'a SymbolTable,
        interner: &'a TypeInterner,
        layout_provider: &'a dyn StructLayoutProvider,
    ) -> Self {
        Self {
            symbols,
            interner,
            layout_provider,
            pool: WitTypePool::new(),
            type_defs: Vec::new(),
            defined_symbols: FxHashSet::default(),
            visiting_symbols: FxHashSet::default(),
            named_map: FxHashMap::default(),
        }
    }

    /// Lower an Arandu [`TypeId`] into a [`WitTypeId`].
    ///
    /// Returns `None` for `Void`, `Err`, or `Error`.
    pub(super) fn lower_type(&mut self, ty: TypeId) -> Option<WitTypeId> {
        let resolved = self.interner.resolve(ty);
        self.lower_ar_type(&resolved)
    }

    /// Lower an [`ArType`] into a [`WitTypeId`].
    pub(super) fn lower_ar_type(&mut self, resolved: &ArType) -> Option<WitTypeId> {
        match resolved {
            ArType::Void | ArType::Err | ArType::Error => None,

            ArType::Primitive(prim) => {
                let p = map_primitive(*prim);
                Some(self.pool.add(WitTypeKind::Primitive(p)))
            }

            ArType::Slice(inner) => {
                let inner_id = self
                    .lower_type(*inner)
                    .unwrap_or_else(|| self.pool.add(WitTypeKind::Primitive(WitPrimitive::U8)));
                Some(self.pool.add(WitTypeKind::List(inner_id)))
            }

            ArType::Tuple(range) => {
                let args = self.interner.type_args(*range);
                let mut elems = SmallVec::with_capacity(args.len());
                for &arg in &args {
                    if let Some(arg_wit) = self.lower_type(arg) {
                        elems.push(arg_wit);
                    }
                }
                Some(self.pool.add(WitTypeKind::Tuple(elems)))
            }

            ArType::Option(inner) => {
                let inner_id = self
                    .lower_type(*inner)
                    .unwrap_or_else(|| self.pool.add(WitTypeKind::Primitive(WitPrimitive::U8)));
                Some(self.pool.add(WitTypeKind::Option(inner_id)))
            }

            ArType::Result(ok, err) => {
                let ok_id = self.lower_type(*ok);
                let err_id = self.lower_type(*err);
                Some(self.pool.add(WitTypeKind::Result {
                    ok: ok_id,
                    err: err_id,
                }))
            }

            ArType::Named(sym_id, generic_args) => {
                let sym_id = *sym_id;
                // Check for builtins (string, vec, option, result)
                if Some(sym_id) == self.symbols.builtins.string {
                    return Some(self.pool.add(WitTypeKind::Primitive(WitPrimitive::String)));
                }
                if Some(sym_id) == self.symbols.builtins.vec {
                    let args = self.interner.type_args(*generic_args);
                    let elem_id = args
                        .first()
                        .and_then(|&a| self.lower_type(a))
                        .unwrap_or_else(|| self.pool.add(WitTypeKind::Primitive(WitPrimitive::U8)));
                    return Some(self.pool.add(WitTypeKind::List(elem_id)));
                }
                if Some(sym_id) == self.symbols.builtins.option {
                    let args = self.interner.type_args(*generic_args);
                    let inner_id = args
                        .first()
                        .and_then(|&a| self.lower_type(a))
                        .unwrap_or_else(|| self.pool.add(WitTypeKind::Primitive(WitPrimitive::U8)));
                    return Some(self.pool.add(WitTypeKind::Option(inner_id)));
                }
                if Some(sym_id) == self.symbols.builtins.result {
                    let args = self.interner.type_args(*generic_args);
                    let ok_id = args.first().and_then(|&a| self.lower_type(a));
                    let err_id = args.get(1).and_then(|&a| self.lower_type(a));
                    return Some(self.pool.add(WitTypeKind::Result {
                        ok: ok_id,
                        err: err_id,
                    }));
                }

                // User-defined nominal struct or enum
                self.ensure_nominal_defined(sym_id);
                Some(self.pool.add(WitTypeKind::Named(sym_id)))
            }

            // References/pointers: unwrap or degrade gracefully
            ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                self.lower_type(*inner)
            }
            ArType::Nullable(inner) => {
                let inner_id = self
                    .lower_type(*inner)
                    .unwrap_or_else(|| self.pool.add(WitTypeKind::Primitive(WitPrimitive::U8)));
                Some(self.pool.add(WitTypeKind::Option(inner_id)))
            }

            _ => Some(self.pool.add(WitTypeKind::Primitive(WitPrimitive::U32))),
        }
    }

    /// Ensure a nominal type (`struct` or `enum`) is collected into [`type_defs`].
    fn ensure_nominal_defined(&mut self, sym_id: SymbolId) {
        if self.defined_symbols.contains(&sym_id) || self.visiting_symbols.contains(&sym_id) {
            return;
        }

        let Some(sym) = self.symbols.try_get(sym_id) else {
            return;
        };

        self.visiting_symbols.insert(sym_id);
        let wit_name = SmolStr::new(to_wit_ident(&sym.name));
        self.named_map.insert(sym_id, wit_name.clone());

        match sym.kind {
            SymbolKind::Struct => {
                let mut fields_vec = SmallVec::new();
                if let Some(fields_def) = self.layout_provider.get_struct_fields(sym_id) {
                    let mut sorted_fields: Vec<_> = fields_def.iter().collect();
                    sorted_fields.sort_by_key(|f| f.index);
                    for field in sorted_fields {
                        let field_wit_ty = self.lower_type(field.ty).unwrap_or_else(|| {
                            self.pool.add(WitTypeKind::Primitive(WitPrimitive::U32))
                        });
                        let field_name = SmolStr::new(to_wit_ident(&field.name));
                        fields_vec.push((field_name, field_wit_ty));
                    }
                }
                self.type_defs.push(WitTypeDef {
                    symbol: sym_id,
                    name: wit_name,
                    kind: WitTypeDefKind::Record { fields: fields_vec },
                });
            }
            SymbolKind::Enum => {
                // Collect associated members of this enum that are EnumVariant
                let mut variants: Vec<(SmolStr, Option<TypeId>)> = Vec::new();
                for ((parent_sym, member_name), &var_sym) in &self.symbols.associated_members {
                    if *parent_sym == sym_id
                        && self
                            .symbols
                            .try_get(var_sym)
                            .is_some_and(|s| s.kind == SymbolKind::EnumVariant)
                    {
                        variants.push((member_name.clone(), None));
                    }
                }

                // Associate payload types from layout provider if available
                if let Some(payload_shapes) = self.layout_provider.get_enum_variants(sym_id) {
                    for (i, shape) in payload_shapes.into_iter().enumerate() {
                        if let Some(var) = variants.get_mut(i) {
                            var.1 = shape.payload_ty;
                        }
                    }
                }

                let has_payload = variants.iter().any(|v| v.1.is_some());
                if has_payload {
                    let mut cases = SmallVec::new();
                    for (v_name, payload_ty) in variants {
                        let payload_wit = payload_ty.and_then(|pt| self.lower_type(pt));
                        cases.push((SmolStr::new(to_wit_ident(&v_name)), payload_wit));
                    }
                    self.type_defs.push(WitTypeDef {
                        symbol: sym_id,
                        name: wit_name,
                        kind: WitTypeDefKind::Variant { cases },
                    });
                } else {
                    let mut cases = SmallVec::new();
                    for (v_name, _) in variants {
                        cases.push(SmolStr::new(to_wit_ident(&v_name)));
                    }
                    self.type_defs.push(WitTypeDef {
                        symbol: sym_id,
                        name: wit_name,
                        kind: WitTypeDefKind::Enum { cases },
                    });
                }
            }
            _ => {}
        }

        self.visiting_symbols.remove(&sym_id);
        self.defined_symbols.insert(sym_id);
    }
}

/// Map an Arandu [`Primitive`] to its [`WitPrimitive`].
pub(super) fn map_primitive(prim: Primitive) -> WitPrimitive {
    match prim {
        Primitive::Bool => WitPrimitive::Bool,
        Primitive::U8 | Primitive::Byte => WitPrimitive::U8,
        Primitive::I8 => WitPrimitive::S8,
        Primitive::U16 => WitPrimitive::U16,
        Primitive::I16 => WitPrimitive::S16,
        Primitive::U32 | Primitive::Uint => WitPrimitive::U32,
        Primitive::I32 | Primitive::Int => WitPrimitive::S32,
        Primitive::U64 => WitPrimitive::U64,
        Primitive::I64 => WitPrimitive::S64,
        Primitive::F32 => WitPrimitive::F32,
        Primitive::F64 | Primitive::Float => WitPrimitive::F64,
        Primitive::Char => WitPrimitive::Char,
        Primitive::Str => WitPrimitive::String,
        Primitive::Any => WitPrimitive::U32,
    }
}
