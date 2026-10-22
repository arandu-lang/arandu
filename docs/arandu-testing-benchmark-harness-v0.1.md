# Arandu — Testing & Benchmark Harness v0.1

**Estado:** `done`; implementação e matriz nativa concluídas, promoção formal
`gold` condicionada ao soak operacional descrito abaixo.

## Visão Geral e Contexto

O harness torna testes e microbenchmarks recursos do produto Arandu. Projetos
descobrem casos por `@Test` e `@Benchmark`, compilam pelo mesmo pipeline
CST-first e executam por `arandu test` e `arandu bench`. O SDK público não
depende de Rust, Cargo, Python, `xtask` ou checkout do compilador.

As decisões foram confrontadas com Go `testing`, Swift Testing, `zig test`, o
runner do Rust, Google Benchmark e as recomendações de benchmarking do LLVM.
Arandu adotou descoberta explícita, IDs hierárquicos, isolamento por processo,
warmup e amostras auditáveis, evitando nomes mágicos, protocolo baseado em
stdout humano e regressão obrigatória em runner compartilhado.

```text
fonte + @Test/@Benchmark
        │
        ▼
CST → AST → resolve → typeck → AMIR → backend
        │
        ▼
manifesto determinístico + executável de harness
        │
        ├── IPC enquadrado/versionado ──► CLI/reporters/VS Code
        └── stdout e stderr capturados separadamente
```

## Detalhes Técnicos da Implementação

### Responsabilidade por camada

| Camada | Responsabilidade atual |
| --- | --- |
| `arandu_parser` | Preserva anotações no CST/AST canônico. |
| `arandu_semantics` | Valida alvo e assinatura de `@Test`/`@Benchmark`. |
| `arandu_query` | Produz manifestos incrementais, ordenados e hash-estáveis. |
| `arandu_codegen` | Define contratos v1 e gera registries/entrypoints. |
| `arandu_mir` | Baixa expectativas e preserva `BlackBox` contra DCE. |
| backends C/Cranelift | Emitem harness e barreiras de otimização equivalentes. |
| `arandu_runtime` | Contexto, falhas, logs, temporários, cleanup, relógio e ABI. |
| `arandu_cli` | Build, seleção, processos, timeout, reporters e baselines. |
| extensão VS Code | Consome listagem/JSON canônicos; não reimplementa análise. |
| `xtask` | Verifica o SDK instalado; não é dependência do usuário. |

### Descoberta e identidade

- `@Test`: função livre, síncrona, não genérica, sem parâmetros, retornando
  `void` ou `Result<void, E>`.
- `@Benchmark`: função livre, síncrona e não genérica com
  `mut bench: testing.Benchmark`, retornando `void`.
- IDs públicos derivam de pacote, alvo, módulo e símbolo; paths absolutos,
  spans, offsets, `FileId` e ordem de mapas não participam.
- `src/` e `tests/` entram no grafo real de projeto. Não existe scanner textual,
  reflexão em runtime ou segunda rota de parsing.

### CLI pública

```text
arandu test [projeto] [--list] [--filter texto] [--exact id]
            [--jobs N] [--timeout segundos] [--seed N] [--fail-fast]
            [--format human|json|junit] [--output arquivo]

arandu bench [projeto] [--list] [--filter texto] [--exact id]
             [--warmup segundos] [--measurement-time segundos]
             [--samples N] [--timeout segundos] [--format human|json]
             [--save-baseline nome | --compare nome]
             [--strict] [--dry-run]
             [--max-regression percentual]
             [--noise-threshold percentual]
```

O filtro é substring literal Unicode; regex não é interpretada. A seed muda a
ordem de início, mas resultados finais voltam à ordem canônica. Paralelismo
ocorre somente entre processos isolados.

### Processo, protocolo e reporters

Cada caso recebe processo e grupo/árvore próprios. IPC, stdout e stderr são
drenados separadamente para evitar deadlock e impedir que saída hostil imite
eventos internos. Timeout encerra descendentes e sempre faz reap do filho.

Contratos compartilhados:

- `arandu.test/v1`;
- `arandu.test-list/v1`;
- `arandu.bench/v1`;
- `arandu.bench-list/v1`;
- `arandu.bench-baseline/v1`.

Frames usam magic `ARND`, tamanho validado e payload máximo de 2 MiB. Estados
de teste são `passed`, `failed`, `skipped`, `timed_out` e `crashed`. JSON é o
contrato rico; JUnit é uma projeção portátil para CI. Arquivos de resultado e
baseline usam publicação atômica.

### `std.testing` e benchmark

`std.testing` expõe expectativas tipadas, `fail`, `skip`, `log`, `tempDir`,
`Benchmark.loop` e `blackBox`. Falhas preservam operação, expressão, valores,
tipo, localização, causa e falhas secundárias. Temporários são contidos e
limpos com proteção contra escapes por symlink/junction.

O benchmark descarta warmup, calibra iterações, mede em batches e preserva
amostras brutas. Reporta mediana, MAD e p95. `blackBox` baixa para
`AmirRvalue::BlackBox`; DCE e ambos os backends conhecem a barreira. Baselines
ficam em `target/arandu/benchmarks/` e só comparam ambientes compatíveis.

### Editor, distribuição e evidências

