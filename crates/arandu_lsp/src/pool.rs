//! Bounded, priority-aware worker scheduler for IDE jobs.

use arandu_query::DocumentId;
use lsp_server::RequestId;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

const DEFAULT_QUEUE_CAPACITY: usize = 64;
type JobFn = Box<dyn FnOnce(CancellationToken) + Send + 'static>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum JobKey {
    Request(RequestId),
    Diagnostics(DocumentId),
    WorkspaceReload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Priority {
    Interactive,
    Background,
}

struct CancellationState {
    state: AtomicU8,
    queries: Mutex<Vec<arandu_query::QueryCancellationToken>>,
}

#[derive(Clone)]
pub struct CancellationToken(Arc<CancellationState>);

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl CancellationToken {
    fn new() -> Self {
        Self(Arc::new(CancellationState {
            state: AtomicU8::new(0),
            queries: Mutex::new(Vec::new()),
        }))
    }

    pub fn cancel(&self) {
        self.0.state.fetch_max(1, Ordering::Release);
        let queries = self
            .0
            .queries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for query in queries.iter() {
            query.cancel();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.state.load(Ordering::Acquire) != 0
    }

    pub fn claim_cancelled(&self) -> bool {
        self.0
            .state
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn link_query(&self, query: arandu_query::QueryCancellationToken) {
        let mut queries = self
            .0
            .queries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_cancelled() {
            drop(queries);
            query.cancel();
        } else {
            queries.push(query);
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

struct Job {
    key: Option<JobKey>,
    cancellation: CancellationToken,
    run: JobFn,
}

#[derive(Default)]
struct QueueState {
    interactive: VecDeque<Job>,
    background: VecDeque<Job>,
    active: FxHashMap<JobKey, CancellationToken>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<QueueState>,
    ready: Condvar,
    capacity: usize,
}

pub struct WorkerPool {
    shared: Arc<Shared>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.shutdown = true;
        for token in state.active.values() {
            token.cancel();
        }
        for job in state.interactive.iter().chain(&state.background) {
            job.cancellation.cancel();
        }
        drop(state);
        self.shared.ready.notify_all();
        for worker in std::mem::take(&mut self.workers) {
            if let Err(payload) = worker.join() {
                crate::logging::log_panic("joining LSP worker", &payload);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueFull;

impl WorkerPool {
    pub fn new(workers: usize) -> std::io::Result<Self> {
        Self::with_capacity(workers, DEFAULT_QUEUE_CAPACITY)
    }

    fn with_capacity(workers: usize, capacity: usize) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
            capacity: capacity.max(1),
        });
        let mut handles = Vec::with_capacity(workers.clamp(1, 16));
        for i in 0..workers.clamp(1, 16) {
            let worker_shared = Arc::clone(&shared);
            match thread::Builder::new()
                .name(format!("arandu-lsp-worker-{i}"))
                .spawn(move || worker_loop(&worker_shared))
            {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.shutdown = true;
                    drop(state);
                    shared.ready.notify_all();
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            shared,
            workers: handles,
        })
    }

    pub fn spawn<F>(
        &self,
        priority: Priority,
        key: Option<JobKey>,
        f: F,
    ) -> Result<CancellationToken, QueueFull>
    where
        F: FnOnce(CancellationToken) + Send + 'static,
    {
        let cancellation = CancellationToken::new();
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = key.as_ref() {
            remove_queued_key(&mut state, key);
            if let Some(previous) = state.active.insert(key.clone(), cancellation.clone()) {
                previous.cancel();
            }
        }
        if state.interactive.len() + state.background.len() >= self.shared.capacity {
            if priority == Priority::Interactive {
                if let Some(evicted) = state.background.pop_front() {
                    retire_job(&mut state, &evicted);
                    evicted.cancellation.cancel();
                } else {
                    remove_active_token(&mut state, key.as_ref(), &cancellation);
                    return Err(QueueFull);
                }
            } else {
                remove_active_token(&mut state, key.as_ref(), &cancellation);
                return Err(QueueFull);
            }
        }
        let job = Job {
            key,
            cancellation: cancellation.clone(),
            run: Box::new(f),
        };
        match priority {
            Priority::Interactive => state.interactive.push_back(job),
            Priority::Background => state.background.push_back(job),
        }
        drop(state);
        self.shared.ready.notify_one();
        Ok(cancellation)
    }

    #[cfg(test)]
    pub fn cancel(&self, key: &JobKey) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(token) = state.active.get(key).cloned() else {
            return false;
        };
        token.cancel();
        let queued = take_queued_key(&mut state, key);
        if let Some(job) = queued.as_ref() {
            retire_job(&mut state, job);
        }
        drop(state);
        if let Some(job) = queued {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| (job.run)(token))) {
                crate::logging::log_panic("synchronously cancelled job", &payload);
            }
        }
        true
    }

    /// Cancel an explicit client request and claim its single protocol reply.
    /// A queued closure is dropped instead of run; an already-running closure
    /// observes state 2 and cannot publish a duplicate terminal response.
    pub fn cancel_for_response(&self, key: &JobKey) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(token) = state.active.get(key).cloned() else {
            return false;
        };
        token.cancel();
        if let Some(job) = take_queued_key(&mut state, key) {
            retire_job(&mut state, &job);
        }
        drop(state);
        token.claim_cancelled()
    }

    /// Retire snapshot jobs before the event-loop writer mutates Salsa.
    /// Queued closures are dropped, never run on the writer thread: their
    /// cancellation response could block on the channel that writer drains.
    pub fn cancel_analysis(&self) -> Vec<RequestId> {
        let keys: Vec<JobKey> = {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .active
                .keys()
                .filter(|key| matches!(key, JobKey::Request(_) | JobKey::Diagnostics(_)))
                .cloned()
                .collect()
        };
        keys.into_iter()
            .filter_map(|key| {
                let claimed = self.cancel_for_response(&key);
                match key {
                    JobKey::Request(id) if claimed => Some(id),
                    _ => None,
                }
            })
            .collect()
    }
}

fn worker_loop(shared: &Shared) {
    loop {
        let job = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while state.interactive.is_empty() && state.background.is_empty() && !state.shutdown {
                state = shared.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.shutdown && state.interactive.is_empty() && state.background.is_empty() {
                return;
            }
            let Some(job) = state
                .interactive
                .pop_front()
                .or_else(|| state.background.pop_front())
            else {
                // The lock and predicate above normally guarantee a job. Keep
                // this defensive branch safe if the queue policy evolves.
                continue;
            };
            job
        };
        let token = job.cancellation.clone();
        let key = job.key.clone();
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| (job.run)(token.clone()))) {
            crate::logging::log_panic("worker job", &payload);
        }
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        remove_active_token(&mut state, key.as_ref(), &token);
    }
}

