use arandu_fuzz_support::Target;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const MAX_CANDIDATE_BYTES: usize = 1024 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);
const EMI_CORPUS_MARKER: &str =
    "const EMI_CORPUS: &[(&str, &str, Option<ExpectedObservation>)] = &[";
const PREFLIGHT_PROCESS_GROUP_ENV: &str = "ARANDU_SMITH_PREFLIGHT_PROCESS_GROUP";
static NEXT_PREFLIGHT_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
use arandu_fuzz_support::process_job::terminate_process_group;
#[cfg(windows)]
use arandu_fuzz_support::process_job::ProcessJob as PreflightJob;

pub fn promote(root: &Path, mut args: impl Iterator<Item = String>) -> i32 {
    let Some(artifact_arg) = args.next() else {
        eprintln!("promote-fuzz-artifact: missing artifact directory");
        return 2;
    };
    let Some(name) = args.next() else {
        eprintln!("promote-fuzz-artifact: missing fixture name");
        return 2;
    };
    if args.next().is_some() || !valid_fixture_name(&name) {
        eprintln!("promote-fuzz-artifact: expected <artifact-dir> <lower-kebab-name>");
        return 2;
    }

    let artifact_path = Path::new(&artifact_arg);
    let artifact_path = if artifact_path.is_absolute() {
        artifact_path.to_path_buf()
    } else {
        root.join(artifact_path)
    };
    match promote_inner(root, &artifact_path, &name) {
        Ok((source_path, provenance_path)) => {
            println!(
                "promote-fuzz-artifact: added {} and {} to the EMI regression corpus; review the working tree diff",
                source_path.display(),
                provenance_path.display()
            );
            0
        }
        Err(error) => {
            eprintln!("promote-fuzz-artifact: {error}");
            1
        }
    }
}

fn promote_inner(
    root: &Path,
    artifact_dir: &Path,
    name: &str,
) -> Result<(PathBuf, PathBuf), String> {
    promote_inner_with(root, artifact_dir, name, verify_in_subprocess)
}

pub fn verify_source(mut args: impl Iterator<Item = String>) -> i32 {
    let (Some(path), Some(seed)) = (args.next(), args.next()) else {
        eprintln!("verify-fuzz-source: expected <candidate.aru> <seed-hex>");
        return 2;
    };
    if args.next().is_some() {
        eprintln!("verify-fuzz-source: unexpected extra argument");
        return 2;
    }
    let seed = match parse_seed(&seed) {
        Ok(seed) => seed,
        Err(error) => {
            eprintln!("verify-fuzz-source: {error}");
            return 2;
        }
    };
    let bytes = match read_regular_file(Path::new(&path), MAX_CANDIDATE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("verify-fuzz-source: {error}");
            return 1;
        }
    };
    if bytes.is_empty()
        || bytes.len() > MAX_CANDIDATE_BYTES
        || bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || bytes.contains(&b'\r')
        || !bytes.ends_with(b"\n")
    {
        eprintln!(
            "verify-fuzz-source: candidate must be nonempty and bounded, without BOM, use LF, and end with a newline"
        );
        return 1;
    }
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("verify-fuzz-source: candidate is not UTF-8: {error}");
            return 1;
        }
    };
    match std::panic::catch_unwind(|| {
        arandu_fuzz_support::verify_emi_regression_source(source, seed)
    }) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("{error}");
            1
        }
        Err(_) => {
            eprintln!("cross-backend/EMI preflight panicked");
            1
        }
    }
}

