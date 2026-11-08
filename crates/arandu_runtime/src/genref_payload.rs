//! Type-erased owned payloads for the GenRef Gold host runtime.
//!
//! The compiler remains responsible for target [`DataLayout`](https://docs.rs/).
//! This module validates the concrete descriptor supplied to the host runtime
//! and owns allocation, movement, and exactly-once drop.

use crate::genref::GenError;
use std::alloc::{Layout, alloc, dealloc};
use std::any::TypeId;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::{self, NonNull};

/// C-compatible destructor for one initialized payload in caller-provided
/// storage. It must not deallocate the storage or unwind across the ABI.
pub type PayloadDropGlue = unsafe extern "C" fn(*mut u8);

/// Checked size/alignment contract for one promoted payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PayloadLayout {
    size: usize,
    align: usize,
}

impl PayloadLayout {
    pub fn new(size: usize, align: usize) -> Result<Self, GenError> {
        Layout::from_size_align(size, align).map_err(|_| GenError::InvalidLayout)?;
        if size > isize::MAX.unsigned_abs() {
            return Err(GenError::InvalidLayout);
        }
        Ok(Self { size, align })
    }

    #[must_use]
    pub const fn size(self) -> usize {
        self.size
    }

    #[must_use]
    pub const fn align(self) -> usize {
        self.align
    }

    /// Returns the allocation size rounded up to the host page boundary when
    /// the requested size exceeds a page, optimizing large arena slabs on
    /// targets with 16 KB pages (e.g. macOS Apple Silicon).
    #[must_use]
    pub fn page_aligned_size(self) -> usize {
        let page = crate::os_runtime::HOST_PAGE_SIZE;
        if self.size >= page {
            (self.size + page - 1) & !(page - 1)
        } else {
            self.size
        }
    }

    fn allocation_layout(self) -> Result<Layout, GenError> {
        Layout::from_size_align(self.size, self.align).map_err(|_| GenError::InvalidLayout)
    }
}

/// Drop glue plus physical layout for a type-erased payload.
#[derive(Clone, Copy)]
pub struct PayloadDescriptor {
    layout: PayloadLayout,
    drop_glue: PayloadDropGlue,
    type_id: Option<TypeId>,
}

impl std::fmt::Debug for PayloadDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PayloadDescriptor")
            .field("layout", &self.layout)
            .field("typed", &self.type_id.is_some())
            .finish_non_exhaustive()
    }
}

impl PayloadDescriptor {
    #[must_use]
    pub fn for_type<T: 'static>() -> Self {
        Self {
            layout: PayloadLayout {
                size: std::mem::size_of::<T>(),
                align: std::mem::align_of::<T>(),
            },
            drop_glue: drop_value::<T>,
            type_id: Some(TypeId::of::<T>()),
        }
    }

    /// Construct a descriptor supplied by compiler-generated ABI code.
    ///
    /// # Safety
    ///
    /// `drop_glue` must accept a live value with exactly `layout`, must drop it
    /// once without deallocating its storage, and must not unwind across an FFI
    /// boundary when called by an ABI adapter.
    pub unsafe fn from_raw_parts(layout: PayloadLayout, drop_glue: PayloadDropGlue) -> Self {
        Self {
            layout,
            drop_glue,
            type_id: None,
        }
    }

    #[must_use]
    pub const fn layout(self) -> PayloadLayout {
        self.layout
    }
}

/// One allocated, initialized payload with exactly-once destruction.
pub struct OwnedPayload {
    ptr: NonNull<u8>,
    descriptor: PayloadDescriptor,
    marker: PhantomData<*mut u8>,
}

impl std::fmt::Debug for OwnedPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedPayload")
            .field("layout", &self.descriptor.layout)
            .finish_non_exhaustive()
    }
}

