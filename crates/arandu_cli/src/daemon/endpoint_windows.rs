//! Where a project's daemon listens and who may talk to it (HOT.3), on
//! Windows: the interface of `endpoint.rs` over a local named pipe.
//!
//! The pipe `\\.\pipe\arandu-<hash>` is named after the current user's SID
//! and the project root. Its DACL grants access to that SID only, it rejects
//! remote clients, and the daemon creates its first instance exclusively, so
//! a pipe another process squats is never served. Each side checks that the
//! process at the other end runs as the current user, and the client opens
//! the pipe at identification level, so a daemon cannot impersonate it.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_ALL, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, EqualSid, GetLengthSid,
    GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

/// `SECURITY_DESCRIPTOR_REVISION` of `winnt.h`.
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
const PIPE_BUFFER: u32 = 64 << 10;
/// How long a client waits for a busy daemon to offer its next instance.
const BUSY_WAIT_MS: u32 = 50;
const BUSY_RETRIES: usize = 20;

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid handle the caller just opened and no one else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn event() -> io::Result<OwnedHandle> {
    // SAFETY: an unnamed manual-reset event without security attributes.
    owned(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) })
}

/// The user SID of a process token, as aligned storage for `TOKEN_USER`.
struct UserSid {
    storage: Vec<u64>,
    len: usize,
}

impl UserSid {
    fn of_process(process: HANDLE) -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        // SAFETY: `token` receives a new handle that `owned` takes.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = owned(token)?;
        let mut length = 0;
        // SAFETY: a null buffer of length zero asks for the size needed.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &raw mut length,
            )
        };
        // `u64` storage keeps the `TOKEN_USER` header aligned.
        let mut buffer = vec![0u64; (length as usize).div_ceil(8).max(1)];
        // SAFETY: `buffer` holds at least `length` writable bytes.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &raw mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call wrote a `TOKEN_USER` at the start of `buffer`; its
        // SID points into the same buffer, which is alive here.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        // SAFETY: `sid` is a valid SID for as long as `buffer` lives.
        let size = unsafe { GetLengthSid(sid) } as usize;
        let words = size.div_ceil(8).max(1);
        let mut storage = vec![0u64; words];
        // SAFETY: `sid` occupies `size` readable bytes and `storage` has capacity.
        unsafe {
            std::ptr::copy_nonoverlapping(
                sid.cast::<u8>(),
                storage.as_mut_ptr().cast::<u8>(),
                size,
            );
        }
        Ok(Self { storage, len: size })
    }

    fn current() -> io::Result<Self> {
        // SAFETY: the pseudo-handle of the current process needs no closing.
        Self::of_process(unsafe { GetCurrentProcess() })
    }

    fn of_pid(pid: u32) -> io::Result<Self> {
        // SAFETY: a new handle that `owned` takes.
        let process = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
        Self::of_process(process.as_raw_handle())
    }

    fn as_psid(&self) -> *mut core::ffi::c_void {
        self.storage.as_ptr().cast_mut().cast()
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: `storage` contains `len` initialized bytes of the SID.
        unsafe { std::slice::from_raw_parts(self.storage.as_ptr().cast::<u8>(), self.len) }
    }
}

impl PartialEq for UserSid {
    fn eq(&self, other: &Self) -> bool {
        // SAFETY: both buffers hold valid SIDs.
        unsafe { EqualSid(self.as_psid(), other.as_psid()) != 0 }
    }
}

/// The pipe of the daemon serving `root`, one per user and project.
pub fn socket_path(root: &Path) -> io::Result<PathBuf> {
    let user = UserSid::current()?;
    let mut key = blake3::Hasher::new();
    key.update(b"arandu-daemon-endpoint/v1\0");
    key.update(user.as_bytes());
    key.update(b"\0");
    key.update(root.as_os_str().as_encoded_bytes());
    Ok(PathBuf::from(format!(
        r"\\.\pipe\arandu-{}",
        &key.finalize().to_hex()[..32]
    )))
}