O Test Explorer executa `arandu test --list --format json` em background e usa
IDs canônicos para rodar casos. O comando `Arandu: Run Benchmark` consome a
listagem de benchmark. O VSIX instalado é testado contra o CLI/LSP do SDK, sem
fallback para o monorepo.

O gate `SL_T / Harness` exige VSIX e SDK nativo em:

| Sistema | Alvo |
| --- | --- |
| Linux | `x86_64-unknown-linux-gnu` |
| macOS | `aarch64-apple-darwin` |
| Windows | `x86_64-pc-windows-msvc` |

`xtask check-slt6-sdk` instala fora do checkout, remove overrides de
desenvolvimento e prova `new → check/build/run → test → JUnit → bench →
baseline/compare`. A campanha cobre frames hostis, crash, timeout, concorrência,
Unicode, CRLF, filesystem adversarial e recuperação após publicação interrompida.

### Topologia de promoção no CI

O check obrigatório `S0 / Gate` é um agregador estável, não um executor
monolítico. Um classificador conservador roda dentro do workflow obrigatório e
seleciona contratos por superfície; assim um workflow nunca desaparece por
filtro de paths e deixa a proteção da `main` pendente.

- política, arquitetura, documentação, LF e `rustfmt` rodam em todo PR;
- mudanças de produto executam uma única suíte `cargo test --workspace` no
  Linux e a mesma suíte em Windows nativo, em paralelo;
- determinismo de diagnóstico permanece uma prova própria de 1 versus 8
  threads;
- extensão e distribuição rodam no PR somente quando suas superfícies são
  afetadas;
- testes individuais P3/P4, minimal Gold e integrações não ignoradas não são
  repetidos depois da suíte completa;
- endurance, budgets e LSP stress ignorado rodam semanalmente ou por despacho
  manual;
- a matriz `SL_T / Harness` roda semanalmente, manualmente e em PRs que alteram
  SDK, VSIX ou distribuição; tags continuam usando a matriz pública de release.

O ruleset existente da `main` ainda exige os nomes históricos S1 e S2 por
sistema. Jobs agregadores preservam esses contextos sem recompilar: S1 promove
a evidência nativa já produzida, e S2 confirma os regressions não ignorados da
suíte do PR. A campanha longa correspondente tem nome próprio e frequência
agendada. `S0 / Gate` também valida esses agregadores, portanto nenhum deles
pode mascarar falha ou execução indevidamente pulada.

Setup Rust e classificação de impacto vivem em ações compostas locais sob
`.github/actions/`. Elas centralizam versões, cache e ownership de paths sem
adicionar um serviço ou uma dependência de runtime ao SDK.

## PONTOS DE MELHORIA (O que não está no roadmap)

- `arandu_cli/src/test_runner.rs` ainda concentra processo, protocolo,
  reporters JSON/humano e baseline. Estatística de benchmark e serialização
  JUnit já vivem em submódulos puros; as demais extrações devem acompanhar a
  próxima mudança funcional dessas superfícies.
- O runtime possui cleanup LIFO interno, mas a stdlib ainda não oferece uma API
  pública genérica `testing.cleanup`.
- O Test Explorer executa seleções sequencialmente pelo cliente; batching pode
  reduzir criação de processos sem alterar IDs ou protocolo.
- A identidade de CPU do benchmark é necessariamente limitada em runners
  hospedados; resultados de performance ali continuam informativos.
- Relatórios JUnit preservam menos dados que JSON por limitação do formato.
- Ajuda da CLI ainda está concentrada no dispatcher principal e merece contrato
  de parsing modular e testável.
- O mapa de impacto do CI é deliberadamente conservador e manual. Novos crates,
  scripts de pacote ou consumidores do protocolo precisam atualizar a ação de
  classificação no mesmo PR.
- Os quatro contextos históricos S1/S2 podem sair do ruleset da `main` quando a
  equipe decidir manter somente o agregador `S0 / Gate`; até lá são preservados
  por compatibilidade explícita, não por repetição da suíte.

## Futuro e Próximos Passos

- Cumprir 10 execuções consecutivas verdes durante pelo menos 7 dias e então
  promover `done` para `gold` no roadmap mestre.
- Testes async somente após scheduler, cancelamento e relógio testável de
  `SL_R` estarem estáveis.
- Avaliar testes parametrizados, fixtures e property testing em RFC própria.
- Coverage exige instrumentação equivalente entre backends antes de ser
  anunciado como recurso.
- Regressão de performance só vira gate obrigatório em runner dedicado,
  identificado e calibrado; baseline não será atualizado automaticamente por PR.
- A campanha local 0.1.11 usa `cargo nextest run --workspace --locked
  --profile verification --test-threads 2`, por orientação do mantenedor.
  O watchdog desse perfil detecta travamentos; não substitui os orçamentos
  internos de latência nem comprova ganho de performance. Nextest isola cada
  teste em um processo e não executa doctests; estes exigem `cargo test --doc`.
  A eventual migração do runner da CI exige medição própria de compilação e
  execução, preservando a cobertura.
- Fuzzing geracional, perf counters portáveis, execução remota e plugins de
  runner permanecem fora do contrato v1.

Os antigos roadmaps SL_T.3–SL_T.6 foram removidos após a consolidação. O histórico
de pesquisa e implementação permanece recuperável no Git e nas evidências de CI.
