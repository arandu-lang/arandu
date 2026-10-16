# RFC 0003: Processamento Paralelo Estruturado e Worker Pool Bounded

- **Número da RFC:** 0003
- **Título:** Concorrência e Processamento Paralelo Estruturado via Worker Pool Bounded
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-11
- **Status:** `Implemented` no recorte funcional; promoção `gold` ainda exige evidência específica
- **Área Principal:** `Runtime` / `Middle-end` (`arandu_mir` / `arandu_runtime`)

---

## 1. Resumo (Summary)

Esta RFC define o modelo de concorrência e paralelismo estruturado do Arandu. Nenhuma tarefa ou worker filho pode escapar do escopo léxico que a instanciou. Erros, falhas e cancelamentos são tratados de modo determinístico no limite do escopo, eliminando vazamentos de tarefas (*detached tasks*), contenção descontrolada de threads e corridas de dados (*data races*).

---

## 2. Motivação (Motivation)

Sistemas concorrentes tradicionais sofrem com duas armadilhas antagônicas:
1. **Threads e Promises Destacadas (Go goroutines, JS/Node Promises, C++ `std::thread`)**: Tarefas filhas continuam executando em segundo plano mesmo após a função pai retornar ou falhar, causando vazamentos silenciosos de recursos, acessos a ponteiros inválidos e consumo desgovernado de memória.
2. **Concorrência com Locks Explícitos (Pthreads, Rust `Mutex<T>`)**: Degradação severa de escalabilidade sob alta concorrência por contenção de cache (*cache bouncing*), deadlocks e não determinismo na ordem de execução.

O Arandu resolve esses problemas através de **Paralelismo Estruturado**: as tarefas filhas são confinadas pelo sistema de tipos (`Send` + `Sync` bounds) e pelo ciclo de vida do bloco léxico. A redução dos dados ocorre através de operadores associativos determinísticos.

---

## 3. Topologia e Fluxo de Execução

```text
Entrada (fatia []T ou coleção contígua)
           │
           ▼
 particionamento fixo pela entrada
 (independente de workers)
           │
           ▼
  WorkerPool bounded ──► [Worker 0] ... [Worker N] (WorkThunk ABI)
           │
           ▼
  redução ordinal associativa (Combine<R>)
           │
           ▼
 resultado estável entre contagens de workers
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### Contrato de ABI e Transporte de Tarefas

Todo trabalho paralelo atravessa a fronteira entre compilador e runtime via ponteiro com convenção C ABI:
```c
int32_t (*ar_work_thunk)(void *context, void *result);
```
* `context`: Ponteiro para a struct contendo o subintervalo de dados e argumentos de entrada.
* `result`: Ponteiro para a região onde o resultado do chunk será gravado pelo worker.
* Retorno: `0` para sucesso, `2` para cancelamento e outro código não zero para falha controlada.

### Inlining de Funções-Folha na AMIR

O passe `arandu_mir::inlining` preserva SSA/OSSA e usa custos limitados:
* Funções-folha têm orçamento de 32 operações e até 12 blocos;
* Wrappers não-folha pequenos admitem até 24 blocos e 4 chamadas, com prova
  transitiva dos alvos e custo adicional por chamada;
* Ciclos no CFG e terminadores `Suspend` impedem a transformação;
* O orçamento cumulativo mede statements do callee, cresce com o tamanho
  original do caller e tem teto de IR expandida; não é limite fixo de 32 sites.

Os detalhes canônicos estão em `arandu_mir::inlining::{cost,splice}`.

---

## 5. Invariantes de Arquitetura

1. **Escopo Imutável**: Um grupo paralelo não pode concluir sua execução até que todas as tarefas admitidas tenham terminado ou sido cooperativamente canceladas.
2. **Partição e Ordem Estáveis**: Em `parallelFold`, os limites de chunks derivam apenas da entrada e os parciais são combinados em ordem ordinal. Alterar somente a contagem de workers não altera a árvore de redução.
3. **Contrato Algébrico Explícito**: `Combine` deve ser associativo, possuir a identidade fornecida como elemento neutro e ser estável sob o reagrupamento. Isso não implica igualdade com fold linear para aritmética IEEE-754 comum.
4. **Limitação de Admissão (*Bounded Admission*)**: A fila possui capacidade finita. Chamadas externas aguardam admissão; submissões aninhadas feitas por workers executam inline para impedir deadlock e crescimento ilimitado.

O runtime Rust usa workers persistentes. O backend C também possui worker
pool reutilizável, com pthreads em POSIX e primitivas Win32 em Windows, fila
bounded, participação da thread chamadora e execução inline aninhada.

## 6. Estado auditado em 15/10/2026

`parallelFoldWithInit` e a interface `AccumulatorInit<R>` já permitem
acumuladores não-`Copy` com o bound `Send` correspondente. Isso não entrega
automaticamente drop glue de recursos arbitrários em falha/cancelamento; a ABI
de retorno agregado também mantém cópias/alocações residuais em certos casos.

O harness `scripts/bench_parallel_scaling.py` já é versionado e mede tempo,
RSS e determinismo com 1/2/4/8/16 workers. Sua existência não equivale a
resultados de promoção em workloads representativos e hosts documentados.
O corpus Pypor e os testes de paridade cobrem o recorte Linux atual.

O SDK v0.1.10 passou instalação e smoke público nos três alvos no
[run 38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029).
Essa evidência não substitui a matriz completa específica de paralelismo em
Windows/macOS. Os limites ficam no
[contrato técnico](../arandu-structured-parallelism-v0.1.md); a fila de execução
permanece exclusivamente no roadmap mestre.
