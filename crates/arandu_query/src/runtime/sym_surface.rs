use arandu_middle::symbol_table::{SymbolId, SymbolKind};
use arandu_middle::Visibility;

use crate::db::HashEq;
use crate::stable_hash::StableHash;
use crate::{ArandCompilerDb, SourceFile};

/// Canonical surface of an individual symbol for incremental tracking (INC.3a).
/// Excludes lexical positions/spans so shifts in sibling bodies do not invalidate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSurface {
    pub symbol: SymbolId,
    pub fingerprint: blake3::Hash,
}

impl StableHash for SymbolSurface {
    fn stable_hash(&self) -> blake3::Hash {
        self.fingerprint
    }
}

/// Computes the isolated surface fingerprint of `symbol` in `file`.
///
/// Changes to sibling declarations, bodies, or positions in `file` do not
/// change the surface of `symbol` unless `symbol`'s own public contract changed.
#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "symbol_surface", file = ?file.file_id(db), symbol = ?symbol)
)]
pub fn symbol_surface(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<SymbolSurface> {
    if *file.file_id(db) != symbol.file_id {
        if let Some(foreign) = db.source_file_by_id(symbol.file_id) {
            return symbol_surface(db, foreign, symbol).clone();
        }
    }
    let signatures = crate::passes::declaration_signatures(db, file);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"SymbolSurface/v2");
    hasher.update(&symbol.file_id.to_le_bytes());
    hasher.update(&symbol.local_id.0.to_le_bytes());

    let sym = signatures.symbols.try_get(symbol);
    if let Some(sym) = sym {
        hasher.update(&[1]);
        hash_str(&mut hasher, &sym.name);
        hasher.update(&[sym.kind as u8]);
        hasher.update(&[match sym.visibility {
            Visibility::Private => 0,
            Visibility::Module => 1,
            Visibility::Internal => 2,
            Visibility::Public => 3,
        }]);
        if let Some(lang) = sym.lang_item {
            hasher.update(&[1, lang as u8]);
        } else {
            hasher.update(&[0]);
        }

        // Type / signature
        if let Some(&ty) = signatures.type_info.decl_types.get(&symbol) {
            hasher.update(&[1]);
            let bytes = crate::stable_hash::semantic_type_bytes(
                ty,
                &signatures.type_info.type_interner,
                &signatures.symbols,
            );
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        } else {
            hasher.update(&[0]);
        }

        // Struct fields & metadata
        if sym.kind == SymbolKind::Struct {
            if let Some(fields) = signatures.type_info.struct_fields.get(&symbol) {
                hasher.update(&[1]);
                hasher.update(&(fields.fields.len() as u64).to_le_bytes());
                for field in &fields.fields {
                    hash_str(&mut hasher, &field.name);
                    let bytes = crate::stable_hash::semantic_type_bytes(
                        field.ty,
                        &signatures.type_info.type_interner,
                        &signatures.symbols,
                    );
                    hasher.update(&(bytes.len() as u64).to_le_bytes());
                    hasher.update(&bytes);
                    let is_private = field
                        .symbol
                        .is_some_and(|sym| signatures.type_info.private_fields.contains(&sym));
                    hasher.update(&[u8::from(is_private)]);
                }
            } else {
                hasher.update(&[0]);
            }
            hasher.update(&[u8::from(
                signatures.type_info.struct_repr_c.contains(&symbol),
            )]);
            hasher.update(&[u8::from(
                signatures.type_info.struct_unions.contains(&symbol),
            )]);
            hasher.update(&[u8::from(
                signatures
                    .type_info
                    .interior_mutable_types
                    .contains(&symbol),
            )]);
            hasher.update(&[u8::from(
                signatures.type_info.non_copy_types.contains(&symbol),
            )]);
            if let Some(dtor) = signatures.type_info.destructors.get(&symbol) {
                hasher.update(&[1]);
                hasher.update(&dtor.file_id.to_le_bytes());
                hasher.update(&dtor.local_id.0.to_le_bytes());
            } else {
                hasher.update(&[0]);
            }
        }

        // Enum variants
        if sym.kind == SymbolKind::Enum || sym.kind == SymbolKind::EnumVariant {
            if let Some((parent, shape)) = signatures.type_info.enum_variants.get(&symbol) {
                hasher.update(&[1]);
                hasher.update(&parent.file_id.to_le_bytes());
                hasher.update(&parent.local_id.0.to_le_bytes());
                match shape {
                    arandu_typeck::EnumPayloadShape::Unit => {
                        hasher.update(&[0]);
                    }
                    arandu_typeck::EnumPayloadShape::Tuple(tys) => {
                        hasher.update(&[1]);
                        hasher.update(&(tys.len() as u64).to_le_bytes());
                        for &ty in tys {
                            let bytes = crate::stable_hash::semantic_type_bytes(
                                ty,
                                &signatures.type_info.type_interner,
                                &signatures.symbols,
                            );
                            hasher.update(&(bytes.len() as u64).to_le_bytes());
                            hasher.update(&bytes);
                        }
                    }
                }
            } else {
                hasher.update(&[0]);
            }
            if let Some(&tag) = signatures.type_info.enum_variant_tags.get(&symbol) {
                hasher.update(&[1]);
                hasher.update(&(tag as u64).to_le_bytes());
            } else {
                hasher.update(&[0]);
            }
            if sym.kind == SymbolKind::Enum {
                hasher.update(&[u8::from(
                    signatures.type_info.non_copy_types.contains(&symbol),
                )]);
            }
        }

        // Generic parameters & constraints
        if let Some(params) = signatures.type_info.generic_params.get(&symbol) {
            hasher.update(&[1]);
            hasher.update(&(params.len() as u64).to_le_bytes());
            for &param in params.iter() {
                hasher.update(&param.file_id.to_le_bytes());
                hasher.update(&param.local_id.0.to_le_bytes());
                if let Some(constraints) = signatures.type_info.param_constraints.get(&param) {
                    hasher.update(&(constraints.len() as u64).to_le_bytes());
                    for constraint in constraints.iter() {
                        hasher.update(&constraint.iface_sym.file_id.to_le_bytes());
                        hasher.update(&constraint.iface_sym.local_id.0.to_le_bytes());
                        hasher.update(&(constraint.type_args.len() as u64).to_le_bytes());
                        for &arg in &constraint.type_args {
                            let bytes = crate::stable_hash::semantic_type_bytes(
                                arg,
                                &signatures.type_info.type_interner,
                                &signatures.symbols,
                            );
                            hasher.update(&(bytes.len() as u64).to_le_bytes());
                            hasher.update(&bytes);
                        }
                    }
                } else {
                    hasher.update(&0u64.to_le_bytes());
                }
                if let Some(&default_ty) = signatures.type_info.generic_defaults.get(&param) {
                    hasher.update(&[1]);
                    let bytes = crate::stable_hash::semantic_type_bytes(
                        default_ty,
                        &signatures.type_info.type_interner,
                        &signatures.symbols,
                    );
                    hasher.update(&(bytes.len() as u64).to_le_bytes());
                    hasher.update(&bytes);
                } else {
                    hasher.update(&[0]);
                }
                if let Some(&bound) = signatures.type_info.callback_bounds.get(&param) {
                    hasher.update(&[1]);
                    let bytes = crate::stable_hash::semantic_type_bytes(
                        bound,
                        &signatures.type_info.type_interner,
                        &signatures.symbols,
                    );
                    hasher.update(&(bytes.len() as u64).to_le_bytes());
                    hasher.update(&bytes);
                } else {
                    hasher.update(&[0]);
                }
            }
        } else {
            hasher.update(&[0]);
        }

        // Function contracts
        if let Some(summary) = signatures.type_info.return_borrow_summaries.get(&symbol) {
            hasher.update(&[1]);
            crate::stable_hash::hash_return_borrow_summary(&mut hasher, summary);
        } else {
            hasher.update(&[0]);
        }
        if let Some(effects) = signatures.type_info.function_effects.get(&symbol) {
            hasher.update(&[1]);
            hasher.update(&effects.0.to_le_bytes());
        } else {
            hasher.update(&[0]);
        }
        hasher.update(&[u8::from(
            signatures.type_info.unsafe_functions.contains(&symbol),
        )]);
        hasher.update(&[u8::from(
            signatures.type_info.cold_functions.contains(&symbol),
        )]);

        // Const values
        if let Some(val) = signatures.type_info.ctfe_global_values.get(&symbol) {
            hasher.update(&[1]);
            hasher.update(&val.canonical_bytes());
        } else {
            hasher.update(&[0]);
        }

        // Interface methods
        if let Some(iface) = signatures.type_info.interfaces.get(&symbol) {
            hasher.update(&[1]);
            let mut methods = iface.methods.clone();
            methods.sort_unstable_by(|a, b| a.name.cmp(&b.name));
            hasher.update(&(methods.len() as u64).to_le_bytes());
            for method in &methods {
                hash_str(&mut hasher, &method.name);
                let bytes = crate::stable_hash::semantic_type_bytes(
                    method.sig_id,
                    &signatures.type_info.type_interner,
                    &signatures.symbols,
                );
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
        } else {
            hasher.update(&[0]);
        }
        hasher.update(&[u8::from(
            signatures.symbols.sealed_interfaces.contains(&symbol),
        )]);

        // Associated members (if type)
        if sym.kind.is_type() {
            let mut associated = Vec::new();
            for ((type_sym, member_name), member_sym) in
                signatures.symbols.associated_members.iter()
            {
                if *type_sym == symbol {
                    associated.push((member_name.clone(), *member_sym));
                }
            }
            associated.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            hasher.update(&(associated.len() as u64).to_le_bytes());
            for (name, member_sym) in associated {
                hash_str(&mut hasher, &name);
                hasher.update(&member_sym.file_id.to_le_bytes());
                hasher.update(&member_sym.local_id.0.to_le_bytes());
            }
        }
    } else {
        hasher.update(&[0]);
    }

    let fingerprint = hasher.finalize();
    HashEq::new(SymbolSurface {
        symbol,
        fingerprint,
    })
}

fn hash_str(hasher: &mut blake3::Hasher, text: &str) {
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
}