fn verify_in_subprocess(source: &str, seed: u64) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate xtask for preflight: {error}"))?;
    let check_id = NEXT_PREFLIGHT_ID.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir();
    let candidate_path = temp_dir.join(format!(
        "arandu-fuzz-preflight-{}-{check_id}.aru",
        std::process::id()
    ));
    let report_path = temp_dir.join(format!(
        "arandu-fuzz-preflight-{}-{check_id}.log",
        std::process::id()
    ));
    #[cfg(windows)]
    let job = PreflightJob::new()?;
    write_new(&candidate_path, source.as_bytes())?;
    let result = (|| {
        let report = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&report_path)
            .map_err(|error| {
                format!(
                    "cannot create preflight log {}: {error}",
                    report_path.display()
                )
            })?;
        let mut command = Command::new(executable);
        command
            .arg("verify-fuzz-source")
            .arg(&candidate_path)
            .arg(format!("0x{seed:016x}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(report))
            .env(PREFLIGHT_PROCESS_GROUP_ENV, std::process::id().to_string());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start isolated preflight: {error}"))?;
        #[cfg(windows)]
        if let Err(error) = job.assign(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() < PREFLIGHT_TIMEOUT => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    #[cfg(unix)]
                    terminate_process_group(child.id());
                    #[cfg(windows)]
                    job.terminate();
                    let _ = child.kill();
                    let _ = child.wait();
                    let report = read_preflight_report(&report_path);
                    return Err(format!(
                        "preflight exceeded {PREFLIGHT_TIMEOUT:?} and was terminated{report}"
                    ));
                }
                Err(error) => {
                    #[cfg(unix)]
                    terminate_process_group(child.id());
                    #[cfg(windows)]
                    job.terminate();
                    let _ = child.kill();
                    let _ = child.wait();
                    let report = read_preflight_report(&report_path);
                    return Err(format!(
                        "failed while waiting for preflight: {error}{report}"
                    ));
                }
            }
        };
        let report = read_preflight_report(&report_path);
        if status.success() {
            Ok(())
        } else {
            Err(format!("preflight failed ({status}){report}"))
        }
    })();
    let _ = fs::remove_file(&candidate_path);
    let _ = fs::remove_file(&report_path);
    result
}

fn read_preflight_report(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|text| {
            if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            }
        })
        .unwrap_or_default()
}

