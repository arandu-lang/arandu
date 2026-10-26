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

Esta RFC propõe os requisitos técnicos e os limites de fronteira da camada **`arandu_core`** (`std.core`), o bloco fundacional irreduzível do ecossistema Arandu.

O `arandu_core` é projetado sob o princípio da **"Física Fundamental da Linguagem"**:
1. **Ambiente Freestanding Puro**: Dependência absolutamente ZERO de sistemas operacionais, chamadas de sistema (syscalls), bibliotecas C externas (`libc`), threads de kernel ou alocação dinâmica de memória global (heap);
2. **Prevenção de "Panic Bloat"**: Eliminação de tabelas complexas de formatação de strings e vtables em situações de erro irrecuperável, adotando traps/aborts de uma única instrução de máquina de hardware (`UD2`, `BKPT`, `EBREAK`) com código escalar de 32 bits;
3. **Fatias e Views (`[]T`, `str`)**: descritores `(ptr, len)` com proveniência; elisão de cópias e de bounds checks depende de prova, e acessos dinâmicos continuam exigindo validação;
4. **Direção de alvos**: core reutilizável em hosts, WebAssembly e, futuramente, bare-metal; cada alvo exige layout, codegen, startup/linker/runtime e validação próprios. Não há SDK Cortex-M/RISC-V publicado por esta proposta;
5. **Compatibilidade futura com CTFE (RFC 0013, Draft)**: a pureza do `core` pode facilitar avaliações em compilação, mas não implica que todas as APIs serão interpretáveis. Só o subconjunto explicitamente suportado e aceito pela RFC 0013 poderá ser avaliado.

### 1.1 Estado da implementação (reconciliado em 2026-10-15)

A fronteira executável atual já contém `std.core.fixed` (Q16.16),
`std.core.io` (`Reader`, `Writer`, `Seeker`, `SliceReader` e `SliceWriter`) e
algoritmos de `std.core.str` sem símbolos `extern "C"`: a conversão de `str` em
bytes é um intrínseco do compilador que preserva proveniência e não aloca. O
módulo de paralelismo, que depende de heap e threads do runtime, reside em
`std.parallel`; não existe wrapper legado em `std.core.parallel`.

O marco ainda é parcial: somente Q16.16 está implementado; Q8.8/Q32.32,
aritmética checked completa, intrínsecos de bits e validação em targets
bare-metal permanecem. O núcleo CTFE da RFC 0013 já está integrado, mas
isso não admite todas as APIs do core. “100% comptime-friendly” não é uma
garantia atual; a admissão depende do contrato público e do subconjunto da VM. `[]T`, `ref []T` e `mut ref []T` compartilham o
ABI de duas words `(data, len)` em C, Cranelift e Wasm; `Reader` recebe
`mut ref []u8`, e a exclusividade existe apenas nos fatos de ownership/OSSA,
sem word adicional na ABI nem alocação na heap. O lowering pode materializar
temporariamente o par em stack slots internos, invisíveis à semântica.

---

## 2. Motivação (Motivation)

### 2.1 Requisitos que motivam a separação

Firmware e kernels precisam controlar dependências de SO, heap, threads,
startup e tratamento de falhas. A divisão core/alloc/host torna essas escolhas
explícitas. Tamanho de panic, presença de símbolos e desempenho devem ser
medidos no objeto final com alvo/configuração declarados; não há uma comparação
universal em bytes com outras linguagens nem certificação de segurança implícita.

### 2.2 A Oportunidade do Arandu

A proposta reaproveita mecanismos existentes, respeitando seus limites:
1. **Modelo de Memória Semântica e OSSA ([RFC 0007](0007-semantic-memory-model.md))**: Confinamento estático de ciclo de vida e empréstimos sem anotações léxicas manuais de tempo de vida;
2. **Colorless Async ([RFC 0008](0008-async-runtime-and-colorless-model.md))**: máquinas de estado e RelativeBorrow nos casos admitidos; storage e escape precisam de prova antes de prometer ausência de heap;
3. **Sistema de Efeitos Explícitos (`@Effects`)**: flags/anotações existem, mas a inferência ampliada e enforcement têm [contrato próprio](../arandu-effects-v0.1.md). `LocalMut` não é flag pública implementada, e pureza não garante admissão CTFE;
4. **Layout Parametrizável por Alvo (`TargetInfo` e `DataLayout`)**: perfis de ponteiro 32/64 bits e identidade explícita do alvo, sem presumir ABI do host. Alvos de 16 bits ainda exigem desenho e implementação próprios.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Proposta de código freestanding e MMIO