/// Whether a daemon still offers the pipe: a stopping daemon closes it once
/// it stops accepting.
pub fn listening(pipe: &Path) -> bool {
    let name = wide(pipe.as_os_str());
    // SAFETY: `name` is a NUL-terminated wide string.
    let available = unsafe { WaitNamedPipeW(name.as_ptr(), 1) } != 0;
    available || io::Error::last_os_error().raw_os_error() != Some(ERROR_FILE_NOT_FOUND as i32)
}

pub fn connect(pipe: &Path) -> io::Result<Connection> {
    let name = wide(pipe.as_os_str());
    for _ in 0..BUSY_RETRIES {
        // SAFETY: `name` is NUL-terminated; identification level keeps the
        // daemon from acting as this client.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        match owned(handle) {
            Ok(pipe) => return Connection::new(pipe, false),
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                // SAFETY: as above.
                unsafe { WaitNamedPipeW(name.as_ptr(), BUSY_WAIT_MS) };
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "daemon pipe busy",
    ))
}

/// Whether the process at the other end runs as this user.
pub fn peer_is_owner(stream: &Connection) -> bool {
    let mut pid = 0;
    let handle = stream.pipe.as_raw_handle();
    // SAFETY: `pid` is writable and the handle is a live pipe end.
    let known = unsafe {
        if stream.server {
            GetNamedPipeClientProcessId(handle, &raw mut pid)
        } else {
            GetNamedPipeServerProcessId(handle, &raw mut pid)
        }
    } != 0;
    known
        && matches!(
            (UserSid::of_pid(pid), UserSid::current()),
            (Ok(peer), Ok(current)) if peer == current
        )
}

/// One end of a connected pipe. I/O is overlapped so reads can time out.
pub struct Connection {
    pipe: OwnedHandle,
    event: OwnedHandle,
    /// Read timeout in milliseconds; zero waits forever.
    timeout_ms: AtomicU64,
    server: bool,
}

impl Connection {
    fn new(pipe: OwnedHandle, server: bool) -> io::Result<Self> {
        Ok(Self {
            pipe,
            event: event()?,
            timeout_ms: AtomicU64::new(0),
            server,
        })
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let millis = timeout.map_or(0, |timeout| {
            u64::try_from(timeout.as_millis())
                .unwrap_or(u64::MAX)
                .max(1)
        });
        self.timeout_ms.store(millis, Ordering::Relaxed);
        Ok(())
    }

    /// Waits for the operation `overlapped` started, up to `timeout_ms`
    /// (zero: forever), and cancels it when the time runs out.
    fn complete(&self, overlapped: &mut OVERLAPPED, timeout_ms: u64) -> io::Result<usize> {
        let handle = self.pipe.as_raw_handle();
        let wait = match timeout_ms {
            0 => INFINITE,
            millis => u32::try_from(millis).unwrap_or(INFINITE - 1),
        };
        // SAFETY: the event belongs to this stream and outlives the wait.
        let waited = unsafe { WaitForSingleObject(self.event.as_raw_handle(), wait) };
        let mut transferred = 0;
        if waited == WAIT_TIMEOUT {
            // SAFETY: `overlapped` describes the operation in flight on
            // `handle`; the blocking `GetOverlappedResult` below waits for the
            // cancellation, so the structure outlives the I/O.
            unsafe { CancelIoEx(handle, overlapped) };
        }
        // SAFETY: as above; `bWait` makes the call return only once the
        // operation completed or was cancelled.
        if unsafe { GetOverlappedResult(handle, overlapped, &raw mut transferred, 1) } != 0 {
            return Ok(transferred as usize);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            _ if waited == WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "daemon pipe read timed out",
            )),
            Some(code)
                if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32 =>
            {
                Ok(0)
            }
            _ => Err(error),
        }
    }

    fn overlapped(&self) -> OVERLAPPED {
        // SAFETY: zero is a valid `OVERLAPPED` before an operation starts.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = self.event.as_raw_handle();
        overlapped
    }

    fn started(result: i32) -> io::Result<()> {
        if result != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code) if code == ERROR_IO_PENDING as i32 => Ok(()),
            _ => Err(error),
        }
    }
}

