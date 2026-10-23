# RFC 0030: FFI Universal, Vistas Zero-Copy e Exportação Poliglota (`arandu_ffi_export`)

- **Número da RFC:** 0030
- **Título:** FFI Universal, Vistas Zero-Copy e Exportação Poliglota (`arandu_ffi_export`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-15
- **Status:** `Draft`
- **Área Principal:** `Frontend` / `Middle-end` / `Tooling` / `ABI`
- **Documentos Relacionados:**
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/arandu-abi-layout-v0.1.md`
  - `docs/rfcs/0002-canonical-attribute-naming.md`
  - `docs/rfcs/0007-semantic-memory-model.md`
  - `docs/rfcs/0009-borrowed-views-safety.md`
  - `docs/rfcs/0014-native-wasm-component-model-and-runtime.md`
  - `docs/rfcs/0015-native-mobile-architecture-and-zero-copy-interop.md`

> **Estado auditado em 2026-10-22:** A anotação `@Export` está implementada
> conforme §3.2.1: símbolo plano, ABI C, `nounwind`, validação `T049`,
> agregados `@Repr("C")` por valor classificados por alvo, fatias e `str`
> como pares `(ptr, len)`, `Result<T, E>` como status + out-parameter e
> cabeçalho C gerado sem backend (`--emit=header`) com asserções de tamanho,
> alinhamento e offset. Handles opacos e `@Destructor` já descrevem ownership
> no cabeçalho. Exportação poliglota universal, vistas zero-copy,
> `@FfiResource`, callbacks e bindings para Python/Node/Swift continuam em
> proposta. O compilador hoje também suporta declarações `extern func`,
> `@Link`, verificação `@Unsafe` (`O013`) e classificação `@Repr("C")`. A fila
> permanente vive no [roadmap mestre](../arandu-compiler-roadmap-v0.1.md).
> **Regra de integridade de falhas:** no modelo nativo sem tabelas de unwinding
> (Invariante 5 — `UD2`/`BRK`), um abort fatal de hardware **não** é uma exceção
> recuperável dentro do mesmo processo nativo. Fronteiras FFI resilientes usam
> validação no shim de entrada e retorno explícito de `Result<T, E>` mapeado
> para códigos de status (`ArFfiStatus`), ou isolamento via WebAssembly Component
> Model / processo separado quando for necessário sobreviver a traps fatais.

---

## 1. Resumo (Summary)

Esta RFC propõe a arquitetura de **FFI Universal (*Foreign Function Interface*) e Exportação Poliglota** do Arandu, cobrindo tanto o consumo seguro de bibliotecas C quanto a exportação de bibliotecas Arandu para C, Python, Node.js/TypeScript e Swift.

O design baseia-se em quatro pilares técnicos:
1. **Vistas Emprestadas Síncronas Zero-Copy (`ref []T` / `mut ref []T`)**: Passagem direta de fatias contíguas como pares `(ptr, len)` na C-ABI durante chamadas síncronas, integrando-se ao *Python Buffer Protocol* (`Py_buffer`), *Node-API TypedArrays* e *Swift `UnsafeBufferPointer`* sem serialização intermediária;
2. **Recursos Externos Gerenciados pelo OSSA (`@FfiResource`)**: Vinculação declarativa de ponteiros opacos de C (ex.: `sqlite3*`, `FILE*`) às suas funções destruidoras nativas na instrução `Destroy` da AMIR, prevenindo vazamentos de recursos e *double-free*;
3. **Contrato Explícito de Erros sem Unwinding (`Result<T, E>` $\rightarrow$ `ArFfiStatus`)**: Tradução determinística de retornos `Result<T, E>` e pré-condições de fronteira (ponteiros nulos, alinhamento, limites e UTF-8) em estruturas de status C-ABI, sem depender de *stack unwinding* nem prometer captura insegura de sinais de hardware (`SIGILL`/`SIGABRT`);
4. **Síntese Determinística de Bindings (`arandu build --export`)**: Geração de cabeçalhos C11 (`.h`), wrappers Python (`.py`/`.pyi`), definições Node-API (`.d.ts`) e módulos Swift a partir dos contratos tipados da AMIR.

---

## 2. Motivação (Motivation)

### 2.1 Limitações das Abordagens Existentes na Indústria
1. **Serialização Obrigatória no Mozilla UniFFI (Rust)**:
   O UniFFI prioriza segurança isolando linguagens através de *buffers* serializados (`RustBuffer`). Ao passar um vetor `Vec<f32>` ou fatia entre Rust e Swift/Python/Kotlin, os dados são serializados e copiados na memória. Embora seguro para configurações e metadados pequenos, esse modelo inviabiliza o processamento zero-copy de tensores, áudio, vídeo e grandes volumes colunares.
2. **Troca de Pilha e Latência no `cgo` (Go)**:
   Cada chamada FFI em Go precisa transicionar do escalonador de goroutines (pilha dinâmica) para a pilha de thread do sistema operacional e adquirir locks de runtime, custando dezenas de vezes mais que uma instrução `CALL` direta da C-ABI.
3. **A Ilusão de `catch_unwind` vs. Aborts Nativos**:
   Em Rust, cruzar a fronteira FFI com um pânico é *Undefined Behavior*; por isso, ferramentas como UniFFI compilam binários com tabelas pesadas de *stack unwinding* (`.eh_frame` / DWARF) e envolvem cada função em `std::panic::catch_unwind`. Já em C, tentativas de interceptar sinais fatais (`SIGSEGV`/`SIGILL`) com `siglongjmp` corrompem o estado do alocador e pulam destruidores locais.
4. **Vazamento de Recursos em `extern "C"` Cru**:
   Tanto em C quanto em blocos `unsafe` de Rust e Zig, ponteiros opacos retornados por bibliotecas externas (`sqlite3_open`, `EVP_CIPHER_CTX_new`) não possuem ciclo de vida rastreado automaticamente, exigindo wrappers manuais repetitivos.

### 2.2 Como o Arandu Combina Segurança e Desempenho
* **Zero-Copy Síncrono Governado pelo OSSA**: Como `ref []T` e `mut ref []T` no Arandu **não podem escapar** da função chamada (rejeitado estaticamente por `O010` e `BV.1–3`), o compilador garante que o código Arandu não guardará o ponteiro do Python/Node.js após o retorno da chamada síncrona!
* **Erros como Valores (`Result<T, E>`)**: No Arandu, falhas recuperáveis de domínio, I/O ou validação são expressas no tipo de retorno `Result<T, E>` (e não por unwinding de exceções). Assim, o shim de FFI traduz erros com custo zero e total segurança de memória.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Consumindo Recursos C com Destruidor OSSA (`@FfiResource`)

```arandu
import std.c as c

@Link("sqlite3")
@Unsafe
extern func sqlite3_open(filename: *const c.char, dbOut: *mut *mut c.void): c.int

@Link("sqlite3")
@Unsafe
extern func sqlite3_close(db: *mut c.void): c.int

// O compilador emite a chamada a `sqlite3_close(self.raw)` no `Destroy` do OSSA
@FfiResource(destructor = sqlite3_close)
public struct SqliteDb {
    raw: *mut c.void
}

public func openDatabase(path: c.CStr): Result[own SqliteDb, c.int] {
    let mut handle: *mut c.void = c.nullMut()
    let rc = unsafe { sqlite3_open(path.asPtr(), c.addrOfMut(mut handle)) }
    if rc != 0 {
        unsafe {
            if !handle.isNull() {
                let _ = sqlite3_close(handle)
            }
        }
        return Result.Err(rc)
    }
    return Result.Ok(SqliteDb { raw: handle })
}
```

Regras garantidas pelo compilador:
* `SqliteDb` é um tipo linear (`own`), **não-`Copy`**.
* Quando `SqliteDb` sai de escopo (ou em retornos antecipados de erro `?`), a fase de elaboração de drop da AMIR (`drop_elaborate`) insere a chamada ao destruidor `sqlite3_close`.
* Mover `SqliteDb` transfere a posse sem duplicar o ponteiro nem chamar o destruidor duas vezes.

### 3.2 Exportando Funções Zero-Copy com Contrato de Erro Explícito (`@FfiExport`)

```arandu
public enum AudioError {
    EmptyBuffer,
    InvalidGain,
}

@FfiExport
@Effects(NoAlloc, NoThrow, NoSuspend)
public func normalizeSamples(
    samples: mut ref []f32,
    gain: f32
): Result[void, AudioError] {
    if samples.isEmpty() {
        return Result.Err(AudioError.EmptyBuffer)
    }
    if gain <= 0.0 {
        return Result.Err(AudioError.InvalidGain)
    }

    for i in 0..samples.len() {
        samples[i] = samples[i] * gain
    }
    return Result.Ok(())
}
```

#### 3.2.1 Estado implementado na 0.1.11 (`@Export`)

A superfície canônica implementada é `@Export` (a forma `@FfiExport` continua
aceita como alias legado, com aviso `W008`). Ela resolve a intenção inteira em
uma única anotação: símbolo plano sem mangling, ABI C do alvo, linkage externo
e `nounwind`. O nome pode ser fixado com `@Export("nome_c")`, útil para
prefixos de SDK e para manter o símbolo estável quando o módulo muda de nome.

O subconjunto validado hoje cobre funções livres (sem receiver, sem `async`,
sem genéricos) com primitivos de largura fixa (`i8`–`i64`, `u8`–`u64`, `f32`,
`f64`, `bool`), os aliases de largura definida pela RFC 0023 (`int`/`uint` como
32 bits, `isize`/`usize` como largura de ponteiro do alvo), ponteiros crus
(`ptr T` e `ref T`/`mut ref T` como `const T *`/`T *`), structs `@Repr("C")`
por valor, fatias síncronas `[]T`/`mut []T` (modeladas como `{ ptr, len }`),
`str` como `AranduStr` (`{ const uint8_t *ptr; size_t len; }`, UTF-8 sem
terminador) e retorno `void`. `Result<T, E>` é aceito no retorno e baixa para
status C + out-parameter (abaixo). `char`, tipos sem representação C
(`Option`, tuplas, `any`), structs sem `@Repr("C")` e campos
recursivamente inseguros são rejeitados com `T049`.

Erros não usam canal de exceção: um `@Export` que retorna `Result<T, E>` é
reescrito, uma única vez, no AMIR (`lower_export_status`) para
`E f(..., T *out)` — `Ok(v)` grava `v` no out-parameter e devolve `0`,
`Err(e)` devolve `e`. A transformação é tudo-ou-nada: `E` precisa ser inteiro
de largura fixa, todo `return` precisa construir `Ok`/`Err` diretamente, um
`@Destructor` não pode retornar `Result`, e uma função exportada nessa forma
não pode ser chamada de dentro do Arandu, porque a assinatura muda na
fronteira. Como o passe vive no AMIR, todo backend herda a mesma forma e o
cabeçalho deriva o out-parameter da mesma assinatura.

Arrays (`[N]T`) existem apenas como campo de struct `@Repr("C")`, onde viram
`T campo[N]`; como parâmetro decairiam silenciosamente para ponteiro, então
são rejeitados. `str`, `int` e `usize` não são “tipos soltos” na fronteira:
cada um tem uma única grafia C documentada e o cabeçalho usa exatamente essa
grafia.

A passagem de agregados não presume layout: `crates/arandu_middle/src/layout/
export_abi.rs` classifica cada tipo em lanes (`iN`, `float`, `double`,
`[N x iW]`, `<2 x float>`, `byval`/`sret`) por família de ABI — System V AMD64,
Windows x64, AAPCS64, AAPCS32 (soft e hard-float, incluindo o caso Cortex-M de
structs de 5–8 bytes em `r0`/`r1`) e RISC-V ILP32. As regras foram derivadas de
`clang -target … -S -emit-llvm` e são cobertas por testes que fixam as larguras
exatas (`i24`, `i40`, `i48`, `i56`) e os limites de indireção de cada alvo.

O backend LLVM materializa a fronteira em memória: cada lane é armazenada ou
carregada no offset que o classificador atribuiu, com `align 1` (acesso byte a
byte é sempre válido e não inventa alinhamento). Um alvo sem classificador
falha com `T049` em vez de herdar silenciosamente a ABI do host.

`arandu build --emit=header` gera o cabeçalho C a partir de um contrato único
(`crates/arandu_ffi`), sem depender de backend: as mesmas assinaturas e o mesmo
`LayoutEngine` descrevem o objeto, qualquer que seja o emissor. O cabeçalho tem
`typedef`s de struct, `typedef { const T *ptr; size_t len; } AranduSlice*` e
`AranduMutSlice*` (fatias mutáveis e imutáveis nunca colapsam), `AranduStr`,
protótipos e `_Static_assert` de tamanho, alinhamento e offset de cada campo. A
largura de ponteiro vem do alvo pedido, não do host: `--emit=header
--target=thumbv6m-none-eabi` emite `_Static_assert(sizeof(void *) == 4)` e os
tamanhos de 32 bits, e um consumidor compilado com o modelo errado falha no
`#include`. O guard é derivado do conjunto exportado, então dois cabeçalhos
podem conviver na mesma unidade de tradução. Nomes de tipo que colidiriam com
os `typedef`s gerados (`AranduStr`, `AranduSlice*`, `AranduMutSlice*`) ou entre
si são diagnosticados em vez de gerar C inválido. Tipos que só atravessam por
ponteiro viram apenas declaração antecipada (`typedef struct Buffer Buffer;`),
então um handle pode conter campos que nunca cruzariam por valor (um `Vec`, um
gerador) sem vazar layout nem exigir `@Repr("C")`. Um consumidor C que discorde
do layout deixa de compilar em vez de corromper memória; o teste e2e compila
esse cabeçalho com `clang -Wall -Wextra`, liga contra o objeto Arandu e executa
a função. Comentários `///` da função exportada e do tipo opaco viajam no mesmo
contrato e viram blocos `/** … */` acima do protótipo ou do `typedef`, com `*/`
escapado; declaração sem doc não gera bloco vazio. Se `TypeInfo::destructors`
associa ao tipo um `@Destructor` exportado, o `typedef` e todo retorno `ptr T`
documentam a função de liberação e a posse do handle; sem destrutor exportado,
o retorno avisa que o chamador não tem função de liberação. Essas notas
descrevem o contrato existente e não produzem diagnóstico.

Callbacks C (`void (*)(...)`) não têm tipo Arandu correspondente ainda. Tipos
de função como `(u32) -> u32` existem e um nome de função é um valor, mas
chamar através desse valor é rejeitado com `T033` (`Arandu` só faz chamadas
diretas). Exportar um callback exige, portanto, chamadas de primeira classe no
AMIR e em todos os backends (RFC 0032), não apenas um `typedef` no cabeçalho.

Colisões de símbolo entre duas funções exportadas são diagnosticadas em tempo
de compilação, ao contrário do comportamento silencioso de `#[no_mangle]`.
A única forma de exportar um método é `@Destructor`: ele é a função de liberação
do tipo e cruza como `void name(T *self)`, sem exigir que o C conheça o layout
do handle.
Definições sem `@Export` mantêm linkage interno no objeto, então `pub` não
publica nada para o C por acidente.

### 3.3 Consumo a partir do Python (Buffer Protocol Zero-Copy)

O wrapper Python gerado adquire o `Py_buffer` com flag `PyBUF_WRITABLE`, passa `(buf.buf, buf.len / sizeof(float))` para a função C-ABI do Arandu e libera o `Py_buffer` imediatamente no bloco `finally`:

```python
import numpy as np
import arandu_audio

samples = np.ones(1_000_000, dtype=np.float32)
# Opera in-place sobre o buffer do NumPy sem alocar nem copiar memória:
arandu_audio.normalize_samples(samples, 1.5)
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Convenção de Chamada C-ABI e Shim de Validação
Para a função `normalizeSamples` acima, o compilador gera um ponto de entrada C-ABI com prefixo determinístico:

```c
typedef struct {
    int32_t tag;       // 0 = Ok, >0 = Domain Err (AudioError), <0 = FFI Precondition Error
    int32_t err_code;  // Discriminante de AudioError quando tag > 0
} ArFfiResult_void_AudioError;

ArFfiResult_void_AudioError ar_ffi_normalizeSamples(
    float* samples_ptr,
    size_t samples_len,
    float gain
);
```

O **shim de entrada gerado** valida todas as pré-condições de fronteira antes de invocar o corpo AMIR:
1. **Validação de Ponteiro e Alinhamento**: Se `samples_len > 0` e (`samples_ptr == NULL` ou `(uintptr_t)samples_ptr % alignof(float) != 0`), retorna imediatamente `{ .tag = -1 /* AR_FFI_ERR_INVALID_PTR */, .err_code = 0 }` sem entrar no corpo da função.
2. **Validação de UTF-8 para `str`**: Parâmetros `str` passados por FFI têm sua validade UTF-8 verificada no shim (via validador SIMD `A7`), retornando `AR_FFI_ERR_INVALID_UTF8` se malformados.
3. **Proibição de Escape Assíncrono**: O verificador OSSA garante que nenhum parâmetro `ref []T`, `mut ref []T` ou `str` recebido em `@FfiExport` escape da chamada síncrona.

### 4.2 Política Honesta de Traps Fatais vs. Erros Recuperáveis
Em conformidade com o Invariante 5 do Arandu:
* **Erros recuperáveis**: Devem ser modelados como `Result<T, E>`. O shim de entrada e as operações verificadas convertem falhas em `ArFfiResult` sem qualquer unwinding.
* **Traps fatais em modo nativo (`UD2` / `BRK` / corrupção de heap)**: No processo nativo C-ABI, um trap de hardware encerra o processo imediatamente (fail-fast). O compilador **não** instala handlers `SIGILL`/`SIGSEGV` com `longjmp`, pois isso violaria a segurança de memória do processo hospedeiro.
* **Isolamento de falhas fatais**: Quando o hospedeiro exige isolamento total contra traps fatais ou estouro de memória de um plugin, o alvo recomendado é o **WebAssembly Component Model ([RFC 0014](0014-native-wasm-component-model-and-runtime.md))**, onde a instrução `unreachable` do Wasm aborta apenas a instância isolada do componente, preservando o hospedeiro intacto.

### 4.3 Contratos de Buffer por Runtime Alvo
1. **C / C++ (`.h`)**: Structs de fatia tipadas `ArSlice_f32 { const float* ptr; size_t len; }` e `ArSliceMut_f32 { float* ptr; size_t len; }`, com `static_assert` de tamanho e alinhamento.
2. **Python (C-API / `ctypes` / `cffi`)**: Aquisição via `PyObject_GetBuffer(obj, &view, PyBUF_CONTIG_RO | PyBUF_FORMAT)` para `ref []T` e `PyBUF_CONTIG | PyBUF_FORMAT` para `mut ref []T`, com `PyBuffer_Release(&view)` garantido antes do retorno da chamada C.
3. **Node.js (Node-API / N-API)**: Extração síncrona via `napi_get_typedarray_info` ou `napi_get_buffer_info`. Para impedir que o GC do V8 mova ou invalide o buffer durante a execução, funções `@FfiExport` que recebem fatias zero-copy **não** podem invocar callbacks de volta para o JavaScript durante o empréstimo.
4. **Swift**: Mapeamento automático para closures `withUnsafeBufferPointer` e `withUnsafeMutableBufferPointer`.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Invariantes Preservados
* **`DataLayout` e `TargetInfo`**: Todos os tipos admitidos em `@Export` e `extern func` são validados pelo classificador de ABI do alvo (`SysV AMD64`, `Windows x64`, `AAPCS64`, `AAPCS32` soft/hard-float e RISC-V ILP32). Tipos sem layout C definido são rejeitados com diagnóstico estruturado.
* **Sem Dependência de Clang/LLVM no Compilador**: A emissão de cabeçalhos `.h` e wrappers poliglotas é puramente baseada na tabela de tipos e assinaturas da AMIR — `--emit=header` não invoca `clang`.

### 5.2 Limitações e Trade-offs
* **Restrição Síncrona de Fatias Zero-Copy**: Fatias `ref []T` emprestadas de runtimes gerenciados (Python/JS/Swift) só existem durante o frame da chamada síncrona. APIs assíncronas que cruzem threads exigem cópia explícita ou handles proprietários (`own`).
* **Proibição de Callbacks Reentrantes sobre Buffers Mutáveis**: Em runtimes como Node.js ou Python, chamar de volta o interpretador enquanto um `mut ref []T` está emprestado poderia violar a exclusividade do OSSA; por isso, funções `@FfiExport` com vistas zero-copy proíbem callbacks reentrantes não verificados.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

* **Alternativa A: Serializar todos os vetores em buffers próprios (estilo UniFFI puro):**
  * *Rejeitada como caminho único*: Impõe cópias de memória O(N) em cada chamada. O Arandu oferece vistas zero-copy para fatias síncronas e reserva handles opacos ou cópia explícita apenas para tipos complexos que mudam de posse.
* **Alternativa B: Usar `setjmp`/`longjmp` ou captura de sinais POSIX para interceptar `UD2`:**
  * *Rejeitada*: Capturar sinais síncronos de hardware (`SIGILL`/`SIGTRAP`) e saltar sobre frames da pilha em C/Arandu é comportamento indefinido (POSIX.1-2008) e vaza recursos/locks. O uso de `Result<T, E>` explícito no nível da linguagem é estritamente mais seguro e rápido.

---

## 7. Arte Prévia (Prior Art)

* **Mozilla UniFFI & Diplomat (Rust)**: Referências na geração declarativa de bindings multi-linguagem; o Diplomat (usado pelo ICU4X) inspira especialmente a passagem direta de fatias e escrita em buffers fornecidos pelo chamador sem alocação.
* **Python Buffer Protocol (PEP 3118)**: Especificação padrão de compartilhamento de memória multidimensional sem cópia entre C e bibliotecas científicas Python (NumPy, PyArrow).
* **Swift C Interoperability (`__counted_by` / `Span`)**: Evolução recente de C e Swift para associar ponteiros a seus comprimentos em fronteiras FFI.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Handles Opacos para Objetos `own` Exportados**: Qual será a representação canônica quando o hospedeiro Python/JS precisar reter um objeto proprietário do Arandu entre várias chamadas (ponteiro opaco tipado com função `_free` gerada vs. handle geracional `GenRef`)?
2. **Anotações de Segurança em Headers C Gerados**: Devemos emitir atributos modernos do Clang/GCC como `__attribute__((nonnull))` e `__counted_by(len)` nos arquivos `.h` exportados?

---

## 9. Possibilidades Futuras (Future Possibilities)

* Integração com o subcomando planejado `arandu bindgen` (`E3` no roadmap) para engenharia reversa segura de cabeçalhos C11 restritos para declarações `@Repr("C")` e `@FfiResource`.
