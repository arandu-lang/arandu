# AGENTS.md — Compilador Arandu

## Propósito

Arandu é um compilador incremental em Rust. Preserve as fronteiras entre fases,
o determinismo dos resultados e o early-cutoff das queries. Uma simplificação
local que aumenta a superfície de invalidação, duplica parsing ou mistura
camadas é uma regressão arquitetural.

## Mapa do workspace

| Área                                                                                            | Responsabilidade                                                                                                                                |
| ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `arandu_base`                                                                                   | Utilitários, dados fundamentais e vocabulário comum à sintaxe e à semântica (`span`, `lang`). Mantenha-o deliberadamente leve; não adicione dependências sem justificativa arquitetural. |
| `arandu_testing_contract`                                                                       | DTOs puros compartilhados pelo runtime e pelo harness de testes; sem IR, Salsa ou dependências de compilação.                                   |
| `arandu_lexer` / `arandu_parser`                                                                | Lexer e CST-first com Rowan; `syntax_tree(file)` é canônico e `parse(file)` apenas baixa CST para AST.                                          |
| `arandu_diagnostics`                                                                            | `DiagCode`, diagnósticos e documentação longa em `docs/errors/`.                                                                                |
| `arandu_middle`                                                                                 | Contratos entre fases sem AST: HIR/AMIR, `SourceDatabase`, IDs, layout e `type_context`. `src/db.rs` declara apenas os tipos Salsa compartilhados; não executa queries. |
| `arandu_resolve` / `arandu_typeck` / `arandu_mir`                                               | Lógica pura de resolução (dona de `ResolvedNames`), tipos (dona do lowering de tipos do AST) e passes/análises AMIR. Não são donos de Salsa.          |
| `arandu_semantics`                                                                              | Lowering AST→HIR, monomorfização e HIR→AMIR (`passes::lower_amir`); hoje também é fachada de reexports. Não cresça a fachada: código novo importa do crate dono. |
| `arandu_codegen`                                                                                | Contrato de backend agnóstico (papel de `rustc_codegen_ssa`).                                                                                   |
| `arandu_query`                                                                                  | Único dono de Salsa: DB, inputs, queries tracked, `AnalysisHost` e reparse incremental.                                                         |
| `arandu_backend_cranelift` / `arandu_backend_c` / `arandu_backend_wasm` / `arandu_backend_llvm` | Backends que consomem AMIR validado: nativo/JIT, C, WebAssembly e LLVM IR textual.                                                              |
| `arandu_runtime`                                                                                | Runtime host; folha que não depende de crates do compilador.                                                                                    |
| `arandu_ffi` / `arandu_doc`                                                                     | Superfície C de pacotes e renderização pura de documentação.                                                                                    |
| `arandu_ide`                                                                                    | Análise pura para IDE (completions, apresentações), compartilhada entre LSP e web sem dependência de UI.                                        |
| `arandu_cli` / `arandu_lsp` / `arandu_web`                                                      | Orquestram a DB e pontos de entrada; LSP usa `lsp-server`, VFS e snapshots; web expõe C-ABI in-memory.                                          |
| `arandu_fmt`                                                                                    | Formatter puro, sem Salsa/LSP.                                                                                                                  |
| `arandu_test_support` / `arandu_fuzz_support` / `xtask`                                         | Infraestrutura de testes, fuzz e tarefas do workspace.                                                                                          |

## Invariantes de arquitetura — não violar sem discussão explícita

- Pipeline: CST (`syntax_tree`) → AST (`parse`) → `resolve` → `type_check` →
  `lower_amir` → backend. Não coloque resolução ou tipagem no parser, nem
  faça re-lex/parse paralelo a partir de texto quando o CST já for disponível.
- Dependências apontam para baixo no pipeline. Fases e contratos nunca dependem
  de backends, `arandu_query` ou drivers. Backends consomem AMIR, layout e
  `arandu_codegen`; não dependem de lexer, parser, resolve, typeck nem de
  lowering de HIR/monomorfização. `check-architecture` impõe isso sobre o grafo
  transitivo, sem exceções: `arandu_middle` e `arandu_mir` não alcançam o
  front-end. Vocabulário comum à sintaxe (`Span`, `Visibility`, `AbiKind`,
  `LayoutQuery`) mora em `arandu_base::lang`; nada que conheça o AST entra no
  `middle`. Fatos de tipo chegam por `&dyn AmirTypeContext` (passes AMIR) ou
  `&dyn CodegenTypeContext` (backends), em `arandu_middle::type_context` e
  implementados pelo typeck; precisando de outro fato, estenda o trait, nunca
  importe `TypeInfo`. Contratos compartilhados moram no crate dono
  (`arandu_middle::symbol_mangling`, análises AMIR em `arandu_mir`), nunca na
  fachada `arandu_semantics`. Ver `docs/arandu-backend-contract-v0.1.md`.
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

### Protocolo obrigatório da tarefa

Antes de editar:

1. Leia este arquivo e o documento técnico indicado para a área afetada. Não
   repita no `AGENTS.md` uma especificação que já tenha fonte de verdade em
   `docs/`.
