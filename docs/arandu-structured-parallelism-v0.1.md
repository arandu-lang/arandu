# Arandu — Processamento Paralelo Estruturado v0.1

**Estado:** `done`, ainda não `gold`. O caminho funcional está integrado no
runtime, nos backends C/Cranelift e em `std.parallel`; a prova atual cobre
Linux x86-64 e o consumidor Pypor. O harness reproduzível já é versionado;
promoção exige resultados representativos e a matriz específica de paralelismo
nativo Windows/macOS. O smoke público do SDK v0.1.10 nos três alvos, aprovado
no [run 38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029)
e verificado em 15/10/2026, não substitui essa matriz.

## Visão Geral e Contexto

O processamento paralelo estruturado provê execução paralela com ciclo de vida
confinado: a chamada só retorna depois que todo trabalho admitido termina. O
runtime seleciona falhas pelo menor ordinal de chunk, e cancelamento anterior à
execução nunca é reportado como sucesso nem deixa buffers não inicializados
serem consumidos.

As decisões arquiteturais foram confrontadas diretamente com as abordagens de mercado, adotando as melhores práticas e evitando armadilhas conhecidas:

- **Swift SE-0304 (Structured Concurrency):** Escopo estruturado estrito onde tarefas filhas não escapam do grupo. Evitou-se o modelo de tarefas destacadas (*detached tasks*) que vazam recursos.
- **Java `StructuredTaskScope` (JEP 453):** Ciclo de vida confinado ao bloco léxico. Evitou-se a falha do JDK-8311867 (onde uma tarefa admitida entre `shutdown()` e o cancelamento escapava da interrupção) por meio de verificação atômica pré-admissão combinada com o token de cancelamento.
- **Go `errgroup`:** Limite de admissão estrito para proteger o sistema contra sobrecarga de concorrência. Evitou-se o erro da estrutura padrão do Go (`Group{}` ilimitado que acumula memória sem controle).
- **.NET `Task.WhenAll`:** Agrupamento determinístico de resultados. Evitou-se a armadilha do .NET de re-lançar apenas a primeira exceção por corrida não determinística; o Arandu registra e consolida resultados em ordem estável.
- **Rayon / C++26 `std::execution`:** Redução paralela e particionamento contíguo de fatias sem alocação por item. Evitou-se a cerimônia excessiva de montagem de *senders/receivers*.
- **Inlining orçado no AMIR:** funções-folha e wrappers pequenos possuem
  limites de custo, prova transitiva e teto de crescimento; ciclos no CFG e
  suspensão impedem a transformação.