Exemplos de firmware/MMIO e typestate abaixo são desenho ilustrativo: `std.embedded.mmio`, handlers e toolchain bare-metal não estão implementados. As assinaturas conceituais não introduzem lifetimes ou sintaxe novos no contrato atual. Implementação precisa definir volatile, barreiras, interrupções e obtenção única de periféricos; ownership local sozinho não prova exclusividade contra ISR/DMA:

```arandu
// Firmware bare-metal para microcontrolador ARM Cortex-M
import std.core.mem as mem
import std.core.slice as slice
import std.core.math as math
import std.core.intrinsics as intrinsics
import std.embedded.mmio as mmio

// 1. Acesso Cru a Registrador MMIO (Primitiva de baixo nível)
const GPIOA_ODR: ptr[mut u32] = 0x40020014 as ptr[mut u32]

public func alternarLedCru(): void {
    unsafe {
        let atual = *GPIOA_ODR
        *GPIOA_ODR = atual ^ 0x00000020 // Alterna pino 5
    }
}

// 2. Modelagem Segura de Periféricos MMIO (Padrão Singleton + Bitfields Fortemente Tipados)
// Exige contrato de obtenção única e coordenação com ISR/DMA; OSSA local não basta.
public struct GpioPortA {
    // Endereço base resolvido estaticamente em tempo de compilação (RFC 0013)
    const BASE_ADDR: usize = 0x40020000

    // Registrador tipado com permissões de campo (Read/Write)
    public func alternarPino(mut ref self, pino: u8): void {
        mmio.writeVolatile(Self.BASE_ADDR + 0x14, mmio.readVolatile(Self.BASE_ADDR + 0x14) ^ (1 << pino))
    }
}
```

### 3.1.1 Typestates de Hardware e Segurança de Periféricos
A proposta usa **typestate** para restringir operações disponíveis por estado. Sua implementação ainda precisa definir o mapeamento para o sistema de tipos e para o hardware:
```arandu
// A UART começa no estado Não-Inicializada
struct Uart<Estado> { /* ... */ }
enum Desabilitada {}
enum Habilitada {}

func inicializar(uart: own Uart<Desabilitada>, baudRate: u32): own Uart<Habilitada> {
    // Configura registradores de hardware...
    return Uart<Habilitada> { /* ... */ }
}

// O método transmitir só existe para o tipo Uart<Habilitada>!
func transmitir(mut ref uart: Uart<Habilitada>, byte: u8): void {
    // Envia byte para o registrador de transmissão (MMIO)
}
```
A API pretendida deve rejeitar chamadas em estado incompatível; custo de wrapper/layout será verificado no artefato, sem promessa antecipada de zero bytes.

### 3.2 O Modelo Canônico de Tratamento de Erros: `Result<T, E>`

Erros recuperáveis usam `Result<T,E>` existente, com passagem/retorno definidos pela ABI do alvo. Traps fatais não se tornam Result automaticamente. Ausência de unwinding não proíbe DWARF de depuração; `build --debug` já emite metadados:

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
Assembly ilustrativo, não saída medida do Arandu: Thumb/TrapCode32 e o canal de diagnóstico ainda precisam de implementação e teste de objeto. Não há garantia atual de seis bytes ou de código em um registrador específico.

### 3.4 Matemática de Ponto Fixo para Hardware sem FPU

Para processadores sem unidade de ponto flutuante por hardware (ARM Cortex-M0, microcontroladores automotivos de 8/16/32 bits), o `core` expõe tipos nativos de ponto fixo:

```arandu
import std.core.fixed as fixed

public func aplicarEscala(valor: fixed.Q16_16, escala: fixed.Q16_16): fixed.Q16_16 {
    return valor.mul(escala)
}
```

### 3.5 Corrotinas e Concorrência Cooperativa sem Alocação na Heap

A proposta busca corrotinas com storage fornecido/limitado pelo chamador em firmware. O modelo async atual tem caminhos stack-first, mas também alocação e runtime; o exemplo não comprova execução bare-metal sem heap:

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

### 4.1 Requisitos de aceitação do core freestanding completo

Qualquer módulo que resida em `arandu_core` deve satisfazer formalmente estes 6 invariantes:

1. **Invariante 1 — Zero Heap Global**: Nenhuma função, tipo ou macro do `core` pode invocar direta ou indiretamente alocadores dinâmicos globais (`malloc`, `free`, alocador de sistema). Toda memória dinâmica deve ser fornecida explicitamente pelo chamador através de fatias (`[]T`) ou buffers pré-alocados.
2. **Invariante 2 — Zero OS / Zero Syscall**: Proibida a inclusão de chamadas de sistema (POSIX, Win32, Linux syscalls). O código deve ser capaz de inicializar em hardware bruto com a CPU em reset.
3. **Invariante 3 — Zero Threading de Kernel**: Não há primitivas de criação de threads de sistema operacional (`pthread_create`, `CreateThread`). A concorrência no `core` restringe-se a operações atômicas de hardware (`std.core.atomic`), interrupções de hardware e corrotinas cooperativas.
4. **Invariante 4 — Sem unwinding implícito**: erros recuperáveis são valores pela ABI do alvo; produção pode remover metadados, enquanto DWARF de depuração é escolha independente.
5. **Invariante 5 — Zero Panic Text Bloat**: Asserções de integridade em código de produção compilam para traps de hardware diretos (`UD2` em x86_64, `BKPT`/`UDF` em ARM, `EBREAK` em RISC-V), com um identificador de 32 bits (`TrapCode`) repassado via registrador da ABI.
6. **Invariante 6 — Compatibilidade com o subconjunto CTFE**: funções de `arandu_core` destinadas a uso em compilação devem permanecer interpretáveis pelo subconjunto documentado da AMIR (conforme a proposta [RFC 0013](0013-deterministic-ctfe-and-comptime-metaprogramming.md)). Pureza não garante suporte da VM já integrada; somente o subconjunto do [contrato público](../arandu-comptime-core-v0.1.md) é admitido.

### 4.2 Topologia de Módulos do `arandu_core`

Topologia pretendida, não inventário de módulos implementados. O inventário real é `stdlib/core/`; Q16.16 está em `std.core.fixed` e recursos host ficam fora do core. Novos SIMD/atomic/crypto/MMIO exigem contratos e targets próprios:

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

Desenho futuro de TrapCode32 e diagnóstico externo. Abort já vira trap em Cranelift e Wasm; o contrato completo abaixo, os perfis `--profile=embedded/debug` e os códigos públicos não estão implementados:

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
4. **Layout dependente do alvo**: Toda representação de tipo respeita `TargetInfo` (ex: `usize` segue os perfis 32/64 admitidos; suporte nativo a ponteiros de 16 bits é proposto, não implementado).

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
   - A decisão do Rust (iniciada em 2014) de isolar `core` de `alloc` e `std` provou-se um dos maiores sucessos de engenharia da linguagem, viabilizando o Rust em kernels (Linux Kernel Rust support) e sistemas embarcados. A divisão conceitual inspira o projeto; o contrato freestanding e o tamanho final ainda precisam de validação por alvo.
2. **Diretrizes MISRA C / AUTOSAR**:
   - Padrões de segurança crítica para software automotivo e aeroespacial proíbem alocação dinâmica de memória (`malloc`/`free`) após a fase de inicialização e vetam exceções de C++. Esses requisitos motivam a proposta; não há certificação MISRA/AUTOSAR ou de segurança funcional declarada.
3. **Klein et al. (2009) — seL4: Formal Verification of an OS Kernel (SOSP '09)**:
   - Demonstra que verificação formal exige modelo, hipóteses e provas explícitos; não é evidência de verificação formal do Arandu.
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

A ordem e os gates de produto vivem no [roadmap mestre](../arandu-compiler-roadmap-v0.1.md). Freestanding/ABI seguem a trilha condicional 0.5.x; exemplos não aceitam esta RFC inteira nem ampliam a matriz do SDK.

1. **Certificação ISO 26262 / IEC 61508**: Criar um subconjunto estrito e formalizado do `arandu_core` destinado à certificação de segurança funcional em veículos autônomos, aviônica e dispositivos médicos.
2. **Biblioteca de Redes Neurais Embarcadas (TinyML)**: Implementação de operadores de inferência de tensores inteiros quantizados (INT8) inteiramente dentro de `std.core.math`, permitindo rodar modelos de Machine Learning em microcontroladores com 64 KB de Flash.
3. **Simulador e Test Runner Freestanding**: Ferramenta nativa do compilador para executar a suíte de testes de `arandu_core` diretamente em simuladores QEMU de arquiteturas Cortex-M e RISC-V sem intervenção manual de scripts.
