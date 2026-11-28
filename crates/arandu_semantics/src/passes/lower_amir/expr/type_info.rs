//! `types.info<T>()`: the description of a concrete type, built as an
//! ordinary `std.core.types.TypeInfo` value after monomorphization. The same
//! AMIR runs in compile-time evaluation and at run time; a `const` holding
//! the description keeps it in static storage.

use super::super::LowerCtx;
use crate::amir::{AmirConstant, AmirOperand, AmirPlace, AmirRvalue, TempId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::passes::type_checker::types::{ArType, Primitive};
use arandu_middle::layout::StructLayoutProvider;

/// One member of the description, before it becomes AMIR.
struct Field {
    name: String,
    type_name: String,
    offset: u64,
    size: u64,
}

struct Variant {
    name: String,
    tag: usize,
    has_payload: bool,
}

struct Description {
    name: String,
    kind: &'static str,
    size: u64,
    align: u64,
    complete: bool,
    fields: Vec<Field>,
    variants: Vec<Variant>,
}

impl LowerCtx<'_> {
    pub(crate) fn lower_type_info(
        &mut self,
        described: crate::types::TypeId,
        result_ty: crate::types::TypeId,
        target: Option<TempId>,
        span: arandu_middle::Span,
    ) -> Result<AmirOperand, Diagnostic> {
        let description = self.describe(described, span)?;
        let info = self.tc.type_info.as_ref();
        let ArType::Named(info_symbol, _) = self.resolve_ty(result_ty) else {
            return Err(Self::not_type_info(span, "the result is not a struct"));
        };
        let member_ty = |name: &str| {
            info.get_struct_fields(info_symbol)
                .and_then(|fields| fields.get(name))
                .map(|field| field.ty)
        };
        let (Some(kind_ty), Some(fields_ty), Some(variants_ty)) = (
            member_ty("kind"),
            member_ty("fields"),
            member_ty("variants"),
        ) else {
            return Err(Self::not_type_info(span, "a `TypeInfo` member is missing"));
        };
        let kind = self.unit_variant(kind_ty, description.kind, span)?;
        let fields = description
            .fields
            .iter()
            .map(|field| {
                vec![
                    ("name", self.text(&field.name)),
                    ("typeName", self.text(&field.type_name)),
                    ("offset", self.size(field.offset)),
                    ("size", self.size(field.size)),
                ]
            })
            .collect::<Vec<_>>();
        let fields = self.slice_of_structs(fields_ty, fields, span)?;
        let variants = description
            .variants
            .iter()
            .map(|variant| {
                vec![
                    ("name", self.text(&variant.name)),
                    ("tag", self.size(variant.tag as u64)),
                    (
                        "hasPayload",
                        AmirOperand::Constant(AmirConstant::Bool(variant.has_payload)),
                    ),
                ]
            })
            .collect::<Vec<_>>();
        let variants = self.slice_of_structs(variants_ty, variants, span)?;
        let members = vec![
            ("name", self.text(&description.name)),
            ("kind", kind),
            ("size", self.size(description.size)),
            ("align", self.size(description.align)),
            (
                "isComplete",
                AmirOperand::Constant(AmirConstant::Bool(description.complete)),
            ),
            ("fields", fields),
            ("variants", variants),
        ];
        let dest = target.unwrap_or_else(|| self.new_temp_id(result_ty));
        self.emit_assign_temp(dest, Self::struct_literal(info_symbol, members));
        Ok(AmirOperand::Copy(dest))
    }

    fn describe(
        &self,
        described: crate::types::TypeId,
        span: arandu_middle::Span,
    ) -> Result<Description, Diagnostic> {
        let info = self.tc.type_info.as_ref();
        let interner = &self.tc.type_info.type_interner;
        let engine = arandu_middle::layout::LayoutEngine::from_data_layout(self.layout);
        let layout_of = |ty: &ArType| {
            engine.layout_of_type(ty, interner, info).map_err(|error| {
                Diagnostic::error(
                    DiagCode::T047InvalidTypeLayout,
                    format!("cannot describe this type: {error}"),
                    span,
                )
                .with_primary_label("type has no concrete target layout")
            })
        };
        let ty = self.resolve_ty(described);
        let layout = layout_of(&ty)?;
        let mut description = Description {
            name: self.type_name(described),
            kind: "Other",
            size: layout.size,
            align: layout.align,
            complete: true,
            fields: Vec::new(),
            variants: Vec::new(),
        };
        let builtin_variants: &[(&str, bool)] = match &ty {
            ArType::Option(_) => &[("None", false), ("Some", true)],
            ArType::Result(..) => &[("Ok", true), ("Err", true)],
            _ => &[],
        };
        description.kind = match &ty {
            ArType::Primitive(_) => "Primitive",
            ArType::Option(_) | ArType::Result(..) => "Enum",
            ArType::Tuple(_) => "Tuple",
            ArType::Array(..) | ArType::ConstArray(..) => "Array",
            ArType::Slice(_) | ArType::MutSlice(_) => "Slice",
            ArType::Ptr(_) => "Pointer",
            ArType::Ref(_) | ArType::RefMut(_) => "Reference",
            ArType::Func(..) => "Function",
            ArType::Vector(_) => "Vector",
            ArType::Named(symbol, _) if info.get_struct_fields(*symbol).is_some() => {
                if info.is_union(*symbol) {
                    "Union"
                } else {
                    "Struct"
                }
            }
            ArType::Named(symbol, _) if info.get_enum_variants(*symbol).is_some() => "Enum",
            _ => "Other",
        };
        for (tag, (name, has_payload)) in builtin_variants.iter().enumerate() {
            description.variants.push(Variant {
                name: (*name).to_string(),
                tag,
                has_payload: *has_payload,
            });
        }
        match &ty {
            ArType::Tuple(arguments) => {
                for (index, &element) in interner.type_args(*arguments).iter().enumerate() {
                    description.fields.push(Field {
                        name: index.to_string(),
                        type_name: self.type_name(element),
                        offset: layout.field_offsets.get(index).copied().unwrap_or(0),
                        size: layout_of(&interner.resolve(element))?.size,
                    });
                }
            }
            ArType::Named(symbol, _)
                if description.kind == "Struct" || description.kind == "Union" =>
            {
                // Private fields are visible in the module that declares the
                // type, where the call may also read them.
                let home = symbol.file_id == span.file_id;
                let fields = info
                    .get_struct_fields_for_type(&ty, interner)
                    .map(|fields| fields.fields.clone())
                    .unwrap_or_default();
                for field in fields {
                    let private = field
                        .symbol
                        .is_some_and(|field| self.tc.type_info.private_fields.contains(&field));
                    if private && !home {
                        description.complete = false;
                        continue;
                    }
                    let field_ty = arandu_middle::layout::instantiated_field_type(
                        &ty,
                        field.name.as_str(),
                        interner,
                        info,
                    )
                    .unwrap_or(field.ty);
                    description.fields.push(Field {
                        name: field.name.to_string(),
                        type_name: self.type_name(field_ty),
                        offset: layout.field_offsets.get(field.index).copied().unwrap_or(0),
                        size: layout_of(&interner.resolve(field_ty))?.size,
                    });
                }
            }
            ArType::Named(symbol, _) if description.kind == "Enum" => {
                let variants = info.get_enum_variants(*symbol).unwrap_or_default();
                for (tag, variant) in variants.iter().enumerate() {
                    let name = info
                        .get_enum_variant_symbol(*symbol, tag)
                        .map(|variant| self.variant_name(variant).to_string())
                        .unwrap_or_else(|| tag.to_string());
                    description.variants.push(Variant {
                        name,
                        tag,
                        has_payload: variant.payload_ty.is_some(),
                    });
                }
            }
            _ => {}
        }
        Ok(description)
    }

    /// Canonical spelling: `module.Type<Args>` for a declared type, the
    /// language's spelling for the others.
    fn type_name(&self, ty: crate::types::TypeId) -> String {
        let interner = &self.tc.type_info.type_interner;
        let symbols = &self.tc.symbols;
        let list = |arguments: &[crate::types::TypeId]| {
            arguments
                .iter()
                .map(|&argument| self.type_name(argument))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match interner.resolve(ty) {
            ArType::Primitive(primitive) => primitive.as_str().to_string(),
            ArType::Named(symbol, arguments) => {
                let name = symbols.host_func_name(symbols.get(symbol)).to_string();
                let arguments = interner.type_args(arguments);
                if arguments.is_empty() {
                    name
                } else {
                    format!("{name}<{}>", list(&arguments))
                }
            }
            ArType::Slice(element) => format!("[]{}", self.type_name(element)),
            ArType::MutSlice(element) => format!("mut []{}", self.type_name(element)),
            ArType::Array(count, element) => format!("[{count}]{}", self.type_name(element)),
            ArType::Tuple(arguments) => format!("({})", list(&interner.type_args(arguments))),
            ArType::Ptr(inner) => format!("ptr[{}]", self.type_name(inner)),
            ArType::Ref(inner) => format!("ref {}", self.type_name(inner)),
            ArType::RefMut(inner) => format!("mut ref {}", self.type_name(inner)),
            ArType::Option(inner) => format!("Option<{}>", self.type_name(inner)),
            ArType::Result(ok, error) => {
                format!("Result<{}, {}>", self.type_name(ok), self.type_name(error))
            }
            _ => interner.display(ty, symbols),
        }
    }

    /// A variant's own name: its symbol is spelled `Enum.Variant`.
    fn variant_name(&self, variant: crate::SymbolId) -> &str {
        let name = self.tc.symbols.get(variant).name.as_str();
        name.rsplit('.').next().unwrap_or(name)
    }

    fn text(&mut self, value: &str) -> AmirOperand {
        let ty = self.intern_ty(ArType::Primitive(Primitive::Str));
        let constant = self.intern_literal_str(value.to_string());
        self.typed_constant(ty, constant)
    }

    fn size(&mut self, value: u64) -> AmirOperand {
        let ty = self.intern_ty(ArType::Primitive(Primitive::USize));
        let constant = self.intern_literal_int(value.to_string());
        self.typed_constant(ty, constant)
    }

    fn typed_constant(&mut self, ty: crate::types::TypeId, constant: AmirConstant) -> AmirOperand {
        let temp = self.new_temp_id(ty);
        self.emit_assign_temp(temp, AmirRvalue::Use(AmirOperand::Constant(constant)));
        AmirOperand::Copy(temp)
    }

    fn unit_variant(
        &mut self,
        enum_ty: crate::types::TypeId,
        name: &str,
        span: arandu_middle::Span,
    ) -> Result<AmirOperand, Diagnostic> {
        let info = self.tc.type_info.as_ref();
        let ArType::Named(symbol, _) = self.resolve_ty(enum_ty) else {
            return Err(Self::not_type_info(span, "`TypeKind` is not an enum"));
        };
        let count = info
            .get_enum_variants(symbol)
            .map_or(0, |variants| variants.len());
        let tag = (0..count)
            .find(|&tag| {
                info.get_enum_variant_symbol(symbol, tag)
                    .is_some_and(|variant| self.variant_name(variant) == name)
            })
            .ok_or_else(|| Self::not_type_info(span, "a `TypeKind` variant is missing"))?;
        let temp = self.new_temp_id(enum_ty);
        self.emit_assign_temp(
            temp,
            AmirRvalue::EnumConstruct {
                variant_tag: tag,
                payload: None,
            },
        );
        Ok(AmirOperand::Copy(temp))
    }

    /// `[]S` over a fresh array of `S` values, viewed whole.
    fn slice_of_structs(
        &mut self,
        slice_ty: crate::types::TypeId,
        items: Vec<Vec<(&str, AmirOperand)>>,
        span: arandu_middle::Span,
    ) -> Result<AmirOperand, Diagnostic> {
        let (ArType::Slice(element_ty), ArType::Named(element, _)) = (
            self.resolve_ty(slice_ty),
            self.resolve_ty(match self.resolve_ty(slice_ty) {
                ArType::Slice(element) => element,
                _ => return Err(Self::not_type_info(span, "a member list is not a slice")),
            }),
        ) else {
            return Err(Self::not_type_info(
                span,
                "a member list is not a slice of structs",
            ));
        };
        let mut values = Vec::with_capacity(items.len());
        for members in items {
            let temp = self.new_temp_id(element_ty);
            self.emit_assign_temp(temp, Self::struct_literal(element, members));
            values.push(AmirOperand::Copy(temp));
        }
        let count = values.len();
        let array_ty = self.intern_ty(ArType::Array(count as u64, element_ty));
        let array = self.new_temp_id(array_ty);
        self.emit_assign_temp(array, AmirRvalue::Array { items: values });
        let local = self.new_compiler_local_id(array_ty, span);
        self.write_variable_source(local, AmirOperand::Copy(array))?;
        self.mark_local_materialized(local);
        let owner = self.new_temp(ArType::Ref(array_ty));
        self.emit_assign_temp(
            owner,
            AmirRvalue::Borrow(AmirPlace {
                local,
                projections: smallvec::SmallVec::new(),
            }),
        );
        let len = AmirOperand::Constant(self.intern_literal_int(count.to_string()));
        let slice = self.new_temp_id(slice_ty);
        self.emit_assign_temp(
            slice,
            AmirRvalue::SliceView {
                owner: AmirOperand::Copy(owner),
                data: AmirOperand::Copy(owner),
                len,
            },
        );
        Ok(AmirOperand::Copy(slice))
    }

    fn struct_literal(symbol: crate::SymbolId, members: Vec<(&str, AmirOperand)>) -> AmirRvalue {
        AmirRvalue::StructLiteral {
            struct_symbol: symbol,
            fields: members
                .into_iter()
                .map(|(name, operand)| (smol_str::SmolStr::new(name), operand))
                .collect(),
        }
    }

    fn not_type_info(span: arandu_middle::Span, what: &str) -> Diagnostic {
        Diagnostic::ice(
            DiagCode::ICEL001,
            format!("`types.info` must return `std.core.types.TypeInfo`: {what}"),
            span,
        )
    }
}