fn end_of_stream(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32
    )
}

impl Read for Connection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let mut overlapped = self.overlapped();
        // SAFETY: `buffer` holds `length` writable bytes and, like
        // `overlapped`, outlives the operation, which `complete` waits for.
        let result = unsafe {
            ReadFile(
                self.pipe.as_raw_handle(),
                buffer.as_mut_ptr(),
                length,
                std::ptr::null_mut(),
                &raw mut overlapped,
            )
        };
        match Self::started(result) {
            Ok(()) => self.complete(&mut overlapped, self.timeout_ms.load(Ordering::Relaxed)),
            Err(error) if end_of_stream(&error) => Ok(0),
            Err(error) => Err(error),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let mut overlapped = self.overlapped();
        // SAFETY: `buffer` holds `length` readable bytes and, like
        // `overlapped`, outlives the operation, which `complete` waits for.
        let result = unsafe {
            WriteFile(
                self.pipe.as_raw_handle(),
                buffer.as_ptr(),
                length,
                std::ptr::null_mut(),
                &raw mut overlapped,
            )
        };
        match Self::started(result) {
            Ok(()) => match self.complete(&mut overlapped, 0)? {
                0 if !buffer.is_empty() => Err(io::ErrorKind::BrokenPipe.into()),
                written => Ok(written),
            },
            Err(error)
                if end_of_stream(&error) || error.raw_os_error() == Some(ERROR_NO_DATA as i32) =>
            {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            Err(error) => Err(error),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The DACL the pipe carries: access for the current user only. The buffers
/// must outlive every `CreateNamedPipeW` that reads them.
struct Security {
    descriptor: Box<SECURITY_DESCRIPTOR>,
    _acl: Vec<u64>,
    _user: UserSid,
}

impl Security {
    fn current_user() -> io::Result<Self> {
        let user = UserSid::current()?;
        let size =
            std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>() + user.len;
        let mut acl = vec![0u64; size.div_ceil(8)];
        // SAFETY: zero is a valid descriptor before initialization.
        let mut descriptor: Box<SECURITY_DESCRIPTOR> = Box::new(unsafe { std::mem::zeroed() });
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        let size = u32::try_from(size).map_err(io::Error::other)?;
        // SAFETY: `acl` holds `size` bytes for one ACE with the user's SID,
        // and `descriptor` is a writable `SECURITY_DESCRIPTOR`; both stay
        // alive in the returned value.
        let ok = unsafe {
            InitializeAcl(acl_ptr, size, ACL_REVISION) != 0
                && AddAccessAllowedAce(acl_ptr, ACL_REVISION, GENERIC_ALL, user.as_psid()) != 0
                && InitializeSecurityDescriptor(
                    (&raw mut *descriptor).cast(),
                    SECURITY_DESCRIPTOR_REVISION,
                ) != 0
                && SetSecurityDescriptorDacl((&raw mut *descriptor).cast(), 1, acl_ptr, 0) != 0
        };
        if !ok {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            _acl: acl,
            _user: user,
        })
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::from_ref(&*self.descriptor).cast_mut().cast(),
            bInheritHandle: 0,
        }
    }
}

/// A pipe instance waiting for its client. `pipe` is taken once a client
/// connected; until then a connect is in flight on it.
struct Pending {
    pipe: Option<OwnedHandle>,
    event: OwnedHandle,
    overlapped: Box<OVERLAPPED>,
    connected: bool,
}

impl Pending {
    fn handle(&self) -> HANDLE {
        self.pipe
            .as_ref()
            .map_or(INVALID_HANDLE_VALUE, AsRawHandle::as_raw_handle)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.connected
            && let Some(pipe) = &self.pipe
        {
            let handle = pipe.as_raw_handle();
            let mut transferred = 0;
            // SAFETY: cancel the pending connect and wait for it, so the
            // boxed `OVERLAPPED` outlives the operation.
            unsafe {
                CancelIoEx(handle, &raw const *self.overlapped);
                GetOverlappedResult(handle, &raw const *self.overlapped, &raw mut transferred, 1);
            }
        }
    }
}

/// The daemon's pipe, created only by the holder of the project's daemon
/// lock. Dropping it closes the instance that waits for a client.
pub struct Listener {
    name: Vec<u16>,
    security: Security,
    pending: Pending,
}

impl Listener {
    pub fn bind(pipe: &Path) -> io::Result<Self> {
        let name = wide(pipe.as_os_str());
        let security = Security::current_user()?;
        let pending = Self::instance(&name, &security, true)?;
        Ok(Self {
            name,
            security,
            pending,
        })
    }

    fn instance(name: &[u16], security: &Security, first: bool) -> io::Result<Pending> {
        let attributes = security.attributes();
        let first = if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
        // SAFETY: `name` is NUL-terminated and `attributes` points at the
        // descriptor `security` keeps alive.
        let pipe = owned(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                &raw const attributes,
            )
        })?;
        let event = event()?;
        // SAFETY: zero is a valid `OVERLAPPED` before the operation starts.
        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        overlapped.hEvent = event.as_raw_handle();
        // SAFETY: the boxed `OVERLAPPED` lives in the returned `Pending`,
        // whose drop waits for the operation.
        let result = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), &raw mut *overlapped) };
        let mut pending = Pending {
            pipe: Some(pipe),
            event,
            overlapped,
            connected: false,
        };
        if result == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(code) if code == ERROR_IO_PENDING as i32 => {}
                Some(code) if code == ERROR_PIPE_CONNECTED as i32 => {
                    // A client connected before the call: no operation is
                    // pending, so signal the event for `wait`.
                    pending.connected = true;
                    // SAFETY: the event belongs to `pending`.
                    unsafe { SetEvent(pending.event.as_raw_handle()) };
                }
                _ => return Err(error),
            }
        }
        Ok(pending)
    }

    /// A waiting client, or `WouldBlock` when none is.
    pub fn accept(&mut self) -> io::Result<Connection> {
        if !self.pending.connected {
            let mut transferred = 0;
            let handle = self.pending.handle();
            // SAFETY: the operation is the pending connect of this instance.
            let done = unsafe {
                GetOverlappedResult(
                    handle,
                    &raw const *self.pending.overlapped,
                    &raw mut transferred,
                    0,
                )
            } != 0;
            if !done {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                // The client left before it was served: offer a new instance.
                self.pending.connected = true;
                self.pending = Self::instance(&self.name, &self.security, false)?;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.pending.connected = true;
        }
        let next = Self::instance(&self.name, &self.security, false)?;
        let mut served = std::mem::replace(&mut self.pending, next);
        let pipe = served
            .pipe
            .take()
            .ok_or_else(|| io::Error::other("daemon pipe instance already served"))?;
        Connection::new(pipe, true)
    }

    /// Blocks until a client connects, `woken` is woken or `timeout` passes.
    pub fn wait(&self, woken: &Woken, timeout: Duration) {
        let handles = [self.pending.event.as_raw_handle(), woken.0.as_raw_handle()];
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(INFINITE - 1);
        // SAFETY: both handles are live events.
        let _ = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis) };
    }
}

/// Wakes a [`Listener::wait`] on its [`Woken`] from another thread.
pub struct Wake(Arc<OwnedHandle>);

pub struct Woken(Arc<OwnedHandle>);

impl Wake {
    pub fn wake(&self) {
        // SAFETY: the event is alive while either side holds it.
        unsafe { SetEvent(self.0.as_raw_handle()) };
    }
}

pub fn waker() -> io::Result<(Wake, Woken)> {
    let event = Arc::new(event()?);
    Ok((Wake(Arc::clone(&event)), Woken(event)))
}
