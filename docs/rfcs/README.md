# Processo de RFCs do Arandu (Request for Comments)

O processo de **RFC (Request for Comments)** do Arandu é o mecanismo formal pelo qual decisões arquiteturais substanciais, novas funcionalidades da linguagem, convenções de sintaxe, contratos de runtime e modelos de memória são propostos, debatidos, validados e estabilizados no ecossistema do compilador.

---

## Ciclo de Vida de uma RFC

Uma RFC evolui através dos seguintes estados formais:

```
  ┌─────────┐      ┌────────────┐      ┌────────────────┐      ┌────────────────┐
  │  Draft  │ ───► │  Accepted  │ ───► │  Implemented  │ ───► │   Frozen-R0    │
  └─────────┘      └────────────┘      └────────────────┘      │ (Experimental) │
       │                                                       └────────────────┘
       ▼                                                               │
  ┌───────────┐                                                        ▼
  │ Rejected  │                                                ┌────────────────┐
  └───────────┘                                                │   Superseded   │
                                                               └────────────────┘
```

1. **`Draft`**: Proposta em discussão aberta. O design detalhado, trade-offs e impactos ainda estão sendo refinados.
2. **`Accepted`**: A proposta foi revisada, está alinhada aos Invariantes de Arquitetura do Arandu e foi aprovada pelos mantenedores para implementação.
3. **`Implemented`**: O recorte explicitamente declarado está implementado nas camadas aplicáveis, com testes e diagnósticos. Não aprova automaticamente as expansões futuras da RFC nem comprova paridade em todos os alvos.
4. **`Planned`**: Investigação ou desenho registrado, ainda sem aprovação/implementação integral; pode ter precursores implementados descritos no texto.
5. **`Frozen-R0`**: O contrato público e semântico foi **congelado cirurgicamente** como baseline de medição empírica (pesquisa acadêmica / TCC). Nenhuma quebra de ABI ou semântica é permitida durante o período de coleta de métricas.
6. **`Superseded`**: Uma RFC posterior substituiu ou reformulou o design anterior.

---

O item 0000 é um template `Living`; o campo `Draft` dentro dele é preenchido
ao criar uma proposta, não o status do próprio catálogo.

## Relação com o produto

