use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arandu_fuzz_support::{Target, MAX_INPUT_BYTES};

const SIMPLE_SEED_TIMEOUT: Duration = Duration::from_secs(2);
const INCREMENTAL_SEED_TIMEOUT: Duration = Duration::from_secs(5);
const LSP_SEED_TIMEOUT: Duration = Duration::from_secs(10);
const SYNTHESIZED_SEED_TIMEOUT: Duration = Duration::from_secs(15);
const DIFFERENTIAL_SEED_TIMEOUT: Duration = Duration::from_secs(120);
static NEXT_SEQUENCE_PREFLIGHT_ID: AtomicU64 = AtomicU64::new(0);

struct Entry {
    path: PathBuf,
    targets: Vec<Target>,
    origin: String,
    risk: String,
}

pub fn check(root: &Path) -> i32 {
    if let Err(error) = validate_fuzz_target_wiring(root) {
        eprintln!("check-fuzz-regressions: {error}");
        return 1;
    }

    let corpus = root.join("tests/fuzz-regressions");
    let entries = match load_manifest(&corpus) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("check-fuzz-regressions: {error}");
            return 1;
        }
    };
    let executable = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("check-fuzz-regressions: cannot locate xtask: {error}");
            return 1;
        }
    };

    for entry in &entries {
        for target in &entry.targets {
            let timeout = seed_timeout(*target);
            let mut child = match Command::new(&executable)
                .arg("run-fuzz-seed")
                .arg(target.name())
                .arg(&entry.path)
                .stdin(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    eprintln!(
                        "cannot start {} for {}: {error}",
                        target.name(),
                        entry.path.display()
                    );
                    return 1;
                }
            };
            let started = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => break,
                    Ok(Some(status)) => {
                        eprintln!(
                            "seed failed: {} target={} origin={} risk={} ({status})",
                            entry.path.display(),
                            target.name(),
                            entry.origin,
                            entry.risk
                        );
                        return 1;
                    }
                    Ok(None) if started.elapsed() < timeout => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        eprintln!(
                            "seed timed out after {:?}: {} target={} origin={} risk={}",
                            timeout,
                            entry.path.display(),
                            target.name(),
                            entry.origin,
                            entry.risk
                        );
                        return 1;
                    }
                    Err(error) => {
                        let _ = child.kill();
                        eprintln!("cannot wait for seed worker: {error}");
                        return 1;
                    }
                }
            }
        }
    }
    let executions: usize = entries.iter().map(|entry| entry.targets.len()).sum();
    println!(
        "check-fuzz-regressions: ok ({} seeds, {executions} isolated executions, max={} bytes, target-specific timeouts up to {:?})",
        entries.len(),
        MAX_INPUT_BYTES,
        DIFFERENTIAL_SEED_TIMEOUT
    );
    0
}

