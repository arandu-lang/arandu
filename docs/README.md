# Documentação do Arandu

Esta pasta tem uma única fila de planejamento: o [roadmap mestre](arandu-compiler-roadmap-v0.1.md). Os demais documentos são decisões aceitas, contratos estáveis ou evidências; não crie checklists paralelos.

## Taxonomia

| Tipo | Finalidade | Pode conter fila de trabalho? |
| --- | --- | --- |
| roadmap | ordem executiva e estado de maturidade | somente o roadmap mestre |
| rfc | proposta arquitetural ou de linguagem (docs/rfcs/) | apenas pendências e futuro do recurso |
| contrato | comportamento público/normativo implementado | não |
| arquitetura | fronteiras, invariantes e ownership | apenas dívida/futuro explícitos |
| decisão concluída | motivação e escolha estabilizada | não |
| diagnóstico | explicação e correção de um `DiagCode` | não |
| release | evidência imutável de versão publicada | não |

Uma campanha pode criar temporariamente um plano de pesquisa e implementação.
Planos ativos vivem em `docs/campaigns/`, fora do catálogo permanente validado
por taxonomia.
Ao terminar, seu conteúdo útil é consolidado no contrato/arquitetura permanente,
dívidas e futuro são classificados, e o plano paralelo é removido.

## Planejamento, RFCs e decisões

- [Processo e Índice de RFCs](rfcs/README.md) — governança formal de propostas (`docs/rfcs/`).
- [Roadmap mestre](arandu-compiler-roadmap-v0.1.md) — fases abertas e decisões Gold consolidadas.
- [RFC 0001: GenRef](rfcs/0001-generational-fallback-genref.md) — fallback geracional e fronteira congelada do R0.
- [RFC 0002: Nomes de anotações](rfcs/0002-canonical-attribute-naming.md) — decisão @PascalCase canônica e migração.
- [RFC 0003: Paralelismo estruturado](rfcs/0003-structured-parallelism.md) — modelo de concorrência estruturada e worker pool.
- [RFC 0004: Ciclo de vida de pacotes](rfcs/0004-project-package-lifecycle.md) — contrato detalhado de projetos e manifesto.
- [RFC 0005: Queries incrementais Salsa](rfcs/0005-incremental-query-system-salsa.md) — motor incremental orientado a demanda e o 6º invariante.
- [RFC 0006: Armazenamento HIR IndexVec](rfcs/0006-hir-indexvec-storage.md) — armazenamento plano contíguo por função para compilação inteira.
- [RFC 0007: Modelo semântico de memória](rfcs/0007-semantic-memory-model.md) — OSSA, janelas de vida e interfaces de empréstimo.
- [RFC 0008: Runtime assíncrono](rfcs/0008-async-runtime-and-colorless-model.md) — modelo colorless e reator cooperativo.
- [RFC 0009: Fatias e views emprestadas](rfcs/0009-borrowed-views-safety.md) — segurança estrutural de slices ([]T) e views.
- [RFC 0010: Pipeline CST-first e IDE](rfcs/0010-cst-resilient-ide-typeck.md) — parsing resiliente com Rowan e reparse de sub-árvore.
- [RFC 0011: Pipeline AOT incremental e linker in-process](rfcs/0011-incremental-partitioned-aot-and-in-process-linker.md) — codegen particionado e link in-process.
- [RFC 0012: Stack de computação científica e dados](rfcs/0012-scientific-computing-and-data-architecture.md) — tensores, Arrow, lazy query engine e GraphBLAS.
- [RFC 0013: CTFE determinístico e `comptime` core](rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md) — proposta de execução limitada sobre AMIR; reflexão ampla e geração de código ficam para etapas futuras.
- [Guia de migração](arandu-project-package-migration-v0.1.md) — uso do contrato de projeto e dependências.
- [Testes e benchmarks](arandu-testing-benchmark-harness-v0.1.md) — contrato consolidado.
- [Contrato de ferramentas e scripts](tooling-scripts-contract.md) — dono e plataforma de cada automação.
- [Auditoria de arquitetura e performance](arandu-architecture-audit-v0.1.md) — achados, guardrails e dívida classificada.
- [Contrato de texto](repository-text-contract.md) — UTF-8/LF em Git, editores e CI.

## Contratos de arquitetura

| Área | Documentos |
| --- | --- |
| Frontend | [lexer](arandu-lexer-v0.1.md), [parser](arandu-parser-v0.1.md), [AST](arandu-ast-v0.1.md) |
| IR e execução | [AHIR](arandu-ahir-v0.1.md), [AMIR](arandu-amir-v0.1.md), [IR/SSA](arandu-ir-architecture-v0.1.md), [backends](arandu-backend-contract-v0.1.md) |
| Compile-time | [núcleo público de comptime](arandu-comptime-core-v0.1.md) |
| ABI e memória | [modelo semântico de memória](arandu-semantic-memory-model-v0.1.md), [ABI/layout](arandu-abi-layout-v0.1.md), [JIT/memória](arandu-jit-memory-v0.1.md), [stdlib](arandu-stdlib-architecture-v0.1.md) |
| Incrementalidade e IDE | [Salsa/LSP](arandu-salsa-lsp-architecture-v0.1.md), [LSP/editor](arandu-lsp-capabilities-v0.1.md), [CLI/LSP](arandu-cli-lsp-contract-v0.1.md) |
| Runtime e distribuição | [async runtime](arandu-async-runtime-design-v0.1.md), [paralelismo estruturado](arandu-structured-parallelism-v0.1.md), [instrumentação](arandu-compiler-instrumentation-v0.1.md), [distribuição](arandu-distribution-contract-v0.1.md) |

## Diagnósticos e releases

- [Especificação de diagnósticos](diagnostics/SPEC.md) e [catálogo por código](errors/).
- [Notas de release](releases/) são evidências imutáveis das versões publicadas.
- [OSSÀ virtual anchoring](ossa-virtual-anchoring.md) registra a decisão de estabilidade de IDs.

## Regras de manutenção

1. Uma decisão tem uma única fonte normativa.
2. Pesquisa concluída vira decisão, risco ou teste; notas temporárias são removidas.
3. Plano concluído é resumido no roadmap mestre e deixa de ser fila de trabalho.
4. Contratos declaram status, escopo e invariantes; exemplos longos ficam nos testes.
5. Antes do merge, valide links relativos e execute `xtask check-diag-docs`.
