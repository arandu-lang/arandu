//! Conservative in-process ELF x86_64 patching for incremental development.
//!
//! The persisted layout is untrusted. Patching is attempted only when the old
//! executable digest, ELF section geometry, function size, object kind and all
//! relocations can be proven safe. Every unsupported case returns `Ok(None)`
//! so the caller can perform a deterministic full link.

use std::collections::BTreeMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use arandu_backend_cranelift::CodegenUnit;
use arandu_backend_cranelift::object::{
    self, File as ObjectFile, Object, ObjectSection, ObjectSymbol, SymbolKind,
};
#[cfg(target_os = "linux")]
use arandu_backend_cranelift::object::{RelocationKind, RelocationTarget};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use tracing::trace;

use crate::artifact;
use crate::cli_error::CliFailure;

const ELF_LAYOUT_SCHEMA: u32 = 3;

#[cfg(target_os = "linux")]
struct PreparedPatch {
    file_offset: usize,
    name: String,
    hash: String,
    bytes: Vec<u8>,
}

/// Verified symbol locations inside a linked ELF executable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElfLayout {
    pub schema_version: u32,
    pub binary_hash: String,
    pub binary_size: u64,
    pub text_vaddr: u64,
    pub text_file_offset: u64,
    pub text_size: u64,
    /// Complete linked-input closure, including functions dead-stripped from
    /// the final symbol table.
    pub cgu_hashes: BTreeMap<String, String>,
    pub function_slots: BTreeMap<String, ElfSlot>,
    pub global_symbols: BTreeMap<String, u64>,
}

/// Exact code slot for one function inside `.text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElfSlot {
    pub symbol_name: String,
    pub vaddr: u64,
    pub file_offset: u64,
    pub code_size: u64,
    pub capacity: u64,
    pub cgu_hash: String,
}

/// Returns whether an untrusted recorded layout proves the exact current CGU
/// closure was linked into the verified executable.
pub fn layout_matches_cgus(layout_file: &Path, units: &[CodegenUnit]) -> bool {
    let Ok(bytes) = fs::read(layout_file) else {
        return false;
    };
    let Ok(layout) = serde_json::from_slice::<ElfLayout>(&bytes) else {
        return false;
    };
    if layout.schema_version != ELF_LAYOUT_SCHEMA || layout.cgu_hashes.len() != units.len() {
        return false;
    }
    units.iter().all(|unit| {
        layout
            .cgu_hashes
            .get(&unit.name)
            .is_some_and(|hash| hash == &unit.hash)
    })
}

