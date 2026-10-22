# RFC 0039: ParkingLot, condições de espera e sincronização por fases

- **Número da RFC:** 0039
- **Data de início:** 2026-10-16
- **Status:** `Draft` — eventos, mutex nativo, guardas compartilhadas e
  admissão phase-fair implementados localmente; promoção multiplataforma pendente.
- **Área:** `Runtime` | `Type System` | `Stdlib` | `Concurrency`
- **Atualização:** 2026-10-18
- **Fila permanente:** [roadmap, ST.10 e ST.11](../arandu-compiler-roadmap-v0.1.md).

## 1. Problema e fronteira atual

Esta RFC separa três contratos: estacionamento de threads por endereço,
liberação/reaquisição de um mutex ao esperar e justiça entre leitores/escritores.
Uma fila de estacionamento correta não estabelece os outros dois contratos.

| Superfície | Estado no código |
| --- | --- |
| Atômicos escalares | Operações nativas; ordens válidas são conservadoramente promovidas a SeqCst. Escritas ainda exigem `mut ref`. |
| ParkingLot Rust | 64 buckets, sequência atômica validada sob o lock do bucket, fila FIFO por endereço exato, espera sem polling e remoção de filas vazias. |
| ParkingLot C | Buckets inicializados uma vez; waiters separados por endereço e sinal; timeout monotônico e limpeza de espera sob cancelamento POSIX diferido. |
| Identidades de threads | IDs não reciclados, registro removido na saída via TLS/FLS e unpark de ID desconhecido/encerrado ignorado. Um único token de park, separado dos sinais de condição. |
| `Condvar` | wait/timeout recebem empréstimo exclusivo de MutexGuard, liberam a aquisição e sempre readquirem antes de retornar; sequência validada sob bucket fecha a janela de notificação. Notify aceita ref compartilhada. |
| `RwLock<T>` | Aquisição por ref compartilhada; filas por endereço, escritores FIFO e fases de leitores em lote, com guardas de liberação única. |
| `Send`/`Sync` | Prova estrutural com regras canônicas de UnsafeCell, Mutex, RwLock e guardas; Copy é negado ao armazenamento com mutabilidade interior. |
| `std.runtime.mutex` | `AsyncMutex<T>` cooperativo com flag comum; não fornece exclusão mútua entre threads nativas. `std.sync.mutex` fornece separadamente a aquisição entre threads nativas. |

O antigo protótipo de bits de escritor pendente foi substituído por um
protocolo de admissão com filas reais. A chave inline identifica armazenamento
estacionário; contadores, filas e monitores permanecem no runtime. Não há
promessa de um lock completo de quatro bytes nem ausência de alocações.

## 2. Protocolo implementado de estacionamento

A chave é o endereço de uma palavra atômica `usize`, alinhada e viva durante
cada chamada. O objeto deve permanecer no mesmo endereço enquanto houver espera
ou notificação em andamento. Todos os acessos concorrentes à palavra devem ser
atômicos; tamanho e alinhamento seguem o alvo.

1. O waiter adquire o bucket, compara a sequência com o valor esperado e retorna
   imediatamente quando ela mudou. Caso contrário, registra uma espera na fila
   daquele endereço antes de soltar o bucket.
2. Notify incrementa primeiro a mesma sequência atômica, adquire o bucket e
   seleciona o waiter mais antigo da chave exata, ou todos daquela chave.
3. O sinal individual continua registrado se ocorrer antes da suspensão física.
   Wakeups espúrios da primitiva do SO não encerram a espera sem esse sinal.
4. Timeout e notify decidem a remoção sob o mesmo lock. Ao retornar, o waiter
   já não pertence à fila. Waiters/monitores vazios não ficam registrados.
5. `thread.unpark` possui seu próprio token coalescente. Esperar numa condição
   não consome nem produz esse token.

A ABI adicional é:

```text
ar_rt_condvar_wait_timeout(key: ptr[u8], sequence: usize, timeout_ms: u64) -> i32
    1: waiter selecionado por notify, ou sequência já mudou antes do registro
    0: timeout venceu a disputa com notify
   -1: chave nula ou desalinhada
```

Uma chave alinhada arbitrária não é validada por esse teste: continua sendo
obrigação do chamador unsafe fornecer armazenamento vivo e atômico. Notify não
valida um predicado de aplicação. Aplicações devem reavaliá-lo; a API `waitNotification*` é o evento de baixo nível. A API `wait(guard)`
associa a espera ao empréstimo da guarda do mutex do predicado.

