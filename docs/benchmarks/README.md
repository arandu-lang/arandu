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
| `private-body-locals` | acrescenta um `let` ao helper a cada amostra: novos símbolos locais no corpo            |
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
o patch foi tentado e recusado; `resized_functions` lista, das funções cujo
código mudou de tamanho, `[bytes antes, bytes depois, slot antes, slot
depois]`: os bytes são os do código antes do padding de slot, gravados no
sidecar de cada CGU (`code_bytes`), e o slot vem do `elf_layout.json`. Por classe, o relatório resume p50/p95/máximo
/média por nearest-rank (o mesmo método de `check-project-performance`), CPU
p50, pico de RSS, distribuição de caminhos, taxa de patch sobre os builds que
religaram, p50 de cada fase e, em `resized_functions`, a distribuição do
delta de bytes e a fração das mudanças que mantiveram o slot (a condição do
patch). O relatório também descreve o host: SO,
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

Com `--daemon`, cada build passa pelo daemon do projeto (HOT.3), como um
usuário o executa; sem ele, cada build é um processo novo com
`-Ztime-passes`. Limites: o corpus gerado usa só inteiros, laços e chamadas
diretas, sem genéricos, stdlib ou async; recuperação após interrupção é
coberta pelos testes do daemon (`cli_daemon.rs`), não por este comando.

Os critérios e a fila de evolução estão no
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md). Para certificar um marco,
o relatório deve identificar corpus/host/toolchain/filesystem, intervalo de
tempo medido, warm-up, repetições e distribuição; comparar sessão quente,
batch e clean; e verificar artefatos equivalentes.

### Resultados da 0.2.0 (host de desenvolvimento)

Linux x86-64 (CachyOS), projeto em btrfs, `bench-incremental --release
--daemon --corpus medium --samples 20 --warmup 3`, determinismo `true`. O
estado térmico e de energia do host variou entre sessões, então só valem
comparações dentro da mesma sessão; nenhuma linha certifica o host de
referência.

| Edição              | 2026-10-28 (antes das rodadas) | 2026-11-06, sessão A | 2026-11-06, sessão B |
| ------------------- | -----------------------------: | -------------------: | -------------------: |
| noop                | 15,5 / 22,5 ms                 | 6,8 / 7,1 ms         | 3,8 / 6,0 ms         |
| corpo privado       | 258 / 314 ms                   | 97,3 / 109,3 ms      | 64,4 / 82,8 ms       |
| corpo cresce        | 398 / 761 ms                   | 115,1 / 193,1 ms     | 70,8 / 136,2 ms      |
| `let` novo          | —                              | 262,9 / 337,6 ms     | 164,5 / 227,8 ms     |
| assinatura pública  | 1948 / 2346 ms                 | 1598 / 1898 ms       | 1158 / 1330 ms       |

Valores p50 / p95. Na sessão B, o corpo que cresce adiciona 6 bytes de código
por amostra e mantém o slot em 80% das edições, que é a taxa de patch medida;
contrato e limites do motor quente estão na
[RFC 0011](../rfcs/0011-incremental-partitioned-aot-and-in-process-linker.md) §8.

Ao comparar mudanças, use a mesma máquina, perfil de energia, toolchain, estado
térmico e filesystem. Faça várias execuções e compare distribuições, não apenas
uma amostra isolada.
