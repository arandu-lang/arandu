//! Test discovery, execution, and harness publishing.

use arandu_query::ArandCompilerDb;
use std::fs;
use std::path::{Path, PathBuf};

use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::pipeline::{ensure_host_jit_layout, pipeline_lower_checked};
use crate::project::{self, ProjectFlags};
use crate::test_runner;

#[derive(serde::Serialize)]
pub struct DiscoveryReport {
    pub schema: &'static str,
    pub cases: Vec<DiscoveryCase>,
}

#[derive(serde::Serialize)]
pub struct DiscoveryCase {
    pub id: String,
    pub path: String,
    pub line: u32,
    pub column_utf16: u32,
}

pub fn discovery_position(text: &str, byte_offset: u32) -> (u32, u32) {
    let offset = usize::try_from(byte_offset)
        .unwrap_or(text.len())
        .min(text.len());
    let prefix = text.get(..offset).unwrap_or(text);
    let line =
        u32::try_from(prefix.bytes().filter(|byte| *byte == b'\n').count()).unwrap_or(u32::MAX);
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column_utf16 =
        u32::try_from(prefix[line_start..].encode_utf16().count()).unwrap_or(u32::MAX);
    (line, column_utf16)
}

pub fn discovery_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn project_test_sources(
    ctx: &project::ProjectContext,
) -> Result<Vec<(PathBuf, String)>, String> {
    let mut sources = std::collections::BTreeMap::new();
    let source_root = ctx
        .entry_path
        .parent()
        .ok_or_else(|| format!("entry {} has no source directory", ctx.entry_path.display()))?;
    for (directory, target) in [
        (source_root.to_path_buf(), ctx.target_kind.as_str()),
        (ctx.root.join("tests"), "test"),
    ] {
        if !directory.is_dir() {
            continue;
        }
        for relative in arandu_query::scan_aru_entries(&directory) {
            let candidate = directory.join(&relative);
            let physical = fs::canonicalize(&candidate).map_err(|error| {
                format!(
                    "cannot canonicalize test source {}: {error}",
                    candidate.display()
                )
            })?;
            if !physical.starts_with(&ctx.root) || !physical.is_file() {
                return Err(format!(
                    "test source {} escapes the project root or is not a file",
                    candidate.display()
                ));
            }
            let module = relative
                .strip_suffix(".aru")
                .unwrap_or(&relative)
                .replace('\\', "/");
            sources.insert(physical, format!("{target}::{module}"));
        }
    }
    Ok(sources.into_iter().collect())
}

