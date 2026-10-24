//! Conservative invocation-owned backing for aggregates.
//!
//! This is a storage policy, not a second ownership checker. Typeck proves
//! affine moves; the internal result ABI proves caller-owned returns. Unsupported
//! lifetime boundaries keep their existing backing rather than guessing.

use arandu_middle::SymbolTable;
use arandu_middle::amir::AmirFunc;
use arandu_middle::type_context::CodegenTypeContext;

/// Bound promoted scratch storage, including existing explicit slots. This
/// does not include register-allocation spills or claim to implement coloring.
pub(super) const FRAME_PROMOTION_BUDGET: u32 = 1024;

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
        let mut checked = type_check(
            resolve_for_test(0, &ast),
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
        let compiler = AranduModule {
            module: cranelift_jit::JITModule::new(builder),
            debug: None,
        };
        let module = compiler
            .compile_program(&program, &checked.symbols, &*checked.type_info)
            .unwrap();
        ALLOCATIONS.with(|count| count.set(0));
        FREES.with(|count| count.set(0));
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
}
