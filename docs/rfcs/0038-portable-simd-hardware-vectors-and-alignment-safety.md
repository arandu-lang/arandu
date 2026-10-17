# RFC 0038: Vetores SIMD Portáveis de Hardware, Segurança de Alinhamento e Lowering Multi-Backend

- **Número da RFC:** 0038
- **Título:** Vetores SIMD Portáveis de Hardware, Segurança de Alinhamento e Lowering Multi-Backend
- **Autor(es):** Arquiteto da Linguagem Arandu
- **Data de Início:** 2026-10-16
- **Status:** `Draft`
- **Área Principal:** `Frontend` | `Middle-end` | `Backend` | `Stdlib`
- **Escopo:** `std.core.simd`, `arandu_middle::types::ArType::Vector`, alinhamento no `DataLayout`, AMIR vector ops, lowering para Cranelift, C e WebAssembly SIMD (`v128`).

---

## Fronteira de implementação e evidência

Esta RFC estabelece a transição formal da biblioteca de vetores do Arandu, saindo do protótipo de emulação escalar em software ([`stdlib/core/simd.aru`](../../stdlib/core/simd.aru)) para tipos vetoriais nativos de hardware com alinhamento garantido pelo sistema de tipos e lowering direto nos três backends do compilador.

- **Implementado na v0.1.11 (2026-10-16):**
  - Módulo de protótipo [`stdlib/core/simd.aru`](../../stdlib/core/simd.aru) com structs de emulação escalar: `F32x4` (quatro campos `float`) e `U8x16` (dezesseis campos `u8`), operando sequencialmente campo a campo na ALU/FPU tradicional.
  - Documentação e testes funcionais garantindo conformidade semântica de adições, subtrações, produtos e saturações básicas sob emulação.
- **Proposta desta RFC (Alvo para a série 0.1.x / ST.2 e 0.2.x):**
  - Introdução do tipo de primeira classe `ArType::Vector(lane_ty, lanes)` em `arandu_middle`.
  - Restrição estrita a larguras de **128 bits com potência de dois** (`F32x4`, `F64x2`, `I32x4`, `I16x8`, `U8x16`, `I64x2`).
  - Alinhamento forçado e imutável de **16 bytes** no `DataLayout`.
  - Operações vetoriais de primeira classe na AMIR (`VectorOp`, `VectorSplat`, `VectorShuffle`, `VectorExtractLane`, `VectorInsertLane`).
  - Lowering nativo: Cranelift (`types::F32X4`, etc.), C (`__attribute__((vector_size(16)))`) e WebAssembly (`v128`).
  - Tipos dedicados de máscara vetorial (`Mask32x4`, etc.) e operações de redução (`all`, `any`, `select`).

---

## 1. Resumo (Summary)

Esta RFC desenha a arquitetura de SIMD (*Single Instruction, Multiple Data*) do Arandu. Em vez de recorrer a auto-vetorizadores de laço imprevisíveis ou intrinsics proprietários específicos de arquitetura que quebram a portabilidade, o Arandu adota um conjunto fixo e seguro de tipos vetoriais portáveis de 128 bits.

A proposta resolve as três maiores dores históricas do SIMD na indústria:
1. **Eliminação de Falhas de Alinhamento (Bus Error / GPF):** O compilador garante alinhamento estrito de 16 bytes na stack e no heap; cargas de ponteiros genéricos exigem verificação explícita em tempo de compilação ou funções unaligned seguras.
2. **Previsibilidade de Silício:** Cada operação vetorial em código Arandu mapeia para **exatamente uma instrução de máquina** no alvo (SSE2/AVX em x86, NEON em ARM, `v128` em Wasm).
3. **Imunidade ao Impasse de Monomorfização:** Ao limitar o núcleo padrão a 128 bits de potência de dois, evitam-se legalizações ineficientes e explosões de código que paralisaram propostas como o `std::simd` do Rust por quase uma década.

---

## 2. Motivação (Motivation)

A computação moderna em processamento de dados, multimídia, aprendizado de máquina, criptografia (hashing e cifras de bloco) e renderização gráfica depende da execução paralela de instruções vetoriais.

### As Falhas das Abordagens Existentes
* **Intrinsics Proprietários em C/C++ (`<immintrin.h>`, `_mm_add_ps`):**
  - Destroem a portabilidade do código, forçando bases a manterem múltiplos blocos de `#ifdef __SSE2__` e `#ifdef __ARM_NEON`.
  - Falta de segurança de tipos: passar um ponteiro desalinhado para `_mm_load_ps` resulta em *General Protection Fault* (GPF) silencioso ou travamento imediato da aplicação.
