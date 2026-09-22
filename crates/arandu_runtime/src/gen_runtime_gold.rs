//! Type-erased compiler-managed GenRef ABI.
//!
//! Handles are process-local, thread-confined, monotonic non-zero tokens. They
//! are never recycled, so stale values cannot become valid through generation
//! wrap. Payload ownership crosses the ABI by movement, not cloning.

use crate::genref::GenError;
use crate::genref_payload::{OwnedPayload, PayloadDescriptor, PayloadDropGlue, PayloadLayout};
use std::cell::RefCell;
use std::ptr;

#[derive(Default)]
struct CompilerManagedRegistry {
    next: u64,
    // Keep active payloads in insertion order. Destructor order is observable,
    // so a hash table would make shutdown nondeterministic across processes.
    // Removed entries are physically erased to avoid leaking token-indexed
    // metadata; tokens themselves remain monotonic and are never recycled.
    payloads: Vec<(u64, OwnedPayload)>,
}

impl CompilerManagedRegistry {
    fn reserve_handle(&mut self) -> Result<u64, GenError> {
        let handle = self.next.checked_add(1).ok_or(GenError::CapacityOverflow)?;
        self.payloads
            .try_reserve(1)
            .map_err(|_| GenError::AllocationFailed)?;
        self.next = handle;
        Ok(handle)
    }

    fn commit(&mut self, handle: u64, payload: OwnedPayload) {
        debug_assert!(self.payloads.capacity() > self.payloads.len());
        self.payloads.push((handle, payload));
    }

    fn position(&self, handle: u64) -> Option<usize> {
        self.payloads
            .iter()
            .position(|(candidate, _)| *candidate == handle)
    }
}

thread_local! {
    static REGISTRY: RefCell<CompilerManagedRegistry> = RefCell::new(CompilerManagedRegistry::default());
}

unsafe extern "C" fn noop_drop(_: *mut u8) {}

fn descriptor(
    size: usize,
    align: usize,
    drop_glue: Option<PayloadDropGlue>,
) -> Result<PayloadDescriptor, GenError> {
    let layout = PayloadLayout::new(size, align)?;
    // SAFETY: the ABI caller owns the pairing of layout and drop glue. A null
    // glue explicitly means a trivially-droppable payload.
    Ok(unsafe { PayloadDescriptor::from_raw_parts(layout, drop_glue.unwrap_or(noop_drop)) })
}

/// Move a payload into the compiler-managed registry, returning zero on
/// validation/allocation failure.
///
/// # Safety
/// `source` and `drop_glue` must satisfy [`OwnedPayload::try_move_from`]. The
/// source becomes logically uninitialized only when a non-zero handle returns.
///
/// Panics abort through `crate::ffi::guard`: ownership may be mid-transfer,
/// so neither unwinding nor a failure return is sound here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_gen_insert_raw(
    source: *mut u8,
    size: usize,
    align: usize,
    drop_glue: Option<PayloadDropGlue>,
) -> u64 {
    crate::ffi::guard(|| {
        let Ok(descriptor) = descriptor(size, align, drop_glue) else {
            return 0;
        };
        // Reserve every fallible registry resource before consuming the source.
        // A failed return must leave ownership with the ABI caller.
        let Ok(handle) = REGISTRY.with_borrow_mut(CompilerManagedRegistry::reserve_handle) else {
            return 0;
        };
        // SAFETY: forwarded ABI contract; allocation failure leaves source intact.
        let Ok(payload) = (unsafe { OwnedPayload::try_move_from(source, descriptor) }) else {
            return 0;
        };
        REGISTRY.with_borrow_mut(|registry| registry.commit(handle, payload));
        handle
    })
}

