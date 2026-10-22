# Benchmarks do compilador

Esta área documenta metodologias permanentes. Resultados de uma máquina ou de
uma execução específica são artefatos de CI em `target/benchmarks/`, não texto
versionado.

## RFC 0011 — build incremental nativo

Execute, a partir da raiz do workspace:

```bash
cargo run --locked -p xtask -- bench-incremental --verify-determinism
```

O harness compila o CLI e a runtime uma vez, cria projetos descartáveis sob
`target/bench-incremental/` e mede cinco classes de edição: toque sem mudança de
bytes, documentação/trivia, corpo privado, assinatura e novo módulo. Cada linha
registra tempo de parede, queries reexecutadas, CGUs reaproveitadas/recompiladas,
caminho de linker, SHA-256 do executável e timers internos por fase. O harness
ativa `-Ztime-passes` apenas nas cinco medições; timers são observacionais e não
participam das chaves de cache nem dos bytes emitidos. O campo `linker` vale
`incremental-reuse` quando todas as CGUs e o executável verificado puderam ser
reutilizados sem nova etapa de link.

Como oráculo de correção, o mesmo programa é compilado em diretórios de
comprimentos diferentes com `RAYON_NUM_THREADS=1` e `RAYON_NUM_THREADS=16`.
Hashes diferentes fazem o comando falhar. O gate prova reprodutibilidade do corpus mínimo exercitado; os
tempos são evidência observacional e não devem ser usados como limite rígido em
runners compartilhados.

Saídas:

- `target/benchmarks/incremental.json` — dados estruturados, schema 2; cada
  medição contém `phase_ms`, um mapa ordenado de fase para milissegundos;
- `target/benchmarks/incremental.md` — tabela legível para inspeção e artefatos
  de CI, seguida pelo detalhamento de fases.

Limite auditado em 2026-10-15: o harness usa duas funções e executa uma
amostra por classe de edição. Não registra distribuição/p95, CPU/RSS nem um
host de referência. Ele não certifica o gate de `0.2.x` (p95 ≤100 ms) ou de
`0.3.x` (p95 ≤10 ms), nem representa o custo de projetos grandes.

Os critérios e a fila de evolução estão no
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md). Para certificar um marco,
o relatório deve identificar corpus/host/toolchain/filesystem, intervalo de
tempo medido, warm-up, repetições e distribuição; comparar sessão quente,
batch e clean; verificar artefatos equivalentes e recuperação após interrupção.
Retenção/CPU/RSS exigem medições próprias. Estes são requisitos da metodologia
futura, não opções disponíveis no comando atual.

Ao comparar mudanças, use a mesma máquina, perfil de energia, toolchain, estado
térmico e filesystem. Faça várias execuções e compare distribuições, não apenas
uma amostra isolada.
