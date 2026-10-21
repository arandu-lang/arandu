# Arandu — Public Comptime Contract

## Visão Geral e Contexto

**Status:** scalar/aggregate values, enums, global constants, computed headers,
composed staging, typed specialization and explicit target identity are
implemented during the 0.1.9 core campaign and included in the published
[0.1.10 release](https://github.com/arandu-lang/arandu/releases/tag/v0.1.10).
The campaign's implementation and remaining boundaries are consolidated here;
the master roadmap owns the remaining work. Publication of this core does not
accept the broader metaprogramming RFC or certify every CT.0–CT.5 ambition.

## Detalhes Técnicos da Implementação

### Integrated core architecture

The integrated core preserves the dependencies between compiler phases. Each frozen value remains independent of AST/HIR/AMIR pools and is
identified by a bounded, versioned semantic encoding.

1. **Closed enums and ADTs.** The frozen aggregate bridge carries an explicit
   variant identity and optional payload. Admission resolves every
   variant against target types, substitutes generic arguments and proves Copy
   and absence of cleanup. The VM interprets construction, discrimination and
   payload extraction with checked tags and projections. Materialization uses
   existing typed HIR constructors and the common backend lowering path. Nominal
   variants require full source symbols; layout-only metadata without those
   symbols cannot admit frozen enums, including inactive variants. Structural
   Option/Result variants retain their canonical tags without nominal symbols.
2. **Computed array dimensions.** Canonical CST/AST preserves the expression;
   the pre-body obligation path evaluates and freezes its length before final
   type checking. Negative/out-of-range values and unresolved dependencies are
   rejected while preserving per-item cutoff.
3. **Composed staging.** Nested roots execute within their enclosing evaluation's
   budget. Helpers use typed, staged obligations without requesting the active
   owner's final typing. Explicit query cycle recovery must prevent reentrant
   Salsa/CTFE evaluation from becoming a compiler panic.
4. **Global constants.** A declaration-scoped frozen-value query provides
   deterministic dependency-cycle recovery, including imported declarations.
   Consumers depend on the semantic value rather than initializer body syntax;
   residual HIR contains the value and never executes its initializer. Static
   storage follows the shared backend eligibility rules.
5. **Typed specialization and target identity.** Constant specialization uses
   checked, declared-type frozen identities while retaining compatibility for
   array lengths. OS and architecture enter as explicit target
   inputs, alongside DataLayout; queries never derive them from the host.

Acceptance requires valid and invalid cases at each bridge: bounds, Copy and
cleanup, nominal and variant identities, generic substitution, target mismatch,
cycles, cancellation and shared budgets. Integration tests must exercise
materialization and backend parity, imported early-cutoff and deterministic
errors. Existing runtime and incremental tests remain part of the final gate.
The feature contracts below specify the implemented boundaries and remaining
restrictions independently of campaign or release promotion.

### Dependent declaration contracts

A computed dimension in a generic declaration header must be frozen for a
concrete owner and its structural arguments before a caller's body is typed.
Freezing only the callee body is insufficient: the caller already needs the
parameter/result type to check the call. Replacing symbol-wide field metadata
with the last instantiated dimension would also make two simultaneous
instantiations share the wrong layout.

The implemented `concrete_header_contract` query is keyed by the full owner
identity, canonical concrete arguments and explicit dependency context. Its output contains structural
function signatures or nominal field/payload types, plus failures with source
spans. Evaluation reuses `HeaderArgument` roots in an `InInstance` environment
and the existing AMIR VM; it does not introduce an AST arithmetic evaluator.
Closed header obligations retain their declaration-only cache. A pure discovery
step records the concrete contracts demanded by the selected source slice.
It uses the canonical checker's existing inference, including inferred call
arguments, and publishes demands rather than running queries in type checking.
The query continuation installs immutable contracts and repeats that selected
check until its demands close, with a bounded number of passes. Only the final
checked result is published; intermediate template types cannot reach lowering. Type checking and
layout consult the same concrete contract so `[N + 1]T` cannot acquire different
lengths in call checking, field access, CTFE admission and backend storage.

The regression matrix covers two lengths of the same nominal declaration
in one function, computed function/alias/enum headers, an imported return type,
`@sizeOf` depending on a type argument, inferred and forwarded arguments,
failure/cycle recovery, target edits, and equal-valued helper edits that cut off
callers. Concrete field/payload lookup is shared by layout/ABI, Copy and borrow
proofs, CTFE admission/materialization, MIR projections and native/C/Wasm storage.
Native CGU fingerprints include concrete field/payload types; C nominal names
include structural arguments so two instances cannot share a typedef.
Frozen global result types demand the same contracts before residual
materialization. These metadata demands are isolated from unrelated seed
signature demands and retain the requesting CTFE dependency context.
No unresolved header placeholder may reach AMIR or backend code generation.

Memo equality for types uses their bounded structural encoding, including full
frozen argument contents and nominal identities. User-facing type presentation
is not a semantic key: two aggregate arguments can display the same type while
containing different values. Signature, field, payload, default and constraint
metadata use that encoding; instance caches are ordered by semantic keys rather
than interner allocation order. Imported signature regressions compare edits
against a fresh database while preserving equal-valued cutoff checks.

Global dependency depth is a separate resource boundary from VM call frames:
one declaration can demand another query before either starts executing AMIR.
Declaration evaluation carries an immutable causal path in its query/request
key. It admits at most 16 nested global/header dependencies, independently of
VM frames. This conservative ceiling protects native query nesting, including
generic helpers, on the default test-thread stack; it is not a host-memory
heuristic. A repeated semantic declaration identity reports T044 before the
depth check; an acyclic over-limit path reports T045. Diagnostics retain up to
eight recent causal source labels. Branch selection, computed arguments,
loop bounds and generic helper lowering inherit the same path. Discarded static
branches create no dependencies. No mutable process/thread counter participates
in query results. Linking consumes already admitted global values instead of
starting a fresh evaluation. Editing a failed dependency revalidates its memo
and permits recovery.

## Syntax and result

In a function body, `comptime` evaluates an admitted expression or
block during compilation and inserts its typed value into the residual program.
Dependent generic bodies are staged with a concrete instance environment.

```arandu
func add(a: int, b: int): int {
    return a + b
}

func main(): int {
    let answer: int = comptime add(20, 22)
    let same: int = comptime {
        let half = 21
        return half * 2
    }
    return answer
}
```

The expression form binds as a prefix operator: `comptime 20 + 22` stages only
`20`; use `comptime (20 + 22)` to stage the whole binary expression. Calls and
postfix operations belong to the prefix operand. The block form owns its scope
and return target. `return value` exits that CTFE root, not the runtime function.
All returns unify with a productive final expression, including syntactically
present unreachable returns/tails. `return;` and an empty block produce `void`.
A semicolon discards a final expression. No new `()` type spelling is introduced.
Every non-unit exit must have a result.

The admitted result types are `bool`, `int`, `uint`, `isize`, `usize`, `i8`,
`i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `byte`, `float`, `f32`, `f64`,
`str`, immutable literal-backed byte views, and `void`. Tuples, fixed arrays
and closed Copy structs/enums may contain admitted values when their types have
no runtime destructor or unsupported resource. This includes `Option<T>` and
`Result<T, E>` with admitted payloads. Arbitrary references and mutable/owned
heap resources remain outside the value model. Contextual
typing chooses the numeric type before evaluation. Without a contextual type,
an integer result defaults to `int` and cannot silently narrow at a later use.
Pointer-sized types use the selected target layout; ordinary `int`/`uint` remain
32-bit. Materialization
preserves the full signed/unsigned range without lossy casts or VM handles.

## Captures, operations and limits

### Repeated arrays

`[value; N]` is an ordinary array expression, also admitted inside `comptime`.
The length can be an integer literal, an integer compile-time parameter, or
an explicit `comptime (expression)` in a supported function-body staging context.
It shares generic argument staging; no parallel AST evaluator is introduced.

```arandu
func flags<comptime N: uint>(): [N]bool { return [false; N] }
func main(): void {
    let horizontal = comptime {
        let mut table = [false; 256]
        table[32] = true
        table[9] = true
        table[13] = true
        table
    }
}
```

The initializer is evaluated exactly once, even for zero elements. Zero-length
repetitions dispose of an owning initializer at the expression boundary;
one-element repetitions move it normally. Greater lengths require Copy,
checked again after specialization. There is no implicit clone or shallow
duplication of owners. T048 explains invalid lengths/ownership.

The AST/HIR retain a single initializer. Shared AMIR lowering constructs an
ordinary array, so CTFE and all backends use existing array semantics. Eager IR
materialization is bounded to 65,536 elements and charged against the residual
work budget inside static loops. This is a resource guard, not a claim of an
unlimited compact repeat representation. A future compact constructor can
remove that eager-expansion ceiling without changing the surface syntax.
The full target layout of a repeated array is checked before lowering its
initializer, including nested aggregates; sizes exceeding the backend's current
32-bit allocation-size interface are rejected with T048 instead of truncated.

Numeric leaves of public staged aggregate types are defaulted structurally
before defining runtime locals, keeping frozen initializers and SSA parameters
consistent across control-flow joins. `ascii.byteTable(text)` exercises repeat,
mutation and imported helper evaluation; `ascii.ByteSet` remains the compact
32-byte alternative. Neither API guarantees vectorization or fewer instructions
on every target.

CTFE evaluation and residual storage are separate contracts. Before the
aggregate-storage refinement, frozen Copy arrays were materialized with heap
backing. An eight-core Pypor experiment with two 256-byte tables retained the
same counts but increased peak RSS compared with compact ByteSets; its assembly
called `ar_rt_raw_malloc` without Copy-only cleanup. The current native/Wasm
consumers use shared bounded static initializers and private backing for
admitted cases, as documented in [the ABI contract](arandu-abi-layout-v0.1.md).
`comptime` alone still does not guarantee allocation-free storage: borrowed
views, aggregate back-edge phis and other conservative exclusions retain
fallbacks. Unconditional drops of Copy values would not be safe; their aliases
must retain independent value semantics without dangling backing or double frees.

Locals declared inside the root belong to its CTFE frame. Ordinary global
constants are available; runtime locals and parameters outside the root cannot
be captured, even if initialized from literals. Pure helpers, including
registered imported modules and concrete generic instances, use independent
AMIR units. Retained operations/callees must be admitted; an untaken runtime
branch does not authorize an external call, allocation or unsupported type.

Integer arithmetic is checked. Overflow/invalid shifts use T046, and division
or remainder by zero uses T040. Resource exhaustion uses T045. Each root has
an allowance drawn from 1,000,000 fuel units and 1,000,000 value slots per
obligation family in a staged item/instance, with 128 frames per evaluation.
Explicit roots, static conditions, computed arguments and loop bounds are
separately bounded families; roots and generated occurrences within each family
share its allowance rather than each restarting the full budget.
At most 4,096 roots are allowed per item. Aggregate shapes/values also enforce
128 levels and 4,096 nodes, while allocation accounting bounds payloads and
backings independently of fuel. These limits do not measure compiler-wide RSS.
Cancellation unwinds analysis rather than caching
a language error. There is no wall-clock, filesystem, network or host-ABI
dependency in CTFE queries and no automatic fallback to runtime execution.

Unsupported result/intermediate types use T042; forbidden captures use T043
with declaration context. Other admission/evaluation failures use T044 with
bounded source traces. The formatter, semantic keyword classification,
completion and TextMate fallback recognize the public keyword. Themes still
choose colors.

## Public target layout expressions

Use `mem.sizeOf<T>()` and `mem.alignOf<T>()`, importing `std.core.mem as mem`,
to inspect the selected target ABI. Both return `usize`, allocate no memory,
and use the shared `LayoutEngine`. The resolved `extern "arandu-intrinsic"`
declarations identify these operations; ordinary functions named `sizeOf` or
`alignOf` retain their bodies.

```arandu
import std.core.mem as mem

func size<T>(): usize {
    return mem.sizeOf<T>()
}

func main(): usize {
    let bytes = comptime (mem.sizeOf<[3]u16>())
    return bytes + mem.alignOf<i64>() + size<int>()
}
```

The 0.1.9 type-operand expressions `@sizeOf(Type)` and `@alignOf(Type)` remain
supported for compatibility. They are not declaration annotations and receive
neither PascalCase aliases nor a new deprecation warning in 0.1.10. The formatter
does not rewrite them. New examples prefer the module API; any future removal
requires an announced migration and parity coverage for global constants,
computed generic arguments and declaration headers as well as function bodies.

Size and alignment are different: i686 System V gives `i64` size 8 and alignment
4. Generic operand types are substituted before AMIR lowering; pre-body scalar
conditions and computed generic arguments can inspect concrete operand types
without requesting runtime AMIR. Unsupported/open nominal layouts and target object-size
overflow use T047; recursive by-value source definitions retain T029. Physical
layout and niche walks share the existing structural ceilings of 128 levels
and 4,096 visits, rather than resetting limits in recursive calls.

`[comptime (expr)]T` freezes fixed array dimensions in local annotations and
supported declaration headers. Concrete generic instances can use layout
queries in dimensions and dependent pre-body selection without requesting
final runtime AMIR.

### Proposed expression APIs

`embed.bytes(path)`, `embed.str(path)` and `types.info<T>()` are design directions,
not implemented standard-library APIs. They do not introduce `std.embed`,
`std.types`, expression forms `@EmbedBytes`/`@TypeInfo`, or an array type
`[comptime]u8`. Fixed arrays still require a concrete, checked length.

Asset inclusion needs explicit path/root, result ownership/type, UTF-8, size
budget and error contracts. Files must be registered as compiler inputs by the
input-owning layer; neither tracked queries nor the CTFE VM may read them.
Dependency changes, missing files and equal-content edits need incremental
invalidation/cutoff tests. Reflection needs bounded frozen descriptors with
defined type identity, visibility and target dependence before it can be
exposed through a module API. See the
[naming contract](arandu-attribute-naming-v0.1.md#operações-em-expressões).

## Architecture

CST/AST preserves explicit staging roots. Initial item typing records local
obligations but does not invoke the interpreter. `item_staged_typing` in
`arandu_query` evaluates them through `ctfe_eval_root` with source-local ordinal
selectors and expected pool-independent types. Concrete environments carry
structural instance arguments, and generated occurrences retain their path.
This path does not request runtime units,
borrow contracts or final AMIR. CTFE helpers use initial typing, preventing a
recursive staging request from cycling through final runtime typing.

The evaluated values are pool-independent metadata keyed by full source spans
(including file identity), not arena IDs retained by another item revision.
HIR lowering uses the pure materializer; backends see ordinary literals,
aggregate constructors and the common AMIR only. Typed IEEE encodings and frozen
byte backing are shared value contracts, not per-backend evaluators.
Values and target layout participate in body fingerprints, not exported
declaration hashes. Items without explicit staging share the initial memo
without scanning the source arena. Regression tests cover sibling cutoff,
callee invalidation, target changes, arithmetic/capture failures, LSP UTF-16
and residual execution in C/Cranelift/Wasm at O0/O1/O2.

## Static branch selection

In function statement blocks, `comptime if` evaluates
its `bool` condition before resolving the body. Both branches must have valid
syntax, but only the selected branch is resolved, typed and lowered:

```arandu
func main(): int {
    comptime if true {
        return 42
    } else {
        return unavailableOnThisTarget()
    }
}
```

The condition uses the same VM, target layout, checked arithmetic and
budgets as other public roots. Global constants and pure helpers,
including imported helpers with static branches, are available. Runtime locals
and parameters are forbidden captures, including names that shadow global
constants or come from an enclosing loop/pattern. A failed or cyclic condition
selects neither branch and reports its own error, not errors from either body.

Selection is parent-first: nested conditions in discarded branches are never
evaluated, and discarded `comptime` expressions create no obligations. A
selected branch retains a normal lexical scope, with ownership, drops, `defer`,
`return`, `break` and `continue` handled by the existing HIR/AMIR pipeline.
Its body may perform ordinary runtime operations. `return` there returns from
the containing function, unlike a return inside a `comptime { ... }` root.
`else comptime if` is static; an ordinary `else if` remains runtime.

`item_static_branches` owns selection in `arandu_query`, using pre-body
`resolved_headers`/`header_signatures` and isolated condition/helper queries.
Its explicit revision-local decision map travels through resolution and typing.
HIR retains only a neutral lexical `Scope`, not a conditional or an `unsafe`
block. Backends receive the common AMIR without a static-selection opcode.
Body-only edits with unchanged decisions cut off selection consumers; exported
declaration hashes remain independent of those decisions.

Concrete generic instances select dependent conditions before resolving/typing
their residual body. Match arms retain their lexical scopes: runtime pattern
bindings cannot become CTFE inputs, although the selected runtime body can use
them. Lambda staging preserves lexical selection and capture checks. General
closure typing and execution remain unsupported (U001), planned separately
for the 0.3.x closure contract. The thread-job shape `move ||` with no
parameters and owned Send captures lowers to a capture struct and `run()`;
this preparation does not enable general executable lambdas. Nested explicit
roots and helper-local roots execute as value scopes within the enclosing VM
evaluation; local returns and propagation leave that value scope. They share
the enclosing VM meter rather than restarting fuel or frame limits.
Unannotated value blocks infer their result from explicit returns and the tail
before validating `?`. Result propagation checks the error channel, not the
success type; Option propagation requires an Option result. Nested blocks retain
independent return and propagation obligations.

## Explicit boundaries and future

### Value parameter spelling

`<comptime N: uint>` is a compatible spelling of `<const N: uint>`. Both lower
to the same value-parameter AST, symbol kind, `Const(u64)` instance argument
and existing monomorphizer. For example:

```arandu
func count<comptime N: uint>(): uint {
    return N
}

func main(): int {
    return (count<20>() + count<22>()) as int
}
```

The same parameters remain available in fixed array sizes (`[N]T`). Parameter
types and numeric bounds retain their existing rules: `uint` is 32-bit, so a
64-bit instance identity does not authorize a value outside that declared
range. Neither spelling introduces an ordinary runtime parameter, another
monomorphizer or permission to exceed the declared parameter's domain.

### Computed generic arguments

In ordinary statement blocks of functions, an explicit
`comptime (expression)` can supply a constant generic argument. Parentheses
are required to separate expression operators and commas from the generic list.

```arandu
func add(a: int, b: int): int { return a + b }
func count<comptime N: uint>(): uint { return N }
func total<comptime N: uint>(values: [N]int): int {
    return (N as int) + values[0] + values[1]
}
func main(): int {
    let answer = count<comptime (add(20, 22))>()
    return total<comptime (1 + 2)>([19, 20, 0])
}
```

`item_const_arguments` freezes each value **before** ordinary body typing,
using isolated roots, body-free headers and pure helpers. The
pure checker reads the frozen argument and the existing monomorphizer receives
`Const(u64)`: `count<comptime (20 + 22)>()` and `count<42>()` share an instance.
No helper call for the argument computation survives in the runtime caller.
Non-negative integer keys retain `Const(u64)` compatibility. Signed integers,
booleans and closed Copy aggregate/enum values use a frozen structural key.
The graph retains each full frozen value for equality. Emitted symbol names use
a domain-separated BLAKE3 digest of its canonical bytes to remain bounded when
aggregate arguments contain large immutable strings; symbol spelling does not
replace the semantic specialization key.
The focused resource regression freezes a one-element array containing a 16 KiB
immutable string: its 16,433 canonical bytes formerly produced a 32,873-byte
argument spelling; the digest spelling occupies 71 bytes. A standalone encoding
microprofile of 1,000 names, repeated three times on the development host,
measured approximately 383 ms for hexadecimal expansion and 12.65 ms for the
digest. This measures symbol encoding only, excluding parsing, CTFE and the
remaining compilation pipeline; it is not an end-to-end compiler speedup claim.
T003 rejects unsupported value kinds, and T011 checks the declared domain. Their expression type is inferred normally
(default `int`), not widened to the generic parameter type. Use an explicitly
typed helper for wider values. T011 also checks the declared parameter range
for computed **and literal** keys, including target-sized `usize`/`isize`.

The same target configuration, resource limits, checked arithmetic and capture
rules apply. Discarded static branches create no argument obligations. At most
4,096 computed arguments are staged per function. The revision-local frozen
map records failed obligations explicitly; missing values cannot fall back to
runtime. Its deterministic hash includes failures and diagnostics. Equal values
cut off consumers; header continuation can still revalidate root lowering after
a sibling edit, so this cut does not promise zero work for every source edit.

Closed computed declaration headers/aliases, plain expression conditions and
nested arguments are staged before their pure consumers. Pattern/loop headers
use the same continuation with lexical scope and runtime-capture checks. Match arms
use the same lexical continuation and capture checks. Lambda scopes are prepared
likewise but remain subject to the U001 closure boundary described above. The
source-symbol helper API remains distinct from the concrete-instance API;
generic evaluation requires structural arguments rather than an unresolved
template. `[comptime (expr)]T` freezes fixed array dimensions in local
annotations and closed declaration headers. Local dimensions dependent on
constant generic parameters are staged per instance. Dependent declaration
headers use the same concrete environment through their canonical signature,
field and payload contracts; template-wide metadata remains immutable.

Computed arguments inside generic function bodies are supported when independent
of that template's parameters: they freeze once per source item and target,
not once per runtime instance. For example, a `wrapper<T>` may call
`count<comptime (20 + 22)>()`. Dependent expressions such as `N + 1` are deferred
until a concrete instance supplies `N`; separate instances retain separate
decisions and frozen arguments without changing `FunctionInstance` identity.

Forwarding an existing constant parameter (`leaf<M>()`) must preserve its entire
declared domain. `M: u8` can flow into `N: u16`, but `M: u16` cannot flow
into `N: u8`, even if a particular caller supplies `1`. This rule prevents
truncation after specialization and uses target-sized bounds for `usize`/`isize`.
T011 labels both declarations and suggests compatible parameter domains.

### IDE values and generated regressions

Hover over a computed argument or scalar root shows `= 42` with an
`evaluated at compile time` caption. Hover over its generic callee keeps the
signature and appends those arguments' frozen values in source order. The pure
presentation lives in `arandu_ide`; the LSP only adapts Markdown and UTF-16
ranges. The AST links each value to its actual callee: no byte windows, nearby
call leakage, guessed result for failed/discarded obligations, or direct VM
execution in the presenter. Subsequent edits use current revision metadata.
Boolean and signed computed arguments use the same frozen-value presenter as
public roots. A failed argument suppresses the instantiated call's value
presentation, even when its other typed arguments evaluated successfully.

T043 explains that runtime locals and parameters do not exist during compilation.
T045 fuel/frame exhaustion suggests checking recursion and loop termination;
value/allocation limits suggest reducing intermediate data instead.

Normal AranduSmith programs include bounded nested static trees, with independent
expected values and unavailable names in discarded branches. One in sixteen
seed keys also checks incremental analysis against clean analysis and requires
cutoff of VM evaluation after same-span discarded-code edits. Edits outside
the owning item preserve selection; displaced source spans require legitimate
revalidation, not stale-map reuse. Cache failures record seed/source artifacts
without pretending single-source shrinking can preserve an edit sequence.

Module/type constants use declaration-scoped frozen-value queries, including
`const X = comptime ...` and admitted pure initializer calls. Imported consumers
read canonical values; equal values cut off their typing dependencies. Direct,
mutual and imported constant cycles emit T044. The residual initializer is a
materialized value, never a runtime helper call.

Target identity is an explicit `TargetConfig` input alongside `DataLayout`.
`std.target.os`, `std.target.arch` and `std.target.pointer_width` expose it via
`@targetOS()`, `@targetArch()` and `@targetPointerWidth()`; semantic queries do
not inspect the host. Native and Wasm CLI drivers select identity before typing.

Arbitrary references and unsupported heap resources remain outside CTFE. Supported
aggregate/string/float values and concrete staging do not authorize general
metaprogramming, heap allocation, reflection or generated declarations.
Finite static expansion accepts integer half-open ranges and fixed Copy arrays.
Array producers are evaluated once by CTFE and frozen before runtime lowering;
struct, enum, string and nested-array elements retain their structural type in
occurrence keys. Iteration bindings are immutable. Empty domains discard the body
before name resolution, including unavailable names. Inclusive ranges, mutable
bindings and arbitrary iterable resources remain unsupported for static expansion.
Computed arguments in ordinary loop and pattern headers use the incoming lexical
scope. Earlier bindings in a pattern conjunction are visible, but remain runtime
captures and are rejected by T043 when read by CTFE.
Occurrence-dependent staging is implemented within the finite expansion
cut and does not imply arbitrary dependent domain evaluation. Ordinary
`if`/`while` inside a CTFE block are evaluated by the VM with both branches
resolved and typed; use the explicit static statement above for branch exclusion.

Public budgets are explicit Salsa inputs (`CtfeLimits`) with positive fuel,
frame and live-value limits. The CLI exposes `--ctfe-fuel`, `--ctfe-frames` and
`--ctfe-values`; invalid driver configuration exits with usage status 2. Budget
changes invalidate staged values and incremental build fingerprints. LSP accepts
`initializationOptions.ctfe.{fuel,frames,values}` and rejects malformed options
with JSON-RPC InvalidParams before workspace discovery. VS Code exposes
`arandu.comptime.{fuel,frames,values}`; restart the server after changing them.
The Rust web API also accepts explicit limits. Playground analysis uses explicit
`unknown` OS / `wasm32` architecture and a four-byte pointer layout, independently
of the machine hosting the compiler.

Generated occurrences divide their family's envelope by the actual ancestor
cardinalities, including jagged nested arrays. Fuel and values never round up,
so sibling evaluations cannot multiply the public envelope. Expansion still
allows at most 4,096 occurrences, with at most 16 nested staging contexts;
these limits remain separate from VM frames and host RSS.
Native installed validation and release readiness are separate gates from
development-host tests; the release evidence below records the published smoke
coverage without extending it to every CTFE or editor scenario.

### Integrated development-host evidence

On Linux x86-64, 2026-10-03, the six prescribed workspace gates passed in
order: formatting, locked check, all-target/all-feature Clippy with warnings
denied, workspace tests, diagnostic documentation and warning-free rustdoc.
The workspace run including repeated-array regressions recorded 2,768 passed
tests, zero failures and eight ignored
tests across 132 suites, including doc tests. Ignored tests are not claimed as
executed. Architecture, canonical LF and diagnostic determinism (one versus
eight threads) checks also passed.

Before the repeated-array integration, the VS Code Extension Host passed both project and manifestless-file runs;
the extension's 26 unit tests, compilation and lint checks passed separately.
Differential tests exercise residual values and finite loops in C, Cranelift
and Wasm at O0/O1/O2. Query regressions cover concrete instances, cycles and
cutoff, including imported layout intrinsics after a sibling body changes
size. This evidence closes the five implementation cuts on the development
host, not native Windows/macOS CI, playground delivery or release promotion.

The final diff review also hardened Cranelift enum payload projections: malformed
field indices and offsets outside the emitter's range produce a reportable ICE
instead of falling back to field zero or truncating an offset. A deliberately
malformed AMIR regression covers both cases. Parser recovery tests keep the
statement-only `comptime if`/`comptime for` diagnostic aligned with the supported
value-producing block syntax.

## A stdlib consumer: byte sets

After the aggregate-storage and tooling refinements, the prescribed six gates
passed again in order on Linux x86-64 (2026-10-04): 2,820 tests passed, zero
failed and eight were ignored across 132 suites. Architecture, canonical LF
and diagnostic determinism checks also passed. This does not replace native
Windows/macOS validation or the earlier, separately executed Extension Host gates.

`std.core.ascii.byteSet(text)` builds a closed Copy `ByteSet` containing four
`u64` words. A caller can write `comptime ascii.byteSet(" \t\r")` to run the
pure construction loop in the VM and freeze the 32-byte result. The same API
accepts runtime text; `contains(u8)` only borrows the set and performs a bounded
word lookup and bit test. UTF-8 input denotes its encoded bytes, not Unicode
scalar values; empty strings and duplicate bytes have ordinary set semantics.

The CLI regression imports the actual stdlib module and checks all 256 byte
values, including empty sets, NUL, word-boundary bits and UTF-8 bytes. It
compares CTFE construction with runtime construction, checks the target layout,
and inspects AMIR to require that only the explicitly runtime builder remains
as a call. Native execution and emitted C execution (when Clang is available)
exercise the residual values.

The external Pypor scanner uses two such frozen sets for horizontal whitespace
and identifier bytes. Its exhaustive test preserves the old byte predicates;
before/after binaries produced identical totals for the local Linux kernel
corpus (65,479 files and 37,995,166 lines). This is a real consumer test, not a
claim of Unicode classification, reduced peak memory or native CI coverage.

## Frozen IEEE values and immutable text

The additional value cut admits `float`, `f32`, `f64`, UTF-8 `str` literals and
immutable byte views backed by those literals. `float` follows
the selected `DataLayout.float.size` (currently binary64 in every supported layout), while
`f32` and `f64` retain their IEEE formats. Decimal parsing converts directly to
the destination format. Addition, subtraction, multiplication, division and
numeric conversions use software arithmetic with nearest-ties-to-even rounding.
Float-to-integer casts truncate toward zero and reject NaN, infinity and values
outside the destination range. Float division by zero follows IEEE semantics;
integer division keeps its existing diagnostic.

Subnormal values, infinities and signed zero survive evaluation and
materialization. Arithmetic NaNs use the positive quiet canonical encoding;
numeric comparisons follow IEEE unordered behavior. Value/cache equality uses
the declared format and exact bits, so it distinguishes signed zeros and can
compare frozen NaNs. Remainder, FMA, transcendentals and fast-math remain outside
this cut. One decimal literal is bounded to 4,096 source bytes before invoking
the non-interruptible decimal conversion; VM fuel also charges literal input.

HIR and AMIR retain typed `FloatBits`, and each backend emits those encodings
directly: Cranelift IEEE immediates, Wasm float constants and C `memcpy` helpers.
Decimal display is presentation only. The AMIR and CGU fingerprints include
type/format/bits and use schema v4 to prevent reuse of older cached objects.

`ConstString`/`ConstBytes` share immutable owned UTF-8 backing plus a checked
logical offset/length. String views require codepoint boundaries; byte views
may start inside a codepoint, and conversion to a string validates UTF-8.
Equality and canonical identity use visible content, independently of the
original backing or offset. Unicode, empty strings and embedded NUL survive
freezing. Residual byte views intern their static backing in the destination
literal pool and lower through ordinary string/slice AMIR. Mutable backing,
runtime captures, owned `String`, interpolation and arbitrary pointers remain
outside the admitted value model.

### Software float dependency and cost evidence

`arandu_middle` uses the exact `rustc_apfloat = "=0.2.3"` dependency; the lockfile
selects `0.2.3+llvm-462a31f5a5ab`. `arandu_base` stays independent. The selected
crate is a Rust port of LLVM APFloat, with `no_std` plus `alloc`,
`forbid(unsafe_code)`, explicit rounding and no FFI/native LLVM dependency.
Its manifest declares `Apache-2.0 WITH LLVM-exception`; the included
`LICENSE.txt` and `LICENSE-DETAILS.md` document the LLVM relicensing review and
the additional port authors' agreements. Dependency upgrades require reviewing
those notices and the unstable upstream API again. These package properties
support portability; they do not replace native Windows/macOS validation or a
Wasm compiler build.

The reproducible cost probe is
`cargo run --locked -p arandu_middle --example ctfe_float_probe --release`.
It parses five ordinary/extreme decimal literals and executes dependent additions,
100,000 operations each, with `black_box`; host arithmetic is only the measured
baseline and never the evaluator. Five Linux x86-64 release runs on 2026-10-02,
excluding Rust compilation, gave these medians:

| Operation | Host baseline | Software |
| --- | ---: | ---: |
| 100,000 decimal conversions | 3.438 ms | 111.634 ms |
| 100,000 dependent additions | 0.282 ms | 5.287 ms |

The dependency adds measurable cost; its benefit is explicit target-independent
rounding and bit preservation rather than throughput. This microprobe is not a
whole-compiler CPU/RSS benchmark or a CI performance threshold. Focused
regressions cover direct f32 parsing versus double rounding, ties, signed zero,
subnormals, canonical NaNs, target layouts and residual backend encodings.


### Driver and residual integration evidence (2026-10-10)

The `tests/projects/medium/comptime_core` installed corpus exercises global
frozen enums and tables, typed boolean/signed specialization, computed declaration
headers, nested roots, fixed-array iteration and target selection. On Linux
x86-64, the packaged SDK passed archive validation, bootstrap SHA-256 and staged
BLAKE3 verification before installation into an isolated prefix. Its installed
CLI/LSP passed `scripts/smoke_distribution.py`, including JIT, AOT artifact
execution, incremental reuse and T045 after lowering the configured fuel.
The canonical tarball installer now exposes the packaged LSP through `bin`,
as the Windows installer already does.

Runtime `for` lowering now reads its compiler counters through SSA, preserves
return/break/continue predecessors, and treats direct integer range expressions
as bounds rather than indexing a `Range`. Inclusive integer MAX exits before an
increment. Native string indexing loads both ABI words through the shared
bounds-checked address calculation; Wasm obtains fixed-array length from the
type, instead of reading the unrelated local following the array address.
Native/C optimized and unoptimized execution and the 47 Wasmtime surface tests
cover these paths. The single changed AMIR golden now records the defined loop
index and the arguments of its back edge.

The VS Code extension passed compilation, lint, 26 unit tests and both real
Extension Host suites. The project suite applies fuel 1, restarts the server,
observes structured T045, restores the default and checks clean diagnostics.
The Wasm compiler was rebuilt and tested through its JavaScript C-ABI: the
installed corpus compiles to a module whose `main` executes successfully.
The website's local playground artifact was updated from that validated build;
removing custom debug sections preserves executable sections. Its Astro/search
build passes. These are local
integration results, not a claim that a release or public website was deployed.

Shared scratch admission now instantiates nominal generic enum payloads before
checking their lifetime shapes. Closed payloads qualify; nested strings, safe
references, slices and coroutine states remain excluded. Unknown retention,
cyclic aggregate joins and borrowed result backing still require their separate
proofs before any broader reuse or reclamation is enabled. Native Windows/macOS
installed checks are wired to the same corpus in the existing CI matrix.

### Published distribution evidence and remaining boundaries

The [0.1.10 release workflow](https://github.com/arandu-lang/arandu/actions/runs/38779239029)
for commit `7df12f7a36ed872ea6c04287cb4e536525bc5291` completed the native installed
CTFE smoke on Linux, Windows and macOS. Together with the published release,
this supersedes the campaign's earlier statement that publication and the
three-platform installed smoke were wholly pending. It does not turn the
historical Linux-only measurements above into cross-platform benchmarks or
prove all editor cancellation, performance and extended staging cases.

The core campaign is closed as an implementation campaign. Its enduring
contracts are the feature sections above, the shared
[ABI storage boundary](arandu-abi-layout-v0.1.md#residual-aggregate-storage-safety-boundary)
and the following acceptance boundaries; scheduling belongs only to the master
roadmap:

- CT.0 distinguishes the delivered value/staging contract from the still-Draft
  RFC. Broader effects, reflection and item generation require separate accepted
  contracts; type values and third-party attributes are not mandatory 0.2 gates.
- CT.1 keeps target identity and layout explicit, validates configurations at
  driver boundaries and never equates a synthetic layout with supported native
  code generation.
- CT.2/CT.3 preserve the admitted domain, structural frozen identities, bounded
  expansion, local returns and deterministic errors. Executable closures remain
  a separate contract; lambda preparation does not enable their execution.
- CT.4 requires measured interactive workloads and preserved cutoff, clean
  equivalence, error/success transitions and span updates. LSP/client evidence
  must cover UTF-16, cancellation without an edit, stale results, manifestless
  files and shutdown; a native SDK smoke does not certify that entire matrix.
- CT.5 retains a reproducible corpus for dimensions, tables and specialization.
  Record cold and warm builds, irrelevant edits, helpers with equal/different
  values, exceeded budgets, query executions, RSS and expansion size before
  choosing performance thresholds. Broader corpus, playground delivery and
  future release candidates need their own evidence.

The existing cutoff regressions include `arandu_query/tests/ctfe_roots.rs`
(irrelevant edits and equal-value helper edits), `comptime_public.rs` (public
staging and changed budgets), `comptime_if.rs` (imported selection), and
`comptime_aggregates.rs` (imported constants, cycles and target identity).
They are regression coverage to preserve, not evidence that the entire RFC is
accepted or that interactive latency targets have already been measured.

### Controlled workload comparison

The Linux comparison uses commit `68ab0b2c` and this integration, both built
with `cargo build --locked -p arandu_cli` in the dev profile, the same stdlib
and runtime archive, isolated project copies, one warm-up and seven measured
checks. These small workloads do not establish a general performance gain.

| Workload | Check median before / after | Check resident memory at exit before / after | Compiler malloc/calloc calls before / after |
| --- | --- | --- | --- |
| `language_mix` | 28.47 / 26.33 ms | 26,724 / 26,844 KiB | 17,329 / 17,326 |
| Fibonacci CTFE table (external scenario 05) | 32.80 / 33.54 ms | 27,104 / 27,232 KiB | 28,305 / 28,312 |

The independently built executables preserve both stdout SHA-256 and exit
status (`language_mix` intentionally returns 42). Runtime allocator counts
are unchanged: one malloc/calloc and no realloc for `language_mix`; seven
malloc/calloc calls and one realloc for Fibonacci. The latter executes ten
million table reads and takes about 10.5 ms on this host. Allocator counters
are calls, not live allocation counts or leak evidence. Exit resident memory
from `/proc/self/smaps_rollup` is 2,620 / 2,628 KiB and 2,788 / 2,872 KiB,
respectively; these are exit samples, not peak RSS. Child `wait4` runtime peak
RSS includes the harness's pre-exec footprint and is unsuitable for interpreting
these tiny executables. The compiler resident-memory figures above are likewise
exit samples, not peak RSS. These measurements describe the earlier driver
integration, not a fresh benchmark of the subsequent parser and portability
fixes. The temporary raw samples from that run are no longer available; do not
use this historical table as a reproducible benchmark for release promotion.

Pypor, Katu and Ita sources available on this host fail checking against both
the baseline and this stdlib because their API assumptions have diverged.
They are excluded from successful execution/performance claims; migrating
those applications is separate from validating compiler changes.

### Parallel block-argument copies

The residual audit also exposed a backend bug unrelated to frame promotion:
C and Wasm assigned incoming block arguments sequentially. A loop edge
`next(b, a)` could overwrite `a` before reading it for the second parameter.
Cranelift and the CTFE VM already preserve simultaneous SSA edge assignment.
C now snapshots all inputs in an edge-local scope before writing destinations;
Wasm reads all inputs onto its operand stack and writes destinations in reverse
order, preserving both words of fat values. Hand-built cyclic scalar and string
swaps reproduce the old failure (22 instead of 11) and execute correctly in
Cranelift, emitted C and real Wasmtime. This fixes phi value semantics; it does
not relax the lifetime admission for cyclic aggregate backing storage.

## PONTOS DE MELHORIA (O que não está no roadmap)

Diagnostics for bounded staging could present the obligation chain more compactly
in deeply nested templates. This is a presentation refinement, not permission to
weaken the deterministic fuel, expansion or native query-depth ceilings.

## Futuro e Próximos Passos

The master [compiler roadmap](arandu-compiler-roadmap-v0.1.md) owns the execution
order. Remaining storage admission requires proofs for cyclic aggregate backing,
borrowed views, unknown retention and coroutine suspension; the native installed
matrix and S0 gate are separate evidence requirements. These boundaries must not
be reported as completed merely because a source value is admitted by CTFE.
