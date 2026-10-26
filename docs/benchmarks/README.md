# Benchmarks do compilador

Esta área documenta metodologias permanentes. Resultados de uma máquina ou de
uma execução específica são artefatos de CI em `target/benchmarks/`, não texto
versionado.

## RFC 0011 — build incremental nativo

Execute, a partir da raiz do workspace:

```bash
cargo run --locked -p xtask -- bench-incremental --verify-determinism
cargo run --locked -p xtask -- bench-incremental --release --corpus all --samples 50 --warmup 5
```

O primeiro comando é o padrão de CI: corpus `small`, 5 amostras por classe,
1 build de aquecimento e CLI debug. O segundo é a forma de medição de marco.
Opções: `--corpus small|medium|large|all`, `--samples N` (≥1), `--warmup N` e
`--release` (compila CLI e runtime em release). A verificação de determinismo
roda sempre; `--verify-determinism` é aceito por compatibilidade.

O harness compila o CLI e a runtime uma vez e gera, sob
`target/bench-incremental/`, um projeto por corpus: `small` (4 módulos × 8
funções), `medium` (16 × 32) e `large` (64 × 64). O módulo `k` importa o
módulo `k - 1` e a entrada chama o último, então todos os módulos são linkados.
As edições atingem o módulo do meio da cadeia de imports. Classes:

| Classe                | Edição                                                                                  |
| --------------------- | --------------------------------------------------------------------------------------- |
| `noop-touch`          | reescreve os mesmos bytes                                                               |
| `documentation`       | muda o doc comment de um helper privado                                                 |
| `private-body`        | muda uma constante imm32 de um helper privado; o tamanho do código de máquina se mantém |
| `private-body-growth` | acrescenta uma instrução ao helper a cada amostra; o tamanho cresce                     |
| `public-signature`    | alterna a aridade da API pública do módulo e reescreve o único chamador                 |
| `new-item`            | adiciona uma função pública chamada pela API do módulo                                  |
| `clean`               | remove `target/` e recompila (no máximo 5 amostras, sem aquecimento)                    |

Cada classe parte do estado base já compilado. Toda amostra produz um texto
que nenhuma amostra anterior produziu, para que o reuso de objetos por conteúdo
não esconda o codegen. As amostras de aquecimento são descartadas.

Cada amostra registra tempo de parede da invocação do CLI até sua saída, CPU
(usuário + sistema) e pico de RSS da árvore de processos coletada pelo CLI
(via `wait4`, apenas Unix), queries reexecutadas, CGUs reaproveitadas e
recompiladas, tamanho e SHA-256 do executável, timers por fase
(`-Ztime-passes`) e o caminho de publicação: `up-to-date` (cutoff integral),
`reuse` (todas as CGUs e o executável verificado reaproveitados), `patch`
(patch ELF in-place) ou `full-link`. `patch_fallback` marca as amostras em que
o patch foi tentado e recusado. Por classe, o relatório resume p50/p95/máximo
/média por nearest-rank (o mesmo método de `check-project-performance`), CPU
p50, pico de RSS, distribuição de caminhos, taxa de patch sobre os builds que
religaram e p50 de cada fase. O relatório também descreve o host: SO,
arquitetura, kernel, CPU, CPUs lógicas, filesystem do diretório de trabalho,
toolchain e commit (com sufixo `-dirty` quando há mudanças rastreadas).

Como oráculo de correção, um programa é compilado em diretórios de
comprimentos diferentes com `RAYON_NUM_THREADS=1` e `RAYON_NUM_THREADS=16`.
Hashes diferentes fazem o comando falhar. Os tempos são observacionais e não
servem de limite rígido em runners compartilhados.

Saídas:

- `target/benchmarks/incremental.json` — dados estruturados, schema 3, com
  configuração, host, resumo e amostras por corpus/classe;
- `target/benchmarks/incremental.md` — tabela legível com o resumo, seguida do
  p50 por fase.

Limites: o corpus gerado usa só inteiros, laços e chamadas diretas, sem
genéricos, stdlib ou async, e o daemon ainda não existe, então todo build é um
processo novo. O harness não mede o delta de tamanho por função, que define a
política de slot padding da HOT.4, nem recuperação após interrupção.

Os critérios e a fila de evolução estão no
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md). Para certificar um marco,
o relatório deve identificar corpus/host/toolchain/filesystem, intervalo de
tempo medido, warm-up, repetições e distribuição; comparar sessão quente,
batch e clean; verificar artefatos equivalentes e recuperação após interrupção.
Sessão quente, recuperação e retenção de memória em sessão longa ainda não são
medidas pelo comando atual.

Ao comparar mudanças, use a mesma máquina, perfil de energia, toolchain, estado
térmico e filesystem. Faça várias execuções e compare distribuições, não apenas
uma amostra isolada.
