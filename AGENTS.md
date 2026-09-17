# AGENTS.md — Compilador Arandu

## Propósito

Arandu é um compilador incremental em Rust. Preserve as fronteiras entre fases,
o determinismo dos resultados e o early-cutoff das queries. Uma simplificação
local que aumenta a superfície de invalidação, duplica parsing ou mistura
camadas é uma regressão arquitetural.

## Mapa do workspace

| Área | Responsabilidade |
| --- | --- |
| `arandu_base` | Utilitários e dados fundamentais. Mantenha-o deliberadamente leve; não adicione dependências sem justificativa arquitetural. |
| `arandu_lexer` / `arandu_parser` | Lexer e CST-first com Rowan; `syntax_tree(file)` é canônico e `parse(file)` apenas baixa CST para AST. |
| `arandu_diagnostics` | `DiagCode`, diagnósticos e documentação longa em `docs/errors/`. |
| `arandu_middle` | Contratos entre fases, HIR/AMIR, `SourceDatabase`, IDs e layout. `src/db.rs` declara apenas os tipos Salsa compartilhados; não executa queries. |
| `arandu_resolve` / `arandu_typeck` / `arandu_mir` | Lógica pura de resolução, tipos, ownership/dataflow e AMIR. Não são donos de Salsa. |
| `arandu_query` | Único dono de Salsa: DB, inputs, queries tracked, `AnalysisHost` e reparse incremental. |
| `arandu_backend_cranelift` / `arandu_backend_c` / `arandu_backend_wasm` | Backends de execução nativa e WebAssembly. |
| `arandu_ide` | Análise pura para IDE (completions, apresentações), compartilhada entre LSP e web sem dependência de UI. |
| `arandu_cli` / `arandu_lsp` / `arandu_web` | Orquestram a DB e pontos de entrada; LSP usa `lsp-server`, VFS e snapshots; web expõe C-ABI in-memory. |
| `arandu_fmt` | Formatter puro, sem Salsa/LSP. |
| `arandu_test_support` / `xtask` | Infraestrutura de testes e tarefas do workspace. |

## Invariantes de arquitetura — não violar sem discussão explícita

- Pipeline: CST (`syntax_tree`) → AST (`parse`) → `resolve` → `type_check` →
  `lower_amir` → backend. Não coloque resolução ou tipagem no parser, nem
  faça re-lex/parse paralelo a partir de texto quando o CST já for disponível.
- `arandu_query` é o único dono da execução Salsa. A exceção estreita é
  `arandu_middle/src/db.rs`, que declara inputs/accumulators e o trait
  `SourceDatabase` compartilhado sem possuir providers. `arandu_resolve`,
  `arandu_typeck`, `arandu_mir`, lexer, parser e backends permanecem puros.
- Queries tracked são puras, determinísticas e sem efeitos observáveis:
  proibidos `println!`, `eprintln!`, I/O, polling de FS, mutação global e
  telemetria com efeito colateral. Instrumente com `#[tracing::instrument]`.
- `exported_symbols` e `resolve` são separadas deliberadamente. Preserve essa
  divisão e as saídas hash-estáveis: uma edição no corpo de uma função não
  pode invalidar importadores quando a superfície exportada não mudou.
- `local_symbols`, `exported_symbols`, `item_source_input`, typeck por item e
  diagnósticos IDE por item existem para early-cutoff. Não os transforme em
  resultados monolíticos nem faça deep-clone de `Program`/`AmirProgram` no hot
  path; use `Arc::clone` ou `HashEq::share`.
- `resolve` e `type_check` nunca fazem `fs::read`. Registro e leitura de
  módulos pertencem à DB/CLI/LSP; listagens de diretório passam por inputs
  Salsa (`DirectoryListing`), jamais `fs::exists`/`read_dir` no hot path.
- `SymbolId` é composto por `{ file_id, local_id }`. Não o achate, hasheie de
  modo instável ou substitua por offset de texto. O alocador de `FileId` é
  monotônico: IDs não podem ser reutilizados após unregister.
- Imports cíclicos usam `ResolutionResult` e devem convergir com resultados e
  diagnósticos determinísticos. Ordem de `HashMap` não pode afetar saída.