impl OwnedPayload {
    pub fn try_new<T: 'static>(value: T) -> Result<Self, GenError> {
        let descriptor = PayloadDescriptor::for_type::<T>();
        let ptr = allocate(descriptor.layout, NonNull::<T>::dangling().cast())?;
        // SAFETY: `ptr` is non-null, aligned for T, and points to a fresh
        // allocation large enough for T (or a valid aligned ZST sentinel).
        unsafe { ptr::write(ptr.cast::<T>().as_ptr(), value) };
        Ok(Self {
            ptr,
            descriptor,
            marker: PhantomData,
        })
    }

    /// Move an initialized value from caller-owned storage into runtime-owned
    /// storage. The source becomes logically uninitialized on success.
    ///
    /// # Safety
    ///
    /// `source` must be non-null and aligned for `descriptor`, point to a live
    /// value described by it, and remain readable for `layout.size()` bytes.
    /// The caller must not read or drop the source after this function returns
    /// `Ok`.
    pub unsafe fn try_move_from(
        source: *mut u8,
        descriptor: PayloadDescriptor,
    ) -> Result<Self, GenError> {
        if source.is_null() || (source.addr() & (descriptor.layout.align - 1)) != 0 {
            return Err(GenError::InvalidPayloadPointer);
        }
        let zst = NonNull::new(source).ok_or(GenError::InvalidPayloadPointer)?;
        let destination = allocate(descriptor.layout, zst)?;
        if descriptor.layout.size > 0 {
            // SAFETY: guaranteed by the caller; destination is a fresh,
            // non-overlapping allocation of the validated size.
            unsafe {
                ptr::copy_nonoverlapping(
                    source.cast_const(),
                    destination.as_ptr(),
                    descriptor.layout.size,
                )
            };
        }
        Ok(Self {
            ptr: destination,
            descriptor,
            marker: PhantomData,
        })
    }

    #[must_use]
    pub const fn descriptor(&self) -> PayloadDescriptor {
        self.descriptor
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr().cast_const()
    }

    #[must_use]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        (self.descriptor.type_id == Some(TypeId::of::<T>())).then(|| {
            // SAFETY: a matching TypeId is only installed by `try_new<T>` or
            // `PayloadDescriptor::for_type<T>` under the unsafe move contract.
            unsafe { self.ptr.cast::<T>().as_ref() }
        })
    }

    pub fn try_take<T: 'static>(self) -> Result<T, Self> {
        if self.descriptor.type_id != Some(TypeId::of::<T>()) {
            return Err(self);
        }
        let this = ManuallyDrop::new(self);
        // SAFETY: TypeId matches the initialized T and ManuallyDrop prevents
        // its drop glue from running after ownership is moved out.
        let value = unsafe { ptr::read(this.ptr.cast::<T>().as_ptr()) };
        deallocate(this.ptr, this.descriptor.layout);
        Ok(value)
    }

    /// Move the erased payload into caller-owned storage without running drop
    /// glue. The destination becomes the unique owner on success.
    ///
    /// # Safety
    ///
    /// `destination` must be non-null, aligned for this payload, writable for
    /// `layout.size()` bytes, and contain no initialized value that requires
    /// dropping. The caller must later run the descriptor's drop glue exactly
    /// once for the moved value.
    pub unsafe fn try_move_into(
        self,
        destination: *mut u8,
        expected_layout: PayloadLayout,
    ) -> Result<(), Self> {
        if self.descriptor.layout != expected_layout
            || destination.is_null()
            || (destination.addr() & (expected_layout.align - 1)) != 0
        {
            return Err(self);
        }
        let this = ManuallyDrop::new(self);
        if expected_layout.size > 0 {
            // SAFETY: guaranteed by the caller and checked above; source and
            // destination are distinct allocations of the exact same layout.
            unsafe {
                ptr::copy_nonoverlapping(
                    this.ptr.as_ptr().cast_const(),
                    destination,
                    expected_layout.size,
                )
            };
        }
        deallocate(this.ptr, expected_layout);
        Ok(())
    }

    /// Release the allocation after another owner consumed the value in place.
    ///
    /// # Safety
    /// The value at [`Self::as_mut_ptr`] must have been moved out or destroyed
    /// exactly once and must no longer be initialized.
    pub(crate) unsafe fn release_consumed(self) {
        let this = ManuallyDrop::new(self);
        deallocate(this.ptr, this.descriptor.layout);
    }
}

