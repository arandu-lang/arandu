# RFC 0031: Backend LLVM Opt-In, Escopos Noalias Verificados e Vetorização de Produção (`arandu_backend_llvm`)

- **Número da RFC:** 0031
- **Título:** Backend LLVM Opt-In, Escopos Noalias Verificados e Vetorização de Produção (`arandu_backend_llvm`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-15
- **Status:** `Draft`
- **Área Principal:** `Backend` / `Codegen` / `Optimization` (`arandu_backend_llvm`)
- **Documentos Relacionados:**
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/arandu-backend-contract-v0.1.md`
  - `docs/rfcs/0007-semantic-memory-model.md`
  - `docs/rfcs/0011-incremental-partitioned-aot-and-in-process-linker.md`
  - `docs/rfcs/0025-optimization-oracle-and-testing-aot.md`

> **Estado auditado em 2026-10-20:** `Draft`, com lowering experimental
> **direto** `arandu_backend_llvm` (AMIR → LLVM IR textual). O subconjunto
> atual cobre escalares, ponteiros, vetores SIMD de 128 bits, atômicos,
> estruturas/tuplas/arrays/slices, `StringInterp`/`ToStr` de primitivos e
> GenRef/`GenArena`; parte dos enums, layouts Named incompletos, corrotinas,
> `noalias` e ABI completa ainda não são emitidos. Exposto por `emit-llvm`,
> `run -b llvm` e `build -b llvm` (host/`clang` + runtime empacotado). Não
> fixa versão do LLVM. No [roadmap mestre](../arandu-compiler-roadmap-v0.1.md),
> o tier LLVM permanece na série `0.4.x` e depende de `OPT.1–7` e do oráculo
> AOT (`RFC 0025`). MLIR permanece extensão futura separada.

---

## 1. Resumo (Summary)

Esta RFC especifica a arquitetura de um futuro **Backend LLVM Opt-In (`arandu_backend_llvm`)** para o compilador Arandu, voltado à geração de binários de produção máxima, preservando o **Cranelift** como backend padrão de desenvolvimento diário, JIT e compilação incremental rápida.

A proposta estrutura-se sobre três compromissos de engenharia:
1. **Arquitetura Dual Estrita (Cranelift Default + LLVM Opt-In)**: O compilador padrão continua leve e autossuficiente com Cranelift e C. O `arandu_backend_llvm` entra como crate isolado e opcional para builds de release onde autovetorização SIMD avançada (AVX-512, ARM SVE/NEON), SLP Vectorization, LTO e PGO justificarem o custo adicional de tempo de compilação;
2. **Emissão Conservadora e Verificada de `!alias.scope` e `!noalias`**: Diferente de abordagens ingênuas que marcam blocos inteiros como `noalias`, o backend LLVM do Arandu projeta metadados de não-aliasing **estritamente dentro da janela de vida útil do empréstimo (`F2.2 loan window`)** e somente quando a análise de proveniência (`OPT.3 PlaceAliasAnalysis`) comprovar disjunção;
3. **Validação Diferencial Quádrupla (Cranelift vs. C vs. Wasm vs. LLVM)**: Toda otimização habilitada no backend LLVM deve passar pelo oráculo diferencial do `AranduSmith` (`RFC 0022` / `RFC 0025`), garantindo paridade exata de resultados, traps e efeitos observáveis.

---

## 2. Motivação (Motivation)

### 2.1 Por que Cranelift + LLVM?
* **Cranelift** foi projetado para baixa latência de compilação ($\sim O(N)$ linear nos blocos da função), tornando-o ideal para `arandu run`, `arandu test` e ciclos iterativos sub-100ms (`RFC 0011`). Contudo, o Cranelift deliberadamente omite análises caras como *Loop Vectorizer*, *SLP Vectorizer* (empacotamento de operações escalares adjacentes em registradores SIMD), *Polly* (otimização poliédrica de loops) e *ThinLTO* cross-module.
* **LLVM** possui o pipeline de otimização de máquina mais maduro da indústria para CPUs modernas, mas impõe alto custo de memória e tempo de compilação, além de exigir cuidado extremo com metadados de aliasing.

### 2.2 A Lição Histórica do Rust: Por que `noalias` Mal Delimitado Causa Miscompilations
Entre o Rust 1.8 e o Rust 1.54, o `rustc` enfrentou graves *miscompilations* ao emitir atributos `noalias` para referências `&mut T` no LLVM. Além de bugs históricos no próprio LLVM (resolvidos na infraestrutura de *Scoped NoAlias* do LLVM 12+), há uma armadilha fundamental de compiladores na tradução de *borrow checking* para alias analysis:

> **O Borrow Checker é temporal (por ponto de programa); o Alias Analysis do otimizador é espacial (sobre intervalos de instruções).**

Considere o seguinte código válido dentro de um **mesmo bloco básico** da AMIR:
```arandu
let mut x: i32 = 10
x = 20                  // (1) Escrita direta na base `x` ANTES do empréstimo
let r: mut ref i32 = mut ref x
*r = 30                 // (2) Escrita via empréstimo exclusivo `r` (janela ativa de `r`)
let finalVal = x        // (3) Leitura direta da base `x` APÓS o fim da janela de `r`
```
Se um compilador ingenuamente marcasse `r` com `!alias.scope !dom_r` e marcasse **todas as outras instruções do mesmo bloco básico** (incluindo `(1)` e `(3)`) com `!noalias !dom_r`, o LLVM concluiria que `*r = 30` não tem relação alguma com `x = 20` nem com `let finalVal = x`! Como consequência, o agendador de instruções do LLVM moveria `let finalVal = x` para antes de `*r = 30` (retornando `20` em vez de `30`), gerando uma **miscompilation silenciosa**!

Portanto, conforme estabelecido em `OPT.3` do [roadmap mestre](../arandu-compiler-roadmap-v0.1.md):
* **Nunca** se deve marcar todas as operações de um bloco básico como `!noalias` apenas por existir um `BorrowMut` no bloco;
* Metadados `!noalias` exigem delimitação exata do intervalo de vida útil do empréstimo (`llvm.experimental.noalias.scope.decl`) combinada com prova de proveniência de `AmirPlace`.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Seleção Explícita de Backend e Diagnóstico de Vetorização

O usuário escolhe o backend de acordo com a fase do ciclo de engenharia:

```bash
# Ciclo de desenvolvimento rápido: usa Cranelift (padrão)
$ arandu build

# Build de produção com backend LLVM opt-in (quando compilado com suporte LLVM)
$ arandu build --release --backend=llvm

# Inspeção de relatórios de otimização e vetorização (integração RFC 0025)
$ arandu build --release --backend=llvm --remarks=vectorize
```

### 3.2 Paridade Semântica Estrita
A troca de `--backend=cranelift` para `--backend=llvm` **nunca** altera a semântica da linguagem:
* Aritmética inteira com checagem de overflow ou wrapping respeita as mesmas regras;
* Divisão por zero e `MIN / -1` produzem o mesmo comportamento de trap definido pelo contrato aritmético;
* Layouts de structs (`DataLayout`), alinhamentos e convenções de chamada (`System V AMD64`, `Windows x64`, `AArch64`) são idênticos e governados pelo `arandu_middle`.

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Fronteira do Crate `arandu_backend_llvm`
* `arandu_backend_llvm` consome `AmirProgram` / `AmirFunction` já validados e otimizados pelos passes independentes de alvo da AMIR (`OPT.1–7`).
* O crate é isolado por feature flag no `arandu_cli` / `arandu_codegen`, garantindo que builds de desenvolvimento do próprio compilador Arandu e instalações mínimas não precisem compilar ou linkar contra dezenas de megabytes de bibliotecas C++ do LLVM.

### 4.2 Algoritmo Correto de Emissão de Scoped NoAlias (`F2.2` + `OPT.3`)
Para traduzir a exclusividade de `mut ref` sem introduzir miscompilations:

1. **Parâmetros de Função (`noalias` de Argumento)**:
   Um parâmetro formal `p: mut ref T` (ou fatia `mut ref []T`) cuja análise de assinatura garanta não-escape recebe o atributo `noalias` de parâmetro no LLVM IR, pois sua janela de empréstimo cobre toda a ativação da função callee.
2. **Empréstimos Locais Intra-Função (`llvm.experimental.noalias.scope.decl`)**:
   Um empréstimo local `r = BorrowMut(place)` só recebe escopo de metadados se:
   * Sua janela de empréstimo `L(r)` (`F2.2`) for contínua e bem delimitada por dominância;
   * Na instrução `BorrowMut`, o backend emite a intrínseca `call void @llvm.experimental.noalias.scope.decl(metadata !{!scope_r})`.
   * Dentro do intervalo ativo `inst ∈ L(r)`:
     * Acessos de memória cuja proveniência (`OPT.3`) deriva exclusivamente de `r` recebem `!alias.scope !{!scope_r}`;
     * Acessos de memória executados **dentro do mesmo intervalo temporal `L(r)`** que o `PlaceAliasAnalysis` provar serem disjuntos de `r` recebem `!noalias !{!scope_r}`;
     * Acessos à variável base realizados **antes** de `BorrowMut` ou **após** o término de `L(r)` **jamais** recebem `!noalias !{!scope_r}`.
3. **Fallback Conservador**:
   Na presença de reborrows complexos em loops, ponteiros crus (`unsafe`) ou falta de prova de proveniência estrita em `OPT.3`, o emissor omite os metadados `!noalias` locais, preservando a correção absoluta do código gerado.

### 4.3 Mapeamento de Tipos, ABI e Traps
* **Classificação de ABI**: Reutiliza o classificador de ABI por alvo do Arandu (`BC.5` / `ABI.1`) para garantir que structs passadas em registradores ou em memória (`sret` / `byval`) coincidam bit a bit entre objetos compilados pelo Cranelift, C e LLVM.
* **Traps e Semântica de Abort**: Falhas irrecuperáveis baixam para `@llvm.trap()` / `@llvm.ubsantrap(i8 <code_id>)` com atributo `noreturn nounwind`, eliminando a geração de `.eh_frame` quando o unwinding não for utilizado.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Invariantes Preservados
* **Invariante 1 e 2 (AMIR Canônica e Pureza Salsa)**: O LLVM opera exclusivamente na fase final de codegen (por CGU), consumindo hashes canônicos da `RFC 0011`. Nenhuma estrutura do LLVM vaza para `arandu_middle` ou `arandu_query`.
* **Invariante 5 (Zero-Metadata Runtime)**: Funções Arandu são emitidas como `nounwind` no LLVM IR, dispensando personalidade DWARF C++ (`__gxx_personality_v0`).

### 5.2 Desvantagens e Riscos
* **Peso de Distribuição e Toolchain**: Empacotar o LLVM aumenta significativamente o tamanho do binário ou exige um componente opcional (`arandu toolchain install llvm`).
* **Superfície de Bugs Upstream do LLVM**: Passagens agressivas do LLVM (como *LoopVectorizer* e *GVN*) exigem validação contínua pelo fuzzing diferencial (`AranduSmith`) para detectar eventuais regressões de otimização.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

* **Alternativa A: Implementar autovetorização completa e LTO diretamente na AMIR/Cranelift:**
  * *Por que não substituir o LLVM completamente no curto prazo?* Embora a AMIR implemente otimizações semânticas de alto valor (`OPT.1–7`), replicar décadas de modelos de custo de hardware específicos (tabelas de latência de microarquiteturas Zen 4/5, Golden Cove, Apple M-series, AVX-512 e SVE) existentes no LLVM exigiria um esforço desproporcional.
* **Alternativa B: Tornar o LLVM o único backend do Arandu:**
  * *Rejeitada*: Comprometeria a velocidade de compilação sub-100ms e a leveza da distribuição padrão do Arandu.

---

## 7. Arte Prévia (Prior Art)

* **Rust (`rustc_codegen_llvm` & `rustc_codegen_cranelift`)**: Histórico completo das interações entre `&mut` e LLVM `noalias`, incluindo a introdução da intrínseca `llvm.experimental.noalias.scope.decl` (Jeroen Dobbelaere, LLVM Euro 2020/2021).
* **Zig (Self-Hosted Backends + LLVM Opt-In)**: Demonstrou na prática o valor de manter backends próprios ultrarrápidos para debug/dev e delegar builds de release pesados ao LLVM quando solicitado.
* **Halide & Julia**: Uso de metadados de não-aliasing garantidos pela linguagem para destravar vetorização SIMD determinística no LLVM.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Distribuição do Componente LLVM**: O suporte a LLVM será distribuído como um plugin/binário complementar (`arandu-codegen-llvm` invocado sob demanda) para manter o binário principal `arandu` enxuto?
2. **Versão do LLVM**: Qual versão LTS do LLVM será fixada no contrato `S0` quando o crate for iniciado?

---

## 9. Possibilidades Futuras: Extensão MLIR (Fora do Escopo Inicial)

Uma vez consolidado o tier básico de LLVM IR em `0.4.x+`, uma extensão futura independente poderá investigar a emissão de dialetos **MLIR (*Linalg* / *StableHLO*)** para compilar grafos de tensores da stack científica ([RFC 0012](0012-scientific-computing-and-data-architecture.md)) diretamente para aceleradores de matrizes (TPUs/NPUs). Essa extensão exigirá RFC e provas próprias e não faz parte do escopo mínimo do `arandu_backend_llvm`.

## 10. Implementação experimental inicial (2026-10-19)

`arandu_backend_llvm::emit_llvm_ir` faz lowering **direto** da AMIR validada
para LLVM IR textual (`.ll`) em Rust puro, sem passar pelo backend C. O
comando de inspeção `arandu emit-llvm <arquivo.aru>` expõe esse caminho para
arquivos/projetos no host. Ferramentas externas (`llvm-as`, `lli` ou `clang`)
são necessárias apenas para verificar ou executar o IR produzido, não para
gerá-lo.

O subconjunto implementado cobre tipos escalares, ponteiros opacos, `bool`,
ponto flutuante, estruturas, tuplas, arrays, descritores básicos de slice,
vetores SIMD de 128 bits, aritmética/comparações, `phi`, `load`/`store`,
chamadas diretas, atômicos `seq_cst`, `llvm.trap`, intrínsecos de bits,
`StringInterp`/`ToStr` de primitivos e operações GenRef
(`GenInsert`/`GenGet`/`GenSet`/`GenUpsert`/`GenRemove`, exercitadas também
via `std.alloc.gen_arena`). Parâmetros `ref`/`mut ref` recebem `nonnull` e
definições recebem `nounwind`. Parte dos enums e layouts Named incompletos,
além de corrotinas e metadados de alias, ainda retornam diagnóstico explícito
`ICEGEN001` em vez de gerar IR incorreto.

Os comandos experimentais `arandu run -b llvm` e `arandu build -b llvm`
compilam/ligam o IR com `clang` e a `libarandu_runtime` empacotada (host
apenas), encaminhando argumentos após `--` no `run`. A execução LLVM é
opcional e não substitui o JIT Cranelift padrão. Fuzz diferencial
(`synthesized-llvm` / `synthesized-all`) e o subconjunto EMI LLVM-ready
(interpolação, GenArena, lifecycle de string) cobrem paridade com Cranelift.

Este backend é experimental e **não** satisfaz o tier de produção descrito
nesta RFC: não há seleção de target cruzado, ABI AOT/CGU completa, integração
de remarks/LTO/PGO, compatibilidade entre versões LLVM ou prova de alias. Em
particular, nenhum `!noalias` é emitido. A promoção além desta experiência
depende da escolha explícita de empacotamento/versão do LLVM, dos
pré-requisitos OPT/oráculo da RFC e da expansão do lowering direto com
testes de paridade. MLIR continua fora do escopo.
