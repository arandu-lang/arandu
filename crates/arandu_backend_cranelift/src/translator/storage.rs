//! Conservative invocation-owned backing for aggregates.
//!
//! This is a storage policy, not a second ownership checker. Typeck proves
//! affine moves; the internal result ABI proves caller-owned returns. Unsupported
//! lifetime boundaries keep their existing backing rather than guessing.

use arandu_middle::SymbolTable;
use arandu_middle::amir::AmirFunc;
use arandu_middle::type_context::CodegenTypeContext;

/// Bound any single promoted scratch slot so oversized aggregates (`[2048]u8`)
/// stay on lazy heap scratch rather than inflating stack frames.
pub(super) const SINGLE_SLOT_PROMOTION_LIMIT: u32 = 1024;

/// Bound cumulative promoted scratch storage per function, including existing
/// explicit slots, so large functions with many small aggregate temporaries keep
/// stack slots instead of spilling to heap scratch.
pub(super) const FRAME_PROMOTION_BUDGET: u32 = 32 * 1024;

pub(super) fn frame_promotion_safe(
    function: &AmirFunc,
    symbols: &SymbolTable,
    info: &dyn CodegenTypeContext,
) -> bool {
    arandu_mir::aggregate_storage::function_scratch_safe(
        function,
        symbols,
        info.type_interner(),
        arandu_mir::aggregate_storage::ScratchResultAbi::CallerOwnedAggregates,
        info.return_borrow_summaries().get(&function.symbol),
    )
}
impl<M: cranelift_module::Module> super::FunctionTranslator<'_, '_, M> {
    /// Same lifetime as an admitted explicit frame slot, but with its payload
    /// in the heap. Allocate lazily so an unentered branch cannot allocate or
    /// fail with OOM. Each static construction/copy site has independent storage.
    pub(super) fn allocate_heap_scratch(&mut self, size: u32) -> cranelift_codegen::ir::Value {
        use cranelift_codegen::ir::{
            BlockArg, InstBuilder, StackSlotData, StackSlotKind, condcodes::IntCC,
        };
        let slot = self.builder.create_sized_stack_slot(StackSlotData {
            kind: StackSlotKind::ExplicitSlot,
            size: self.ptr_type.bytes(),
            align_shift: u8::try_from(self.ptr_type.bytes().trailing_zeros()).unwrap_or(0),
            key: None,
        });
        self.heap_scratch_slots.push(slot);
        let previous = self
            .builder
            .ins()
            .stack_load(self.ptr_type, self.ptr_type, slot, 0);
        let initialized = self.builder.ins().icmp_imm_u(IntCC::NotEqual, previous, 0);
        let allocate = self.builder.create_block();
        let ready = self.builder.create_block();
        let result = self.builder.append_block_param(ready, self.ptr_type);
        self.builder.ins().brif(
            initialized,
            ready,
            &[BlockArg::Value(previous)],
            allocate,
            &[],
        );
        self.builder.switch_to_block(allocate);
        self.builder.seal_block(allocate);
        let pointer = self.call_malloc(size);
        self.builder
            .ins()
            .stack_store(self.ptr_type, pointer, slot, 0);
        self.builder.ins().jump(ready, &[BlockArg::Value(pointer)]);
        self.builder.switch_to_block(ready);
        self.builder.seal_block(ready);
        result
    }

