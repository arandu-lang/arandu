//! The local build service (HOT.3) and its fail-closed client (HOT.5).
//!
//! `arandu build` asks a resident daemon of its project to build, so the
//! project database stays warm between builds. The daemon is an optimization
//! only: any failure to reach it, a toolchain or environment it does not
//! share, a protocol error or a build it does not run sends the client to the
//! ordinary batch build, whose result is the same.

#[cfg(any(unix, windows))]
#[cfg_attr(windows, path = "endpoint_windows.rs")]
mod endpoint;
mod protocol;
#[cfg(any(unix, windows))]
mod server;

use crate::args::CliInvocation;
use crate::cli_error::CliResult;

/// Environment variables a build reads besides its arguments. A daemon serves
/// only clients whose values match its own, which it inherited when spawned.
const BUILD_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "CC",
    "CXX",
    "LD",
    "LDFLAGS",
    "LIBRARY_PATH",
    "LD_LIBRARY_PATH",
    "SDKROOT",
    "MACOSX_DEPLOYMENT_TARGET",
];

/// Whether this invocation may go through the daemon: `arandu build` on Unix
/// or Windows, without process-wide `-Z` switches or `--watch`, unless `--no-daemon`,
/// `ARANDU_DAEMON=0` or a CI environment (`CI` set) ask for batch. With
/// `ARANDU_DAEMON=1` or `require` CI uses it too.
pub fn eligible(invocation: &CliInvocation) -> bool {
    let requested = std::env::var("ARANDU_DAEMON").ok();
    let default = std::env::var_os("CI").is_none_or(|value| value.is_empty());
    cfg!(any(unix, windows))
        && invocation
            .args
            .get(1)
            .is_some_and(|command| command == "build")
        && invocation.z_flags.is_empty()
        && !invocation.watch
        && !invocation.no_daemon
        && match requested.as_deref() {
            Some("0") => false,
            Some("1" | "require") => true,
            _ => default,
        }
}

/// `ARANDU_DAEMON=require`: a build the daemon cannot serve fails instead of
/// running in batch. Measurements use it so they never time a batch build.
pub fn required() -> bool {
    std::env::var("ARANDU_DAEMON").is_ok_and(|value| value == "require")
}

/// The toolchain and environment a daemon shares with its clients: the
/// compiler binary as a file (any reinstall changes its inode or times) and
/// every variable a build reads.
pub fn identity() -> std::io::Result<String> {
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let metadata = std::fs::metadata(&executable)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"arandu-daemon-identity/v1\0");
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.update(&protocol::VERSION.to_le_bytes());
    hash.update(executable.as_os_str().as_encoded_bytes());
    hash.update(b"\0");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            hash.update(&value.to_le_bytes());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let file = std::fs::File::open(&executable)?;
        // SAFETY: zero is a valid `BY_HANDLE_FILE_INFORMATION`, which the
        // call fills for the open file.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open for the lifetime of `file`.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        for value in [
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
            info.nFileSizeHigh,
            info.nFileSizeLow,
            info.ftCreationTime.dwHighDateTime,
            info.ftCreationTime.dwLowDateTime,
            info.ftLastWriteTime.dwHighDateTime,
            info.ftLastWriteTime.dwLowDateTime,
        ] {
            hash.update(&value.to_le_bytes());
        }
        let _ = metadata;
    }
    #[cfg(not(any(unix, windows)))]
    hash.update(&metadata.len().to_le_bytes());
    let mut variables = std::env::vars_os()
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                (name.starts_with("ARANDU_") && name != "ARANDU_DAEMON")
                    || BUILD_ENVIRONMENT.contains(&name)
            })
        })
        .collect::<Vec<_>>();
    variables.sort();
    for (name, value) in variables {
        hash.update(name.as_encoded_bytes());
        hash.update(b"=");
        hash.update(value.as_encoded_bytes());
        hash.update(b"\0");
    }
    Ok(hash.finalize().to_hex().to_string())
}

/// Builds through the daemon of the project, starting one when none serves
/// it. `None` asks for the batch build.
/// With `verbose`, a build the daemon served says so on stderr.
pub fn try_build(raw_args: &[String], color: bool, verbose: bool) -> Option<CliResult> {
    #[cfg(any(unix, windows))]
    {
        let result = client::build(raw_args, color);
        if verbose {
            eprintln!(
                "{}",
                if result.is_some() {
                    "[daemon] built by the project daemon"
                } else {
                    "[daemon] unavailable for this build; building in this process"
                }
            );
        }
        result
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (raw_args, color, verbose);
        None
    }
}

/// `arandu __daemon <root> <socket>`: serves builds of `root` until idle.
pub fn serve(args: &[String]) -> CliResult {
    #[cfg(any(unix, windows))]
    {
        server::serve(args)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = args;
        Err(crate::cli_error::CliFailure::usage(
            "the build daemon runs on Unix and Windows only",
        ))
    }
}

/// `arandu __daemon-stop [path]`: asks the daemon of the project at `path`
/// to finish. Succeeds when none runs.
pub fn stop(args: &[String]) -> CliResult {
    #[cfg(any(unix, windows))]
    {
        client::stop(args)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = args;
        Ok(crate::cli_error::CliSuccess::Done)
    }
}

#[cfg(any(unix, windows))]
mod client {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::endpoint::{self, Connection};
    use super::protocol::{Request, Response, read_frame, write_frame};
    use crate::cli_error::{CliResult, CliSuccess};
    use crate::output::{RenderStyle, Stream};