/// Copy a borrowed payload into caller storage without transferring ownership.
///
/// # Safety
/// `destination` must be writable and aligned for the supplied layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_gen_get_raw(
    handle: u64,
    destination: *mut u8,
    size: usize,
    align: usize,
) -> bool {
    crate::ffi::guard(|| {
        let Ok(layout) = PayloadLayout::new(size, align) else {
            return false;
        };
        if destination.is_null() || (destination.addr() & (layout.align() - 1)) != 0 {
            return false;
        }
        REGISTRY.with_borrow(|registry| {
            let Some((_, payload)) = registry
                .payloads
                .iter()
                .find(|(candidate, _)| *candidate == handle)
            else {
                return false;
            };
            if payload.descriptor().layout() != layout {
                return false;
            }
            if size > 0 {
                // SAFETY: layout and destination were validated; get borrows the
                // runtime payload and copies into distinct caller storage.
                unsafe { ptr::copy_nonoverlapping(payload.as_ptr(), destination, size) };
            }
            true
        })
    })
}

/// Replace a live payload. The old drop glue runs after registry state is no
/// longer borrowed, allowing destructor reentrancy.
///
/// # Safety
/// Same source/drop contract as [`ar_gen_insert_raw`].
///
/// Panics abort through `crate::ffi::guard`: the source may already be
/// mid-transfer and the replaced payload's drop glue runs here, so neither
/// unwinding nor a `false` return (which would make the caller drop a moved
/// source twice) is sound.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_gen_set_raw(
    handle: u64,
    source: *mut u8,
    size: usize,
    align: usize,
    drop_glue: Option<PayloadDropGlue>,
) -> bool {
    crate::ffi::guard(|| {
        let Ok(descriptor) = descriptor(size, align, drop_glue) else {
            return false;
        };
        // Validate every registry-owned precondition before moving from `source`.
        // The registry is thread-local and no user callback runs between this
        // preflight and the commit, so the matching slot cannot disappear here.
        // This preserves the ABI rule that a false return leaves ownership with
        // the caller.
        let compatible_position = REGISTRY.with_borrow(|registry| {
            registry
                .payloads
                .iter()
                .enumerate()
                .find(|(_, (candidate, _))| *candidate == handle)
                .and_then(|(position, (_, slot))| {
                    (slot.descriptor().layout() == descriptor.layout()).then_some(position)
                })
        });
        let Some(position) = compatible_position else {
            return false;
        };
        // SAFETY: forwarded ABI contract; the new payload is constructed before
        // registry mutation, so failure leaves the old value intact.
        let Ok(payload) = (unsafe { OwnedPayload::try_move_from(source, descriptor) }) else {
            return false;
        };
        let old = REGISTRY.with_borrow_mut(|registry| {
            let slot = &mut registry.payloads[position].1;
            std::mem::replace(slot, payload)
        });
        drop(old);
        true
    })
}

/// Insert for zero, otherwise replace, returning the canonical handle or zero
/// on failure.
///
/// # Safety
/// Same source/drop contract as [`ar_gen_insert_raw`].
///
/// Panics abort through `crate::ffi::guard` (see [`ar_gen_set_raw`]).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_gen_upsert_raw(
    handle: u64,
    source: *mut u8,
    size: usize,
    align: usize,
    drop_glue: Option<PayloadDropGlue>,
) -> u64 {
    if handle == 0 {
        // SAFETY: forwarded unchanged to insert.
        unsafe { ar_gen_insert_raw(source, size, align, drop_glue) }
    } else if unsafe { ar_gen_set_raw(handle, source, size, align, drop_glue) } {
        handle
    } else {
        0
    }
}

