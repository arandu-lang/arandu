//! SL_R.0 — cooperative multi-task host for debug JIT (i64 payload MVP).
//!
//! Complements [`crate::poll_runtime`] (single-coroutine poll/block_on).
//!
//! ## Model
//! - Explicit handles, no global language-level executor in user code beyond
//!   these host symbols (stdlib wraps them as `SyncExecutor`).
//! - `spawn` parks a coroutine state blob; `join` drives it with
//!   [`crate::poll_runtime::ar_co_block_on_i64`].
//! - Cooperative only: Pending spins (no OS reactor yet — SL_R.2).

mod entropy;
mod path;
mod spawn;
mod thread;

pub use entropy::ar_rt_get_entropy;
pub use path::{
    ArFatStr, ar_path_file_name, ar_path_is_absolute, ar_path_is_empty, ar_path_join,
    ar_path_join_owned, ar_str_concat, ar_str_split_last,
};
pub(crate) use path::{fat_str_from_string, path_from_fat};
pub use spawn::{ar_rt_block_on_i64, ar_rt_cancel_i64, ar_rt_join_i64, ar_rt_spawn_i64};
pub use thread::{
    ar_rt_condvar_notify_all, ar_rt_condvar_notify_one, ar_rt_condvar_wait,
    ar_rt_condvar_wait_timeout, ar_rt_thread_current_id, ar_rt_thread_park,
    ar_rt_thread_park_timeout, ar_rt_thread_sleep_ms, ar_rt_thread_unpark, ar_rt_thread_yield,
};