/// Inspect a freshly linked ELF and atomically record its verified layout.
pub fn record_elf_layout(
    executable_path: &Path,
    cgu_objects: &[PathBuf],
    layout_file: &Path,
) -> Result<(), CliFailure> {
    let bytes = fs::read(executable_path).map_err(|error| {
        failure(
            "read linked ELF for incremental layout",
            executable_path,
            error.to_string(),
        )
    })?;
    let file = match ObjectFile::parse(bytes.as_slice()) {
        Ok(file)
            if file.format() == object::BinaryFormat::Elf
                && file.architecture() == object::Architecture::X86_64 =>
        {
            file
        }
        _ => return Ok(()),
    };
    let Some(text) = file.section_by_name(".text") else {
        return Ok(());
    };
    let Some((text_file_offset, text_size)) = text.file_range() else {
        return Ok(());
    };
    let text_vaddr = text.address();
    let Some(text_file_end) = text_file_offset.checked_add(text_size) else {
        return Ok(());
    };
    let Ok(binary_size) = u64::try_from(bytes.len()) else {
        return Ok(());
    };
    if text_file_end > binary_size {
        return Ok(());
    }

    let mut global_symbols = BTreeMap::new();
    for symbol in file.symbols() {
        if symbol.is_definition()
            && let Ok(name) = symbol.name()
            && !name.is_empty()
        {
            global_symbols.insert(name.to_owned(), symbol.address());
        }
    }

    let mut cgu_info = BTreeMap::new();
    for object_path in cgu_objects {
        let object_bytes = fs::read(object_path).map_err(|error| {
            failure(
                "read CGU object for ELF layout",
                object_path,
                error.to_string(),
            )
        })?;
        let object = ObjectFile::parse(object_bytes.as_slice()).map_err(|error| {
            failure(
                "parse CGU object for ELF layout",
                object_path,
                error.to_string(),
            )
        })?;
        if object.kind() != object::ObjectKind::Relocatable
            || object.architecture() != object::Architecture::X86_64
        {
            return Ok(());
        }
        let cgu_hash = cgu_hash_from_path(object_path).unwrap_or_default();
        for symbol in object.symbols() {
            if symbol.is_definition()
                && symbol.is_global()
                && symbol.kind() == SymbolKind::Text
                && let Ok(name) = symbol.name()
            {
                cgu_info.insert(name.to_owned(), (symbol.size(), cgu_hash.clone()));
            }
        }
    }

    let cgu_hashes = cgu_info
        .iter()
        .map(|(name, (_, hash))| (name.clone(), hash.clone()))
        .collect();
    let mut function_slots = BTreeMap::new();
    for (name, (code_size, cgu_hash)) in cgu_info {
        let Some(&vaddr) = global_symbols.get(&name) else {
            continue;
        };
        let Some(relative) = vaddr.checked_sub(text_vaddr) else {
            continue;
        };
        let Some(file_offset) = text_file_offset.checked_add(relative) else {
            continue;
        };
        let Some(slot_end) = file_offset.checked_add(code_size) else {
            continue;
        };
        if slot_end > text_file_end {
            continue;
        }
        function_slots.insert(
            name.clone(),
            ElfSlot {
                symbol_name: name,
                vaddr,
                file_offset,
                code_size,
                // Exact-size patches preserve unwind/symbol/layout metadata.
                capacity: code_size,
                cgu_hash,
            },
        );
    }

    let layout = ElfLayout {
        schema_version: ELF_LAYOUT_SCHEMA,
        binary_hash: blake3::hash(&bytes).to_hex().to_string(),
        binary_size,
        text_vaddr,
        text_file_offset,
        text_size,
        cgu_hashes,
        function_slots,
        global_symbols,
    };
    if let Some(parent) = layout_file.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            failure(
                "create ELF incremental layout directory",
                parent,
                error.to_string(),
            )
        })?;
    }
    let mut encoded = serde_json::to_vec_pretty(&layout).map_err(|error| {
        failure(
            "serialize ELF incremental layout",
            layout_file,
            error.to_string(),
        )
    })?;
    encoded.push(b'\n');
    artifact::atomic_replace(layout_file, &encoded)
}

/// Attempt in-place patching of whole function slots through a checked shared
/// mapping: a recompiled function must fill a slot of the same capacity.
///
/// `Ok(None)` means a safety or determinism precondition was not met.
///
/// `trusted_digest` is the digest of the artifact `executable_path` was copied
/// from, when the caller published it itself and checked it unchanged across
/// the copy; the executable is then not hashed before patching.
pub fn try_patch_elf_in_place(
    executable_path: &Path,
    layout_file: &Path,
    current_cgus: &[CodegenUnit],
    recompiled_cgus: &[(&CodegenUnit, &[u8])],
    trusted_digest: Option<&str>,
) -> Result<Option<String>, CliFailure> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            executable_path,
            layout_file,
            current_cgus,
            recompiled_cgus,
            trusted_digest,
        );
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    {
        try_patch_linux(
            executable_path,
            layout_file,
            current_cgus,
            recompiled_cgus,
            trusted_digest,
        )
    }
}

