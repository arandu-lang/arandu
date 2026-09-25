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
3. **`Implemented`**: O recurso foi completamente implementado no frontend/middle-end/backends, acompanhado por testes e documentação de diagnósticos.
4. **`Frozen-R0`**: O contrato público e semântico foi **congelado cirurgicamente** como baseline de medição empírica (pesquisa acadêmica / TCC). Nenhuma quebra de ABI ou semântica é permitida durante o período de coleta de métricas.
5. **`Superseded`**: Uma RFC posterior substituiu ou reformulou o design anterior.

---

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
| [0013](0013-deterministic-ctfe-and-comptime-metaprogramming.md) | Metaprogramação Determinística em Tempo de Compilação (CTFE & Comptime) via AMIR VM | Frontend / Middle-end | `Draft` | 2026-09-12 |
| [0014](0014-native-wasm-component-model-and-runtime.md) | Backend WebAssembly Nativo com Component Model (WIT), Compilação Incremental e Paralelismo Determinístico | Backend | `Draft` | 2026-09-13 |
| [0015](0015-native-mobile-architecture-and-zero-copy-interop.md) | Arquitetura Mobile Nativa, Interoperabilidade Zero-Copy e Pipeline de Bindings Multiplataforma | Backend / Tooling | `Draft` | 2026-09-13 |
| [0016](0016-capability-safe-filesystem-and-path-resolution.md) | Sistema de Arquivos Orientado a Capacidades e Resolução Segura de Caminhos | Stdlib / Runtime / Tooling | `Draft` | 2026-09-19 |
| [0017](0017-lean-freestanding-core-architecture.md) | Arquitetura Fundamental do `arandu_core` — Camada Freestanding e Zero-Heap | Stdlib / Core / Embedded | `Draft` | 2026-09-19 |
| [0018](0018-pretty-idiomatic-c-codegen.md) | Emissor C Idiomático, Estruturado e Legível (*Pretty & Idiomatic C Codegen*) | Backend / Codegen | `Draft` | 2026-09-19 |
| [0019](0019-zero-bloat-target-and-shared-cache.md) | Arquitetura de Cache Global Compartilhado e Prevenção de Inchaço de Build (*Zero-Bloat Target*) | Tooling / Build / Storage | `Draft` | 2026-09-19 |
| [0020](0020-native-packaging-and-python-eradication.md) | Empacotamento Nativo, Validação de Arquivos e Erradicação de Python via `xtask` e CLI | Tooling / Distribution / CI | `Draft` | 2026-09-20 |
| [0021](0021-visibility-and-module-surface.md) | Visibilidade Granular e Superfície de Módulo (`internal`, `sealed`, re-exports) | Frontend / Middle-end / Tooling | `Draft` | 2026-09-21 |
| [0022](0022-holistic-synthesis-differential-fuzzing.md) | Síntese Holística de Programas, Testes Diferenciais Multi-Backend e Fuzzing Incremental (`AranduSmith`) | Tooling / Middle-end / Backend | `Draft` | 2026-09-22 |
| [0023](0023-portable-default-integer-model.md) | Modelo Portável de Inteiros Padrão e Tipos de Largura do Alvo | Frontend / Middle-end / Backend / Stdlib | `Draft` | 2026-09-24 |

---

## Como Propor uma Nova RFC

1. Faça uma cópia do arquivo [`0000-template.md`](0000-template.md) nomeando-a temporariamente como `0000-meu-recurso.md`.
2. Preencha todas as seções obrigatórias: **Resumo**, **Motivação**, **Guia**, **Referência**, **Invariantes**, **Alternativas** e **Arte Prévia**.
3. Abra um Pull Request contra a branch principal com o título `rfc: meu recurso`.
4. Após o debate e consenso da equipe, o número oficial definitivo será atribuído e o status passará para `Accepted`.
