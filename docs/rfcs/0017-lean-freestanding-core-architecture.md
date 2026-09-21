# RFC 0017: Arquitetura Fundamental do `arandu_core` — Camada Freestanding, Zero-Heap e de Custo Zero

- **Número da RFC:** 0017
- **Título:** Arquitetura Fundamental do `arandu_core` — Camada Freestanding, Zero-Heap e de Custo Zero (*Lean Freestanding Core Architecture*)
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-19
- **Status:** `Draft`
- **Área Principal:** `Stdlib` / `Core` / `Embedded` (`arandu_core`, `std.core`)
- **Documentos Relacionados:**
  - `docs/arandu-stdlib-architecture-v0.1.md`
  - `docs/rfcs/0001-generational-fallback-genref.md`
  - `docs/rfcs/0007-semantic-memory-model.md`
  - `docs/rfcs/0008-async-runtime-and-colorless-model.md`
  - `docs/rfcs/0009-borrowed-views-safety.md`
  - `docs/rfcs/0012-scientific-computing-and-data-architecture.md`
  - `docs/rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md`
  - `docs/rfcs/0014-native-wasm-component-model-and-runtime.md`
  - `docs/rfcs/0015-native-mobile-architecture-and-zero-copy-interop.md`
  - `docs/rfcs/0016-capability-safe-filesystem-and-path-resolution.md`

---

## 1. Resumo (Summary)

Esta RFC formaliza a especificação técnica, os invariantes matemáticos e os limites de fronteira da camada **`arandu_core`** (`std.core`), o bloco fundacional irreduzível do ecossistema Arandu.

O `arandu_core` é projetado sob o princípio da **"Física Fundamental da Linguagem"**:
1. **Ambiente Freestanding Puro**: Dependência absolutamente ZERO de sistemas operacionais, chamadas de sistema (syscalls), bibliotecas C externas (`libc`), threads de kernel ou alocação dinâmica de memória global (heap);
2. **Prevenção de "Panic Bloat"**: Eliminação de tabelas complexas de formatação de strings e vtables em situações de erro irrecuperável, adotando traps/aborts de uma única instrução de máquina de hardware (`UD2`, `BKPT`, `EBREAK`) com código escalar de 32 bits;
3. **Fatias e Views como Primitivas de Primeira Classe (`[]T`, `str`)**: Todo processamento de sequências opera sobre *fat pointers* determinísticos `(ptr, len)`, garantindo zero cópias e verificação estática de limites;
4. **Universalidade de Alvos**: Execução idêntica e sem atritos em microcontroladores de baixíssimo consumo (ARM Cortex-M0/M3/M4, RISC-V de 4 KB a 16 KB de RAM), WebAssembly puro sem WASI (`wasm32-unknown-unknown`), kernels de sistemas operacionais, engines gráficas 3D de 120 FPS e nós de computação de alta densidade;
5. **Comptime Nativo (CTFE / RFC 0013)**: A pureza do `core` define a superfície que a VM determinística deverá avaliar em tempo de compilação quando a RFC 0013 estiver implementada.

### 1.1 Estado da implementação (2026-09-20)

A fronteira executável atual já contém `std.core.fixed` (Q16.16),
`std.core.io` (`Reader`, `Writer`, `Seeker`, `SliceReader` e `SliceWriter`) e
algoritmos de `std.core.str` sem símbolos `extern "C"`: a conversão de `str` em
bytes é um intrínseco do compilador que preserva proveniência e não aloca. O
módulo de paralelismo, que depende de heap e threads do runtime, reside em
`std.parallel`; não existe wrapper legado em `std.core.parallel`.

O marco ainda é parcial: somente Q16.16 está implementado; Q8.8/Q32.32,
aritmética checked completa, intrínsecos de bits e validação em targets
bare-metal permanecem. CTFE determinístico também continua sendo objetivo da
RFC 0013, portanto o invariante “100% comptime-friendly” é gate futuro, não uma
garantia da toolchain atual. `[]T`, `ref []T` e `mut ref []T` compartilham o
ABI de duas words `(data, len)` em C, Cranelift e Wasm; `Reader` recebe
`mut ref []u8`, e a exclusividade existe apenas nos fatos de ownership/OSSA,
sem word adicional na ABI nem alocação na heap. O lowering pode materializar
temporariamente o par em stack slots internos, invisíveis à semântica.

