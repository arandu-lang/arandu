# Arandu Mid-level Intermediate Representation (AMIR) v0.1

> **Roadmap executivo:** [arandu-compiler-roadmap-v0.1.md](./arandu-compiler-roadmap-v0.1.md)

AMIR v0.1 is a Control Flow Graph (CFG) representation of the program. It flattens the high-level syntax structures (like nesting and structured control flow) into linear lists of instructions grouped in Basic Blocks and linked by jumps.

## Visão Geral e Contexto

AMIR é o CFG SSA/OSSA compartilhado pelas análises de ownership, otimizações
e backends, sem depender de detalhes de host ou frontend.

## Detalhes Técnicos da Implementação

### Core Concepts

1. **Locals and Temporary Registers**:
   - All function parameters, local variables, and intermediate expression values are represented as numbered registers: `_0`, `_1`, `_2`, etc.
   - By convention:
     - `_0` is the register designated for the function's return value.
     - `_1` to `_N` are registers designated for the function's incoming parameters in order.
     - `_N+1` onwards represent local variables and intermediate evaluation temporaries.
   - Every register has a concrete `ArType`.

2. **Basic Blocks**:
   - A basic block (`bb0`, `bb1`, etc.) is a straight-line sequence of statements that executes from start to end without branching.
   - Every basic block ends with a single **Terminator** instruction.

3. **Terminators**:
   - `Return`: Exits the function, passing the value in the return register `_0`.
   - `Goto(bbX)`: Unconditionally transfers control to basic block `bbX`.
   - `Branch { condition, if_true: bbX, if_false: bbY }`: Boolean conditional branch; used for `if`, `while`, and C-style `for` conditions.
   - `SwitchInt { discriminant, targets: [(value, bbX), ...], otherwise: bbY }`: Integer discriminant switch used for `match` on `int` literals, enum unit variants (via `discriminant`), `if`/`while` boolean conditions (`1 => true`), and hybrid fallbacks (range/guard arms chain on `otherwise`).
   - `Suspend { resume, args, ... }`: suspension frontier with live values and resume block.
   - `Unreachable`: Denotes code that cannot be executed.

Goto/Branch/SwitchInt edges carry arguments matching destination block parameters. These logical examples omit the physical dense statement table and some arguments; canonical fixtures and `arandu_middle::amir` define the current schema.

4. **Statements and Rvalues**:
   - `Assign(lhs, rvalue)`: Evaluates an rvalue and assigns it to a register.
   - `Call(lhs, callee, args)`: Invokes a function or callable, writing the result to `lhs` (if any).

### Textual Pretty-Printing Contract

AMIR outputs conform to a deterministic, indented format:

1. Two spaces (`  `) are used for code blocks.
2. The `locals` section lists all registers defined in the function with their types and names (if they correspond to a source variable).
3. Basic blocks are printed as `bbX:` followed by statements and a terminator.

### Example: Basic Addition

Source:

```arandu
func add(a: int, b: int): int {
    return a + b
}
```

AMIR Output:

```text
Func add(a: int, b: int) -> int
  locals:
    _0: int // return
    _1: int // a
    _2: int // b

  bb0:
    _0 = add _1, _2
    return
```

### Example: Branching (If/Else)

Source:

```arandu
func test(x: int): int {
    if x > 5 {
        return 10
    } else {
        return 20
    }
}
```

AMIR Output:

```text
Func test(x: int) -> int
  locals:
    _0: int // return
    _1: int // x
    _2: bool // temp for comparison

  bb0:
    _2 = gt _1, 5
    switchInt _2 { 1 => bb1, otherwise => bb2 }

  bb1:
    _0 = 10
    return

  bb2:
    _0 = 20
    return
```

### Example: While Loop

Source:

```arandu
func test() {
    idx = 0
    while idx < 5 {
        set idx = idx + 1
    }
}
```

AMIR Output:

```text
Func test() -> void
  locals:
    _0: void // return
    _1: int // idx
    _2: bool // temp for comparison
    _3: int // temp for add

  bb0:
    _1 = 0
    goto bb1

  bb1:
    _2 = lt _1, 5
    switchInt _2 { 1 => bb2, otherwise => bb3 }

  bb2:
    _3 = add _1, 1
    _1 = _3
    goto bb1

  bb3:
    return
```

### Invariantes formais (v0.1)

Todo pass que lê ou escreve AMIR deve preservar estas regras. O validador `validate_amir_program` em `crates/arandu_middle/src/amir_validate.rs` aplica CFG-1…CFG-5 e TYP-1 nos goldens `tests/amir/*`. HIR usa `validate_invariants` separadamente.

### CFG

| ID | Invariante |
|----|------------|
| CFG-1 | Todo basic block termina com exatamente um **terminador** (`Return`, `Goto`, `Branch`, `SwitchInt`, `Suspend`, `Unreachable`). |
| CFG-2 | Nenhuma instrução aparece **após** o terminador no mesmo bloco. |
| CFG-3 | Todo `Goto` / `Branch` / `SwitchInt` / `Suspend.resume` aponta para um `bb` existente na mesma função. |
| CFG-4 | `bb0` é o único ponto de entrada; blocos órfãos são erro de lowering (ou `Unreachable` explícito). |
| CFG-5 | Blocos alcançáveis a partir de `bb0` (exceto código morto marcado `Unreachable`). |

