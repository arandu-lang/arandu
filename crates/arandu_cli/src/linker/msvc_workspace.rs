//! Short, disposable filesystem paths for MSVC, independent of CGU cache names.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::strip_verbatim_prefix;

static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

pub(super) struct MsvcWorkspace {
    directory: PathBuf,
    pub objects: Vec<PathBuf>,
    pub runtime: PathBuf,
    pub output: PathBuf,
}

impl Drop for MsvcWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl MsvcWorkspace {
    pub fn new(objects: &[&Path], runtime: &Path) -> io::Result<Self> {
        Self::new_in(&std::env::temp_dir(), objects, runtime)
    }

    fn new_in(root: &Path, objects: &[&Path], runtime: &Path) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        for _ in 0..128 {
            let id = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let directory = root.join(format!("ar-link-{}-{id}", std::process::id()));
            // Reserve room for any usize index, rustc stub and discovery script,
            // including the terminating NUL. Relative paths do not bypass the
            // linker's fully-qualified MAX_PATH limit.
            ensure_short(&directory.join("o0123456789abcdef.obj"))?;
            ensure_short(&directory.join("app.msvc-env.cmd"))?;
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(&directory)
            };
            #[cfg(not(unix))]
            let created = fs::create_dir(&directory);
            match created {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
                Ok(()) => {}
            }
            // Construct the guard before any fallible input operation.
            let mut workspace = Self {
                runtime: directory.join("runtime.lib"),
                output: directory.join("app.exe"),
                directory,
                objects: Vec::with_capacity(objects.len()),
            };
            for (index, object) in objects.iter().enumerate() {
                let destination = workspace.directory.join(format!("o{index:x}.obj"));
                link_or_copy(object, &destination)?;
                workspace.objects.push(destination);
            }
            link_or_copy(runtime, &workspace.runtime)?;
            return Ok(workspace);
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a temporary MSVC linking directory",
        ))
    }

    pub fn copy_output(&self, destination: &Path) -> io::Result<()> {
        // Copy only after link success; artifact.rs validates and atomically
        // publishes the caller's staging file. Rust handles verbatim long paths.
        fs::copy(&self.output, destination)?;
        Ok(())
    }
}

fn link_or_copy(source: &Path, destination: &Path) -> io::Result<()> {
    // The linker only reads these inputs. Preserve the cache and avoid copying
    // the runtime archive when possible; cross-volume/UNC inputs use a copy.
    if fs::hard_link(source, destination).is_err() {
        fs::copy(source, destination).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("prepare MSVC input '{}': {error}", source.display()),
            )
        })?;
    }
    Ok(())
}