fn validate_fuzz_target_wiring(root: &Path) -> Result<(), String> {
    let manifest_path = root.join("arandu_fuzz/Cargo.toml");
    let cargo_manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let workflow_path = root.join(".github/workflows/fuzz.yml");
    let workflow = fs::read_to_string(&workflow_path)
        .map_err(|error| format!("cannot read {}: {error}", workflow_path.display()))?;

    let mut binary_names = BTreeSet::new();
    for target in Target::ALL {
        let binary = target.fuzz_binary_name();
        if !binary_names.insert(binary) {
            return Err(format!("duplicate cargo-fuzz binary mapping: {binary}"));
        }
        let target_path = root
            .join("arandu_fuzz/fuzz_targets")
            .join(format!("{binary}.rs"));
        if !target_path.is_file() {
            return Err(format!(
                "target {} maps to missing fuzz entrypoint {}",
                target.name(),
                target_path.display()
            ));
        }
        let name_entry = format!("name = \"{binary}\"");
        let path_entry = format!("path = \"fuzz_targets/{binary}.rs\"");
        let has_matching_binary = cargo_manifest.split("[[bin]]").skip(1).any(|entry| {
            entry.lines().any(|line| line.trim() == name_entry)
                && entry.lines().any(|line| line.trim() == path_entry)
        });
        if !has_matching_binary {
            return Err(format!(
                "target {} has no [[bin]] entry for {binary} with path {path_entry} in {}",
                target.name(),
                manifest_path.display()
            ));
        }
        if !workflow
            .lines()
            .any(|line| line.contains(&format!("name: {binary},")))
        {
            return Err(format!(
                "target {} is absent from the scheduled fuzz matrix in {}",
                target.name(),
                workflow_path.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn seed_timeout(target: Target) -> Duration {
    match target {
        Target::Lex
        | Target::Syntax
        | Target::Pipeline
        | Target::Cycles
        | Target::LexSimd
        | Target::Structured
        | Target::GenRef => SIMPLE_SEED_TIMEOUT,
        Target::Incremental | Target::ModuleGraph | Target::IncrementalCutoff => {
            INCREMENTAL_SEED_TIMEOUT
        }
        Target::LspSession => LSP_SEED_TIMEOUT,
        Target::Synthesized => SYNTHESIZED_SEED_TIMEOUT,
        Target::SynthesizedC
        | Target::SynthesizedWasm
        | Target::EmiCorpus
        | Target::SynthesizedAll => DIFFERENTIAL_SEED_TIMEOUT,
    }
}

pub fn run_one(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(target) = args.next().and_then(|name| Target::parse(&name)) else {
        eprintln!("run-fuzz-seed: invalid target");
        return 2;
    };
    let Some(path) = args.next() else {
        eprintln!("run-fuzz-seed: missing seed path");
        return 2;
    };
    let data = match decode_seed(Path::new(&path)) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("run-fuzz-seed: {error}");
            return 2;
        }
    };
    match std::panic::catch_unwind(|| arandu_fuzz_support::run(target, &data)) {
        Ok(()) => 0,
        Err(_) => {
            eprintln!(
                "run-fuzz-seed: target {} panicked for {path}",
                target.name()
            );
            1
        }
    }
}

/// Verify a minimized byte-sequence candidate, then add it to the regression
/// seed manifest for an incremental or LSP target.
pub fn promote_sequence(root: &Path, mut args: impl Iterator<Item = String>) -> i32 {
    let (Some(target), Some(sequence_hex), Some(name), Some(description)) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        eprintln!(
            "promote-fuzz-sequence: expected <incremental|incremental-cutoff|lsp-session> <hex-sequence> <lower-kebab-name> <risk-description>"
        );
        return 2;
    };
    if args.next().is_some() {
        eprintln!("promote-fuzz-sequence: unexpected extra argument");
        return 2;
    }
    let Some(target) = Target::parse(&target).filter(|target| {
        matches!(
            target,
            Target::Incremental | Target::IncrementalCutoff | Target::LspSession
        )
    }) else {
        eprintln!(
            "promote-fuzz-sequence: target must be incremental, incremental-cutoff, or lsp-session"
        );
        return 2;
    };
    let max_sequence_bytes = if target == Target::LspSession {
        4096
    } else {
        MAX_INPUT_BYTES
    };
    if sequence_hex.len() > max_sequence_bytes.saturating_mul(2) {
        eprintln!(
            "promote-fuzz-sequence: sequence exceeds {max_sequence_bytes} bytes for {}",
            target.name()
        );
        return 2;
    }
    if !valid_seed_name(&name)
        || description.trim().is_empty()
        || description.contains(['\t', '\r', '\n'])
    {
        eprintln!("promote-fuzz-sequence: invalid seed name or risk description");
        return 2;
    }
    let sequence = match decode_sequence_hex(&sequence_hex) {
        Ok(sequence) => sequence,
        Err(error) => {
            eprintln!("promote-fuzz-sequence: {error}");
            return 2;
        }
    };
    match promote_sequence_inner(
        root,
        target,
        &sequence,
        &name,
        &description,
        verify_sequence,
    ) {
        Ok(path) => {
            let Some(corpus_name) = sequence_corpus_name(target) else {
                eprintln!(
                    "promote-fuzz-sequence: target {} has no libFuzzer sequence corpus",
                    target.name()
                );
                return 1;
            };
            println!(
                "promote-fuzz-sequence: added {} and arandu_fuzz/corpus/{corpus_name}/{name} for target {}; review the working tree diff",
                path.display(),
                target.name()
            );
            0
        }
        Err(error) => {
            eprintln!("promote-fuzz-sequence: {error}");
            1
        }
    }
}

fn promote_sequence_inner(
    root: &Path,
    target: Target,
    sequence: &[u8],
    name: &str,
    description: &str,
    mut verify: impl FnMut(Target, &[u8]) -> Result<(), String>,
) -> Result<PathBuf, String> {
    if !matches!(
        target,
        Target::Incremental | Target::IncrementalCutoff | Target::LspSession
    ) {
        return Err(format!(
            "target {} does not accept byte-sequence seeds",
            target.name()
        ));
    }
    let max_bytes = if target == Target::LspSession {
        4096
    } else {
        MAX_INPUT_BYTES
    };
    if sequence.is_empty() || sequence.len() > max_bytes {
        return Err(format!(
            "sequence size must be between 1 and {max_bytes} bytes for {}",
            target.name()
        ));
    }
    if !valid_seed_name(name)
        || description.trim().is_empty()
        || description.contains(['\t', '\r', '\n'])
    {
        return Err("invalid seed name or risk description".into());
    }

    let corpus = root.join("tests/fuzz-regressions");
    let fuzz_corpus =
        root.join("arandu_fuzz/corpus")
            .join(sequence_corpus_name(target).ok_or_else(|| {
                format!(
                    "target {} does not accept byte-sequence seeds",
                    target.name()
                )
            })?);
    let manifest_path = corpus.join("manifest.tsv");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let entries = load_manifest(&corpus)?;
    let seed_file_name = format!("{name}.seed");
    if entries
        .iter()
        .any(|entry| entry.path.file_name() == Some(std::ffi::OsStr::new(&seed_file_name)))
    {
        return Err(format!("seed name {name:?} is already in the manifest"));
    }
    if entries
        .iter()
        .any(|entry| decode_seed(&entry.path).is_ok_and(|data| data == sequence))
    {
        return Err("sequence bytes are already present in the regression manifest".into());
    }

    let seed_path = corpus.join(format!("{name}.seed"));
    let fuzz_seed_path = fuzz_corpus.join(name);
    let seed_contents = encode_sequence(sequence);
    let manifest_line = format!(
        "{name}.seed\t{}\trfc:0022\t{}\n",
        target.name(),
        description.trim()
    );
    let mut updated_manifest = manifest;
    if !updated_manifest.ends_with('\n') {
        updated_manifest.push('\n');
    }
    updated_manifest.push_str(&manifest_line);
    let manifest_temp = manifest_path.with_extension(format!("promote-{}.tmp", std::process::id()));
    if seed_path.exists() {
        return Err(format!("refusing to overwrite {}", seed_path.display()));
    }
    if fuzz_seed_path.exists() {
        return Err(format!(
            "refusing to overwrite libFuzzer seed {}",
            fuzz_seed_path.display()
        ));
    }
    verify(target, sequence).map_err(|error| {
        format!("current target replay failed; refusing to promote a non-green seed: {error}")
    })?;
    write_new(&seed_path, seed_contents.as_bytes())?;
    let mut fuzz_seed_created = false;
    let mut manifest_temp_created = false;
    let result = (|| {
        fs::create_dir_all(&fuzz_corpus).map_err(|error| {
            format!(
                "cannot create libFuzzer corpus {}: {error}",
                fuzz_corpus.display()
            )
        })?;
        write_new(&fuzz_seed_path, sequence)?;
        fuzz_seed_created = true;
        write_new(&manifest_temp, updated_manifest.as_bytes())?;
        manifest_temp_created = true;
        replace_manifest(&manifest_temp, &manifest_path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&seed_path);
        if fuzz_seed_created {
            let _ = fs::remove_file(&fuzz_seed_path);
        }
        if manifest_temp_created {
            let _ = fs::remove_file(&manifest_temp);
        }
        return Err(error);
    }
    Ok(seed_path)
}

fn sequence_corpus_name(target: Target) -> Option<&'static str> {
    match target {
        Target::Incremental => Some("fuzz_incremental"),
        Target::IncrementalCutoff => Some("fuzz_incremental_cutoff"),
        Target::LspSession => Some("fuzz_lsp_session"),
        _ => None,
    }
}

fn verify_sequence(target: Target, sequence: &[u8]) -> Result<(), String> {
    let executable = env::current_exe().map_err(|error| format!("cannot locate xtask: {error}"))?;
    let nonce = NEXT_SEQUENCE_PREFLIGHT_ID.fetch_add(1, Ordering::Relaxed);
    let temp_dir = env::temp_dir();
    let seed_path = temp_dir.join(format!(
        "arandu-sequence-preflight-{}-{nonce}.seed",
        std::process::id()
    ));
    write_new(&seed_path, encode_sequence(sequence).as_bytes())?;
    let result = (|| {
        let mut child = Command::new(executable)
            .arg("run-fuzz-seed")
            .arg(target.name())
            .arg(&seed_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start target preflight: {error}"))?;
        let started = Instant::now();
        let timeout = seed_timeout(target);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(format!("{} exited with {status}", target.name())),
                Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{} exceeded {timeout:?}", target.name()));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "cannot wait for {} preflight: {error}",
                        target.name()
                    ));
                }
            }
        }
    })();
    let _ = fs::remove_file(seed_path);
    result
}

