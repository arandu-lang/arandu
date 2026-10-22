//! Mechanical checks for repository agent instructions.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 300;

pub fn check(root: &Path) -> i32 {
    match violations(root) {
        Ok(violations) if violations.is_empty() => {
            println!("check-agents: ok (agent instructions are coherent and linked)");
            0
        }
        Ok(violations) => {
            eprintln!("check-agents: violation(s):");
            for violation in violations {
                eprintln!("  - {violation}");
            }
            1
        }
        Err(error) => {
            eprintln!("check-agents: {error}");
            1
        }
    }
}

fn violations(root: &Path) -> Result<Vec<String>, String> {
    let files = find_agents(root)?;
    let mut violations = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("read {} as UTF-8: {error}", path.display()))?;
        let display = path.strip_prefix(root).unwrap_or(&path).display();
        let line_count = text.lines().count();
        if line_count > MAX_LINES {
            violations.push(format!(
                "{display} has {line_count} lines (limit: {MAX_LINES})"
            ));
        }

        let mut headings = HashSet::new();
        for line in text.lines().filter(|line| line.starts_with('#')) {
            let heading = line.trim();
            if !headings.insert(heading.to_string()) {
                violations.push(format!("{display} repeats heading `{heading}`"));
            }
        }
        for target in markdown_targets(&text) {
            if target.starts_with('#') || target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            let target = target.split('#').next().unwrap_or_default();
            if target.is_empty() {
                continue;
            }
            let resolved = path.parent().unwrap_or(root).join(target);
            if !resolved.exists() {
                violations.push(format!("{display} links to missing `{target}`"));
            }
        }
    }
    Ok(violations)
}

fn find_agents(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    visit(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn visit(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("read {} entry: {error}", dir.display()))?
            .path();
        if path.file_name().and_then(|name| name.to_str()) == Some("target") {
            continue;
        }
        if path.is_dir() {
            visit(&path, files)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("AGENTS.md") {
            files.push(path);
        }
    }
    Ok(())
}

fn markdown_targets(text: &str) -> impl Iterator<Item = &str> {
    text.split("](").filter_map(|part| part.split(')').next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_duplicate_headings_and_missing_links() {
        let root = std::env::temp_dir().join(format!(
            "arandu-agents-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("AGENTS.md"), "# A\n# A\n[missing](nope.md)\n").unwrap();
        let errors = violations(&root).unwrap();
        assert!(errors.iter().any(|error| error.contains("repeats heading")));
        assert!(errors.iter().any(|error| error.contains("missing")));
        fs::remove_dir_all(root).unwrap();
    }
}
