//! Compare live package diagnostics after structural edits with a cold DB.
use arandu_query::{
    file_ide_diagnostics, AnalysisHost, DirectoryListing, IdeDiagnostic, ManifestData, SourceFile,
};

const PATHS: [&str; 3] = ["main.aru", "dep.aru", "other.aru"];
const MAIN: &str = "import graph.dep as dep\nfunc main(): int { return dep.value() }\n";
const DEP: &str = "public func value(): int { return 1 }\n";
type Sources = [Option<String>; 3];
type Files = [Option<SourceFile>; 3];

fn path(index: usize) -> String {
    std::path::Path::new("graph")
        .join(PATHS[index])
        .to_string_lossy()
        .into_owned()
}

fn entries(sources: &Sources) -> Vec<String> {
    sources
        .iter()
        .enumerate()
        .filter_map(|(index, text)| text.as_ref().map(|_| PATHS[index].into()))
        .collect()
}

fn cold(sources: &Sources) -> (AnalysisHost, Files, DirectoryListing) {
    cold_with_registration_order(sources, [0, 1, 2])
}

fn cold_with_registration_order(
    sources: &Sources,
    registration_order: [usize; 3],
) -> (AnalysisHost, Files, DirectoryListing) {
    let mut host = AnalysisHost::new();
    let mut files = [None; 3];
    for index in registration_order {
        files[index] = sources[index]
            .as_ref()
            .map(|text| host.new_file(path(index), text.clone()));
    }
    let (_, listing, _) = host.configure_package(
        "Arandu.toml".into(),
        ManifestData::legacy("graph".into(), "0.1.0".into(), "main.aru".into()),
        "fixture".into(),
        "graph".into(),
        entries(sources),
        None,
    );
    (host, files, listing)
}

fn assert_cycle_registration_order_equivalence(sources: &Sources, operations: &[u8]) {
    if !has_import_cycle(sources) {
        return;
    }

    let (forward, forward_files, _) = cold_with_registration_order(sources, [0, 1, 2]);
    let (reverse, reverse_files, _) = cold_with_registration_order(sources, [2, 1, 0]);
    for index in 0..3 {
        let (Some(forward_file), Some(reverse_file)) = (forward_files[index], reverse_files[index])
        else {
            continue;
        };
        assert_eq!(
            diagnostics(&forward, &forward_files, forward_file, operations),
            diagnostics(&reverse, &reverse_files, reverse_file, operations),
            "cycle diagnostics depend on file registration order: operations={operations:?}, file={}",
            PATHS[index]
        );
        let text = sources[index]
            .as_deref()
            .expect("live cycle file has source text");
        assert_completion_equivalence(
            (&forward, forward_file),
            (&reverse, reverse_file),
            text,
            operations,
            operations.len(),
            PATHS[index],
        );
    }
}

fn has_import_cycle(sources: &Sources) -> bool {
    let graph: [Vec<usize>; 3] = std::array::from_fn(|index| {
        sources[index]
            .as_deref()
            .into_iter()
            .flat_map(str::lines)
            .filter_map(|line| {
                let module = line.strip_prefix("import ")?.split_whitespace().next()?;
                let module = module.strip_prefix("graph.")?.split('.').next()?;
                match module {
                    "main" => Some(0),
                    "dep" => Some(1),
                    "other" => Some(2),
                    _ => None,
                }
            })
            .collect()
    });

    fn visit(
        node: usize,
        graph: &[Vec<usize>; 3],
        visiting: &mut [bool; 3],
        visited: &mut [bool; 3],
    ) -> bool {
        if visiting[node] {
            return true;
        }
        if visited[node] {
            return false;
        }
        visiting[node] = true;
        for &next in &graph[node] {
            if visit(next, graph, visiting, visited) {
                return true;
            }
        }
        visiting[node] = false;
        visited[node] = true;
        false
    }

    let mut visiting = [false; 3];
    let mut visited = [false; 3];
    (0..3).any(|node| visit(node, &graph, &mut visiting, &mut visited))
}