fn remove_queued_key(state: &mut QueueState, key: &JobKey) {
    if let Some(job) = take_queued_key(state, key) {
        job.cancellation.cancel();
    }
}

fn take_queued_key(state: &mut QueueState, key: &JobKey) -> Option<Job> {
    for queue in [&mut state.interactive, &mut state.background] {
        if let Some(index) = queue.iter().position(|job| job.key.as_ref() == Some(key)) {
            return queue.remove(index);
        }
    }
    None
}

fn retire_job(state: &mut QueueState, job: &Job) {
    remove_active_token(state, job.key.as_ref(), &job.cancellation);
}

fn remove_active_token(
    state: &mut QueueState,
    key: Option<&JobKey>,
    cancellation: &CancellationToken,
) {
    let Some(key) = key else { return };
    if state
        .active
        .get(key)
        .is_some_and(|active| active.same_as(cancellation))
    {
        state.active.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn panic_in_one_job_does_not_kill_the_worker() {
        let pool = WorkerPool::new(1).expect("test worker must start");
        let (tx, rx) = mpsc::channel();
        pool.spawn(Priority::Interactive, None, |_| {
            panic!("synthetic worker failure")
        })
        .expect("panic job must queue");
        pool.spawn(Priority::Interactive, None, move |_| {
            tx.send(42).expect("test receiver must remain alive")
        })
        .expect("follow-up job must queue");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(42));
    }

    #[test]
    fn interactive_job_overtakes_background_backlog() {
        let pool = WorkerPool::with_capacity(1, 4).expect("test worker must start");
        let (release_tx, release_rx) = mpsc::channel();
        let (order_tx, order_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |_| {
            release_rx.recv().expect("release first job");
        })
        .expect("blocking job must queue");
        for value in 1..=2 {
            let tx = order_tx.clone();
            pool.spawn(Priority::Background, None, move |_| {
                tx.send(value).expect("record background order")
            })
            .expect("background job must queue");
        }
        let tx = order_tx;
        pool.spawn(Priority::Interactive, None, move |_| {
            tx.send(0).expect("record interactive order")
        })
        .expect("interactive job must queue");
        release_tx.send(()).expect("release worker");
        assert_eq!(order_rx.recv_timeout(Duration::from_secs(2)), Ok(0));
    }

    #[test]
    fn keyed_background_jobs_coalesce_and_cancel_previous_work() {
        let pool = WorkerPool::with_capacity(1, 3).expect("test worker must start");
        let (release_tx, release_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        pool.spawn(Priority::Interactive, None, move |_| {
            release_rx.recv().expect("release first job");
        })
        .expect("blocking job must queue");
        let key = JobKey::Request(7.into());
        let first = pool
            .spawn(Priority::Background, Some(key.clone()), {
                let tx = tx.clone();
                move |_| tx.send(1).expect("record stale job")
            })
            .expect("first keyed job must queue");
        pool.spawn(Priority::Background, Some(key), move |_| {
            tx.send(2).expect("record replacement job")
        })
        .expect("replacement keyed job must queue");
        assert!(first.is_cancelled());
        release_tx.send(()).expect("release worker");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(2));
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn bounded_queue_drops_background_and_admits_interactive_work() {
        let pool = WorkerPool::with_capacity(1, 1).expect("test worker must start");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |_| {
            started_tx.send(()).expect("signal running job");
            release_rx.recv().expect("release running job");
        })
        .expect("running job must queue");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker must start first job");

        let background = pool
            .spawn(Priority::Background, None, |_| {})
            .expect("one background job fits");
        assert!(matches!(
            pool.spawn(Priority::Background, None, |_| {}),
            Err(QueueFull)
        ));
        pool.spawn(Priority::Interactive, None, |_| {})
            .expect("interactive work evicts queued background work");
        assert!(background.is_cancelled());
        release_tx.send(()).expect("release running job");
    }

    #[test]
    fn cancelling_a_queued_request_runs_its_short_cancel_path_immediately() {
        let pool = WorkerPool::with_capacity(1, 2).expect("test worker must start");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |_| {
            started_tx.send(()).expect("signal running job");
            release_rx.recv().expect("release running job");
        })
        .expect("running job must queue");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker must start first job");

        let key = JobKey::Request(9.into());
        let (tx, rx) = mpsc::channel();
        pool.spawn(Priority::Interactive, Some(key.clone()), move |token| {
            tx.send(token.is_cancelled()).expect("record cancellation")
        })
        .expect("request must queue");
        assert!(pool.cancel(&key));
        assert_eq!(rx.recv_timeout(Duration::from_millis(100)), Ok(true));
        release_tx.send(()).expect("release running job");
    }

    #[test]
    fn cancelling_one_active_request_does_not_cancel_another() {
        let pool = WorkerPool::with_capacity(2, 4).expect("test workers must start");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let (release_second_tx, release_second_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();

        let first_key = JobKey::Request(10.into());
        pool.spawn(Priority::Interactive, Some(first_key.clone()), {
            let started_tx = started_tx.clone();
            let result_tx = result_tx.clone();
            move |token| {
                started_tx.send(10).expect("record first request start");
                release_first_rx.recv().expect("release first request");
                result_tx
                    .send((10, token.is_cancelled()))
                    .expect("record first request result");
            }
        })
        .expect("first request must queue");

        pool.spawn(Priority::Interactive, Some(JobKey::Request(11.into())), {
            let started_tx = started_tx;
            let result_tx = result_tx;
            move |token| {
                started_tx.send(11).expect("record second request start");
                release_second_rx.recv().expect("release second request");
                result_tx
                    .send((11, token.is_cancelled()))
                    .expect("record second request result");
            }
        })
        .expect("second request must queue");

        let mut started = [
            started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("one request must start"),
            started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("both workers must run concurrently"),
        ];
        started.sort_unstable();
        assert_eq!(started, [10, 11]);

        assert!(pool.cancel(&first_key));
        release_first_tx.send(()).expect("release first request");
        release_second_tx.send(()).expect("release second request");

        let mut results = [
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("first result must arrive"),
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("second result must arrive"),
        ];
        results.sort_unstable_by_key(|result| result.0);
        assert_eq!(results, [(10, true), (11, false)]);
    }

    #[test]
    fn drop_cancels_queued_jobs_and_joins_the_worker() {
        let pool = WorkerPool::with_capacity(1, 2).expect("test worker must start");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |_| {
            started_tx.send(()).expect("signal running job");
            release_rx.recv().expect("release running job");
        })
        .expect("running job must queue");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker must start first job");

        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |token| {
            cancelled_tx
                .send(token.is_cancelled())
                .expect("record queued cancellation");
        })
        .expect("queued job must fit");

        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            release_tx.send(()).expect("release running job");
        });
        drop(pool);
        releaser.join().expect("release helper must join cleanly");
        assert_eq!(cancelled_rx.recv_timeout(Duration::from_secs(2)), Ok(true));
    }
}
