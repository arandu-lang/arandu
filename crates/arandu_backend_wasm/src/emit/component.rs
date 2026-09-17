use rustc_hash::FxHashMap;
use wit_parser::WorldItem;
use wit_parser::abi::AbiVariant;

use arandu_middle::{Diagnostic, SymbolId};

use super::{ComponentExportMap, ExportStyle, WasmModuleBuilder};

impl<'a> WasmModuleBuilder<'a> {
    /// Build a WebAssembly **Component** binary by wrapping the core module
    /// with Component Model metadata derived from the generated WIT interface.
    ///
    /// `pkg_name` becomes the WIT package name (e.g. `"my-app"`); it must be a
    /// valid WIT identifier.
    ///
    /// If the program has no public functions, the core module is returned as-is
    /// (component encoding is skipped — there is nothing to export).
    ///
    /// # Errors
    ///
    /// Returns a [`Diagnostic`] on ICE / unsupported program structure, or if
    /// the `wit-component` encoding step fails.
    pub fn build_component(self, pkg_name: &str) -> Result<Vec<u8>, Diagnostic> {
        use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
        use wit_parser::Resolve;

        // Generate WIT text from public functions.
        let maybe_wit = crate::wit_gen::generate_wit(
            self.program,
            self.symbols,
            self.interner,
            self.layout_provider,
            pkg_name,
        );

        let Some(wit_text) = maybe_wit else {
            // No public functions → nothing to wrap; return core module.
            return self.build_core(ExportStyle::Host);
        };

        // Parse the WIT text into a wit_parser::Resolve + PackageId.
        let mut resolve = Resolve::new();
        let pkg_id = resolve
            .push_source("generated.wit", &wit_text)
            .map_err(|e| {
                Diagnostic::ice(
                    arandu_middle::diagnostics::DiagCode::ICEGEN002,
                    format!("WIT parse error: {e}"),
                    arandu_base::Span::new(0, 0, 0),
                )
            })?;

        // Find the world in the package (we always generate exactly one).
        let pkg = &resolve.packages[pkg_id];
        let world_id = pkg.worlds.values().copied().next().ok_or_else(|| {
            Diagnostic::ice(
                arandu_middle::diagnostics::DiagCode::ICEGEN002,
                "generated WIT package has no worlds".to_owned(),
                arandu_base::Span::new(0, 0, 0),
            )
        })?;

        // Derive the Canonical ABI export names from the parsed resolve and
        // rebuild the core module with them.
        let component_names = self.component_export_names(&resolve, world_id);
        let core_bytes = self.build_core(ExportStyle::Component(&component_names))?;

        // Embed component-type metadata into the core module bytes.
        let mut annotated = core_bytes;
        embed_component_metadata(&mut annotated, &resolve, world_id, StringEncoding::UTF8)
            .map_err(|e| {
                Diagnostic::ice(
                    arandu_middle::diagnostics::DiagCode::ICEGEN002,
                    format!("embed_component_metadata failed: {e:?}"),
                    arandu_base::Span::new(0, 0, 0),
                )
            })?;

        // Encode as a WebAssembly Component.
        let component_bytes = ComponentEncoder::default()
            .module(&annotated)
            .map_err(|e| {
                Diagnostic::ice(
                    arandu_middle::diagnostics::DiagCode::ICEGEN002,
                    format!("ComponentEncoder::module failed: {e:?}"),
                    arandu_base::Span::new(0, 0, 0),
                )
            })?
            .validate(true)
            .encode()
            .map_err(|e| {
                Diagnostic::ice(
                    arandu_middle::diagnostics::DiagCode::ICEGEN002,
                    format!("ComponentEncoder::encode failed: {e:?}"),
                    arandu_base::Span::new(0, 0, 0),
                )
            })?;

        Ok(component_bytes)
    }

    /// Compute the Canonical ABI core export names and signatures
    /// expected by the `wit-component` decoder for every public function.
    ///
    /// The names and signatures are derived from the parsed WIT `Resolve` so that the
    /// mangling logic and Canonical ABI calling conventions are never duplicated.
    fn component_export_names(
        &self,
        resolve: &wit_parser::Resolve,
        world_id: wit_parser::WorldId,
    ) -> ComponentExportMap {
        // Index public program funcs by their WIT (kebab-case) name.
        let mut public_by_kebab: FxHashMap<String, SymbolId> = FxHashMap::default();
        for func in &self.program.funcs {
            let Some(sym) = self.symbols.try_get(func.symbol) else {
                continue;
            };
            if sym.is_public {
                public_by_kebab.insert(crate::wit_gen::to_wit_ident(&sym.name), func.symbol);
                if let Some(method) = crate::wit_gen::extract_method_name(&sym.name) {
                    public_by_kebab.insert(crate::wit_gen::to_wit_ident(method), func.symbol);
                }
            }
        }

        let mut names: ComponentExportMap = FxHashMap::default();
        let world = &resolve.worlds[world_id];
        for key in world.exports.keys() {
            match &world.exports[key] {
                WorldItem::Interface { id, .. } => {
                    let interface_name = resolve.name_world_key(key);
                    for f in resolve.interfaces[*id].functions.values() {
                        if let Some(&symbol) = public_by_kebab.get(&f.name) {
                            let core = f
                                .legacy_core_export_name(Some(&interface_name))
                                .into_owned();
                            let sig = resolve.wasm_signature(AbiVariant::GuestExport, f);
                            names.insert(symbol, (core, sig));
                        }
                    }
                }
                WorldItem::Function(f) => {
                    if let Some(&symbol) = public_by_kebab.get(&f.name) {
                        let core = f.legacy_core_export_name(None).into_owned();
                        let sig = resolve.wasm_signature(AbiVariant::GuestExport, f);
                        names.insert(symbol, (core, sig));
                    }
                }
                _ => {}
            }
        }
        names
    }
}