2. Execute `git status --short` e `git diff --stat`. A árvore pode conter
   trabalho do usuário; preserve-o, não faça `reset`, `checkout`, `clean` ou
   stash destrutivo e não atribua alterações preexistentes à tarefa atual.
3. Identifique o crate dono, a fase do pipeline, o teste/regressão existente e
   o menor comando de validação aplicável. Para mudanças que cruzem mais de um
   crate, escreva primeiro um plano curto com arquivos, riscos e gates.
4. Procure implementações, testes e documentação antes de criar uma nova API,
   diagnóstico, RFC ou fixture. Não resolva uma inconsistência apenas por
   conveniência de release: registre a fronteira arquitetural real.

Durante a implementação:

- Preserve o escopo confirmado. Se uma falha revelar outro defeito, corrija-o
  somente quando ele for necessário para a tarefa; caso contrário, registre-o
  como pendência com arquivo, teste e impacto.
- Não trate texto de documentação, nomes de módulos, aliases ou comentários
  como prova de autoridade, ownership ou capacidade. Essas propriedades devem
  vir de IDs, inputs confiáveis e invariantes verificadas pelo compilador.
- Não introduza cópias, caches, dependências, `unsafe`, fallback permissivo ou
  wildcard em enumeração para silenciar um erro. Primeiro demonstre a
  necessidade e cubra a semântica com uma regressão.

Ao concluir:

- Separe sempre evidência executada nesta árvore de resultados históricos,
  expectativas e testes que não puderam rodar. Um comando não executado não é
  uma validação implícita.
- Informe o comando exato, o resultado, os testes ignorados e qualquer limite
  de plataforma, ambiente ou integração externa. Não diga “passou” com base
  apenas em compilação parcial.
- Revise `git diff --check`, `git diff --stat` e `git status --short`; confirme
  que cada arquivo alterado pertence ao escopo ou já estava modificado.
- Atualize a fonte de verdade correspondente: `DiagCode`/docs de erro,
  roadmap permanente, RFC, snapshots ou documentação técnica. Remova planos
  temporários quando a campanha terminar.

### Hierarquia de regras e manutenção

Quando houver conflito, siga nesta ordem: instrução explícita do usuário,
invariantes deste arquivo, documento técnico da área, código e testes atuais,
preferências de estilo. Se o código contradizer uma regra arquitetural, pare
de tratar a regra como fato e investigue a discrepância antes de alterar ambos.

Este arquivo é um contrato operacional, não uma enciclopédia. Uma regra nova
deve corrigir um erro observado ou proteger uma decisão não inferível pelo
código; inclua a fonte de verdade quando houver. Remova regras obsoletas e
revise comandos quando o toolchain mudar. Regras específicas de uma subárvore
devem ficar no `AGENTS.md` daquela subárvore ou no skill apropriado.

### Validação e testes por camada

1. **Ciclo Rápido Iterativo (Inner Loop — Uso padrão durante desenvolvimento):**
   Durante alterações pontuais ou investigação, **NUNCA** execute a suíte de testes do workspace inteiro nem `cargo doc`. Teste estritamente o crate e os alvos afetados com feedback imediato:
   - Compilação rápida: `cargo check -p <crate> --locked`
   - Teste direcionado rápido (preferir `nextest` para velocidade e isolamento):
     `cargo nextest run -p <crate> [filtro]` (ou `cargo test -p <crate> [filtro]`)
   - Exemplos práticos:
     - Parser / Sintaxe: `cargo nextest run -p arandu_parser`
     - Lexer: `cargo nextest run -p arandu_lexer`
     - Typechecker: `cargo nextest run -p arandu_typeck`
     - MIR: `cargo nextest run -p arandu_mir`
     - Runtime: `cargo nextest run -p arandu_runtime`
     - CLI / E2E: `cargo nextest run -p arandu_cli --test <nome_do_teste>`
   - Para mudanças em diagnósticos pontuais, execute `bash scripts/check-diag-determinism.sh arandu_typeck 8` quando Bash estiver disponível. Para queries/LSP, execute os testes relevantes em `arandu_query/tests/`.

2. **Gate Completo (Pre-PR / Confirmação Global):**
   Execute a suíte global de 6 passos **SOMENTE** quando:
   - O usuário pedir explicitamente para rodar o gate completo ou validar o workspace.
   - Antes de abrir/submeter um PR final ou preparar uma release.

   Passos do gate global:
   1. `cargo fmt --all -- --check`
   2. `cargo check --workspace --locked`
   3. `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
   4. `cargo nextest run --workspace --locked` (ou `cargo test --workspace --locked`)
   5. `cargo run --locked -p xtask -- check-diag-docs`
   6. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`

3. **Interação com o Usuário:**
   - Se o usuário fizer uma pergunta, solicitar explicação ou pedir para parar, responda de imediato. Não dispare compilações nem tarefas em background desnecessárias.
   - Não adicione dependências, altere IDs, una queries, remova guardrails ou atualize snapshots em massa sem justificar a decisão e cobrir o risco com teste de regressão.