No runtime Rust a suspensão usa `std::sync::Condvar` por espera. No runtime C,
usa `pthread_cond_t` por espera em POSIX ou `CONDITION_VARIABLE` no Windows,
com lock do bucket protegendo o sinal. O timeout usa relógio monotônico; POSIX
satura durações além do horizonte representável de `time_t`. O backend Wasm
mantém estas funções como imports de host quando alcançáveis; não oferece uma
implementação silenciosa que ignore waits ou notificações. Wasm sem host de
threads deve rejeitar essa capacidade ou fornecer uma implementação apropriada.

A pegada atual de `Condvar` inclui uma `AtomicUsize`: não há garantia de 1–4
bytes em todos os alvos. Também há alocações e primitivas do SO nos waiters e
nos buckets. Uma representação compacta e um parker reutilizável por thread
exigem medição antes de substituir esta base de correção.

## 3. Condição de espera com guarda

`Condvar.wait` e `waitTimeout` recebem `mut ref MutexGuard<T>`. Amostram a
sequência enquanto o chamador detém o mutex, liberam sua admissão, esperam pela
sequência e readquirem o mutex antes de retornar. A validação da sequência sob
o bucket impede perder uma notificação entre liberação e registro. O CAS de
notify recusa wrap antes de publicar zero, evitando reutilização da geração.
O chamador sempre reavalia o predicado em um loop. O prazo de timeout cobre
a espera da condição; a reaquisição posterior pode demorar mais.

`UnsafeCell` é armazenamento canônico não-Copy e não-Sync; sua exposição de
ponteiro exige unsafe. Os locks encapsulam essa fronteira e não permitem
acesso seguro direto ao payload. A guarda continua exclusivamente emprestada
durante wait: uma referência ativa ao valor protegido impede esse empréstimo.

Cancelamento diferido de `pthread_cond_wait` remove o waiter e libera o bucket
antes da saída da thread. Isso é limpeza do mecanismo POSIX, não uma API para
interromper código Arandu. Cancelamento assíncrono/preemptivo e interrupção
arbitrária de código Arandu não são admitidos. `SyncExecutor.cancel` descarta
tarefas ainda pendentes; se o join já está dirigindo a tarefa, marca o handle
para descarte depois que ela retornar, sem sinalizar a coroutine. O token interno
de `WorkerTask` só é consultado antes da execução; ainda não existe um
`CancelToken` público geral observado por tarefas em pontos de suspensão.
`std.thread.JoinHandle.drop` faz join bloqueante e não cancela a thread; o
trabalho precisa retornar por conta própria. Tarefas cooperativas não devem
bloquear o worker nativo em uma espera comum do SO.

`std.sync.cancel_token.CancelToken` é um sinal atômico compartilhado e
monotônico. O chamador deve cloná-lo para o job/coroutine e consultar
`isCancelled()` em pontos onde possa encerrar com segurança. O runtime não
injeta verificações em `await`, não interrompe uma chamada bloqueante e não
libera guards automaticamente. O token interno de `WorkerTask` permanece uma
checagem pré-execução do scheduler, independente desse tipo público.

## 4. RwLock phase-fair implementado