---

## 2. Motivação (Motivation)

### 2.1 As Falhas Históricas das Bibliotecas Fundamentais

O desenvolvimento de software de sistemas nas últimas décadas foi repetidamente prejudicado por decisões de design nas camadas fundamentais de linguagens consagradas:

| Linguagem | Erro Arquitetural no Núcleo | Consequência Prática |
| :--- | :--- | :--- |
| **C (`freestanding libc`)** | Strings terminadas em nulo (`\0`), falta de estruturas com comprimento embutido e ausência de tratamento canônico de erros. | Décadas de estouros de buffer, vulnerabilidades críticas de segurança de memória e ausência de uma biblioteca padrão segura em ambientes bare-metal. |
| **C++ (`<type_traits>`, `<utility>`)** | Exceções de linguagem (`throw`/`catch`) e estruturas monolíticas fortemente acopladas a templates pesados. | Geração obrigatória de tabelas gigantescas de *stack unwinding* (DWARF/SEH). Em sistemas embarcados e jogos, a primeira diretiva é `-fno-exceptions`, fragmentando o ecossistema. |
| **Rust (`core`)** | O problema do **"Panic Formatting Bloat"**: um simples `assert!` em `core` carrega indiretamente o motor de `core::fmt`, vtables e metadados de arquivo/linha. | Em microcontroladores com 16 KB a 32 KB de Flash, incluir um único `panic!` consome mais de 20 KB de memória flash com strings e tabelas de formatação. Além disso, a abstração `Pin<T>` introduziu alta fricção cognitiva para corrotinas. |
| **Go** | Ausência completa de uma camada `core` desacoplada; o coletor de lixo (GC) e o escalonador estão fundidos na linguagem. | Impossibilidade de rodar em microcontroladores sem projetos de reescrita total (TinyGo) e binários WASM que partem de vários megabytes. |
| **Zig** | Falta de uma divisão formal entre `core` e `std`. | O desenvolvedor só descobre que uma função depende de sistema operacional quando o compilador falha por símbolos ausentes da `libc` na fase de linkagem. |

### 2.2 A Oportunidade do Arandu

O Arandu possui vantagens arquiteturais únicas que permitem conceber um `core` significativamente superior:
1. **Modelo de Memória Semântica e OSSA ([RFC 0007](0007-semantic-memory-model.md))**: Confinamento estático de ciclo de vida e empréstimos sem anotações léxicas manuais de tempo de vida;
2. **Colorless Async ([RFC 0008](0008-async-runtime-and-colorless-model.md))**: Máquinas de estado de corrotina geradas pelo compilador sem necessidade de `Pin` ou de alocação dinâmica na heap;
3. **Sistema de Efeitos Explícitos (`@Effects`)**: Garantia matemática de que funções do `core` possuem `@Effects(Pure)` ou operam apenas em mutação de memória local (`LocalMut`), permitindo execução em tempo de compilação (CTFE) e verificação formal;
4. **Layout Parametrizável por Alvo (`TargetInfo` e `DataLayout`)**: Adaptação precisa a ponteiros de 16, 32 ou 64 bits sem suposições sobre a arquitetura do host.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Escrevendo Código em Modo Freestanding (`@no_std`)

Quando desenvolvemos para um microcontrolador bare-metal ou um ambiente sem sistema operacional, declaramos a dependência exclusiva de `std.core`:

```arandu
// Firmware bare-metal para microcontrolador ARM Cortex-M
import std.core.mem as mem
import std.core.slice as slice
import std.core.math as math
import std.core.intrinsics as intrinsics

// Registro de periférico mapeado em memória (MMIO)
const GPIOA_ODR: ptr[mut u32] = 0x40020014 as ptr[mut u32]

public func alternarLed(): void {
    unsafe {
        let atual = *GPIOA_ODR
        *GPIOA_ODR = atual ^ 0x00000020 // Alterna pino 5
    }
}
```

