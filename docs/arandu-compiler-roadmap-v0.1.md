# Arandu Compiler Architecture — Master Roadmap (v0.1 → v0.4)

> **Fonte única de planejamento.** Itens marcados como concluídos abaixo são
> decisões consolidadas, não tarefas pendentes. Os documentos técnicos ligados
> aqui preservam apenas contratos necessários para implementar e validar essas
> decisões.

## Decisões consolidadas (Gold v0.1)

- **Projeto e pacotes:** `arandu.toml` + `arandu.lock`, TOML versionado,
  identidade por pacote/alvo/módulo, imports por alias explícito e grafo DAG
  determinístico. Dependências remotas ficam presas a origem HTTPS/Git e
  commit exato; não há branches flutuantes, registries ou scripts de build.
- **Integridade:** lockfile canônico, hashes de conteúdo, cache imutável por
  digest, publicação staging/rename e políticas `--locked`, `--offline` e
  `--frozen`. Archives rejeitam traversal, links, duplicatas e bombas de
  expansão; raízes são canonicalizadas em Windows, Linux e macOS.
- **Reprodutibilidade:** manifests, grafos, lockfiles e metadados de artefatos
  devem ser byte-estáveis entre sistemas suportados. A promoção Gold exige
  E2E nativo fora do checkout e recuperação após interrupção.
- **Tooling:** LSP incremental prioriza documentos abertos; snapshots stale
  são descartados, filas são limitadas e diagnósticos permanecem estruturados.
  O servidor classifica semantic tokens; cores e ícones pertencem à extensão
  e ao tema do editor.
- **Memória e execução:** o [Semantic Memory Model](./arandu-semantic-memory-model-v0.1.md)
  conecta OSSA/liveness, `RelativeBorrow`, escape e GenRef. GenRef é fallback
  seguro e explícito; efeitos, ownership, layout por alvo e invariantes
  SSA/AMIR permanecem contratos do compilador. RC/ARC e tracing GC não são
  requisitos da Gold v0.1.
- **Tipos de produto:** um pacote Arandu pode publicar um target binário (`bin`),
  um target de biblioteca (`lib`) ou, futuramente, ambos (`mixed`). Binários
  exigem uma função `main` para `run`/`build`; bibliotecas não. O manifesto é a
  unidade de projeto e pacote, no mesmo sentido em que Cargo combina pacote e
  targets, enquanto a extensão do editor apenas expõe ações compatíveis com o
  tipo de target.
- **Templates de ecossistema:** `arandu new --bin` e `arandu new --lib` são a
  superfície Gold atual. Templates de plugin multiplataforma, FFI e workspace
  são extensões futuras, não requisitos da stdlib mínima. Eles só entram após
  contratos de ABI, efeitos, runtime e distribuição estarem definidos.

Detalhes históricos de pesquisa, checklists já executados e evidências de CI
vivem no Git e nos contratos técnicos; não são novas tarefas do roadmap.

**Fonte única de verdade (checklist executivo).**
Este documento consolida as decisões arquiteturais sobre Data-Oriented Design (Interning), Polimorfismo Híbrido, OSSA (Ownership SSA), Effects, Async Colorless, Arquitetura de Memória e Binários de Pegada Zero em uma especificação técnica unificada e acionável.

> **Execução atual:** as campanhas transversais S0–S3, L0–L3, decomposição de
> monólitos, anotações PascalCase e GenRef estão encerradas no escopo publicado.
> Este é o único roadmap executivo; contratos técnicos vivos permanecem em
> documentos próprios e campanhas concluídas permanecem recuperáveis no Git.

> **Campanha de estabilização ativa:** auditoria de arquitetura, documentação e
> portabilidade. Nenhuma nova superfície de linguagem entra antes de concluir a
> consolidação dos contratos implementados e classificar as dívidas encontradas.

### Semântica de status

O checklist histórico `[x]` significa que a implementação prevista naquele
marco foi integrada; não significa automaticamente qualidade `gold`. A
classificação canônica de maturidade é `gold`, `done`, `partial`,
`experimental` ou `planned`, conforme definida abaixo.
Uma fase posterior pode liberar a versão completa de um item antigo sem apagar
o marco anterior: o item antigo permanece implementado, mas só vira `gold`
quando cumprir seu contrato atual.

| Estado | Significado |
| --- | --- |
| `gold` | escopo publicado implementado, exercitado pelo gate correspondente e sem bloqueador conhecido |
| `done` | implementação prevista existe, mas falta ao menos uma prova ou obrigação de produto |
| `partial` | caminho útil existe, porém partes do contrato ainda estão ausentes |
| `experimental` | disponível sem promessa de estabilidade ou suporte completo |
| `planned` | decisão aceita ou investigação registrada, ainda sem implementação |

### Registro consolidado das campanhas encerradas

| Campanha | Estado | Evidência e contrato vivo |
| --- | --- | --- |
| S0 — baseline reproduzível | `gold` | toolchain/lockfile fixos e `S0 / Gate` obrigatório |
| S1 — contratos e recovery | `gold` | falhas tipadas, ICE reportável, determinismo e contratos de [backend](./arandu-backend-contract-v0.1.md) e [CLI/LSP](./arandu-cli-lsp-contract-v0.1.md) |
| S2 — projetos reais/endurance | `gold` | corpus versionado, churn, budgets, fuzz regressivo e `S2 / Endurance` |
| S3 — distribuição beta | `gold` no canal RC publicado | pacotes nativos Linux x86-64, macOS ARM64 e Windows x86-64; [contrato de distribuição](./arandu-distribution-contract-v0.1.md) e [verificação](./release-verification.md) |
| L0–L3 — LSP/editor | `gold` no escopo publicado | VFS/snapshots, Unicode, cancelamento, multi-file, UX e Extension Host; [arquitetura](./arandu-salsa-lsp-architecture-v0.1.md) e [matriz pública](./arandu-lsp-capabilities-v0.1.md) |
| Decomposição arquitetural | `gold` | LSP handlers e IDE (`arandu_lsp/src/ide/`), pass manager, `arandu_runtime`, `arandu_codegen`, CLI (`main.rs`, `args.rs`, `pipeline.rs`, `commands/`, `project/`, `test_runner/`) e manifesto modularizados; separação de constraints permanece backlog futuro do type checker |
| Minimal 0.1 e CLI de projeto | `gold` | superfície exercitada por `examples/minimal/` e comandos `new/check/run/build/doctor` |
| Project & Package Lifecycle | `gold` | [manifesto, lockfile, grafo, cache e dependências remotas](./rfcs/0004-project-package-lifecycle.md) com recovery e E2E multiplataforma |
| Anotações públicas | `gold` | contrato [PascalCase](./rfcs/0002-canonical-attribute-naming.md), aliases legados apenas na janela de migração |
| GenRef (R0) | `frozen-R0` | [RFC 0001 (R0 Frozen)](./rfcs/0001-generational-fallback-genref.md); casca empírica congelada para medição acadêmica (TCC); AMIR tipada, payload/drop, C/Cranelift, O004/LSP |
| SL_T — testes e benchmarks | `done`; soak para `gold` | [contrato consolidado](./arandu-testing-benchmark-harness-v0.1.md), SDK/VSIX e matriz nativa `SL_T / Harness` |
| Paralelismo Estruturado | `done` | [RFC 0003](./rfcs/0003-structured-parallelism.md); funcional em Linux, aguardando benchmark reproduzível e matriz nativa para `gold` |

### Fila de execução

1. Concluir a campanha de auditoria, documentação, modularização e portabilidade.
   A rodada rc.5 corrigiu ciclos/renumeração no SimplifyCFG, validação de
   parâmetros densos, liveness de domínio vazio, pilha recursiva do RPO e dispatch Linux de sockets
   sem timer; a evidência fica na [auditoria](./arandu-architecture-audit-v0.1.md).
   Antes de encerrar: executar os novos casos nos runners nativos, perfilar
   validação incremental do corpus válido de 50 módulos, aprofundar limites
   de dataflow e pressão da fila de resultados LSP. Remoção de
   cópias redundantes não promove O2 nem constitui benchmark de velocidade.
   Decisão de fechamento: o S0 exige o workspace nativo em Linux, Windows e
   macOS quando houver mudança de produto; publicar somente após resultado
   verde do PR, mantendo o soak/SDK como evidência separada. As extrações
   CLI/runner/IDE já existem. Renomear `arandu_package` e substituir guardrails
   ou estruturas de memória exige benefício demonstrado, não contagem de linhas.
   A revisão pré-commit corrigiu ownership join/cancel, empréstimo de ExprKind
   e recuperação sintática no IDE sem duplicar lowering. O probe de 64 funções
   comprova cutoff dos resumos de borrow; a granularidade de lower_amir e a
   pressão do canal de resultados LSP continuam abertas com evidência na auditoria.
2. Concluir o soak e promover [SL_T](./arandu-testing-benchmark-harness-v0.1.md) a `gold`.
3. Entregar a `SL_S-Core`:
   fundação `core`/`alloc`, targets `bin`/`lib`, link multi-file, módulos,
   texto/coleções seguros, `std.path` estrutural e readiness `wasm32`, sem
   efeitos de sistema.
4. **Isolamento e Congelamento do R0 (GenRef)**: Conforme [RFC 0001](./rfcs/0001-generational-fallback-genref.md),
   a superfície pública, semântica (`@NoFallback`/`O004`/`O010`), métricas e paridade C/Cranelift
   do GenRef permanecem estritamente congeladas para a coleta de dados empíricos do TCC.
   O restante do compilador evolui de forma independente sem bloqueio.