## LSP, snapshots e identidades

- Há três identidades distintas: `DocumentId` geracional para buffers LSP,
  `FileId` para a análise atual e `AnalysisRevision` geracional para handles.
  `LspSymbolId` só resolve quando sua revisão coincide com a do snapshot.
- Nunca mantenha `AnalysisSnapshot` nem clone de `DatabaseImpl` na mesma thread
  durante `set_text`: Salsa aguarda clones serem descartados e pode deadlockar.
- Workers LSP só analisam snapshots; a thread principal registra arquivos e
  publica resultados apenas se `DocumentId` ainda estiver vivo e a revisão
  coincidir. Não comite Salsa a cada tecla: preserve debounce/save/goto.
- `initialize` deve responder sem varrer, ler ou analisar o workspace. Faça
  descoberta e indexação depois do handshake, em background, priorizando
  documentos abertos. Recursos locais devem continuar disponíveis durante
  reload/indexação parcial.
- Descartar um resultado stale não basta: filas de trabalho devem ser limitadas
  e coalescer/cancelar jobs obsoletos. Nunca permita que uma rajada de edição
  gere backlog sem limite ou atrase completion/goto atrás de análise global.
- Separe trabalho interativo (arquivo aberto, completion, hover, goto) de
  trabalho amplo (workspace diagnostics/index). O primeiro tem prioridade; o
  segundo roda após idle/save e deve expor progresso/cancelamento quando longo.
- Mudanças de protocolo exigem testes stdio E2E, incluindo `initialize`,
  `$/cancelRequest`, shutdown, Unicode/UTF-16, edição incremental e resposta
  stale. A extensão exige testes no VS Code Extension Host, não apenas `tsc`.
- Preserve a riqueza do diagnóstico até o cliente: código, labels, notes, hints
  e replacements não podem ser reduzidos a uma string no DTO IDE. Quick fixes
  usam replacements estruturados; nunca parseiam o texto da mensagem.
- O LSP classifica semantic tokens; o editor/tema escolhe cores. Prefira tipos
  e modificadores LSP padrão, mantenha TextMate como fallback e teste temas
  claro, escuro e alto contraste. Não fixe RGB no servidor.
- Hover, completion e signature help devem compartilhar apresentação de tipos,
  assinaturas e doc comments. Não exponha `Debug` de IR, `SymbolId` ou detalhes
  internos ao usuário e não crie sintaxe apenas para uma decoração do editor.

## IR, ownership e backends

- Preserve SSA/OSSA: definições dominam usos, parâmetros de bloco correspondem
  a cada predecessor e argumentos de `Goto`/`Branch`/`Suspend` continuam
  alinhados com os parâmetros do destino.
- AMIR tem DCE mark-sweep, CFG simplification, jump threading e análises por
  worklist até fixpoint. Mudanças devem conservar efeitos observáveis, valores
  de retorno de todos os caminhos e usos de terminadores — inclusive argumentos
  de salto — e devem convergir.
- Ao criar variante de rvalue/terminador, atualize os visitors compartilhados;
  DCE, move checker, liveness e backends não podem divergir.
- Layout é dependente do alvo. Use `DataLayout`/`TargetInfo` e
  `TargetInfo.float_size`; nunca presuma `Float` = `f64`, tamanho de ponteiro,
  alinhamento ou ABI do host.
- Código inválido deve recuperar e emitir diagnóstico, não `panic!`, `unwrap`
  ou `expect` em código de produção de crates. Quebras de invariantes internas
  devem virar `Diagnostic::ice(...)` reportável.

## Diagnósticos e testes dourados

- Os prefixos são `LX`, `P`, `N`, `T`, `O`, `W` e `ICE`. `DiagCode` é a fonte
  única da verdade para códigos voltados ao usuário.
- Todo código novo voltado ao usuário exige entrada em `DiagCode`, mapeamento,
  catálogo em `docs/diagnostics/SPEC.md` e `docs/errors/<CODIGO>.md` em inglês.
  ICEs não exigem documento em `docs/errors/`.
