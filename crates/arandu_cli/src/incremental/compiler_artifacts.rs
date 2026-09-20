//! Atomic publication of versioned compiler-IR sidecars.
//!
//! Encoding is pure and owned by `arandu_query`; this module is the filesystem
//! boundary. The Salsa database itself is never serialized.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use arandu_query::{
    ArandCompilerDb, ArtifactDigest, CompilerArtifactKind, LowerAmirArtifacts, SourceFile,
    decode_compiler_artifact, encode_compiler_artifact,
};

use crate::cli_error::CliFailure;

pub(super) const COMPILER_ARTIFACT_DIRECTORY: &str = "compiler-artifacts";

pub(super) fn compiler_artifact_path(
    incremental_dir: &Path,
    kind: CompilerArtifactKind,
) -> PathBuf {
    let filename = match kind {
        CompilerArtifactKind::Air => "current.air",
        CompilerArtifactKind::Amir => "current.amir",
        CompilerArtifactKind::Ameta => "current.ameta",
    };
    incremental_dir
        .join(COMPILER_ARTIFACT_DIRECTORY)
        .join(filename)
}

/// Publish `.air`, `.amir`, and `.ameta` under the profile's incremental
/// directory. Existing byte-identical artifacts are retained.
pub fn publish_compiler_artifacts(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    artifacts: &LowerAmirArtifacts,
    incremental_dir: &Path,
) -> Result<(), CliFailure> {
    let source = db.source_text(*file.file_id(db));
    let input_digest = ArtifactDigest::of(source.as_bytes());
    let parsed = arandu_query::passes::parse(db, file);
    let parse_result = parsed.as_ref();
    let program = parse_result.as_ref().map_err(|error| {
        CliFailure::operational(
            "serialize AIR",
            Some(db.file_path(*file.file_id(db)).as_ref().clone()),
            error.to_string(),
        )
    })?;

    // These canonical textual projections are deliberately payload schemas,
    // not a serialization of Salsa internals. Their binary envelope carries
    // kind/version/length and BLAKE3 integrity.
    let air_payload = program.dump(&source);
    let amir_payload = artifacts.amir.pretty_print(
        artifacts.type_check.symbols.as_ref(),
        &artifacts.type_check.type_info.type_interner,
    );
    let ameta_payload = canonical_export_metadata(artifacts);

    let directory = incremental_dir.join(COMPILER_ARTIFACT_DIRECTORY);
    fs::create_dir_all(&directory).map_err(|error| {
        CliFailure::operational(
            "create compiler artifact directory",
            Some(directory.clone()),
            error.to_string(),
        )
    })?;

    publish_one(
        &directory,
        CompilerArtifactKind::Air,
        input_digest,
        air_payload.as_bytes(),
    )?;
    publish_one(
        &directory,
        CompilerArtifactKind::Amir,
        input_digest,
        amir_payload.as_bytes(),
    )?;
    publish_one(
        &directory,
        CompilerArtifactKind::Ameta,
        input_digest,
        ameta_payload.as_bytes(),
    )?;

    Ok(())
}

fn canonical_export_metadata(artifacts: &LowerAmirArtifacts) -> String {
    let symbols = artifacts.type_check.symbols.as_ref();
    let type_info = artifacts.type_check.type_info.as_ref();
    let global = symbols.global_scope();
    let mut exported = symbols
        .iter()
        .filter(|symbol| symbol.scope == global && symbol.is_public)
        .collect::<Vec<_>>();
    exported.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.file_id.cmp(&right.id.file_id))
            .then_with(|| left.id.local_id.0.cmp(&right.id.local_id.0))
    });

    let mut output = String::new();
    for symbol in exported {
        let kind = symbol_kind_tag(symbol.kind);
        let _ = write!(output, "{kind}\t{}", symbol.name);
        if let Some(ty) = type_info.decl_type(symbol.id) {
            output.push('\t');
            output.push_str(&ty.display(symbols, &type_info.type_interner));
        }
        output.push('\n');
    }
    output
}

/// Stable schema tag mirrored from the semantic symbol enum. Keep existing
/// values immutable; append only.
const fn symbol_kind_tag(kind: arandu_middle::SymbolKind) -> u8 {
    use arandu_middle::SymbolKind;
    match kind {
        SymbolKind::Module => 0,
        SymbolKind::ImportValue => 1,
        SymbolKind::ImportType => 2,
        SymbolKind::Func => 3,
        SymbolKind::Const => 4,
        SymbolKind::TypeAlias => 5,
        SymbolKind::Struct => 6,
        SymbolKind::Enum => 7,
        SymbolKind::Interface => 8,
        SymbolKind::ExternFunc => 9,
        SymbolKind::Param => 10,
        SymbolKind::Local => 11,
        SymbolKind::Field => 12,
        SymbolKind::EnumVariant => 13,
        SymbolKind::TypeParam => 14,
        SymbolKind::NamespaceMember => 15,
        SymbolKind::AssociatedFunc => 16,
        SymbolKind::ConstParam => 17,
    }
}

fn publish_one(
    directory: &Path,
    kind: CompilerArtifactKind,
    input_digest: ArtifactDigest,
    payload: &[u8],
) -> Result<PathBuf, CliFailure> {
    let incremental_dir = directory.parent().ok_or_else(|| {
        CliFailure::operational(
            "publish compiler artifact",
            Some(directory.to_path_buf()),
            "compiler artifact directory has no parent",
        )
    })?;
    let path = compiler_artifact_path(incremental_dir, kind);
    let bytes = encode_compiler_artifact(kind, input_digest, payload);
    if let Ok(existing) = fs::read(&path)
        && existing == bytes
        && decode_compiler_artifact(&existing, kind).is_ok()
    {
        return Ok(path);
    }

    decode_compiler_artifact(&bytes, kind).map_err(|error| {
        CliFailure::operational(
            "verify compiler artifact before publication",
            Some(path.clone()),
            error.to_string(),
        )
    })?;
    crate::artifact::atomic_replace(&path, &bytes)?;
    Ok(path)
}