#[cfg(test)]
use entropy::fill_entropy_with;
#[cfg(test)]
use spawn::{TaskState, TaskTable, cancel_on, join_on, spawn_on};
#[cfg(test)]
use thread::{PARKING_LOT_BUCKETS, THREAD_REGISTRY, get_bucket};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll_runtime::{ar_co_block_on_i64, ar_co_make_ready_i64, ar_co_pending_once_i64};
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn entropy_failure_clears_partial_output_without_fallback() {
        let mut bytes = [0x55; 32];
        let status = fill_entropy_with(&mut bytes, |dest| {
            dest[..3].copy_from_slice(&[1, 2, 3]);
            Err(std::io::Error::other("injected entropy failure"))
        });
        assert_eq!(status, -1);
        assert_eq!(bytes, [0; 32]);
        assert_eq!(
            fill_entropy_with(&mut bytes, |dest| {
                dest.fill(0x42);
                Ok(())
            }),
            0
        );
        assert_eq!(bytes, [0x42; 32]);
    }

    #[test]
    fn entropy_ffi_rejects_invalid_buffers_and_accepts_empty_requests() {
        // SAFETY: invalid pointers/lengths are rejected before constructing a
        // slice; the empty request accesses no storage.
        unsafe {
            assert_eq!(ar_rt_get_entropy(std::ptr::null_mut(), 0), 0);
            assert_eq!(ar_rt_get_entropy(std::ptr::null_mut(), 1), -1);
            assert_eq!(ar_rt_get_entropy(std::ptr::dangling_mut(), usize::MAX), -1);
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn entropy_os_source_fills_a_live_buffer() {
        let mut bytes = [0; 32];
        // SAFETY: the local array is live and exclusively borrowed for the call.
        assert_eq!(
            unsafe { ar_rt_get_entropy(bytes.as_mut_ptr(), bytes.len()) },
            0
        );
    }

    #[test]
    fn spawn_join_ready() {
        unsafe {
            let s = ar_co_make_ready_i64(42);
            let h = ar_rt_spawn_i64(s);
            assert_eq!(ar_rt_join_i64(h), 42);
            ar_rt_cancel_i64(h);
        }
    }

    #[test]
    fn completed_join_is_cached_until_cancel_releases_the_slot() {
        let tasks = TaskTable::new(Vec::new());
        // SAFETY: every blob is transferred to its own live task and freed by
        // join/cancel; the isolated table prevents unrelated tests reusing slots.
        unsafe {
            for value in 0..64 {
                let handle = spawn_on(&tasks, ar_co_make_ready_i64(value));
                assert_eq!(handle, 0, "cancel must make the slot reusable");
                assert_eq!(join_on(&tasks, handle, |s| ar_co_block_on_i64(s)), value);
                assert_eq!(
                    join_on(&tasks, handle, |_| unreachable!(
                        "cached join must not poll"
                    )),
                    value
                );
                cancel_on(&tasks, handle);
                assert!(tasks.lock().unwrap()[0].is_none());
            }
        }
    }

    #[test]
    fn cancel_before_join_releases_pending_state_and_slot() {
        let tasks = TaskTable::new(Vec::new());
        // SAFETY: no join exists and cancellation receives exclusive ownership.
        unsafe {
            for value in 0..64 {
                let handle = spawn_on(&tasks, ar_co_pending_once_i64(value));
                assert_eq!(handle, 0);
                cancel_on(&tasks, handle);
                assert!(tasks.lock().unwrap()[0].is_none());
            }
        }
    }

    #[test]
    fn concurrent_join_then_cancel_frees_once() {
        use std::sync::{Arc, mpsc};
        let tasks = Arc::new(TaskTable::new(Vec::new()));
        for value in 0..64 {
            // SAFETY: the channels establish that join owns the blob before
            // cancellation. Neither thread accesses the blob after join frees it.
            unsafe {
                let handle = spawn_on(&tasks, ar_co_pending_once_i64(value));
                assert_eq!(handle, 0);
                let (claimed_tx, claimed_rx) = mpsc::channel();
                let (resume_tx, resume_rx) = mpsc::channel();
                let joining_tasks = Arc::clone(&tasks);
                let joiner = std::thread::spawn(move || {
                    join_on(&joining_tasks, handle, |state| {
                        claimed_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        ar_co_block_on_i64(state)
                    })
                });
                claimed_rx.recv().unwrap();
                cancel_on(&tasks, handle);
                assert!(matches!(
                    tasks.lock().unwrap()[0],
                    Some(TaskState::Running {
                        cancel_requested: true
                    })
                ));
                resume_tx.send(()).unwrap();
                assert_eq!(joiner.join().unwrap(), value);
                assert!(tasks.lock().unwrap()[0].is_none());
            }
        }
    }

    #[test]
    fn path_absolute() {
        unsafe {
            let current_dir = std::env::current_dir().unwrap();
            let current_dir = current_dir.to_string_lossy();
            assert_eq!(
                ar_path_is_absolute(current_dir.as_ptr(), current_dir.len() as isize),
                1
            );
            assert_eq!(ar_path_is_absolute(b"rel".as_ptr(), 3), 0);
            assert_eq!(ar_path_is_absolute(b"".as_ptr(), 0), 0);
            assert_eq!(ar_path_is_absolute(b"./x".as_ptr(), 3), 0);
            assert_eq!(ar_path_is_empty(b"".as_ptr(), 0), 1);
        }
    }

    #[test]
    fn path_join_and_file_name() {
        unsafe {
            let j = ar_path_join(b"/tmp".as_ptr(), 4, b"x".as_ptr(), 1);
            let s = std::slice::from_raw_parts(j.ptr, j.len as usize);
            assert_eq!(
                s,
                std::path::Path::new("/tmp")
                    .join("x")
                    .to_string_lossy()
                    .as_bytes()
            );

            let j2 = ar_path_join(b"a".as_ptr(), 1, b"b".as_ptr(), 1);
            let s2 = std::slice::from_raw_parts(j2.ptr, j2.len as usize);
            assert_eq!(
                s2,
                std::path::Path::new("a")
                    .join("b")
                    .to_string_lossy()
                    .as_bytes()
            );

            let fnm = ar_path_file_name(b"/tmp/leaf".as_ptr(), 9);
            let sn = std::slice::from_raw_parts(fnm.ptr, fnm.len as usize);
            assert_eq!(sn, b"leaf");

            let leaf = ar_path_file_name(b"leaf".as_ptr(), 4);
            let sl = std::slice::from_raw_parts(leaf.ptr, leaf.len as usize);
            assert_eq!(sl, b"leaf");
        }
    }

    #[test]
    fn owned_path_join_returns_an_exact_caller_owned_buffer() {
        unsafe {
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            let mut capacity = 0;
            assert_eq!(
                ar_path_join_owned(
                    b"/tmp".as_ptr(),
                    4,
                    b"owned".as_ptr(),
                    5,
                    &mut data,
                    &mut len,
                    &mut capacity,
                ),
                1
            );
            let expected = std::path::Path::new("/tmp")
                .join("owned")
                .to_string_lossy()
                .into_owned();
            assert_eq!(std::slice::from_raw_parts(data, len), expected.as_bytes());
            assert_eq!(capacity, len);
            crate::vec_runtime::ar_vec_buf_free(data, capacity);
        }
    }

    #[test]
    fn str_concat_prefix_suffix_split() {
        unsafe {
            let c = ar_str_concat(b"ab".as_ptr(), 2, b"cd".as_ptr(), 2);
            let cs = std::slice::from_raw_parts(c.ptr, c.len as usize);
            assert_eq!(cs, b"abcd");
            let tail = ar_str_split_last(b"a/b/c".as_ptr(), 5, b"/".as_ptr(), 1);
            let ts = std::slice::from_raw_parts(tail.ptr, tail.len as usize);
            assert_eq!(ts, b"c");
        }
    }

    fn wait_for_monitor(key: usize, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let registered = get_bucket(key)
                .lock()
                .unwrap()
                .monitors
                .get(&key)
                .map_or(0, VecDeque::len);
            if registered == count {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "waiter registration timed out"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn thread_registry_retires_exited_threads_without_reusing_ids() {
        let mut last = 0;
        for _ in 0..32 {
            let id = std::thread::spawn(|| unsafe { ar_rt_thread_current_id() })
                .join()
                .unwrap();
            assert!(id > last);
            last = id;
            assert!(
                !THREAD_REGISTRY
                    .read()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .contains_key(&id)
            );
            // A stale unpark cannot affect another thread or recreate its record.
            unsafe {
                ar_rt_thread_unpark(id);
            }
        }
    }

    #[test]
    fn condvar_preserves_explicit_thread_unpark_token() {
        let seq = Arc::new(AtomicUsize::new(0));
        let address = Arc::as_ptr(&seq) as usize;
        let child_seq = Arc::clone(&seq);
        let (id_tx, id_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let child = std::thread::spawn(move || unsafe {
            let id = ar_rt_thread_current_id();
            ar_rt_thread_unpark(id);
            id_tx.send(id).unwrap();
            ar_rt_condvar_wait(Arc::as_ptr(&child_seq).cast(), 0);
            ar_rt_thread_park(); // must consume the preserved explicit token
            done_tx.send(()).unwrap();
        });
        id_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        wait_for_monitor(address, 1);
        seq.fetch_add(1, Ordering::Release);
        unsafe {
            ar_rt_condvar_notify_one(address as *const u8);
        }
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        child.join().unwrap();
        assert!(
            !get_bucket(address)
                .lock()
                .unwrap()
                .monitors
                .contains_key(&address)
        );
    }

    #[test]
    fn condvar_distinguishes_colliding_keys_and_notifies_fifo() {
        let words: Vec<_> = (0..=PARKING_LOT_BUCKETS)
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect();
        let mut collision = None;
        'outer: for (i, first) in words.iter().enumerate() {
            for second in &words[i + 1..] {
                if std::ptr::eq(
                    get_bucket(Arc::as_ptr(first) as usize),
                    get_bucket(Arc::as_ptr(second) as usize),
                ) {
                    collision = Some((Arc::clone(first), Arc::clone(second)));
                    break 'outer;
                }
            }
        }
        let (first, second) = collision.unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut children = Vec::new();
        // Register two waiters on the first key in a known order, one on the
        // colliding key. notify_one must wake only the oldest first-key waiter.
        for (index, word) in [Arc::clone(&first), Arc::clone(&second), Arc::clone(&first)]
            .into_iter()
            .enumerate()
        {
            let addr = Arc::as_ptr(&word) as usize;
            let tx = tx.clone();
            children.push(std::thread::spawn(move || {
                let result =
                    unsafe { ar_rt_condvar_wait_timeout(Arc::as_ptr(&word).cast(), 0, 5_000) };
                tx.send((index, result)).unwrap();
            }));
            wait_for_monitor(addr, if index == 2 { 2 } else { 1 });
        }
        first.fetch_add(1, Ordering::Release);
        unsafe {
            ar_rt_condvar_notify_one(Arc::as_ptr(&first).cast());
        }
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            (0, 1)
        );
        wait_for_monitor(Arc::as_ptr(&first) as usize, 1);
        wait_for_monitor(Arc::as_ptr(&second) as usize, 1);
        unsafe {
            ar_rt_condvar_notify_all(Arc::as_ptr(&first).cast());
        }
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            (2, 1)
        );
        second.fetch_add(1, Ordering::Release);
        unsafe {
            ar_rt_condvar_notify_all(Arc::as_ptr(&second).cast());
        }
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            (1, 1)
        );
        for child in children {
            child.join().unwrap();
        }
    }

    #[test]
    fn condvar_timeout_and_changed_sequence_remove_registrations() {
        let seq = AtomicUsize::new(1);
        let key = (&seq as *const AtomicUsize).cast::<u8>();
        unsafe {
            assert_eq!(ar_rt_condvar_wait_timeout(key, 0, 5_000), 1);
            assert_eq!(ar_rt_condvar_wait_timeout(key, 1, 0), 0);
            assert_eq!(ar_rt_condvar_wait_timeout(key, 1, 1), 0);
            assert_eq!(ar_rt_condvar_wait_timeout(std::ptr::null(), 0, 0), -1);
            assert_eq!(ar_rt_condvar_wait_timeout(key.wrapping_add(1), 0, 0), -1);
        }
        assert!(
            !get_bucket(key as usize)
                .lock()
                .unwrap()
                .monitors
                .contains_key(&(key as usize))
        );
    }

    #[test]
    fn park_timeout_wakes_early_on_unpark_and_consumes_preexisting_token() {
        // 1. Pre-existing token consumed immediately without waiting for timeout:
        unsafe {
            let id = ar_rt_thread_current_id();
            ar_rt_thread_unpark(id);
            let start = std::time::Instant::now();
            ar_rt_thread_park_timeout(10_000); // 10 seconds timeout
            assert!(
                start.elapsed() < std::time::Duration::from_millis(500),
                "pre-existing unpark token must wake park_timeout immediately"
            );
        }

        // 2. Unpark during wait interrupts park_timeout immediately:
        let (id_tx, id_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let child = std::thread::spawn(move || unsafe {
            let id = ar_rt_thread_current_id();
            id_tx.send(id).unwrap();
            let start = std::time::Instant::now();
            ar_rt_thread_park_timeout(10_000); // 10 seconds timeout
            let elapsed = start.elapsed();
            done_tx.send(elapsed).unwrap();
        });

        let child_id = id_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Give the child thread time to enter park_timeout
        std::thread::sleep(std::time::Duration::from_millis(30));
        unsafe {
            ar_rt_thread_unpark(child_id);
        }

        let elapsed = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(1_000),
            "unpark must interrupt park_timeout well before 10s timeout, took: {elapsed:?}"
        );
        child.join().unwrap();

        // 3. Timeout without token expires after requested duration:
        unsafe {
            let start = std::time::Instant::now();
            ar_rt_thread_park_timeout(20); // 20 ms
            let elapsed = start.elapsed();
            assert!(
                elapsed >= std::time::Duration::from_millis(15),
                "park_timeout without token must wait for requested timeout, took: {elapsed:?}"
            );
        }

        // 4. Race timeout vs unpark: completes deterministically without hanging
        let (race_id_tx, race_id_rx) = std::sync::mpsc::channel();
        let race_child = std::thread::spawn(move || unsafe {
            let id = ar_rt_thread_current_id();
            race_id_tx.send(id).unwrap();
            ar_rt_thread_park_timeout(10); // 10 ms
        });
        let race_id = race_id_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        unsafe {
            ar_rt_thread_unpark(race_id);
        }
        race_child.join().unwrap();
    }
}

mod phase_lock;
pub use phase_lock::{
    ar_rt_rwlock_read, ar_rt_rwlock_read_unlock, ar_rt_rwlock_try_read, ar_rt_rwlock_try_write,
    ar_rt_rwlock_write, ar_rt_rwlock_write_unlock,
};