fn diagnostics(
    host: &AnalysisHost,
    files: &Files,
    file: SourceFile,
    operations: &[u8],
) -> Vec<IdeDiagnostic> {
    // FileIds are monotonic and deliberately differ after deletion/recreation.
    // Normalize only file identity, retaining diagnostic order and all contents.
    let canonical_id = |id| {
        // Signature-cycle recovery currently emits a source-less (0, 0, 0)
        // diagnostic. Preserve that sentinel independently of live identities.
        if id == 0 {
            return 0;
        }
        let index = files
            .iter()
            .position(|file| file.is_some_and(|file| *file.file_id(host.db()) == id))
            .unwrap_or_else(|| panic!("diagnostic references missing file {id}: operations={operations:?}, live={:?}, diagnostics={:?}", files.map(|f| f.map(|f| *f.file_id(host.db()))), **file_ide_diagnostics(host.db(), file)));
        u32::try_from(index + 1).expect("three fixture files")
    };
    let mut diagnostics = file_ide_diagnostics(host.db(), file).to_vec();
    for diagnostic in &mut diagnostics {
        diagnostic.file_id = canonical_id(diagnostic.file_id);
        for label in &mut diagnostic.labels {
            label.file_id = canonical_id(label.file_id);
        }
        for hint in &mut diagnostic.hints {
            if let Some(replacement) = &mut hint.replacement {
                replacement.file_id = canonical_id(replacement.file_id);
            }
        }
        if let Some(function) = &mut diagnostic.func {
            function.file_id = canonical_id(function.file_id);
        }
        assert!(
            !diagnostic.code.starts_with("ICE"),
            "generated graph must recover: {diagnostic:?}"
        );
    }
    diagnostics
}