A referência de Brandenburg e Anderson distingue quatro propriedades: alternar
fases de leitura/escrita quando ambas têm demanda; servir escritores em FIFO;
admitir todos os leitores pendentes no início de uma fase de leitura; impedir
que leitores novos ultrapassem escritores já pendentes. Isso não equivale a
prometer que qualquer escritor espera apenas uma fase: pode haver escritores
anteriores na fila. Veja o [artigo dos autores](https://people.mpi-sws.org/~bbb/papers/pdf/rtsj11.pdf).

O motor mantém uma fila FIFO de escritores e uma fila de leitores em cada
endereço. Novos leitores não ultrapassam escritores pendentes. Ao liberar um
escritor, todos os leitores já enfileirados recebem sua admissão antes do
próximo escritor. O último leitor acorda o escritor mais antigo. As transições
ocorrem sob o bucket; estados sem holders nem esperas são retirados da tabela.
Essa implementação conserva as fases sem usar tickets numéricos recicláveis.
Overflow do contador de leitores é recusado. O mirror C retira waiters sob
cancelamento POSIX diferido, inclusive quando o sinal venceu a disputa e
reservou uma admissão. Cancelamento assíncrono não faz parte do contrato.

A ausência de starvation requer progresso dos participantes, término das
seções críticas e hipóteses de escalonamento explícitas. Limites de bloqueio
em modelos de tempo real não dão um prazo de relógio num SO geral, onde um
holder pode ser suspenso indefinidamente. Benchmarks de latência e throughput
são evidência adicional, não prova dessas propriedades.

Leitura recursiva pelo mesmo holder é admitida somente quando não há escritor
na fila. Cada aquisição é contada por identidade monotônica de thread. Uma
tentativa bloqueante de escrita recursiva, upgrade leitura→escrita, leitura
enquanto a mesma thread mantém escrita, ou leitura recursiva atrás de escritor
na fila aborta com diagnóstico de runtime e endereço do lock, em vez de
esperar por uma aquisição que depende da própria thread. As variantes `try*`
continuam não bloqueantes e retornam falha nesses casos. Unlock de leitor ou
escritor por thread sem a aquisição correspondente aborta. As guardas da stdlib
são afins e não `Send`, além da validação do runtime na fronteira ABI.

O estado por endereço e os registros de leitores são alocados enquanto há
holders ou waiters e retirados quando ficam ociosos. No C, falha de `calloc` ou
`malloc` aborta o processo; não há fast path atômica nem pool por bucket. O
bucket também serializa locks distintos que colidem. Essas escolhas permanecem
corretas, mas qualquer pool, alteração do striping ou fast path precisa de
workload e medição antes/depois para justificar memória e complexidade.

## 5. Send, Sync e guardas

A prova atual em `arandu_typeck` percorre o armazenamento de forma determinística,
com limites para grafos genéricos. Rejeita ponteiros crus, referências, views,
capturas e recursos com destrutores sem contrato específico; reconhece String
e Vec canônicos. Ela não prova duração de empréstimos nem efeitos de tarefas.

Para admitir locks com mutabilidade interior, as regras são:

| Tipo | Transferência por posse (`Send`) | Referência compartilhada (`Sync`) |
| --- | --- | --- |
| `Mutex<T>` | Requer `T: Send` | Requer `T: Send`, com exclusão exclusiva comprovada |
| `RwLock<T>` | Requer `T: Send` | Requer `T: Send + Sync`, pois leitores expõem `ref T` simultaneamente |

A diferença de bounds do RwLock é essencial e coincide com a
[referência de Rust](https://doc.rust-lang.org/std/sync/struct.RwLock.html#impl-Sync-for-RwLock%3CT%3E).
A guarda precisa reter o empréstimo do lock, liberar a aquisição exatamente
uma vez e impedir acesso ao payload depois de sua destruição. Guardas mutáveis
não podem produzir aliases incompatíveis, e operações diretas no payload não
podem contornar a aquisição ao se tornar compartilhado.

Esses contratos devem depender da identidade canônica da primitiva, não do nome
de uma struct que qualquer módulo possa imitar. Nem Arc nem um campo atômico
conferem automaticamente Sync ao payload. Os marcadores não autorizam leitura
concorrente de um tipo com mutabilidade interior não sincronizada.

## 6. Evidência e critérios de aceitação

As regressões de runtime exercitam sequência já alterada, timeout zero e
positivo, remoção de espera, colisões de bucket, ordem de notifyOne, isolamento
do token de thread e retirada de IDs ao encerrar threads. O harness do runtime
C exercita também cancelamento POSIX diferido, compilado em O0/O2 com UBSan;
o mesmo trecho de runtime pode ser exercitado com ASan. A ponte JIT testa a
assinatura real `ptr/usize/u64 -> i32`, inclusive duração ampla.

As regressões de fases verificam escritores FIFO, leitores em lote, isolamento
de chaves, try-lock, contenção e retirada de estados. Testes fonte verificam
receptores compartilhados, bounds e duração de guardas; o harness C cobre
cancelamento antes e depois da reserva de admissão. Isso não promove suporte
nativo de Windows/macOS: esses alvos exigem compilação e execução próprias,
incluindo seus caminhos de
FLS, eventos e relógio relativo. As validações gerais do workspace e resultados
atuais pertencem ao relatório de entrega, não são inferidas desta RFC.

A [arquitetura ParkingLot descrita pelo WebKit](https://webkit.org/blog/6161/locking-in-webkit/)
justifica separar estado compacto e filas por endereço. Seus números de memória
e desempenho descrevem aquela implementação e workloads; não são resultados
do Arandu. Não há benchmark representativo que autorize aqui prometer redução
de memória ou ganho de latência.
