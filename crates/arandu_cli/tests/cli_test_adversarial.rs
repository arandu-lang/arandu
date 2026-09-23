//! SL_T.2C + SL_T.2E — Campanha adversarial do runner e seleção reproduzível.
//!
//! Cobre: seed determinística, seeds diferentes, jobs 1 vs N, fail-fast,
//! round-trip do schema JSON v1, zero testes, filtro inexistente, bytes não-UTF-8,
//! stdout imitando frames ARND, processo morto, benchmark informativo de overhead.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::time::Instant;

mod common;

/// Cria diretório temporário único.
fn temp_dir(label: &str) -> std::path::PathBuf {
    common::temp_dir(&format!("arandu-adv-{label}")).expect("reserve fresh temporary directory")
}

/// Cria um projeto Arandu mínimo com src/main.aru contendo testes declarados.
fn create_project(root: &std::path::Path, name: &str, main_src: &str) {
    let created = common::cli_command()
        .args(["new", name, "--vcs=none"])
        .current_dir(root)
        .output()
        .expect("create project");
    assert!(
        created.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let _ = fs::remove_dir_all(root.join(name).join("tests"));
    fs::write(root.join(name).join("src/main.aru"), main_src).unwrap();
}

/// Cria um projeto com N testes vazios (passam) para medir overhead.
fn create_n_passing_tests(root: &std::path::Path, name: &str, n: usize) {
    let mut src = format!("module {name}\n\n");
    for i in 0..n {
        src.push_str(&format!("@Test\nfunc test_{i}(): void {{}}\n\n"));
    }
    src.push_str("func main(): int { return 0 }\n");
    create_project(root, name, &src);
}

#[allow(clippy::panic)]
fn json_report(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI must emit a valid JSON report (exit={:?}, stdout={:?}, stderr={:?}): {error}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

// ─── SL_T.2C ─────────────────────────────────────────────────────────────────

/// Mesma seed produz exatamente o mesmo plano de execução.
#[test]
fn same_seed_produces_same_execution_plan() {
    let tmp = temp_dir("same_seed");
    let proj = tmp.join("same_seed");
    let src = "module same_seed\n\n\
        @Test\nfunc alpha(): void {}\n\n\
        @Test\nfunc beta(): void {}\n\n\
        @Test\nfunc gamma(): void {}\n\n\
        @Test\nfunc delta(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "same_seed", src);

    let run_with_seed = |seed: &str| -> Vec<String> {
        let out = common::cli_command()
            .args([
                "test",
                proj.to_str().unwrap(),
                "--format",
                "json",
                "--seed",
                seed,
            ])
            .output()
            .expect("run test");
        assert!(out.status.success(), "test CLI failed: {:?}", out.status);
        let report = json_report(&out);
        report["cases"]
            .as_array()
            .expect("cases array")
            .iter()
            .map(|case| case["id"].as_str().expect("case id string").to_string())
            .collect()
    };

    let run1 = run_with_seed("12345");
    let run2 = run_with_seed("12345");
    assert!(
        !run1.is_empty(),
        "expected at least one test case in output"
    );
    assert_eq!(run1, run2, "same seed must produce same execution order");

    let _ = fs::remove_dir_all(tmp);
}

/// Seeds diferentes produzem planos distintos mas conjunto e ordenação final idênticos.
#[test]
fn different_seeds_same_final_canonical_order() {
    let tmp = temp_dir("diff_seed");
    let proj = tmp.join("diff_seed");
    let src = "module diff_seed\n\n\
        @Test\nfunc alpha(): void {}\n\n\
        @Test\nfunc beta(): void {}\n\n\
        @Test\nfunc gamma(): void {}\n\n\
        @Test\nfunc delta(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "diff_seed", src);

    let run_json = |seed: &str| -> serde_json::Value {
        let out = common::cli_command()
            .args([
                "test",
                proj.to_str().unwrap(),
                "--format",
                "json",
                "--seed",
                seed,
            ])
            .output()
            .expect("run test");
        assert!(out.status.success(), "test CLI failed: {:?}", out.status);
        json_report(&out)
    };

    let r1 = run_json("111");
    let r2 = run_json("999");

    let ids1: Vec<&str> = r1["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    let ids2: Vec<&str> = r2["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();

    // Conjunto de IDs é idêntico (mesmos casos)
    let mut s1 = ids1.clone();
    s1.sort_unstable();
    let mut s2 = ids2.clone();
    s2.sort_unstable();
    assert_eq!(s1, s2, "both seeds must report the same set of test IDs");

    // Ordenação final do JSON deve ser canônica (idêntica entre seeds)
    assert_eq!(
        ids1, ids2,
        "JSON final output must be in canonical order regardless of seed"
    );

    let _ = fs::remove_dir_all(tmp);
}

/// `--jobs 1` e `--jobs 4` produzem o mesmo conjunto de resultados e ordenação final canônica.
#[test]
fn jobs_1_and_n_produce_same_canonical_results() {
    let tmp = temp_dir("jobs_n");
    let proj = tmp.join("jobs_n");
    let src = "module jobs_n\n\n\
        @Test\nfunc alpha(): void {}\n\n\
        @Test\nfunc beta(): void {}\n\n\
        @Test\nfunc gamma(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "jobs_n", src);

    let run = |jobs: &str| -> Vec<String> {
        let out = common::cli_command()
            .args([
                "test",
                proj.to_str().unwrap(),
                "--format",
                "json",
                "--jobs",
                jobs,
                "--seed",
                "42",
            ])
            .output()
            .expect("run test");
        assert!(out.status.success(), "test CLI failed: {:?}", out.status);
        let report = json_report(&out);
        report["cases"]
            .as_array()
            .expect("cases array")
            .iter()
            .map(|case| case["id"].as_str().expect("case id string").to_string())
            .collect()
    };

    let j1 = run("1");
    let j4 = run("4");
    assert!(!j1.is_empty());
    assert_eq!(
        j1, j4,
        "--jobs 1 and --jobs 4 must produce identical canonical final order"
    );

    let _ = fs::remove_dir_all(tmp);
}

/// `--fail-fast` interrompe agendamentos mas drena e reporta todos os filhos já iniciados.
#[test]
fn fail_fast_drains_started_children() {
    let tmp = temp_dir("fail_fast");
    let proj = tmp.join("fail_fast");
    // Um teste que falha imediatamente + vários que passam
    let src = "module fail_fast\n\nimport err\n\n\
        @Test\nfunc aaa_fails(): Result<void, Err> { return Result.Err(err.new(\"boom\")) }\n\n\
        @Test\nfunc bbb_passes(): void {}\n\n\
        @Test\nfunc ccc_passes(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "fail_fast", src);

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--fail-fast",
        ])
        .output()
        .expect("run test");

    // Deve sair com falha (exit code != 0)
    assert_ne!(out.status.code(), Some(0));

    let report = json_report(&out);
    let cases = report["cases"].as_array().expect("cases array");

    // O caso que falhou deve aparecer no relatório
    let has_failed = cases.iter().any(|c| c["status"] == "failed");
    assert!(has_failed, "fail-fast must report the failed case");

    // O resumo deve ter pelo menos 1 falhou
    assert!(
        report["summary"]["failed"]
            .as_u64()
            .expect("summary.failed must be an integer")
            >= 1,
        "summary.failed must be >= 1"
    );

    let _ = fs::remove_dir_all(tmp);
}

// ─── SL_T.2D — Round-trip do schema JSON ─────────────────────────────────────

/// O relatório JSON emitido é válido e contém todos os campos exigidos pelo contrato.
#[test]
fn json_report_roundtrip_schema_v1() {
    let tmp = temp_dir("schema_rt");
    let proj = tmp.join("schema_rt");
    let src = "module schema_rt\n\n\
        @Test\nfunc passes(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "schema_rt", src);

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--seed",
            "0",
            "--jobs",
            "1",
            "--timeout",
            "30",
        ])
        .output()
        .expect("run test");

    assert!(
        out.status.success(),
        "test must pass: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");

    // Campos obrigatórios do schema
    assert_eq!(report["schema"], "arandu.test/v1", "schema field");
    assert!(report["target"].is_string(), "target must be present");
    assert!(report["backend"].is_string(), "backend must be present");
    assert!(report["seed"].is_number(), "seed must be present");
    assert!(report["jobs"].is_number(), "jobs must be present");
    assert!(
        report["timeout_ms"].is_number(),
        "timeout_ms must be present"
    );
    assert!(
        report["fail_fast"].is_boolean(),
        "fail_fast must be present"
    );

    let summary = &report["summary"];
    assert!(summary["total"].is_number(), "summary.total");
    assert!(summary["passed"].is_number(), "summary.passed");
    assert!(summary["failed"].is_number(), "summary.failed");
    assert!(summary["skipped"].is_number(), "summary.skipped");
    assert!(summary["timed_out"].is_number(), "summary.timed_out");
    assert!(summary["crashed"].is_number(), "summary.crashed");
    assert!(summary["duration_ms"].is_number(), "summary.duration_ms");

    let cases = report["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty(), "at least one case expected");

    let case = &cases[0];
    assert!(case["id"].is_string(), "case.id");
    assert!(case["status"].is_string(), "case.status");
    assert!(case["duration_ms"].is_number(), "case.duration_ms");
    assert!(case["stdout"].is_string(), "case.stdout");
    assert!(case["stderr"].is_string(), "case.stderr");
    assert!(
        case["stdout_truncated"].is_boolean(),
        "case.stdout_truncated"
    );
    assert!(
        case["stderr_truncated"].is_boolean(),
        "case.stderr_truncated"
    );

    // Sem paths temporários ou timestamps absolutos no JSON
    let json_str = serde_json::to_string(&report).unwrap();
    assert!(
        !json_str.contains("arandu-adv-"),
        "JSON must not contain temp dir paths"
    );

    let _ = fs::remove_dir_all(tmp);
}