    /// Finish after all construction sites are known. Zero the handles before
    /// the entry instructions, and release them after return values were packed
    /// or copied to the caller. This does not drop user values or their fields.
    pub(super) fn finish_heap_scratch(&mut self) {
        use cranelift_codegen::cursor::{Cursor, FuncCursor};
        use cranelift_codegen::ir::{InstBuilder, Opcode};
        if self.error.is_some() || self.heap_scratch_slots.is_empty() {
            return;
        }
        let Some(entry) = self.builder.func.layout.entry_block() else {
            self.record_ice(
                "missing entry for heap scratch initialization",
                self.func_span(),
            );
            return;
        };
        let Some(first) = self.builder.func.layout.first_inst(entry) else {
            self.record_ice(
                "missing entry instruction for heap scratch initialization",
                self.func_span(),
            );
            return;
        };
        let Some(free) = self.free_func_id() else {
            return;
        };
        let free = self.module.declare_func_in_func(free, self.builder.func);
        let returns: Vec<_> = self
            .builder
            .func
            .layout
            .blocks()
            .filter_map(|block| self.builder.func.layout.last_inst(block))
            .filter(|inst| self.builder.func.dfg.insts[*inst].opcode() == Opcode::Return)
            .collect();
        let mut cursor = FuncCursor::new(self.builder.func);
        cursor.goto_inst(first);
        let null = cursor.ins().iconst(self.ptr_type, 0);
        for &slot in &self.heap_scratch_slots {
            cursor.ins().stack_store(self.ptr_type, null, slot, 0);
        }
        for instruction in returns {
            cursor.goto_inst(instruction);
            for &slot in self.heap_scratch_slots.iter().rev() {
                let pointer = cursor
                    .ins()
                    .stack_load(self.ptr_type, self.ptr_type, slot, 0);
                // The runtime free contract accepts null for unentered sites.
                cursor.ins().call(free, &[pointer]);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::jit::{AranduModule, builder::create_jit_builder};
    use arandu_semantics::{
        OptLevel, lower_to_amir_with_interfaces, lower_to_hir, optimize_amir_checked_with_level,
        resolve_for_test, type_check,
    };
    use std::cell::Cell;

    thread_local! { static ALLOCATIONS: Cell<usize> = const { Cell::new(0) }; }
    thread_local! { static FREES: Cell<usize> = const { Cell::new(0) }; }
    thread_local! { static MEMMOVES: Cell<usize> = const { Cell::new(0) }; }
    thread_local! { static MEMCPYS: Cell<usize> = const { Cell::new(0) }; }

    unsafe extern "C" fn counted_memmove(dest: *mut u8, src: *const u8, len: usize) -> *mut u8 {
        MEMMOVES.with(|count| count.set(count.get() + 1));
        // SAFETY: Preserve libc memmove overlap-safe copy semantics for larger
        // aggregates when falling back to emit_small_memory_copy.
        unsafe {
            std::ptr::copy(src, dest, len);
        }
        dest
    }

    unsafe extern "C" fn counted_memcpy(dest: *mut u8, src: *const u8, len: usize) -> *mut u8 {
        MEMCPYS.with(|count| count.set(count.get() + 1));
        // SAFETY: Preserve libc memcpy copy semantics for larger aggregates
        // when callers fall back to memcpy.
        unsafe {
            std::ptr::copy(src, dest, len);
        }
        dest
    }

    unsafe extern "C" fn counted_malloc(size: usize) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: Preserve the runtime's malloc-compatible allocation contract;
        // the callback only adds test-local accounting on the executing thread.
        unsafe { crate::vec_runtime::ar_rt_raw_malloc(size) }
    }

    fn run_counted(source: &str, level: OptLevel) -> (i32, usize) {
        let (result, allocations, _) = run_counted_lifetimes(source, level);
        (result, allocations)
    }

    unsafe extern "C" fn counted_free(pointer: *mut u8) {
        if !pointer.is_null() {
            FREES.with(|count| count.set(count.get() + 1));
        }
        // SAFETY: This callback preserves the runtime free contract. Compiled
        // code passes either null or a live allocation from counted_malloc.
        unsafe { crate::vec_runtime::ar_rt_raw_free(pointer) }
    }

    fn run_counted_lifetimes(source: &str, level: OptLevel) -> (i32, usize, usize) {
        let ast = arandu_parser::parse(source).unwrap();
        let mut resolution = resolve_for_test(0, &ast);
        std::sync::Arc::make_mut(&mut resolution.symbols).trusted_stdlib = true;
        let mut checked = type_check(
            resolution,
            &ast,
            arandu_semantics::TargetInfo { pointer_width: 64 },
        );
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let hir = lower_to_hir(&mut checked, &ast).unwrap();
        let (mut program, _) = lower_to_amir_with_interfaces(&mut checked, &hir, 8).unwrap();
        optimize_amir_checked_with_level(
            &mut program,
            &checked.symbols,
            &checked.type_info.type_interner,
            level,
        )
        .unwrap();
        let mut builder = create_jit_builder().unwrap();
        builder.symbol("ar_rt_raw_malloc", counted_malloc as *const u8);
        builder.symbol("ar_rt_raw_free", counted_free as *const u8);
        builder.symbol("memmove", counted_memmove as *const u8);
        builder.symbol("memcpy", counted_memcpy as *const u8);
        let compiler = AranduModule {
            module: cranelift_jit::JITModule::new(builder),
            debug: None,
            slot_trap: None,
            code_bytes: 0,
        };
        let module = compiler
            .compile_program(&program, &checked.symbols, &*checked.type_info)
            .unwrap();
        ALLOCATIONS.with(|count| count.set(0));
        FREES.with(|count| count.set(0));
        MEMMOVES.with(|count| count.set(0));
        MEMCPYS.with(|count| count.set(0));
        // SAFETY: The fixture declares exactly main(): int (i32). The module
        // stays alive throughout invocation; the callback matches malloc(size).
        let result = unsafe {
            module
                .get_fn::<unsafe extern "C" fn() -> i32>("main")
                .unwrap()()
        };
        (result, ALLOCATIONS.with(Cell::get), FREES.with(Cell::get))
    }

    /// Source-level shape of the `katu` searcher hot path: slice params, a
    /// bounded loop, and a scalar helper call. Before the descriptor
    /// admission, each `[]u8` copy in those bodies cost a `malloc(16)` per
    /// call — the fixtures pin the shape to zero heap traffic.
    #[cfg(target_pointer_width = "64")]
    mod katu_searcher_shape {
        use super::{OptLevel, run_counted};

        /// Intrinsics are recognized by bare name, so a local declaration of
        /// `strBytes` becomes the `str -> []u8` view at every call site. That
        /// is how these fixtures get slices without the stdlib; the body is
        /// never executed.
        ///
        /// The harness also has no `slice.len`, so bounds travel as scalar
        /// params while the views themselves stay `[]u8`.
        const SLICE_LOOP: &str = r#"
module std.core.fixture_shapes

extern "arandu-intrinsic" {
    func strBytes(source: str): []u8
}
func inner(x: u8): u8 {
    return x + 32
}
func probe(hay: []u8, pos: usize, hlen: usize, needle: []u8, nlen: usize, ic: bool): bool {
    if pos + nlen > hlen { return false }
    let mut k: usize = 0
    while k < nlen {
        let h = hay[pos + k]
        let n = needle[k]
        if ic {
            if inner(h) != inner(n) { return false }
        } else {
            if h != n { return false }
        }
        k = k + 1
    }
    return true
}
func main(): int {
    let hay = strBytes("abcd")
    let needle = strBytes("c")
    let a = probe(hay, 2, 4, needle, 1, false)
    let b = probe(hay, 2, 4, needle, 1, true)
    if !a || !b { return 1 }
    return 0
}
"#;

        /// The same helper, but the loop body has no scalar helper call.
        const SLICE_LOOP_NO_CALL: &str = r#"
module std.core.fixture_shapes

extern "arandu-intrinsic" {
    func strBytes(source: str): []u8
}
func probe(hay: []u8, pos: usize, hlen: usize, needle: []u8, nlen: usize): bool {
    if pos + nlen > hlen { return false }
    let mut k: usize = 0
    while k < nlen {
        if hay[pos + k] != needle[k] { return false }
        k = k + 1
    }
    return true
}
func main(): int {
    let hay = strBytes("abcd")
    let needle = strBytes("c")
    if !probe(hay, 2, 4, needle, 1) { return 1 }
    return 0
}
"#;

        /// One slice param instead of two, to see whether the count scales
        /// with the number of slice parameters.
        const ONE_SLICE_PARAM: &str = r#"
module std.core.fixture_shapes

extern "arandu-intrinsic" {
    func strBytes(source: str): []u8
}
func probe(text: []u8, pos: usize, hlen: usize): bool {
    if pos >= hlen { return false }
    return text[pos] == 97
}
func main(): int {
    let text = strBytes("ab")
    if !probe(text, 0, 2) { return 1 }
    return 0
}
"#;

        /// Scalar params only, with a helper call, to separate "has a call"
        /// from "has slice params".
        const SCALARS_WITH_CALL: &str = r#"
func inner(x: u8): u8 {
    return x + 32
}
func probe(a: u8, b: u8, pos: usize, ic: bool): bool {
    if ic {
        return inner(a) != inner(b)
    }
    return a != b
}
func main(): int {
    let a = probe(1, 2, 0, false)
    let b = probe(1, 2, 0, true)
    return 0
}
"#;

        #[test]
        fn records_baseline_counts_for_every_variant() {
            // Prints the counts so the isolating assertions below can be
            // written against observed values rather than assumptions.
            for (name, src) in [
                ("slice_loop_with_call", SLICE_LOOP),
                ("slice_loop_no_call", SLICE_LOOP_NO_CALL),
                ("one_slice_param", ONE_SLICE_PARAM),
                ("scalars_with_call", SCALARS_WITH_CALL),
            ] {
                let (_, allocs) = run_counted(src, OptLevel::O2);
                println!("katu-shape {name}: allocations={allocs}");
            }
        }

        #[test]
        fn one_slice_param_does_not_allocate() {
            let (result, allocs) = run_counted(ONE_SLICE_PARAM, OptLevel::O2);
            assert_eq!(result, 0);
            assert_eq!(
                allocs, 0,
                "a single slice param must not force heap scratch"
            );
        }

        #[test]
        fn slice_params_without_any_call_do_not_allocate() {
            let (result, allocs) = run_counted(SLICE_LOOP_NO_CALL, OptLevel::O2);
            assert_eq!(result, 0);
            assert_eq!(allocs, 0, "slice params alone must not force heap scratch");
        }

        #[test]
        fn slice_loop_with_scalar_helper_does_not_allocate() {
            let (result, allocs) = run_counted(SLICE_LOOP, OptLevel::O2);
            assert_eq!(result, 0);
            assert_eq!(
                allocs, 0,
                "a slice loop with a scalar helper call must not allocate"
            );
        }

        #[test]
        fn scalar_helper_call_does_not_allocate() {
            let (result, allocs) = run_counted(SCALARS_WITH_CALL, OptLevel::O2);
            assert_eq!(result, 0);
            assert_eq!(
                allocs, 0,
                "a scalar helper call must not force heap scratch"
            );
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn copy_frames_preserve_mutation_loop_and_recursive_return_semantics_without_malloc() {
        let source = r#"
struct Block { words: [4]u64, sum: u64 }
func make(value: u64): Block {
    let mut words: [4]u64 = [0; 4]
    let mut j: usize = 0
    while j < 4 { words[j] = value + j as u64; j += 1 }
    return Block { words: words, sum: value + 3 }
}
func recurse(depth: int, x: [4]u64): [4]u64 {
    if depth == 0 { return x }
    return recurse(depth - 1, x)
}
func main(): int {
    let mut i: u64 = 0
    while i < 100 {
        let original = make(i)
        let mut independent = original
        independent.words[0] = 255
        if original.words[0] != i || original.sum != i + 3 { return 1 }
        let result = recurse(3, original.words)
        if result[0] != i || independent.words[0] != 255 { return 2 }
        i += 1
    }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted(source, level), (0, 0), "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn zero_sized_elements_do_not_store_pointer_bytes_into_aggregate_storage() {
        let source = r#"
struct Empty {}
func consume(value: Empty): void {}
func pair(): (Empty, int) { return Empty {}, 42 }
func main(): int {
    let values: [4]Empty = [Empty {}; 4]
    consume(values[0])
    let empty, result = pair()
    consume(empty)
    return result
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted(source, level), (42, 0), "{level:?}");
        }
    }

    #[test]
    fn malformed_array_initializer_is_rejected_before_emitting_out_of_bounds_stores() {
        use arandu_middle::amir::{AmirRvalue, AmirStmt};
        let ast = arandu_parser::parse("func main(): int { let a: [2]int = [1,2]; return a[0] }")
            .unwrap();
        let mut checked = type_check(
            resolve_for_test(0, &ast),
            &ast,
            arandu_semantics::TargetInfo { pointer_width: 64 },
        );
        assert!(checked.diagnostics.is_empty());
        let hir = lower_to_hir(&mut checked, &ast).unwrap();
        let (mut program, _) = lower_to_amir_with_interfaces(&mut checked, &hir, 8).unwrap();
        let mut changed = false;
        for function in &mut program.funcs {
            for statement in function.stmts.payloads.iter_mut() {
                if let AmirStmt::Assign {
                    rhs: AmirRvalue::Array { items },
                    ..
                } = statement
                {
                    items.push(items[0]);
                    changed = true;
                }
            }
        }
        assert!(changed);
        let compiler = AranduModule {
            module: cranelift_jit::JITModule::new(create_jit_builder().unwrap()),
            debug: None,
            slot_trap: None,
            code_bytes: 0,
        };
        let error = compiler
            .compile_program(&program, &checked.symbols, &*checked.type_info)
            .err()
            .expect("invalid AMIR must fail closed");
        assert!(format!("{error:?}").contains("array initializer length does not match its type"));
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn destructor_arguments_use_partial_aggregate_abi_slots() {
        let source = r#"
struct Tiny { bytes: [3]u8 }
@Destructor
func Tiny.destroy(own self: Tiny): void {}
func main(): int {
    let value = Tiny { bytes: [1,2,3] }
    return value.bytes[2] as int
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted(source, level).0, 3, "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn affine_backing_survives_moves_calls_and_loop_drops_without_heap_storage() {
        let source = r#"
struct Owned { bytes: [16]u8 }
@Destructor
func Owned.destroy(own self: Owned): void {}
func consume(value: own Owned): int { return value.bytes[15] as int }
func make(): Owned { return Owned { bytes: [42; 16] } }
func main(): int {
    let mut i = 0
    while i < 100 {
        let value = make()
        if consume(value) != 42 { return 1 }
        let another = Owned { bytes: [7; 16] }
        if another.bytes[0] != 7 { return 2 }
        i += 1
    }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted_lifetimes(source, level), (0, 0, 0), "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn independent_string_results_allow_bounded_reclaimed_aggregate_scratch() {
        let source = r#"
func label(seed: int): str {
    let mut index = 0
    let mut sum = 0
    while index < 1000 {
        let row: [2048]u8 = [seed as u8; 2048]
        sum = sum + row[2047] as int
        index = index + 1
    }
    if sum > 0 { return "positive" }
    return "zero"
}
func forward(value: str, seed: int): str {
    let mut index = 0
    while index < 1000 {
        let row: [2048]u8 = [seed as u8; 2048]
        if row[2047] != seed as u8 { return "bad" }
        index = index + 1
    }
    return value
}
func main(): int {
    if label(1) == "positive" && label(0) == "zero" && forward("caller", 7) == "caller" { return 0 }
    return 1
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let (result, allocations, frees) = run_counted_lifetimes(source, level);
            assert_eq!(result, 0, "{level:?}");
            assert!(
                allocations < 12,
                "{level:?}: allocations scale with iterations: {allocations}"
            );
            assert_eq!(allocations, frees, "{level:?}: unreclaimed private backing");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn oversized_copy_scratch_is_independent_reused_and_freed() {
        let source = r#"
func main(): int {
    let mut i: int = 0
    while i < 100 {
        let original: [2048]u8 = [7; 2048]
        let mut independent = original
        independent[2047] = 42
        if original[2047] != 7 || independent[2047] != 42 { return 1 }
        i += 1
    }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let (result, allocations, frees) = run_counted_lifetimes(source, level);
            assert_eq!(result, 0, "{level:?}");
            assert!((2..=3).contains(&allocations), "{level:?}: {allocations}");
            assert_eq!(allocations, frees, "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn affine_resources_are_destroyed_once_and_backing_is_not_a_second_owner() {
        let source = r#"
struct Owned { data: ptr[u8], length: uint }
func release(pointer: ptr[u8]): void { unsafe { free(pointer) } }
@Destructor
func Owned.destroy(own self: Owned): void { release(self.data) }
func make(): Owned {
    let data = unsafe { alloc(16 as uint) as ptr[u8] }
    return Owned { data: data, length: 16 }
}
func main(): int {
    let mut i = 0
    while i < 100 {
        let value = make()
        if value.length != 16 { return 1 }
        i += 1
    }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(
                run_counted_lifetimes(source, level),
                (0, 100, 100),
                "{level:?}"
            );
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn tagged_results_have_caller_owned_backing_across_recursive_returns() {
        let source = r#"
func make(depth: int): Option<[64]u8> {
    if depth == 0 { return Option.Some([42; 64]) }
    let child = make(depth - 1)
    return child
}
func main(): int {
    let mut i = 0
    while i < 100 {
        let result = make(4)
        match result {
            Some(bytes) => { if bytes[63] != 42 { return bytes[63] as int + 100 } }
            None => { return 2 }
        }
        i += 1
    }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted_lifetimes(source, level), (0, 0, 0), "{level:?}");
        }
    }
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn heap_scratch_is_lazy_and_cleaned_on_early_and_normal_returns() {
        let source = r#"
func choose(enter: bool, early: bool): int {
    if !enter { return 0 }
    let mut large: [2048]u8 = [7; 2048]
    if early { return large[0] as int }
    large[2047] = 42
    return large[2047] as int
}
func main(): int {
    if choose(false, false) != 0 { return 1 }
    if choose(true, true) != 7 { return 2 }
    if choose(true, false) != 42 { return 3 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let (result, allocations, frees) = run_counted_lifetimes(source, level);
            assert_eq!(result, 0, "{level:?}");
            assert!((2..=4).contains(&allocations), "{level:?}: {allocations}");
            assert_eq!(allocations, frees, "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn unentered_large_constructor_never_allocates() {
        let source = r#"
func choose(enter: bool): int {
    if !enter { return 42 }
    let large: [2048]u8 = [7; 2048]
    return large[0] as int
}
func main(): int { return choose(false) }
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(
                run_counted_lifetimes(source, level),
                (42, 0, 0),
                "{level:?}"
            );
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn oversized_recursive_results_remain_live_after_callee_scratch_is_freed() {
        let source = r#"
func recurse(depth: int, value: [2048]u8): [2048]u8 {
    if depth == 0 { return value }
    return recurse(depth - 1, value)
}
func main(): int {
    let original: [2048]u8 = [7; 2048]
    let mut result = recurse(3, original)
    result[2047] = 42
    if original[2047] != 7 || result[0] != 7 || result[2047] != 42 { return 1 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let (result, allocations, frees) = run_counted_lifetimes(source, level);
            assert_eq!(result, 0, "{level:?}");
            assert!(allocations > 0, "{level:?}");
            assert_eq!(allocations, frees, "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn oversized_aggregate_keeps_heap_fallback_instead_of_exceeding_frame_budget() {
        let source = r#"
func main(): int {
    let mut large: [2048]u8 = [7; 2048]
    large[2047] = 42
    if large[0] != 7 || large[2047] != 42 { return 1 }
    return 0
}
"#;
        let (result, allocations) = run_counted(source, OptLevel::O0);
        assert_eq!(result, 0);
        assert!(allocations > 0);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn coroutine_functions_keep_conservative_payload_storage() {
        let source = r#"
async func answer(): int {
    let values: [4]int = [42, 1, 2, 3]
    return values[0]
}
func main(): int { return await answer() }
"#;
        let (result, allocations) = run_counted(source, OptLevel::O0);
        assert_eq!(result, 42);
        assert!(allocations > 0);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn read_only_initializers_materialize_independent_mutable_values() {
        let source = r#"
func main(): int {
    let mut first: [8]i32 = [1,2,3,4,5,6,7,8]
    let second = first
    first[1] = 99
    if first[1] != 99 || second[1] != 2 || second[7] != 8 { return 1 }
    let mut flags: [256]bool = [false; 256]
    flags[32] = true
    if !flags[32] || flags[31] { return 2 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted(source, level), (0, 0), "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn acyclic_aggregate_branches_and_pointer_loops_promote_without_heap_traffic() {
        let source = r#"
struct Pair { left: [4]int, right: [4]int }
func selectPair(flag: bool, seed: int): Pair {
    let chosen: [4]int = if flag {
        [seed, seed + 1, seed + 2, seed + 3]
    } else {
        [seed + 10, seed + 20, seed + 30, seed + 40]
    }
    let backup: [4]int = [1, 2, 3, 4]
    return Pair { left: chosen, right: backup }
}
func main(): int {
    let a = selectPair(true, 5)
    let b = selectPair(false, 2)
    if a.left[0] != 5 || a.left[3] != 8 || a.right[2] != 3 { return 1 }
    if b.left[0] != 12 || b.left[3] != 42 || b.right[3] != 4 { return 2 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted(source, level), (0, 0), "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn small_aggregate_copies_inline_without_memmove_and_keep_stack_promotion_above_1kb() {
        let source = r#"
module std.core.fixture_small_copy

struct SmallValue { a: u64, b: u64, c: u64, d: u64, e: u64 }
func bump(x: u64): SmallValue {
    return SmallValue { a: x, b: x + 1, c: x + 2, d: x + 3, e: x + 4 }
}
func copyStep(src: ptr[SmallValue], dst: ptr[SmallValue]): void {
    unsafe {
        let v = ptrRead<SmallValue>(src)
        ptrWrite<SmallValue>(dst, v)
    }
}
extern "arandu-intrinsic" {
    func ptrRead<T>(pointer: ptr[T]): T
    func ptrWrite<T>(pointer: ptr[T], value: T): void
    func addressOf<T>(value: mut ref T): ptr[T]
}
func manyTemporaries(seed: u64): u64 {
    let mut i: u64 = 0
    let mut acc: u64 = 0
    while i < 40 {
        let mut s = bump(seed + i)
        let p = unsafe { addressOf<SmallValue>(mut ref s) }
        let mut out = SmallValue { a: 0, b: 0, c: 0, d: 0, e: 0 }
        let q = unsafe { addressOf<SmallValue>(mut ref out) }
        copyStep(p, q)
        acc = acc + out.e
        let b0 = [seed + i; 8]
        let b1 = [seed + i + 1; 8]
        let b2 = [seed + i + 2; 8]
        let b3 = [seed + i + 3; 8]
        let b4 = [seed + i + 4; 8]
        let b5 = [seed + i + 5; 8]
        let b6 = [seed + i + 6; 8]
        let b7 = [seed + i + 7; 8]
        let b8 = [seed + i + 8; 8]
        let b9 = [seed + i + 9; 8]
        let b10 = [seed + i + 10; 8]
        let b11 = [seed + i + 11; 8]
        let b12 = [seed + i + 12; 8]
        let b13 = [seed + i + 13; 8]
        let b14 = [seed + i + 14; 8]
        let b15 = [seed + i + 15; 8]
        let b16 = [seed + i + 16; 8]
        let b17 = [seed + i + 17; 8]
        let b18 = [seed + i + 18; 8]
        let b19 = [seed + i + 19; 8]
        acc = acc + b0[0] + b1[1] + b2[2] + b3[3] + b4[4] + b5[5] + b6[6] + b7[7] + b8[0] + b9[1] + b10[2] + b11[3] + b12[4] + b13[5] + b14[6] + b15[7] + b16[0] + b17[1] + b18[2] + b19[3]
        i += 1
    }
    return acc
}
func main(): int {
    let sum = manyTemporaries(1)
    if sum == 0 { return 1 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let (result, allocs, frees) = run_counted_lifetimes(source, level);
            assert_eq!(result, 0, "{level:?}");
            assert_eq!((allocs, frees), (0, 0), "{level:?}");
            assert_eq!(MEMMOVES.with(Cell::get), 0, "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn mut_slice_indexed_stores_survive_o2_dce_when_slice_is_not_read_afterward() {
        let source = r#"
module std.core.fixture_mut_slice_dce

extern "arandu-intrinsic" {
    func mutSliceFromRaw<Owner, Element>(owner: mut ref Owner, data: ptr[Element], len: usize): mut []Element
    func addressOf<T>(value: mut ref T): ptr[T]
}
struct FixedBuf { a: int, b: int, c: int, d: int }
func FixedBuf.asMutSlice(self: mut ref FixedBuf): mut []int {
    unsafe {
        let base = addressOf<int>(mut ref self.a)
        return mutSliceFromRaw<FixedBuf, int>(self, base, 4 as usize)
    }
}
func overwriteThroughUnreadSlice(buf: mut ref FixedBuf, idx: usize, val: int): void {
    let mut sl = buf.asMutSlice()
    sl[idx] = val
    sl[0] = val + 10
}
func main(): int {
    let mut buf = FixedBuf { a: 1, b: 2, c: 3, d: 4 }
    overwriteThroughUnreadSlice(mut ref buf, 2 as usize, 77)
    if buf.a != 87 || buf.c != 77 { return 1 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            assert_eq!(run_counted(source, level), (0, 0), "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn acyclic_aggregate_join_inside_loop_promotes_without_heap_traffic() {
        let source = r#"
module std.core.fixture_loop_join

extern "arandu-intrinsic" {
    func sliceFromRaw<Owner, Element>(owner: ref Owner, data: ptr[Element], len: usize): []Element
    func mutSliceFromRaw<Owner, Element>(owner: mut ref Owner, data: ptr[Element], len: usize): mut []Element
    func addressOf<T>(value: mut ref T): ptr[T]
    func addressOfShared<T>(value: ref T): ptr[T]
}

enum Tag { Empty, Num, Obj }
struct Handle { gen: u32, idx: u32 }
struct Val { tag: Tag, i: i64, f: f64, h: Handle }
struct Buf { a: Val, b: Val }

func makeNum(n: i64): Val {
    return Val {
        tag: Tag.Num,
        i: n,
        f: 0.0,
        h: Handle { gen: 0 as u32, idx: 0 as u32 },
    }
}

func makeObj(gen: u32, idx: u32): Val {
    return Val {
        tag: Tag.Obj,
        i: 0,
        f: 1.5,
        h: Handle { gen: gen, idx: idx },
    }
}

func Buf.asSlice(self: ref Buf): []Val {
    unsafe {
        let base = addressOfShared<Val>(ref self.a)
        return sliceFromRaw<Buf, Val>(self, base, 2 as usize)
    }
}

func Buf.asMutSlice(self: mut ref Buf): mut []Val {
    unsafe {
        let base = addressOf<Val>(mut ref self.a)
        return mutSliceFromRaw<Buf, Val>(self, base, 2 as usize)
    }
}

func runLoopShape(buf: mut ref Buf, limit: int): i64 {
    let mut sum: i64 = 0
    let mut step: int = 0
    while step < limit {
        let produced = if (step % 2) == 0 {
            makeNum(step as i64)
        } else {
            makeObj(1 as u32, step as u32)
        }
        let mut chosen = makeNum(10)
        if step > 2 {
            chosen = produced
        }
        let mut msl = buf.asMutSlice()
        msl[(step as usize) % 2] = chosen
        let sl = buf.asSlice()
        let readBack = sl[(step as usize) % 2]
        sum = sum + readBack.i + (readBack.h.idx as i64)
        step += 1
    }
    return sum
}

func main(): int {
    let empty = Val {
        tag: Tag.Empty,
        i: 0,
        f: 0.0,
        h: Handle { gen: 0 as u32, idx: 0 as u32 },
    }
    let mut buf = Buf { a: empty, b: empty }
    // steps 0..2 contribute 10 each (30); step 3: 3, step 4: 4, step 5: 5 -> total 42
    let total = runLoopShape(mut ref buf, 6)
    if total != 42 { return 1 }
    if buf.b.h.idx != (5 as u32) { return 2 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let (result, allocs, frees) = run_counted_lifetimes(source, level);
            assert_eq!(result, 0, "{level:?}");
            assert_eq!((allocs, frees), (0, 0), "{level:?}");
            assert_eq!(MEMMOVES.with(Cell::get), 0, "{level:?}");
            assert_eq!(MEMCPYS.with(Cell::get), 0, "{level:?}");
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn loop_carried_aggregate_preserves_previous_iteration_value() {
        let source = r#"
enum Tag { Num, Obj }
struct Handle { gen: u32, idx: u32 }
struct Val { tag: Tag, n: int, h: Handle }

func makeVal(n: int): Val {
    return Val {
        tag: Tag.Num,
        n: n,
        h: Handle { gen: 1 as u32, idx: n as u32 },
    }
}

func main(): int {
    let mut prev = makeVal(0)
    let mut i: int = 1
    while i < 10 {
        let cur = makeVal(i)
        if prev.n != i - 1 || prev.h.idx != ((i - 1) as u32) { return 1 }
        prev = cur
        i += 1
    }
    if prev.n != 9 { return 2 }
    return 0
}
"#;
        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let (result, _, _) = run_counted_lifetimes(source, level);
            assert_eq!(result, 0, "{level:?}");
        }
    }
}
