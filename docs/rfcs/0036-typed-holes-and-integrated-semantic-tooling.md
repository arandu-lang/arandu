# RFC 0036: Typed holes e ferramental semântico integrado

- **Número da RFC:** 0036
- **Título:** Typed holes e ferramental semântico integrado
- **Status:** `Draft`; typed holes condicionais na direção 0.3.x e tooling na 0.6.x
- **Data original:** 2026-10-15
- **Revisão de escopo:** 2026-10-16
- **Autor:** Arquiteto da Linguagem Arandu
- **Escopo:** parser, typeck, IDE/LSP, query e CLI

## 1. Estado atual e intenção

Esta RFC propõe lacunas tipadas com contexto de ownership e ferramentas que
compartilham os resultados do compilador. `todo`/`todo("motivo")` como expressão
tipada, `arandu repl`, `arandu bindgen`, `arandu lint` e seus flags não são
superfícies públicas implementadas.

O compilador atual já possui CST com recuperação, nós de erro, `ArType::Error`,
LSP incremental, formatter e JIT Cranelift. Recuperar sintaxe/tipos incompletos
não equivale a registrar uma lacuna explícita com tipo esperado, contexto OSSA,
candidatos e ação de preenchimento. A introdução original de
`ErrorGuaranteed`/`Ty::Error` descrevia abstrações de outro desenho; a fonte
atual está em
[`ast_pool.rs`](../../crates/arandu_parser/src/ast/ast_pool.rs) e
[`synth/expr/mod.rs`](../../crates/arandu_typeck/src/type_checker/synth/expr/mod.rs).

O [roadmap mestre](../arandu-compiler-roadmap-v0.1.md) concentra escopo e gates:
0.2 motor quente; 0.3 instâncias, com typed holes/reflexão e closures em
contratos condicionais; 0.5 capabilities; 0.6 tooling/cache. Ter Salsa/JIT não
entrega automaticamente REPL persistente, reflexão nem latência por célula.

## 2. Typed holes propostos

### Sintaxe e tipagem

`todo` e `todo("descrição")` são candidatos de design, ainda sem keyword/API
aprovada. A implementação deve distinguir hole deliberado de `ExprKind::Error`
produzido pela recuperação de sintaxe. Um hole não pode esconder erros reais
nem fabricar um valor válido que chegue à execução.

Em posição com tipo esperado, a lacuna deve registrar o goal e as obrigações de
ownership. Em posição de síntese, pode precisar de variável de inferência; goal
não resolvido deve ser apresentado como tal. Não se promete que toda lacuna
seja inferível nem que o restante do programa se torne livre de diagnósticos.

### Contexto OSSA e candidatos

Sugestões devem considerar locais inicializados, moves, empréstimos, caminhos de
controle, projeções e efeitos permitidos. As análises pertencem às fases
existentes; o type checker não deve absorver todo o move checker nem consultar
um estado fictício `OwnershipState` como API pronta.

Um candidato precisa satisfazer tipo e ownership no ponto da substituição.
Emprestar, mover ou chamar um construtor pode mudar semântica e custos; nenhuma
ação automática deve ser aplicada só pela coincidência de tipo. Candidatos devem
ser limitados, ordenados deterministicamente e revalidados na revisão atual.
Efeitos disponíveis são os do
[contrato atual](../arandu-effects-v0.1.md); não pressupor capabilities ou
restrições transitivas futuras já implementadas.

### Barreira antes de qualquer execução

Um programa que conserva holes deve ser bloqueado antes de codegen e execução:
`build`, JIT de `run`, testes, benchmarks, REPL e avaliação CTFE que dependa do
hole. Não se propõe o flag `--allow-todo` nem lowering silencioso para trap.
Uma sessão pode continuar analisando/formatando código incompleto; isso não é
permissão para avaliar um programa inválido.

Uma futura avaliação parcial isolada precisaria de proposta própria que prove
independência da unidade executada e preserve a barreira. A literatura de live
programming inspira o design, mas não aprova essa exceção automaticamente.
`run --release` também não é o modo atual de execução da CLI.

### Diagnósticos, CLI e editor

O goal, contexto e candidates devem permanecer em DTOs estruturados, com spans,
replacements e identidade de revisão. `T_TYPED_HOLE` é somente nome conceitual,
não `DiagCode` existente; um código real exigirá registro, catálogo e documento
em inglês. Quick fixes não devem parsear texto de mensagem nem editar resultado
stale. A apresentação deve compartilhar tipos/assinaturas com completion/hover.

