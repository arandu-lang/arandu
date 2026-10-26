use std::path::{Path, PathBuf};

// Only rebase absolute inputs. Relative paths (including an explicit runtime
// override) belong to the caller's directory and must keep that meaning.
pub(super) fn linker_work_dir<'a>(
    objects: &[&'a Path],
    runtime: &Path,
    output: &Path,
) -> Option<&'a Path> {
    if !runtime.is_absolute()
        || !output.is_absolute()
        || objects.iter().any(|obj| !obj.is_absolute())
    {
        return None;
    }
    objects
        .first()
        .and_then(|obj| obj.parent())
        .filter(|dir| !dir.as_os_str().is_empty())
}

pub(super) fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

pub(super) fn make_relative(target: &Path, base: &Path) -> PathBuf {
    let target = strip_verbatim_prefix(target);
    let base = strip_verbatim_prefix(base);
    let target_components: Vec<_> = target.components().collect();
    let base_components: Vec<_> = base.components().collect();
    let mut common = 0;
    while common < target_components.len()
        && common < base_components.len()
        && target_components[common] == base_components[common]
    {
        common += 1;
    }
    if common == 0 {
        return target;
    }
    let mut result = PathBuf::new();
    for _ in common..base_components.len() {
        result.push("..");
    }
    for component in &target_components[common..] {
        result.push(component.as_os_str());
    }
    result
}