impl Drop for OwnedPayload {
    fn drop(&mut self) {
        // SAFETY: OwnedPayload always contains exactly one initialized value;
        // this is its unique destruction path.
        unsafe { (self.descriptor.drop_glue)(self.ptr.as_ptr()) };
        deallocate(self.ptr, self.descriptor.layout);
    }
}

fn allocate(layout: PayloadLayout, zst: NonNull<u8>) -> Result<NonNull<u8>, GenError> {
    if layout.size == 0 {
        return Ok(zst);
    }
    let allocation_layout = layout.allocation_layout()?;
    // SAFETY: allocation_layout is validated and has non-zero size.
    let allocated = unsafe { alloc(allocation_layout) };
    NonNull::new(allocated).ok_or(GenError::AllocationFailed)
}

fn deallocate(ptr: NonNull<u8>, layout: PayloadLayout) {
    if layout.size == 0 {
        return;
    }
    let Ok(allocation_layout) = layout.allocation_layout() else {
        // PayloadLayout fields are private and validated at construction. If
        // this fails, runtime state is corrupt and leaking onward is unsafe.
        std::process::abort();
    };
    // SAFETY: ptr was allocated by `allocate` using this exact layout and has
    // not been deallocated yet.
    unsafe { dealloc(ptr.as_ptr(), allocation_layout) };
}

unsafe extern "C" fn drop_value<T>(value: *mut u8) {
    // SAFETY: PayloadDescriptor::for_type<T> pairs this glue with T's exact
    // layout, and OwnedPayload calls it once while the value is initialized.
    unsafe { ptr::drop_in_place(value.cast::<T>()) };
}

/// Allocated but uninitialized storage for one erased payload.
pub(crate) struct UninitPayload {
    ptr: NonNull<u8>,
    descriptor: PayloadDescriptor,
}

impl UninitPayload {
    pub(crate) fn try_new(descriptor: PayloadDescriptor) -> Result<Self, GenError> {
        let aligned_zst = NonNull::new(ptr::without_provenance_mut(descriptor.layout.align))
            .ok_or(GenError::InvalidLayout)?;
        let ptr = allocate(descriptor.layout, aligned_zst)?;
        Ok(Self { ptr, descriptor })
    }

    #[must_use]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Promote storage after an ABI producer initialized its value.
    ///
    /// # Safety
    /// The buffer must contain exactly one live value described by the stored
    /// descriptor.
    pub(crate) unsafe fn assume_init(self) -> OwnedPayload {
        let this = ManuallyDrop::new(self);
        OwnedPayload {
            ptr: this.ptr,
            descriptor: this.descriptor,
            marker: PhantomData,
        }
    }
}

