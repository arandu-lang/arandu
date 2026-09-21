//! End-to-end tests for the surface language: Arandu source → AMIR → wasm
//! bytes → real execution inside the wasmtime runtime. These are the first
//! tests that *run* emitted modules instead of only checking well-formedness.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

mod common;

use arandu_middle::amir::block::{AmirBasicBlock, BlockId};
use arandu_middle::amir::local::TempId;
use arandu_middle::amir::program::AmirFunc;
use arandu_middle::amir::stmt::{AmirStmt, AmirStmtTable, AmirTerminator};
use arandu_middle::amir::value::{AmirConstant, AmirOperand, AmirRvalue};
use arandu_middle::cfg::compute_cfg_edges;
use arandu_middle::layout::DenseRange;
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use common::{
    compile_source, emit_one, find_func_export, foreign_sym, run_main_i32, run_main_traps, temp,
};

#[test]
fn surface_arithmetic_returns_5() {
    let bytes = compile_source(
        r#"
func main(): int {
    return 2 + 3
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 5);
}

#[test]
fn surface_if_else_branch_executes() {
    let bytes = compile_source(
        r#"
func main(): int {
    let x = 7
    let mut res = 0
    if x > 5 {
        res = 10
    } else {
        res = 20
    }
    return res
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 10);
}

#[test]
fn surface_while_loop_sums_0_through_4() {
    // Exercises SSA values flowing across loop edges (block parameters).
    let bytes = compile_source(
        r#"
func main(): int {
    let mut sum = 0
    let mut i = 0
    while i < 5 {
        sum = sum + i
        i = i + 1
    }
    return sum
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 10);
}

#[test]
fn surface_function_call_returns_42() {
    // Exercises call-argument typing (per-operand arity, not `int`-for-all).
    let bytes = compile_source(
        r#"
func add(a: int, b: int): int {
    return a + b
}
func main(): int {
    return add(20, 22)
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn surface_mut_ref_slice_preserves_data_and_length_words() {
    let bytes = compile_source(
        r#"
module std.core.mut_slice_wasm

extern "arandu-intrinsic" {
    func sliceFromRaw(owner: ptr[u8], data: ptr[u8], len: uint): []u8
    func sliceLen<T>(source: []T): uint
}

func fill(buf: mut ref []u8): uint {
    let len = unsafe { sliceLen<u8>(*buf) }
    if len != 4 { return 99 }
    buf[1] = 42 as u8
    return len
}

func observedLen(buf: ref []u8): uint {
    return unsafe { sliceLen<u8>(*buf) }
}

func main(): int {
    let raw = alloc(4) as ptr[u8]
    let mut view = unsafe { sliceFromRaw(raw, raw, 4 as uint) }
    let len = fill(mut ref view)
    if len != 4 || observedLen(ref view) != 4 || view[1] != (42 as u8) {
        unsafe { free(raw) }
        return 1
    }
    unsafe { free(raw) }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[test]
fn surface_struct_field_access_and_mutation() {
    // Exercises StructLiteral (heap cell), empty-projection Load/Store value
    // copies, projected field Store via a real cell address and field loads.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func main(): int {
    let p: Point = Point { x: 10, y: 20 }
    p.x = 30
    p.y = 40
    return p.x + p.y
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 70);
}

#[test]
fn surface_array_index_read() {
    // Exercises ArrayLiteral (heap cell with per-element stores) and an Index
    // projection with a bounds check on a fixed-length array.
    let bytes = compile_source(
        r#"
func main(): int {
    let arr = [10, 20, 30]
    return arr[1]
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 20);
}

#[test]
fn surface_enum_discriminant_and_payload() {
    // Exercises EnumConstruct (tag + payload cell), Discriminant and the
    // layout-driven EnumPayload load inside an `if is` pattern.
    let bytes = compile_source(
        r#"
enum MyResult {
    MyOk(int),
    MyError(str),
}

func main(): int {
    let r = MyResult.MyOk(42)
    if r is MyResult.MyOk(x) {
        return x
    } else {
        return -1
    }
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn surface_string_literal_argument() {
    // Exercises the fat-pair (data_ptr, len) materialization of string
    // literals in the bump heap when passed as an argument.
    let bytes = compile_source(
        r#"
func consume(name: str): int {
    return 7
}
func main(): int {
    return consume("hello")
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 7);
}

#[test]
fn surface_whole_struct_argument() {
    // A whole aggregate passed by value: the callee addresses the caller's
    // heap cell through the scalar (single-slot) argument register.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func getX(p: Point): int {
    return p.x
}
func main(): int {
    return getX(Point { x: 9, y: 8 })
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 9);
}

#[test]
fn surface_struct_is_pattern_binds_int() {
    // Struct `is` pattern: binds `y` to the field while guarding `x` with a
    // literal. Reaches the guarded path only when the literal matches.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func main(): int {
    let p = Point { x: 8, y: 25 }
    if p is Point { x: 8, y } {
        return y
    }
    return -1
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 25);
}

#[test]
fn surface_if_is_bind_scalar() {
    // Plain `x is y` binder over a scalar local.
    let bytes = compile_source(
        r#"
func main(): int {
    let x = 42
    if x is y {
        return y
    }
    return -1
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 42);
}

#[test]
fn surface_enum_else_payload_str() {
    // Enum else path: a fat payload (str) enum value that does not match the
    // bound variant falls through to the else branch with its runtime tag.
    let bytes = compile_source(
        r#"
enum MyResult {
    MyOk(int),
    MyError(str),
}

func main(): int {
    let r = MyResult.MyError("file not found")
    if r is MyResult.MyOk(x) {
        return x
    } else {
        return -1
    }
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), -1);
}

#[test]
fn surface_enum_simple_multivariant_without_payload() {
    // Enums without payloads: tag-only cells (no fat slot). Exercises several
    // runtime discriminant comparisons against a single stacked tag.
    let bytes = compile_source(
        r#"
enum State {
    Idle,
    Running,
    Done,
}

func main(): int {
    let s = State.Done
    if s is State.Idle {
        return 0
    }
    if s is State.Running {
        return 1
    }
    return 2
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 2);
}

#[test]
fn surface_borrow_shared_field() {
    // `&p` lowers to `Borrow` (empty projections on a memory-backed local);
    // the callee reads through a `Deref` + `Field` projection chain.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func readX(p: &Point): int {
    return p.x
}
func main(): int {
    let p = Point { x: 9, y: 8 }
    return readX(&p)
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 9);
}

#[test]
fn surface_array_oob_traps() {
    // Out-of-bounds array index: the bounds check lowers to `unreachable`.
    let bytes = compile_source(
        r#"
func main(): int {
    let arr = [1, 2, 3]
    return arr[5]
}
"#,
    );
    assert!(run_main_traps(&bytes));
}

#[test]
fn surface_struct_value_semantics_copy_does_not_mutate_original() {
    // Tests value semantics of aggregates: `let mut b = a` copies via
    // `memory.copy`. Mutating `b.x` must not alter `a.x`.
    let bytes = compile_source(
        r#"
struct Point {
    x: int
    y: int
}

func main(): int {
    let a = Point { x: 10, y: 20 }
    let mut b = a
    b.x = 99
    return a.x + b.x
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 109);
}

#[test]
fn surface_memory_grow_allocates_beyond_initial_pages() {
    // Allocates > 128 KiB (2 initial pages) across loop iterations to verify
    // that auto-grow triggers `memory.grow` and continues without trap.
    let bytes = compile_source(
        r#"
struct BigNode {
    v1: int
    v2: int
    v3: int
    v4: int
}

func main(): int {
    let mut sum = 0
    let mut i = 0
    // Each BigNode is 32 bytes. 5000 iterations = 160,000 bytes > 128 KiB.
    while i < 5000 {
        let node = BigNode { v1: i, v2: 1, v3: 2, v4: 3 }
        sum = sum + node.v2
        i = i + 1
    }
    return sum
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 5000);
}

#[test]
fn surface_string_literals_in_rodata() {
    // Verifies that string literals emitted in DataSection are accessible
    // through fat pointer len and data operations.
    let bytes = compile_source(
        r#"
func check(s: str): int {
    return 42
}

func main(): int {
    let s1 = "static rodata string"
    let s2 = "another constant"
    return check(s1) + check(s2)
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 84);
}

/// `free` must reclaim heap blocks so a tight allocate/free loop never runs out
/// of the initial pages: 1000 allocations × 80 bytes would cross 64 KiB of
/// heap without a real free list and trigger `memory.grow`.
#[test]
fn surface_alloc_free_loop_keeps_memory_at_initial_pages() {
    let bytes = compile_source(
        r#"
func main(): int {
    let mut i = 0
    while i < 1000 {
        let p = alloc(80)
        free(p)
        i = i + 1
    }
    return i
}
"#,
    );

    let name = find_func_export(&bytes, "main").expect("module must export `main`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`main` must have type () → i32");

    let before = memory.data_size(&mut store);
    let result = main
        .call(&mut store, ())
        .expect("`main` must run without trapping");
    let after = memory.data_size(&mut store);

    assert_eq!(result, 1000);
    assert_eq!(
        after, before,
        "recycling must keep the heap inside the initial pages (a leaking bump allocator would grow)"
    );
}

/// Drop elaboration turns an owned value into `Destroy(place)` before `return`;
/// the wasm backend must actually call the nominal `@Destructor` so resources it
/// owns are released. The destructor frees the handle the host handed in, which
/// the next same-size allocation must observe by reusing the very same block.
#[test]
fn surface_destructor_frees_owned_resource_at_return() {
    let bytes = compile_source(
        r#"
struct Resource { handle: ptr[u8] }

@Destructor
func Resource.close(own self): void {
    free(self.handle)
}

public func adopt(p: ptr[u8]): i32 {
    let r = Resource { handle: p }
    return 0
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");
    let name = find_func_export(&bytes, "adopt").expect("module must export `adopt`");
    let adopt = instance
        .get_typed_func::<i32, i32>(&mut store, &name)
        .expect("`adopt` must have type (i32) -> i32");

    // Hand a 64-byte block to the guest; its `@Destructor` must release it.
    let p = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("host allocation must not trap");
    let rc = adopt.call(&mut store, p).expect("`adopt` must not trap");
    assert_eq!(rc, 0);

    let q = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("reuse allocation must not trap");
    assert_eq!(
        q, p,
        "the @Destructor must free the owned handle before `adopt` returns"
    );
}

/// The AMIR contract materializes a method receiver as `func.params[0]`; the
/// wasm signature must therefore include it exactly once and `obj.m()` must
/// forward it as the first argument. Drop glue calls destructors through the
/// same method path, so this pins the receiver arity independently of `Destroy`.
#[test]
fn surface_method_call_passes_receiver_once() {
    let bytes = compile_source(
        r#"
struct Point {
    x: i32
    y: i32
}

func Point.sum(self): i32 {
    return self.x + self.y
}

public func main(): i32 {
    let p = Point { x: 20, y: 22 }
    return p.sum()
}
"#,
    );

    assert_eq!(run_main_i32(&bytes), 42);
}

/// A composite without its own `@Destructor` drops each field by elaborating
/// `Destroy(composite.field)`, then releases the composite's own cell. The
/// backend must load the field's *cell pointer* (named fields are boxed) at the
/// right offset and pass it to the field type's destructor.
///
/// Destructors run in reverse field order, and the composite's own cell is
/// released last, so the resulting free list is `C → CA → pa → CB → pb`
/// (allocations are appended, frees are prepended LIFO). A same-size 64-byte
/// reallocation skips the small cell blocks and returns the last-dropped
/// field's handle `pa`, proving the backend read it from the non-zero offset 4
/// (`pad` shifts `a` there).
#[test]
fn surface_nested_destructor_frees_field_handle_at_offset() {
    let bytes = compile_source(
        r#"
struct HandleA { p: ptr[u8] }
@Destructor
func HandleA.close(own self): void {
    free(self.p)
}

struct HandleB { p: ptr[u8] }
@Destructor
func HandleB.close(own self): void {
    free(self.p)
}

struct Bundle {
    pad: i32
    a: HandleA
    b: HandleB
}

public func adopt2(pa: ptr[u8], pb: ptr[u8]): i32 {
    let bundle = Bundle {
        pad: 0,
        a: HandleA { p: pa },
        b: HandleB { p: pb },
    }
    return 0
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");
    let name = find_func_export(&bytes, "adopt2").expect("module must export `adopt2`");
    let adopt2 = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, &name)
        .expect("`adopt2` must have type (i32, i32) -> i32");

    let pa = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("first host allocation must not trap");
    let pb = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("second host allocation must not trap");
    let rc = adopt2
        .call(&mut store, (pa, pb))
        .expect("`adopt2` must not trap");
    assert_eq!(rc, 0);

    let r1 = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("reuse allocation must not trap");
    assert_eq!(
        r1, pa,
        "the last-dropped field's @Destructor must load its handle from offset 4 and free it"
    );
}

/// AMIR locals and temps are distinct index spaces, so the backend must give
/// them distinct wasm slots. Sharing a slot by numeric index silently aliases a
/// live local with the temp of the same index; here both `a` and `b` are still
/// live after the return temp is written, so a shared slot would return a wrong
/// value (or trap).
#[test]
fn surface_local_and_temp_slots_do_not_alias() {
    let bytes = compile_source(
        r#"
struct Res { v: i32 }

@Destructor
func Res.close(own self): void {}

public func main(): i32 {
    let a = Res { v: 7 }
    let b = Res { v: 9 }
    return a.v + b.v
}
"#,
    );
    assert_eq!(
        run_main_i32(&bytes),
        16,
        "locals and temps must not share wasm slots"
    );
}

/// A `@Destructor` is the storage cleanup for the whole value: besides running
/// the user destructor, the backend must reclaim the receiver's heap cell, and
/// it must not materialize a temporary value-copy of the receiver. Either
/// omission leaks one 64-byte cell per call. `make` builds a 64-byte composite
/// and drops it before returning, so a tight host loop must stay inside the
/// initial pages (a leaking backend would need `memory.grow`).
#[test]
fn surface_destructor_reclaims_receiver_cell_without_growth() {
    let bytes = compile_source(
        r#"
struct Big {
    f0: i32
    f1: i32
    f2: i32
    f3: i32
    f4: i32
    f5: i32
    f6: i32
    f7: i32
    f8: i32
    f9: i32
    f10: i32
    f11: i32
    f12: i32
    f13: i32
    f14: i32
    f15: i32
}

@Destructor
func Big.close(own self): void {}

public func make(): i32 {
    let x = Big {
        f0: 1, f1: 2, f2: 3, f3: 4,
        f4: 5, f5: 6, f6: 7, f7: 8,
        f8: 9, f9: 10, f10: 11, f11: 12,
        f12: 13, f13: 14, f14: 15, f15: 16,
    }
    return x.f0
}
"#,
    );

    let name = find_func_export(&bytes, "make").expect("module must export `make`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");
    let make = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`make` must have type () → i32");

    let before = memory.data_size(&mut store);
    for _ in 0..10_000 {
        assert_eq!(make.call(&mut store, ()).expect("`make` must not trap"), 1);
    }
    let after = memory.data_size(&mut store);

    assert_eq!(
        after, before,
        "each @Destructor call must reclaim its receiver cell without a temporary copy"
    );
}

/// A composite without its own `@Destructor` still owns its heap cell: after the
/// field destructors run, `Destroy(root)` must release the container's cell too,
/// otherwise every drop leaks the container. `make_bundle` allocates a container
/// plus two field cells and drops them, so the host loop must not grow memory.
#[test]
fn surface_nested_destroy_reclaims_container_cell() {
    let bytes = compile_source(
        r#"
struct Handle { p: ptr[u8] }
@Destructor
func Handle.close(own self): void {
    free(self.p)
}

struct Bundle {
    a: Handle
    b: Handle
}

public func make_bundle(): i32 {
    let bundle = Bundle {
        a: Handle { p: nil },
        b: Handle { p: nil },
    }
    return 0
}
"#,
    );

    let name = find_func_export(&bytes, "make_bundle").expect("module must export `make_bundle`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");
    let make_bundle = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`make_bundle` must have type () → i32");

    let before = memory.data_size(&mut store);
    for _ in 0..50_000 {
        assert_eq!(
            make_bundle
                .call(&mut store, ())
                .expect("`make_bundle` must not trap"),
            0
        );
    }
    let after = memory.data_size(&mut store);

    assert_eq!(
        after, before,
        "dropping a composite must reclaim the container cell, not just its fields"
    );
}

#[test]
fn to_str_and_string_interp_with_f64_executes() {
    let interner = TypeInterner::new();
    let f64_ty = interner.intern(ArType::Primitive(Primitive::F64));
    let str_ty = interner.intern(ArType::Primitive(Primitive::Str));
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let mut pool = AmirLiteralPool::default();
    let pi_lit = pool.intern_float("3.14159");
    let pi_op = || AmirOperand::Constant(AmirConstant::Pool(pi_lit));

    let ret_t = TempId::from_usize(0);
    let str_t = TempId::from_usize(1);
    let len_t = TempId::from_usize(2);

    let mut stmts = AmirStmtTable::new();
    // str_t = ToStr(3.14159)
    stmts.push(AmirStmt::Assign {
        lhs: str_t,
        rhs: AmirRvalue::ToStr {
            value: pi_op(),
            src_ty: f64_ty,
        },
    });
    // len_t = Len(str_t)
    stmts.push(AmirStmt::Assign {
        lhs: len_t,
        rhs: AmirRvalue::Len(AmirOperand::Copy(str_t)),
    });
    // ret = len_t
    stmts.push(AmirStmt::Assign {
        lhs: ret_t,
        rhs: AmirRvalue::Use(AmirOperand::Copy(len_t)),
    });

    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 3),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![temp(0, int), temp(1, str_ty), temp(2, int)],
        blocks: blocks.clone(),
        block_params: vec![],
        stmts,
        cfg: compute_cfg_edges(&blocks),
    };

    let bytes = emit_one(func, &interner, &mut pool);
    // "3.14159" has length 7
    assert_eq!(run_main_i32(&bytes), 7);
}