### 3.2 O Modelo Canônico de Tratamento de Erros: `Result<T, E>`

No `arandu_core`, erros são valores que trafegam em registradores através do enum unificado `Result<T, E>`. Não existem exceções, não há saltos não-locais e não há geração de tabelas DWARF:

```arandu
import std.core.result.Result

public enum SensorError {
    Timeout,
    ChecksumMismatch,
    BusBusy,
}

public func lerSensorI2C(endereco: u8): Result<u16, SensorError> {
    if !barramentoPronto() {
        return Result.Err(SensorError.BusBusy)
    }
    let valor = lerRegistrador(endereco)
    return Result.Ok(valor)
}
```

### 3.3 O Pânico Leve (*Zero-Bloat Abort*)

Em sistemas críticos ou microcontroladores de poucos kilobytes de memória, um erro de programação irrecuperável (ex: falha em checagem estática de limites) dispara uma instrução atômica de CPU, **sem carregar formatadores de texto**:

```arandu
public func obterAmostra(buffer: []u16, indice: uint): u16 {
    // Se o índice estiver fora do intervalo, dispara abort(0x0001_E001)
    // O código de erro vai para o registrador R0/EAX e a CPU entra em trap.
    return buffer[indice]
}
```

No binário gerado para ARM Thumb, a linha acima traduz-se em:
```asm
cmp   r1, r2       ; Compara índice com tamanho da fatia
bhs   .Lpanic_trap ; Salta se índice >= tamanho
ldrh  r0, [r0, r1, lsl #1]
bx    lr

.Lpanic_trap:
mov   r0, #0xE001  ; Código compacto do erro
bkpt  #0x01        ; Ponto de parada imediato do depurador JTAG/SWD (2 bytes!)
```
Custo total em Flash: **6 bytes** (ao invés dos 20 KB de formatadores de string do Rust!).

### 3.4 Matemática de Ponto Fixo para Hardware sem FPU

Para processadores sem unidade de ponto flutuante por hardware (ARM Cortex-M0, microcontroladores automotivos de 8/16/32 bits), o `core` expõe tipos nativos de ponto fixo:

```arandu
import std.core.fixed as fixed

public func aplicarEscala(valor: fixed.Q16_16, escala: fixed.Q16_16): fixed.Q16_16 {
    return valor.mul(escala)
}
```

### 3.5 Corrotinas e Concorrência Cooperativa sem Alocação na Heap

No Arandu, tarefas assíncronas cooperativas rodam no `core` sem alocar nenhum byte de heap:

```arandu
import std.core.future.Poll

// Uma corrotina compila para uma struct de máquina de estados alocada na stack!
public async func piscarPeriodico(intervaloMs: u32): void {
    while true {
        alternarLed()
        await aguardarTicks(intervaloMs)
    }
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Os 6 Invariantes Absolutos de `arandu_core`

Qualquer módulo que resida em `arandu_core` deve satisfazer formalmente estes 6 invariantes:

1. **Invariante 1 — Zero Heap Global**: Nenhuma função, tipo ou macro do `core` pode invocar direta ou indiretamente alocadores dinâmicos globais (`malloc`, `free`, alocador de sistema). Toda memória dinâmica deve ser fornecida explicitamente pelo chamador através de fatias (`[]T`) ou buffers pré-alocados.
2. **Invariante 2 — Zero OS / Zero Syscall**: Proibida a inclusão de chamadas de sistema (POSIX, Win32, Linux syscalls). O código deve ser capaz de inicializar em hardware bruto com a CPU em reset.
3. **Invariante 3 — Zero Threading de Kernel**: Não há primitivas de criação de threads de sistema operacional (`pthread_create`, `CreateThread`). A concorrência no `core` restringe-se a operações atômicas de hardware (`std.core.atomic`), interrupções de hardware e corrotinas cooperativas.
4. **Invariante 4 — Zero DWARF / Zero Exception Unwinding**: O modelo de erro é puramente baseado em valores escalares em registradores. Nenhum metadado de propagação de exceção é emitido no binário.
5. **Invariante 5 — Zero Panic Text Bloat**: Asserções de integridade em código de produção compilam para traps de hardware diretos (`UD2` em x86_64, `BKPT`/`UDF` em ARM, `EBREAK` em RISC-V), com um identificador de 32 bits (`TrapCode`) repassado via registrador da ABI.
6. **Invariante 6 — 100% Comptime-Friendly (CTFE)**: Todas as funções puras de `arandu_core` devem permanecer interpretáveis pela futura máquina virtual determinística da AMIR (conforme [RFC 0013](0013-deterministic-ctfe-and-comptime-metaprogramming.md)); o executor CTFE completo ainda não integra a toolchain.

### 4.2 Topologia de Módulos do `arandu_core`

A estrutura interna de `arandu_core` é modularizada para máxima densidade e separação estrita de responsabilidades:

```text
arandu_core (std.core)
 ├─ mem           # Layout de tipos, alinhamento, size_of, align_of, cópia contígua (copy_nonoverlapping)
 ├─ pointer       # Identidade segura de ponteiros brutos (null, isNull, offset, read/write volátil para MMIO)
 ├─ option        # Option<T> canônico (Some / None)
 ├─ result        # Result<T, E> canônico (Ok / Err)
 ├─ slice         # Fat pointers ([T]), sub-fatias, indexação com elisão de bounds-check, busca binária
 ├─ iter          # Iteradores lazy zero-cost baseados em registros, adaptadores puros (map, filter, zip, enumerate)
 ├─ math          # Operações matemáticas puras
 │   ├─ scalar    # Trigonometria, potências, raízes (software fallback e aceleração de instrução única)
 │   ├─ fixed     # Ponto fixo: Q8.8, Q16.16, Q32.32 (essencial para CPUs sem FPU)
 │   └─ saturating# Aritmética saturante e wrapping controlada
 ├─ cmp           # Interfaces fundamentais de ordem e equivalência: Eq, PartialEq, Ord, PartialOrd
 ├─ hash          # Interfaces de hashing e algoritmos puros de bloco (WyHash, BLAKE3, SipHash, CRC32)
 ├─ crypto        # Primitivas puras de cifra e autenticação em bloco (ChaCha20, Poly1305, AES-NI fallback)
 ├─ codec         # Encoders/decoders zero-copy sobre []u8: UTF-8 validation, Base64, Hex, LEB128, Varint
 ├─ io            # Interfaces abstratas de I/O em memória: Reader, Writer, Seeker sobre fatias contíguas
 ├─ bitset        # Operações de manipulação de bits: popcount, clz, ctz, bswap, bitsets densos em u32/u64
 ├─ simd          # Vetores SIMD portáveis de hardware (128 bits: NEON, SSE2/AVX, WASM SIMD128)
 ├─ future        # Semântica abstrata de concorrência: Poll, Context, Waker, interface Coroutine
 ├─ cell          # Mutabilidade interior sem overhead de thread: Cell<T>, UnsafeCell<T>
 ├─ marker        # Marcadores fundamentais do compilador: Send, Sync, Copy, Sized, Unpin
 ├─ atomic        # Atomics de hardware: AtomicBool, AtomicI32, AtomicUsize (load, store, CAS, fetch_add)
 ├─ fmt           # Formatação em buffer de memória estático fornecido pelo chamador (WriteBuf, sem heap)
 ├─ panic         # Handlers de trap: TrapCode, abort, abortGenerationalMismatch
 └─ intrinsics    # Primitivas diretas de backend: unreachable, breakpoint, memory_barrier
