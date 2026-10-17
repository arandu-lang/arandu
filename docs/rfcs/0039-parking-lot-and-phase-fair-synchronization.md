# RFC 0039: Concorrência de Alta Performance, Infraestrutura ParkingLot e Sincronização Phase-Fair

- **Número da RFC:** 0039
- **Título:** Concorrência de Alta Performance, Infraestrutura ParkingLot e Sincronização Phase-Fair
- **Autor(es):** Arquiteto da Linguagem Arandu
- **Data de Início:** 2026-10-16
- **Status:** `Draft`
- **Área Principal:** `Runtime` | `Middle-end` | `Stdlib` | `Concurrency`
- **Escopo:** `std.sync.condvar`, `std.sync.rwlock`, `std.sync.mutex`, infraestrutura `ParkingLot` em `crates/arandu_runtime`, marcadores de concorrência `Send` e `Sync` no `arandu_middle`.

---

## Fronteira de implementação e evidência

Esta RFC estabelece a refundação dos contratos de concorrência e sincronização de baixa latência do Arandu, eliminando travas pesadas do sistema operacional e substituindo rotinas com sleeps cooperativos por primitivas baseadas em endereçamento atômico e ausência de inanição (*starvation-free*).

- **Implementado na v0.1.11 (2026-10-16):**
  - Primitivas atômicas escalares em hardware ([`stdlib/core/atomic.aru`](../../stdlib/core/atomic.aru)): `AtomicBool`, `AtomicI32`, `AtomicUsize`, com ordenação de memória `SeqCst` e `Acquire`/`Release` validadas em teste de contenção com 6 threads concorrentes.
  - Protótipos cooperativos de `Condvar` e `RwLock` em [`stdlib/std/sync/`](../../stdlib/std/sync/): `Condvar.wait` baseado em valor de sequência e polling de 50ms; `RwLock` requerendo `mut ref self` agindo como trava de exclusão mútua básica.
- **Proposta desta RFC (Alvo para ST.10, ST.11 e série 0.2.x):**
  - Implementação da tabela global e centralizada **`ParkingLot`** no `arandu_runtime` (estilo WebKit / Rust `parking_lot`), indexando suspensões de threads pelo endereço de memória do monitor (`uintptr_t`).
  - Redução da pegada de memória de `Mutex` e `Condvar` para **1 a 4 bytes**.
  - Adoção do algoritmo de sincronização **Phase-Fair Reader-Writer Lock (PFLock)** de Brandenburg & Anderson, eliminando inanição de escritores e garantindo latência máxima de espera limitada (*bounded worst-case blocking*).
  - Formalização dos marcadores de tipo `Send` e `Sync` no sistema de tipos para permitir leitura compartilhada concorrente segura (`RwLock.read(&self)`).

---

## 1. Resumo (Summary)

Travas de sincronização convencionais baseadas em chamadas do sistema operacional (como `pthread_mutex_t`, `pthread_cond_t` ou `CRITICAL_SECTION`) impõem duas penalidades severas ao desenvolvimento de software de alta performance:
1. **Inchaço de Memória:** Cada lock de SO consome entre 40 e 64 bytes. Estruturas de dados refinadas com granularidade fina (como tabelas hash concorrentes, árvores ou caches) desperdiçam dezenas de megabytes apenas em cabeçalhos de travamento.
2. **Inanição (*Starvation*) ou Serialização de Leitores:** RWLocks convencionais ou sofrem de inanição infinita de escritores (sob carga pesada de leitura) ou forçam uma ordenação FIFO que impede leitores consecutivos de executarem em paralelo.

Esta RFC desenha a infraestrutura de concorrência do Arandu combinando a arquitetura **`ParkingLot`** (introduzida por Filip Pizlo no WebKit) com o algoritmo matemático de justiça por fases **PFLock** (*Phase-Fair Locks*, Brandenburg & Anderson, 2010).

---

## 2. Motivação (Motivation)

### O Problema do `Condvar` Atual
Na v0.1.11, a API de `Condvar` recebe apenas o número de sequência (`Condvar.wait(current: usize)`), forçando o runtime a simular espera através de suspensão temporizada (`usleep(50000)`). Não há associação estrutural entre a variável de condição e o `Mutex` que protege o predicado. Isso abre brechas para *lost wakeups* e consome CPU desnecessariamente.