// Use the same deterministic name in discovery and in each harness child.
// Checking all source symbols also avoids collisions with ordinary non-test functions.
fn doctest_function_name(
    db: &arandu_query::DatabaseImpl,
    file: arandu_query::SourceFile,
    index: usize,
) -> String {
    let resolution = arandu_query::passes::resolve(db, file);
    let mut name = format!("doctest_{index}");
    while resolution
        .symbols
        .iter()
        .any(|symbol| symbol.name.as_str() == name)
    {
        name.push('_');
    }
    name
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_project_test_list(
    start: &Path,
    flags: &ProjectFlags,
    list_only: bool,
    exact: Option<&str>,
    filter: Option<&str>,
    harness_child: bool,
    runner: &test_runner::RunnerOptions,
    data_layout: arandu_middle::layout::DataLayout,
    doc_tests: bool,
) -> CliResult {
    let mut db = arandu_query::DatabaseImpl::new();
    db.set_target_config(data_layout);
    let ctx = project::load_project(&mut db, start, flags).map_err(|error| {
        CliFailure::operational("load test project", Some(start.to_path_buf()), error)
    })?;
    let sources = project_test_sources(&ctx).map_err(|error| {
        CliFailure::operational("discover test sources", Some(ctx.root.clone()), error)
    })?;
    let mut registry = arandu_codegen::testing::TestRegistry::default();
    let mut discovered = Vec::new();
    for (path, module) in sources {
        let key = path.to_string_lossy().into_owned();
        let file = if let Some(existing) = db.as_source_db().resolve_module_path(&key) {
            existing
        } else {
            let text = fs::read_to_string(&path).map_err(|error| {
                CliFailure::operational("read test source", Some(path.clone()), error.to_string())
            })?;
            db.new_file(key, text)
        };
        let checked = arandu_query::passes::type_check(&db, file);
        if checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error)
        {
            return Err(CliFailure::diagnostics(
                checked.diagnostics.clone(),
                Some(path),
            ));
        }
        let text = file.text(&db);
        for case in arandu_query::file_test_manifest(&db, file).iter() {
            let id = format!("{}::{module}::{}", ctx.name, case.name);
            let (line, column_utf16) = discovery_position(text, case.span.start);
            discovered.push(DiscoveryCase {
                id: id.clone(),
                path: discovery_path(&ctx.root, &path),
                line,
                column_utf16,
            });
            registry.insert(arandu_codegen::testing::TestEntry {
                id,
                function: case.name.to_string(),
            });
        }
        if doc_tests {
            for (idx, doctest) in arandu_query::file_doctests(&db, file).iter().enumerate() {
                let function = doctest_function_name(&db, file, idx);
                let id = format!("{}::{module}::{function}", ctx.name);
                discovered.push(DiscoveryCase {
                    id: id.clone(),
                    path: discovery_path(&ctx.root, &path),
                    line: doctest.line_offset,
                    column_utf16: 0,
                });
                registry.insert(arandu_codegen::testing::TestEntry { id, function });
            }
        }
    }
    let cases: Vec<String> = registry.iter().map(|entry| entry.id.clone()).collect();
    let cases: Vec<String> = match filter {
        Some(filter) => cases
            .into_iter()
            .filter(|case| case.contains(filter))
            .collect(),
        None => cases,
    };
    let harness = if harness_child {
        None
    } else {
        Some(artifact::publish_test_harness(&ctx.root, &registry)?)
    };
    if let Some(exact) = exact {
        if !cases.iter().any(|case| case == exact) {
            return Err(CliFailure::operational(
                "select test case",
                Some(ctx.root),
                format!("test case `{exact}` was not found"),
            ));
        }
        if list_only {
            println!("{exact}");
        } else if harness_child {
            let start = std::time::Instant::now();
            let sequence: u64 = std::env::var("ARANDU_TEST_SEQUENCE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let temp_root = std::env::var_os("ARANDU_TEST_TEMP_ROOT").map(PathBuf::from);
            arandu_runtime::testing_runtime::init_test_context(exact, sequence, temp_root);
            let result = run_exact_test(&mut db, &ctx, exact, data_layout);
            let outcome = arandu_runtime::testing_runtime::finish_test_context();

            let (status, failure) =
                if outcome.status == arandu_codegen::testing::TestStatus::Skipped {
                    (
                        arandu_codegen::testing::TestStatus::Skipped,
                        outcome.failure,
                    )
                } else if outcome.status == arandu_codegen::testing::TestStatus::Failed {
                    (arandu_codegen::testing::TestStatus::Failed, outcome.failure)
                } else if let Err(err) = &result {
                    // Preserve labels/notes in captured stderr; keep the IPC
                    // summary concise and free of internal Rust/IR Debug data.
                    err.render();
                    (
                        arandu_codegen::testing::TestStatus::Failed,
                        Some(arandu_codegen::testing::TestFailure::simple(format!(
                            "{err}"
                        ))),
                    )
                } else {
                    (arandu_codegen::testing::TestStatus::Passed, None)
                };

            let event = arandu_codegen::testing::TestEventV1 {
                sequence,
                id: exact.to_string(),
                status,
                duration: start.elapsed(),
                stdout: arandu_codegen::testing::CapturedOutput {
                    bytes: Vec::new(),
                    truncated: false,
                },
                stderr: arandu_codegen::testing::CapturedOutput {
                    bytes: Vec::new(),
                    truncated: false,
                },
                failure,
                secondary_failures: outcome.secondary_failures,
                logs: outcome.logs,
                logs_truncated: outcome.logs_truncated,
            };
            let _ = test_runner::send_child_event(sequence, &event);
            if status == arandu_codegen::testing::TestStatus::Failed {
                return Err(CliFailure::operational(
                    "run test case",
                    None,
                    "test failed",
                ));
            }
            return Ok(CliSuccess::Done);
        } else {
            let passed = test_runner::run_cases(
                &ctx.root,
                &ctx.stdlib.path,
                vec![exact.to_string()],
                runner,
            )
            .map_err(|error| CliFailure::operational("run tests", None, error))?;
            if !passed {
                return Err(CliFailure::operational(
                    "run tests",
                    None,
                    "one or more tests failed",
                ));
            }
        }
    } else if list_only {
        if runner.format == test_runner::TestOutputFormat::Json {
            discovered.retain(|case| cases.iter().any(|id| id == &case.id));
            discovered.sort_by(|left, right| left.id.cmp(&right.id));
            let report = DiscoveryReport {
                schema: arandu_codegen::testing::TEST_LIST_PROTOCOL_V1,
                cases: discovered,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| {
                    CliFailure::operational("serialize test discovery", None, error.to_string())
                })?
            );
        } else {
            for case in cases {
                println!("{case}");
            }
        }
    } else {
        if runner.format == test_runner::TestOutputFormat::Human {
            let harness_manifest = harness.as_ref().ok_or_else(|| {
                CliFailure::operational("run tests", None, "missing published harness")
            })?;
            eprintln!(
                "test harness: {} cases (manifest={})",
                cases.len(),
                harness_manifest.display(),
            );
        }
        let passed = test_runner::run_cases(&ctx.root, &ctx.stdlib.path, cases, runner)
            .map_err(|error| CliFailure::operational("run tests", None, error))?;
        if !passed {
            return Err(CliFailure::operational(
                "run tests",
                None,
                "one or more tests failed",
            ));
        }
    }
    Ok(CliSuccess::Done)
}

