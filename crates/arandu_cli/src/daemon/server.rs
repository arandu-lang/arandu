//! The daemon of one project (HOT.3).
//!
//! It holds the project's daemon lock for as long as it listens, so one
//! daemon serves a project per toolchain and a socket left by a dead one is
//! safe to replace. Each connection gets a thread; builds run on one builder
//! thread that owns the sessions, the single writer of their databases.
//! Requests for the same build that wait while another build runs are served
//! by one build of the newest inputs.

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::endpoint;
use super::protocol::{Outcome, Request, Response, read_frame, write_frame};
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::commands::build::{BuildRequest, BuildSession};
use crate::output::{RenderStyle, Stream};

/// Sessions kept warm at once; the least recently used one is dropped.
const MAX_SESSIONS: usize = 4;
/// Connections served at once; more are refused and those clients build in
/// batch, so a burst cannot grow the daemon without bound.
const MAX_CONNECTIONS: usize = 32;
const HEARTBEAT: Duration = Duration::from_secs(1);

/// `ARANDU_DAEMON_IDLE_SECS`, 15 minutes by default.
fn idle_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("ARANDU_DAEMON_IDLE_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(15 * 60),
    )
}

/// `ARANDU_DAEMON_MAX_RSS_MB`, 4 GiB by default: past it the daemon finishes
/// its builds and exits, and the next build starts a fresh one.
fn rss_ceiling() -> u64 {
    std::env::var("ARANDU_DAEMON_MAX_RSS_MB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4096)
        << 20
}

fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: `sysconf` has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Some(pages * u64::try_from(page).ok()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // The peak, never below the current size: recycling stays safe.
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `getrusage` fills the zeroed `rusage` it is given.
        let usage = unsafe {
            (libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) == 0)
                .then(|| usage.assume_init())
        }?;
        // macOS reports bytes.
        u64::try_from(usage.ru_maxrss).ok()
    }
}

struct Shared {
    identity: String,
    stopping: AtomicBool,
    connections: AtomicUsize,
    last_activity: Mutex<Instant>,
    /// The build the builder runs now, and the signal that tells it a newer
    /// request for the same build arrived.
    running: Mutex<Option<SessionKey>>,
    supersede: Arc<AtomicBool>,
}

impl Shared {
    fn touch(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }

    fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .map_or(Duration::ZERO, |last| last.elapsed())
    }
}

/// One build a client waits for.
struct Job {
    cwd: PathBuf,
    args: Vec<String>,
    style: RenderStyle,
    reply: mpsc::Sender<Reply>,
}

#[derive(Clone)]
enum Reply {
    Unsupported,
    Built {
        output: Vec<(Stream, String)>,
        outcome: Outcome,
    },
}

