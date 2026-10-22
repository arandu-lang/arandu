//! C top-level type definitions, string literal constants, drop shims, and function declarations.

use std::fmt::Write;

use arandu_middle::SymbolId;
use arandu_middle::amir::{AmirFunc, AmirRvalue, AmirStmt};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::ArType;

use super::{CEmitter, sanitize_c_ident};

impl<'a> CEmitter<'a> {
    pub(super) fn gen_drop_glue(
        &self,
        ty_id: arandu_middle::types::TypeId,
        ty: &ArType,
    ) -> Option<(String, SymbolId)> {
        let ArType::Named(_, _) = ty else {
            return None;
        };
        let destructor = self.provider.destructor_for_type(ty_id)?;
        Some((
            format!("__ar_drop_{}_{}", destructor.file_id, destructor.local_id.0),
            destructor,
        ))
    }

    pub(super) fn emit_gen_drop_glues(&mut self) {
        let mut payloads = std::collections::BTreeMap::<String, (ArType, SymbolId)>::new();
        for func in &self.program.funcs {
            if !self.live_functions.contains(&func.symbol) {
                continue;
            }
            for stmt in func.stmts.payloads.iter() {
                let AmirStmt::Assign { rhs, .. } = stmt else {
                    continue;
                };
                let payload_ty = match rhs {
                    AmirRvalue::GenInsert { payload_ty, .. }
                    | AmirRvalue::GenSet { payload_ty, .. }
                    | AmirRvalue::GenUpsert { payload_ty, .. } => *payload_ty,
                    _ => continue,
                };
                let ty = self.interner.resolve(payload_ty);
                if let Some((name, destructor)) = self.gen_drop_glue(payload_ty, &ty) {
                    payloads.entry(name).or_insert((ty, destructor));
                }
            }
        }
        for (glue, (ty, destructor)) in payloads {
            let payload_c = self.format_type(&ty);
            let destructor_sym = self.symbols.get(destructor);
            let destructor_c = sanitize_c_ident(self.symbols.host_func_name(destructor_sym));
            let _ = writeln!(
                &mut self.output,
                "static void {glue}(void *raw) {{ {destructor_c}(*({payload_c} *)raw); }}"
            );
        }
    }

    /// C linkage name for a function's return type (`main` is always `int`).
    pub(super) fn c_func_return_type(&self, func: &AmirFunc) -> String {
        let sym = self.symbols.get(func.symbol);
        let name = sanitize_c_ident(self.symbols.host_func_name(sym));
        if name == "main" {
            return "int".to_string();
        }
        let ret = self.interner.resolve(func.return_type);
        self.format_type(&ret).into_owned()
    }

    pub(super) fn emit_str_literals(&mut self) {
        for (i, entry) in self.program.literal_pool.entries.iter().enumerate() {
            if let AmirLiteralEntry::Str(s) = entry {
                // emit global array
                let _ = write!(&mut self.output, "static const uint8_t STR_{}[] = {{", i);
                for b in s.bytes() {
                    let _ = write!(&mut self.output, "{},", b);
                }
                let _ = writeln!(&mut self.output, "0}};"); // null terminator for safety

                // ArStr fat-pointer constant matching LayoutEngine (ptr + len)
                let _ = writeln!(
                    &mut self.output,
                    "static const ArStr AR_STR_{} = {{ .ptr = STR_{}, .len = {} }};",
                    i,
                    i,
                    s.len()
                );
            }
        }
    }

    pub(super) fn ensure_type_emitted(&mut self, ty: &ArType) {
        if let ArType::Nullable(inner) = ty {
            let inner_ty = self.interner.resolve(*inner);
            if matches!(
                inner_ty,
                ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
            ) && inner_ty.slice_abi_element(self.interner).is_none()
            {
                self.ensure_type_emitted(&inner_ty);
                return;
            }
        }
        if let ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) = ty {
            let inner_ty = self.interner.resolve(*inner);
            self.ensure_type_emitted(&inner_ty);
            return;
        }
        let name = self.format_type(ty);
        // Never redefine C/stdlib primitive types as blob structs (e.g. `double`).
        if self.emitted_types.contains(name.as_ref())
            || matches!(
                name.as_ref(),
                "void"
                    | "bool"
                    | "float"
                    | "double"
                    | "void*"
                    | "int8_t"
                    | "int16_t"
                    | "int32_t"
                    | "int64_t"
                    | "uint8_t"
                    | "uint16_t"
                    | "uint32_t"
                    | "uint64_t"
                    | "ArStr"
            )
        {
            return;
        }