    /// A daemon that stays silent this long is treated as gone: it sends a
    /// heartbeat every second while it builds.
    const SILENCE: Duration = Duration::from_secs(5);
    const SPAWN_WAIT: Duration = Duration::from_secs(5);

    fn project_root(cwd: &Path, args: &[String]) -> Option<PathBuf> {
        let start = args
            .iter()
            .skip(2)
            .find(|arg| !arg.starts_with('-'))
            .map_or_else(|| cwd.to_path_buf(), |path| cwd.join(path));
        let discovery = crate::manifest_io::find_manifest(&start).ok()??;
        std::fs::canonicalize(discovery.path.parent()?).ok()
    }

    pub(super) fn build(raw_args: &[String], color: bool) -> Option<CliResult> {
        let identity = super::identity().ok()?;
        let cwd = std::env::current_dir().ok()?;
        let root = project_root(&cwd, raw_args)?;
        let socket = endpoint::socket_path(&root).ok()?;
        let request = Request::Build {
            identity,
            cwd,
            args: raw_args.to_vec(),
            style: RenderStyle {
                color,
                width: terminal_width(),
            },
        };
        // A daemon of another toolchain answers `Mismatch` and stops; the
        // second attempt starts one of this toolchain.
        for _ in 0..2 {
            let stream = connect_or_spawn(&root, &socket)?;
            match exchange(stream, &request)? {
                Exchange::Done(result) => return Some(result),
                Exchange::Replaced => {
                    // The old daemon removes its socket once it stops
                    // listening; connecting earlier would reach it again.
                    let deadline = Instant::now() + SPAWN_WAIT;
                    while endpoint::listening(&socket) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                Exchange::Unsupported => return None,
            }
        }
        None
    }

    enum Exchange {
        Done(CliResult),
        Replaced,
        Unsupported,
    }

    fn exchange(mut stream: Connection, request: &Request) -> Option<Exchange> {
        if !endpoint::peer_is_owner(&stream) {
            return None;
        }
        stream.set_read_timeout(Some(SILENCE)).ok()?;
        write_frame(&mut stream, request).ok()?;
        loop {
            match read_frame::<Response>(&mut stream).ok()? {
                Response::Heartbeat => {}
                Response::Output {
                    stream: target,
                    text,
                } => {
                    let _ = match target {
                        Stream::Stdout => std::io::stdout().write_all(text.as_bytes()),
                        Stream::Stderr => std::io::stderr().write_all(text.as_bytes()),
                    };
                }
                Response::Done(outcome) => return Some(Exchange::Done(outcome.into_result())),
                Response::Mismatch => return Some(Exchange::Replaced),
                Response::Unsupported => return Some(Exchange::Unsupported),
            }
        }
    }

    /// Connects to the daemon of `root`, starting it when none listens. A
    /// stale socket left by a dead daemon is replaced by the new one, which
    /// takes the project's daemon lock before it binds.
    fn connect_or_spawn(root: &Path, socket: &Path) -> Option<Connection> {
        if let Ok(stream) = endpoint::connect(socket) {
            return Some(stream);
        }
        let executable = std::env::current_exe().ok()?;
        let mut child = endpoint::spawn_daemon(&executable, root, socket).ok()?;
        let deadline = Instant::now() + SPAWN_WAIT;
        while Instant::now() < deadline {
            if let Ok(stream) = endpoint::connect(socket) {
                child.detach();
                return Some(stream);
            }
            if child.has_exited() {
                // Another daemon took the lock first; it is about to listen.
                if let Ok(stream) = endpoint::connect(socket) {
                    return Some(stream);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[cfg(windows)]
    fn terminal_width() -> Option<usize> {
        use windows_sys::Win32::System::Console::{
            CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE,
        };
        // SAFETY: zero is a valid `CONSOLE_SCREEN_BUFFER_INFO`, which the call
        // fills when stderr is a console.
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: `GetStdHandle` has no preconditions and the call only
        // writes `info`; a handle that is not a console makes it fail.
        let filled =
            unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_ERROR_HANDLE), &raw mut info) };
        let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
        (filled != 0 && width > 0)
            .then(|| usize::try_from(width).ok())
            .flatten()
    }

    #[cfg(unix)]
    fn terminal_width() -> Option<usize> {
        use std::os::fd::AsRawFd;
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `TIOCGWINSZ` writes one `winsize` into `size`.
        let result = unsafe {
            libc::ioctl(
                std::io::stderr().as_raw_fd(),
                libc::TIOCGWINSZ,
                &raw mut size,
            )
        };
        (result == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
    }

    pub(super) fn stop(args: &[String]) -> CliResult {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let Some(root) = project_root(&cwd, args) else {
            return Ok(CliSuccess::Done);
        };
        let Ok(identity) = super::identity() else {
            return Ok(CliSuccess::Done);
        };
        if let Ok(socket) = endpoint::socket_path(&root)
            && let Ok(mut stream) = endpoint::connect(&socket)
            && endpoint::peer_is_owner(&stream)
        {
            let _ = write_frame(&mut stream, &Request::Shutdown { identity });
            // The daemon closes the connection once it has stopped listening.
            let _ = stream.set_read_timeout(Some(SPAWN_WAIT));
            let _ = read_frame::<Response>(&mut stream);
        }
        Ok(CliSuccess::Done)
    }
}
