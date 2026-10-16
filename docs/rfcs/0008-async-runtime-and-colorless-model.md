# RFC 0008: Runtime Assíncrono e Modelo Colorless (SL_R / A3)

- **Número da RFC:** 0008
- **Título:** Runtime Assíncrono Cooperativo e Modelo Colorless de Corrotinas
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-08-20
- **Status:** `Implemented` no recorte SL_R.0–SL_R.3, com limites de payload, backend e plataforma descritos abaixo
- **Área Principal:** `Runtime` / `Frontend` / `Compiler Infrastructure` (`A3`, `SL_R`)
- **Documento Relacionado:** `docs/arandu-async-runtime-design-v0.1.md`

---

## 1. Resumo (Summary)

Esta RFC especifica a infraestrutura de computação assíncrona do Arandu, composta pelo modelo semântico no compilador (**Fase A3**) e pela biblioteca de runtime cooperativa na stdlib (**Fase SL_R**).

A proposta isola rigidamente a semântica da linguagem (`async`/`await`, `Coroutine[T]`, `Poll[T]`, suspensão de CFG) dos detalhes do sistema operacional (threads, `epoll`, `io_uring`, sockets), mantendo uma fronteira própria para a semântica de suspensão. Isso não promete
que os hosts do executor e de I/O sejam portáveis integralmente entre ambientes
nativos e WebAssembly.

---

## 2. Separação Estrita de Responsabilidades

| Camada | É Dono de | NÃO é Dono de |
| :--- | :--- | :--- |
| **Compilador (`A3`)** | `async`/`await`, `Coroutine[T]`, `Poll[T]`, CFG de Suspensão | Threads do SO, epoll, filas de spawn |
| **`std.core.future`** | Enum `Poll[T]` (Ready / Pending) | Reator do SO |
| **`std.runtime.executor`** | Executor cooperativo (`SyncExecutor`, `TaskHandle<T>`, `spawn`, `join`, `blockOn`, `cancel`) | Reator do SO, wakers, sockets |
| **`std.runtime.waker`** | `Waker` / `Context` (token de notificação) | Sockets de rede |
| **`std.runtime.reactor`** | Linux `epoll`/`io_uring`; timers portáveis nos demais hosts | Executor, sockets |
| **`std.net`** | Sockets TCP e abstrações de I/O de rede | Reator, wakers |

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

A superfície existente de `stdlib/std/runtime/executor.aru` recebe uma
`Coroutine<T>` produzida por `async func`. `spawn` devolve `TaskHandle<T>` e
`join` espera esse handle; `blockOn` recebe a própria corrotina. O exemplo usa
o mesmo recorte de `examples/minimal/m07_async_spawn_join.aru`:

```arandu
import std.runtime.executor as rt

async func answer(): int {
    return 42
}

func main(): int {
    let ex = rt.SyncExecutor.new()
    let handle = ex.spawn(answer())
    return ex.join(handle)
}
```

### Limites auditados em 15/10/2026

O transporte do executor cooperativo ainda usa bits i64; o parâmetro `T`
retém a checagem estática, mas não oferece transporte genérico de agregados.
O backend C standalone não fornece todos os hosts `ar_rt_spawn_i64`,
`ar_rt_join_i64`, `ar_rt_cancel_i64` e `ar_rt_block_on_i64` desse módulo.

O reactor Linux integra epoll/timerfd, wakers e o caminho opcional io_uring.
Windows/macOS usam deadline portável para timers e espera TCP direta
(WSAPoll/poll); registrar sockets no reactor retorna `-1` nesses dois hosts.
Não há backend kqueue/IOCP implementado com esse contrato. A interface `Future`
completa para `Coroutine` permanece aberta.

A instalação pública do SDK v0.1.10 passou nos três alvos no
[run 38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029),
verificado em 15/10/2026. O smoke cobre fluxos selecionados, incluindo CTFE;
não comprova paridade completa do runtime. O contrato técnico
[async](../arandu-async-runtime-design-v0.1.md) detalha as restrições; a fila
executiva permanece no roadmap mestre.

---

## 4. Explicação em Nível de Referência: Transformação na AMIR

O compilador transforma funções assíncronas em máquinas de estado determinísticas baseadas em blocos básicos da AMIR:
1. **Ponto de Suspensão (`Suspend`)**: A instrução `await` é reduzida a um terminador de controle `Suspend` na CFG.
2. **Salvamento de Estado**: Variáveis locais vivas através do ponto de suspensão são projetadas para o frame de estado da corrotina.
3. **`RelativeBorrow`**: Referências que cruzam pontos de suspensão são transformadas em empréstimos relativos indexados no frame (`LocalId`), impedindo corrupção de ponteiros quando a corrotina é retomada por outro worker.
4. **Proibição de Empréstimos de Pilha Trans-Suspensão (`O010`)**: O compilador emite erro fatal se um empréstimo puro de pilha tentar atravessar um ponto de suspensão, garantindo que nenhum ponteiro solto sobreviva à liberação temporária da thread.

---

## 5. Invariantes de Arquitetura

1. **Ausência de Efeitos Ocultos na IR**: A AMIR e o type checker não realizam chamadas ao sistema operacional ou alocações opacas de threads durante a compilação de código assíncrono.
2. **Tipagem Estrita de Handles**: `TaskHandle<T>` carrega o tipo do resultado inferido diretamente de `Coroutine[T]`. Tentar esperar um resultado incompatível resulta em erro de checagem de tipos estático.