#[cfg(target_os = "linux")]
fn try_patch_linux(
    executable_path: &Path,
    layout_file: &Path,
    current_cgus: &[CodegenUnit],
    recompiled_cgus: &[(&CodegenUnit, &[u8])],
    trusted_digest: Option<&str>,
) -> Result<Option<String>, CliFailure> {
    if recompiled_cgus.is_empty() || !layout_file.is_file() {
        return Ok(None);
    }
    let layout_bytes = match fs::read(layout_file) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    let mut layout: ElfLayout = match serde_json::from_slice::<ElfLayout>(&layout_bytes) {
        Ok(layout) if layout.schema_version == ELF_LAYOUT_SCHEMA => layout,
        _ => return Ok(None),
    };
    let recompiled_names: std::collections::BTreeSet<_> = recompiled_cgus
        .iter()
        .map(|(unit, _)| unit.name.as_str())
        .collect();
    if layout.cgu_hashes.len() != current_cgus.len()
        || !current_cgus.iter().all(|unit| {
            layout.cgu_hashes.get(&unit.name).is_some_and(|old_hash| {
                recompiled_names.contains(unit.name.as_str()) || old_hash == &unit.hash
            })
        })
    {
        trace!("[in-process-elf] linked CGU closure changed");
        return Ok(None);
    }

    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(executable_path)
    {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    file.lock().map_err(|error| {
        failure(
            "lock ELF staging artifact",
            executable_path,
            error.to_string(),
        )
    })?;
    let Ok(binary_size) = file.metadata().map(|metadata| metadata.len()) else {
        return Ok(None);
    };
    if binary_size != layout.binary_size || binary_size == 0 {
        trace!("[in-process-elf] executable size changed");
        return Ok(None);
    }
    let file_size = usize::try_from(binary_size).map_err(|_| {
        failure(
            "map ELF staging artifact",
            executable_path,
            "artifact is too large for this process address space",
        )
    })?;
    // One shared mapping replaces reading the executable: verification,
    // patching and the new digest all use it. No `msync`: the patched pages
    // reach the published path through the page cache and the `rename`, and a
    // later process re-verifies the digest before trusting the file (HOT.5).
    let mut mapping = SharedMapping::new(&file, file_size, executable_path)?;
    let prepared = {
        let executable = mapping.bytes();
        let trusted = trusted_digest.is_some_and(|digest| digest == layout.binary_hash);
        if !trusted && blake3::hash(executable).to_hex().as_str() != layout.binary_hash {
            trace!("[in-process-elf] executable digest changed");
            return Ok(None);
        }
        match prepare_patches(executable, &layout, recompiled_cgus)? {
            Some(prepared) => prepared,
            None => return Ok(None),
        }
    };
    for patch in &prepared {
        let Some(slot) = mapping
            .bytes_mut()
            .get_mut(patch.file_offset..patch.file_offset + patch.bytes.len())
        else {
            return Ok(None);
        };
        slot.copy_from_slice(&patch.bytes);
    }
    let new_digest = blake3::hash(mapping.bytes()).to_hex().to_string();
    drop(mapping);

    for patch in prepared {
        layout
            .cgu_hashes
            .insert(patch.name.clone(), patch.hash.clone());
        if let Some(slot) = layout.function_slots.get_mut(&patch.name) {
            slot.cgu_hash = patch.hash;
        }
    }
    layout.binary_hash.clone_from(&new_digest);
    let mut encoded = serde_json::to_vec_pretty(&layout).map_err(|error| {
        failure(
            "serialize patched ELF layout",
            layout_file,
            error.to_string(),
        )
    })?;
    encoded.push(b'\n');
    artifact::atomic_replace(layout_file, &encoded)?;
    Ok(Some(new_digest))
}

/// The patches of `recompiled_cgus` against the verified `executable`, or
/// `None` when one cannot be proven safe.
#[cfg(target_os = "linux")]
fn prepare_patches(
    executable: &[u8],
    layout: &ElfLayout,
    recompiled_cgus: &[(&CodegenUnit, &[u8])],
) -> Result<Option<Vec<PreparedPatch>>, CliFailure> {
    let Ok(binary_size) = u64::try_from(executable.len()) else {
        return Ok(None);
    };
    let old_elf = match ObjectFile::parse(executable) {
        Ok(file)
            if file.format() == object::BinaryFormat::Elf
                && file.architecture() == object::Architecture::X86_64 =>
        {
            file
        }
        _ => return Ok(None),
    };
    let Some(text) = old_elf.section_by_name(".text") else {
        return Ok(None);
    };
    let Some((text_offset, text_size)) = text.file_range() else {
        return Ok(None);
    };
    if text.address() != layout.text_vaddr
        || text_offset != layout.text_file_offset
        || text_size != layout.text_size
    {
        return Ok(None);
    }
    let Some(text_end) = text_offset.checked_add(text_size) else {
        return Ok(None);
    };
    if text_end > binary_size {
        return Ok(None);
    }

    let mut prepared = Vec::with_capacity(recompiled_cgus.len());
    for (unit, object_bytes) in recompiled_cgus {
        let Some(slot) = layout.function_slots.get(&unit.name) else {
            return Ok(None);
        };
        if slot.symbol_name != unit.name || slot.capacity != slot.code_size {
            return Ok(None);
        }
        let object = match ObjectFile::parse(*object_bytes) {
            Ok(object)
                if object.kind() == object::ObjectKind::Relocatable
                    && object.format() == object::BinaryFormat::Elf
                    && object.architecture() == object::Architecture::X86_64 =>
            {
                object
            }
            _ => return Ok(None),
        };
        let Some(function) = object.symbols().find(|symbol| {
            symbol.is_definition()
                && symbol.kind() == SymbolKind::Text
                && symbol.name().is_ok_and(|name| name == unit.name)
        }) else {
            return Ok(None);
        };
        if function.size() != slot.code_size {
            trace!(
                "[in-process-elf] exact-size precondition failed for '{}'",
                unit.name
            );
            return Ok(None);
        }
        let Some(section_index) = function.section_index() else {
            return Ok(None);
        };
        let Ok(section) = object.section_by_index(section_index) else {
            return Ok(None);
        };
        let Ok(section_data) = section.data() else {
            return Ok(None);
        };
        let Ok(function_start) = usize::try_from(function.address()) else {
            return Ok(None);
        };
        let Ok(function_size) = usize::try_from(function.size()) else {
            return Ok(None);
        };
        let Some(function_end) = function_start.checked_add(function_size) else {
            return Ok(None);
        };
        let Some(raw_code) = section_data.get(function_start..function_end) else {
            return Ok(None);
        };
        let mut patch = raw_code.to_vec();

        for (relocation_address, relocation) in section.relocations() {
            if relocation_address < function.address()
                || relocation_address >= function.address().saturating_add(function.size())
            {
                continue;
            }
            let Some(relative_offset) = relocation_address.checked_sub(function.address()) else {
                return Ok(None);
            };
            let Ok(relocation_offset) = usize::try_from(relative_offset) else {
                return Ok(None);
            };
            let target_vaddr = match relocation.target() {
                RelocationTarget::Symbol(symbol_index) => {
                    let Ok(target) = object.symbol_by_index(symbol_index) else {
                        return Ok(None);
                    };
                    if target.is_definition() {
                        if target.kind() != SymbolKind::Text
                            || target.section_index() != Some(section_index)
                        {
                            // Data/rodata belongs to the new object, not to the old
                            // executable slot. A full link must place it.
                            return Ok(None);
                        }
                        let Some(relative) = target.address().checked_sub(function.address())
                        else {
                            return Ok(None);
                        };
                        let Some(address) = slot.vaddr.checked_add(relative) else {
                            return Ok(None);
                        };
                        address
                    } else {
                        let Ok(name) = target.name() else {
                            return Ok(None);
                        };
                        let lookup_name = if relocation.kind() == RelocationKind::GotRelative {
                            format!("{name}$got")
                        } else {
                            name.to_owned()
                        };
                        let Some(&address) = layout.global_symbols.get(&lookup_name) else {
                            return Ok(None);
                        };
                        address
                    }
                }
                // A section-relative relocation may target rodata or another
                // contribution whose final placement is linker-owned.
                RelocationTarget::Section(_) | RelocationTarget::Absolute => return Ok(None),
                _ => return Ok(None),
            };
            apply_relocation(
                &mut patch,
                relocation_offset,
                slot.vaddr,
                target_vaddr,
                relocation,
            )?;
        }

        let Some(slot_relative) = slot.vaddr.checked_sub(layout.text_vaddr) else {
            return Ok(None);
        };
        let Some(expected_offset) = layout.text_file_offset.checked_add(slot_relative) else {
            return Ok(None);
        };
        if expected_offset != slot.file_offset {
            return Ok(None);
        }
        let Some(patch_end) = slot.file_offset.checked_add(slot.code_size) else {
            return Ok(None);
        };
        if slot.file_offset < text_offset || patch_end > text_end || patch_end > binary_size {
            return Ok(None);
        }
        let Ok(file_offset) = usize::try_from(slot.file_offset) else {
            return Ok(None);
        };
        prepared.push(PreparedPatch {
            file_offset,
            name: unit.name.clone(),
            hash: unit.hash.clone(),
            bytes: patch,
        });
    }

    Ok(Some(prepared))
}

#[cfg(target_os = "linux")]
fn apply_relocation(
    patch: &mut [u8],
    offset: usize,
    function_vaddr: u64,
    target_vaddr: u64,
    relocation: object::Relocation,
) -> Result<(), CliFailure> {
    let place_vaddr = function_vaddr
        .checked_add(u64::try_from(offset).map_err(|_| {
            CliFailure::operational(
                "apply ELF relocation",
                None,
                "relocation offset does not fit u64",
            )
        })?)
        .ok_or_else(|| {
            CliFailure::operational("apply ELF relocation", None, "relocation address overflow")
        })?;
    let value = i128::from(target_vaddr) + i128::from(relocation.addend());
    match relocation.kind() {
        RelocationKind::Relative | RelocationKind::PltRelative | RelocationKind::GotRelative
            if relocation.size() == 32 =>
        {
            let displacement = value - i128::from(place_vaddr);
            let Ok(displacement) = i32::try_from(displacement) else {
                return Err(CliFailure::operational(
                    "apply ELF relocation",
                    None,
                    "32-bit PC-relative relocation is out of range",
                ));
            };
            write_relocation(patch, offset, &displacement.to_le_bytes())
        }
        RelocationKind::Absolute if relocation.size() == 64 => {
            let Ok(value) = u64::try_from(value) else {
                return Err(CliFailure::operational(
                    "apply ELF relocation",
                    None,
                    "absolute relocation is negative or overflows u64",
                ));
            };
            write_relocation(patch, offset, &value.to_le_bytes())
        }
        kind => Err(CliFailure::operational(
            "apply ELF relocation",
            None,
            format!(
                "unsupported relocation kind {kind:?} with {} bits",
                relocation.size()
            ),
        )),
    }
}

#[cfg(target_os = "linux")]
fn write_relocation(patch: &mut [u8], offset: usize, value: &[u8]) -> Result<(), CliFailure> {
    let end = offset.checked_add(value.len()).ok_or_else(|| {
        CliFailure::operational("apply ELF relocation", None, "relocation range overflow")
    })?;
    let Some(destination) = patch.get_mut(offset..end) else {
        return Err(CliFailure::operational(
            "apply ELF relocation",
            None,
            "relocation writes outside the function code range",
        ));
    };
    destination.copy_from_slice(value);
    Ok(())
}

/// A read-write shared mapping of a whole locked file, unmapped on drop.
#[cfg(target_os = "linux")]
struct SharedMapping {
    address: std::ptr::NonNull<u8>,
    len: usize,
}

#[cfg(target_os = "linux")]
impl SharedMapping {
    fn new(file: &File, len: usize, path: &Path) -> Result<Self, CliFailure> {
        use std::os::fd::AsRawFd;

        // SAFETY: `file` is open read-write and exclusively locked by the
        // caller for the mapping's lifetime, and `len` is its verified size.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(failure(
                "map ELF staging artifact",
                path,
                std::io::Error::last_os_error().to_string(),
            ));
        }
        let Some(address) = std::ptr::NonNull::new(address.cast::<u8>()) else {
            return Err(failure(
                "map ELF staging artifact",
                path,
                "mmap returned a null mapping",
            ));
        };
        Ok(Self { address, len })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping covers `len` readable bytes until drop, and
        // `bytes_mut` cannot alias it while this borrow lives.
        unsafe { std::slice::from_raw_parts(self.address.as_ptr(), self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `bytes`, with exclusive access through `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.address.as_ptr(), self.len) }
    }
}

#[cfg(target_os = "linux")]
impl Drop for SharedMapping {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping created in `new`; a failure only
        // leaks the address range.
        unsafe {
            libc::munmap(self.address.as_ptr().cast(), self.len);
        }
    }
}

fn cgu_hash_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let (_, hash) = stem.rsplit_once('-')?;
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| hash.to_owned())
}

fn failure(operation: &'static str, path: &Path, source: impl Into<String>) -> CliFailure {
    CliFailure::operational(operation, Some(path.to_path_buf()), source)
}