fn assert_completion_equivalence(
    warm: (&AnalysisHost, SourceFile),
    fresh: (&AnalysisHost, SourceFile),
    text: &str,
    operations: &[u8],
    step: usize,
    file_name: &str,
) {
    let Ok(end) = u32::try_from(text.len()) else {
        return;
    };
    let mut offsets: Vec<_> = text
        .match_indices("dep.")
        .filter_map(|(start, _)| u32::try_from(start + "dep.".len()).ok())
        .collect();
    offsets.push(end);
    offsets.sort_unstable();
    offsets.dedup();

    let warm_snapshot = warm.0.snapshot();
    let fresh_snapshot = fresh.0.snapshot();
    for offset in offsets {
        let actual = arandu_ide::completions(&warm_snapshot, warm.1, text, offset);
        let expected = arandu_ide::completions(&fresh_snapshot, fresh.1, text, offset);
        let labels = |items: &[arandu_ide::CompletionItem]| {
            items
                .iter()
                .map(|item| {
                    (
                        item.label.clone(),
                        format!("{:?}", item.kind),
                        item.detail.clone(),
                        item.insert_text.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let actual_labels = labels(&actual);
        let expected_labels = labels(&expected);
        if actual_labels != expected_labels {
            let actual_typeck = arandu_query::passes::type_check(&warm_snapshot.db, warm.1);
            let expected_typeck = arandu_query::passes::type_check(&fresh_snapshot.db, fresh.1);
            let symbols = |result: &arandu_semantics::TypeCheckResult| {
                result
                    .symbols
                    .iter()
                    .map(|symbol| (symbol.name.to_string(), format!("{:?}", symbol.kind)))
                    .collect::<Vec<_>>()
            };
            panic!(
                "graph incremental/cold completion mismatch: step={step}, operations={operations:?}, file={file_name}, offset={offset}, sources={text:?}, actual_labels={actual_labels:?}, expected_labels={expected_labels:?}, actual_symbols={:?}, expected_symbols={:?}",
                symbols(actual_typeck),
                symbols(expected_typeck),
            );
        }
    }
}

pub(super) fn run(data: &[u8]) {
    let mut sources: Sources = [Some(MAIN.into()), Some(DEP.into()), None];
    let (mut warm, mut files, listing) = cold(&sources);
    let mut last_file_ids = [None; 3];
    let mut retired_symbols: [Vec<arandu_middle::SymbolId>; 3] =
        std::array::from_fn(|_| Vec::new());
    for file in files.iter().flatten() {
        assert!(
            diagnostics(&warm, &files, *file, &[]).is_empty(),
            "valid baseline"
        );
    }
    for (step, operation) in data.iter().take(24).enumerate() {
        match operation % 10 {
            0 => sources[1] = None,
            1 | 9 => sources[1] = Some(DEP.into()),
            2 => sources[2] = sources[1].take(),
            3 => {
                sources[0] = Some(
                    "import graph.other as dep\nfunc main(): int { return dep.value() }\n".into(),
                )
            }
            4 => sources[0] = Some(MAIN.into()),
            5 => sources[1] = Some("public func value(): str { return \"text\" }\n".into()),
            6 => {
                sources[2] = Some(
                    "import graph.dep as dependency\npublic func value(): int { return dependency.value() }\n"
                        .into(),
                )
            }
            7 => sources[2] = None,
            8 => {
                sources[1] = Some(
                    "import graph.main as root\npublic func value(): int { return root.main() }\n"
                        .into(),
                )
            }
            _ => unreachable!("modulo ten"),
        }
        for index in 0..3 {
            match (files[index], sources[index].as_ref()) {
                (Some(file), Some(text)) => {
                    if file.text(warm.db()).as_ref() != text {
                        warm.set_text(file, text.as_str());
                    }
                }
                (None, Some(text)) => {
                    let file = warm.new_file(path(index), text.clone());
                    let file_id = *file.file_id(warm.db());
                    if let Some(previous) = last_file_ids[index] {
                        assert!(
                            file_id > previous,
                            "FileId was reused or moved backwards after unregister: path={}, previous={previous}, current={file_id}, operations={:?}",
                            PATHS[index],
                            &data[..=step]
                        );
                    }
                    let current_symbols = arandu_query::passes::resolve(warm.db(), file)
                        .symbols
                        .iter()
                        .filter(|symbol| symbol.id.file_id == file_id)
                        .map(|symbol| symbol.id)
                        .collect::<Vec<_>>();
                    assert!(
                        current_symbols
                            .iter()
                            .all(|symbol| !retired_symbols[index].contains(symbol)),
                        "SymbolId was reused after unregister: path={}, file_id={file_id}, operations={:?}",
                        PATHS[index],
                        &data[..=step]
                    );
                    files[index] = Some(file);
                }
                (Some(file), None) => {
                    let file_id = *file.file_id(warm.db());
                    retired_symbols[index].extend(
                        arandu_query::passes::resolve(warm.db(), file)
                            .symbols
                            .iter()
                            .filter(|symbol| symbol.id.file_id == file_id)
                            .map(|symbol| symbol.id),
                    );
                    last_file_ids[index] = Some(file_id);
                    warm.unregister_source_file(&path(index));
                    files[index] = None;
                }
                (None, None) => {}
            }
        }
        warm.set_directory_entries(listing, entries(&sources));
        let (fresh, fresh_files, _) = cold(&sources);
        for index in if step % 2 == 0 { [2, 1, 0] } else { [0, 1, 2] } {
            if let (Some(actual), Some(expected)) = (files[index], fresh_files[index]) {
                assert_eq!(
                    diagnostics(&warm, &files, actual, &data[..=step]),
                    diagnostics(&fresh, &fresh_files, expected, &data[..=step]),
                    "graph mismatch: step={step}, operations={:?}, file={}, sources={sources:?}",
                    &data[..=step],
                    PATHS[index]
                );
                let text = sources[index]
                    .as_deref()
                    .expect("live file has source text");
                assert_completion_equivalence(
                    (&warm, actual),
                    (&fresh, expected),
                    text,
                    &data[..=step],
                    step,
                    PATHS[index],
                );
            }
        }
        assert_cycle_registration_order_equivalence(&sources, &data[..=step]);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_structural_edit_pairs_match_cold_analysis() {
        for first in 0..10 {
            for second in 0..10 {
                super::run(&[first, second, 1, 4, 7]);
            }
        }
    }

    #[test]
    fn longer_structural_edit_sequences_match_cold_analysis() {
        for seed in 1..=32u64 {
            let mut state = seed;
            let operations: [u8; 24] = std::array::from_fn(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state.to_le_bytes()[7]
            });
            super::run(&operations);
        }
    }

    #[test]
    fn cyclic_import_diagnostics_ignore_file_registration_order() {
        let sources = [
            Some("import graph.dep as dep\nfunc main(): int { return dep.value() }\n".into()),
            Some(
                "import graph.main as root\npublic func value(): int { return root.main() }\n"
                    .into(),
            ),
            None,
        ];
        super::assert_cycle_registration_order_equivalence(&sources, &[8]);
    }

    #[test]
    fn three_module_import_cycle_ignores_file_registration_order() {
        let sources = [
            Some(
                "import graph.other as dep\nfunc main(): int { return dep.value() }\n".into(),
            ),
            Some(
                "import graph.main as root\npublic func value(): int { return root.main() }\n"
                    .into(),
            ),
            Some(
                "import graph.dep as dependency\npublic func value(): int { return dependency.value() }\n"
                    .into(),
            ),
        ];
        assert!(super::has_import_cycle(&sources));
        super::assert_cycle_registration_order_equivalence(&sources, &[3, 6, 8]);
    }
}