        if let ArType::Func(params, ret) = ty {
            let ret_ty = self.interner.resolve(*ret);
            self.ensure_type_emitted(&ret_ty);
            let mut params_c_tys = Vec::new();
            for &p in self.interner.type_args(*params).iter() {
                let p_ty = self.interner.resolve(p);
                if matches!(p_ty, ArType::Void) {
                    continue;
                }
                self.ensure_type_emitted(&p_ty);
                params_c_tys.push(self.format_type(&p_ty));
            }
            let ret_c_ty = self.format_type(&ret_ty);
            let params_str = if params_c_tys.is_empty() {
                "void".to_string()
            } else {
                params_c_tys.join(", ")
            };
            let _ = writeln!(
                &mut self.output,
                "typedef {} (*{})({});",
                ret_c_ty, name, params_str
            );
            self.emitted_types.insert(name.into_owned());
            return;
        }

        if let ArType::Array(len, inner) = ty {
            let inner_ty = self.interner.resolve(*inner);
            self.ensure_type_emitted(&inner_ty);
            if self.emitted_types.contains(name.as_ref()) {
                return;
            }
            let layout = self.checked_layout(ty);
            if layout.size > 0 && *len > 0 {
                let elem_c_ty = self.format_type(&inner_ty);
                let _ = writeln!(
                    &mut self.output,
                    "typedef struct AR_MAY_ALIAS {{ _Alignas({}) {} data[{}]; }} {};",
                    layout.align, elem_c_ty, len, name
                );
            } else {
                let _ = writeln!(
                    &mut self.output,
                    "typedef struct AR_MAY_ALIAS {{ uint8_t empty; }} {};",
                    name
                );
            }
            self.emitted_types.insert(name.into_owned());
            return;
        }

        if let ArType::Vector(vector) = ty {
            self.emit_vector_type(*vector);
            return;
        }
        let layout = self.checked_layout(ty);
        if layout.size > 0 {
            let _ = writeln!(
                &mut self.output,
                "typedef struct AR_MAY_ALIAS {{ _Alignas({}) uint8_t memory[{}]; }} {};",
                layout.align, layout.size, name
            );
        } else {
            let _ = writeln!(
                &mut self.output,
                "typedef struct AR_MAY_ALIAS {{ uint8_t empty; }} {};",
                name
            ); // C doesn't like zero sized structs sometimes
        }
        self.emitted_types.insert(name.into_owned());

        match ty {
            ArType::Tuple(tys) => {
                for &t in self.interner.type_args(*tys).iter() {
                    let field_ty = self.interner.resolve(t);
                    self.ensure_type_emitted(&field_ty);
                }
            }
            ArType::Option(inner)
            | ArType::Nullable(inner)
            | ArType::Slice(inner)
            | ArType::MutSlice(inner) => {
                let inner_ty = self.interner.resolve(*inner);
                self.ensure_type_emitted(&inner_ty);
            }
            ArType::Result(ok, err) => {
                let ok_ty = self.interner.resolve(*ok);
                let err_ty = self.interner.resolve(*err);
                self.ensure_type_emitted(&ok_ty);
                self.ensure_type_emitted(&err_ty);
            }
            ArType::Named(_, _) => {
                let field_names: Vec<_> = self
                    .provider
                    .get_struct_fields_for_type(ty, self.interner)
                    .map(|fields| fields.fields.iter().map(|f| f.name.clone()).collect())
                    .unwrap_or_default();
                for field_name in field_names {
                    let field_ty = self.instantiated_field_ty(ty, &field_name);
                    if !matches!(field_ty, ArType::Error)
                        && self
                            .layout
                            .layout_of_type(&field_ty, self.interner, self.provider)
                            .is_ok()
                    {
                        self.ensure_type_emitted(&field_ty);
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn emit_func_decl(&mut self, func: &AmirFunc) {
        let sym = self.symbols.get(func.symbol);
        let name = sanitize_c_ident(self.symbols.host_func_name(sym));
        let ret_ty = self.c_func_return_type(func);
        let _ = write!(&mut self.output, "{} {}(", ret_ty, name);
        self.emit_func_parameters(func);
        let _ = writeln!(&mut self.output, ");");
    }

    pub(super) fn emit_func_parameters(&mut self, func: &AmirFunc) {
        let mut emitted = false;
        for param in &func.params {
            let ty = self.temp_ty(func, *param);
            if matches!(ty, ArType::Void) {
                continue;
            }
            if emitted {
                let _ = write!(&mut self.output, ", ");
            }
            let ty_str = self.format_type(&ty);
            let _ = write!(&mut self.output, "{} p{}", ty_str, param.as_usize());
            emitted = true;
        }
        if !emitted {
            let _ = write!(&mut self.output, "void");
        }
    }
}