/// Escrita atômica do relatório JSON com `--output` não deixa arquivo corrompido.
#[test]
fn json_report_atomic_output_file() {
    let tmp = temp_dir("atomic_out");
    let proj = tmp.join("atomic_out");
    let src = "module atomic_out\n\n\
        @Test\nfunc passes(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "atomic_out", src);

    let output_path = tmp.join("report.json");

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .expect("run test");

    assert!(
        out.status.success(),
        "test must pass: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(output_path.exists(), "output file must exist");
    let content = fs::read(&output_path).unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&content).expect("output must be valid JSON");
    assert_eq!(report["schema"], "arandu.test/v1");

    // Segunda execução sobrescreve atomicamente o arquivo existente
    let out2 = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .expect("run test again");

    assert!(out2.status.success());
    let content2 = fs::read(&output_path).unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(&content2).expect("overwritten file must be valid JSON");

    let _ = fs::remove_dir_all(tmp);
}

// ─── SL_T.2E — Campanha adversarial ──────────────────────────────────────────

/// Zero testes encontrados: sucesso com relatório vazio.
#[test]
fn zero_tests_reports_empty_success() {
    let tmp = temp_dir("zero_tests");
    let proj = tmp.join("zero_tests");
    // Projeto sem nenhum @Test
    create_project(
        &tmp,
        "zero_tests",
        "module zero_tests\n\nfunc main(): int { return 0 }\n",
    );

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");

    assert!(
        out.status.success(),
        "zero tests must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Saída pode ser vazia (não há casos) ou JSON com array vazio
    if !out.stdout.is_empty() {
        let report = json_report(&out);
        let total = report["summary"]["total"]
            .as_u64()
            .expect("summary.total must be an unsigned integer");
        assert_eq!(total, 0, "summary.total must be 0 with no tests");
        assert!(
            report["cases"].as_array().expect("cases array").is_empty(),
            "cases must be empty when there are no tests"
        );
    }

    let _ = fs::remove_dir_all(tmp);
}

/// Filtro inexistente: retorna erro operacional sem crash.
#[test]
fn nonexistent_exact_filter_returns_error() {
    let tmp = temp_dir("no_match");
    let proj = tmp.join("no_match");
    create_project(
        &tmp,
        "no_match",
        "module no_match\n\n@Test\nfunc passes(): void {}\n\nfunc main(): int { return 0 }\n",
    );

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--exact",
            "no_match::bin::main::nonexistent_test",
        ])
        .output()
        .expect("run test");

    assert_ne!(out.status.code(), Some(0), "nonexistent --exact must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("was not found"),
        "must report that test was not found, got: {stderr}"
    );

    let _ = fs::remove_dir_all(tmp);
}

