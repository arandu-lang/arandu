# Documentação do Arandu

Esta pasta tem uma única fila de planejamento: o [roadmap mestre](arandu-compiler-roadmap-v0.1.md). Os demais documentos distinguem propostas, decisões aceitas, contratos implementados e evidências; não crie checklists paralelos.

## Ponto de entrada por objetivo

| Objetivo | Documento |
| --- | --- |
| Saber o que existe e o que falta | [Base entregue e fila atual](arandu-compiler-roadmap-v0.1.md#fila-de-execução) |
| Planejar 0.2.x, 0.3.x e as séries seguintes até 1.0 | [Plano de longo prazo](arandu-compiler-roadmap-v0.1.md#plano-de-longo-prazo-por-série) |
| Distinguir proposta, implementação e suporte publicado | [Estados e versões](arandu-compiler-roadmap-v0.1.md#como-ler-estados-e-versões) e [processo de RFCs](rfcs/README.md) |
| Confirmar uma release específica | [0.1.10](releases/0.1.10.md), [verificação](release-verification.md) e [distribuição](arandu-distribution-contract-v0.1.md) |
| Alterar o compilador | [AGENTS.md](../AGENTS.md), contrato do subsistema e regressões do crate proprietário |

Os sufixos `v0.1` nos nomes são históricos, não limites da versão que um
contrato pode descrever. Exemplos conceituais não especificam a representação
Rust física nem habilitam uma API ausente. Uma RFC `Draft` não é funcionalidade
suportada; uma prova de layout não publica um SDK para esse alvo.

## Taxonomia

| Tipo | Finalidade | Pode conter fila de trabalho? |
| --- | --- | --- |
| roadmap | ordem executiva e estado de maturidade | somente o roadmap mestre |
| rfc | proposta arquitetural ou de linguagem (docs/rfcs/) | apenas pendências e futuro do recurso |
| contrato | comportamento público/normativo implementado | não |
| arquitetura | fronteiras, invariantes e ownership | apenas dívida/futuro explícitos |
| decisão concluída | motivação e escolha estabilizada | não |
| diagnóstico | explicação e correção de um `DiagCode` | não |
| release | evidência de candidata, promoção ou versão publicada | não |

Uma campanha pode criar temporariamente um plano de pesquisa e implementação.
Planos ativos vivem em `docs/campaigns/`, fora do catálogo permanente validado
por taxonomia.
Ao terminar, seu conteúdo útil é consolidado no contrato/arquitetura permanente,
dívidas e futuro são classificados, e o plano paralelo é removido.

## Planejamento, RFCs e decisões

- [Processo e Índice de RFCs](rfcs/README.md) — governança formal de propostas (`docs/rfcs/`).
- [Roadmap mestre](arandu-compiler-roadmap-v0.1.md) — base entregue, fila atual, dependências e gates por série até 1.0.
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
- [RFC 0013: CTFE determinístico e `comptime` core](rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md) — proposta ampla com núcleo CTFE/AMIR já integrado; reflexão e geração de código têm contratos futuros próprios.
- [RFC 0014: WebAssembly e Component Model](rfcs/0014-native-wasm-component-model-and-runtime.md) — backend Wasm, WIT e runtime.
- [RFC 0015: Mobile e interoperabilidade](rfcs/0015-native-mobile-architecture-and-zero-copy-interop.md) — bindings e arquitetura mobile nativa.
- [RFC 0016: Filesystem por capacidades](rfcs/0016-capability-safe-filesystem-and-path-resolution.md) — acesso a arquivos e resolução segura de caminhos.
- [RFC 0017: Core freestanding](rfcs/0017-lean-freestanding-core-architecture.md) — camada fundamental sem heap.
- [RFC 0018: Emissão C idiomática](rfcs/0018-pretty-idiomatic-c-codegen.md) — legibilidade e estrutura do C gerado.
- [RFC 0019: Cache compartilhado](rfcs/0019-zero-bloat-target-and-shared-cache.md) — armazenamento de builds e prevenção de inchaço do target.
- [RFC 0020: Empacotamento nativo](rfcs/0020-native-packaging-and-python-eradication.md) — integridade de arquivos e automação via xtask/CLI.
- [RFC 0021: Visibilidade e módulos](rfcs/0021-visibility-and-module-surface.md) — internal, sealed e re-exports.
- [RFC 0022: AranduSmith](rfcs/0022-holistic-synthesis-differential-fuzzing.md) — síntese e testes diferenciais entre backends.
- [RFC 0023: Inteiros portáveis](rfcs/0023-portable-default-integer-model.md) — inteiros padrão e larguras dependentes do alvo.
- [RFC 0024: Atributos unificados](rfcs/0024-unified-attribute-system.md) — mecanismo comptime de atributos.
- [RFC 0025: Oráculo de otimização](rfcs/0025-optimization-oracle-and-testing-aot.md) — diagnósticos de codegen e testes metamórficos.
- [RFC 0026: Convenções da stdlib](rfcs/0026-standard-library-naming-and-resource-conventions.md) — nomenclatura e gerenciamento de recursos.
- [RFC 0027: Redução de código](rfcs/0027-zero-bloat-binary-diet-and-on-demand-runtime.md) — perfil de tamanho e runtime sob demanda.
- [RFC 0028: eBPF](rfcs/0028-native-ebpf-backend-and-cooperative-verification.md) — proposta de backend e verificação cooperativa, sem implementação.
- [RFC 0029: GPU/SPIR-V](rfcs/0029-gpu-heterogeneous-compute-and-spirv-backend.md) — proposta de computação heterogênea, com provas próprias de memória/concorrência.
- [RFC 0030: FFI poliglota](rfcs/0030-universal-ffi-zero-copy-and-polyglot-export.md) — proposta de exportação e recursos estrangeiros, dependente de ABI/lifetimes.
- [RFC 0031: LLVM/MLIR](rfcs/0031-llvm-mlir-backend-and-scoped-noalias-vectorization.md) — proposta de tier de produção opcional; LLVM e MLIR não são backends existentes.
- [RFC 0032: Closures e capturas OSSA](rfcs/0032-closures-ossa-capture-modes-and-first-class-functions.md) — proposta de ambientes/capturas e funções de primeira classe; execução de closures ainda não suportada.
- [RFC 0033: Otimização cooperativa AMIR](rfcs/0033-amir-cooperative-optimization-place-provenance-and-memoryssa.md) — proposta técnica para OPT.1–7, alias/proveniência e MemorySSA; passes existentes conservam seu recorte.
- [RFC 0034: Polimorfismo híbrido e witness tables](rfcs/0034-adaptive-hybrid-polymorphism-and-witness-tables.md) — proposta de existenciais e compartilhamento adaptativo; monomorfização atual não a implementa inteira.
- [RFC 0035: Efeitos e autoridade](rfcs/0035-effect-system-polymorphism-and-capability-authority.md) — proposta de inferência ampliada, polimorfismo e capabilities sobre a base atual de flags.
- [RFC 0036: Typed holes e tooling](rfcs/0036-typed-holes-and-integrated-semantic-tooling.md) — proposta distinta de recovery CST, com gates próprios para holes e ferramentas futuras.
- [Guia de migração](arandu-project-package-migration-v0.1.md) — uso do contrato de projeto e dependências.
- [Testes e benchmarks](arandu-testing-benchmark-harness-v0.1.md) — contrato consolidado.
- [Contrato de ferramentas e scripts](tooling-scripts-contract.md) — dono e plataforma de cada automação.
- [Auditoria de arquitetura e performance](arandu-architecture-audit-v0.1.md) — achados, guardrails e dívida classificada.
- [Contrato de texto](repository-text-contract.md) — UTF-8/LF em Git, editores e CI.

- [RFC 0037: Decimal e precisão arbitrária](rfcs/0037-decimal-arithmetic-and-arbitrary-precision-big-numbers.md) — Decimal local com precisão finita; protótipos BigInt/BigDecimal locais com validação ainda pendente.

## Contratos de arquitetura

| Área | Documentos |
| --- | --- |
| Frontend | [lexer](arandu-lexer-v0.1.md), [parser](arandu-parser-v0.1.md), [AST](arandu-ast-v0.1.md) |
| IR e execução | [AHIR](arandu-ahir-v0.1.md), [AMIR](arandu-amir-v0.1.md), [IR/SSA](arandu-ir-architecture-v0.1.md), [backends](arandu-backend-contract-v0.1.md) |
| Compile-time | [núcleo público de comptime](arandu-comptime-core-v0.1.md) |
| ABI e memória | [modelo semântico de memória](arandu-semantic-memory-model-v0.1.md), [ABI/layout](arandu-abi-layout-v0.1.md), [JIT/memória](arandu-jit-memory-v0.1.md), [stdlib](arandu-stdlib-architecture-v0.1.md) |
| Incrementalidade e IDE | [Salsa/LSP](arandu-salsa-lsp-architecture-v0.1.md), [LSP/editor](arandu-lsp-capabilities-v0.1.md), [CLI/LSP](arandu-cli-lsp-contract-v0.1.md) |
| Efeitos e autoridade | [contrato e desenho A2](arandu-effects-v0.1.md) |
| Runtime e distribuição | [async runtime](arandu-async-runtime-design-v0.1.md), [paralelismo estruturado](arandu-structured-parallelism-v0.1.md), [instrumentação](arandu-compiler-instrumentation-v0.1.md), [distribuição](arandu-distribution-contract-v0.1.md) |

## Diagnósticos e releases

- [Especificação de diagnósticos](diagnostics/SPEC.md) e [catálogo por código](errors/).
- [Notas de release](releases/) registram candidatas, falhas de promoção e provas
  das versões publicadas; a existência de uma tag não comprova distribuição validada.
- [OSSÀ virtual anchoring](ossa-virtual-anchoring.md) registra a decisão de estabilidade de IDs.

## Regras de manutenção

1. Uma decisão tem uma única fonte normativa.
2. Pesquisa concluída vira decisão, risco ou teste; notas temporárias são removidas.
3. Plano concluído é resumido no roadmap mestre e deixa de ser fila de trabalho.
4. Contratos declaram status, escopo e invariantes; exemplos longos ficam nos testes.
5. Antes do merge, valide links relativos, estados/versões, `xtask check-docs-taxonomy`, `xtask check-line-endings` e `xtask check-diag-docs`.
6. Ao mudar um estado, informe recorte, lacuna, evidência e data; não promova a RFC inteira porque uma parte passou.
7. Um relatório de release é específico do commit/artefato. Não copie checkboxes pendentes de uma candidata anterior como estado atual do projeto.
8. Preserve intenções de pesquisa com rótulo de proposta e dependências; não mantenha garantias de performance, segurança ou suporte sem demonstração.