pub fn serve(args: &[String]) -> CliResult {
    let [root, socket] = &args[2..] else {
        return Err(CliFailure::usage("usage: arandu __daemon <root> <socket>"));
    };
    let (root, socket) = (PathBuf::from(root), PathBuf::from(socket));
    let lock_path = endpoint::lock_path(&root);
    let failure = |operation, error: std::io::Error| {
        CliFailure::operational(operation, Some(root.clone()), error.to_string())
    };
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| failure("create daemon state", error))?;
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|error| failure("open daemon lock", error))?;
    // A daemon of another toolchain that a client just asked to step aside
    // releases the lock once its builds answer.
    let deadline = Instant::now() + Duration::from_secs(5);
    while lock.try_lock().is_err() {
        if Instant::now() >= deadline {
            // A live daemon serves this project.
            return Ok(CliSuccess::Done);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let identity = super::identity().map_err(|error| failure("identify toolchain", error))?;
    if endpoint::socket_path(&root).ok().as_deref() != Some(socket.as_path()) {
        return Err(CliFailure::usage(
            "the daemon socket does not belong to this project",
        ));
    }
    // The lock proves no daemon listens on a leftover socket.
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(|error| failure("bind daemon", error))?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .map_err(|error| failure("restrict daemon socket", error))?;
    let bound = socket_inode(&socket);
    listener
        .set_nonblocking(true)
        .map_err(|error| failure("configure daemon socket", error))?;

    let shared = Arc::new(Shared {
        identity,
        stopping: AtomicBool::new(false),
        connections: AtomicUsize::new(0),
        last_activity: Mutex::new(Instant::now()),
        running: Mutex::new(None),
        supersede: Arc::new(AtomicBool::new(false)),
    });
    let (jobs, queue) = mpsc::channel::<Job>();
    let builder = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || build_loop(&queue, &shared))
    };
    let idle = idle_timeout();
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                shared.touch();
                if shared.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                    shared.connections.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let shared = Arc::clone(&shared);
                let jobs = jobs.clone();
                std::thread::spawn(move || {
                    let _ = serve_connection(stream, &shared, &jobs);
                    shared.touch();
                    shared.connections.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if shared.connections.load(Ordering::Acquire) == 0 && shared.idle_for() >= idle {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    // Stop listening first so new clients start a fresh daemon or build in
    // batch, then let the builds in flight answer.
    if socket_inode(&socket) == bound {
        let _ = fs::remove_file(&socket);
    }
    drop(listener);
    shared.stopping.store(true, Ordering::Release);
    while shared.connections.load(Ordering::Acquire) > 0 {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(jobs);
    let _ = builder.join();
    drop(lock);
    Ok(CliSuccess::Done)
}

fn socket_inode(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.ino())
}

fn serve_connection(
    mut stream: UnixStream,
    shared: &Shared,
    jobs: &mpsc::Sender<Job>,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    if !endpoint::peer_is_owner(&stream) {
        return Ok(());
    }
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    match read_frame::<Request>(&mut stream)? {
        Request::Shutdown { identity } => {
            if identity == shared.identity {
                shared.stopping.store(true, Ordering::Release);
            }
            Ok(())
        }
        Request::Build { identity, .. } if identity != shared.identity => {
            // Another toolchain or environment: step aside for its daemon.
            shared.stopping.store(true, Ordering::Release);
            write_frame(&mut stream, &Response::Mismatch)
        }
        Request::Build {
            cwd, args, style, ..
        } => {
            // A newer request for the build in flight stops it at its next
            // checkpoint; this request then shares the build of its inputs.
            if shared
                .running
                .lock()
                .is_ok_and(|running| running.as_ref() == Some(&(cwd.clone(), args.clone())))
            {
                shared.supersede.store(true, Ordering::Release);
            }
            let (reply, answer) = mpsc::channel();
            if jobs
                .send(Job {
                    cwd,
                    args,
                    style,
                    reply,
                })
                .is_err()
            {
                return write_frame(&mut stream, &Response::Unsupported);
            }
            let reply = loop {
                match answer.recv_timeout(HEARTBEAT) {
                    Ok(reply) => break reply,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        write_frame(&mut stream, &Response::Heartbeat)?;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return write_frame(&mut stream, &Response::Unsupported);
                    }
                }
            };
            match reply {
                Reply::Unsupported => write_frame(&mut stream, &Response::Unsupported),
                Reply::Built { output, outcome } => {
                    for (target, text) in output {
                        write_frame(
                            &mut stream,
                            &Response::Output {
                                stream: target,
                                text,
                            },
                        )?;
                    }
                    write_frame(&mut stream, &Response::Done(outcome))
                }
            }
        }
    }
}

/// A warm build of one request: the client's directory and arguments.
type SessionKey = (PathBuf, Vec<String>);

/// Times in a row a build may be superseded before it runs to the end, so a
/// stream of edits cannot starve publication.
const MAX_SUPERSEDED: u32 = 3;

fn build_loop(queue: &mpsc::Receiver<Job>, shared: &Shared) {
    let mut sessions: VecDeque<(SessionKey, BuildSession)> = VecDeque::new();
    let mut pending: Vec<Job> = Vec::new();
    let mut streak = 0;
    loop {
        if pending.is_empty() {
            match queue.recv() {
                Ok(job) => pending.push(job),
                Err(_) => break,
            }
        }
        pending.extend(queue.try_iter());
        let mut groups = coalesce(std::mem::take(&mut pending)).into_iter();
        while let Some(group) = groups.next() {
            let Some(last) = group.last() else {
                continue;
            };
            let Some(reply) = build(&mut sessions, last, shared, streak < MAX_SUPERSEDED) else {
                // Superseded before publication: its clients wait for the
                // build of the newer request, which is already queued.
                streak += 1;
                pending.extend(group);
                pending.extend(groups.flatten());
                break;
            };
            streak = 0;
            for job in &group {
                let _ = job.reply.send(reply.clone());
            }
            if resident_bytes().is_some_and(|bytes| bytes > rss_ceiling()) {
                shared.stopping.store(true, Ordering::Release);
            }
        }
    }
}

/// Jobs grouped by request, in the order each request first arrived. One
/// build of the newest inputs answers a whole group.
fn coalesce(pending: Vec<Job>) -> Vec<Vec<Job>> {
    let mut groups: Vec<Vec<Job>> = Vec::new();
    for job in pending {
        match groups
            .iter_mut()
            .find(|group| group[0].cwd == job.cwd && group[0].args == job.args)
        {
            Some(group) => group.push(job),
            None => groups.push(vec![job]),
        }
    }
    groups
}

/// `ARANDU_DAEMON_FAULT=before-build|after-build` aborts the daemon at that
/// point, as a crash or `kill -9` would; the recovery tests (HOT.5) use it.
fn inject_fault(point: &str) {
    if std::env::var("ARANDU_DAEMON_FAULT").is_ok_and(|fault| fault == point) {
        std::process::abort();
    }
}

/// The reply to `job`, or `None` when a newer request superseded the build
/// at a checkpoint and `cancellable` allowed it.
fn build(
    sessions: &mut VecDeque<(SessionKey, BuildSession)>,
    job: &Job,
    shared: &Shared,
    cancellable: bool,
) -> Option<Reply> {
    inject_fault("before-build");
    // Relative paths in the arguments are the client's; builds run one at a
    // time on this thread, the only one that reads relative paths.
    if std::env::set_current_dir(&job.cwd).is_err() {
        return Some(Reply::Unsupported);
    }
    let key = (job.cwd.clone(), job.args.clone());
    shared.supersede.store(false, Ordering::Release);
    if let Ok(mut running) = shared.running.lock() {
        *running = Some(key.clone());
    }
    let signal = cancellable.then(|| Arc::clone(&shared.supersede));
    let output = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = std::rc::Rc::clone(&output);
    let result = crate::output::capture(
        move |stream, text: &str| sink.borrow_mut().push((stream, text.to_owned())),
        job.style,
        || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(sessions, key, &job.args, signal)
            }))
        },
    );
    if let Ok(mut running) = shared.running.lock() {
        *running = None;
    }
    let result = match result {
        Ok(Some(Run::Built(result))) => result,
        Ok(Some(Run::Superseded)) => return None,
        Ok(None) => return Some(Reply::Unsupported),
        Err(_) => {
            // A panic may leave a session half-updated: drop them all and let
            // the next build start a fresh daemon.
            sessions.clear();
            shared.stopping.store(true, Ordering::Release);
            Err(CliFailure::operational(
                "build in the daemon",
                None,
                "the build panicked; the daemon restarts on the next build",
            ))
        }
    };
    inject_fault("after-build");
    let output = std::rc::Rc::try_unwrap(output)
        .map(std::cell::RefCell::into_inner)
        .unwrap_or_default();
    Some(Reply::Built {
        output,
        outcome: Outcome::of(&result),
    })
}

