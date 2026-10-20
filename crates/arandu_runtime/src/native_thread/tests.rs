#![allow(clippy::unwrap_used)]

use super::*;
use std::ptr;
use std::sync::Arc;

#[repr(align(64))]
struct Probe {
    value: usize,
    drops: Arc<AtomicUsize>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

unsafe extern "C" fn drop_probe(pointer: *mut u8) {
    // SAFETY: this callback is paired only with initialized Probe buffers.
    unsafe { ptr::drop_in_place(pointer.cast::<Probe>()) };
}

unsafe extern "C-unwind" fn complete(context: *mut u8, result: *mut u8) -> i32 {
    assert!(context.addr().is_multiple_of(64));
    assert!(result.addr().is_multiple_of(64));
    // SAFETY: the test allocates context/result with the Probe layout.
    let input = unsafe { ptr::read(context.cast::<Probe>()) };
    let output = Probe {
        value: input.value + 1,
        drops: Arc::clone(&input.drops),
    };
    drop(input);
    // SAFETY: result is uninitialized aligned Probe storage.
    unsafe { ptr::write(result.cast::<Probe>(), output) };
    0
}

unsafe extern "C-unwind" fn fail(context: *mut u8, _result: *mut u8) -> i32 {
    // SAFETY: context contains one initialized Probe, consumed on failure too.
    drop(unsafe { ptr::read(context.cast::<Probe>()) });
    1
}

fn spawn_probe(drops: Arc<AtomicUsize>, entry: Entry) -> usize {
    let size = std::mem::size_of::<Probe>();
    let align = std::mem::align_of::<Probe>();
    // SAFETY: exact validated layout; each allocation is transferred once.
    unsafe {
        let context = crate::vec_runtime::ar_rt_alloc_aligned(size, align);
        let result = crate::vec_runtime::ar_rt_alloc_aligned(size, align);
        assert!(!context.is_null());
        assert!(!result.is_null());
        ptr::write(context.cast::<Probe>(), Probe { value: 41, drops });
        ar_rt_native_thread_spawn(
            context,
            size,
            align,
            entry as *const u8,
            drop_probe as *const u8,
            result,
            size,
            align,
            drop_probe as *const u8,
        )
    }
}

#[test]
fn join_transfers_aligned_owned_result_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = spawn_probe(Arc::clone(&drops), complete);
    assert_ne!(handle, 0);
    let raw = ar_rt_native_thread_join(handle);
    assert!(!raw.is_null());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    // SAFETY: join transfers the initialized Probe allocation to this caller.
    let output = unsafe { ptr::read(raw.cast::<Probe>()) };
    assert_eq!(output.value, 42);
    // SAFETY: the Probe was moved out; only its allocation remains.
    unsafe {
        crate::vec_runtime::ar_rt_free_aligned(
            raw,
            std::mem::size_of::<Probe>(),
            std::mem::align_of::<Probe>(),
        )
    };
    drop(output);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(ar_rt_native_thread_join(handle).is_null());
    ar_rt_native_thread_drop(handle);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

#[test]
fn dropping_handle_joins_and_destroys_unclaimed_result() {
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = spawn_probe(Arc::clone(&drops), complete);
    assert_ne!(handle, 0);
    ar_rt_native_thread_drop(handle);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(!ar_rt_native_thread_is_finished(handle));
}

struct SelfDropContext {
    handle: std::sync::mpsc::Receiver<usize>,
    drops: Arc<AtomicUsize>,
}

impl Drop for SelfDropContext {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

unsafe extern "C" fn drop_self_context(pointer: *mut u8) {
    // SAFETY: this callback is paired with the initialized SelfDropContext.
    unsafe { ptr::drop_in_place(pointer.cast::<SelfDropContext>()) };
}

unsafe extern "C-unwind" fn drop_own_handle(context: *mut u8, _result: *mut u8) -> i32 {
    // SAFETY: the spawn below pairs this pointer with SelfDropContext.
    let context = unsafe { ptr::read(context.cast::<SelfDropContext>()) };
    let handle = context
        .handle
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    ar_rt_native_thread_drop(handle);
    drop(context);
    1
}

#[test]
fn self_drop_returns_without_consuming_the_handle() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (handle_tx, handle_rx) = std::sync::mpsc::channel();
    let context_size = std::mem::size_of::<SelfDropContext>();
    let context_align = std::mem::align_of::<SelfDropContext>();
    // SAFETY: the context/result use their exact layouts and ownership is
    // transferred once. The result is left uninitialized on status 1.
    let handle = unsafe {
        let context = crate::vec_runtime::ar_rt_alloc_aligned(context_size, context_align);
        let result = crate::vec_runtime::ar_rt_alloc_aligned(1, 1);
        assert!(!context.is_null());
        assert!(!result.is_null());
        ptr::write(
            context.cast::<SelfDropContext>(),
            SelfDropContext {
                handle: handle_rx,
                drops: Arc::clone(&drops),
            },
        );
        ar_rt_native_thread_spawn(
            context,
            context_size,
            context_align,
            drop_own_handle as *const u8,
            drop_self_context as *const u8,
            result,
            1,
            1,
            drop_probe as *const u8,
        )
    };
    assert_ne!(handle, 0);
    handle_tx.send(handle).unwrap();
    assert_eq!(ar_rt_native_thread_join(handle), AR_THREAD_PANICKED);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(ar_rt_native_thread_join(handle).is_null());
}

#[test]
fn failure_never_drops_uninitialized_result() {
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = spawn_probe(Arc::clone(&drops), fail);
    assert_ne!(handle, 0);
    assert_eq!(ar_rt_native_thread_join(handle), AR_THREAD_PANICKED);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    // Subsequent join on already-joined handle returns null (JoinFailed)
    assert!(ar_rt_native_thread_join(handle).is_null());
}

unsafe extern "C-unwind" fn panic_worker(_context: *mut u8, _result: *mut u8) -> i32 {
    panic!("simulated worker unwind");
}

#[test]
fn panicked_thread_unwinds_safely_and_returns_sentinel_without_double_drop() {
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = spawn_probe(Arc::clone(&drops), panic_worker);
    assert_ne!(handle, 0);
    assert_eq!(ar_rt_native_thread_join(handle), AR_THREAD_PANICKED);
    // Context was destroyed exactly once during unwind
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    // Subsequent join returns null
    assert!(ar_rt_native_thread_join(handle).is_null());
}

#[test]
fn handles_are_not_reused_after_join() {
    let drops = Arc::new(AtomicUsize::new(0));
    let first = spawn_probe(Arc::clone(&drops), complete);
    ar_rt_native_thread_drop(first);
    let second = spawn_probe(drops, complete);
    assert!(second > first);
    assert!(ar_rt_native_thread_join(first).is_null());
    ar_rt_native_thread_drop(second);
}

unsafe extern "C-unwind" fn descendant(context: *mut u8, _result: *mut u8) -> i32 {
    // SAFETY: this callback receives one Probe and consumes it on failure.
    let input = unsafe { ptr::read(context.cast::<Probe>()) };
    drop(input);
    1
}

unsafe extern "C-unwind" fn parent(context: *mut u8, _result: *mut u8) -> i32 {
    // SAFETY: this callback receives one Probe and consumes it on failure.
    let input = unsafe { ptr::read(context.cast::<Probe>()) };
    let child = spawn_probe(Arc::clone(&input.drops), descendant);
    assert_ne!(child, 0);
    drop(input);
    1
}

#[test]
fn module_barrier_waits_for_descendants_and_result_destruction() {
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = spawn_probe(Arc::clone(&drops), parent);
    assert_ne!(handle, 0);
    join_module_threads(|callback| {
        callback == parent as *const () as usize || callback == descendant as *const () as usize
    });
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(ar_rt_native_thread_join(handle).is_null());
}

#[test]
fn invalid_raw_buffers_are_rejected_without_transfer() {
    assert!(!valid_buffer(std::ptr::null_mut(), 8, 8));
    assert!(!valid_buffer(std::ptr::dangling_mut(), 8, 0));
    assert!(!valid_buffer(std::ptr::dangling_mut(), 8, 3));
    assert!(!valid_buffer(std::ptr::dangling_mut(), 0, 1));
    // SAFETY: null preflight rejects all pointers before dereference/transfer.
    let id = unsafe {
        ar_rt_native_thread_spawn(
            ptr::null_mut(),
            0,
            0,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            0,
            0,
            ptr::null(),
        )
    };
    assert_eq!(id, 0);
}

struct JoiningParent {
    drops: Arc<AtomicUsize>,
    ready: std::sync::mpsc::Sender<()>,
    proceed: std::sync::mpsc::Receiver<()>,
}

unsafe extern "C" fn drop_joining_parent(pointer: *mut u8) {
    // SAFETY: paired with a live JoiningParent by this test's raw constructor.
    unsafe { ptr::drop_in_place(pointer.cast::<JoiningParent>()) };
}

unsafe extern "C-unwind" fn joining_child(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: identical Probe buffers and lifecycle as complete.
    unsafe { complete(context, result) }
}

unsafe extern "C-unwind" fn joining_parent(context: *mut u8, _result: *mut u8) -> i32 {
    // SAFETY: the raw constructor below pairs this with JoiningParent.
    let parent = unsafe { ptr::read(context.cast::<JoiningParent>()) };
    let child = spawn_probe(Arc::clone(&parent.drops), joining_child);
    parent.ready.send(()).unwrap();
    parent.proceed.recv().unwrap();
    let raw = ar_rt_native_thread_join(child);
    assert!(
        !raw.is_null(),
        "module barrier stole a live parent's child handle"
    );
    // SAFETY: join returned the initialized Probe and its original allocation.
    unsafe {
        drop(ptr::read(raw.cast::<Probe>()));
        crate::vec_runtime::ar_rt_free_aligned(
            raw,
            std::mem::size_of::<Probe>(),
            std::mem::align_of::<Probe>(),
        );
    }
    drop(parent);
    1
}

#[test]
fn module_barrier_preserves_parent_join_while_waiting() {
    barrier_preserves_join(false);
}

#[test]
fn module_barrier_waits_for_a_host_claimed_join() {
    barrier_preserves_join(true);
}

fn barrier_preserves_join(claim_parent: bool) {
    let drops = Arc::new(AtomicUsize::new(0));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let context_size = std::mem::size_of::<JoiningParent>();
    let context_align = std::mem::align_of::<JoiningParent>();
    // SAFETY: exact layouts and function signatures, Send-owned channels and
    // Probe values, unique allocation ownership on this raw spawn boundary.
    let handle = unsafe {
        let context = crate::vec_runtime::ar_rt_alloc_aligned(context_size, context_align);
        let result = crate::vec_runtime::ar_rt_alloc_aligned(
            std::mem::size_of::<Probe>(),
            std::mem::align_of::<Probe>(),
        );
        assert!(!context.is_null());
        assert!(!result.is_null());
        ptr::write(
            context.cast::<JoiningParent>(),
            JoiningParent {
                drops: Arc::clone(&drops),
                ready: ready_tx,
                proceed: proceed_rx,
            },
        );
        ar_rt_native_thread_spawn(
            context,
            context_size,
            context_align,
            joining_parent as *const u8,
            drop_joining_parent as *const u8,
            result,
            std::mem::size_of::<Probe>(),
            std::mem::align_of::<Probe>(),
            drop_probe as *const u8,
        )
    };
    assert_ne!(handle, 0);
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let joining = claim_parent.then(|| {
        let joining =
            std::thread::spawn(move || ar_rt_native_thread_join(handle) == AR_THREAD_PANICKED);
        let mut registry = lock_threads();
        while registry
            .get(&handle)
            .is_some_and(|running| running.thread.is_some())
        {
            let (next, timeout) = COMPLETED
                .wait_timeout(registry, std::time::Duration::from_secs(5))
                .unwrap();
            assert!(
                !timeout.timed_out(),
                "host join did not claim the native handle"
            );
            registry = next;
        }
        joining
    });
    let barrier = std::thread::spawn(move || {
        join_module_threads(|callback| {
            callback == joining_parent as *const () as usize
                || callback == joining_child as *const () as usize
        });
        done_tx.send(()).unwrap();
    });
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(30))
            .is_err()
    );
    proceed_tx.send(()).unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    barrier.join().unwrap();
    if let Some(joining) = joining {
        assert!(joining.join().unwrap());
    }
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