### O Problema do `RwLock` Atual
Para evitar corridas de dados no sistema de tipos atual sem a formalização do marcador `Sync`, o método `read()` foi forçado a assinar `self: mut ref RwLock<T>`. Embora isso preserve a segurança de memória, transforma o `RwLock` em um lock de exclusão mútua estrito, anulando o benefício de leituras concorrentes em código seguro.

### As Falhas das Abordagens Industriais Tradicionais
* **`pthread_rwlock` / `std::shared_mutex` de C++:** Na maioria das implementações glibc, locks que priorizam leitores bloqueiam escritores indefinidamente sob alta carga de tráfego web ou telemetria.
* **Java `ReentrantReadWriteLock`:** Sobrecarga severa de objetos de thread e alocações de nós de fila de contenção na JVM.
* **Go Channels:** Embora canais Go sejam convenientes, seu uso para sincronização de pequenos estados compartilhados introduz uma sobrecarga massiva de agendamento de goroutines se comparado a travas atômicas puras.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Uso Ergonômico e Leve de `Mutex` e `Condvar`

Com o novo modelo, um `Mutex` ocupa apenas 1 byte e não aloca handles do sistema operacional:

```arandu
module app.servico

import std.sync.mutex as mutex
import std.sync.condvar as cv

public struct FilaSegura<T> {
    trava: mutex.Mutex<[]T>,
    nao_vazia: cv.Condvar,
}

public func FilaSegura.novo<T>(): FilaSegura<T> {
    return FilaSegura {
        trava: mutex.Mutex.novo([]),
        nao_vazia: cv.Condvar.novo(),
    }
}

public func FilaSegura.enfileirar<T>(self: ref FilaSegura<T>, item: T): void {
    let mut guard = self.trava.lock()
    guard.push(item)
    // Acorda instantaneamente uma thread adormecida pelo endereço deste monitor
    self.nao_vazia.notifyOne()
}

public func FilaSegura.desenfileirar<T>(self: ref FilaSegura<T>): T {
    let mut guard = self.trava.lock()
    while guard.isEmpty() {
        // Libera a trava e suspende a thread atomicamente no ParkingLot
        guard = self.nao_vazia.wait(guard)
    }
    return guard.pop()
}
```

### 3.2 O `RwLock` com Justiça de Fases (Phase-Fair)

Leitores adquirem a trava simultaneamente usando referências imutáveis (`&self`). Escritores nunca sofrem inanição:

```arandu
import std.sync.rwlock as rw

public struct BancoDeMemoria {
    tabela: rw.RwLock<TabelaDados>,
}

public func BancoDeMemoria.consultar(self: ref BancoDeMemoria, chave: str): Registro {
    // Múltiplos leitores adquirem a trava simultaneamente em paralelo real
    let leitura = self.tabela.read()
    return leitura.obter(chave)
}

public func BancoDeMemoria.atualizar(self: ref BancoDeMemoria, chave: str, val: Registro): void {
    // Escritor aguarda apenas a fase de leitura corrente terminar; novos leitores
    // não podem furar a fila do escritor.
    let mut escrita = self.tabela.write()
    escrita.inserir(chave, val)
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 A Arquitetura do `ParkingLot` no Runtime (`arandu_runtime`)

O `ParkingLot` é uma tabela hash particionada com *striping* de locks internos, mantida em nível de runtime:

```
[ Endereço do Lock na Memória: 0x7ffd9820 ]
                  │
                  ▼ (Hash & Máscara de Balde)
         ┌─────────────────┐
         │ Balde do Bucket │ ──► Fila Intrusiva de Threads Adormecidas
         └─────────────────┘       Thread A (parked) ──► Thread B (parked)