/// Move a live payload out, invalidating the handle on success.
///
/// # Safety
/// `destination` must satisfy [`OwnedPayload::try_move_into`].
///
/// Panics abort through `crate::ffi::guard`: the payload is removed from
/// the registry before the move, so a `false` return would both lose it and
/// leave `destination` partially written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_gen_remove_raw(
    handle: u64,
    destination: *mut u8,
    size: usize,
    align: usize,
) -> bool {
    crate::ffi::guard(|| {
        let Ok(layout) = PayloadLayout::new(size, align) else {
            return false;
        };
        if destination.is_null() || (destination.addr() & (layout.align() - 1)) != 0 {
            return false;
        }
        let Some(payload) = REGISTRY.with_borrow_mut(|registry| {
            let position = registry.position(handle)?;
            (registry.payloads[position].1.descriptor().layout() == layout)
                .then(|| registry.payloads.remove(position).1)
        }) else {
            return false;
        };
        // SAFETY: destination/layout were preflighted against the payload before
        // removal; no fallible condition remains.
        unsafe { payload.try_move_into(destination, layout) }.is_ok()
    })
}

/// Drop all payloads owned by this thread's compiler-managed registry.
/// Destructors run after releasing the registry borrow.
///
/// Panics from a payload destructor are caught by `crate::ffi::guard` and
/// abort: the remaining payloads would otherwise leak while the process kept
/// running with a half-drained registry.
#[unsafe(no_mangle)]
pub extern "C" fn ar_gen_shutdown_raw() {
    crate::ffi::guard(|| {
        let payloads = REGISTRY.with_borrow_mut(|registry| std::mem::take(&mut registry.payloads));
        drop(payloads);
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::cell::{Cell, RefCell};
    use std::mem::{ManuallyDrop, MaybeUninit};
    use std::rc::Rc;

    #[derive(Debug)]
    struct Probe(Rc<Cell<usize>>, u64);

    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn generic_abi_moves_sets_removes_and_drops_exactly_once() {
        ar_gen_shutdown_raw();
        let drops = Rc::new(Cell::new(0));
        let mut first = ManuallyDrop::new(Probe(Rc::clone(&drops), 7));
        let descriptor = PayloadDescriptor::for_type::<Probe>();
        let layout = descriptor.layout();
        let handle = unsafe {
            ar_gen_upsert_raw(
                0,
                (&mut *first as *mut Probe).cast(),
                layout.size(),
                layout.align(),
                Some(drop_value_for_test),
            )
        };
        assert_ne!(handle, 0);

        let mut borrowed = MaybeUninit::<Probe>::uninit();
        assert!(unsafe {
            ar_gen_get_raw(
                handle,
                borrowed.as_mut_ptr().cast(),
                layout.size(),
                layout.align(),
            )
        });
        // get is a byte borrow for backend-managed temporaries; do not drop
        // the copied view because runtime retains ownership.
        let borrowed = ManuallyDrop::new(unsafe { borrowed.assume_init() });
        assert_eq!(borrowed.1, 7);

        let mut second = ManuallyDrop::new(Probe(Rc::clone(&drops), 9));
        assert_eq!(
            unsafe {
                ar_gen_upsert_raw(
                    handle,
                    (&mut *second as *mut Probe).cast(),
                    layout.size(),
                    layout.align(),
                    Some(drop_value_for_test),
                )
            },
            handle
        );
        assert_eq!(drops.get(), 1, "set drops the replaced payload once");

        let mut removed = MaybeUninit::<Probe>::uninit();
        assert!(unsafe {
            ar_gen_remove_raw(
                handle,
                removed.as_mut_ptr().cast(),
                layout.size(),
                layout.align(),
            )
        });
        let mut stale_destination = MaybeUninit::<Probe>::uninit();
        assert!(!unsafe {
            ar_gen_get_raw(
                handle,
                stale_destination.as_mut_ptr().cast(),
                layout.size(),
                layout.align(),
            )
        });
        let removed = unsafe { removed.assume_init() };
        assert_eq!(removed.1, 9);
        drop(removed);
        assert_eq!(drops.get(), 2);
        ar_gen_shutdown_raw();
        assert_eq!(drops.get(), 2);
    }

    unsafe extern "C" fn drop_value_for_test(value: *mut u8) {
        // SAFETY: the test pairs this glue only with Probe's exact layout.
        unsafe { ptr::drop_in_place(value.cast::<Probe>()) };
    }

    #[test]
    fn failed_set_does_not_consume_the_callers_payload() {
        ar_gen_shutdown_raw();
        let drops = Rc::new(Cell::new(0));
        let descriptor = PayloadDescriptor::for_type::<Probe>();
        let layout = descriptor.layout();
        let mut source = ManuallyDrop::new(Probe(Rc::clone(&drops), 41));

        assert!(!unsafe {
            ar_gen_set_raw(
                u64::MAX,
                (&mut *source as *mut Probe).cast(),
                layout.size(),
                layout.align(),
                Some(drop_value_for_test),
            )
        });
        assert_eq!(drops.get(), 0, "a failed set must not consume its source");

        // SAFETY: set returned false, so ownership remained in `source`.
        unsafe { ManuallyDrop::drop(&mut source) };
        assert_eq!(drops.get(), 1);
    }

    #[derive(Debug)]
    struct ReentrantProbe(Rc<Cell<usize>>, bool);

    impl Drop for ReentrantProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
            if self.1 {
                ar_gen_shutdown_raw();
            }
        }
    }

    #[test]
    fn set_releases_registry_borrow_before_running_reentrant_drop_glue() {
        ar_gen_shutdown_raw();
        let drops = Rc::new(Cell::new(0));
        let layout = PayloadDescriptor::for_type::<ReentrantProbe>().layout();
        let mut first = ManuallyDrop::new(ReentrantProbe(Rc::clone(&drops), true));
        let handle = unsafe {
            ar_gen_insert_raw(
                (&mut *first as *mut ReentrantProbe).cast(),
                layout.size(),
                layout.align(),
                Some(drop_reentrant_probe),
            )
        };
        let mut second = ManuallyDrop::new(ReentrantProbe(Rc::clone(&drops), false));
        assert!(unsafe {
            ar_gen_set_raw(
                handle,
                (&mut *second as *mut ReentrantProbe).cast(),
                layout.size(),
                layout.align(),
                Some(drop_reentrant_probe),
            )
        });
        assert_eq!(
            drops.get(),
            2,
            "old drop reentrantly drains the new payload"
        );
        let mut stale = MaybeUninit::<ReentrantProbe>::uninit();
        assert!(!unsafe {
            ar_gen_get_raw(
                handle,
                stale.as_mut_ptr().cast(),
                layout.size(),
                layout.align(),
            )
        });
    }

    unsafe extern "C" fn drop_reentrant_probe(value: *mut u8) {
        // SAFETY: the test pairs this glue with ReentrantProbe's exact layout.
        unsafe { ptr::drop_in_place(value.cast::<ReentrantProbe>()) };
    }

    struct OrderedProbe(Rc<RefCell<Vec<u8>>>, u8);

    impl Drop for OrderedProbe {
        fn drop(&mut self) {
            self.0.borrow_mut().push(self.1);
        }
    }

    unsafe extern "C" fn drop_ordered_probe(value: *mut u8) {
        // SAFETY: the test pairs this glue with OrderedProbe's exact layout.
        unsafe { ptr::drop_in_place(value.cast::<OrderedProbe>()) };
    }

    #[test]
    fn shutdown_drops_active_payloads_in_insertion_order() {
        ar_gen_shutdown_raw();
        let order = Rc::new(RefCell::new(Vec::new()));
        let layout = PayloadDescriptor::for_type::<OrderedProbe>().layout();
        for id in [1, 2, 3] {
            let mut payload = ManuallyDrop::new(OrderedProbe(Rc::clone(&order), id));
            assert_ne!(
                unsafe {
                    ar_gen_insert_raw(
                        (&mut *payload as *mut OrderedProbe).cast(),
                        layout.size(),
                        layout.align(),
                        Some(drop_ordered_probe),
                    )
                },
                0
            );
        }
        ar_gen_shutdown_raw();
        assert_eq!(&*order.borrow(), &[1, 2, 3]);
    }
}