struct BlockingDrop {
    started: std::sync::mpsc::Sender<()>,
    proceed: std::sync::mpsc::Receiver<()>,
}

unsafe extern "C" fn blocking_drop(pointer: *mut u8) {
    // SAFETY: this callback is paired with an initialized BlockingDrop value.
    let value = unsafe { ptr::read(pointer.cast::<BlockingDrop>()) };
    value.started.send(()).unwrap();
    value.proceed.recv().unwrap();
    drop(value);
}

unsafe extern "C" fn move_blocking(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: the constructor below pairs both buffers with BlockingDrop.
    unsafe {
        ptr::write(
            result.cast::<BlockingDrop>(),
            ptr::read(context.cast::<BlockingDrop>()),
        )
    };
    0
}

#[test]
fn module_barrier_waits_for_claimed_drop_and_result_destructor() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let size = std::mem::size_of::<BlockingDrop>();
    let align = std::mem::align_of::<BlockingDrop>();
    // SAFETY: both buffers use the exact layout and uniquely owned Send
    // channels. move_blocking consumes context and initializes result once.
    let handle = unsafe {
        let context = crate::vec_runtime::ar_rt_alloc_aligned(size, align);
        let result = crate::vec_runtime::ar_rt_alloc_aligned(size, align);
        assert!(!context.is_null());
        assert!(!result.is_null());
        ptr::write(
            context.cast::<BlockingDrop>(),
            BlockingDrop {
                started: started_tx,
                proceed: proceed_rx,
            },
        );
        ar_rt_native_thread_spawn(
            context,
            size,
            align,
            move_blocking as *const u8,
            blocking_drop as *const u8,
            result,
            size,
            align,
            blocking_drop as *const u8,
        )
    };
    assert_ne!(handle, 0);
    let dropping = std::thread::spawn(move || ar_rt_native_thread_drop(handle));
    // At this point the native child has returned and the host claimant is
    // executing JIT-owned result drop glue; the registry must still retain it.
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let barrier = std::thread::spawn(move || {
        join_module_threads(|callback| {
            callback == move_blocking as *const () as usize
                || callback == blocking_drop as *const () as usize
        });
        done_tx.send(()).unwrap();
    });
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(30))
            .is_err()
    );
    proceed_tx.send(()).unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    dropping.join().unwrap();
    barrier.join().unwrap();
}