A única ordem executiva é o [roadmap mestre](../arandu-compiler-roadmap-v0.1.md#plano-de-longo-prazo-por-série).
Status de governança, maturidade da implementação e publicação são dimensões
separadas. O número da RFC e as fases de seu desenho não definem uma versão.
Tabelas de fases futuras em RFCs são propostas condicionais; a distribuição
suportada é definida pelos contratos e pela evidência de release.

| Grupo | Leitura correta do estado atual |
| --- | --- |
| 0001 | GenRef R0 congelado, com fronteira host/thread-confined |
| 0002–0005, 0007–0010 | recortes integrados; async, borrow e infra conservam limites nos contratos técnicos |
| 0006 | proposta de organização HIR; pools/IndexVec já são precursores implementados, não a arena por função inteira |
| 0011 / 0013 | `Draft` amplas com fundação incremental/núcleo CTFE existentes; motor quente e metaprogramação ampla continuam separados |
| 0012 / 0014–0019 | propostas com bases parciais quando documentadas; não implicam stack científica, mobile, sandbox ou CAS completo |
| 0020–0023 / 0027 | recortes integrados; packaging ainda conserva dívida de bootstrap e Python residual |
| 0024–0026 | propostas com intrínsecos, testes e convenções existentes; não implicam atributos executáveis, remarks ou auditoria integral de recursos |
| 0028–0031 | propostas novas de eBPF, GPU, FFI e LLVM/MLIR; sem backends/exportador universais implementados |
| 0032–0036 | propostas arquiteturais das trilhas `LAMBDA`, `OPT.1–7`, `TYP.NEXT`/`GEN`, `A2` (efeitos/capacidades) e `IDE.5`/`E1, E3, E5` |
| 0037 | Decimal em validação; BigInt/BigDecimal com protótipos locais e provas pendentes no fechamento da stdlib; ainda não publicados |
| 0038 / 0039 | SIMD possui API fonte canônica, máscaras e stores com vista exclusiva; locks nativos têm guardas compartilhadas, fases FIFO e Condvar com guarda; spawn usa ThreadJob/JoinHandle possuídos; prova multiplataforma instalada pendente em ST.2/ST.10/ST.11 |

## Índice Oficial de RFCs

| RFC | Título | Área | Status | Data |
| :---: | :--- | :--- | :---: | :---: |
| [0000](0000-template.md) | Template Padrão de RFCs | Governança | `Living` | 2026-09-11 |
| [0001](0001-generational-fallback-genref.md) | Fallback Geracional com GenRef | Memória / Runtime | `Frozen-R0` | 2026-09-11 |
| [0002](0002-canonical-attribute-naming.md) | Convenção Canônica de Anotações (@PascalCase) | Frontend / Sintaxe | `Implemented` | 2026-09-11 |
| [0003](0003-structured-parallelism.md) | Paralelismo Estruturado e Worker Pool | Runtime / AMIR | `Implemented` | 2026-09-11 |
| [0004](0004-project-package-lifecycle.md) | Ciclo de Vida de Pacotes e Manifesto `Arandu.toml` | Tooling / CLI | `Implemented` | 2026-09-11 |
| [0005](0005-incremental-query-system-salsa.md) | Sistema de Queries Incrementais com Salsa (A1) | Incrementalidade | `Implemented` | 2026-07-05 |
| [0006](0006-hir-indexvec-storage.md) | Armazenamento Achatado de HIR com IndexVec | Middle-end | `Planned` | 2026-06-25 |
| [0007](0007-semantic-memory-model.md) | Modelo Semântico de Memória e Empréstimos | Memória / OSSA | `Implemented` | 2026-08-15 |
| [0008](0008-async-runtime-and-colorless-model.md) | Runtime Assíncrono e Modelo Colorless (SL_R / A3) | Runtime / Async | `Implemented` | 2026-08-20 |
| [0009](0009-borrowed-views-safety.md) | Segurança Estrutural de Fatias e Views Emprestadas | Memória / Stdlib | `Implemented` | 2026-08-01 |
| [0010](0010-cst-resilient-ide-typeck.md) | Pipeline CST-First Resiliente e Typeck Incremental | Frontend / IDE | `Implemented` | 2026-07-20 |
| [0011](0011-incremental-partitioned-aot-and-in-process-linker.md) | Pipeline AOT Incremental, Codegen Particionado e Linker In-Process | Backend / Incremental | `Draft` | 2026-09-12 |
| [0012](0012-scientific-computing-and-data-architecture.md) | Arquitetura da Stack de Computação Científica, Numérica e de Dados | Ecosystem (Out-of-Tree) | `Draft` | 2026-09-12 |
| [0013](0013-deterministic-ctfe-and-comptime-metaprogramming.md) | CTFE determinístico e `comptime` core via AMIR | Frontend / Middle-end / Incrementalidade | `Draft` | 2026-09-12 |
| [0014](0014-native-wasm-component-model-and-runtime.md) | Backend WebAssembly Nativo com Component Model (WIT), Compilação Incremental e Paralelismo Determinístico | Backend | `Draft` | 2026-09-13 |
| [0015](0015-native-mobile-architecture-and-zero-copy-interop.md) | Arquitetura Mobile Nativa, Interoperabilidade Zero-Copy e Pipeline de Bindings Multiplataforma | Backend / Tooling | `Draft` | 2026-09-13 |
| [0016](0016-capability-safe-filesystem-and-path-resolution.md) | Sistema de Arquivos Orientado a Capacidades e Resolução Segura de Caminhos | Stdlib / Runtime / Tooling | `Draft` | 2026-09-19 |
| [0017](0017-lean-freestanding-core-architecture.md) | Arquitetura Fundamental do `arandu_core` — Camada Freestanding e Zero-Heap | Stdlib / Core / Embedded | `Draft` | 2026-09-19 |
| [0018](0018-pretty-idiomatic-c-codegen.md) | Emissor C Idiomático, Estruturado e Legível (*Pretty & Idiomatic C Codegen*) | Backend / Codegen | `Draft` | 2026-09-19 |
| [0019](0019-zero-bloat-target-and-shared-cache.md) | Arquitetura de Cache Global Compartilhado e Prevenção de Inchaço de Build (*Zero-Bloat Target*) | Tooling / Build / Storage | `Draft` | 2026-09-19 |
| [0020](0020-native-packaging-and-python-eradication.md) | Empacotamento Nativo, Validação de Arquivos e Erradicação de Python via `xtask` e CLI | Tooling / Distribution / CI | `Implemented` | 2026-09-20 |
| [0021](0021-visibility-and-module-surface.md) | Visibilidade Granular e Superfície de Módulo (`internal`, `sealed`, re-exports) | Frontend / Middle-end / Tooling | `Implemented` | 2026-09-21 |
| [0022](0022-holistic-synthesis-differential-fuzzing.md) | Síntese Holística de Programas, Testes Diferenciais Multi-Backend e Fuzzing Incremental (`AranduSmith`) | Tooling / Middle-end / Backend | `Implemented` | 2026-09-22 |
| [0023](0023-portable-default-integer-model.md) | Modelo Portável de Inteiros Padrão e Tipos de Largura do Alvo | Frontend / Middle-end / Backend / Stdlib | `Implemented` | 2026-09-24 |
| [0024](0024-unified-attribute-system.md) | Sistema Unificado de Atributos — `@` como mecanismo comptime de primeira classe | Frontend / Middle-end / Stdlib | `Draft` | 2026-10-03 |
| [0025](0025-optimization-oracle-and-testing-aot.md) | Arandu Optimization Oracle & Testing (`AOT`): Diagnóstico Estruturado, Testes de Codegen e Fuzzing Metamórfico | Middle-end / Backend / Tooling | `Draft` | 2026-10-04 |
| [0026](0026-standard-library-naming-and-resource-conventions.md) | Convenções Canônicas de Nomenclatura e Gerenciamento de Recursos da Stdlib | Stdlib / Frontend / Runtime | `Draft` | 2026-10-06 |
| [0027](0027-zero-bloat-binary-diet-and-on-demand-runtime.md) | Redução de Código e Runtime Sob Demanda | Backend / Runtime / Tooling | `Implemented` | 2026-10-12 |
| [0028](0028-native-ebpf-backend-and-cooperative-verification.md) | Backend eBPF Nativo e Verificação Cooperativa de Kernel | Backend / Kernel / Tooling | `Draft` | 2026-10-15 |
| [0029](0029-gpu-heterogeneous-compute-and-spirv-backend.md) | Computação Heterogênea em GPU e Backend SPIR-V | Backend / GPU / Heterogeneous | `Draft` | 2026-10-15 |
| [0030](0030-universal-ffi-zero-copy-and-polyglot-export.md) | FFI Universal, Vistas Zero-Copy e Exportação Poliglota | Frontend / Middle-end / Tooling / ABI | `Draft` | 2026-10-15 |
| [0031](0031-llvm-mlir-backend-and-scoped-noalias-vectorization.md) | Backend LLVM/MLIR, Escopos Noalias e Otimização Extrema de Produção | Backend / Codegen / Optimization | `Draft` | 2026-10-15 |
| [0032](0032-closures-ossa-capture-modes-and-first-class-functions.md) | Closures, Modos de Captura OSSA e Funções de Primeira Classe (`LAMBDA`) | Frontend / Middle-end / Backend | `Draft` | 2026-10-15 |
| [0033](0033-amir-cooperative-optimization-place-provenance-and-memoryssa.md) | Otimização Cooperativa na AMIR, Proveniência de Places (`AmirPlace`) e `MemorySSA` Virtual (`OPT.1–7`) | Middle-end / Optimization | `Draft` | 2026-10-15 |
| [0034](0034-adaptive-hybrid-polymorphism-and-witness-tables.md) | Polimorfismo Híbrido Adaptativo, *Witness Tables* e *Existential Containers* sem Heap Implícito (`TYP.NEXT` / `GEN`) | Frontend / Middle-end / ABI | `Draft` | 2026-10-15 |
| [0035](0035-effect-system-polymorphism-and-capability-authority.md) | Sistema de Efeitos Tridimensional, Polimorfismo Contextual e Autoridade por Capacidades (`A2`) | Frontend / Middle-end / Stdlib | `Draft` | 2026-10-15 |
| [0036](0036-typed-holes-and-integrated-semantic-tooling.md) | *Typed Holes* (`todo`) com Contexto OSSA e Ferramental Semântico Integrado (`REPL`, `Bindgen`, `Lint`) | Frontend / IDE / Tooling | `Draft` | 2026-10-15 |
| [0037](0037-decimal-arithmetic-and-arbitrary-precision-big-numbers.md) | Aritmética Decimal de Valor Fixo (128 bits) e Números de Precisão Arbitrária (`Decimal`, `BigInt`, `BigDecimal`) | Stdlib / Middle-end / Runtime | `Draft` | 2026-10-16 |
| [0038](0038-portable-simd-hardware-vectors-and-alignment-safety.md) | SIMD portável de 128 bits e segurança de alinhamento | Frontend / Middle-end / Backend / Stdlib | `Draft` | 2026-10-16 |
| [0039](0039-parking-lot-and-phase-fair-synchronization.md) | ParkingLot, condições de espera e sincronização por fases | Runtime / Type System / Stdlib / Concurrency | `Draft` | 2026-10-16 |
| [0040](0040-algebraic-formatter-ir-and-cst-resilience.md) | Arquitetura de Formatação Algébrica, Formatter IR (Wadler-Leijen) e Resiliência CST (`arandu_fmt v2`) | Tooling / Frontend | `Draft` | 2026-10-18 |
| [0041](0041-advanced-test-harness-cross-target-and-sarif.md) | Harness de Testes Avançado, Execução Cross-Target (WASI/Wasmtime), Snapshot Testing e Diagnósticos SARIF (`arandu_testing_contract v2`) | Tooling / Testing / CLI | `Draft` | 2026-10-18 |


---

## Como Propor uma Nova RFC

1. Faça uma cópia do arquivo [`0000-template.md`](0000-template.md) nomeando-a temporariamente como `0000-meu-recurso.md`.
2. Preencha todas as seções obrigatórias: **Resumo**, **Motivação**, **Guia**, **Referência**, **Invariantes**, **Alternativas** e **Arte Prévia**.
3. Abra um Pull Request contra a branch principal com o título `rfc: meu recurso`.
4. Após o debate e consenso da equipe, o número oficial definitivo será atribuído e o status passará para `Accepted`.
