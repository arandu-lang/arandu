use super::DeclarationHir;
use crate::ctfe::item_staged_typing as item_typing;
use crate::db::HashEq;
use crate::passes::{declaration_signatures, item_source_input, parse, PreparedHir};
use crate::{ArandCompilerDb, SourceFile, StableHash};

pub(super) fn declaration_hir_from(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    headers_only: bool,
) -> HashEq<DeclarationHir> {
    let declared = if headers_only {
        crate::passes::header_signatures(db, file)
    } else {
        declaration_signatures(db, file)
    };
    let parsed = parse(db, file);
    let mut checked = (**declared).clone();
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"DeclarationHir/v1");
    let mut hir = None;
    let mut diagnostics = Vec::new();
    if let Ok(program) = &**parsed {
        let mut bodies = Vec::new();
        program.for_each_decl_recursive(|_, declaration| {
            if let arandu_parser::TopLevelDecl::Func(function) = declaration {
                bodies.push(function.body.span);
            }
        });
        // Resolution visits all bodies. Their errors belong to their own
        // function units, not to the declaration-only HIR context.
        checked.diagnostics.retain(|diagnostic| {
            !bodies.iter().any(|span| {
                diagnostic.span.file_id == span.file_id
                    && diagnostic.span.start >= span.start
                    && diagnostic.span.end <= span.end
            })
        });
        program.for_each_decl_recursive(|_, declaration| {
            if !matches!(declaration, arandu_parser::TopLevelDecl::Const(_)) {
                return;
            }
            if let Some(symbol) = arandu_semantics::primary_def_key(declaration)
                .and_then(|key| checked.resolved.definitions.get(&key))
                .copied()
            {
                if headers_only {
                    let mut item = arandu_semantics::check_item_body_only(
                        declared,
                        program,
                        symbol,
                        crate::passes::database_target_info(db),
                    );
                    crate::ctfe::globals::stage_initializer(
                        &mut item,
                        program,
                        symbol,
                        *db.target_config().data_layout(db),
                    );
                    checked.type_info_mut().merge_from(&item.type_info);
                    checked.diagnostics.extend(item.diagnostics);
                } else {
                    let item = item_typing(db, file, symbol);
                    checked.type_info_mut().merge_from(&item.type_info);
                    checked.diagnostics.extend(item.diagnostics.iter().cloned());
                }
                fingerprint.update(item_source_input(db, file, symbol).stable_hash().as_bytes());
            }
        });
        let text = file.text(db);
        for import in &program.imports {
            let span = import.span();
            if let Some(source) = text.get(span.start as usize..span.end as usize) {
                fingerprint.update(source.as_bytes());
            }
        }
        match arandu_semantics::lower_declarations_to_hir(&mut checked, program) {
            Ok(declarations) => {
                for &id in &declarations.decls {
                    let declaration = declarations.pool.decl(id);
                    let span = declaration.span();
                    fingerprint.update(&span.file_id.to_le_bytes());
                    fingerprint.update(&span.start.to_le_bytes());
                    fingerprint.update(&span.end.to_le_bytes());
                    if let arandu_middle::hir::HirDecl::Func(function) = declaration {
                        fingerprint
                            .update(&[u8::from(function.is_async), u8::from(function.no_fallback)]);
                        for parameter in declarations.pool.params_list(function.params) {
                            let receiver = match parameter.receiver_kind {
                                None => 0,
                                Some(arandu_middle::hir::ReceiverKind::Shared) => 1,
                                Some(arandu_middle::hir::ReceiverKind::Mut) => 2,
                                Some(arandu_middle::hir::ReceiverKind::Own) => 3,
                            };
                            fingerprint.update(&[u8::from(parameter.is_receiver), receiver]);
                        }
                    }
                }
                hir = Some(declarations);
            }
            Err(errors) => diagnostics = errors,
        }
    }
    fingerprint.update(crate::stable_hash::type_signature_hash(&checked).as_bytes());
    fingerprint.update(diagnostics.stable_hash().as_bytes());
    fingerprint.update(&[u8::from(hir.is_some())]);
    let fingerprint = fingerprint.finalize();
    HashEq::new(DeclarationHir {
        artifacts: PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint: fingerprint,
        },
        fingerprint,
    })
}