```

1. **Operação de `park(key, validate, timeout)`:**
   - Adquire o lock interno do balde correspondente ao hash do endereço `key`.
   - Executa a closure `validate()`. Se a condição atômica mudou (ex: o lock foi liberado antes de dormir), aborta o estacionamento e retorna sem dormir.
   - Enfileira a thread corrente na lista encadeada do balde.
   - Libera o lock interno do balde e invoca o bloqueio do kernel (`SYS_futex` no Linux, `WaitOnAddress` no Windows, `__ulock_wait` no macOS).
2. **Operação de `unpark_one(key)` / `unpark_all(key)`:**
   - Localiza o balde correspondente ao endereço `key`.
   - Retira uma ou todas as threads da fila.
   - Invoca o desbloqueio do kernel (`FUTEX_WAKE`, `WakeByAddressSingle`, etc.).

### 4.2 O Algoritmo Phase-Fair Reader-Writer Lock (PFLock)

O algoritmo de Brandenburg & Anderson estrutura o estado do `RwLock` em uma única palavra atômica de 32 bits (`uint32_t`):

```
Bits [31..16]: Contador de Leitores Ativos na Fase Presente
Bit  [15]    : Bit de Fase de Leitura Presente (0 ou 1)
Bit  [14]    : Flag de Escritor Presente / Aguardando
Bits [13..0] : Ingresso / Ticket da Fila de Espera
```

1. **Entrada de Leitores:** Se não há escritor ativo nem pendente na fase oposta, o leitor incrementa o contador da fase correspondente e entra imediatamente em concorrência.
2. **Entrada de Escritores:** Um escritor adquire o próximo ticket de fase. Novos leitores subsequentes são direcionados para a próxima fase e não podem furar o escritor.
3. **Alternância de Fases:** Assim que o último leitor da fase ativa conclui (`contador == 0`), a fase de escrita é liberada atomicamente. Ao término da escrita, a fase volta para leitura, liberando todos os leitores acumulados de uma só vez.
4. **Garantia de Tempo Limite:** O tempo de espera de qualquer escritor é limitado a no máximo uma única fase de leitura prévia, eliminando matematicamente o problema de inanição.

### 4.3 Integração com o Sistema de Tipos: Marcadores `Send` e `Sync`

Em `arandu_middle::types`:
1. **`Send`:** Um tipo `T` implementa `Send` se a transferência de posse entre threads de execução for segura (tipos que não dependem de estado de thread local ou referências lineares vinculadas a um frame).
2. **`Sync`:** Um tipo `T` implementa `Sync` se uma referência imutável `&T` puder ser compartilhada com segurança entre múltiplas threads concorrentes.
3. **Regra de Mutabilidade Interior:** `RwLock<T>` e `Mutex<T>` implementam `Sync` se e somente se `T: Send`. Isso permite que métodos que recebem `self: ref RwLock<T>` gerem guardas concorrentes sem risco de corridas de dados.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

1. **Preservação de Determinismo e Early-Cutoff:** A implementação do `ParkingLot` reside no runtime e não polui as queries Salsa com efeitos colaterais de I/O.
2. **Desvantagem Consciente (Contenção de Baldes):** Se milhares de locks distintos colidirem no mesmo balde da tabela do `ParkingLot`, haverá contenção espúria no lock interno do balde. Mitiga-se isso usando particionamento em 1024 baldes (*bucket striping*) e hashing de Fibonacci de 64 bits sobre os endereços de memória.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

- **Alternativa Rejeitada: Mutexes Nativos de SO (POSIX/Win32):**
  - Rejeitada porque consumiria 64 bytes por lock, inviabilizando estruturas de concorrência com granularidade fina.
- **Alternativa Rejeitada: Spinlocks Puros:**
  - Rejeitada porque o consumo de energia e a degradação de performance por inversão de prioridades (*priority inversion*) e *lock convoys* em CPUs com múltiplos núcleos é inaceitável.

---

## 7. Arte Prévia (Prior Art)

- **Filip Pizlo ("Locking in WebKit", 2016):** Demonstrou que a arquitetura `ParkingLot` com travas de 1 byte acelerou benchmarks do navegador (Speedometer, JetStream) em dois dígitos em relação aos mutexes tradicionais do SO.
- **Brandenburg & Anderson ("Phase-Fair Reader-Writer Locks for Real-Time Multiprocessor Systems", ECRTS 2009 / Real-Time Systems 2010):** Criação canônica do algoritmo PFLock, comprovando limites rígidos de bloqueio no pior caso.
- **Amanieu d'Antras (`parking_lot` crate):** Adoção de referência do padrão na linguagem Rust.

---

## 8. Questões em Aberto (Unresolved Questions)

1. *Suporte a WebAssembly Single-Threaded:* Em alvos Wasm sem a extensão *threads/shared-memory*, o `ParkingLot` deve degradar de forma estática para um conjunto de operações de no-op sem overhead de atomics ou futex?
   - **Recomendação:** Sim, através de compilação condicional de features de runtime em `crates/arandu_runtime`.