fn decode_sequence_hex(text: &str) -> Result<Vec<u8>, String> {
    if text.is_empty()
        || !text.len().is_multiple_of(2)
        || !text.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("sequence must be nonempty even-length hexadecimal".into());
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
            u8::from_str_radix(pair, 16).map_err(|error| format!("invalid sequence hex: {error}"))
        })
        .collect()
}

fn encode_sequence(sequence: &[u8]) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(sequence.len() * 2);
    for byte in sequence {
        write!(hex, "{byte:02x}").expect("write seed hex");
    }
    format!("encoding=hex\n{hex}\n")
}

fn valid_seed_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.as_bytes()[0].is_ascii_lowercase()
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn write_new(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    if let Err(error) = file.write_all(contents).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(format!("cannot write {}: {error}", path.display()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_manifest(temp: &Path, manifest: &Path) -> Result<(), String> {
    fs::rename(temp, manifest).map_err(|error| format!("cannot update manifest: {error}"))
}

#[cfg(windows)]
fn replace_manifest(temp: &Path, manifest: &Path) -> Result<(), String> {
    let backup = manifest.with_extension(format!("promote-{}.bak", std::process::id()));
    fs::rename(manifest, &backup).map_err(|error| format!("cannot stage manifest: {error}"))?;
    match fs::rename(temp, manifest) {
        Ok(()) => {
            let _ = fs::remove_file(backup);
            Ok(())
        }
        Err(error) => {
            let restore = fs::rename(&backup, manifest);
            match restore {
                Ok(()) => Err(format!("cannot update manifest: {error}")),
                Err(restore_error) => Err(format!(
                    "cannot update manifest: {error}; cannot restore backup: {restore_error}"
                )),
            }
        }
    }
}

fn load_manifest(corpus: &Path) -> Result<Vec<Entry>, String> {
    let manifest_path = corpus.join("manifest.tsv");
    let text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let mut entries = Vec::new();
    let mut paths = BTreeSet::new();
    let mut contents: BTreeMap<Vec<u8>, PathBuf> = BTreeMap::new();
    let canonical_corpus = corpus
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", corpus.display()))?;

    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || fields.iter().any(|field| field.trim().is_empty()) {
            return Err(format!(
                "manifest line {} must have 4 non-empty TSV fields",
                line_index + 1
            ));
        }
        let relative = Path::new(fields[0]);
        if relative.is_absolute() {
            return Err(format!(
                "absolute seed path on manifest line {}",
                line_index + 1
            ));
        }
        let path = corpus.join(relative);
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
        if !canonical.starts_with(&canonical_corpus) {
            return Err(format!("seed escapes corpus: {}", path.display()));
        }
        if !paths.insert(canonical.clone()) {
            return Err(format!("duplicate seed path: {}", path.display()));
        }
        let data = decode_seed(&canonical)?;
        if data.len() > MAX_INPUT_BYTES {
            return Err(format!(
                "seed exceeds {MAX_INPUT_BYTES} bytes: {}",
                path.display()
            ));
        }
        if let Some(first) = contents.insert(data, canonical.clone()) {
            return Err(format!(
                "duplicate seed content: {} and {}",
                first.display(),
                path.display()
            ));
        }
        let targets = fields[1]
            .split(',')
            .map(|name| {
                Target::parse(name)
                    .ok_or_else(|| format!("unknown target {name:?} on line {}", line_index + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.push(Entry {
            path: canonical,
            targets,
            origin: fields[2].into(),
            risk: fields[3].into(),
        });
    }
    if entries.is_empty() {
        return Err("manifest has no seeds".into());
    }
    Ok(entries)
}

fn decode_seed(path: &Path) -> Result<Vec<u8>, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    decode_seed_bytes(&bytes, &path.display().to_string())
}

fn decode_seed_bytes(bytes: &[u8], source: &str) -> Result<Vec<u8>, String> {
    let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
        return Err(format!("missing encoding header in {source}"));
    };
    let header = bytes[..newline]
        .strip_suffix(b"\r")
        .unwrap_or(&bytes[..newline]);
    let payload = &bytes[newline + 1..];
    if header == b"encoding=hex" {
        let hex = payload;
        let compact: Vec<u8> = hex
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        if !compact.len().is_multiple_of(2) {
            return Err(format!("odd hex length in {source}"));
        }
        let (chunks, _) = compact.as_chunks::<2>();
        return chunks
            .iter()
            .map(|pair| {
                let text =
                    std::str::from_utf8(pair).map_err(|_| format!("non-ASCII hex in {source}"))?;
                u8::from_str_radix(text, 16).map_err(|_| format!("invalid hex in {source}"))
            })
            .collect();
    }
    if header == b"encoding=utf8" {
        return Ok(payload.to_vec());
    }
    Err(format!("missing encoding header in {source}"))
}

#[cfg(test)]
mod tests {
    use super::{
        decode_seed, decode_seed_bytes, decode_sequence_hex, fs, load_manifest,
        promote_sequence_inner, seed_timeout, Path, Target,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static NEXT_TEMP_CORPUS: AtomicU64 = AtomicU64::new(0);

    fn temp_corpus() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "arandu-promote-sequence-{}-{}",
            std::process::id(),
            NEXT_TEMP_CORPUS.fetch_add(1, Ordering::Relaxed)
        ));
        let corpus = root.join("tests/fuzz-regressions");
        fs::create_dir_all(&corpus).unwrap();
        for target in [
            "fuzz_incremental",
            "fuzz_incremental_cutoff",
            "fuzz_lsp_session",
        ] {
            fs::create_dir_all(root.join("arandu_fuzz/corpus").join(target)).unwrap();
        }
        fs::write(corpus.join("baseline.seed"), b"encoding=hex\n00\n").unwrap();
        fs::write(
            corpus.join("manifest.tsv"),
            b"# path<TAB>targets<TAB>origin<TAB>risk\nbaseline.seed\tincremental\ttest\tbaseline seed\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn sequence_hex_parser_requires_nonempty_even_ascii_hex() {
        assert_eq!(decode_sequence_hex("01aF"), Ok(vec![1, 0xaf]));
        assert!(decode_sequence_hex("").is_err());
        assert!(decode_sequence_hex("0").is_err());
        assert!(decode_sequence_hex("0g").is_err());
    }

    #[test]
    fn promoted_incremental_sequence_is_replayed_and_added_to_manifest() {
        let root = temp_corpus();
        let seed_path = promote_sequence_inner(
            &root,
            Target::Incremental,
            &[1, 2, 3],
            "minimized-invalidation",
            "public edit must invalidate importer diagnostics",
            |target, sequence| {
                assert_eq!(target, Target::Incremental);
                assert_eq!(sequence, [1, 2, 3]);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(decode_seed(&seed_path).unwrap(), [1, 2, 3]);
        assert_eq!(
            fs::read(root.join("arandu_fuzz/corpus/fuzz_incremental/minimized-invalidation"))
                .unwrap(),
            [1, 2, 3]
        );
        let entries = load_manifest(&root.join("tests/fuzz-regressions")).unwrap();
        let promoted = entries
            .iter()
            .find(|entry| entry.path == seed_path)
            .expect("promoted seed is recorded in the manifest");
        assert!(promoted.targets.contains(&Target::Incremental));
        assert_eq!(promoted.origin, "rfc:0022");
        assert_eq!(
            promoted.risk,
            "public edit must invalidate importer diagnostics"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn promoted_sequence_enters_the_matching_libfuzzer_corpus() {
        let root = temp_corpus();
        for (target, name, sequence, expected_corpus) in [
            (
                Target::IncrementalCutoff,
                "private-cutoff-regression",
                &[4, 5][..],
                "fuzz_incremental_cutoff",
            ),
            (
                Target::LspSession,
                "stale-lsp-regression",
                &[6, 7, 8][..],
                "fuzz_lsp_session",
            ),
        ] {
            promote_sequence_inner(
                &root,
                target,
                sequence,
                name,
                "replayable regression for the target suite",
                |actual_target, actual_sequence| {
                    assert_eq!(actual_target, target);
                    assert_eq!(actual_sequence, sequence);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(
                fs::read(
                    root.join("arandu_fuzz/corpus")
                        .join(expected_corpus)
                        .join(name)
                )
                .unwrap(),
                sequence
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_sequence_preflight_leaves_corpus_unchanged() {
        let root = temp_corpus();
        let manifest_path = root.join("tests/fuzz-regressions/manifest.tsv");
        let original_manifest = fs::read(&manifest_path).unwrap();
        let result = promote_sequence_inner(
            &root,
            Target::Incremental,
            &[1, 2, 3],
            "must-not-promote",
            "candidate still fails replay",
            |_, _| Err("preflight failed".to_owned()),
        );

        assert!(result.is_err());
        assert!(!root
            .join("tests/fuzz-regressions/must-not-promote.seed")
            .exists());
        assert!(!root
            .join("arandu_fuzz/corpus/fuzz_incremental/must-not-promote")
            .exists());
        assert_eq!(fs::read(manifest_path).unwrap(), original_manifest);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_manifest_install_rolls_back_both_seed_copies() {
        let root = temp_corpus();
        let manifest_path = root.join("tests/fuzz-regressions/manifest.tsv");
        let original_manifest = fs::read(&manifest_path).unwrap();
        let manifest_temp =
            manifest_path.with_extension(format!("promote-{}.tmp", std::process::id()));
        fs::write(&manifest_temp, b"owned by another operation").unwrap();

        let result = promote_sequence_inner(
            &root,
            Target::Incremental,
            &[9, 10, 11],
            "rollback-case",
            "verify seed copy rollback on manifest installation failure",
            |_, _| Ok(()),
        );

        assert!(result.is_err());
        assert!(!root
            .join("tests/fuzz-regressions/rollback-case.seed")
            .exists());
        assert!(!root
            .join("arandu_fuzz/corpus/fuzz_incremental/rollback-case")
            .exists());
        assert_eq!(fs::read(manifest_path).unwrap(), original_manifest);
        assert_eq!(
            fs::read(&manifest_temp).unwrap(),
            b"owned by another operation"
        );
        fs::remove_file(manifest_temp).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_libfuzzer_seed_is_not_overwritten_or_replayed() {
        let root = temp_corpus();
        let existing = root.join("arandu_fuzz/corpus/fuzz_incremental/keep-me");
        fs::write(&existing, b"existing corpus input").unwrap();
        let result = promote_sequence_inner(
            &root,
            Target::Incremental,
            &[12, 13],
            "keep-me",
            "must not replace an existing fuzz input",
            |_, _| panic!("a path conflict must be rejected before replay"),
        );

        assert!(result.is_err());
        assert_eq!(fs::read(existing).unwrap(), b"existing corpus input");
        assert!(!root.join("tests/fuzz-regressions/keep-me.seed").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn seed_headers_accept_lf_and_crlf_checkouts() {
        assert_eq!(
            decode_seed_bytes(b"encoding=hex\nf09f92\n", "lf.seed"),
            Ok(vec![0xf0, 0x9f, 0x92])
        );
        assert_eq!(
            decode_seed_bytes(b"encoding=hex\r\nf09f92\r\n", "crlf.seed"),
            Ok(vec![0xf0, 0x9f, 0x92])
        );
        assert_eq!(
            decode_seed_bytes(b"encoding=utf8\r\nfunc main() {}\r\n", "utf8.seed"),
            Ok(b"func main() {}\r\n".to_vec())
        );
    }

    #[test]
    fn lsp_session_seed_matches_the_libfuzzer_corpus_entry() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask manifest lives below the workspace root");
        let entries = load_manifest(&root.join("tests/fuzz-regressions"))
            .expect("versioned fuzz regression manifest must be valid");
        let entry = entries
            .iter()
            .find(|entry| {
                entry
                    .path
                    .file_name()
                    .is_some_and(|name| name == "lsp-session.seed")
            })
            .expect("LSP session seed must be in the regression manifest");
        assert!(entry.targets.contains(&Target::LspSession));

        let regression = decode_seed(&entry.path).expect("decode versioned LSP session seed");
        let libfuzzer =
            fs::read(root.join("arandu_fuzz/corpus/fuzz_lsp_session/seed-basic-session"))
                .expect("read libFuzzer LSP session seed");
        assert_eq!(regression, libfuzzer);
    }

    #[test]
    fn expensive_regression_targets_have_bounded_but_realistic_timeouts() {
        assert_eq!(seed_timeout(Target::Lex), Duration::from_secs(2));
        assert_eq!(seed_timeout(Target::Incremental), Duration::from_secs(5));
        assert_eq!(seed_timeout(Target::LspSession), Duration::from_secs(10));
        assert_eq!(seed_timeout(Target::Synthesized), Duration::from_secs(15));
        assert_eq!(
            seed_timeout(Target::SynthesizedAll),
            Duration::from_secs(120)
        );
        assert_eq!(seed_timeout(Target::EmiCorpus), Duration::from_secs(120));
    }
}
