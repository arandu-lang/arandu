use super::*;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

fn await_queued(key: usize, readers: usize, writers: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        {
            let entries = states(key).lock().unwrap();
            if entries.get(&key).is_some_and(|state| {
                state.waiting_readers.len() == readers && state.waiting_writers.len() == writers
            }) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "lock waiter did not register");
        std::thread::yield_now();
    }
}

fn worker(key: usize, write: bool, id: u8, acquired: Sender<u8>, release: Receiver<()>) {
    // SAFETY: tests keep their key allocations stationary until joins; each
    // acquire is paired with exactly one release, no protected data escapes.
    unsafe {
        if write {
            ar_rt_rwlock_write(key as *mut u8);
        } else {
            ar_rt_rwlock_read(key as *mut u8);
        }
        acquired.send(id).unwrap();
        release.recv_timeout(Duration::from_secs(5)).unwrap();
        if write {
            ar_rt_rwlock_write_unlock(key as *mut u8);
        } else {
            ar_rt_rwlock_read_unlock(key as *mut u8);
        }
    }
}

#[test]
fn phase_fair_read_batches_alternate_with_fifo_writers() {
    let mut allocation = 0u8;
    let key = (&raw mut allocation) as usize;
    let (acquired, arrivals) = mpsc::channel();
    // SAFETY: live key; the initial writer is released after registration.
    unsafe {
        ar_rt_rwlock_write(key as *mut u8);
    }
    let (r1, release_r1) = mpsc::channel();
    let (r2, release_r2) = mpsc::channel();
    let (w1, release_w1) = mpsc::channel();
    let (w2, release_w2) = mpsc::channel();
    let (r3, release_r3) = mpsc::channel();
    let tx = acquired.clone();
    let first_reader = std::thread::spawn(move || worker(key, false, 1, tx, release_r1));
    await_queued(key, 1, 0);
    let tx = acquired.clone();
    let first_writer = std::thread::spawn(move || worker(key, true, 3, tx, release_w1));
    await_queued(key, 1, 1);
    let tx = acquired.clone();
    let second_reader = std::thread::spawn(move || worker(key, false, 2, tx, release_r2));
    await_queued(key, 2, 1);
    let tx = acquired.clone();
    let second_writer = std::thread::spawn(move || worker(key, true, 5, tx, release_w2));
    await_queued(key, 2, 2);
    // SAFETY: release the initial writer exactly once.
    unsafe {
        ar_rt_rwlock_write_unlock(key as *mut u8);
    }
    let mut batch = [
        arrivals.recv_timeout(Duration::from_secs(5)).unwrap(),
        arrivals.recv_timeout(Duration::from_secs(5)).unwrap(),
    ];
    batch.sort_unstable();
    assert_eq!(batch, [1, 2]);
    let third_reader = std::thread::spawn(move || worker(key, false, 4, acquired, release_r3));
    await_queued(key, 1, 2);
    r1.send(()).unwrap();
    r2.send(()).unwrap();
    assert_eq!(arrivals.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
    assert!(arrivals.try_recv().is_err());
    w1.send(()).unwrap();
    assert_eq!(arrivals.recv_timeout(Duration::from_secs(5)).unwrap(), 4);
    assert!(arrivals.try_recv().is_err());
    r3.send(()).unwrap();
    assert_eq!(arrivals.recv_timeout(Duration::from_secs(5)).unwrap(), 5);
    w2.send(()).unwrap();
    for join in [
        first_reader,
        second_reader,
        third_reader,
        first_writer,
        second_writer,
    ] {
        join.join().unwrap();
    }
    assert!(!states(key).lock().unwrap().contains_key(&key));
}

#[test]
fn phase_lock_try_admission_and_address_isolation() {
    let mut keys = [0u8; 2];
    let a = keys.as_mut_ptr();
    // SAFETY: both addresses remain live; matching releases are below.
    unsafe {
        let b = a.add(1);
        assert_eq!(ar_rt_rwlock_try_read(a), 1);
        assert_eq!(ar_rt_rwlock_try_read(a), 1);
        assert_eq!(ar_rt_rwlock_try_write(a), 0);
        assert_eq!(ar_rt_rwlock_try_write(b), 1);
        assert_eq!(ar_rt_rwlock_try_read(b), 0);
        ar_rt_rwlock_read_unlock(a);
        assert_eq!(ar_rt_rwlock_try_write(a), 0);
        ar_rt_rwlock_read_unlock(a);
        assert_eq!(ar_rt_rwlock_try_write(a), 1);
        ar_rt_rwlock_write_unlock(a);
        ar_rt_rwlock_write_unlock(b);
    }
    assert!(
        !states(a as usize)
            .lock()
            .unwrap()
            .contains_key(&(a as usize))
    );
}

#[test]
fn native_mutex_queue_serializes_updates_and_retires_state() {
    use std::cell::UnsafeCell;
    struct Protected(UnsafeCell<usize>);
    // SAFETY: this test accesses the value only under the tested write lock.
    unsafe impl Sync for Protected {}
    let data = Protected(UnsafeCell::new(0));
    let key = data.0.get() as usize;
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let data = &data;
            scope.spawn(move || {
                for _ in 0..1000 {
                    // SAFETY: stable scope lifetime and exclusive writer access.
                    unsafe {
                        ar_rt_rwlock_write(key as *mut u8);
                        *data.0.get() += 1;
                        ar_rt_rwlock_write_unlock(key as *mut u8);
                    }
                }
            });
        }
    });
    // SAFETY: every thread has joined and no guard remains.
    assert_eq!(unsafe { *data.0.get() }, 8000);
    assert!(!states(key).lock().unwrap().contains_key(&key));
}