fn ensure_short(path: &Path) -> io::Result<()> {
    let normalized = strip_verbatim_prefix(path);
    #[cfg(windows)]
    let length = {
        use std::os::windows::ffi::OsStrExt;
        normalized.as_os_str().encode_wide().count()
    };
    #[cfg(not(windows))]
    let length = normalized.to_string_lossy().encode_utf16().count();
    if length >= 260 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MSVC temporary paths exceed MAX_PATH; set TEMP to a shorter writable directory",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_runtime() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
    }

    #[test]
    fn stages_long_duplicate_names_preserves_inputs_and_cleans_up() {
        let cwd = std::env::current_dir().unwrap();
        let relative_runtime = super::super::make_relative(&fixture_runtime(), &cwd);
        assert!(!relative_runtime.is_absolute());
        let parent = MsvcWorkspace::new(&[], &relative_runtime).unwrap();
        let nested = parent.directory.join("项目 with spaces".repeat(8));
        fs::create_dir(&nested).unwrap();
        let first = nested.join(format!("{}.obj", "a".repeat(140)));
        let other = parent.directory.join(first.file_name().unwrap());
        fs::write(&first, b"first object").unwrap();
        fs::write(&other, b"second object").unwrap();
        let runtime = nested.join("runtime.lib");
        fs::write(&runtime, b"runtime archive").unwrap();
        assert!(first.to_string_lossy().encode_utf16().count() > 260);

        let first_arg = super::super::make_relative(&first, &cwd);
        let workspace = MsvcWorkspace::new(&[&first_arg, &other], &runtime).unwrap();
        let path = workspace.directory.clone();
        assert_eq!(fs::read(&workspace.objects[0]).unwrap(), b"first object");
        assert_eq!(fs::read(&workspace.objects[1]).unwrap(), b"second object");
        assert_eq!(fs::read(&workspace.runtime).unwrap(), b"runtime archive");
        for input in workspace
            .objects
            .iter()
            .chain([&workspace.runtime, &workspace.output])
        {
            ensure_short(input).unwrap();
        }
        assert_ne!(workspace.objects[0], workspace.objects[1]);
        let refs: Vec<_> = workspace.objects.iter().map(PathBuf::as_path).collect();
        let command = super::super::build_system_linker_command(
            std::ffi::OsStr::new("link.exe"),
            &refs,
            &workspace.runtime,
            &workspace.output,
            false,
            false,
        );
        assert_eq!(
            command.get_current_dir(),
            Some(strip_verbatim_prefix(&workspace.directory).as_path())
        );
        let args: Vec<_> = command.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("o0.obj")));
        assert!(args.contains(&std::ffi::OsStr::new("o1.obj")));
        assert!(args.contains(&std::ffi::OsStr::new("runtime.lib")));
        #[cfg(windows)]
        assert!(args.contains(&std::ffi::OsStr::new("/OUT:app.exe")));
        fs::write(&workspace.output, b"linked executable").unwrap();
        let output = nested.join("final.exe");
        workspace.copy_output(&output).unwrap();
        drop(workspace);
        assert!(!path.exists());
        assert_eq!(fs::read(output).unwrap(), b"linked executable");
        assert_eq!(fs::read(first).unwrap(), b"first object");
        assert_eq!(fs::read(other).unwrap(), b"second object");
        assert_eq!(fs::read(runtime).unwrap(), b"runtime archive");
    }

    #[test]
    fn failed_preparation_cleans_only_its_own_directory() {
        let parent = MsvcWorkspace::new(&[], &fixture_runtime()).unwrap();
        let before = fs::read_dir(&parent.directory).unwrap().count();
        assert!(
            MsvcWorkspace::new_in(
                &parent.directory,
                &[&parent.directory.join("missing.obj")],
                &parent.runtime,
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&parent.directory).unwrap().count(), before);
        assert!(parent.runtime.is_file());
    }

    #[test]
    fn absent_link_output_does_not_replace_the_callers_staging_file() {
        let workspace = MsvcWorkspace::new(&[], &fixture_runtime()).unwrap();
        let destination = workspace.directory.join("existing.exe");
        fs::write(&destination, b"previous output").unwrap();
        assert!(workspace.copy_output(&destination).is_err());
        assert_eq!(fs::read(destination).unwrap(), b"previous output");
    }

    #[test]
    fn checks_full_utf16_length_including_relative_path_resolution_budget() {
        assert!(ensure_short(Path::new(&"a".repeat(259))).is_ok());
        assert!(ensure_short(Path::new(&"a".repeat(260))).is_err());
        assert!(ensure_short(Path::new(&"🦀".repeat(130))).is_err());
        assert!(ensure_short(Path::new(&format!(r"\\?\C:\{}", "x".repeat(257)))).is_err());
    }

    #[test]
    fn refuses_an_overlong_temporary_root_before_creating_a_workspace() {
        let parent = MsvcWorkspace::new(&[], &fixture_runtime()).unwrap();
        let root = parent
            .directory
            .join("a".repeat(90))
            .join("b".repeat(90))
            .join("c".repeat(90));
        fs::create_dir_all(&root).unwrap();
        let error = MsvcWorkspace::new_in(&root, &[], &fixture_runtime())
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("set TEMP"));
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    }
}