```text
Entrada (fatia []T ou coleção)
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

## Detalhes Técnicos da Implementação

### Responsabilidade por Camada

| Camada | Responsabilidade |
| :--- | :--- |
| `arandu_typeck` | Bounds canônicos `Copy`, `Send` e `Sync` via `LangItem`. Rejeição de storage emprestado, ponteiros crus, handles cooperativos e valores com destrutor quando a capacidade exigida não pode ser provada. |
| `arandu_middle` | Definição de layout, ABI de rvalues, terminadores e contratos de funções e tipos compartilhados. |
| `arandu_mir` | Otimizações, preservação de SSA/OSSA e inlining automático de funções-folha (`arandu_mir::inlining`) com splicing puro de CFG e remapeamento denso de ranges. |
| `arandu_runtime` | `WorkerPool` reutilizável, admissão limitada (`sync_channel`), execução inline de trabalho aninhado, cancelamento atômico e ABI `ar_rt_parallel_fold_run`. |
| `arandu_backend_cranelift` | Tradução de chamadas C ABI, JIT builder com registro de símbolos runtime, resolução de tipos de agregados em memória e materialização de cópia de structs por valor. |
| `arandu_backend_c` | Emissor C com worker pool reutilizável (`pthread` ou Win32), batches estruturados, fila bounded, work-sharing na thread chamadora, self-help inline contra deadlock e o mesmo critério ordinal de falha. |
| `stdlib` | `parallelFold` com identidade explícita, seed aplicada uma vez, chunks fixados pela entrada e quatro slabs alinhados por operação — nenhuma chamada ao alocador por item ou por chunk. `parallelFoldWithGrain` permite que consumidores de custo irregular escolham limites explícitos sem alterar o default. |
| `pypor` | Consumidor ponta a ponta. O teste cruza o cutoff com 1.025 itens e compara 1/2/4/8 workers; listagem de nomes permanece sequencial porque sua saída é observável. |

---

### Contrato de ABI e Transporte de Tarefas

1. **Assinatura do Thunk de Trabalho:**
   Todo trabalho paralelo atravessa a fronteira entre compilador e runtime via ponteiro C ABI:
   ```c
   int32_t (*ar_work_thunk)(void *context, void *result);
   ```
   Retornos de status:
   - `0`: Sucesso (`WORK_COMPLETED`). O buffer `result` contém o valor de retorno inicializado.
   - `1`: Falha na execução da tarefa.
   - `2`: Cancelado antes ou durante a execução (`WORK_CANCELED`).

2. **Trabalho Aninhado e Prevenção de Deadlock:**
   Uma submissão feita de dentro de um worker executa inline. Isso evita o ciclo
   em que todos os workers aguardariam filhos enfileirados, independentemente de
   ainda existir espaço na fila de admissão.

3. **Inlining Automático de Funções-Folha (AMIR):**
   Pequenas funções utilitárias (como testes de caracteres e predicados) são automaticamente inlinadas nos callers antes do laço de fixpoint do otimizador:
   - **Elegibilidade:** funções-folha ou wrappers pequenos com alvos
     transitivamente elegíveis, sem `Suspend` ou ciclos no CFG.
   - **Orçamento:** folhas têm custo até 32 operações e 12 blocos; wrappers
     não-folha admitem até 24 blocos e 4 chamadas com custo adicional. O
     orçamento cumulativo de statements depende do tamanho original do caller
     e do teto de IR expandida; não existe limite fixo de 32 sites por caller.
   - **Splicing SSA:** O registrador de retorno `TempId(0)` da callee é mapeado diretamente para o registrador SSA de destino do caller (`call.lhs`), os blocos intermediários são inseridos e as tabelas de statements e parâmetros de bloco são reconstruídas de forma contígua e densa.
   - **Sinergia com Passos Existentes:** Após o splice, `simplify_cfg` funde os blocos sequenciais e `sccp` dobra constantes diretamente nos locais de uso.

---

## Evidência atual

- Testes unitários do runtime exercitam pre-cancelamento, inicialização de todos
  os resultados, alinhamento e escolha da falha de menor ordinal.
- O backend C e Cranelift executam o `parallelFold` real no harness do Pypor e
  nos testes de paridade (`parity_tests.rs`), incluindo:
  - `parity_parallel_fold_non_copy`: fold paralelo com fábrica de acumuladores
    `AccumulatorInit` para tipos sem semântica `Copy`.
  - `parity_parallel_float_determinism`: validação de determinismo bit-a-bit
    em reduções de `float` entre 1, 2, 4 e 8 workers.
- O teste Pypor usa 1.025 elementos, portanto cruza o cutoff de 1.024, verifica
  que a seed é aplicada uma vez e compara 1/2/4/8 workers.
- A regressão de integração exercita `parallelFoldWithGrain` com um chunk por
  item, compara 1/4 workers e rejeita política com grão zero sem panic.
- A regressão de inlining cobre o predicado de três alternativas e o
  classificador branch-only usados pelo scanner do Pypor. No corpus Linux, a
  remoção das chamadas quentes reduziu o fold de um worker de 18.403 ms para
  15.107 ms (17,9%); em oito workers, o end-to-end mediano passou de 2,36 s
  para 2,32 s, quando I/O e contenção já dominam o ganho restante.
- O pool LSP prova fila limitada, prioridade, coalescing, cancelamento e join no
  shutdown. O runner de testes converte panic de worker em evento `Crashed`.
- Harness versionado e reproduzível disponível em `scripts/bench_parallel_scaling.py`,
  medindo wall time, user time, sys time, RSS máximo e determinismo estrito com
  ordem alternada de execução para mitigar viés térmico/cache.

## Limites semânticos

### 1. Contrato Canônico de Redução de Ponto Flutuante (IEEE 754)
A adição de ponto flutuante não é associativa devido ao arredondamento IEEE-754:
$(a + b) + c \neq a + (b + c)$.

No modelo de paralelismo estruturado do Arandu:
- **Particionamento desacoplado de workers:** O número e os limites de cada chunk
  são determinados unicamente pelo comprimento da fatia (`total_len`) e pelo
  limiar de granularidade (`granularityCutoff()`), nunca pela contagem de workers.
  No `parallelFoldWithGrain`, `target_chunk_size` e `max_chunks` também fazem
  parte explícita dessa política sem depender do agendador.
- **Redução ordinal estrita:** A combinação dos resultados parciais ocorre em
  ordem ordinal canônica $0, 1, \dots, K-1$.
- **Garantia de determinismo entre workers:** Para qualquer entrada fixada,
  executar `parallelFold` com 1, 2, 4, 8 ou $N$ workers produz saídas **bit a bit
  idênticas**. O agendamento é pura estratégia de execução concorrente e não afeta
  a semântica.
- **Relação com `foldSequential`:** Para entradas acima do cutoff ($N > 1.024$), o
  agrupamento intermediário dos chunks introduz uma parentização de soma diferente
  de um fold linear contínuo, podendo resultar em diferenças de arredondamento
  menores em relação a `foldSequential`. Para $N \le 1.024$, o pipeline executa
  automaticamente o caminho sequencial.

### 2. Política Explícita para Cargas Irregulares

`parallelFold` conserva a política geral de 64 itens-alvo e no máximo 1.024
chunks. Consumidores em que o custo por item varia muito podem chamar
`parallelFoldWithGrain` com `target_chunk_size` e `max_chunks` próprios.

Esses dois valores são parte da semântica da árvore de redução: precisam ser
mantidos constantes ao comparar quantidades de workers. Valores zero são
rejeitados com `ParallelError.Failed(-3)`, e os mesmos limites de tamanho,
alinhamento e quatro slabs contíguos continuam valendo. A API não cria uma
tarefa ou uma alocação individual para cada item; workers retiram os chunks dos
slabs por ordinal usando o contador atômico compartilhado.

### 3. Acumuladores Não-`Copy` via `AccumulatorInit`
Para tipos acumuladores que requerem inicialização independente e não possuem
`marker.Copy` (evitando duplicação bitwise da identidade que causaria aliasing
ou double-free):
- A interface `AccumulatorInit<R>` fornece um método `init(self: ref Self): R`.
- A primitiva `parallelFoldWithInit` delega a inicialização do acumulador a cada
  worker/chunk antes do processamento dos itens.
- O tipo de retorno requer apenas `marker.Send`, eliminando a restrição de `Copy`
  para reduções ricas.

### 4. Portabilidade Multiplataforma do WorkerPool
O runtime C possui implementações especializadas e equivalentes:
- **Windows (MSVC/MinGW):** Sincronização via `CRITICAL_SECTION` e
  `CONDITION_VARIABLE`, inicialização estática com `INIT_ONCE`, contagem de
  processadores com `GetSystemInfo` e criação de threads via `CreateThread`.
- **POSIX (Linux/macOS):** Sincronização via `pthread_mutex_t` e `pthread_cond_t`,
  inicialização com `pthread_once` e topologia com `sysconf(_SC_NPROCESSORS_ONLN)`.
- Ambas as variantes implementam admissão bounded (`AR_PARALLEL_QUEUE_CAP`),
  work-sharing na thread chamadora e self-help inline (`ar_c_in_worker`) para
  prevenção de deadlocks sob chamadas aninhadas.

## PONTOS DE MELHORIA (O que não está no roadmap)

A ABI de agregados `Copy` por valor ainda depende de simplificação no backend Cranelift para evitar cópias residuais, e o transporte com drop glue automático integrado ao type checker deve ser formalizado quando destrutores explícitos forem adicionados ao sistema de tipos.

## Futuro e Próximos Passos

A fila permanece no roadmap mestre. Os limites deste contrato são a promoção
nativa específica, resultados reproduzíveis em mais de um workload, ABI medida
de retorno agregado e eventual transporte com drop glue automático. A fábrica
`AccumulatorInit` já admite acumuladores não-`Copy`; isso não comprova destruição
automática de recursos arbitrários em todos os caminhos de falha/cancelamento.