- A bijeção `DiagCode` ↔ `docs/errors/*.md` é obrigatória. Não mantenha lista
  paralela de códigos no build script.
- Preserve spans reais e a ordenação determinística dos diagnósticos. O
  renderer atual é Miette; não introduza outro renderer sem decisão explícita.
- Fixtures dourados cobrem lexer/parser/semântica/HIR/AMIR/UI. Só use
  `UPDATE_EXPECT=1 cargo test --workspace --locked` após inspecionar e aceitar cada
  alteração de snapshot.
- Snapshots não devem serializar offsets incidentais quando o contrato testado
  é semântico. Quando spans fizerem parte do contrato, fixtures e scripts devem
  preservar bytes e finais de linha entre Windows, Linux e macOS.

## Releases e portabilidade

- A `main` recebe mudanças apenas por PR com `S0 / Gate`; não tente contornar o
  ruleset com push direto. O gate completo roda no PR, e workflows de tag devem
  provar que o commit veio de PR verde sem repetir toda a suíte sem necessidade.
- Tags e releases são imutáveis. Toda correção de candidata usa um novo número
  `rc.N`; nunca mova uma tag publicada nem sobrescreva seus artefatos.
- O contrato de release mantém tag, versões dos crates, CLI, LSP, extensão,
  manifest e stdlib alinhados. Use `xtask prepare-release` e
  `check-release-contract`, não atualizações manuais parciais.
- Bootstrap de integridade não pode depender de uma instalação prévia do
  Arandu: o archive externo usa SHA-256; depois da extração em staging, o
  próprio binário valida o `BLAKE3SUMS` interno antes da publicação atômica.
- Sucesso no host de desenvolvimento não promove suporte multiplataforma.
  Packages e installers devem ser exercitados em runners nativos Windows,
  Linux e macOS, fora do checkout e usando exatamente os artefatos públicos.

## Regra de edição e validação

- Texto versionado usa UTF-8 sem BOM e LF. `.gitattributes` é a autoridade
  independente de `core.autocrlf`; `.editorconfig` configura editores. Nunca
  normalize fixtures douradas fora dessas regras. Execute
  `cargo run --locked -p xtask -- check-line-endings` e, ao introduzir as
  regras em um clone antigo, `git add --renormalize .` antes de revisar o diff.
- A documentação usa a taxonomia de `docs/README.md`. Só
  `docs/arandu-compiler-roadmap-v0.1.md` mantém a fila permanente. Planos de
  campanha são temporários: no encerramento, remova o checklist e consolide
  contexto, implementação, pontos de melhoria e futuro no documento técnico.
- Uma contagem de `.clone()`, `String` ou linhas é sinal de inspeção, não
  evidência de regressão. Durante desenvolvimento, remova somente cópias
  obviamente redundantes e cubra semântica com testes. Mudanças de estrutura
  de dados, internamento, `Cow`, `SmallVec` ou alocador exigem workload
  representativo e perfil/benchmark antes e depois.
- Execute `cargo run --locked -p xtask -- check-architecture` ao alterar
  dependências, ownership de crates ou efeitos de filesystem.

- Antes de editar, identifique a fase e o crate proprietário. Faça a menor
  mudança que preserve as APIs estreitas de query e acrescente regressões para
  qualquer invariante tocada (cutoff, ciclos, determinismo, CFG, layout ou LSP).
- Não reporte conclusão sem executar, nesta ordem, a partir da raiz:

  1. `cargo fmt --all -- --check`
  2. `cargo check --workspace --locked`
  3. `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  4. `cargo test --workspace --locked`
  5. `cargo run --locked -p xtask -- check-diag-docs`
  6. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`

- Para mudanças em diagnósticos, execute também
  `bash scripts/check-diag-determinism.sh arandu_typeck 8` quando Bash estiver
  disponível. Para queries/LSP, execute os testes de integração relevantes em
  `arandu_query/tests/` (por exemplo `architecture_invariants`,
  `salsa_imports`, `item_body_cutoff`, `ide_diag_delta` e `block_delta`).
- Não adicione dependências, altere IDs, una queries, remova guardrails ou
  atualize snapshots em massa sem justificar a decisão e cobrir o risco com
  teste de regressão.
