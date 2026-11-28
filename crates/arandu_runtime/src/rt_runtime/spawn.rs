use crate::poll_runtime::{ar_co_block_on_i64, ar_co_free};
use std::sync::Mutex;

/// Ownership of the coroutine blob changes exactly once at Pending -> Running.
/// Running stores no pointer: only the joining thread can poll/free the blob.
pub(super) enum TaskState {
    Pending(*mut u8),
    Running { cancel_requested: bool },
    Completed(i64),
}

// SAFETY: pending blobs are accessed only while holding the task-table mutex.
// A join transfers the pointer to its own thread and leaves no pointer in the
// shared Running state. Cancellation can only mark that state for retirement.
unsafe impl Send for TaskState {}

pub(super) struct TaskSlot {
    pub(super) generation: u32,
    pub(super) state: Option<TaskState>,
}

pub(super) type TaskTable = Mutex<Vec<TaskSlot>>;
static TASKS: TaskTable = Mutex::new(Vec::new());

/// Spawn a coroutine state onto the SyncExecutor queue. Returns handle (>= 0).
///
/// # Safety
/// `state` must be a valid coroutine blob exclusively transferred to this task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_spawn_i64(state: *mut u8) -> i64 {
    spawn_on(&TASKS, state)
}

pub(super) fn spawn_on(tasks: &TaskTable, state: *mut u8) -> i64 {
    if state.is_null() {
        std::process::abort();
    }
    let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
    let index = guard
        .iter()
        .position(|slot| slot.state.is_none() && slot.generation != 0)
        .unwrap_or(guard.len());
    if index > u32::MAX as usize {
        std::process::abort();
    }
    if index == guard.len() {
        guard.push(TaskSlot {
            generation: 1,
            state: Some(TaskState::Pending(state)),
        });
    }
    let slot = &mut guard[index];
    slot.state = Some(TaskState::Pending(state));
    encode_handle(index as u32, slot.generation)
}

fn encode_handle(index: u32, generation: u32) -> i64 {
    ((generation as i64) << 32) | index as i64
}

fn decode_handle(handle: i64) -> Option<(usize, u32)> {
    if handle < 0 {
        return None;
    }
    Some(((handle as u32) as usize, (handle >> 32) as u32))
}

fn retire(slot: &mut TaskSlot) {
    slot.state = None;
    if slot.generation < 0x7fff_ffff {
        slot.generation += 1;
    } else {
        slot.generation = 0;
    }
}

/// Drive task to completion. Rejoining a completed live handle returns its
/// cached result. Cancellation during the join retires the handle on completion.
///
/// # Safety
/// `handle` must be live. Only one concurrent join may claim it. A cancel racing
/// with this call must run after the join has claimed the task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_join_i64(handle: i64) -> i64 {
    // SAFETY: the public caller provides the live, exclusively joined handle;
    // join_on owns the claimed blob until it has been polled and freed.
    unsafe { join_on(&TASKS, handle, |state| ar_co_block_on_i64(state)) }
}

pub(super) unsafe fn join_on(
    tasks: &TaskTable,
    handle: i64,
    drive: impl FnOnce(*mut u8) -> i64,
) -> i64 {
    let Some((index, generation)) = decode_handle(handle) else {
        std::process::abort();
    };
    let state = {
        let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = guard
            .get_mut(index)
            .filter(|entry| entry.generation == generation && generation != 0)
        else {
            std::process::abort();
        };
        let Some(slot) = entry.state.as_mut() else {
            std::process::abort();
        };
        match slot {
            TaskState::Completed(result) => return *result,
            TaskState::Running { .. } => std::process::abort(),
            TaskState::Pending(state) => {
                let state = *state;
                *slot = TaskState::Running {
                    cancel_requested: false,
                };
                state
            }
        }
    };
    let result = drive(state);
    // SAFETY: the Pending -> Running transition transferred sole ownership to
    // this join; cancel_on cannot access or free the pointer while it is running.
    unsafe { ar_co_free(state) };
    let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = guard
        .get_mut(index)
        .filter(|entry| entry.generation == generation)
    else {
        std::process::abort();
    };
    match entry.state.as_mut() {
        Some(TaskState::Running { cancel_requested }) => {
            if *cancel_requested {
                retire(entry);
            } else {
                entry.state = Some(TaskState::Completed(result));
            }
        }
        _ => std::process::abort(),
    }
    result
}

/// Block on a single coroutine without spawn (alias surface for std.runtime).
///
/// # Safety
/// Same as [`ar_co_block_on_i64`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_block_on_i64(state: *mut u8) -> i64 {
    unsafe { ar_co_block_on_i64(state) }
}

/// Release a handle. If a join owns the blob, request retirement when it finishes.
///
/// # Safety
/// Handle from spawn; not usable after this call. When racing with join, the
/// join must already have claimed the task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_cancel_i64(handle: i64) {
    // SAFETY: the caller relinquishes this live task handle.
    unsafe { cancel_on(&TASKS, handle) };
}

pub(super) unsafe fn cancel_on(tasks: &TaskTable, handle: i64) {
    let Some((index, generation)) = decode_handle(handle) else {
        return;
    };
    let pending = {
        let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = guard
            .get_mut(index)
            .filter(|entry| entry.generation == generation && generation != 0)
        else {
            return;
        };
        match entry.state.take() {
            Some(TaskState::Pending(state)) => {
                retire(entry);
                Some(state)
            }
            Some(TaskState::Running { .. }) => {
                entry.state = Some(TaskState::Running {
                    cancel_requested: true,
                });
                None
            }
            Some(TaskState::Completed(_)) => {
                retire(entry);
                None
            }
            None => None,
        }
    };
    if let Some(state) = pending {
        // SAFETY: removing Pending transfers sole ownership to this cancel;
        // there is no joining thread using that blob.
        unsafe { ar_co_free(state) };
    }
}
