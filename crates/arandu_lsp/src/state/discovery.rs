//! Workspace filesystem discovery of Arandu source files.

use std::path::PathBuf;

/// Read a deterministic, bounded set of workspace sources outside the LSP
/// handshake. Registration remains on the main server thread.
#[must_use]
pub fn discover_aru_files(roots: &[PathBuf]) -> Vec<(PathBuf, String)> {
    const MAX_FILES: usize = 256;

    let mut stack = roots.to_vec();
    stack.sort();
    stack.reverse();
    let mut paths = std::collections::BTreeSet::new();
    let mut visited_dirs = std::collections::HashSet::new();

    while let Some(dir) = stack.pop() {
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !visited_dirs.insert(canonical) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for ent in entries.into_iter().rev() {
            let p = ent.path();
            if p.is_dir() {
                if matches!(
                    p.file_name().and_then(|s| s.to_str()),
                    Some("target" | ".git" | "node_modules")
                ) {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("aru") {
                paths.insert(p);
                if paths.len() >= MAX_FILES {
                    break;
                }
            }
        }
        if paths.len() >= MAX_FILES {
            break;
        }
    }

    paths
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((path, text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_aru_files_handles_symlink_cycles() {
        let temp_dir = std::env::temp_dir().join("arandu_test_discovery_symlink_cycle");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let sub_dir = temp_dir.join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();

        std::fs::write(sub_dir.join("main.aru"), "func main(): int { 0 }").unwrap();

        #[cfg(unix)]
        {
            let cycle_link = sub_dir.join("cycle_to_parent");
            let _ = std::os::unix::fs::symlink(&temp_dir, &cycle_link);
        }

        let discovered = discover_aru_files(std::slice::from_ref(&temp_dir));
        assert_eq!(discovered.len(), 1);
        assert_eq!(
            discovered[0].0.file_name().and_then(|s| s.to_str()),
            Some("main.aru")
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