```

### 4.3 Arquitetura de Abort/Trap sem Bloat de Texto

Para resolver definitivamente o problema que afeta o ecossistema Rust em microcontroladores de 32 KB, a infraestrutura de `panic` em `arandu_core` é modelada com separação entre **modo de diagnóstico de desenvolvimento** e **modo de produção embarcada**:

```arandu
// Estrutura compacta de identificação de falha (4 bytes)
public struct TrapCode {
    code: u32
}

public namespace TrapCodes {
    public const OUT_OF_BOUNDS: u32 = 0xE000_0001
    public const INTEGER_OVERFLOW: u32 = 0xE000_0002
    public const GENERATIONAL_MISMATCH: u32 = 0xE000_0003
    public const UNREACHABLE: u32 = 0xE000_0004
}
```

* **Em compilação com perfil de tamanho/embarcado (`--release --profile=embedded`)**:
  Qualquer asserção que falhe emite apenas a carga do código no registrador principal da ABI e a instrução de trap da CPU. O binário final não contém uma única string com caminhos de arquivo, números de linha ou mensagens em inglês.
* **Em compilação de depuração/desenvolvimento (`--profile=debug`)**:
  Se o alvo suportar ou possuir um canal de telemetria configurado, o compilador injeta um handler desacoplado que decodifica o `TrapCode` usando uma tabela de símbolos externa (*split dwarf / map file*) gerada no host.

### 4.4 I/O Abstrato Baseado em Memória (`std.core.io`)

No `arandu_core`, ler e escrever não depende de arquivos do sistema operacional, mas de interfaces sobre memória contígua:

```arandu
public interface Reader {
    func read(buf: mut ref []u8): Result<uint, u32>
}

public interface Writer {
    func write(buf: []u8): Result<uint, u32>
}

// Cursor seguro sobre fatia de memória emprestada:
public struct SliceReader<'a> {
    data: ref ['a]u8,
    position: uint,
}