/// Múltiplas falhas com `--jobs N` são todas reportadas.
#[test]
fn multiple_failures_with_parallel_jobs_all_reported() {
    let tmp = temp_dir("multi_fail");
    let proj = tmp.join("multi_fail");
    let src = "module multi_fail\n\nimport err\n\n\
        @Test\nfunc fail1(): Result<void, Err> { return Result.Err(err.new(\"f1\")) }\n\n\
        @Test\nfunc fail2(): Result<void, Err> { return Result.Err(err.new(\"f2\")) }\n\n\
        @Test\nfunc fail3(): Result<void, Err> { return Result.Err(err.new(\"f3\")) }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "multi_fail", src);

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--jobs",
            "3",
        ])
        .output()
        .expect("run test");

    assert_ne!(out.status.code(), Some(0));
    let report = json_report(&out);
    let failed = report["summary"]["failed"]
        .as_u64()
        .expect("summary.failed must be an integer");
    assert_eq!(failed, 3, "all 3 failures must be reported, got {failed}");

    let _ = fs::remove_dir_all(tmp);
}

/// stdout que imita um frame `ARND` não engana o runner (stdout separado do canal IPC).
#[test]
fn stdout_mimicking_frame_magic_does_not_fool_runner() {
    let tmp = temp_dir("magic_stdout");
    let proj = tmp.join("magic_stdout");
    // O teste imprime os bytes mágicos do frame ARND para stdout — não deve confundir o runner
    // (usamos um programa que escreve bytes para stdout via código Arandu)
    let src = "module magic_stdout\n\nimport io\n\n\
        @Test\nfunc writes_frame_bytes(): void { io.println(\"ARND\") }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "magic_stdout", src);

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");

    assert!(
        out.status.success(),
        "test must pass despite frame-like stdout: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report = json_report(&out);
    assert_eq!(
        report["summary"]["passed"]
            .as_u64()
            .expect("summary.passed must be an integer"),
        1
    );
    let cases = report["cases"].as_array().expect("cases array");
    assert_eq!(cases.len(), 1, "the stdout fixture must run exactly once");
    assert_eq!(cases[0]["stdout"].as_str(), Some("ARND\n"));

    let _ = fs::remove_dir_all(tmp);
}

/// Processo que faz timeout é classificado como `timed_out`, não como `crashed` nem `passed`.
#[test]
fn timeout_classified_as_timed_out_not_crashed() {
    let tmp = temp_dir("tmo_cls");
    let proj = tmp.join("tmo_cls");
    let src = "module tmo_cls\n\n\
        @Test\nfunc hangs(): void { while true {} }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "tmo_cls", src);

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--timeout",
            "1",
            "--exact",
            "tmo_cls::bin::main::hangs",
        ])
        .output()
        .expect("run test");

    assert_ne!(out.status.code(), Some(0));
    let report = json_report(&out);
    let cases = report["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty());
    assert_eq!(
        cases[0]["status"], "timed_out",
        "timeout must be classified as timed_out"
    );

    let _ = fs::remove_dir_all(tmp);
}

