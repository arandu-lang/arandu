//! Unit tests for the Dual-Locus `TaskArena` allocator (RFC 0043).

use arandu_runtime::task_arena::{
    DEFAULT_CHUNK_CAPACITY, TaskArena, ar_task_arena_alloc, ar_task_arena_checkpoint,
    ar_task_arena_reset, ar_task_arena_rewind,
};

#[test]
fn task_arena_aligns_and_preserves_values_across_chunk_growth() {
    let mut arena = TaskArena::new();

    for align in [1usize, 2, 4, 8, 16, 32, 64] {
        let ptr = arena.alloc(7, align);
        assert!(!ptr.is_null());
        assert_eq!(
            (ptr as usize) % align,
            0,
            "pointer {ptr:p} not aligned to {align}"
        );
        unsafe {
            std::ptr::write_bytes(ptr, 0xAB, 7);
        }
    }

    // Force allocation of multiple 64 KiB slabs and verify earlier pointers remain valid.
    let first_i64 = arena.alloc(8, 8).cast::<i64>();
    unsafe {
        first_i64.write(0x1122_3344_5566_7788);
    }

    let mut pointers = Vec::new();
    for idx in 0..300i64 {
        let slot = arena.alloc(512, 16).cast::<i64>();
        assert_eq!((slot as usize) % 16, 0);
        unsafe {
            slot.write(idx * 17 + 3);
        }
        pointers.push((idx, slot));
    }

    assert!(
        arena.chunk_count() >= 2,
        "expected multiple slabs after >150 KiB of allocations"
    );
    assert_eq!(unsafe { first_i64.read() }, 0x1122_3344_5566_7788);

    for (idx, slot) in pointers {
        assert_eq!(unsafe { slot.read() }, idx * 17 + 3);
    }
}

#[test]
fn task_arena_checkpoint_and_rewind_reuse_memory_in_tight_loop() {
    let mut arena = TaskArena::new();

    // Pre-loop allocation that must survive loop rewinds.
    let outer = arena.alloc(8, 8).cast::<i64>();
    unsafe {
        outer.write(999);
    }
    let mark = arena.checkpoint();
    let initial_cursor = arena.current_cursor();

    let mut first_iter_ptr = std::ptr::null_mut();
    for iter in 0..10_000i64 {
        let mark_iter = arena.checkpoint();
        let slot = arena.alloc(64, 8).cast::<i64>();
        if iter == 0 {
            first_iter_ptr = slot;
        } else {
            assert_eq!(
                slot, first_iter_ptr,
                "rewind should recycle the exact slab offset on iteration {iter}"
            );
        }
        unsafe {
            slot.write(iter);
            assert_eq!(slot.read(), iter);
        }
        arena.rewind(mark_iter);
    }

    assert_eq!(arena.chunk_count(), 1);
    assert_eq!(arena.checkpoint(), mark);
    assert_eq!(arena.current_cursor(), initial_cursor);
    assert_eq!(unsafe { outer.read() }, 999);
}

#[test]
fn task_arena_extern_c_entrypoints_round_trip() {
    ar_task_arena_reset();
    let base_mark = ar_task_arena_checkpoint();
    assert_eq!(base_mark, 0);

    let p1 = ar_task_arena_alloc(16, 8).cast::<i64>();
    assert!(!p1.is_null());
    unsafe {
        p1.write(42);
        p1.add(1).write(84);
    }

    let loop_mark = ar_task_arena_checkpoint();
    let p_temp = ar_task_arena_alloc(DEFAULT_CHUNK_CAPACITY / 2, 16);
    assert!(!p_temp.is_null());
    ar_task_arena_rewind(loop_mark);

    let p_reused = ar_task_arena_alloc(32, 16);
    assert_eq!(p_temp, p_reused);
    assert_eq!(unsafe { p1.read() }, 42);
    assert_eq!(unsafe { p1.add(1).read() }, 84);

    ar_task_arena_reset();
    assert_eq!(ar_task_arena_checkpoint(), 0);
}