fn promote_inner_with(
    root: &Path,
    artifact_dir: &Path,
    name: &str,
    mut verify: impl FnMut(&str, u64) -> Result<(), String>,
) -> Result<(PathBuf, PathBuf), String> {
    let artifact_dir = artifact_dir
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", artifact_dir.display()))?;
    if !artifact_dir.is_dir() {
        return Err(format!(
            "{} is not an artifact directory",
            artifact_dir.display()
        ));
    }

    let metadata_path = artifact_dir.join("metadata.txt");
    let metadata_bytes = read_regular_file(&metadata_path, MAX_METADATA_BYTES)?;
    let metadata_text = std::str::from_utf8(&metadata_bytes)
        .map_err(|error| format!("metadata.txt is not UTF-8: {error}"))?;
    let metadata = parse_metadata(metadata_text)?;
    let target_name = required_metadata(&metadata, "target")?;
    if Target::parse(target_name).is_none() {
        return Err(format!(
            "unsupported fuzz target in artifact: {target_name}"
        ));
    }
    let corpus_name = required_metadata(&metadata, "corpus")?;
    let failure_kind = required_metadata(&metadata, "failure_kind")?;
    let failure_scope = metadata
        .get("failure_scope")
        .copied()
        .filter(|scope| !scope.is_empty())
        .unwrap_or("not recorded");
    let seed = parse_seed(required_metadata(&metadata, "seed")?)?;
    let attempts = parse_count(
        required_metadata(&metadata, "shrink_attempts")?,
        "shrink_attempts",
    )?;
    let reductions = parse_count(
        required_metadata(&metadata, "shrink_reductions")?,
        "shrink_reductions",
    )?;
    if required_metadata(&metadata, "shrink_confirmed")? != "true" {
        return Err("artifact's minimized candidate was not confirmed; refusing promotion".into());
    }

    let candidate_path = artifact_dir.join("candidate.aru");
    let candidate_bytes = read_regular_file(&candidate_path, MAX_CANDIDATE_BYTES)?;
    if candidate_bytes.is_empty() || candidate_bytes.len() > MAX_CANDIDATE_BYTES {
        return Err(format!(
            "candidate size must be between 1 and {MAX_CANDIDATE_BYTES} bytes"
        ));
    }
    if candidate_bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || candidate_bytes.contains(&b'\r')
        || !candidate_bytes.ends_with(b"\n")
    {
        return Err(
            "candidate must be UTF-8 without BOM, use LF line endings, and end with a newline"
                .into(),
        );
    }
    let candidate = std::str::from_utf8(&candidate_bytes)
        .map_err(|error| format!("candidate.aru is not UTF-8: {error}"))?;
    let formatted_candidate = arandu_fmt::format_source(candidate);
    if arandu_fmt::format_source(&formatted_candidate) != formatted_candidate {
        return Err(
            "formatter output is not idempotent; refusing to promote a non-canonical fixture"
                .into(),
        );
    }
    let formatted_bytes = formatted_candidate.as_bytes();
    if formatted_bytes.is_empty()
        || formatted_bytes.len() > MAX_CANDIDATE_BYTES
        || formatted_bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || formatted_bytes.contains(&b'\r')
        || !formatted_bytes.ends_with(b"\n")
    {
        return Err("formatted candidate must be nonempty, bounded UTF-8 without BOM, use LF line endings, and end with a newline".into());
    }
    let fixture_name = format!("smith-{name}");
    let source_path = root
        .join("tests/regressions")
        .join(format!("{fixture_name}.aru"));
    let provenance_path = root
        .join("tests/regressions")
        .join(format!("{fixture_name}.provenance.md"));
    let seed_path = root
        .join("tests/regressions")
        .join(format!("{fixture_name}.seed"));
    for path in [&source_path, &provenance_path, &seed_path] {
        if path.exists() {
            return Err(format!("refusing to overwrite {}", path.display()));
        }
    }

    let smith_path = root.join("crates/arandu_fuzz_support/src/smith/emi.rs");
    let smith_source = fs::read_to_string(&smith_path)
        .map_err(|error| format!("cannot read {}: {error}", smith_path.display()))?;
    let updated_smith = add_emi_corpus_entry(&smith_source, &fixture_name)?;
    verify(&formatted_candidate, seed).map_err(|error| {
        format!("candidate failed Cranelift/C/Wasm plus EMI preflight: {error}")
    })?;

    let digest = blake3::hash(formatted_bytes);
    let provenance = format!(
        "# Promoted AranduSmith regression\n\n\
         - Source: `{}`\n\
         - Target: `{target_name}`\n\
         - Corpus: `{corpus_name}`\n\
         - Failure: `{failure_kind}` (`{failure_scope}`)\n\
         - Seed: `0x{seed:016x}` (saved in `{fixture_name}.seed`)\n\
         - Shrink attempts: {attempts}\n\
         - Accepted reductions: {reductions}\n\
         - Confirmed before promotion: yes\n\
         - Formatter: applied before preflight and promotion\n\
         - Cranelift/C/Wasm and EMI preflight: passed\n\
         - BLAKE3(source): `{digest}`\n\n\
         This fixture is compiled and exercised by the `emi_corpus_mutation_preserves_results_across_every_backend` test.\n",
        artifact_dir.display()
    );
    let encoded_seed = encode_seed(seed);
    let mut created = Vec::new();
    for (path, contents) in [
        (source_path.as_path(), formatted_bytes),
        (provenance_path.as_path(), provenance.as_bytes()),
        (seed_path.as_path(), encoded_seed.as_bytes()),
    ] {
        if let Err(error) = write_new(path, contents) {
            cleanup_created(&created);
            return Err(error);
        }
        created.push(path.to_path_buf());
    }

    if let Err(error) = atomic_replace(&smith_path, updated_smith.as_bytes()) {
        cleanup_created(&created);
        return Err(error);
    }
    Ok((source_path, provenance_path))
}