impl Drop for UninitPayload {
    fn drop(&mut self) {
        deallocate(self.ptr, self.descriptor.layout);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::genref::ArenaRegistry;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn layout_rejects_invalid_alignment_and_overflow() {
        assert_eq!(PayloadLayout::new(1, 0), Err(GenError::InvalidLayout));
        assert_eq!(PayloadLayout::new(1, 3), Err(GenError::InvalidLayout));
        assert_eq!(
            PayloadLayout::new(usize::MAX, 1),
            Err(GenError::InvalidLayout)
        );
    }

    #[test]
    fn zst_and_high_alignment_are_supported() {
        #[derive(Debug, PartialEq, Eq)]
        struct Zst;
        #[repr(align(64))]
        #[derive(Debug, PartialEq, Eq)]
        struct Aligned(u8);

        let zst = OwnedPayload::try_new(Zst).unwrap();
        assert_eq!(zst.descriptor().layout().size(), 0);
        assert_eq!(zst.downcast_ref::<Zst>(), Some(&Zst));

        let aligned = OwnedPayload::try_new(Aligned(7)).unwrap();
        assert_eq!(aligned.descriptor().layout().align(), 64);
        assert_eq!(aligned.as_ptr().addr() % 64, 0);
        assert_eq!(aligned.downcast_ref::<Aligned>(), Some(&Aligned(7)));
    }

    #[test]
    fn string_enum_move_and_wrong_downcast_preserve_ownership() {
        #[derive(Debug, PartialEq, Eq)]
        enum Payload {
            Text(String),
        }

        let payload = OwnedPayload::try_new(Payload::Text("arandu".into())).unwrap();
        assert!(payload.downcast_ref::<String>().is_none());
        let payload = payload.try_take::<String>().unwrap_err();
        assert_eq!(
            payload.try_take::<Payload>().unwrap(),
            Payload::Text("arandu".into())
        );
    }

    #[test]
    fn arena_remove_and_destroy_run_erased_drop_exactly_once() {
        #[derive(Debug)]
        struct Probe(Rc<Cell<usize>>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let drops = Rc::new(Cell::new(0));
        let mut registry = ArenaRegistry::<OwnedPayload>::new();
        let arena = registry.create_arena().unwrap();
        let removed = registry
            .insert(
                arena,
                OwnedPayload::try_new(Probe(Rc::clone(&drops))).unwrap(),
            )
            .unwrap();
        let _live = registry
            .insert(
                arena,
                OwnedPayload::try_new(Probe(Rc::clone(&drops))).unwrap(),
            )
            .unwrap();

        drop(registry.remove(arena, removed).unwrap());
        assert_eq!(drops.get(), 1);
        registry.destroy_arena(arena).unwrap();
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn raw_move_transfers_ownership_without_double_drop() {
        #[derive(Debug)]
        struct Probe(Rc<Cell<usize>>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let drops = Rc::new(Cell::new(0));
        let mut source = ManuallyDrop::new(Probe(Rc::clone(&drops)));
        let source_ptr = (&mut *source as *mut Probe).cast::<u8>();
        // SAFETY: source_ptr names one initialized Probe with the descriptor's
        // exact layout, and ManuallyDrop prevents the moved source from dropping.
        let payload = unsafe {
            OwnedPayload::try_move_from(source_ptr, PayloadDescriptor::for_type::<Probe>())
        }
        .unwrap();
        assert_eq!(drops.get(), 0);
        drop(payload);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn erased_move_out_transfers_drop_obligation_to_destination() {
        #[derive(Debug)]
        struct Probe(Rc<Cell<usize>>, u32);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let drops = Rc::new(Cell::new(0));
        let payload = OwnedPayload::try_new(Probe(Rc::clone(&drops), 17)).unwrap();
        let layout = payload.descriptor().layout();
        let mut destination = std::mem::MaybeUninit::<Probe>::uninit();
        // SAFETY: destination is aligned, writable, and uninitialized for the
        // payload's exact Probe layout. It becomes the unique owner.
        unsafe {
            payload
                .try_move_into(destination.as_mut_ptr().cast::<u8>(), layout)
                .unwrap();
        }
        assert_eq!(drops.get(), 0);
        // SAFETY: try_move_into initialized destination with one Probe.
        let moved = unsafe { destination.assume_init() };
        assert_eq!(moved.1, 17);
        drop(moved);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn raw_move_rejects_misaligned_source_before_reading_it() {
        #[repr(align(8))]
        struct Buffer([u8; 16]);

        let mut buffer = Buffer([0; 16]);
        // SAFETY: this intentionally invalid pointer is never dereferenced;
        // the operation must reject it during preflight alignment validation.
        let result = unsafe {
            OwnedPayload::try_move_from(
                buffer.0.as_mut_ptr().wrapping_add(1),
                PayloadDescriptor::for_type::<u64>(),
            )
        };
        assert_eq!(result.unwrap_err(), GenError::InvalidPayloadPointer);
    }

    #[test]
    fn page_aligned_size_calibrates_to_host_page() {
        let page = crate::os_runtime::HOST_PAGE_SIZE;
        let small = PayloadLayout::new(128, 8).unwrap();
        assert_eq!(small.page_aligned_size(), 128);

        let exact = PayloadLayout::new(page, 8).unwrap();
        assert_eq!(exact.page_aligned_size(), page);

        let large = PayloadLayout::new(page + 1, 8).unwrap();
        assert_eq!(large.page_aligned_size(), page * 2);
    }
}