* **O Impasse do Rust (`std::simd` / RFC 2938):**
  - Ao tentar criar uma abstração genérica universal `Simd<T, N>` com *const-generics* para qualquer largura arbitrária, o ecossistema esbarrou na explosão combinatória de codegen e nas sutis divergências semânticas de hardware (como tratamento de NaNs e denormais entre x86 e ARM).
* **Legalização Degenerada em Zig (`@Vector(N, T)`):**
  - Ao permitir comprimentos não convencionais como `@Vector(3, f32)`, o compilador gera frequentemente sequências lentas de empacotamento escalar e *stack spills* no backend LLVM.

O Arandu precisa de uma API de SIMD que seja **100% segura em memória, determinística e com zero custo de abstração**.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Tipos Vetoriais Nativos

O módulo `std.core.simd` passa a fornecer tipos de registradores de 128 bits:

```arandu
module app.geometria

import std.core.simd as simd

public func somarPontos(a: simd.F32x4, b: simd.F32x4): simd.F32x4 {
    // Compila para uma única instrução 'addps' (x86), 'fadd' (ARM NEON) ou 'f32x4.add' (Wasm)
    return a + b
}

public func normalizarEscalar(v: simd.F32x4, fator: float): simd.F32x4 {
    // Difunde o escalar para todas as 4 vias em um registrador
    let splat = simd.F32x4.splat(fator)
    return v * splat
}
```

### 3.2 Máscaras e Operações Condicionais

Comparações entre vetores geram uma máscara vetorial (`Mask32x4`), que pode ser combinada logicamente ou usada para seleção vetorial (*blend*):

```arandu
func clamparZero(v: simd.F32x4): simd.F32x4 {
    let zeros = simd.F32x4.splat(0.0)
    let mask = v > zeros // Retorna Mask32x4
    
    // Onde 'mask' for verdadeiro, mantém 'v'; onde falso, escolhe 'zeros'
    return mask.select(v, zeros)
}
```

### 3.3 Cargas de Memória Seguras e Alinhamento

Carregar dados contíguos de fatias para vetores SIMD requer respeitar as garantias de alinhamento:

```arandu
func processarBuffer(dados: []u8): void {
    // Carga não alinhada explícita e segura:
    let bloco = simd.U8x16.loadUnaligned(dados, 0)
    
    // Operações SIMD paralelas de 16 bytes simultâneos...
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Modificações no Sistema de Tipos (`arandu_middle`)

1. **Nova Variante `ArType`:**
   ```rust
   pub enum ArType {
       // ... variantes existentes
       Vector(Box<ArType>, u32), // Tipo da via (primitivo) e número de vias
   }
   ```
2. **Matriz Canônica de Tipos Suportados:**
   - `F32x4`: 4 vias de 32 bits flutuantes (128 bits).
   - `F64x2`: 2 vias de 64 bits flutuantes (128 bits).
   - `I32x4` / `U32x4`: 4 vias inteiras de 32 bits (128 bits).
   - `I16x8` / `U16x8`: 8 vias inteiras de 16 bits (128 bits).
   - `I8x16` / `U8x16`: 16 vias inteiras de 8 bits (128 bits).
   - `I64x2` / `U64x2`: 2 vias inteiras de 64 bits (128 bits).

3. **Alinhamento no `DataLayout`:**
   Para todo tipo `ArType::Vector(_, _)`:
   - `size`: 16 bytes.
   - `align`: 16 bytes obrigatórios.

### 4.2 Nós Vetoriais na AMIR (`arandu_middle::amir`)

Estender `AmirRvalue` para expressar semântica vetorial pura sem descer para chamadas de função C:
```rust
pub enum AmirRvalue {
    // ...
    VectorOp {
        op: VectorOp,
        left: AmirOperand,
        right: AmirOperand,
    },
    VectorSplat {
        value: AmirOperand,
        lanes: u32,
    },
    VectorExtractLane {
        vec: AmirOperand,
        lane: u32,
    },
    VectorInsertLane {
        vec: AmirOperand,
        lane: u32,
        val: AmirOperand,
    },
    VectorShuffle {
        left: AmirOperand,
        right: AmirOperand,
        mask: Vec<u32>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VectorOp {
    Add, Sub, Mul, Div,
    BitAnd, BitOr, BitXor,
    Equal, NotEqual, LessThan, LessEqual, GreaterThan, GreaterEqual,
}
```

### 4.3 Lowering nos Três Backends

#### A. Cranelift Backend (`arandu_backend_cranelift`)
- Mapeamento direto de tipos da AMIR para Cranelift IR:
  - `F32x4` $\rightarrow$ `cranelift_codegen::ir::types::F32X4`
  - `I32x4` $\rightarrow$ `cranelift_codegen::ir::types::I32X4`
  - `U8x16` $\rightarrow$ `cranelift_codegen::ir::types::I8X16`
- O Cranelift aloca registradores físicos `xmm` (x86_64) ou `v` (AArch64) e emite instruções diretas de hardware (`addps`, `fadd`, `paddb`).

#### B. C Backend (`arandu_backend_c`)
- O emissor C declara tipos vetoriais usando extensões vetorizadas padrão da indústria (suportadas por Clang, GCC, ICC e TCC com extensões):
  ```c
  typedef float ar_f32x4 __attribute__((vector_size(16)));
  typedef uint8_t ar_u8x16 __attribute__((vector_size(16)));
  ```
- Para compatibilidade máxima com compiladores C ANSI puros (como MSVC em modo C estrito), o emissor gera shims estruturados com alinhamento forçado `alignas(16)`.

#### C. WebAssembly Backend (`arandu_backend_wasm`)
- Emite o tipo primitivo `v128` (código de tipo `0x7B` na especificação Wasm).
- Instruções de bytecode:
  - `VectorOp::Add` em `F32x4` $\rightarrow$ opcode `0xFD 0xE4` (`f32x4.add`).
  - `VectorSplat` em `F32x4` $\rightarrow$ opcode `0xFD 0x13` (`f32x4.splat`).

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

1. **Preservação de Early-Cutoff:** A introdução do tipo vetorial não altera as assinaturas de hashes de símbolos externos. Mudanças no corpo de funções vetorizadas preservam o cutoff para chamadores.
2. **Ausência de Comportamento Indefinido (No UB):** Ao rejeitar ponteiros genéricos desalinhados em favor de construtores validados de fatias e `loadUnaligned`, o compilador Arandu previne falhas de hardware em tempo de execução.
3. **Desvantagem Consciente:** O Arandu deliberadamente **não suporta** vetores nativos de 256 bits (AVX2) ou 512 bits (AVX-512) no núcleo fundamental desta RFC, reservando larguras escaláveis para a RFC 0031 (LLVM/MLIR). Isso garante paridade absoluta em qualquer celular ARM, máquina x86 de entrada ou ambiente WebAssembly.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

- **Alternativa Rejeitada: Auto-Vetorização Automática de Laços no Cranelift:**
  - O Cranelift é desenhado para compilação rápida em tempo linear $O(N)$ e não possui infraestrutura pesada de análise de dependência de ponteiros para vetorizar laços automaticamente.
  - A abordagem explícita em nível de tipo coloca o poder nas mãos do programador com previsibilidade máxima de desempenho.
- **Alternativa Rejeitada: Const-Generics Arbitrários (Estilo Rust/Zig):**
  - Rejeitada para evitar o custo de monomorfização e a complexidade de legalização no compilador. 128 bits é a interseção universal perfeita de todo hardware moderno.

---

## 7. Arte Prévia (Prior Art)

- **Google Highway (`hwy`)**: Demonstrou que abstrações portáveis com largura fixa de 128 bits entregam até 98% da velocidade de código assembly manuscrito em x86 e ARM.
- **Intel ISPC**: Estabeleceu o paradigma de que abstrações vetoriais explícitas superam auto-vetorizadores de compiladores C em aplicações reais.
- **WebAssembly SIMD (W3C Standard)**: Padronização do registrador `v128` como o alicerce fundamental de computação paralela na Web.

---

## 8. Questões em Aberto (Unresolved Questions)

1. *Sintaxe de Sobrecarga de Operadores:* A sintaxe de operadores (`+`, `-`, `*`) deve ser habilitada nativamente pelo typeck para `ArType::Vector` ou exposta através de métodos inerentes nomeados (`.add()`, `.mul()`) na primeira fase de implementação?
   - **Recomendação:** Métodos inerentes no recorte inicial ST.2; operadores estéticos introduzidos após estabilização da resolução de operadores.