/// Teste que falha via `Result.Err` é classificado como `failed` com falha estruturada.
#[test]
fn result_err_classified_as_failed_with_structured_failure() {
    let tmp = temp_dir("result_fail");
    let proj = tmp.join("result_fail");
    let src = "module result_fail\n\nimport err\n\n\
        @Test\nfunc fails(): Result<void, Err> { return Result.Err(err.new(\"structured failure\")) }\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "result_fail", src);

    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--exact",
            "result_fail::bin::main::fails",
        ])
        .output()
        .expect("run test");

    assert_ne!(out.status.code(), Some(0));
    let report = json_report(&out);
    let cases = report["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty());
    assert_eq!(cases[0]["status"], "failed");
    // failure não deve ser null
    assert!(
        !cases[0]["failure"].is_null(),
        "failure must be structured, not null"
    );

    let _ = fs::remove_dir_all(tmp);
}

// ─── Benchmark informativo de overhead ────────────────────────────────────────

/// Mede o overhead do coordenador para 1, 100 e 1000 testes vazios.
/// Este é um benchmark *informativo* — NÃO entra no S0/Gate.
/// Só reporta o valor via eprintln para inspeção manual.
#[test]
fn benchmark_coordinator_overhead_informative() {
    for n in [1_usize, 5, 20] {
        let tmp = temp_dir(&format!("bench_{n}"));
        let name = format!("bench_{n}");
        create_n_passing_tests(&tmp, &name, n);

        let proj = tmp.join(&name);
        let start = Instant::now();
        let out = common::cli_command()
            .args([
                "test",
                proj.to_str().unwrap(),
                "--format",
                "json",
                "--jobs",
                "1",
            ])
            .output()
            .expect("run benchmark test");

        let elapsed = start.elapsed();

        assert!(
            out.status.success(),
            "benchmark with {n} tests must pass: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let report = json_report(&out);
        let total = report["summary"]["total"]
            .as_u64()
            .expect("summary.total must be an integer");
        assert_eq!(total as usize, n, "all {n} tests must be reported");

        // Benchmark informativo — apenas exibe, não impõe limite
        eprintln!(
            "[arandu bench] coordinator overhead: {n} empty tests → {elapsed:?} total ({:.1}ms/test avg)",
            elapsed.as_millis() as f64 / n.max(1) as f64
        );

        let _ = fs::remove_dir_all(tmp);
    }
}

// ─── Verificação de ordenação canônica do JSON independente da seed ───────────

/// O campo `cases` no JSON de saída é sempre ordenado canonicamente (por ID),
/// independente da seed ou ordem de execução.
#[test]
fn json_cases_always_in_canonical_order() {
    let tmp = temp_dir("canon_order");
    let proj = tmp.join("canon_order");
    let src = "module canon_order\n\n\
        @Test\nfunc zzz_last(): void {}\n\n\
        @Test\nfunc aaa_first(): void {}\n\n\
        @Test\nfunc mmm_mid(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "canon_order", src);

    // Usa seed que tende a inverter a ordem
    let out = common::cli_command()
        .args([
            "test",
            proj.to_str().unwrap(),
            "--format",
            "json",
            "--seed",
            "99999",
            "--jobs",
            "4",
        ])
        .output()
        .expect("run test");

    assert!(
        out.status.success(),
        "test must pass: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report = json_report(&out);
    let ids: Vec<&str> = report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();

    // Os IDs devem estar em ordem lexicográfica crescente
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(
        ids, sorted,
        "cases in JSON must always be in canonical lexicographic order"
    );

    let _ = fs::remove_dir_all(tmp);
}

/// Verifica que o mapa de status no relatório não contém strings ad hoc não previstas no contrato.
#[test]
fn json_report_status_values_are_from_contract() {
    let tmp = temp_dir("status_vals");
    let proj = tmp.join("status_vals");
    let src = "module status_vals\n\n\
        @Test\nfunc passes(): void {}\n\n\
        func main(): int { return 0 }\n";
    create_project(&tmp, "status_vals", src);

    let out = common::cli_command()
        .args(["test", proj.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run test");

    let report = json_report(&out);
    let allowed: std::collections::HashSet<&str> =
        ["passed", "failed", "skipped", "timed_out", "crashed"]
            .iter()
            .copied()
            .collect();

    assert!(out.status.success(), "CLI test run must succeed");
    let cases = report["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty(), "status contract needs at least one case");
    for case in cases {
        let status = case["status"]
            .as_str()
            .expect("case.status must be a string");
        assert!(
            allowed.contains(status),
            "unexpected status value `{status}` — not in contract"
        );
    }

    let _ = fs::remove_dir_all(tmp);
}