fn parse_metadata(text: &str) -> Result<BTreeMap<&str, &str>, String> {
    let mut fields = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("invalid metadata line {}", index + 1));
        };
        if key.is_empty() || value.is_empty() || fields.insert(key, value).is_some() {
            return Err(format!(
                "empty or duplicate metadata field on line {}",
                index + 1
            ));
        }
    }
    Ok(fields)
}

fn required_metadata<'a>(fields: &'a BTreeMap<&str, &str>, key: &str) -> Result<&'a str, String> {
    fields
        .get(key)
        .copied()
        .ok_or_else(|| format!("artifact metadata is missing {key}"))
}

fn parse_seed(value: &str) -> Result<u64, String> {
    value
        .strip_prefix("0x")
        .ok_or_else(|| "artifact seed must use 0x hexadecimal form".to_owned())
        .and_then(|hex| {
            u64::from_str_radix(hex, 16).map_err(|error| format!("invalid seed: {error}"))
        })
}

fn parse_count(value: &str, field: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|error| format!("invalid {field}: {error}"))
}

fn encode_seed(seed: u64) -> String {
    let bytes = seed.to_le_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(hex, "{byte:02x}").expect("write seed hex");
    }
    format!("encoding=hex\n{hex}\n")
}

fn valid_fixture_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name.as_bytes()[0].is_ascii_lowercase()
        && !name.ends_with('-')
        && !name.contains("--")
}

fn add_emi_corpus_entry(source: &str, fixture_name: &str) -> Result<String, String> {
    if source.contains(&format!("\"{fixture_name}\"")) {
        return Err(format!("EMI corpus already includes {fixture_name}"));
    }
    let table = source
        .find(EMI_CORPUS_MARKER)
        .ok_or_else(|| "cannot find EMI_CORPUS declaration in smith/emi.rs".to_owned())?;
    let closing = source[table..]
        .find("\n];")
        .map(|offset| table + offset)
        .ok_or_else(|| "cannot find EMI_CORPUS closing delimiter".to_owned())?;
    let entry = format!(
        "\n    (\n        \"{fixture_name}\",\n        include_str!(\"../../../../tests/regressions/{fixture_name}.aru\"),\n        None,\n    ),"
    );
    let mut updated = String::with_capacity(source.len() + entry.len());
    updated.push_str(&source[..closing]);
    updated.push_str(&entry);
    updated.push_str(&source[closing..]);
    Ok(updated)
}