### SSA (implementado)

| ID | Invariante |
|----|------------|
| SSA-1 | Cada local `_N` tem tipo `ArType` concreto (não `Error`) após type check. |
| SSA-2 | **Definições dominam usos**; merges usam parâmetros de bloco e argumentos de aresta já implementados, incluindo backedges e atribuição simultânea. Isso não prova lifetime de todo backing agregado. |
| SSA-3 | Parâmetros `_1.._N` e retorno `_0` alocados antes do corpo; nomes de símbolo opcionais na seção `locals`. |

### Ownership / OSSA (a partir de F1)

| ID | Invariante |
|----|------------|
| OSSA-1 | `move dst ← src` invalida `src` para usos posteriores no mesmo caminho (checado por move checker). |
| OSSA-2 | `destroy` só em valores **own** no fim de escopo ou após move não propagado. |
| OSSA-3 | `copy` só em tipos **Copy** (primitivos, `ref` compartilhado — lista no plano estratégico). |
| OSSA-4 | Instruções OSSA não aparecem no AHIR — só no AMIR. |

### Tipagem

| ID | Invariante |
|----|------------|
| TYP-1 | Todo operando de rvalue tem tipo compatível com a operação (checker já validou; AMIR não re-inventa tipos). |
| TYP-2 | `Result` / `Option` no AMIR usam representação acordada (`ResultCtor` ou layout ok/err interno) — tipagem fonte só `Result<T,E>` e `Option<T>`. |

### Efeitos e suspensão

| ID | Invariante |
|----|------------|
| EFF-1 | Efeitos são inferidos/validados pela análise semântica; suspensão é explícita no CFG. Os nomes de flags deste esquema conceitual não prometem campos Rust ou ABI públicos. |

Violações devem falhar no validador de IR ou no pass que as introduz, não no backend C.

---

### Lowering from AHIR to AMIR

The AMIR lowering compiler pass performs the following steps:

1. Pre-allocates parameters (`_1.._N`) and maps their symbol IDs in a translation context.
2. Creates an initial basic block `bb0`.
3. Processes declarations and statements sequentially.
4. For expressions:
   - Constants and direct references to variables are treated as simple operands (constants or copies of registers).
   - Nested operators (e.g. `a + b * c`) are flattened: inner operators are evaluated into temporary registers, and these temporaries are used as operands in subsequent statements.
5. For structured statements:
   - `If`: Allocates the conditional evaluation block, branches into a `then` block and an `else` block (each ending with a jump to a joint `bb_exit` block).
   
- `While`: Jumps to a condition evaluation block, which checks the condition and jumps either to the loop body block or the loop exit block. The loop body block ends with an unconditional jump back to the condition block.

### OSSA (Ownership Static Single Assignment) Construction & Virtual Anchoring

In Arandu, local variable tracking (definite initialization and ownership/move checking) is performed on the completed AMIR CFG as separate compiler passes. However, to generate optimal machine code, variables are promoted to pure Static Single Assignment (SSA) form using **Braun's algorithm**.

Directly promoting to SSA during lowering would eliminate all `Load` and `Store` statements of simple local variables, leaving the move and initialization checkers with no statements to inspect (making use-before-init and use-after-move undetectable). 

To solve this, Arandu implements the **Virtual Anchoring & Pruning** design pattern:

### 1. The Virtual Anchor Pattern

During HIR-to-AMIR lowering:
1. **SSABuilder (Braun's algorithm)** is used to construct pure SSA values and parameter blocks (phi nodes) on-the-fly.
2. Every source-level write of a simple local variable (via `write_variable_source`) updates the SSA builder's `current_def` map and *additionally* emits a dummy `Store` statement:
   `Store(local, value)`
3. Every source-level read of a simple local variable (via `read_variable_source`) queries the SSA builder for the active SSA value (`val`). It then generates a new virtual temporary (`temp`) and emits a dummy `Load` statement:
   `temp = Load(local)`
   And registers a redirection: `redirected_temps.insert(temp, val)`.
   Subsequent statements in the block refer to `temp`.

### 2. Validation Stage

The compiler runs all verification passes (`check_definite_init`, `check_moves`, and `validate_amir_func`) on the **raw AMIR**, which still contains all virtual `Store` and `Load` anchors. 
- The move checker can trace the flow of values and state modifications (e.g. `Available` -> `Moved`) through the dummy statements.
- The definite initialization checker uses the virtual `Store` instructions to compute block-level initialization facts.

### 3. Operand Rewriting & Pruning

After all validations succeed with no errors:
1. **Operand Rewriting**: `rewrite_all_operands()` recursively replaces all virtual temporaries in statements and block parameters with their final, resolved SSA values from the redirection map.
2. **Poda (Pruning)**: `prune_dummy_loads_stores()` sweeps the entire function CFG and deletes all virtual `Store` and `Load` statements of simple local variables (those with empty projections).
3. The remaining AMIR contains pure, optimal SSA code with zero redundant copies, ready to be translated by the Cranelift backend.

## PONTOS DE MELHORIA (O que não está no roadmap)

Cada nova variante exige atualizar visitors, liveness, DCE, move checker e
todos os backends. O texto não implica que toda otimização possível já exista.

## Futuro e Próximos Passos

Manter validação de dominância, argumentos de bloco e efeitos antes/depois de
passes; novas otimizações exigem corpus e medição.
