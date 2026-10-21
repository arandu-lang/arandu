//! Package configuration, background discovery sync, and import alias bindings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arandu_query::db::SourceFile;
use arandu_query::{scan_aru_entries, ModuleBinding};

use super::path::{normalize_path_soft, package_relative_path};
use super::types::{PackageState, ServerState};

impl ServerState {
    /// Commit the loader-verified stdlib root, including editor overlays opened
    /// before background discovery completed. Never replace their source text.
    pub fn configure_stdlib_root(&mut self, root: PathBuf) {
        let normalized_root = normalize_path_soft(&root);
        for &id in self.by_uri.values() {
            if let Some(doc) = self.docs.get(id) {
                if normalize_path_soft(doc.path.as_ref()).starts_with(&normalized_root) {
                    self.host.db_mut().trust_stdlib_source(doc.source);
                }
            }
        }
        self.host.db_mut().set_stdlib_root(root);
    }

    pub fn configure_package(
        &mut self,
        project: crate::workspace::WorkspaceProject,
    ) -> Result<(), String> {
        let crate::workspace::WorkspaceProject {
            manifest_path,
            manifest_data,
            manifest_hash,
            package_src,
            entries,
            stdlib_root,
            module_plan,
            module_files: _,
        } = project;
        let trusted_stdlib_root = stdlib_root.clone();
        let package_name = manifest_data.name.clone();
        let mut open_by_path = HashMap::<PathBuf, SourceFile>::new();
        for &id in self.by_uri.values() {
            if let Some(doc) = self.docs.get(id) {
                open_by_path
                    .entry(normalize_path_soft(doc.path.as_ref()))
                    .or_insert(doc.source);
            }
        }
        let package_bindings = module_plan
            .as_ref()
            .map(|plan| {
                plan.bindings
                    .iter()
                    .map(|binding| {
                        let normalized = normalize_path_soft(&binding.physical);
                        let source = open_by_path.get(&normalized).copied().ok_or_else(|| {
                            format!(
                                "package module {} was not registered before graph commit",
                                binding.physical.display()
                            )
                        })?;
                        Ok((
                            binding.logical.clone(),
                            ModuleBinding {
                                package: binding.package,
                                target: binding.target,
                                module: binding.module,
                                file: source,
                            },
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .transpose()?;
        let package_map = module_plan
            .as_ref()
            .zip(package_bindings)
            .map(|(plan, bindings)| arandu_query::ResolvedPackageMap {
                current_package: plan.current_package,
                current_target: plan.current_target,
                bindings,
            });
        let (_, listing, _) =
            self.host
                .configure_package_with_map(arandu_query::PackageConfiguration {
                    manifest_path: manifest_path.clone(),
                    manifest_data,
                    manifest_hash,
                    package_src: package_src.clone(),
                    entries: entries.clone(),
                    stdlib_root,
                    package_map,
                });
        self.package = Some(PackageState {
            manifest_path,
            package_src,
            package_name,
            listing,
            entries,
        });

        // Files may have been opened before background project discovery
        // finished. Attach their import aliases without replacing overlays.
        let known: Vec<_> = self
            .by_uri
            .values()
            .filter_map(|&id| self.docs.get(id))
            .map(|doc| (doc.path.as_ref().clone(), doc.source))
            .collect();
        let normalized_stdlib = trusted_stdlib_root
            .as_ref()
            .map(|root| normalize_path_soft(root));
        for (path, source) in known {
            if normalized_stdlib
                .as_ref()
                .is_some_and(|root| normalize_path_soft(&path).starts_with(root))
            {
                self.host.db_mut().trust_stdlib_source(source);
            }
            self.register_package_aliases(&path, source);
        }
        Ok(())
    }

    #[must_use]
    pub fn package_manifest_path(&self) -> Option<PathBuf> {
        self.package
            .as_ref()
            .map(|package| package.manifest_path.clone())
    }

    /// Rescan package structure outside Salsa queries and commit one listing
    /// input only when create/delete/rename changed its semantic contents.
    pub fn refresh_package_listing(&mut self) -> bool {
        let Some(package) = self.package.as_ref() else {
            return false;
        };
        let entries = scan_aru_entries(&package.package_src);
        if entries == package.entries {
            return false;
        }
        let package_src = package.package_src.clone();
        let package_name = package.package_name.clone();
        let listing = package.listing;
        self.host.set_directory_entries(listing, entries.clone());
        if let Some(package) = self.package.as_mut() {
            package.entries.clone_from(&entries);
        }

        // Register exact import keys from the authoritative relative listing.
        // This avoids deriving package identity from platform-specific URI
        // spellings and keeps goto attached to the already-known document.
        for rel in &entries {
            let absolute = package_src.join(rel);
            let normalized = normalize_path_soft(&absolute);
            let source = self
                .by_uri
                .values()
                .filter_map(|&id| self.docs.get(id))
                .find(|doc| normalize_path_soft(doc.path.as_ref()) == normalized)
                .map(|doc| doc.source);
            if let Some(source) = source {
                self.register_import_key(format!("{package_name}/{rel}"), source);
                self.register_import_key(rel.clone(), source);
            }
        }
        true
    }

    pub(super) fn package_keys_for_path(&self, path: &Path) -> Vec<String> {
        let Some(package) = self.package.as_ref() else {
            return Vec::new();
        };
        let Some(rel) = package_relative_path(path, &package.package_src) else {
            return Vec::new();
        };
        vec![format!("{}/{}", package.package_name, rel), rel]
    }

    pub(super) fn register_package_aliases(&mut self, path: &Path, source: SourceFile) {
        for key in self.package_keys_for_path(path) {
            self.register_import_key(key, source);
        }
    }

    pub(super) fn register_import_key(&mut self, key: String, source: SourceFile) {
        let source_id = *source.file_id(self.host.db());
        let already_current = self
            .host
            .db()
            .source_file_by_path(&key)
            .is_some_and(|known| *known.file_id(self.host.db()) == source_id);
        if !already_current {
            if self.host.db().is_registered(&key) {
                self.host.unregister_source_file(&key);
            }
            self.host.register_source_file(key.clone(), source);
        }
        for owned in self.package_aliases.values_mut() {
            owned.retain(|candidate| candidate != &key);
        }
        let owned = self.package_aliases.entry(source_id).or_default();
        if !owned.contains(&key) {
            owned.push(key);
        }
    }

    pub(super) fn unregister_package_aliases(&mut self, path: &Path, source_id: Option<u32>) {
        let mut keys = self.package_keys_for_path(path);
        if let Some(source_id) = source_id {
            keys.extend(self.package_aliases.remove(&source_id).unwrap_or_default());
        }
        keys.sort();
        keys.dedup();
        for key in keys {
            if self.host.db().is_registered(&key) {
                self.host.unregister_source_file(&key);
            }
        }
    }
}