fn write_new(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
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

fn read_regular_file(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    let read_limit = u64::try_from(max_bytes)
        .map_err(|_| format!("maximum size for {} is not representable", path.display()))?
        .saturating_add(1);
    let file =
        fs::File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "{} exceeds the {max_bytes}-byte size limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn atomic_replace(path: &Path, contents: &[u8]) -> Result<(), String> {
    let temp_path = path.with_extension(format!("promote-{}.tmp", std::process::id()));
    let result = (|| {
        write_new(&temp_path, contents)?;
        replace_file(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(temp_path, destination)
        .map_err(|error| format!("cannot replace {}: {error}", destination.display()))
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, destination: &Path) -> Result<(), String> {
    let backup = destination.with_extension(format!("promote-{}.bak", std::process::id()));
    fs::rename(destination, &backup).map_err(|error| {
        format!(
            "cannot stage {} for replacement: {error}",
            destination.display()
        )
    })?;
    match fs::rename(temp_path, destination) {
        Ok(()) => {
            let _ = fs::remove_file(backup);
            Ok(())
        }
        Err(error) => {
            let restore = fs::rename(&backup, destination);
            match restore {
                Ok(()) => Err(format!("cannot replace {}: {error}", destination.display())),
                Err(restore_error) => Err(format!(
                    "cannot replace {}: {error}; cannot restore backup: {restore_error}",
                    destination.display()
                )),
            }
        }
    }
}

fn cleanup_created(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_name_rejects_paths_and_noncanonical_names() {
        for name in ["", "../escape", "Upper", "double--dash", "tail-", "a name"] {
            assert!(!valid_fixture_name(name), "accepted {name:?}");
        }
        assert!(valid_fixture_name("c-backend-regression-17"));
    }

    #[test]
    fn metadata_parser_rejects_duplicate_fields() {
        assert!(parse_metadata("seed=0x1\nseed=0x2\n").is_err());
        assert_eq!(parse_seed("0x000000000000002a").unwrap(), 42);
        assert_eq!(encode_seed(42), "encoding=hex\n2a00000000000000\n");
    }

    #[test]
    fn bounded_file_reader_rejects_oversized_input() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "arandu-bounded-read-{}-{nonce}",
            std::process::id()
        ));
        fs::write(&path, b"12345").unwrap();

        assert!(read_regular_file(&path, 4)
            .unwrap_err()
            .contains("exceeds the 4-byte size limit"));
        assert_eq!(read_regular_file(&path, 5).unwrap(), b"12345");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn promoted_fixture_is_added_to_the_compiled_emi_corpus() {
        let smith_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/arandu_fuzz_support/src/smith/emi.rs");
        let source = fs::read_to_string(smith_path).unwrap();
        let updated = add_emi_corpus_entry(&source, "smith-regression").unwrap();
        assert!(updated.contains("\"smith-regression\""));
        assert!(updated.contains("../../../../tests/regressions/smith-regression.aru"));
        assert!(updated.contains("        None,\n    ),"));
        assert!(add_emi_corpus_entry(&updated, "smith-regression").is_err());
    }

    #[test]
    fn confirmed_artifact_is_promoted_with_provenance_and_corpus_registration() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "arandu-promote-artifact-{}-{}",
            std::process::id(),
            nonce
        ));
        let artifact_dir = root.join("artifact");
        let regression_dir = root.join("tests/regressions");
        let smith_path = root.join("crates/arandu_fuzz_support/src/smith/emi.rs");
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::create_dir_all(&regression_dir).unwrap();
        fs::create_dir_all(smith_path.parent().unwrap()).unwrap();
        fs::write(
            artifact_dir.join("metadata.txt"),
            "target=emi-corpus\ncorpus=vec\nseed=0x000000000000002a\nfailure_kind=codegen-mismatch\nfailure_scope=O2\nshrink_attempts=4\nshrink_reductions=2\nshrink_confirmed=true\nreplay=run-fuzz-seed\n",
        )
        .unwrap();
        let candidate = "func main(): int { return 2 }\n";
        let formatted_candidate = arandu_fmt::format_source(candidate);
        fs::write(artifact_dir.join("candidate.aru"), candidate).unwrap();
        let table = format!("{EMI_CORPUS_MARKER}\n];\n");
        fs::write(&smith_path, table).unwrap();

        let mut verified = false;
        let (source_path, provenance_path) =
            promote_inner_with(&root, &artifact_dir, "unit-case", |source, seed| {
                assert_eq!(source, formatted_candidate.as_str());
                assert_eq!(seed, 42);
                verified = true;
                Ok(())
            })
            .unwrap();

        assert!(verified);
        assert_eq!(
            fs::read_to_string(source_path).unwrap(),
            formatted_candidate
        );
        assert!(fs::read_to_string(provenance_path)
            .unwrap()
            .contains("Failure: `codegen-mismatch` (`O2`)"));
        assert!(
            fs::read_to_string(regression_dir.join("smith-unit-case.provenance.md"))
                .unwrap()
                .contains("Formatter: applied before preflight and promotion")
        );
        assert_eq!(
            fs::read_to_string(regression_dir.join("smith-unit-case.seed")).unwrap(),
            "encoding=hex\n2a00000000000000\n"
        );
        assert!(fs::read_to_string(smith_path)
            .unwrap()
            .contains("../../../tests/regressions/smith-unit-case.aru"));
        fs::remove_dir_all(root).unwrap();
    }
}