5. Publicação de `0.1.0-rc.5` concluída (tag `v0.1.0-rc.5`, PR #26).
6. Implementar `A2` (Effect System) antes de APIs de filesystem, processos,
   plugins ou dependências externas.
7. Entregar `SL_S-Host`: `std.path` e APIs de sistema com efeitos explícitos,
   testes nativos, contenção por capacidades contra TOCTOU/symlink races ([RFC 0016](./rfcs/0016-capability-safe-filesystem-and-path-resolution.md))
   e limites por plataforma.
8. Implementar `SL_R` (runtime async) e só então o compiler service com sandbox
   e site/editor remoto.
9. Estabilizar a campanha de otimização AMIR descrita em 3.2: validar o O2
   existente, introduzir análises cooperativas antes de novos passes de memória
   e promover LICM/TCO somente após as regressões semânticas e estruturais
   obrigatórias.
10. Publicar `0.1.0-rc.6` como candidata otimizada, preservando o comportamento
    e os contratos de `rc.5`, com melhorias de custo validadas por benchmark
    antes/depois e sem inferir ganhos de hardware apenas da forma da AMIR.
11. Avaliar templates `mixed`, `ffi`, `plugin` e `workspace` conforme ABI,
    efeitos e distribuição amadureçam.

### Trilha incremental nativa — RFC 0011

Os números abaixo são marcos de produto, não uma renumeração retroativa das
fases históricas deste documento. O caminho batch completo continua sendo o
fallback de correção em todos os marcos: cache ausente, incompatível ou
corrompido deve causar rebuild/link completo com erro descritivo, nunca crash
ou aceitação de estado não verificado.

| Marco | Estado | Corpo funcional e critério de saída |
| --- | --- | --- |
| Fundação atual (`0.1`) | `done` | O CLI verifica a closure de inputs e o BLAKE3 do executável antes de aceitar cutoff; mudanças apenas documentais cortam antes das queries; fingerprints atuais são movidos para a sessão substituta sem reler inputs imutáveis; CGUs têm chaves canônicas e sidecars verificados. No Linux x86-64, se todas as CGUs forem hits e o layout provar igualdade exata da closure, o executável verificado é reutilizado sem link; caso contrário, o CLI tenta patch ELF fail-closed e sempre pode voltar ao linker completo. O oráculo compara SHA-256 entre paths e `RAYON_NUM_THREADS=1/16`, e o relatório schema 2 separa tempo de parede de custos por fase. Limite conhecido: cada `build` ainda nasce com uma DB nova e `lower_amir` continua program-wide. |
| Motor quente e imagem dev (`0.2`) | `planned` | Manter uma `DatabaseImpl` viva em um serviço local supervisionado, com protocolo versionado, fila limitada/coalescida, prioridade para o build solicitado e fallback transparente ao CLI batch após crash ou incompatibilidade. A DB Salsa não é serializada e queries continuam sem filesystem. Em paralelo, definir uma publicação de imagem dev recuperável que sincronize somente ranges/páginas alterados ou use clone CoW quando disponível, sem mutar artefatos CAS publicados; ausência de suporte sempre cai no protocolo atômico atual. O gate exige equivalência byte a byte com clean build, testes de kill/recovery, corpus de edições repetidas e `p95 ≤ 100 ms` para edição de corpo no host de referência documentado; CI compartilhada observa, mas não impõe a latência. |
| Granularidade por instância (`0.3`) | `planned` | Fazer `item_source_input → typeck por item → AMIR por instância → CGU` ser a cadeia real de demanda. A projeção atual `func_amir` sobre `lower_amir` monolítico não satisfaz este marco. Persistência em disco, se necessária, cobre apenas saídas canônicas e versionadas fora do grafo Salsa, com CAS BLAKE3, dependências explícitas, GC limitado e validação fail-closed. O gate exige que uma edição privada não rebaixe importadores nem CGUs irmãs, equivalência incremental/clean e `p95 ≤ 10 ms` no workload e host de referência. |

Antes de otimizar estruturas por contagem de `.clone()`, `String` ou alocações,
o relatório por fase deve localizar o custo dominante e um perfil antes/depois
deve demonstrar o ganho. A ordem planejada é motor quente e publicação dev
proporcional aos bytes alterados primeiro, lowering por instância depois e
somente então cache remoto/distribuído ou novos formatos persistentes.

### Trilha da Stack Científica, Numérica e de Dados — RFC 0012

Esta trilha formaliza os marcos de maturidade para a infraestrutura oficial de
computação científica, tensores, processamento colunar analítico e modelagem
matemática, preservando o invariante de zero alocação oculta, `no_std` no core
e interoperabilidade nativa com o padrão Apache Arrow.

| Marco | Estado | Corpo funcional e critério de saída |
| --- | --- | --- |
| SCI.1 — Arandu Math v1 | `partial` | A árvore contém a fundação bidimensional atual (`ArrayView`/`ArrayViewMut`, `Matrix`, `StaticMatrix`, operações com destino e `ScratchArena`) e kernels escalares de referência. Ainda faltam o `Array<T, N, A>` N-dimensional, prova instrumentada de zero alocação, SIMD/fusão, GEMM bloqueado e conector BLAS. Esses itens permanecem gate de SCI.1; SCI.2 não deve começar sobre contratos provisórios. |
| SCI.2 — Arandu Data v1 | `planned` | Layout colunar compatível com Apache Arrow (`RecordBatch`, validity bitmaps de 1 bit por nulo, buffers contíguos de offsets para strings UTF-8). SoA explícito (`StructArray<T>` vs `Array<T>`). Interoperabilidade zero-copy bidirecional via Arrow C Data Interface (`ArrowArray`, `ArrowSchema`) com garantia de tipos para saneamento de buffers uninit antes da exportação. Leitores e escritores em streaming para CSV, Arrow IPC e Parquet. |
| SCI.3 — Arandu Compute | `planned` | Motor de consultas preguiçosas (*lazy query engine*). Representação de planos lógicos desacoplada com otimizador puro em pipeline: predicate pushdown, projection pushdown, slice pushdown e simplificação de expressões. Motor de execução física colunar em streaming chunked com controle estrito de RSS para datasets maiores que a memória RAM. |
| SCI.4 — Arandu Science | `planned` | Matrizes esparsas com ciclo de vida segregado: `CooBuilder` para construção dinâmica mutável e `CsrMatrix`/`CscMatrix`/`BsrMatrix` para computação imutável de alto desempenho. Algoritmos de grafos e redes complexas implementados via álgebra linear esparsa e semirings (padrão GraphBLAS: SpMV, SpGEMM). Transformada rápida de Fourier baseada no modelo de planos reutilizáveis (FFTW: `Plan.estimate` vs `Plan.measure`). Geradores de números pseudo-aleatórios counter-based (Philox) e PCG desacoplados de distribuições, sem estado global mutável e compatíveis com paralelismo determinístico. Solvers para EDOs e processamento digital de sinais. |

### Trilha de IntelliSense, IDE e Playground — consolidação

A direção é uma única engine de análise neutra compartilhada pelo servidor de
linguagem e pelo playground wasm, apoiada no pipeline CST-first resiliente
([RFC 0010](./rfcs/0010-cst-resilient-ide-typeck.md)) e no backend Component
Model ([RFC 0014](./rfcs/0014-native-wasm-component-model-and-runtime.md)).
Os itens abaixo consolidam desempenho, paridade de apresentação e tooling; não
criam sintaxe, não relaxam o `parse` estrito e não introduzem uma API de editor
paralela. O compilador continua retornando erro no primeiro diagnóstico; apenas
o caminho IDE analisa o programa recuperado.

| Item | Estado | Corpo funcional e critério de saída |
| --- | --- | --- |
| IDE.1 — Sessão de análise quente no wasm | `planned` | Manter uma `AnalysisHost` viva no playground e aplicar edições incrementais em vez de reconstruir a DB a cada request de completion. Critério: orçamento de latência documentado no host de referência e itens semânticos idênticos aos do caminho atual. |
| IDE.2 — Memoização por item do typeck recuperado | `planned` | Aproveitar os memos por item no fallback que analisa um buffer com erro de sintaxe (hoje resolve/checa por request). Critério: cutoff comprovado entre edições que não alteram o item, sem acumular diagnósticos nem tocar `parse`/`type_check`. |
| IDE.3 — `sortText`/`filterText` alinhados ao cliente | `partial` | Mapear o rank neutro para `sortText` e `filterText` estáveis no LSP e no Monaco. Critério: ordem determinística idêntica entre os dois clientes para o mesmo buffer. |
| IDE.4 — Hover e signature help sobre apresentação única | `planned` | Consumir a apresentação de tipos, assinaturas e doc comments no hover e no signature help dos dois clientes. Critério: mesma assinatura ativa e nenhum `SymbolId`/`Debug` de IR exposto ao usuário. |
| IDE.5 — Alinhamento ao marco de typed holes | `planned` | Aproximar a superfície IDE do marco de typed holes listado acima: tipos, completion e hover durante código incompleto, com execução ainda bloqueada. Critério: nenhum caminho de execução aceita programa com holes. |
| IDE.6 — Build wasm reprodutível e site | `planned` | Automatizar o build do playground fora do host (override das flags de linker globais e perfil sem debug info) e manter o site consumindo o artefato com `astro check` limpo ou exceções justificadas. Critério: um comando reproduz o `.wasm` de ~4 MB e o site o publica sem edição manual. |

### Resíduos que continuam abertos

- GenRef R0 possui casca pública congelada ([RFC 0001](./rfcs/0001-generational-fallback-genref.md))
  para aferição experimental (sem concorrência nos handles, thread-confined, host-only).
  Evoluções como inlining R1 e cross-thread channels pertencem a marcos posteriores.
- Governança formal de RFCs unificada em `docs/rfcs/`.
- A remoção dos aliases legados de anotações exige uma fronteira de release.
- A separação completa `constraint_gen`/solver do type checker é melhoria de
  arquitetura planejada, não bloqueador das garantias atuais.
- Runtime async completo, effect system, ABI pública geral, LLVM, cache estável,
  ecossistema e self-hosting continuam nos marcos futuros abaixo.

### Linhas de pesquisa avaliadas

Estas linhas foram confrontadas com papers e especificações primárias. Elas não
autorizam implementação imediata: só viram marco executável quando houver
contrato, benchmark ou prova de segurança que justifique o custo.

| Linha | Estado | Aplicação possível no Arandu | Referência primária |
| --- | --- | --- | --- |
| Typed holes e sintaxe total | `planned` | LSP mantém tipos, completion e hover durante código incompleto; execução continua bloqueada enquanto houver holes | [Hazelnut Live](https://arxiv.org/abs/1805.00155), [Live Pattern Matching with Typed Holes](https://doi.org/10.1145/3586048) |
| Effects + capabilities | `planned` | A2 evolui de rótulos de efeito para autoridade explícita sobre rede, filesystem (`std.fs.Dir` / [RFC 0016](./rfcs/0016-capability-safe-filesystem-and-path-resolution.md)), processos e FFI; contenção de kernel contra TOCTOU/symlinks | [Capsicum](https://www.usenix.org/legacy/event/sec10/tech/full_papers/Watson.pdf), [RFC 0016](./rfcs/0016-capability-safe-filesystem-and-path-resolution.md), [Effect Capabilities](https://doi.org/10.1016/j.scico.2015.12.002), [Object-Capability Model](https://arxiv.org/abs/1907.07154) |
| Teste diferencial/metamórfico | `partial` | Fuzzing incremental compara diagnósticos após edições de texto e do grafo de módulos com uma DB nova; ampliar a comparação C/Cranelift e transformações semanticamente equivalentes no SL_T permanece pendente | [Csmith](https://users.cs.utah.edu/~regehr/papers/pldi11-preprint.pdf), [Metamorphic Testing de compiladores](https://onlinelibrary.wiley.com/doi/10.1002/stvr.1812) |
| Equality saturation/e-graphs | `research` | Otimizações AMIR para expressões puras depois de benchmark de custo e limite de crescimento do e-graph | [egg](https://arxiv.org/abs/2004.03082) |
| WebAssembly Component Model/WIT | `planned` | target WASM, plugins e compiler service com interfaces tipadas e ABI portável; substitui a ideia de usar Protobuf como ABI | [WIT](https://component-model.bytecodealliance.org/design/wit.html), [Component Model](https://component-model.bytecodealliance.org/design/component-model-concepts.html) |
| Refinement types leves | `research` | Contratos opcionais para índices, paths, handles e estados de recursos sem introduzir tipos dependentes completos | [Refinement Types: A Tutorial](https://arxiv.org/abs/2010.07763) |
| Núcleo formal OSSA/GenRef | `research` | Provar em um microcálculo as regras de ownership, promoção e ausência de use-after-free antes de tentar provar o compilador inteiro | [RustBelt](https://plv.mpi-sws.org/rustbelt/popl18/paper.pdf) |
| Incrementalidade orientada à demanda | `research` | Estudar reutilização de resultados não observados sem substituir Salsa antes de medir workloads reais | [Adapton](https://matthewhammer.org/adapton/adapton-pldi2014.pdf) |

As quatro linhas com maior retorno provável são typed holes, capabilities,
teste diferencial/metamórfico e WebAssembly Component Model. E-graphs,
refinement types, prova formal e incrementalidade orientada à demanda ficam
como pesquisa até que a linguagem tenha workloads e contratos suficientes.

---

## 📊 Painel de Progresso

Legenda: `[x]` feito · `[/]` em andamento · `[ ]` não iniciado

### Pipeline de Compilação

| Passo | Estado | Notas |
|-------|--------|-------|
| Lexer | `[x]` | Recovery, spans, semicolon insertion |
| Parser → AST | `[x]` | AST estruturada, `self`, `Result<T,E>` canônico |
| Name resolver | `[x]` | N001–N006 |
| Type checker | `[x]` | HM Bidirecional, `TypeId` interner |
| AHIR | `[x]` | Golden `tests/hir/` |
| AMIR CFG | `[x]` | Dominadores, SSA registers vs stack slots |
| Definite init | `[x]` | lattices, InitBits flow, O008 diagnostic |
| Move checker | `[x]` | OSSA intraprocedural, O001/O005/O007, spans reais |
| Middle-end opt | `[/]` | O1 estável: SCCP + DCE + SimplifyCFG; O2 experimental: field forwarding (SROA limitado) + GVN; LICM/TCO fora do pipeline até cumprir os gates de 3.2 |
| Backend Cranelift | `[x]` | v0.2 Dev/Debug |
| Backend C | `[x]` | Portabilidade fallback |
| Backend LLVM | `[ ]` | v0.4+ Release Optimizer |

### Marcos de Entrega

```
Fase 1 — Estabilização Semântica (v0.1) · [CONCLUÍDA]
[x] B      Result<T,E> + Option<T> no type checker
[x] C      receiver canônico (`self: own T` | `self: mut ref T` | `self: ref T`)
[x] T      Generics instantiations + Constraints (Go-style)
[x] G      Definite initialization (O008)
[x] F1     Instruções OSSA no AMIR (StorageLive/StorageDead/Destroy/Borrow/Move)
[x] M1     Move checker básico (O001, O005, O007) com spans reais (BUG-01)
[x] O1     Constant folding + DCE com bitset denso O(1) (SCALE-02)

Fase 2 — A Construção da Infraestrutura & Execução (v0.2) · [FECHADA no checklist de infra]
                  · Residual de produto em Fase 3 (F2 OSSA, A3 compiler model) tratado abaixo.
[x] INF2.1 Refatoração para InternPool (AstPool & TypeInterner centralizado)
[x] HIR Pool-first migration — structural HIR nodes (blocks, stmts, expr-blocks) stored in `HirPool` and referenced via `HirBlockId` (lowering, monomorphize, pretty-print, AMIR lowering and tests updated)
[x] ARCH   Decomposição de monólitos — pass manager/AmirBuilder, handlers LSP,
           `arandu_runtime` e `arandu_codegen` extraídos; ownership dos crates
           e testes arquiteturais impedem reacoplamento.
[x] A5     Layout de Dados Orientado a Objetos/Registros (SoA, dense ID-based graphs no AMIR/CFG)
[x] A6     CPU-Oriented Execution Model (table-driven parsing, branchless categorization)
[x] A7     Portable SIMD Infrastructure (AVX2/NEON UTF-8 validation e keyword matching)
[x] A8     Parallel Task Scheduler (work-stealing DAG, thread-local arenas, affinity)
[x] A9     Dense Bitset Infrastructure (dataflow, liveness, OSSA/DCE bits)
[x] A10    Stable ID Infrastructure
   ├─ [x] A10.a  IDs inteiros estáveis (ExprId, TypeId, SymbolId — NonZeroU32 + IndexVec) — em uso em todo o compilador
   ├─ [x] A10.b  StableHandle por hash estrutural — removido (código morto, nunca integrado)
   ├─ [x] A10.c  Generational IDs — `DocumentStore` + `slotmap::SlotMap` (`DocumentId`);
   │              close/reopen invalida handles antigos (testes em `doc_store`)
   └─ [x] A10.d  AnalysisRevision / LspSymbolId — stale-safety de análise por revisão de snapshot
                  (não generation em SymbolId); ver `arandu_query::analysis`
[x] A11    Token & String Storage Engine (packed tokens, SSO via smol_str, string interning)
[ ] A12    Deterministic CTFE & Comptime Metaprogramming (AMIR VM, Salsa queries, RFC 0013)
[x] BC     Backend Cranelift (Dev/Debug com compilador em memória)
   ├─ [x] BC.1   Fat Pointer String JIT (tratar String como ptr + len na convenção de chamadas do Cranelift)
   ├─ [x] BC.1a  Fechar ownership de buffers produzidos por `ToStr`, interpolação
   │              e helpers de path nos dois backends; elaboração de drop glue
   │              em `drop_elaborate` e limpeza de temporários em statement boundaries.
   ├─ [x] BC.2   Implementar EnumPayload & Discriminant no Cranelift JIT (Garantia estática contra double-free depende de M2; atualmente mitigado via poison-check em debug)
   ├─ [x] BC.3   Implementar IndexAccess & Array/Tuple no Cranelift JIT (Garantia estática contra double-free depende de M2; atualmente mitigado via poison-check em debug)
   ├─ [x] BC.4a  Borrow/BorrowMut no Cranelift JIT
   │              · `AmirProjection::Deref` + place addr via `use_var` (nunca `stack_addr` do slot do ponteiro)
   │              · materializar base (`is_memory`) em place projetado para Stores sobreviverem ao prune
   │              · `ref T`/`mut ref T` local: F2.0–F2.3 (stack home + OSSA);
   │                projeções seguras sobre storage indireto: path BC.4a
   │              · backend C: `format_place` com Deref lvalue
   ├─ [x] BC.4b  Await no Cranelift JIT (A3.0–A3.6: layout disc/payload + block_on; scheduler = SL_R)
   ├─ [x] BC.5   Classificador ABI por target para tipos nomeados no Cranelift;
   │              não usar limiar universal `<=16`: SysV AMD64, Windows x64 e
   │              AArch64 divergem em passagem/retorno de agregados
   └─ [x] FUZZ   Fuzzing Lexer/Parser SIMD (arandu_fuzz e cron jobs semanais de robustez)
[x] C_FB   Backend C de portabilidade e bootstrapping
[x] DX     Diagnostics & Tooling Infrastructure (DX1-DX3, DX4 CFG visualization; DX2 recovery anchors completed)
[x] PERF   Compiler Instrumentation & Observabilidade (pass timers, allocations, query logs, -Z flags, tracing-based self-profile)
    ├─ [x] PERF.1   Tracing subscriber + -Zdebug-* flags via EnvFilter (replaces time_pass!/debug_point!)
    ├─ [x] PERF.2   SelfProfile Layer — feature `self-profile` + debug build;
    │               Trace Event JSON em memória e `finalize_self_profile()`
    ├─ [x] PERF.3   #[instrument] em 22 funções críticas (parser, unify, typeck, resolve, etc.)
    └─ [x] PERF.4   ParseCache (legado CompileSession) — absorvido pela query Salsa `parse`
[x] SL_C   Scaffolding da stdlib: primitivas iniciais de `core`/`alloc` e provas
           de compilação; não equivale ao contrato público Gold de `SL_S-Core`
[x] DOC1   docs/ossa-virtual-anchoring.md — RFC retroativo documentando a técnica de âncoras virtuais + poda

Fase 3 — OSSA Avançado, Semântica e OS Runtime (v0.3) · [PARCIAL; vários marcos concluídos]
[x] A1     Query System (Incremental Semantic Database, Salsa-like O(1) invalidation)
   ├─ [x] A1.1   Salsa Integration / CompilerDatabase migration (`CompileSession` removido)
   ├─ [x] A1.2   O(1) FileId lookup em DatabaseImpl (índice reverso FileId→SourceFile via FxHashMap)
   ├─ [x] A1.3   Dataflow por bloco — `liveness_facts` / `block_dataflow_facts` (live+init+move)
   ├─ [x] A1.4   F4 IDE diags — `file_ide_diagnostics` / `func_analysis_diags` + early cutoff
   ├─ [x] DX.5   Causal-Chain explain-rebuild — `RebuildLog` + Salsa `Event` callback;
   │              CLI `-Zexplain-rebuild`; testes `explain_rebuild`
   └─ [x] DX.6   LSP gold — `arandu_lsp` com `lsp-server` + main síncrona + VFS debounce +
                 snapshot workers (`AnalysisHost`/`AnalysisSnapshot`); diagnostics + goto-def +
                 multi-file; `DocumentId` geracional; stale revision descarta jobs
[x] DX.6a   Descoberta inicial e reload de pacote aguardam a entrega das respostas
            interativas; controle limitado a 64 requests e um reload coalescido.
            Regressões controladas cobrem ordenação, erro, cancelamento, saturação
            e revisão stale sem relaxar `stdio_open_document_stays_interactive_during_discovery`.
[x] PERF.5  Arc nos campos pesados de TypeCheckResult (pré-requisito para DX.6)
   │  Feito: symbols / resolved / type_info atrás de `Arc`; diagnostics por valor.
   │  Clone de `TypeCheckResult` é O(1) atomic refcount; `type_info_mut()` /
   │  `Arc::make_mut` no lower HIR quando o interner precisa mutar.
   │  `check_bodies_only` usa `Arc::unwrap_or_clone` ao reentrar no checker.
[x] A2     Effect System (pure, readonly, noalloc, nothrow, nosuspend)
[x] A3     Modelo Async **no compilador** (colorless / corrotina) — FECHADO como marco de language
   │  Fronteira honesta: **não** é async runtime. `await` = block_on; scheduler/Waker/I/O = **SL_R**.
   ├─ [x] A3.0   `async func` / `async {}` → `Coroutine[T]`; `CoroutineReady`
   ├─ [x] A3.1   `Suspend` CFG frontier + resume
   ├─ [x] A3.2   borrow-across-await (O010 absolute; A3.4 rewrites local refs)
   ├─ [x] A3.3   stack-first `CoroutineReady.stack` (C multi-stmt)
   ├─ [x] A3.4   pin-free `RelativeBorrow` / Load rewrite
   ├─ [x] A3.5   dense live capture em `Suspend.args`
   ├─ [x] A3.6   poll layout + block_on + `Poll[T]` typeck
   └─ [→] SL_R    Runtime async real (fila, Waker, I/O) — **próximo marco async**, não residual A3
[x] A4     Memory Layout Optimization Engine (field reordering, niche tags, SOO)
   ├─ [x] A4.0   Struct Field Reordering (eliminação de padding)
   ├─ [x] A4.1   Niche Optimization (Option/Enum Packing) — REQUER F2.0 referências seguras
   ├─ [x] A4.2   Pointer Tagging (metadados nos bits menos significativos)
   └─ [x] A4.3   Small Object Optimization (SOO) para tipos <= 24 bytes
[x] F2     OSSA borrow completo — FECHADO no escopo de linguagem v0.3 compiler
   │  Residuals W3 (parcialmente fechados):
   │  · [x] auto-ref/auto-deref call args & method receivers (`T` ↔ `ref T`/`mut ref T`)
   │  · [x] lower materializa `Borrow`/`BorrowMut` quando formal é Ref/RefMut
   │  · [x] `ArgConsumeKind::is_exclusive` (`mut ref` vs `ref`)
   │  · [x] assinaturas canônicas carregam `ref T`/`mut ref T` diretamente
   │  Backend honesty (W5):
   │  · [x] T033 barrando call indireto no typeck; JIT só rede de segurança
   │  · [x] `??` é CFG no AMIR; BinaryOp::NullCoalesce no JIT = ICE de pipeline
   ├─ [x] F2.0   Tipos seguros `ref T` / `mut ref T` no parser + type-checker
   ├─ [x] F2.1   Local Borrow Checking Incremental (Salsa `block_borrow_facts` / may-borrow dataflow A9)
   ├─ [x] F2.2   Janelas de Liveness de Empréstimos (loan window = live range da ref; `is_borrowed_at`)
   ├─ [x] F2.3   Escape policy + O004/O010 + G2 promote (análise + GenRef nos backends i64 MVP)
   │    ├─ [x] F2.3.1 Escape detection (return → O010; heap-store → O004 path)
   │    ├─ [x] F2.3.2 O004 nota informativa (Magia Inspecionável — nunca silencioso)
   │    └─ [x] F2.3.3 G2: `@NoFallback` / `--no-generational-fallback` promove O004→erro
   └─ [x] F2.3.runtime  GenRef Gold no escopo seguro do
        │               `docs/arandu-genref-gold-rfc-v0.1.md`
        ├─ [x] Handle opaco tipado, identidade de arena, zero inválido e retirement sem ABA
        ├─ [x] AMIR `GenInsert`/`GenGet`/`GenSet`/`GenUpsert`/`GenRemove` validada
        ├─ [x] Payload type-erased com `DataLayout`, ownership e drop glue explícitos
        ├─ [x] Promoção CFG para raízes representáveis; projeções sem owner/path first-class são rejeitadas
        ├─ [x] Paridade C/Cranelift, shutdown determinístico e ausência do limite legado de 256 slots
        ├─ [x] O004 rico, quick fix `@NoFallback`, relatório opt-in e métricas puras
        └─ [x] Fuzz/oracle, endurance, Miri, ASan e UBSan; thread/FFI/persistência fora do escopo
[x] M2     Move checker avançado (O002, O003, O006) — `borrow_check` sobre F2.1/F2.2
           └─ Poison 0xDE em debug (BC.2/BC.3) permanece defesa extra, não substituto de M2
[x] G2     fundido em F2.3.3 (promote O004; ver acima)
[x] T2     DX Enhancements: Default Generic Parameters & Scoped Enum Variant Sugar
   ├─ [x] T2.1   Default Generic Parameters (`T = Default`; expand on lower/return/instantiate; stdlib `Vec<T>` / `GenArena<T>` use `A = GlobalAllocator`)
   └─ [x] T2.2   Implicit Enum Variant Dot-Notation Sugar (`.Ok`/`.None`/`.Some`/`.Pending` via expected type on return/let/set; `VariantSugar` AST + HIR ResultCtor)
[x] T3     DX: Import Sem Aspas para Módulos Internos (LSP-friendly path tokens)
   │
   │  Motivação: `import "std.core.mem" as mem` usa uma string opaca que o LSP
   │  não consegue completar sem tratamento especial de "cursor dentro de string".
   │  Com `import std.core.mem as mem` o lexer emite tokens limpos (IDENT + DOT)
   │  que o LSP completa com o mesmo mecanismo de qualquer expressão pontuada.
   │  Strings com aspas continuam válidas para paths de filesystem com caracteres
   │  inválidos como identificadores (barras, hifens, domínios).
   │
   ├─ [x] T3.1   AST: ImportDecl::ModuleAlias { span, path, alias }
   ├─ [x] T3.2   Parser hand+RD: `import <path> as <alias>` sem aspas
   ├─ [x] T3.3   Resolve: ModuleAlias via canonicalize_import_path + collect/load
   ├─ [x] T3.4   Stdlib migrada (core/alloc usam path tokens; residual aspas só onde External)
   ├─ [x] T3.5   Contrato parser: import_module + import_module_alias_path
   └─ [x] T3.6   LSP complete em path tokens `import std.▮` + members `alias.▮` (W4)
[x] SL_S-Core   Stdlib fundamental: targets `bin`/`lib`, multi-file HIR link,
                 módulos/imports e fundação `std.core`/`std.alloc` em camelCase;
                 resta fechar ownership genérico, OOM/allocators, paridade de alvo
                 e a janela explícita de migração antes da promoção Gold
                 · AUD.0–AUD.5 e BV.1–BV.3 implementados: contratos de retorno,
                   múltiplas origens, carriers OSSA e APIs seguras de `[]T` e
                   `String` preservam proveniência somente em compile-time;
                   a classificação global continua **parcial** pelos gates de
                   allocator, drop e matriz nativa; relatório
                   permanente em [arquitetura da stdlib](./arandu-stdlib-architecture-v0.1.md#relatório-final-aud5--segurança-de-borrowed-views)
                 · [ ] evidência nativa de pointer width 32 quando um SDK 32-bit
                   for oficialmente publicado; layout 32/64 já possui regressão
    ├─ [x] SL_S-Core.1  Fundação freestanding (`std.core` / [RFC 0017](./rfcs/0017-lean-freestanding-core-architecture.md)):
    │                   Zero OS, Zero Heap Global, Zero Threads; `str` usa somente intrínseco do compilador e
    │                   o paralelismo com runtime/alocação reside em `std.parallel`, fora de `std.core`
    ├─ [ ] SL_S-Core.2  Anti-Panic Bloat: emissão de traps nativos de 1 instrução (`UD2`/`BKPT`/`EBREAK`)
    │                   com `TrapCode` de 32 bits, sem metadados de string ou vtables de formatação em produção
    ├─ [→] SL_S-Core.3  Primitivas para hardware restrito: `std.core.fixed.Q16_16` usa armazenamento `i32`
    │                   e intermediários `i64`; Q8.8/Q32.32, operações checked e intrínsecos (`clz`, `ctz`, `popcount`, `bswap`) permanecem
    └─ [x] SL_S-Core.4  I/O abstrato em memória: interfaces `Reader`/`Writer`/`Seeker` e implementações
                        `SliceReader`/`SliceWriter` sobre buffers fornecidos pelo chamador (`std.core.io`)
[x] SL_S-Host   APIs de sistema: host path/rt helpers, filesystem e processos;
                 depende de A2 e de contratos nativos por plataforma
    ├─ [x] SL_S-Host.1  Primitivas básicas de leitura e diretório (`readToString`, `DirListing` em `std.fs`, runtime C/Cranelift)
    ├─ [ ] SL_S-Host.2  Capacidade de diretório e resolução segura ([RFC 0016](./rfcs/0016-capability-safe-filesystem-and-path-resolution.md)):
    │                   introduzir `std.fs.Dir` em `std.fs` e `arandu_std::os::descriptors` eliminando autoridade ambiente em mutações
    ├─ [ ] SL_S-Host.3  Motor nativo imune a TOCTOU/Symlink races: `openat2` (`RESOLVE_BENEATH`) no Linux, `openat`/`unlinkat` (`O_NOFOLLOW`)
    │                   no Darwin/BSD, e `FILE_FLAG_OPEN_REPARSE_POINT` com semântica POSIX no Windows NT
    ├─ [ ] SL_S-Host.4  Blindagem de limites do VFS do compilador: hardening em `scan_aru_entries_rec` (`arandu_query::vfs`)
    │                   para auditar `d_type` e abortar travessia de symlinks que escapem de `package_src`
    └─ [ ] SL_S-Host.5  Efeitos de sistema de arquivos e política de pacotes: distinção formal entre `@Effects(FileRead/FileWrite)`
                        (escopo de capacidade) e `@Effects(AmbientFsRead/AmbientFsWrite)` (acesso irrestrito bloqueável via `arandu.toml`)
[x] SL_R   Async Runtime: SL_R.0 typed spawn/join/block_on Coroutine + SyncExecutor; SL_R.2 EpollReactor (epoll+timerfd); SL_R.1/3 open
[x] SL_P   [Processamento paralelo estruturado](./arandu-structured-parallelism-v0.1.md):
           corpo funcional integrado; `WorkerPool` bounded e reutilizável no runtime Rust,
           WorkThunk ABI `(ptr[C], ptr[R]) -> i32`, operação pública `parallelFold`
           em `std.parallel`, inlining automático no AMIR (`arandu_mir::inlining`),
           chunks fixos independentes da contagem de workers, seed/identidade separadas,
            slabs alinhados e integração Pypor acima do cutoff. O backend C usa
            worker pool nativo reutilizável com fila bounded e self-help. Promoção a Gold depende de:
            · [x] pool reutilizável no backend C com fila bounded e self-help
            · [ ] benchmark versionado e reproduzível com 1/2/4/8 workers
            · [ ] matriz nativa Windows/macOS com cancelamento e falha de spawn
            · [ ] desenho de ownership/drop glue antes de aceitar resultados não-`Copy`
[x] SL_T   [Testing & Benchmark Harness](./arandu-testing-benchmark-harness-v0.1.md):
           implementação, SDK/VSIX e matriz nativa concluídos; soak operacional
           permanece como único requisito para promoção formal a Gold

Fase 4 — Expressividade de Linguagem e Tipagem (v0.35) · [PARCIAL; superfície inicial integrada]
[x] SYN.1  Retorno implícito: última `Expr` do body → valor de retorno (typeck + AMIR; async wrap A3)
[x] SYN.2  Interpolação: `$name` + `${expr}` (lexer → StringInterp/ToStr; e2e CLI)
[x] SYN.3  Opcionais: `nil` → Option.None (contexto); match Some/None no AMIR; `T?` permanece Nullable (§2.1)
[x] SYN.4  Patterns: `_`, binds, ranges, or-patterns `p1|p2` (parse/typeck/AMIR)
   ├─ [x] SYN.4.1  Desconstrução Qualificada em Patterns: suporte transparente a enums do prelude/core (`Option.Some(v)`, `Option.None`, `Result.Ok(v)`, `Result.Err(v)`) em `is`/`match`, evitando desvio para enums nominais de usuário com erro T018 e oferecendo diagnósticos estruturados com hints
   └─ [x] SYN.4.2  Condições de Padrão Compostas: encadeamento de múltiplos padrões e expressões booleanas em guardas de controle de fluxo (`if a is Some(x) && b is Some(y)`), eliminando aninhamentos artificiais em kernels e coleções
[x] SYN.5  Identificadores Contextuais em Membros: relaxamento léxico/sintático permitindo palavras reservadas da linguagem como nomes de campos e métodos após `.` (`obj.set(...)`, `Type.set`)
[→] TYP.1  Structural interface satisfaction (method sig duck) — done; dyn/existential interface types later
[x] TYP.2  Constraints: `<T: I>` + `where T: I`; Self em interface; check_instantiation T025; call via bound
[x] TYP.3  Inferência Bidirecional e Coerção Segura de Literais (Target-Aware Literal Unification)
   ├─ [x] TYP.3.1  Descida Contextual Direta (`expected: Option<TypeId>` em chamadas, let, retornos e campos; validação com `TargetInfo` e T038)
   ├─ [x] TYP.3.2  Propagação em Operadores Binários e Coleções (Inferência contextual em `1 + x`, `x + 1`, `[1, 2, 3]` sem casts manuais)
   └─ [x] TYP.3.3  Unificação de Restrições Tardias no Solver (`TypeVar` de literais não resolvidos; resolução retroativa sem fallback arbitrário `i32`)
[x] TYP.4  Const Generics em Parâmetros de Tipo: suporte a parâmetros inteiros/escalares em structs e aliases (`struct StaticMatrix<T, const M: uint, const N: uint>`), unificando matrizes de stack e buffers de tamanho fixo sem duplicação de tipos dedicados

Fase 5 — Otimização Global, CodeGen & Ecossistema (v0.4+) · [NÃO INICIADA]
[ ] LLVM   Backend LLVM (Release Optimizer, LTO, PGO profile-guided optimization pipeline)
[ ] REG    Register Allocation (Linear Scan para Cranelift, Graph Coloring para LLVM)
[ ] GEN    Adaptive Monomorphization (Witness tables para cold paths vs Lazy Monomorphization para loops)
[ ] ABI    ABI & Layout Stability (repr(C) garantido, fat pointers, stable calling conventions)
   ├─ [x] ABI.1   Classificador de ABI System V AMD64 / Calling Conventions (BC.5): classificação de agregados (INTEGER, SSE, MEMORY) para passagem/retorno de structs <= 16 bytes em registradores no Cranelift/LLVM.
   └─ [→] DBG     Metadados DWARF v5: `build --debug` emite `.debug_info`, `.debug_line` e `.debug_loclists`,
                  com spans, argumentos e variáveis escalares validados no GDB. Agregados completos, matriz LLDB,
                  integração VS Code e stack traces de panic ainda são gates pendentes da Fase 3.
[ ] C_PRETTY Emissor C Idiomático e Estruturado ([RFC 0018](./rfcs/0018-pretty-idiomatic-c-codegen.md)):
   ├─ [ ] C_PRETTY.1  Reestruturação de Controle de Fluxo: algoritmo de dominância/Relooper convertendo o grafo de BasicBlocks em `if`/`else`, `while`, `for` e `switch`, eliminando >95% dos `goto bbX;`
   ├─ [ ] C_PRETTY.2  Preservação de Identificadores Reais: mapeamento de `SymbolTable` para restaurar nomes de variáveis originais e escopos `{ ... }` locais
   ├─ [ ] C_PRETTY.3  Coalescência de Expressões SSA: inline de temporários de uso único em expressões C naturais (`int res = (a + b) * c;`)
   └─ [ ] C_PRETTY.4  Geração Canônica de Headers `.h`: exportação de interfaces C limpas com documentação e tipos `<stdint.h>` para integração direta em projetos C/C++ e conformidade MISRA C (Regra 15.1)
[x] PAN    Panic & Error Model sem unwinding (abort nativo UD2/BRK, zero metadata overhead)
[ ] CACHE  Cache Compartilhado Global e Target Zero-Bloat ([RFC 0019](./rfcs/0019-zero-bloat-target-and-shared-cache.md)):
   ├─ [ ] CACHE.1  Content-Addressable Storage (CAS) Global: repositório centralizado indexado por BLAKE3 em `~/.cache/arandu/cas/` deduplicando artefatos compilados entre todos os projetos locais
   ├─ [ ] CACHE.2  Target Local Zero-Bloat: diretório `target/` do projeto estritamente restrito a binários e bibliotecas finais (`bin/`, `lib/`), banindo objetos intermediários (`.o`), `.amir` e sessões incrementais
   ├─ [ ] CACHE.3  Publicação por Cópia em Gravação (CoW/Reflinks): instanciação instantânea no `target/` via `ioctl(FICLONE)`, `clonefile` ou hardlink com zero consumo adicional de blocos físicos
   ├─ [ ] CACHE.4  Coletor de Lixo LRU Automático: política de teto de disco rígido (default: 2,0 GiB) com expurgo assíncrono em background sem necessidade de ferramentas externas ou intervenção manual
   └─ [→] CACHE.5  Envelope binário v1 atômico para payloads canônicos `.air`, `.amir` e `.ameta`, com namespace,
                   comprimentos e hashes BLAKE3 verificados fail-closed. Os payloads atuais são dumps textuais write-only:
                   hidratação direta de AST/AMIR, compatibilidade evolutiva e cutoff real por IR desserializada permanecem pendentes.
* Mover json e xml para arandu_ext::serialization
[ ] EXT    Ecosystem Extensions: arandu_ext (ecs, game loop, renderer, audio, media, physics, gui — Out-of-Tree)
[ ] SCI    Scientific & Data Stack (Out-of-Tree / Crates Externas): arandu_math, arandu_data, arandu_science (RFC 0012)
   ├─ [→] SCI.1  Fundação matemática parcial (views 2D, Matrix/StaticMatrix, *_into e ScratchArena);
   │              Array N-dimensional, prova zero-allocation, SIMD, GEMM bloqueado e BLAS permanecem
   ├─ [ ] SCI.2  Arandu Data v1 (Layout colunar Arrow, RecordBatch, validity bitmaps, Arrow C Data Interface zero-copy)
   ├─ [ ] SCI.3  Arandu Compute (Motor de queries lazy, otimizador com pushdowns, executor streaming chunked)
   └─ [ ] SCI.4  Arandu Science (Esparsos COO/CSR/CSC, GraphBLAS semirings, FFTW plans, Philox RNG, ODE solvers)

Fase 6 — Bootstrap & Auto-Hospedagem (v1.0) · [NÃO INICIADA]
[ ] HOST   Self-Hosting: compilador Arandu compilando a si mesmo de forma convergente (3-passos)
[ ] BOOT   Remoção total de dependências do compilador Rust para build releases
[ ] DIST   Empacotamento Nativo e Eliminação de Python ([RFC 0020](./rfcs/0020-native-packaging-and-python-eradication.md)):
   ├─ [ ] DIST.1  Motor de Empacotamento no `xtask`: comandos `package-archive` e `validate-archive` em Rust puro (`tar`, `flate2`, `zip`), normalizando mtime (`SOURCE_DATE_EPOCH`), uid/gid 0 e permissões
   ├─ [ ] DIST.2  Agregação de Assets de Release: comando `prepare-release-assets` em `xtask` gerando `SHA256SUMS`, `BLAKE3SUMS` e `release-manifest.json` com `sha2` e `blake3` nativos
   ├─ [ ] DIST.3  Hardening de Instaladores Shell: remoção de `python3` de `install.sh` e `install-from-tarball.sh`, cascata de SHA-256 (`sha256sum`/`shasum`/`openssl`) e validação via `arandu hash-file`
   └─ [ ] DIST.4  Validador Nativo no CLI (`arandu archive validate`): validação estrutural de segurança e manifest embutida no binário Arandu sem dependência de runtimes externos
[ ] MS     Completa compilação paralela usando o runtime nativo de concorrência com compilação < 3 segundos

Fase E — Ferramentas Integradas e Ecossistema (Evoluções Fora do Core) · [NÃO INICIADA]
[ ] E1     REPL Interativo (`arandu repl` com compilação JIT incremental em memória via Cranelift)
[ ] E2     Gerador de Documentação Integrado (`arandu doc` extraindo Rowan pending_docs para HTML estático)
[ ] E3     FFI Bindgen Automatizado (`arandu bindgen` bidirecional C/Arandu para stubs extern)
[ ] E4     Gerenciador de Pacotes Integrado (`arandu pkg` com manifesto arandu.toml e resolvedor Git)
[ ] E5     Linter de Alocação e Escape (`arandu clippy` baseado no fluxo OSSA para performance)
```

---

## 🏛️ Os 6 Invariantes Arquiteturais do Arandu

Para evitar o inchaço de binários do Rust e o overhead de runtimes pesados tradicionais, o compilador do Arandu assume seis premissas fixas de design:

1. **Dataflow-First (Semantics-First)**: O compilador não tenta encaixar otimizações ou regras de memória após a geração de código. A linguagem converte a semântica em fatos propagáveis sobre um Grafo de Fluxo de Controle (CFG) estrito.
2. **InternPool Centralizado (ID-Based)**: Proibido o uso de estruturas recursivas baseadas em ponteiros (`Box`, `Rc`, `Vec<Box<Node>>`) nas IRs intermediárias. Toda a árvore sintática e de tipos é armazenada em arrays contíguos na memória e referenciada por IDs compactos de 32 bits (`NodeId`, `TypeId`, `LiteralId`).
3. **Polimorfismo Híbrido Adaptativo**: Rejeita witness tables como default absoluto (evitando o gargalo de inlining do Swift) e recusa monomorfização total por padrão (evitando a explosão de tamanho de binário do Rust). O compilador decide de forma adaptativa.
4. **Ownership no OSSA (Ownership Semantic SSA)**: O gerenciamento de memória não é um validador de tipos na AST. Ele vive no AMIR através de instruções explícitas de fluxo de posse: `move`, `copy`, `borrow_shared`, `borrow_mut`, e `destroy`.
5. **Zero-Metadata Runtime**: Abort imediato via instruções nativas do processador (`UD2`/`BRK`) elimina a necessidade de tabelas gigantescas de stack unwinding (`.eh_frame`) e strings de pânico embutidas no binário.
6. **Identidade Única sob Incrementalidade**: A engine incremental (Salsa) nunca introduz um sistema de identidade paralelo ao já existente no compilador. Toda query usa como chave os IDs nativos do Arandu (`FileId`, `SymbolId`, `BlockId`, `TypeId`) diretamente, atuando apenas como camada de memoização.
---

## 🏛️ Linguagem & Runtime Semantics

Abstrações que prejudiquem a otimização e a análise estática são expressamente proibidas no core da linguagem.

### Estilo de Código e Nomenclatura

Constantes globais usam `SCREAMING_SNAKE_CASE` para diferenciação explícita de fluxo estático:

* `MAX_INLINE_SIZE`
* `DEFAULT_STACK_SIZE`

### Closures e Scoped Blocks

Closures utilizam a sintaxe de parenthesized closures para clareza visual e simplicidade de análise:

```arandu
thread::scope (scope) {
    scope.spawn {
        compile_file(job)
    }
}
```

**Motivação:**

* Parser simples e previsível;
* Sem necessidade de introduzir tokens especiais complexos;
* Reduz ruído visual na leitura de código;
* Favorece a legibilidade de fluxos concorrentes/estruturados;
* Evita indireções sintáticas excessivas.

### Async/Await

A sintaxe canônica e oficial na linguagem é de prefixo:

```arandu
user = await fetch_user(id)
```

A forma sufixada:

```arandu
fetch_user(id).await
```

é aceita estritamente como açúcar sintático opcional. O formatter oficial converte automaticamente qualquer uso de sufixo para a forma prefixada.

**Motivação:**

* Linearização visual clara do fluxo de controle;
* Melhor leitura do grafo de dataflow;
* Lowering mais intuitivo para o CFG/AMIR;
* Reduz o encadeamento (chaining) visual excessivo que esconde pontos de suspensão.

---

## 🛠️ O Novo Pipeline de Dados do Arandu

```text
Source (.aru)
    ↓
  Lexer           → Error recovery + String Interning
    ↓
  Parser          → AST estruturada em Pools Lineares (NodeId)
    ↓
  Name Resolver   → Symbol Table Hierárquica O(1) via IDs
    ↓
  Type Checker    → Bidirecional HM + TypeInterner (TypeId)
    ↓
  AHIR            → Typed AST + Preservação de Interfaces
    ↓
  AMIR (CFG/SSA)  → Construção do Grafo + SSA Locals
    ↓
  OSSA Engine     → Definite Init (Lattices) + Move Checker + Escape Analysis
    ↓
  Middle-End Opt  → Constant Folding + Tree-Shaking DCE na AMIR
    ↓
  Backend Selector
       ⚡ Dev/Debug   → Cranelift (Compilação Instantânea em Memória)
       🚀 Release     → AMIR O2 + Cranelift AOT `speed`
       🔌 Portability → C Puro (Fallback)
```

---

## 🚀 Fases e Detalhamento de Subsistemas

### Fase A — Compiler Infrastructure & Core Subsystems (v0.2)

Antes de expandir as capacidades de otimização, o compilador do Arandu constrói sua fundação infraestrutural. A Fase A cubre tanto a semântica e efeitos da linguagem (A1–A4) quanto a **arquitetura de execução** do compilador em si (A5–A12).

#### A1 — Query System (Incremental Semantic Database via Salsa)

Inspirado por Salsa e o request-evaluator do Swift, o compilador é estruturado como um banco de dados de consultas (queries) puras e memoizadas:

* **ParseCache (precursor, Fase 2)**: `HashMap<PathBuf, &Program>` mantido em `CompileSession` evita re-parsing de arquivos stdlib entre resolução de nomes e type-check. É o primeiro passo de memoização no pipeline e será absorvido pelo Salsa database como uma query `parse(path) -> Program`.
* **Grafo de Dependências Estáticas**: Rastreia de forma fina quais consultas dependem de quais arquivos fonte.
* **Compilação Incremental O(1)**: Alterações em um método em uma classe só invalidam as queries daquele bloco de código específico, mantendo feedback de compilação abaixo de 50ms.
* **Queries Determinísticas**: Garante que compilações repetidas com os mesmos inputs gerem binários idênticos byte a byte.

**Roteiro de migração para Salsa:**
1. `ParseCache` → query `parse(path: Path) -> Program` no Salsa database
2. `CompileSession` → `salsa::Database` com todos os recursos (type_interner, symbol_table, etc.)
3. Queries de name resolution e type-check → queries Salsa com dependências finas entre arquivos
4. Cancelamento automático de queries obsoletas em edições LSP

#### A2 — Effect System (v0.3)

Um sistema de efeitos estrito e rastreável pelo compilador. Effects são
**inferidos transitivamente** a partir do código resolvido e tipado; anotações
como `@Effects(Net)` declaram um contrato verificável, nunca uma afirmação em
que o compilador confia cegamente. Omitir a anotação não oculta comportamento,
e uma declaração menor que o conjunto inferido é erro.

O modelo separa três dimensões para que alertas permaneçam úteis:

* **autoridade/segurança**: `Net`, `FileRead`, `FileWrite`, `Environment`,
  `Process`, `Foreign` e `UnknownCapability`;
* **recursos**: `Heap`, `Blocking`, `Suspend` e `Thread`;
* **semântica**: estado, não determinismo e falhas tipadas.

`Unsafe` não concede autoridade. Operações de rede dentro de código unsafe
continuam exigindo `Net`; FFI não classificada introduz conservadoramente
`Foreign + Unsafe + UnknownCapability`. Uso interno auditável de memória unsafe
é registrado como risco de implementação, mas não contamina automaticamente
toda API pública. `UnknownCapability` sempre se propaga e políticas rigorosas o
bloqueiam.

O resumo inferido da API pública e os riscos de implementação formarão metadata
assinada pelo compilador. O gerenciador de pacotes compara versões e exige
aprovação para ampliações como `{} -> {Net}` ou alerta para mudanças de recurso
como `{} -> {Heap}`. Manifesto define o teto autorizado, lockfile registra o
perfil observado, e sandbox/runtime aplica a autoridade; checksum sozinho só
prova identidade dos bytes.

Propriedades semânticas iniciais:

* `pure`: Garante ausência de efeitos colaterais e mutações globais. Permite otimização agressiva de GVN (Global Value Numbering) e eliminação total de sub-chamadas redundantes.
* `readonly`: Permite ler dados arbitrários mas proíbe qualquer mutação. O compilador usa isso para promover borrows mutáveis em compartilhados de forma segura.
* `noalloc`: Proíbe alocações na heap. Ideal para kernels, drivers e hot-paths de alta performance.
* `nothrow`: Garante que a função nunca pânico/abort, eliminando caminhos de erro nas análises de controle de fluxo do AMIR.
* `nosuspend`: Garante que a função é síncrona e nunca suspende controle, permitindo chamadas diretas sem overhead de corrotinas.

**Marcos de implementação:**

1. IDs estáveis de effects/capabilities e representação canônica de conjuntos;
2. effects intrínsecos da stdlib e fronteiras conservadoras de FFI/unsafe;
3. inferência local e propagação interprocedural com ciclos até ponto fixo;
4. contratos explícitos e diagnósticos com cadeia causal;
5. resumo público separado do risco interno, preservando early-cutoff;
6. metadata de pacote, diff de atualização e políticas no manifesto/lockfile;
7. enforcement por sandbox para autoridade — análise estática não substitui
   isolamento de código nativo.

#### A3 — Modelo Async Semântico e Colorless (v0.3)

O Arandu resolve o "Color Problem" das linguagens modernas (onde funções síncronas e assíncronas não se misturam facilmente) através de uma semântica flexível e de baixo nível no compilador:

* **Sintaxe Colorless Adaptativa**: O parser aceita tanto a notação de prefixo quanto de sufixo:

  ```arandu
  user = await fetch_user(10)  // Prefixo
  user = fetch_user(10).await  // Sufixo
  ```

  O formatter oficial padroniza a escrita de forma uniforme, mas a flexibilidade é garantida nativamente.
* **Coroutine-Based Type System**: Para o sistema de tipos (`ArType`), o compilador possui a variante embutida `ArType::Coroutine(TypeId)` (representada como `Coroutine[T]`).
  * `async func f() -> T` é açúcar sintático idêntico a `func f() -> Coroutine[T]`, e o compilador infere os tipos de forma equivalente.
  * O interface `Future[T]` (com `poll` e `TaskContext`) existe apenas em `arandu_core` para bibliotecas de runtime, sendo implementado debaixo do capô pelo compilador para todas as `Coroutine[T]` geradas.
* **Colorless Async & @nosuspend**: É possível invocar uma corrotina diretamente em contexto síncrono. Se o compilador provar estaticamente ou dinamicamente que ela não suspende (ou se o desenvolvedor decorar com `@NoSuspend`), ela executa síncrona e imediatamente sem overhead de agendamento de tarefas.
* **Coroutining Lowering & State Machine**: Toda função `async` ou bloco `async { ... }` é quebrado em blocos básicos no CFG do AMIR contendo pontos de suspensão explícitos (`suspend` e `resume`). O compilador realiza o *coroutine splitting* transformando variáveis locais que atravessam suspension points em slots de uma struct de estado da tarefa.
* **Zero Heap Alloc por Padrão & OSSA-Aware Suspension**: As structs de estado das corrotinas utilizam *stack-first allocation* na pilha do chamador por padrão. A alocação na heap só ocorre se a tarefa escapar do escopo corrente (via escape analysis).
* **Pin-free Self-References via OSSA Indices**: O Arandu elimina a necessidade de `Pin` para corrotinas auto-referenciais. Toda variável que atravessa um suspension point é guardada na struct da corrotina. Para evitar ponteiros auto-referenciais diretos na RAM (que quebrariam se a corrotina fosse movida de posição), o compilador converte as referências internas em índices locais (`LocalId(u32)`). A struct pode assim ser movida livremente da Stack para a Heap sem quebrar ponteiros. A análise de ownership OSSA (Ownership and State Stack Allocation) valida e rastreia ownership através dos suspension points, proibindo moves parciais e borrows ativos incompatíveis que atravessem um `await`.

#### A4 — Memory Layout Optimization Engine

Um subsistema dedicado a rearranjar dados na pilha e na memória física para garantir máxima eficiência de cache e pegada zero:

* **Struct Field Reordering**: Organiza campos de structs automaticamente para eliminar padding de alinhamento desnecessário, minimizando o consumo de cache L1.
* **Niche Optimization (Option/Enum Packing)**: Enums como `Option<T>` e `Result<T, E>` aproveitam valores inválidos do tipo base (como padrões de bits inválidos) para codificar tags, mantendo a representação de `Option<ref T>` no mesmo tamanho de um ponteiro cru.
  * *Invariante de Segurança e Sequenciamento*: Esta otimização possui dependência sequencial estrita de **F2.0 (OSSA Borrow completo)**. Somente referências seguras (`ref T` / `mut ref T`), garantidas como não-nulas pelo Borrow Checker, são qualificadas para nicho.
  * *Exclusão de Ponteiros Crus*: Ponteiros crus (`ptr[T]`) são **estritamente inelegíveis** para otimização de nicho, pois o valor `0` (NULL) é um padrão de bits válido e comum em limites FFI e allocators.
  * *Garantia GenRef*: Handles geracionais (`GenRef`) requerem validação estática de que o valor de geração/índice `0` é reservado e inválido antes de serem elegíveis.
* **Pointer Tagging**: Codifica metadados ou tags de variantes de enums nos bits menos significativos não utilizados de ponteiros alinhados de 64 bits.
* **Small Object Optimization (SOO)**: Evita alocações para structs ou vetores pequenos armazenando seus dados diretamente inline dentro do próprio container se o tamanho for menor ou igual a 24 bytes.

---

### Fase A (cont.) — Execution Architecture (A5–A12)

Os subsistemas A5–A12 definem **como o compilador em si executa**: como os dados fluem pela CPU, como evitar stalls de pipeline, como paralelismo escala e como cada traversal acontece. Isso é o que separa um compilador acadêmico de um compilador industrial.

#### A5 — Data-Oriented Layout Engine

O compilador do Arandu prioriza layouts contíguos e traversal linear de memória para minimizar cache misses e pointer chasing.

**Status v0.2:** implementado no AMIR/CFG. Instruções AMIR agora vivem em uma tabela densa por função (`AmirStmtTable`) com IDs compactos (`InstrId`) e blocos básicos guardam apenas ranges contíguos (`DenseRange`). Traversals de CFG usam `BlockId` e RPO explícito; A6-A8 permanecem responsáveis por parsing table-driven, SIMD e scheduler.

* **Struct-of-Arrays (SoA)**: Subsistemas de alta densidade computacional (dataflow, liveness, SSA analysis, dominators, register allocation) utilizam layouts SoA em vez de árvores orientadas a objetos. Exemplo: em vez de `Vec<Instruction>` onde cada `Instruction` contém opcode, operands e span intercalados, o compilador armazena `opcodes: Vec<Opcode>`, `operands: Vec<OperandPair>`, `spans: Vec<Span>` como arrays paralelos. Isso permite que um pass que só precisa de opcodes itere apenas sobre a memória dos opcodes, sem poluir cache com operands e spans.
* **Dense ID-Based Graphs**: O AMIR evita ponteiros crus entre instruções e blocos básicos. Relações são representadas por IDs compactos (`InstrId(u32)`, `BlockId(u32)`, `TempId(u32)`) indexando tabelas densas contíguas. Comparação, cópia e hashing de qualquer entidade são O(1) por inteiro.
* **Pointer Compression**: Estruturas persistentes evitam ponteiros de 64 bits sempre que possível, utilizando offsets compactos de 32 bits dentro de arenas contíguas. Isso dobra a densidade efetiva de cache L1 para grafos e árvores.
* **Traversal Linearization**: Passes do middle-end reorganizam blocos básicos e instruções em ordem de Reverse Post-Order (RPO) para maximizar prefetching automático da CPU e locality durante análises iterativas de ponto fixo.

#### A6 — CPU-Oriented Execution Model

O Arandu trata o compilador como um pipeline intensivo em cache locality e previsibilidade microarquitetural. O design do frontend e middle-end prioriza:

* Redução de cache misses (L1d/L1i/L2);
* Redução de branch misprediction;
* Redução de pointer chasing;
* Maximização de linear traversal;
* Maximização de prefetching automático da CPU.

**Estratégias adotadas:**

| Técnica | Onde Aplicada | Impacto |
|---------|--------------|--------|
| Table-driven parsing e dispatch | Lexer, Parser | Elimina cascatas de `if-else`/`match` longas, converte decisões em lookups indexados por tabela O(1) |
| Branchless token classification | Lexer | Classifica categorias de caracteres (alpha, digit, whitespace, operator) via aritmética de inteiros sem branches condicionais |
| Dense bitsets para dataflow | Move checker, Definite Init, Liveness | Operações vetoriais de set (`union`, `intersect`, `diff`) em palavras de 64 bits, processando 64 locais por instrução CPU |
| Intrusive structures no IR | AMIR instructions | Metadados de encadeamento (`prev`/`next`) vivem dentro do próprio nó, eliminando alocações separadas de lista |
| Compact CFG ordering | AMIR blocks | Blocos em RPO sequencial garantem que travessias de dataflow iterem linearmente na memória |
| Flat hash tables com probing linear | Symbol tables, Type interner | Robin Hood / Swiss Tables minimizam probes e maximizam cache locality em lookups de alta frequência |
| Hot/Cold path separation | Todo o compilador | Caminhos raros de erro, recovery e diagnósticos são separados fisicamente dos hot paths principais, reduzindo pressão sobre I-cache e melhorando branch prediction |
| Arena recycling | Optimization passes | Páginas físicas de arenas transientes são reutilizadas entre passes sem devolver ao SO, evitando TLB churn e page faults |

#### A7 — Portable SIMD Infrastructure

O frontend textual do Arandu (lexer, UTF validation, keyword matching) suporta aceleração vetorial opcional baseada em capacidades da CPU.

**Backends SIMD suportados:**

| Backend | Plataforma | Largura | Uso Principal |
|---------|-----------|---------|---------------|
| Scalar fallback | Universal | 1 byte | Baseline garantido em qualquer CPU |
| SSE2 | x86_64 baseline | 16 bytes | UTF-8 validation, whitespace skip |
| AVX2 | x86_64 moderno | 32 bytes | Scanning léxico de alta vazão, keyword matching |
| NEON | ARM64 (Apple Silicon, mobile) | 16 bytes | Paridade com SSE2 em ARM |

**Runtime Dispatch:** O compilador detecta as capacidades da CPU em runtime (`cpuid` no x86, `/proc/cpuinfo` ou feature registers no ARM) e seleciona automaticamente o backend vetorial mais capaz disponível.

**Objetivos mensuráveis:**

* Reduzir branch pressure no lexer em ~4x comparado com classificação escalar;
* Acelerar scanning textual para ~2 GB/s em AVX2;
* Validar UTF-8 em blocos de 32 bytes por instrução;
* Manter fallback escalar com performance aceitável (~400 MB/s).

#### A8 — Parallel Task Scheduler

O compilador utiliza um scheduler baseado em DAG de tarefas independentes para escalar linearmente com núcleos físicos.

**Modelo de execução:**

```text
                    ┌──────────┐
                    │  Source  │
                    │  Files   │
                    └────┬─────┘
                         │
              ┌──────────┼──────────┐
              ▼          ▼          ▼
         ┌────────┐ ┌────────┐ ┌────────┐
         │ Parse  │ │ Parse  │ │ Parse  │   ← Thread-local arenas
         │ file_a │ │ file_b │ │ file_c │
         └───┬────┘ └───┬────┘ └───┬────┘
             │          │          │
             ▼          ▼          ▼
         ┌──────────────────────────────┐
         │    Merge Symbol Tables       │   ← Lock-free union
         └──────────────┬───────────────┘
              ┌─────────┼─────────┐
              ▼         ▼         ▼
         ┌────────┐ ┌────────┐ ┌────────┐
         │Typecheck│ │Typecheck│ │Typecheck│  ← Per-worker allocators
         │ mod_a  │ │ mod_b  │ │ mod_c  │
         └───┬────┘ └───┬────┘ └───┬────┘
             │          │          │
              ▼         ▼         ▼
         ┌────────┐ ┌────────┐ ┌────────┐
         │Codegen │ │Codegen │ │Codegen │   ← Per-core NUMA arenas
         │ mod_a  │ │ mod_b  │ │ mod_c  │
         └────────┘ └────────┘ └────────┘
```

**Estratégias:**

* **Work-stealing queues**: Threads ociosas roubam pacotes de compilação de outras threads sem sincronização pesada;
* **Thread-local arenas**: Cada worker opera sobre sua própria arena, eliminando mutex e false sharing;
* **Lock-free scheduling**: O DAG de dependências é resolvido com contadores atômicos — quando todas as dependências de uma tarefa completam, ela é enfileirada automaticamente;
* **Affinity-aware worker assignment**: Workers são fixados em cores físicos (CPU affinity / pinning) para evitar migração de threads e maximizar reuso de cache L1/L2.

#### A9 — Dense Bitset Infrastructure

As análises de fluxo de dados (dataflow), tempo de vida (liveness), dominadores, checagem de empréstimo (borrow checker), eliminação de código morto (DCE) e detecção de invalidade de consultas do compilador dependem de um motor de manipulação de bits de altíssima performance:

**Status v0.2:** implementado como infraestrutura densa compartilhada. `BitSet<T>` e `BitMatrix<R,C>` usam `Vec<u64>` e IDs densos; definite-init, move-state tracking, liveness local, reachability de CFG, dominance frontiers e DCE já usam a base densa. O escopo de **Borrow Tracking** em A9 é somente representacional: os conjuntos densos necessários para rastrear regiões/locais vivos ficam disponíveis para OSSA, mas as regras semânticas completas de conflito entre `borrow_shared`, `borrow_mut` e `end_borrow` continuam no marco **F2 — OSSA borrow completo**.

* **Representação Vetorial**: Estados são armazenados em `Vec<u64>` contíguos de memória, garantindo acesso linear e tirando proveito do prefetching automático da CPU;
* **Throughput Microarquitetural**: Operações lógicas fundamentais (`union`, `intersect`, `diff`) operam em palavras de 64 bits, processando até 64 elementos de dados por ciclo de instrução;
* **Cache Locality**: Consome apenas ~128 bytes para rastrear 1024 locais, comparado aos ~8 KB exigidos por `HashSet<T>` baseados em ponteiros;
* **Vetorização Automática**: Compilações release tiram proveito de instruções AVX2/NEON para processar bitsets de 256 bits em uma única operação lógica de hardware.

**Utilização planejada:**

| Análise | Bits por Local | Operações Dominantes |
|---------|---------------|---------------------|
| Definite Initialization | 1 bit | `union`, `intersect`, bit test |
| Liveness Analysis | 1 bit | `union`, `diff`, bit test |
| Move State Tracking | 2 bits (Available/Moved/MaybeMoved) | `join`, bit test |
| Dominance Frontiers | 1 bit por bloco | `union`, membership |
| Borrow Tracking | 1 bit por região | Infraestrutura representacional; regras semânticas completas em F2 |
| DCE Reachability | 1 bit por instrução | `union`, sweep |

**Estratégia de Pipeline Cache-Aware acoplada:**

* **Compact CFG ordering**: Blocos básicos são renumerados em Reverse Post-Order (RPO) após construção e após cada transformação significativa para garantir que as travessias de dataflow baseadas em bitset acessem memória sequencialmente.
* **Reverse Post-Order traversal**: Todas as análises de ponto fixo iteram os blocos em RPO, garantindo convergência de liveness/init mais rápida.
* **Dataflow batching**: Múltiplas análises independentes são mescladas no mesmo traversal para maximizar reuso de cache L1.
* **Arena recycling**: Chunks das arenas temporárias são resetados instantaneamente ao término do pass (bump pointer para 0) sem retornar páginas ao OS, eliminando page faults.

#### A10 — Stable ID Infrastructure

O compilador do Arandu evita expressamente ponteiros brutos e referências cruzadas que inviabilizariam a compilação incremental e criariam pointer chasing complexo. Toda a infraestrutura do compilador (AST, HIR, AMIR, caches e queries) baseia-se em IDs Estáveis:

* **IDs inteiros estáveis** (`NonZeroU32` em `ExprId`, `TypeId`, `SymbolId`, `FileId`, etc.) indexando `IndexVec`s contíguos — **implementado e em uso em todo o compilador**.
* **Generational IDs** para buffers LSP implementados por `DocumentStore` + `slotmap::SlotMap`; `AnalysisRevision` protege handles semânticos stale. IDs densos do compilador mantêm seus contratos próprios e não devem ser convertidos mecanicamente em IDs geracionais.
* **Stable Handles**: removidos (código morto, nunca integrados ao compilador). A identidade estável entre sessões de compilação é provida pelos IDs inteiros determinísticos do Salsa.
* **Zero Overhead de Serialização**: Como os IDs não dependem do endereço de memória virtual, salvar e restaurar caches de compilação do disco é um dump contíguo e direto de bytes.

#### A11 — Token & String Storage Engine

O frontend textual evita alocações individuais de tokens e strings, tratando o fluxo léxico como um problema de throughput de dados:

| Componente | Técnica | Benefício |
|-----------|---------|----------|
| **Token Buffer** | Packed contiguous arrays (`Vec<Token>` onde `Token` é 12 bytes: `kind: u32` + `start: u32` + `len: u32`) | Zero alocação individual, locality perfeita |
| **String Interning** | Pool global de strings deduplicado via `HashMap<&str, StringId>` | Comparação de identificadores vira comparação de inteiros O(1) |
| **UTF-8 Validation** | Validação vetorizada (SIMD quando disponível, via A7) durante o scan do lexer | Custo amortizado: validação integrada ao scanning, sem segunda passada |
| **Small-String Optimization (SSO)** | Identificadores ≤ 23 bytes armazenados inline sem alocação heap | ~95% dos identificadores reais cabem inline |
| **Buffer Reuse** | Buffers temporários de diagnósticos e formatação são arenas scratch reutilizadas | Zero pressão sobre o alocador global |

#### A12 — Deterministic CTFE & Comptime Metaprogramming (AMIR VM, RFC 0013)

O Arandu formaliza em sua [RFC 0013](./rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md) o modelo canônico de **Compile-Time Function Execution (CTFE)** e metaprogramação de primeira classe via **interpretador de AMIR desacoplado e determinístico**, superando as limitações históricas de **Miri (Rust)**, **Zig Comptime** e **Rust Procedural Macros**.

**Status:** `planned` (v0.3/v0.4); especificado normativamente pela RFC 0013.

##### 1. O que aproveitamos de melhor (Miri, Zig, Circle e D)
* **De Miri (Rust):**
  * Execução sobre a representação intermediária (**AMIR**) em vez de AST bruta, garantindo que o código em tempo de compilação siga exatamente a mesma semântica (CFG, SSA, tipos densos) do runtime.
  * Modelo de memória virtual tipada e segura com detecção rigorosa de bounds checking, *use-after-free* e *out-of-bounds* durante a compilação.
* **Do Zig Comptime:**
  * **Mesma Linguagem, Sem Macros Secundárias:** O usuário programa metaprogramação usando a sintaxe e tipos regulares do Arandu (`comptime expr`, `comptime param: Type`), eliminando a necessidade de uma linguagem de macro separada ou compilação de crates externos (`proc-macros`).
  * Introspecção e reflexão de tipos em tempo de compilação (`std.core.meta`) eliminando 90% das macros através de laços desdobrados (`comptime for`) e acesso a campos por nome (`val.@field(name)`).
* **De Circle C++ e D Language:**
  * Quasiquoting higiênico (`quote { ... }`) com splicing `${expr}` para os 10% restantes de metaprogramação (geração declarativa de novas estruturas, interfaces e anotações `@Derive`).

##### 2. Onde superamos o Miri e o Zig
* **Salsa-First & Early-Cutoff (Superando o Zig):**
  * O Zig sofre com invalidações em cascata que reexecutam o comptime desnecessariamente.
  * No Arandu, toda avaliação é uma **query Salsa pura e memoizada** (`eval_comptime(db, amir_func_id, args) -> Arc<ConstValue>`). Se a edição não alterar o valor resultante, o *early-cutoff* do Salsa impede a re-emissão de código downstream.
* **Desempenho Orientado a Dados (Superando o Miri):**
  * O Miri no `rustc` é pesado devido a camadas de abstração e rastreamento exaustivo de aliasing.
  * A VM de AMIR do Arandu aproveita o layout denso de **A5** (`AmirStmtTable`, `DenseRange`, IDs inteiros contíguos), permitindo um interpretador *cache-aware* com dispatch por tabela O(1) e alocação via arena scratch.
* **LSP & IDE Immunity (Resiliência contra Travamentos):**
  * Toda execução de comptime roda sob um **orçamento estrito de passos (*fuel budget*)** e suporte a cancelamento cooperativo assíncrono. Laços infinitos em código incompleto digitado no editor são interrompidos com diagnósticos claros, sem nunca travar a thread do LSP.
* **Cross-Compilation Exata:**
  * A VM consulta o `DataLayout` e `TargetInfo` do alvo configurado (tamanho de ponteiro, endianness, padding de structs) e não o host onde o compilador roda.
* **Inclusão de Arquivos Pura via Salsa:**
  * Primitivas como `meta.embedBytes(path)` registram o arquivo como input Salsa (`FileId`), proibindo `fs::read` direto no hot path e mantendo a pureza do compilador.

##### 3. Invariantes de Arquitetura
1. **Pureza Absoluta:** O interpretador de AMIR proíbe I/O de rede, mutação global e acesso não sandboxado ao sistema de arquivos do host.
2. **Determinismo Byte-a-Byte:** Executar o mesmo código comptime em Windows, Linux ou macOS produz idêntico `ConstValue`.
3. **Erros Estruturados:** Falhas de execução viram diagnósticos `Txxx` reportáveis com spans precisos do código fonte.

---

## 🧠 Arquitetura de Memória & Modelo de Alocação (Memory-First)

O Arandu assume oficialmente a diretriz **"Memory Architecture First"**. A performance e escalabilidade de um compilador dependem da redução de pointer chasing, cache misses, fragmentação e contention de threads. Portanto, o pipeline é desenhado com estratégias de memória e alocadores sob medida para cada etapa.

### 1. Base do Compilador: Arenas de Scratch por Passe

> **Nota de implementação (2026-07):** A implementação manual de `VmReservation` (`mmap`/`VirtualAlloc`) e `BumpArena` foi removida do codebase por ser código morto — nenhuma fase do compilador a utilizava. Os dois arquivos continham bugs de segurança (integer overflow em bounds check e granularidade errada de commit no Windows). A estratégia de memória adotada é:

* **Alocador padrão do sistema** para todas as estruturas persistentes (AST pools, tabelas de símbolos, AMIR). O design SoA com `IndexVec` já garante localidade de cache L1 sem precisar de arena customizada.
* **`bumpalo`** (crate portável, segura, zero `unsafe` exposta, suporta WASM) como arena de scratch nos passes de otimização onde dados temporários são alocados e descartados em massa: monomorphization graph, scratch buffers do move checker, grafos temporários de CFG. **[ ] A implementar — próxima etapa da Fase 3.**
* **NUMA Awareness** e **thread-local arenas por worker** permanecem como objetivo de longo prazo para quando o scheduler paralelo por arquivo (A8) for expandido além do estado atual.

---

### 2. Frontend Allocation Model

O modelo de memória no frontend é focado em alta densidade de dados e eliminação de alocações na heap global:

| Subsistema / Fase | Estratégia de Alocação | Descrição Técnica & Vantagens |
|-------------------|-------------------------|-------------------------------|
| **Lexer** | Stack Buffers + Temp Arenas | Tokens não são alocados individualmente. São emitidos linearmente em packed buffers contíguos (`SmallVector<Token>` ou buffers estáticos reusáveis). Strings temporárias e diagnósticos rápidos utilizam alocações scratch curtas. |
| **Parser** | Hierarchical Growable Arena | Nós da AST são alocados consecutivamente em chunks de arenas. Como a árvore sintática vive até o lowering/análise semântica e morre junta, toda a arena é descartada em O(1) ao fim do ciclo de parsing. |
| **AST / AHIR** | VM-Backed Bump Arenas | A árvore sintática utiliza *Pointer Compression*. Em vez de ponteiros brutos de 64 bits (`Node*`), os nós referenciam uns aos outros através de offsets numéricos compactos de 32 bits (`NodeId`), dobrando a densidade de cache L1. |
| **Type System** | Interned Canonical Types | Proibido estruturação de tipos redundantes. Todo tipo inferido ou resolvido é canônico e registrado no `TypeInterner`. O compilador manipula apenas `TypeId(u32)`, reduzindo comparações estruturais profundas a comparações simples de inteiros. |
| **Semantic Analysis** | Temp Arenas + Scoped Rollback | A inferência e overload resolution geram milhões de fatos intermediários. Escopos de funções usam *Temporary Arenas* com checkpoints. Ao sair do escopo semântico, o bump-pointer retrocede (rollback) instantaneamente. |
| **Symbol Tables** | Stable IDs + Dense Hash Storage | Símbolos não carregam ponteiros diretos que seriam invalidados por compilação incremental. São armazenados em Slot Maps compactos indexados por `SymbolId` e mapeados por tabelas Hash densas com Robin Hood Hashing / Swiss Tables. |

---

### 3. Middle-End Allocation Model

O middle-end trabalha com modificação e transformação frequente de código, exigindo alocação dinâmica mas controlada:

| Subsistema / Fase | Estratégia de Alocação | Descrição Técnica & Vantagens |
|-------------------|-------------------------|-------------------------------|
| **AMIR (SSA / CFG)** | Arena + Slab Allocator | A representação intermediária é mutável por natureza (otimizações apagam e recriam instruções). Instruções e operandos utilizam um alocador Slab integrado a *Free Lists*, reciclando slots mortos para evitar vazamentos de memória na arena. |
| **CFG Graph** | Contiguous Dense Storage | Blocos básicos (`AmirBasicBlock`) e arestas de dominadores são armazenados em vetores densos e contíguos (`IndexVec`). Isso garante buscas lineares rápidas e cache locality excelente durante travessias de análise de fluxo de dados. |
| **SSA Nodes** | Intrusive Linked Structures | Fluxo de instruções e dependências SSA utilizam estruturas encadeadas intrusivas (onde os metadados de links vivem dentro do próprio objeto instrução). Isso elimina alocações e indireções extras em listas padrão como `std::list`. |
| **Optimization Passes** | Scratch Arenas | Passes como Dominator Analysis e Liveness Analysis reservam uma Transient Arena dedicada. Todo grafo temporário de arestas e conjuntos liveness morre e é resetado instantaneamente ao término do pass. |
| **DCE & Dataflow** | Arena Recycling | Análises iterativas que exigem recriação frequente de mapas utilizam reciclagem de páginas da arena. A memória física associada nunca é devolvida ao SO entre passes, evitando overhead de alocação de página e TLB misses. |

---

### 4. Parallel Compilation Model

O suporte a compilação paralela maciça exige isolamento de memória absoluto para evitar contenção de travas globais:

| Subsistema / Fase | Estratégia de Alocação | Descrição Técnica & Vantagens |
|-------------------|-------------------------|-------------------------------|
| **Parsing** | Thread-Local Arenas | Cada thread de parsing lê arquivos fonte independentes e aloca sua AST em uma arena exclusiva. Zero lock contention global e ausência total de falsos compartilhamentos (false sharing) de linhas de cache. |
| **Typechecking** | Per-Worker Allocators | Trabalhadores semânticos resolvem classes e métodos em paralelo. Cada um opera sobre sua própria arena temporária e interner local, unificando os símbolos no pool principal de forma controlada apenas ao término da fase. |
| **Codegen** | Per-Core Arenas | A geração de código de máquina final subdivide os módulos por núcleos físicos (cores). As estruturas geradoras e buffers de escrita de binários operam em arenas NUMA-aware alinhadas à CPU física local. |
| **Job System** | Lock-Free Queues | O agendamento de tarefas do compilador utiliza filas sem travas (lock-free rings) com algoritmos de *Work-Stealing*. Threads ociosas roubam pacotes de compilação de outras threads sem forçar sincronização pesada no kernel. |

---

### 5. Incremental & IDE/LSP Model

Ambientes de longa execução como IDEs e Language Servers (LSPs) exigem persistência de dados históricos sem fragmentação de memória:

| Subsistema / Fase | Estratégia de Alocação | Descrição Técnica & Vantagens |
|-------------------|-------------------------|-------------------------------|
| **Syntax Trees** | Persistent Immutable Trees | Em vez de destruir a AST a cada digitação do usuário, o compilador IDE utiliza estruturas de dados persistentes e imutáveis (Green Trees / Ropes). Células inalteradas da árvore sintática são compartilhadas estritamente por referência. |
| **Handles** | Generational IDs | Entidades e tipos persistentes no banco semântico são referenciados por `GenerationalId` (ID composto de `index` + `generation`). Evita referências dangling e detecta instantaneamente dados invalidados por edições de código. |
| **Queries** | Salsa-like Dependency Graph | Todo o estado semântico do compilador IDE é modelado como queries memoizadas em um grafo de dependências estáticas. Apenas queries cujos arquivos de entrada sofreram alterações diretas ou indiretas são recomputadas. |
| **Snapshots** | Copy-On-Write (COW) | Mapeamentos virtuais de arquivos e registros semânticos de builds antigos coexistem com a versão ativa usando proteção de página Copy-On-Write do sistema operacional, duplicando dados físicos somente quando editados. |

---

### Fase 3 — Otimização Baseada em Fatos Semânticos (v0.3)

#### 3.1 Polimorfismo Híbrido Adaptativo (Adaptive Monomorphization)

O compilador do Arandu rejeita abordagens extremas e escolhe a estratégia ótima baseada no local de uso:

* **Witness Tables (Caminhos Frios & Fronteiras)**: Por padrão, genéricos geram uma única implementação compartilhada que opera sobre ponteiros opacos e recebe uma tabela de metadados (`ValueWitnessTable`). Isso reduz drasticamente o tamanho do binário e acelera o tempo de compilação.
* **Lazy Monomorphization (Caminhos Quentes)**: O compilador realiza a monomorfização cirúrgica (duplicação e especialização de código concreto) para:
  * Loops e hot-paths identificados por PGO ou análise estática.
  * Tipos primitivos numéricos e tipos pequenos de dados.
  * Funções explicitamente anotadas com `@Specialize`.
  * Candidatos ideais para inlining de performance.

#### 3.2 Otimizações Avançadas na AMIR

O middle-end mantém dois níveis deliberadamente separados. Salsa, em
`arandu_query`, reutiliza resultados entre revisões no nível de arquivos e itens;
o `PassManager`, em `arandu_mir`, executa transformações puras dentro de uma
função. Análises mutáveis pela passagem corrente nunca viram queries Salsa nem
alteram a superfície exportada de módulos.

##### Estado honesto do pipeline

| Componente | Estado | Contrato atual / gate de promoção |
| --- | --- | --- |
| O0 | `done` | AMIR já nasce em SSA; O0 não executa transformações e não significa, por si só, que todo valor será materializado na stack |
| O1 | `done` | fixpoint limitado de SCCP → mark-sweep DCE → SimplifyCFG, preservando argumentos de terminadores e parâmetros de bloco |
| O2 | `experimental` | field forwarding (`SROA` limitado) → GVN → núcleo O1; não promete SROA completo, ausência de spills nem registradores físicos específicos |
| LICM | `planned` | só entra após existir `LoopInfo`, inserção correta em `DenseRange`, agrupamento determinístico de loops, prova de dominância e exclusão de operações que podem trap ou não são seguras para especulação; protótipos desconectados não permanecem no produto |
| TCO | `planned` | só entra após modelar parâmetros de entrada/loop corretamente, manter `AmirStmt`/`AmirStmtKind` sincronizados e provar tail position, ABI, drop e ownership; protótipos desconectados não permanecem no produto |
| Tree-shaking | `planned` | alcançabilidade interprocedural determinística, raízes públicas/FFI/runtime explícitas e teste de paridade entre backends |
| Stack promotion | `planned` | depende de escape/ownership; “SSA” não equivale a stack promotion e promoção não garante registrador físico |

O nome SROA, enquanto o passe apenas encaminhar campos de
`StructLiteral`/tupla para `FieldAccess`, designa um subconjunto conservador. A
promoção para SROA completo exige stores parciais, escapes, projeções, layout por
alvo e ownership de campos não-`Copy` cobertos por testes.

##### OPT.1 — Infraestrutura de análises cooperativas

- Um `AnalysisManager` por função vive por toda a execução do fixpoint e faz
  lazy-compute/cache de análises intra-função; ele é descartado ao fim da função
  e nunca é persistido em Salsa.
- Passes recebem um único contexto de execução com literal pool, scratch arena e
  acesso ao manager. O resultado de cada execução informa `changed` e a classe
  real de mudança/preservação; preservação não deve ser uma promessa estática
  quando o passe pode tanto reescrever valores quanto alterar terminadores.
- A invalidação começa conservadora e respeita dependências transitivas:
  `LoopInfo → Dominators → CFG`; futuras `MemorySSA → Dominators + Alias +
  MemoryEffects`. Alterar arestas, reachability, ids ou numeração de blocos
  invalida todas as análises dependentes.
- SCCP só preserva análises de CFG quando não dobra branches nem altera
  terminadores. GVN/DCE podem preservar análises puramente estruturais apenas
  quando a transformação efetiva não muda CFG; SimplifyCFG invalida-as.
- O cache é validado por contagem determinística de construções, reuse,
  invalidação e dependências, não por igualdade acidental de endereços.

##### OPT.2 — `LoopInfo` e transformações de loops

- Implementar a detecção de loops como análise reutilizável baseada em
  dominadores, antes do próprio LICM. Back-edges com o mesmo header formam um
  único loop natural.
- Representar header, latches, body ordenado canonicamente, preheader, nesting,
  parent/children e profundidade; `loop_for(block)` retorna deterministicamente
  o loop mais interno.
- Definir comportamento conservador para CFG irredutível. Um predecessor externo
  único não é automaticamente um preheader válido: a aresta e os argumentos de
  bloco precisam permitir o hoist sem mudar semântica.
- LICM só move uma definição quando operands dominam o destino, a instrução é
  invariável, segura para especulação ou executada obrigatoriamente, e não move
  `Move`/drop/efeito/trap através de fronteiras observáveis. `Div`, `Mod`, shifts,
  loads e calls são conservadores até existir prova específica.
- Toda movimentação reconstrói a tabela/ranges de statements por API central;
  nunca aumenta um `DenseRange` e faz `push` global assumindo que o bloco é o
  último da tabela.

##### OPT.3 — Semântica de places, alias e efeitos de memória

Antes de load forwarding, DSE ou MemorySSA, o contrato de `AmirPlace` deve
definir identidade, proveniência e sobreposição:

- `Local(x)` sem `Deref` identifica o storage local; locais-base distintos só
  provam `NoAlias` enquanto nenhum caminho atravessar indireção.
- Após `Deref`, o `LocalId` identifica o slot que contém um ponteiro, não o
  objeto apontado; bases locais diferentes continuam `MayAlias` sem prova de
  proveniência.
- Campos distintos só são `NoAlias` quando o tipo/layout garante subobjetos não
  sobrepostos. Índices dinâmicos são `MayAlias`; índices constantes exigem prova
  de igualdade/desigualdade e validade.
- O borrow checker responde se um acesso é permitido em um ponto; ele não
  substitui análise de alias. Reborrows podem compartilhar identidade em
  regiões temporais diferentes.
- A análise inicial é `BasicAliasAnalysis`/`PlaceAliasAnalysis` conservadora:
  retorna `NoAlias` somente com prova explícita, `MustAlias` com identidade
  comprovada e `MayAlias` no restante. Ponteiros crus, globals, calls e interior
  mutability formam barreiras até contratos mais precisos.
- `MemoryEffects`/`ModRef` classifica loads, stores, destroy/free, alloc, calls,
  atomics/volatile futuros e operações de runtime. Calls desconhecidas são
  clobbers conservadores; o Effect System A2 poderá refinar essa resposta.

O contrato normativo da linguagem para essas provas vive no
[modelo semântico de memória](./arandu-semantic-memory-model-v0.1.md); o roadmap
mantém apenas a ordem e os gates de entrega.

##### OPT.4 — Dataflow compartilhado

- Extrair primeiro uma engine forward do comportamento comum real de
  `definite_init` e `move_checker`, migrando uma análise por vez e comparando
  estados/diagnósticos com a implementação anterior.
- O contrato cobre boundary por entry/exit, blocos inalcançáveis, join/meet,
  transferência por bloco e por aresta, argumentos de `Goto`/`Branch`/`Suspend`,
  ordem determinística da worklist e limite de convergência.
- Análises backward, múltiplos exits, estado por program point e emissão de
  diagnósticos entram somente quando houver consumidor concreto; não criar um
  framework universal antecipadamente.

##### OPT.5 — Canonicalização e GVN

- Regras vivem numa biblioteca pura, type-aware e idempotente; primeiro são
  exercitadas por um passe standalone observável. SCCP/GVN podem reutilizar
  subconjuntos inline somente depois de testes de convergência e medição.
- Cada regra preserva `Copy`/`Move`, traps, overflow, signed zero, NaN e a
  semântica numérica do tipo. Identidades como `x * 0`, `x + 0` e `-(-x)` não
  são universais para floats, inteiros com overflow observável ou operações
  potencialmente trapping.
- GVN exige dominância da definição líder, igualdade semântica da operação e
  tratamento conservador de valores não-`Copy`, calls, memória e floats. Ordem
  de `HashMap` nunca influencia a saída.

##### OPT.6 — Otimizações de memória em camadas

1. Formalizar places e implementar `MemoryEffects`/ModRef.
2. Introduzir alias analysis básica e conservadora com corpus de ponteiros,
   campos, índices, reborrow, calls e globals.
3. Implementar load forwarding/DSE inicialmente local a bloco e medir ganho.
4. Somente se workloads mostrarem benefício, criar MemorySSA intraprocedural
   como IR virtual/side table com `liveOnEntry`, uses, defs, merges e clobber
   walker. Tokens de memória não são parâmetros executáveis do AMIR e não
   vazam para backends, pretty-print canônico ou hashing sem decisão explícita.
5. Atualizações incrementais de MemorySSA só são aceitas com validator próprio;
   caso contrário a transformação invalida e força recomputação.

##### OPT.7 — TCO, alcance global e escape

- TCO transforma apenas self tail calls em posição final comprovada. Argumentos
  alimentam um header SSA explícito compatível com `func.params`; valores de
  retorno, drops, borrows, calling convention e caminhos de erro permanecem
  observáveis e equivalentes.
- Tree-shaking parte de roots explícitas (`main`, exports, FFI, runtime e
  reflection futura), preserva ordem determinística e não elimina símbolos
  alcançáveis indiretamente sem prova.
- Escape analysis pode promover heap para stack somente quando lifetime,
  tamanho/layout por alvo, chamadas e retornos provarem não-escape. A documentação
  reporta “sem load/store explícito na AMIR” separadamente de “sem spill” ou
  “sem acesso à memória” no código de máquina.

##### Gates obrigatórios de correção e desempenho

- Validator após cada passe em testes/debug: ids densos, definições dominam usos,
  block params e argumentos alinhados em `Goto`/`Branch`/`Suspend`, CFG coerente,
  `DenseRange` e `AmirStmtKind` sincronizados e visitors exaustivos.
- Regressões por passe para diamonds, blocos mortos, loops aninhados/múltiplos
  latches, CFG irredutível, traps, overflow, floats, `Move`, drops, borrows,
  calls, globals, corrotinas e determinismo repetido.
- Testes metamórficos/diferenciais com O0/O1/O2 e paridade C/Cranelift: mesma
  saída, traps e efeitos observáveis. TCO recebe teste de profundidade constante
  apenas depois de provar tail calls com argumentos.
- Benchmarks antes/depois registram tempo por passe/função, construções e hits do
  cache de análises, iterações do fixpoint, tamanho da AMIR/código, compile time e
  runtime. Contagem de blocos ou forma SSA isolada não prova branch prediction,
  cache L1, ausência de stack/spills ou “latência zero”; tais alegações exigem
  assembly e contadores de hardware apropriados.
- Nenhum novo passe vira default por melhorar apenas uma fixture. O1 continua a
  baseline estável; O2 permanece experimental até passar o gate completo do
  `AGENTS.md`, corpus E2E, comparação entre backends e workload representativo.

---

### Fase 4 — Geração e Execução Multitarget (v0.4+)

#### 4.1 Pipeline de Duplo Backend

O compilador do Arandu abandona o acoplamento exclusivo a um único backend:

* **arandu run / build --dev**: `run` utiliza o JIT **Cranelift** em memória; `build` reutiliza o mesmo lowering para emitir objeto baseline do host, ligar o runtime estático distribuído e publicar um executável nativo transacional.
* **arandu build --release**: utiliza **AMIR O2 + Cranelift AOT `speed`** como
  pipeline de produção v0.1. LLVM/LTO/PGO permanece um tier futuro opcional,
  condicionado a benchmark e sem mudar o significado de `--release`.
* **arandu build --portability**: Utiliza o backend **C** puro para transpilar o código linearizado 1:1, servindo estritamente como fallback para plataformas de nicho, embarcados de arquiteturas exóticas e bootstrapping.

#### 4.2 Register Allocation Strategy

A alocação de registradores é o ponto onde a qualidade do código gerado vive ou morre. O Arandu adota estratégias diferentes por backend:

| Backend | Algoritmo | Prioridade | Descrição |
|---------|-----------|------------|----------|
| **Cranelift (Dev/Release v0.1)** | alocador do backend | Dev prioriza compilação; release usa `speed` | A política de qualidade pertence ao Cranelift; o Arandu não promete algoritmo interno específico do backend |
| **LLVM (tier futuro)** | definido pelo backend | qualidade adicional comprovada | só entra após benchmark representativo; LTO/PGO não são promessa atual |
| **C (Portability)** | Delegado ao compilador C host | Portabilidade | O backend C emite variáveis locais e confia no GCC/Clang para alocação |

**Objetivos mensuráveis:**

* Dev builds: zero spills para funções com ≤ 12 variáveis live simultaneamente;
* Release builds: spill pressure ≤ 5% para hot loops identificados por PGO;
* Locality de registradores: priorizar reuso do mesmo registrador físico para variáveis com lifetimes não-sobrepostos.

#### 4.3 Controlled Generational Fallback

Onde a análise de tempo de vida estática do OSSA falhar em garantir a liberação automática sem overhead, o Arandu insere tags geracionais dinâmicas. Contudo, essa inserção é restrita e controlada pelo desenvolvedor:

* **Bloqueio Explícito**: O desenvolvedor pode proibir qualquer fallback de heap geracional ou alocação dinâmica anotando o escopo com `@NoFallback` ou passando a flag global `--no-generational-fallback`.
* **Diagnósticos Informativos**: O compilador emite a nota informativa **O004** detalhando onde e por que o fallback dinâmico foi inserido, fornecendo hints claros de como refatorar o código para se manter stack-first.

---

### Fase DX — Diagnostics & Tooling Infrastructure

#### DX1 — Rich Diagnostics Engine

O compilador do Arandu implementa um sistema moderno de diagnósticos inspirado nas melhores práticas visuais do Rust, Swift e Clang.

##### Recursos

* **Multi-span diagnostics**: Aponta múltiplos locais no código envolvidos no mesmo erro semântico;
* **Labels encadeadas**: Inline annotations explicativas no próprio trecho de código fonte;
* **Fix-it hints**: Sugestões automáticas de correção sintática e semântica;
* **Notes hierárquicas**: Explicações conceituais acopladas aos códigos de erro;
* **Rendering colorido**: Terminal output rico com cores e indicadores de coluna;
* **Mensagens estruturadas**: Representação interna unificada para fácil serialização.

##### Exemplo de Output Técnico

```text
error[O002]: cannot move borrowed value
  --> src/main.aru:5:10
   |
 3 | x = &y;
   |         -- value borrowed here
 4 |
 5 | z = y;
   |         ^ move occurs here
   |
note: borrow later used here on line 7
```

#### DX2 — Recovery Architecture

O parser e as análises semânticas são estruturados para resiliência a falhas, permitindo o máximo de utilidade em IDEs e Language Servers:

* **Error Nodes na AST**: Em vez de parar na primeira falha, construções sintáticas inválidas produzem nós de erro específicos sem interromper o parsing do restante do arquivo;
* **Synchronization Points**: O parser avança até delimitadores de escopo conhecidos (como `;` ou `}`) para sincronizar e continuar a análise;
* **Partial AST Continuation**: Análises de tipo operam sobre árvores sintáticas parcialmente inválidas;
* **Speculative Recovery**: Correções heurísticas simples de digitação ou tokens faltantes são assumidas temporariamente para continuar capturando erros subsequentes.

**Objetivo:** IDE responsiveness ultra rápida, exibindo múltiplos erros em uma única compilação e mantendo o LSP resiliente a código inacabado.

#### DX3 — Structured Diagnostics

Todos os diagnósticos emitidos pelo compilador possuem formato serializável nativo (JSON), permitindo integrações ricas com ferramentas externas de CI/CD e suporte a LSPs de forma consistente.

#### DX4 — CFG & IR Visualization

O compilador inclui suporte nativo para emissão visual de fluxo de controle (CFG) e estruturas de IR intermediárias em formato Dot/Graphviz. Permite que o desenvolvedor depure caminhos de OSSA, liveness, dominância e transformações de otimização de forma imediata e visual.

#### IDE — Native LSP Engine

Uma camada unificada de Language Server (LSP) nativa no compilador que expõe consultas eficientes de autocompletar, goto definition, busca de referências e diagnósticos inline. Ao compartilhar o mesmo banco de dados Salsa e as Persistent Green Trees de parser, a engine LSP responde a alterações de código em menos de 5ms de forma incremental.

---

### TYP.3 — Inferência Bidirecional e Coerção Segura de Literais

O Arandu implementa inferência contextual bidirecional estrita com coerção segura de literais inteiros sem sufixo (`10`, `0`, `1`), eliminando ruído de casts manuais (`as uint`) sem incorrer nos erros de fallback silencioso (`i32`) do Rust.

#### Arquitetura de 3 Camadas

1. **TYP.3.1 — Descida Contextual Direta (`expected: Option<TypeId>`) [v0.1 / CONCLUÍDO]**:
   - `synth_expr_expected` propaga o tipo esperado top-down para argumentos de função, declarações `let x: T`, retornos de função e campos de struct.
   - O `synth_literal_expr` consulta `TargetInfo` (`uint_max()`, `int_min()`, `int_max()`) derivado do `TargetConfig` (Salsa input) e materializa o tipo concreto diretamente, emitindo `T038IntegerLiteralOutOfRange` se o valor exceder a largura do alvo.

2. **TYP.3.2 — Propagação em Operadores Binários e Coleções [v0.2 / CONCLUÍDO]**:
   - **Operações Binárias**: `1 + x` e `x + 1` onde `x: T` (com `T` sendo qualquer tipo inteiro primitivo) propagam `Some(T)` para o lado literal, sintetizando `T` sem exigir cast explícito.
   - **Coleções e Arrays**: Literais de array `[1, 2, 3]` sob contexto `array[uint, 3]` ou `slice[u8]` propagam o tipo do elemento para cada item da lista.
   - **Requisitos de Implementação**:
     - `crates/arandu_typeck/src/type_checker/synth/expr/binary.rs`: inspeção de operandos heterogêneos `(IntLiteral, ConcreteInt)` e `(ConcreteInt, IntLiteral)`.
     - `crates/arandu_typeck/src/type_checker/synth/expr/array.rs`: propagação de `expected_element_ty` para os elementos.

3. **TYP.3.3 — Unificação de Restrições Tardias no Solver [v0.35 / TÉCNICO]**:
   - Para variáveis livres inicializadas sem anotação explícita (`let a = 10; let b = a + 20; take_uint(b);`), o compilador cria uma variável de tipo `TypeVar` atrelada a `ArType::IntLiteral`.
   - O *constraint solver* resolve o grafo de tipos por unificação tardia: quando `take_uint(b)` impõe `uint`, o solver propaga `uint` para `b`, `a` e os literais, validando os limites numéricos retroativamente.
   - **Política Anti-Ambiguidade**: Caso uma variável inteira livre nunca participe de uma operação com tipo concreto determinístico, o compilador adota `int` nativo (largura de ponteiro) ou emite aviso de desambiguação, nunca adotando `i32` silencioso.
   - **Requisitos de Implementação**:
     - `crates/arandu_typeck/src/type_checker/solver/`: extensão do unificador para restrições `ConstraintOrigin::LiteralPromotion`.
     - `crates/arandu_typeck/src/type_checker/context/`: resolução de substituições na tabela de tipos locais (`TyCtx`).

---

### Ergonomia de Sintaxe e Tipagem — Refinamentos de Campo (SYN.4.1, SYN.4.2, SYN.5, TYP.4)

Identificados durante a implementação da biblioteca padrão e computação científica (`SCI.1`), estes refinamentos visam eliminar atritos práticos de codificação sem violar determinismo ou early-cutoff:

1. **SYN.4.1 — Desconstrução Qualificada em Patterns**:
   - **Contexto**: A linguagem constrói opções via `Option.Some(v)` e `Option.None`, mas a desconstrução em patterns exigia a forma não-qualificada `Some(v)` e `None` (`Pattern::TypeTuple`). Escrever `if x is Option.Some(v)` caía em `Pattern::Enum` para tipos nominais de usuário, gerando `T018: variant 'Some' is not defined on enum 'Option'`.
   - **Solução**: `crates/arandu_typeck/src/type_checker/synth/pattern.rs` deve tratar `Pattern::Enum` qualificado com `Option` ou `Result` redirecionando para a semântica de tipo algébrico correspondente, além de emitir diagnósticos com `CodeReplacement` sugerindo a forma canônica caso haja incompatibilidade.

2. **SYN.4.2 — Condições de Padrão Compostas**:
   - **Contexto**: `Condition::Is` atualmente só suporta uma única cláusula `is` por comando `if`, exigindo aninhamento artificial (`if a is Some(x) { if b is Some(y) { ... } }`) em verificações simultâneas de múltiplos opcionais ou resultados.
   - **Solução**: Estender a gramática do parser e o lowering da AMIR para permitir conjunções lógicas (`&&`) contendo múltiplos padrões `is` e predicados booleanos (`if a is Some(x) && b is Some(y)`), gerando ramificações de curto-circuito sem blocos aninhados.

3. **SYN.5 — Identificadores Contextuais em Membros e Chamadas**:
   - **Contexto**: Palavras-chave de instrução como `set` (`TokenKind::KwSet`) atualmente geram conflitos no parser quando utilizadas como identificadores de campo ou método (`obj.set(...)` ou `func Type.set(...)`), forçando renomeações artificiais como `setAt`.
   - **Solução**: Tornar palavras-chave da linguagem sensíveis ao contexto: na posição de membro após `.` ou após `func Type.`, tokens reservados de instrução são tratados como `IdentValue`, compatível com a prática padrão de linguagens modernas (Rust e TypeScript).

4. **TYP.4 — Const Generics em Parâmetros de Tipo**:
   - **Contexto**: Arandu v0.1 suporta arrays nativos de tamanho fixo na stack (`[16]float`), mas structs genéricas ainda não aceitam parâmetros inteiros escalares (`<T, const M: uint, const N: uint>`).
   - **Solução**: Suporte a parâmetros constantes no typechecker e no pipeline de monomorfização, inclusive como valores no corpo (`while i < N`) e como dimensões encaminhadas entre instanciações (`[M][N]T`). A biblioteca matemática foi migrada diretamente de `StaticMat2/3/4` e `setAt` para uma única definição estrutural `StaticMatrix<T, const M: uint, const N: uint>` e o membro contextual `set`, sem aliases ou wrappers de compatibilidade.

---

### Fase PERF — Compiler Instrumentation & Profiling

Para garantir que o compilador do Arandu permaneça sub-segundo à medida que o projeto escala, ele possui infraestrutura nativa de observabilidade interna e profiling.

#### PERF1 — Pass Timing

Medição em nanossegundos de cada passagem lógica do compilador, incluindo Lexer, Parser, Type Checker, lowering de AMIR, OSSA/Move Checker e otimizações de Backend.

#### PERF2 — Allocation Tracking

Mapeamento preciso de recursos de memória consumidos:

* Monitoramento de páginas das Arenas e Slabs transientes;
* Taxa de reciclagem de slots nas Free Lists de instruções;
* Identificação de hotspots de alocação;
* Rastreamento de cache pressure e desperdício de padding em estruturas de IR.

#### PERF3 — Query Profiling

Instrumentação fina do motor incremental (Salsa-like):

* Grafo de invalidação de queries ativo em tempo real;
* Custo de rebuild incremental por query;
* Memoization hit rate de análises de tipo e resolução de escopos.
* **ParseCache hit-rate** no self-profile: quantos `parse_with_file_id` foram evitados pelo cache (`-Zself-profile` mostra 6 em vez de 11 chamadas para um build single-file típico).

#### PERF4 — Debug Flags (-Z)

Flags internas ativadas em compilações debug/nightly para inspeção microarquitetural e dumping de IRs:

* `-Ztime-passes`: Exibe tempo detalhado gasto em cada pass de otimização;
* `-Zdump-amir`: Imprime a representação AMIR SSA por função;
* `-Zdump-ossa`: Dump do grafo de liveness e estados de moves rastreados pelo OSSA;
* `-Zprofile-queries`: Exibe hit rate e custos do cache de queries incrementais;
* `-Zdump-cfg`: Emite grafos de fluxo de controle dos blocos básicos em formato `.dot`.

---

### Geração de Código de Máquina & Perfilamento (Fase 4)

#### PGO — Profile-Guided Optimization Pipeline

O compilador implementa suporte a otimizações guiadas por perfilamento (PGO). O desenvolvedor compila um binário de instrumentação que coleta métricas de execução reais em caminhos quentes (hot paths). Na compilação final:

* O compilador prioriza a monomorfização agressiva de genéricos e o inlining de funções apenas em hot loops com profiling documentado;
* Branches condicionais frios de erro são marcados para ordenação física distante na geração final do LLVM IR, maximizando o I-Cache de loops quentes.

---

## ⚙️ Runtime Philosophy

O runtime do Arandu segue uma diretriz de minimalismo e isolamento absoluto de dependências.

### Modelo Oficial

* **Corrotinas Stackless**: Suspensões do async/await geram splits de blocos básicos na AMIR e salvamento de estado em structs locais compactas (zero stack overhead de threads);
* **Lowering para State Machines**: O compilador gera código linear e determinístico para a transição de estados das tarefas;
* **Scheduler Cooperativo**: Tarefas cedem a CPU voluntariamente em pontos de suspensão explícitos (`await`), eliminando custos de preempção;
* **Work-Stealing Executor**: Roteia e distribui a carga de tarefas dinamicamente sobre um pool de threads de forma lock-free com NUMA awareness.

### Rejeição de Tracing Garbage Collectors (GC)

O Arandu exclui expressamente o uso de um Garbage Collector clássico (tracing, stop-the-world, mark-sweep ou compactador). A decisão de rejeitar GCs é sustentada por dez objeções técnicas principais:

1. **Perda de Previsibilidade**: GCs introduzem pausas (stop-the-world) e heurísticas de varredura temporais imprevisíveis, violando a diretriz de previsibilidade semântica;
2. **Destruição do Stack-First Design**: GCs incentivam a alocação irresponsável na heap. O Arandu promove a alocação na Stack via Escape Analysis;
3. **Piora de Locality (Pointer Chasing)**: A movimentação ou espalhamento de objetos por coletores aumenta a fragmentação de memória e deteriora a eficiência de cache L1/L2;
4. **Poluição de Hot Paths com Barriers**: GCs modernos exigem barreiras de escrita (write barriers) e leitura (read barriers) nas instruções da CPU, poluindo o fluxo e reduzindo o throughput de execução;
5. **Inchaço de Metadata**: Coletores de lixo exigem cabeçalhos de objetos gigantes, mark bits e metadados de RTTI implícitos, colidindo com o objetivo de "zero-metadata runtime";
6. **Ineficiência Multicore**: Concorrer a threads globais de marcação causa gargalos e contenção de locks de memória, anulando a escalabilidade NUMA e thread-local do Arandu;
7. **Enfraquecimento do OSSA**: O ownership-first faz do tempo de vida (lifetime) um fato explícito e determinístico. O GC esvazia o valor semântico de instruções `destroy` e `move`;
8. **Complexidade no Async**: GCs acoplam a alocação de frames assíncronos ao heap global. O Arandu realiza coroutine splitting stack-first;
9. **Footprint Gigante**: O suporte a runtime de tracing exige adicionar dezenas de megabytes ao binário final;
10. **Inviabilização de Bare-metal/no_std**: Runtimes com GC não rodam com eficiência e segurança em microcontroladores e sistemas embarcados com restrição severa de recursos.

A gerência de memória do Arandu baseia-se exclusivamente em **Semantics-driven memory**: `ownership + stack + arenas + escape analysis + controlled fallback`.

### Objetivos Principais

* Evitar stacks gigantescas de sistema por tarefa assíncrona;
* Proibir alocações de heap implícitas durante suspensões de rotinas;
* Garantir independência absoluta de garbage collectors globais;
* Manter o custo de runtime invisível em compilações embarcadas.

### Heap Allocation Policy

Nenhuma operação built-in da linguagem realiza alocação de heap implícita. Quando o compilador detecta escape semântico inevitável de um valor, ele exige um alocador explícito ou insere fallbacks controlados geracionais. Em caso de fallback automático, o compilador emitirá o diagnóstico informativo **O004**, com notas de rodapé de refatoração para stack-first.

---

## 🧱 ABI e Garantias de Layout

O Arandu define explicitamente suas regras de ABI para garantir robustez em FFI, builds incrementais e interoperabilidade limpa entre backends.

### Garantias de Layout do Compilador

* **Struct Layout Determinístico**: O reordenamento de campos para eliminação de padding segue um algoritmo canônico fixo. Caso o desenvolvedor precise de compatibilidade C pura, ele deve anotar a struct com `@Repr(C)`;
* **Enum Tagging Estável**: Tags de enums com valores de dados acoplados (como `Result`) utilizam nichos de bits nulos ou tags de tamanho previsível;
* **Pointer Alignment & Calling Convention**: Alinhamento estrito baseado na plataforma de destino e passagens de parâmetros por registradores por padrão para Cranelift e LLVM.

### Representações Internas

Abstrações de tipos e referências dinâmicas usam witness tables compactas e ponteiros duplos (fat pointers) explícitos contendo ponteiro do objeto + ponteiro de metadados de interface.

### Async ABI

Os frames e estados das tarefas assíncronas gerados pelo compilador têm tamanho e layout resolvidos em tempo de compilação, permitindo que a OSSA rastreie ownership e liveness dos empréstimos através dos suspension points com segurança.

---

## 🚨 Modelo de Falhas e Tratamento de Erros

O Arandu adota uma filosofia pragmática dividida entre erros recuperáveis e falhas fatais não recuperáveis.

### Recuperação de Erros (`Result<T, E>`)

Todo erro que pode ser contornado pelo chamador é retornado explicitamente via tipo monádico `Result`. O compilador otimiza caminhos de erro para manter overhead zero em caminhos quentes.

### Abort Imediato (Falhas Fatais)

Falhas que representam quebra de invariantes (como out-of-bounds ou asserções violadas) abortam a execução imediatamente. O Arandu **não realiza stack unwinding**.

* **Traps Nativas**: O runtime emite instruções de hardware como `ud2` (x86) ou `brk` (ARM) para encerrar o processo imediatamente;
* **Custo Zero**: Sem unwinding, elimina-se a necessidade de metadados `.eh_frame`, tabelas de exceção complexas e código de limpeza invisível no binário, simplificando o Grafo de Fluxo de Controle e reduzindo drasticamente o tamanho do executável.

---

## 🔥 Hot/Cold Path Separation

O compilador separa fisicamente seus caminhos quentes de processamento de dados dos caminhos frios (exibição de erros e logs).

* **Hot Paths**: Lexer, Parser, SSA traversal, análises CFG do OSSA, alocadores das arenas e passes de otimização de instruções. Esses blocos são mantidos compactos, lineares e em loops densos para maximizar o cache de instruções da CPU (I-cache) e reduzir branch mispredictions;
* **Cold Paths**: Geração de formatação visual de diagnósticos, pretty printers do AMIR para depuração, escrita de arquivos de dump metadata e logs de instrumentação. Todo esse código é compilado com atributos de "cold coldness", instruindo o linker a movê-los para seções de memória distantes.

---

## 💾 Stable Serialization & Incremental Cache

O compilador define formatos e extensões serializáveis estáveis para garantir persistência determinística e reutilização de cache entre builds incrementais ou compartilhados em rede:

* `.air`: Representação serializada compacta da AST em formato binário estável;
* `.amir`: Grafo serializado de instruções SSA do middle-end;
* `.ameta`: Metadados exportados de módulos com assinaturas de tipo e definições públicas;
* `.aobj`: Código objeto final gerado pelo backend de compilação.

### Garantias de Cache & Hashing Estável

* **Stable Hashing**: O compilador computa hashes estáveis baseadas no algoritmo criptográfico **BLAKE3** para cada arquivo fonte e query semântica intermediária. Isso permite invalidar e reconstruir o grafo de dependências incrementais Salsa de forma instantânea sem reprocessar blocos de código inalterados;
* **Versioned Schema**: Os metadados `.ameta` e as IRs intermediárias possuem cabeçalhos com esquemas binários versionados para prevenir erros ou conflitos de desserialização em atualizações de ferramentas do compilador.

### DET — Deterministic & Reproducible Builds

O Arandu garante a reprodutibilidade de compilação byte a byte (byte-by-byte binary convergence):

* **Deterministic Ordering**: Hashes e ordenação de queries incrementais no banco de dados utilizam ordenação lexicográfica estável nos identificadores de símbolos, garantindo que o compilador emita o mesmo binário final independentemente do paralelismo ou ordem de leitura dos arquivos no sistema multi-core;
* **Stable Compilation Outputs**: A ordenação determinística garante que builds distribuídos em CI/CD ou compilações remotas/compartilhadas produzam artefatos idênticos, acelerando o hit-rate em caches remotos.

---

## 📋 Tabela de Códigos de Diagnóstico (DiagCodes)

| Código | Categoria | Descrição Técnica e Gatilho Arquitetural |
|--------|-----------|-----------------------------------------|
| **N001** | Name Resolution | Identificador não declarado (com sugestão) |
| **N002** | Name Resolution | Redeclaração no mesmo escopo |
| **N003** | Name Resolution | Tipo usado erroneamente como valor |
| **N004** | Name Resolution | Valor usado erroneamente como tipo |
| **N005** | Name Resolution | Import não encontrado |
| **N006** | Name Resolution | Conflito de símbolos entre imports |
| **T011** | Type Checker | Generic constraint ou cláusula `where` inválida |
| **T019** | Warning | `Result<T,E>` ignorado na atribuição sem handling (`?`) |
| **T025** | Error | Interface não satisfeita (métodos faltantes no Go-style) |
| **P006** | Parser | Uso sintático inválido de tupla para retorno de erro |
| **O001** | Ownership | Uso de variável local após comando de move no CFG |
| **O002** | Ownership | Move/consume while borrowed (`O002MoveWhileBorrowed`) |
| **O003** | Ownership | Empréstimo mutável concorrendo com referências compartilhadas |
| **O004** | Info | Generational/escape fallback (G2/F2.3) — not shared-borrow conflict |
| **O005** | Ownership | Dupla liberação de memória (Double Free) |
| **O006** | Ownership | Destroy/free while borrow still active (`O006DestroyWhileBorrowed`) |
| **O007** | Ownership | Estado de move inconsistente entre branches no merge do CFG |
| **O008** | Ownership | Leitura ou cópia de slot local não inicializado |

---

## 🌟 Ecossistema Avançado & Ferramental (Propostas de Evolução)

Estas propostas descrevem ferramentas auxiliares e subsistemas externos projetados para expandir o ecossistema do Arandu para além do compilador core.

### E1 — REPL Interativo (`arandu repl`)
Um interpretador de linha de comando interativo impulsionado pelo backend Cranelift JIT.
* **Mecanismo**: Lê expressões e declarações do terminal, executa a verificação de tipos e compilação incremental de forma JIT na memória de processo, e executa a máquina de estado imediatamente para exibir o resultado avaliado.
* **Objetivo**: Facilitar prototipagem rápida, aprendizado e testes exploratórios de código sem necessidade de criar arquivos `.aru` no disco.

### E2 — Gerador de Documentação Integrado (`arandu doc`)
Gerador estático de documentação de código.
* **Mecanismo**: Extrai as strings estruturadas capturadas nos comentários de documentação do Rowan (`pending_docs`) e as compila em um site HTML estático com estilização moderna, navegação e caixa de busca integrada baseada em WebAssembly/JSON.
* **Objetivo**: Garantir que projetos e bibliotecas tenham documentação de qualidade gerada sem dependências de ferramentas externas.

### E3 — FFI Bindgen Automatizado (`arandu bindgen`)
Gerador de bindings bidirecionais C/Arandu.
* **Mecanismo**: Lê arquivos de cabeçalho C (`.h`) e emite stubs correspondentes de funções e structs `extern "C"` Arandu. Também suporta o fluxo reverso (gerar arquivos `.h` correspondentes para interfaces Arandu públicas compiladas como bibliotecas estáticas ou dinâmicas).
* **Objetivo**: Facilitar a integração com APIs de sistema legadas e bibliotecas nativas C sem o risco de erros manuais de layout de struct ou alinhamento de ponteiros.

### E4 — Gerenciador de Pacotes Integrado (`arandu pkg`)
Orquestrador de dependências simples embutido na CLI do Arandu.
* **Mecanismo**: Gerencia um manifesto `arandu.toml` e gera um arquivo de trava `arandu.lock`. Realiza download de dependências (resolução simples de branches/tags do Git) e alimenta os caminhos de importação da query Salsa automaticamente.
* **Objetivo**: Prover um ecossistema pronto para compartilhamento de código de forma modular ("batteries-included") sem necessidade de caminhos relativos complexos.

### E5 — Linter de Alocação e Escape (`arandu clippy`)
Analisador estático avançado de uso de memória e desempenho.
* **Mecanismo**: Analisa a árvore OSSA e o fluxo de posse no CFG para detectar padrões de ineficiência, como alocações em laços que causam escape redundante para a heap (`O004`), e sugere refatorações de tempo de vida ou reutilização de buffers.
* **Objetivo**: Ajudar os programadores a manterem o código no caminho de performance ideal.

---

## 12. Histórico de Revisões

| Data | Autor / Agente | Mudança Realizada |
|------|----------------|-------------------|
| 2026-05 | Antigravity | Roadmap v0.1 criado; invariantes e decisões canônicas |
| 2026-05 | Antigravity | **Grande Expansão v0.2**: Inclusão formal de Phase A (Salsa, Effects, Colorless Async, Memory Layout Engine), Hybrid Generics, Dual-Backend pipeline e Controlled Fallbacks. |
| 2026-05 | Antigravity | **Integração Memory-First**: Inclusão formal do subsistema detalhado de alocação de memória por estágio do compilador (Lexer, Parser, AST, SSA, CFG, Parallel, Incremental e IDE/LSP). |
| 2026-05 | Antigravity | **Execution Architecture (A5–A11)**: Inclusão formal dos subsistemas de Data-Oriented Layout (SoA, pointer compression), CPU-Oriented Execution Model (branchless, table-driven), Portable SIMD (SSE2/AVX2/NEON), Parallel Task Scheduler (work-stealing DAG), Cache-Aware Optimization Pipeline (RPO, arena recycling), Dense Bitset Engine, Token & String Storage Engine, Register Allocation Strategy e Hot/Cold Path Separation. |
| 2026-05 | Antigravity | **Semantics, DX & Tooling**: Inclusão formal das especificações de Semântica e Sintaxe da Linguagem (closures, async canônico), Filosofia de Runtime, ABI/Layout Stability, Abort/Panic Model, Fase DX (Rich Diagnostics Engine, Recovery, JSON output), Fase PERF (Compiler instrumentation), Hot/Cold separation e Stable Serialization. |
| 2026-07 | Antigravity | **Auditoria de Honestidade A10/A11/VM**: Removidos `vm.rs`, `arena.rs`, `stable_id.rs` e `string_pool.rs` (~1.080 LOC, 16 blocos `unsafe`) — código morto nunca integrado ao compilador. A10 corrigido para `[~]` parcial: IDs inteiros estáveis em uso, Generational IDs aguardam LSP (Fase 3) com `slotmap`. VM Reservation substituída por plano `bumpalo` para arenas de scratch nos passes de otimização. A11 permanece `[x]` via `smol_str`. |
| 2026-07 | Antigravity | **Evolução do Ecossistema (E1–E5)**: Documentadas as propostas de evolução de ferramentas integradas (REPL, Gerador de Docs, FFI Bindgen, Package Manager e Linter de Alocação). |
| 2026-08 | Codex | **Linhas de pesquisa avaliadas**: typed holes, effects/capabilities, teste diferencial/metamórfico, e-graphs, WebAssembly Component Model/WIT, refinement types, prova formal OSSA/GenRef e incrementalidade orientada à demanda; somente as linhas com contrato e evidência futura poderão virar implementação. |
| 2026-08 | Codex | **Roadmap de otimização AMIR consolidado**: estado honesto de O0/O1/O2, análises cooperativas, LoopInfo, semântica de places/alias/ModRef, dataflow, canonicalização, MemorySSA virtual, TCO/escape e gates de correção e benchmark. |
| 2026-09 | Codex | **Fechamento de hipóteses rc.5 em tipos, runtime e genéricos**: a substituição estrutural ganhou regressão que prova inserção finita sem expansão recursiva de aliases; o reactor passou a descartar o registro inteiro e liberar recursos após poison, restaurando um estado conhecido antes de aceitar novos IDs; acessos a um único campo genérico deixaram de materializar o mapa completo da struct; e um teste de integração passou a provar que instanciações genéricas idênticas feitas por módulos distintos convergem para uma única definição AMIR. |
| 2026-09 | Codex | **Ownership sensível a campos na rc.5**: `Loan` e o M1 passaram a preservar caminhos compactos de campos do `AmirPlace`. Campos com `SymbolId` distintos não produzem O003/O001 falsos; roots/prefixes continuam sobrepostos, e índices/dereferences permanecem conservadores. O join distingue campo movido em todos ou apenas alguns predecessores, store reinicializa o subpath exato e drop glue ignora só o campo transferido. Extração parcial de tipos com `@Destructor` explícito é rejeitada porque o destrutor exige o valor completo. Matrizes tipadas cobrem shared/exclusive, paths aninhados, moves, reinicialização e CFG linear/diamond. |
| 2026-09 | Codex | **Trilha RFC 0011 tornada executável**: a fundação batch verificável permanece funcional no 0.1; o 0.2 recebe o serviço de build quente sem persistir Salsa e o 0.3 recebe lowering/CGU realmente por instância, com gates explícitos de equivalência clean, determinismo e p95 em host documentado. |
| 2026-09 | Antigravity | **Stack Científica e de Dados (RFC 0012)**: inclusão formal dos marcos SCI.1–SCI.4; arrays/views strided sem cópia, separação de matrizes stack/heap, destination-passing style, scratch arenas reutilizáveis, formato colunar Arrow, motor lazy em batches e álgebra de grafos sobre semirings GraphBLAS. |
| 2026-09 | Antigravity | **Metaprogramação Comptime & CTFE (RFC 0013)**: especificação formal do subsistema A12; unificação sintática em `comptime`, reflexão estática de tipos (`std.core.meta`), eliminação de 90% das macros via `comptime for` e `@field`, quasiquoting higiênico com `${expr}`, AMIR VM determinística estilo Miri, fuel budget e queries Salsa puras com early-cutoff. |
| 2026-09 | Antigravity | **Ergonomia de Sintaxe e Tipagem (SYN.4.1, SYN.4.2, SYN.5, TYP.4)**: inclusão formal dos débitos técnicos de ergonomia identificados na SCI.1; desconstrução qualificada de enums do prelude em patterns, condições compostas com múltiplos padrões is, palavras-chave contextuais como membros e const generics escalares em structs. |
| 2026-09 | Antigravity | **Filesystem Seguro e Resolução por Capacidades (RFC 0016)**: especificação formal do subsistema de segurança de I/O (`SL_S-Host.1–5`); abstração `std.fs.Dir` eliminando autoridade ambiente em mutações, motor nativo imune a TOCTOU e symlink races (`openat2` com `RESOLVE_BENEATH` no Linux, `O_NOFOLLOW` no Darwin/BSD e `FILE_FLAG_OPEN_REPARSE_POINT` no Windows NT), hardening de limites do VFS em `arandu_query::vfs` e distinção formal de efeitos (`FileRead/Write` vs `AmbientFsRead/Write`). |
| 2026-09 | Antigravity | **Arquitetura Fundamental do `arandu_core` Freestanding (RFC 0017)**: especificação formal do núcleo irreduzível da linguagem (`SL_S-Core.1–4`); garantia estrita de Zero OS, Zero Heap Global e Zero Threads, erradicação de "panic formatting bloat" via traps de 1 instrução (`UD2`/`BKPT`/`EBREAK`) com código numérico de 32 bits, matemática de ponto fixo (`Q16.16`), fatias e views canônicas `[]T`, I/O puro em memória (`Reader`/`Writer`) e compatibilidade universal do Cortex-M0 ao WebAssembly e AArch64. |
| 2026-09 | Antigravity | **Emissor C Idiomático e Estruturado (RFC 0018)**: especificação formal do subsistema `C_PRETTY.1–4`; de-estruturação de controle de fluxo (Relooper/dominance) reconstruindo `if`/`while`/`for`/`switch` e eliminando >95% de `goto bbX`, preservação de identificadores do `SymbolTable`, declaração local de variáveis, coalescência de expressões SSA e conformidade MISRA C (Regra 15.1). |
| 2026-09 | Antigravity | **Cache Global Compartilhado e Target Zero-Bloat (RFC 0019)**: especificação formal do subsistema `CACHE.1–5`; CAS global indexado por BLAKE3 (`~/.cache/arandu/cas/`), diretório `target/` estritamente limpo contendo apenas produtos finais (`bin/`, `lib/`), eliminação de 30–50 GB de inchaço do modelo Cargo, coletor de lixo LRU automático com cota configurável (default 2,0 GiB), política de depuração *Line-Tables-First* e publicação zero-copy via reflinks CoW (`FICLONE`/`clonefile`). |
| 2026-09 | Antigravity | **Empacotamento Nativo e Erradicação de Python (RFC 0020)**: especificação formal do subsistema `DIST.1–4`; migração da geração/validação de `.tar.gz` e `.zip` para o `xtask`, agregação de assets (`SHA256SUMS`, `BLAKE3SUMS`, `release-manifest.json`), hardening de instaladores shell com cascata POSIX e validação via `arandu hash-file`, subcomando nativo `arandu archive validate` no CLI e preservação de scripts Python legados exclusivamente como oracles de teste diferencial. |

---

*Mantenha este documento atualizado a cada avanço estratégico do compilador.*