enum Run {
    Built(CliResult),
    Superseded,
}

/// The build of `args` on its warm session, opening it on first use. `None`
/// for a build the daemon does not run: only native Cranelift builds go
/// through it, whose tools all report through the build's own output.
fn run(
    sessions: &mut VecDeque<(SessionKey, BuildSession)>,
    key: SessionKey,
    args: &[String],
    supersede: Option<Arc<AtomicBool>>,
) -> Option<Run> {
    let index = match sessions.iter().position(|(existing, _)| *existing == key) {
        Some(index) => index,
        None => {
            let invocation = crate::args::parse_invocation(args.to_vec());
            let start = invocation
                .args
                .get(2)
                .map_or_else(|| key.0.clone(), |path| key.0.join(path));
            let request = BuildRequest {
                flags: invocation.project_flags.clone(),
                opt: invocation.opt,
                debug_info: invocation.debug,
                genref_report: invocation.genref_report,
                data_layout: invocation.data_layout,
                backend_choice: invocation.backend,
            };
            let session = match BuildSession::open(&start, request) {
                Ok(session) => session,
                Err(failure) => return Some(Run::Built(Err(failure))),
            };
            if !session.serves_in_daemon() {
                return None;
            }
            if sessions.len() == MAX_SESSIONS {
                sessions.pop_back();
            }
            sessions.push_front((key, session));
            0
        }
    };
    let entry = sessions.remove(index)?;
    sessions.push_front(entry);
    let (_, session) = sessions.front_mut()?;
    session.set_supersede(supersede);
    let result = session.build();
    Some(if session.take_superseded() {
        Run::Superseded
    } else {
        Run::Built(result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(cwd: &str, args: &[&str]) -> (Job, mpsc::Receiver<Reply>) {
        let (reply, answer) = mpsc::channel();
        (
            Job {
                cwd: PathBuf::from(cwd),
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                style: RenderStyle {
                    color: false,
                    width: None,
                },
                reply,
            },
            answer,
        )
    }

    #[test]
    fn waiting_requests_of_one_build_share_a_single_build() {
        let (a1, _) = job("/p", &["arandu", "build"]);
        let (b, _) = job("/q", &["arandu", "build"]);
        let (a2, _) = job("/p", &["arandu", "build"]);
        let (a3, _) = job("/p", &["arandu", "build", "--release"]);
        let groups = coalesce(vec![a1, b, a2, a3]);
        let sizes = groups.iter().map(Vec::len).collect::<Vec<_>>();
        assert_eq!(sizes, [2, 1, 1]);
        assert_eq!(groups[0][0].cwd, PathBuf::from("/p"));
        assert_eq!(groups[1][0].cwd, PathBuf::from("/q"));
    }
}