impl Reader for SliceReader {
    func read(buf: mut ref []u8): Result<uint, u32> {
        let disponivel = self.data.len() - self.position
        if disponivel == 0 {
            return Result.Ok(0) // EOF
        }
        let a_copiar = math.min(buf.len(), disponivel)
        slice.copy(buf[0..a_copiar], self.data[self.position..self.position + a_copiar])
        self.position = self.position + a_copiar
        return Result.Ok(a_copiar)
    }
}
```
Isso permite que um parser de JSON, um descompactador ou um decodificador de imagem no `core` processe dados vindos de uma fatia de memória, de um buffer DMA de rede ou de um arquivo em disco (em camadas superiores) sem alterar uma única linha de código.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Preservação dos Invariantes Centrais do Arandu

1. **No hidden allocation**: O `core` não aloca. Ponto final. Funções que necessitam de memória auxiliar de rascunho (*scratch memory*) recebem explicitamente um buffer contíguo (`scratch: mut ref []u8`).
2. **No implicit runtime**: Corrotinas compilam para máquinas de estado explícitas. Agendadores cooperativos rodam em loops simples sem despacho indireto de threads.
3. **Cache locality over abstraction purity**: Algoritmos operam diretamente sobre fatias contíguas (`[]T`), minimizando saltos de ponteiro e maximizando a previsibilidade de execução em hardware restrito.
4. **Layout dependente do alvo**: Toda representação de tipo respeita `TargetInfo` (ex: `usize` é `u16` em microcontroladores de 16 bits, `u32` em ARM Cortex-M e WASM, e `u64` em x86_64/AArch64).

### 5.2 Desvantagens e Trade-offs

* **Depuração em Bare-Metal**: A ausência de mensagens de texto ricas no binário compilado em modo de produção exige que desenvolvedores de microcontroladores utilizem depuradores de hardware (JTAG, SWD, OpenOCD) ou inspecionem o mapa de símbolos para correlacionar o `TrapCode` com o ponto exato do código fonte.
* **Sobrecarga de Assinatura**: Funções que em outras linguagens alocariam buffers dinâmicos internamente (ex: encoders de Base64 ou formatadores de número para string) exigem no `core` a passagem explícita de um buffer de saída (`out_buf: mut ref []u8`).

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### Alternativa 1: Permitir um Alocador Opcional Global dentro do `core`
* **Abordagem:** Permitir que o `core` dependa de um ponteiro de alocador global fraco (*weak symbol*), caindo para pânico se não for implementado.
* **Por que foi rejeitada:** Violação direta da filosofia de camadas do Arandu. Abre brecha para que funções do `core` comecem a alocar silenciosamente, destruindo a garantia de compatibilidade com ambientes que possuem 4 KB de RAM ou bootloaders de primeiro estágio. Para alocação dinâmica, existe a camada canônica **`arandu_alloc`**.

### Alternativa 2: Modelo de Erros por Exceções ou Flags Globais
* **Abordagem:** Uso de `setjmp`/`longjmp`, tabelas DWARF ou variável global de erro (`errno`).
* **Por que foi rejeitada:** Exceções aumentam exponencialmente o tamanho do código de máquina e tornam o fluxo de controle imprevisível. `errno` global é inseguro em concorrência e proíbe otimizações agressivas de fluxo de controle na AMIR. O modelo `Result<T, E>` baseado em valores é formal, puro e mapeia perfeitamente para registradores.

---

## 7. Arte Prévia e Literatura Científica (Prior Art)

1. **The Rust Core Library (`core`)**:
   - A decisão do Rust (iniciada em 2014) de isolar `core` de `alloc` e `std` provou-se um dos maiores sucessos de engenharia da linguagem, viabilizando o Rust em kernels (Linux Kernel Rust support) e sistemas embarcados. O Arandu adota a mesma divisão conceitual, mas elimina as deficiências de "panic formatting bloat" e a complexidade do `Pin`.
2. **Diretrizes MISRA C / AUTOSAR**:
   - Padrões de segurança crítica para software automotivo e aeroespacial proíbem alocação dinâmica de memória (`malloc`/`free`) após a fase de inicialização e vetam exceções de C++. A arquitetura do `arandu_core` cumpre nativamente essas exigências.
3. **Klein et al. (2009) — seL4: Formal Verification of an OS Kernel (SOSP '09)**:
   - Demonstra que a garantia de ausência de falhas em sistemas de alto isolamento depende de evitar alocação de memória dinâmica não rastreável e manter o núcleo do sistema puramente determinístico.
4. **Zig Freestanding / `std.mem.Allocator`**:
   - O modelo de passar explicitamente buffers ou alocadores inspirou diretamente a ergonomia do `core` do Arandu para funções que precisam materializar saídas de tamanho dinâmico.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Customização de Handlers de Trap em Firmware**: Definir a sintaxe canônica para o desenvolvedor embarcado registrar uma função de interrupção personalizada quando um `TrapCode` for acionado:
   ```arandu
   @TrapHandler
   func meuHandlerDeFalha(codigo: u32): void {
       // Desliga motores e aciona alarme de emergência
   }
   ```
2. **Exposição de Ponto Fixo no Prelude do Core**: Avaliar se tipos de ponto fixo (`Q16_16`) devem entrar no escopo padrão do `core` automaticamente quando o alvo não possuir FPU de hardware (`TargetInfo.has_fpu == false`).

---

## 9. Possibilidades Futuras (Future Possibilities)

1. **Certificação ISO 26262 / IEC 61508**: Criar um subconjunto estrito e formalizado do `arandu_core` destinado à certificação de segurança funcional em veículos autônomos, aviônica e dispositivos médicos.
2. **Biblioteca de Redes Neurais Embarcadas (TinyML)**: Implementação de operadores de inferência de tensores inteiros quantizados (INT8) inteiramente dentro de `std.core.math`, permitindo rodar modelos de Machine Learning em microcontroladores com 64 KB de Flash.
3. **Simulador e Test Runner Freestanding**: Ferramenta nativa do compilador para executar a suíte de testes de `arandu_core` diretamente em simuladores QEMU de arquiteturas Cortex-M e RISC-V sem intervenção manual de scripts.