pub fn run_exact_test(
    db: &mut arandu_query::DatabaseImpl,
    ctx: &project::ProjectContext,
    exact: &str,
    data_layout: arandu_middle::layout::DataLayout,
) -> CliResult {
    let function = exact.rsplit("::").next().unwrap_or_default();
    let sources = project_test_sources(ctx).map_err(|error| {
        CliFailure::operational("discover test sources", Some(ctx.root.clone()), error)
    })?;
    for (path, module) in sources {
        let target = format!("{}::{}::{}", ctx.name, module, function);
        if !exact.eq(&target) {
            continue;
        }
        let filepath = path.to_string_lossy().into_owned();
        let file = db.source_file_by_path(&filepath).ok_or_else(|| {
            CliFailure::operational(
                "run test case",
                Some(path.clone()),
                "test source was not registered in the project database",
            )
        })?;

        let is_source_test = arandu_query::file_test_manifest(db, file)
            .iter()
            .any(|case| case.name.as_str() == function);
        let snippet = if is_source_test {
            None
        } else {
            let doctests = arandu_query::file_doctests(db, file);
            doctests.iter().enumerate().find_map(|(index, snippet)| {
                (doctest_function_name(db, file, index) == function).then(|| snippet.clone())
            })
        };
        if let Some(snippet) = snippet {
            let original_text = file.text(db).to_string();
            let synthetic_source = format!(
                "{original_text}\n\nfunc {function}(): void {{\n{}\n}}\n",
                snippet.code
            );
            let doctest_key = format!("{}#{function}", filepath);
            let doctest_file = db.new_file(doctest_key, synthetic_source);
            let artifacts = pipeline_lower_checked(db, doctest_file)
                .map_err(|diagnostics| CliFailure::diagnostics(diagnostics, Some(path.clone())))?
                .artifacts;
            if snippet.no_run {
                return Ok(CliSuccess::Done);
            }
            ensure_host_jit_layout(data_layout)?;
            let backend = arandu_backend_cranelift::CraneliftBackend::try_new()
                .map_err(|diag| CliFailure::diagnostics([diag], Some(path.clone())))?;
            let output = arandu_semantics::CodegenBackend::compile(
                backend,
                &artifacts.amir,
                artifacts.type_check.symbols.as_ref(),
                artifacts.type_check.type_info.as_ref(),
            )
            .map_err(|diag| CliFailure::diagnostics([diag], Some(path.clone())))?;
            let host_name = artifacts.amir.funcs.iter().find_map(|func_def| {
                let symbol = artifacts.type_check.symbols.get(func_def.symbol);
                (symbol.name.as_str() == function)
                    .then(|| artifacts.type_check.symbols.host_func_name(symbol))
            });
            unsafe {
                if let Some(test_fn) = host_name.as_ref().and_then(|name| {
                    arandu_semantics::CompiledCode::get_fn::<unsafe fn()>(&output, name)
                }) {
                    test_fn();
                    if snippet.should_panic {
                        let outcome = arandu_runtime::testing_runtime::finish_test_context();
                        let temp_root =
                            std::env::var_os("ARANDU_TEST_TEMP_ROOT").map(PathBuf::from);
                        arandu_runtime::testing_runtime::init_test_context(exact, 0, temp_root);
                        if outcome.status == arandu_codegen::testing::TestStatus::Failed {
                            return Ok(CliSuccess::Done);
                        }
                        return Err(CliFailure::operational(
                            "run test case",
                            Some(path),
                            "doctest marked `should_panic` completed without failing",
                        ));
                    }
                    return Ok(CliSuccess::Done);
                }
            }
            return Err(CliFailure::operational(
                "run test case",
                Some(path),
                format!("doctest function `{function}` is not callable as `fn() -> void`"),
            ));
        }

        // A harness child must report compilation failures through its terminal
        // event, not exit inside the pipeline before the IPC frame is written.
        let artifacts = pipeline_lower_checked(db, file)
            .map_err(|diagnostics| CliFailure::diagnostics(diagnostics, Some(path.clone())))?
            .artifacts;
        ensure_host_jit_layout(data_layout)?;
        let backend = arandu_backend_cranelift::CraneliftBackend::try_new()
            .map_err(|diag| CliFailure::diagnostics([diag], Some(path.clone())))?;
        let output = arandu_semantics::CodegenBackend::compile(
            backend,
            &artifacts.amir,
            artifacts.type_check.symbols.as_ref(),
            artifacts.type_check.type_info.as_ref(),
        )
        .map_err(|diag| CliFailure::diagnostics([diag], Some(path.clone())))?;
        let function_def = artifacts.amir.funcs.iter().find(|func_def| {
            artifacts
                .type_check
                .symbols
                .get(func_def.symbol)
                .name
                .as_str()
                == function
        });
        let return_type = function_def.map(|func_def| {
            artifacts
                .type_check
                .type_info
                .type_interner
                .resolve(func_def.return_type)
        });
        let host_name = function_def.map(|func_def| {
            let symbol = artifacts.type_check.symbols.get(func_def.symbol);
            artifacts.type_check.symbols.host_func_name(symbol)
        });
        unsafe {
            if matches!(return_type, Some(arandu_semantics::types::ArType::Void)) {
                if let Some(test_fn) = host_name.as_ref().and_then(|name| {
                    arandu_semantics::CompiledCode::get_fn::<unsafe fn()>(&output, name)
                }) {
                    test_fn();
                    return Ok(CliSuccess::Done);
                }
            } else if let Some(arandu_semantics::types::ArType::Result(ok, _)) = return_type
                && matches!(
                    artifacts.type_check.type_info.type_interner.resolve(ok),
                    arandu_semantics::types::ArType::Void
                )
                && let Some(test_fn) = host_name.as_ref().and_then(|name| {
                    arandu_semantics::CompiledCode::get_fn::<unsafe fn() -> *mut u8>(&output, name)
                })
            {
                let result = test_fn();
                if result.is_null() || *(result.cast::<usize>()) == 0 {
                    return Ok(CliSuccess::Done);
                }
                return Err(CliFailure::operational(
                    "run test case",
                    Some(path),
                    "test returned Result::Err",
                ));
            }
        }
        return Err(CliFailure::operational(
            "run test case",
            Some(path),
            format!("test function `{function}` is not callable as `fn() -> void`"),
        ));
    }
    Err(CliFailure::operational(
        "run test case",
        Some(ctx.root.clone()),
        format!("test case `{exact}` could not be compiled"),
    ))
}