## 3. Ferramentas propostas

### REPL

O JIT existente é uma base, não uma sessão persistente. O contrato futuro deve
definir módulos/células, redefinição de símbolos, invalidação, ownership das
variáveis de sessão, falhas e teardown. `ReplSessionFrame`, `:type`, `:layout` e
`:amir` são ideias de interface, não APIs implementadas. Não há evidência para
prometer compilação de célula em menos de 2 ms.

A sessão não pode manter snapshot Salsa na thread durante `set_text`. Tampouco
pode preservar ponteiros/funções JIT antigos sem provar que os recursos e dados
usados continuam vivos. Saída e cancelamento devem liberar estado corretamente.

### Bindgen C

A intenção é gerar declarações para um subconjunto explícito de headers C e
ABI de alvo. Isso depende da
[RFC 0030](0030-universal-ffi-zero-copy-and-polyglot-export.md).
Pré-processamento, includes, macros, unions, bitfields e diferenças de ABI
precisam de regras explícitas; casos não suportados devem falhar com diagnóstico.

Não basta conhecer `const T*` e um parâmetro `len` para inferir wrapper seguro:
provenance, retenção, alias, mutabilidade e liberação precisam de contrato.
Atributos `@ExternOpaque`/`@FfiResource` e inferência automática de destrutor não
são superfícies prontas. Tipos target-sized não podem ser mapeados a `uint`
fixo por conveniência; layout/header/assinatura precisam de comparação com a
toolchain C do alvo. Um frontend enxuto não torna todo SDK C autossuficiente
nem elimina a necessidade de sysroot/toolchain nativa.

### Linter

A proposta é reutilizar HIR/AMIR e análises de ownership para lints opt-in de
moves/cópias evitáveis, mutabilidade e alocações. Nomes `W_NEEDLESS_OWN_PARAM`,
`W_LARGE_STACK_COPY` etc. do texto original são conceitos, não códigos existentes.
Limites de 128/256 bytes não têm gate aprovado nem evidência universal de ganho.

Replacements que mudam `own` para `ref` precisam validar lifetime, callers,
contratos públicos e efeitos. Um agregado grande ou uma alocação local não é,
isoladamente, bug. `dyn` e stack promotion geral não podem ser presumidos como
soluções implementadas. Lints não substituem o formatter puro existente.

## 4. Invariantes e evidência exigida

A implementação deve preservar pipeline CST→AST→resolve→typeck→AMIR, queries
puras e resultados estreitos por item. APIs de queries propostas não devem ser
tratadas como `type_check_item`/`lower_amir_item` já existentes com esse contrato.
Uma edição deve invalidar o necessário e manter cutoff quando o resultado
semântico não muda; debounce, prioridade e filas limitadas continuam obrigatórios.

Protocolo de holes/actions exige regressões stdio LSP com initialize,
cancelamento, shutdown, Unicode/UTF-16, edições incrementais e resultados stale.
O [harness stdio atual](../../crates/arandu_lsp/tests/stdio_lifecycle.rs) serve
como base; sua existência não prova holes que ainda não foram implementados.
A extensão exige testes reais no
[Extension Host](../../editors/vscode/src/test/runTest.ts), incluindo aplicação
segura dos replacements. `tsc` ou teste unitário de DTO não substitui essa prova.

REPL/bindgen/lints exigem contratos e regressões próprios; gates e prioridades
permanecem no roadmap mestre, sem checklist executivo paralelo nesta RFC.

## 5. Referências e Literatura Base

1. **Omar, C., Voysey, I., Hilton, M., Aldrich, J., & Hammer, M. A. (2017 & 2019)** — *Hazelnut: A Bidirectionally Typed Structure Editor Calculus* (POPL 2017) & *Live Functional Programming with Typed Holes* (POPL 2019).
2. **GHC Team & HallGrímsson et al. (2014–2022)** — *Typed Holes and Valid Hole Fits in the Glasgow Haskell Compiler*.
3. **de Moura, L., & Ullrich, S. (2021)** — *The Lean 4 Theorem Prover and Programming Language: Metavariables, Holes, and Delaboration*. CADE-28.
