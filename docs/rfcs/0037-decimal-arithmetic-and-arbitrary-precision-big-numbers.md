# RFC 0037: Aritmética Decimal de Valor Fixo (128 bits) e Números de Precisão Arbitrária (`Decimal`, `BigInt`, `BigDecimal`)

- **Número da RFC:** 0037
- **Título:** Aritmética Decimal de Valor Fixo (128 bits) e Números de Precisão Arbitrária (`Decimal`, `BigInt`, `BigDecimal`)
- **Autor(es):** Arquiteto da Linguagem Arandu
- **Data de Início:** 2026-10-16
- **Status:** `Draft`
- **Área Principal:** `Stdlib` | `Middle-end` | `Runtime`
- **Escopo:** `std.core.decimal` (implementado na fundação `std.core`), `std.math.big` (protótipos locais de `BigInt`/`BigDecimal`, ainda em validação) e integração `comptime`

---

## Fronteira de implementação e evidência

Esta RFC estabelece a arquitetura numérica de três camadas do Arandu para computação financeira, comercial, científica e criptográfica.

- **Implementado nesta revisão (2026-10-16):**
  - Camada 1: Tipos primitivos de hardware (`int`, `uint`, `i8`..`i64`, `u8`..`u64`) e ponto flutuante binário (`float`, `f32`, `f64`). A representação IEEE não certifica conformidade integral de todas as operações, conversões ou backends.
  - Camada 2 (`std.core.decimal`): Tipo de valor `Decimal` de 128 bits (mantissa inteira de 96 bits + sinal + escala decimal `0..=28`), armazenamento por valor (`Copy`), sem alocação explícita na API, construtores (`zero`, `one`, `fromInt`, `fromI64`, `fromCents`, `fromParts`, `fromStr`), comparações com normalização de escala (`eq`, `cmp`, `lt`, `le`, `gt`, `ge`), aritmética de precisão finita (`checkedAdd`/`checkedSub`/`checkedMul`, wrappers `add`/`sub`/`mul` e `div`) e arredondamento configurável (`RoundingMode`: `HalfEven`, `HalfUp`, `Truncate`, `Floor`, `Ceil`).
- **Implementação local inicial, ainda sem fechamento:**
  - Camada 3 ([`std.math.big`](../../stdlib/math/big.aru)): `BigInt` com magnitude em `Vec<u64>` e sinal bool; `BigDecimal` com mantissa `BigInt` e escala `i64`. Existe corpus inicial nos backends nativos; faltam oráculos independentes além de 128 bits, falhas de alocação, ownership/drop, divisão/arredondamento e limites de escala/trabalho.
  - O buffer usa o alocador atual de `Vec`, sem seleção uniforme de arena/alocador pela API. A implementação não comprova Karatsuba nem operações em tempo constante; comparações podem percorrer limbs. A aplicação criptográfica ilustrada na proposta exige provas próprias. A fila de fechamento é ST.15 no [roadmap mestre](../arandu-compiler-roadmap-v0.1.md).
- **Proposta separada, fora do fechamento autorizado:**
  - Sufixos literais dedicados (como `0.10d`) caso a ergonomia de `Decimal.fromStr` em `comptime` demande açúcar sintático no léxico.

---

## 1. Resumo (Summary)

Esta RFC define uma separação estrita e ergonômica entre três domínios numéricos no Arandu:
1. **Ponto Flutuante Binário (`float`, `f32`, `f64`):** Velocidade nativa de hardware via FPU conforme IEEE 754-2019, destinado a física, gráficos, processamento de sinais e IA.
2. **Decimal de Valor Fixo de 128 bits (`std.core.decimal.Decimal`):** Estrutura por valor de 16 bytes, com mantissa de 96 bits e escala base 10 de `0` a `28`, garantindo que `0.1 + 0.2 == 0.3` seja matematicamente exato para finanças, tributação e comércio.
3. **Precisão Arbitrária (`std.math.big.BigInt` e `BigDecimal`):** Tipos baseados em buffers dinâmicos para cálculos sem limite de dígitos significativos.

---

## 2. Motivação (Motivation)

Como demonstrado no artigo clássico de David Goldberg (*"What Every Computer Scientist Should Know About Floating-Point Arithmetic"*, 1991), frações decimais elementares como $0,1$ ($\frac{1}{10}$) e $0,2$ ($\frac{1}{5}$) não possuem representação finita na base 2, resultando em dízimas periódicas binárias onde `0.1 + 0.2 != 0.3`.

A análise histórica das linguagens mostra três armadilhas recorrentes:
- **Falta de tipo decimal na biblioteca padrão (ex: Go):** Causa fragmentação severa no ecossistema de bancos de dados, ORMs e bibliotecas financeiras, onde cada pacote define sua própria estrutura incompatível.
- **Uso exclusivo de `BigDecimal` alocado na Heap (ex: Java):** Força toda soma monetária a alocar objetos no heap, degradando a localidade de cache e inviabilizando o uso em ambientes `freestanding`, WebAssembly sem runtime pesado ou sistemas embarcados.
- **Coerção implícita ou tipo numérico único (ex: JavaScript):** Induz erros silenciosos de arredondamento de centavos em produção.

Por outro lado, o modelo de `decimal` de 128 bits por valor consagrado em C#/.NET e na crate `rust_decimal` do ecossistema Rust oferece uma representação decimal de tamanho fixo, enquanto `BigInt` e `BigDecimal` cobrem os casos de precisão ilimitada.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Quando usar cada tipo numérico no Arandu

| Domínio | Tipo Recomendado | Alocação | Características |
| :--- | :--- | :--- | :--- |
| Contadores, índices, centavos simples | `int`, `i64` | Stack (Registrador) | Inteiro binário exato, velocidade máxima da ALU |
| Gráficos, jogos, simulações, sensores | `float`, `f32`, `f64` | Stack (Registrador FPU) | IEEE 754-2019 binário (`0.1 + 0.2 != 0.3`) |
| **Finanças, ERP, impostos, bancos de dados (`NUMERIC`)** | **`std.core.decimal.Decimal`** | **Valor de 16 bytes** | **Base 10 exata até 28 casas decimais (`0.1 + 0.2 == 0.3`)** |
| Teoria dos números, matemática simbólica | `std.math.big.BigInt` / `BigDecimal` | Heap / Arena | Precisão arbitrária com limites de memória, escala e trabalho definidos pela API |

### 3.2 Uso de `std.core.decimal`

```arandu
module app.financeiro

import std.core.decimal as dec

func main(): int {
    let a = dec.Decimal.fromStr("0.1").unwrapOr(dec.Decimal.zero())
    let b = dec.Decimal.fromStr("0.2").unwrapOr(dec.Decimal.zero())
    let c = dec.Decimal.fromStr("0.3").unwrapOr(dec.Decimal.zero())

    let soma = a.add(b)

    // Este caso é exato dentro da mantissa e escala admitidas.
    if !soma.eq(c) {
        return 1
    }

    // Arredondamento bancário (HalfEven: 2.5 -> 2, 3.5 -> 4)
    let taxa = dec.Decimal.fromStr("2.5").unwrapOr(dec.Decimal.zero())
    let arredondado = taxa.round(0 as u32, dec.RoundingMode.HalfEven)
    if arredondado.toInt() != 2 {
        return 2
    }

    return 0
}
```

### 3.3 Direção arquitetural e protótipo: `std.math.big` (`BigInt` e `BigDecimal`)

Para cenários que extrapolam 28 dígitos decimais ou 96 bits de mantissa, o protótipo `std.math.big` usa palavras de 64 bits em `Vec<u64>`. Integração com arenas, o contrato integral e a API ilustrada a seguir permanecem sujeitos ao fechamento descrito acima:

```arandu
// Exemplo ilustrativo da futura camada 3 (std.math.big)
import std.math.big as big

func exemploBig(): void {
    // Inteiro de precisão arbitrária
    // BigDecimal = BigInt (unscaled) + scale: i64
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### Política de precisão e falha do recorte local

- A mantissa é de 96 bits; 128 bits é o tamanho do valor completo, não sua precisão inteira.
- Soma/subtração alinham escalas em um intermediário inteiro de 192 bits; multiplicação conserva o produto de 192 bits. A normalização arredonda uma vez com `HalfEven` quando precisa reduzir escala para caber.
- `checkedAdd`, `checkedSub` e `checkedMul` retornam `None` se nem a escala zero comportar a magnitude. Os wrappers `add`/`sub`/`mul` fazem trap nesse caso; não devolvem um operando nem descartam os bits altos.
- `div` retorna `None` em divisor zero ou overflow; resultados inexatos são arredondados com `HalfEven` dentro da mantissa/escala. Não existe promessa de divisão exata para frações como `1/3`.
- `checkedToI64`, `checkedToInt` e `checkedToCents` rejeitam resultados fora do destino; os wrappers correspondentes fazem trap. Conversão para centavos arredonda antes de escalar para evitar arredondamento duplo.
- Separadores `_` no parsing só são admitidos entre dois dígitos ASCII.
- A implementação não usa heap, I/O ou funções do host explicitamente. A ausência de alocação no programa gerado depende também da promoção de storage/ABI dos backends, conforme o contrato de agregados; não decorre apenas de ser `Copy`.
- O corpus compartilhado `tests/corpus/stdlib_decimal.aru` cobre limites, arredondamento, conversões e parsing. O oráculo em `tests/support/decimal_oracle.rs` compara aritmética de limbs com inteiros Rust de 128 bits em entradas limitadas. A promoção depende dos gates e da matriz instalada da candidata.

### 4.1 Layout de Memória do `Decimal` (128 bits)

O tipo `Decimal` reside em `stdlib/core/decimal.aru` e ocupa exatamente 16 bytes (alinhamento de 4 bytes), composto por quatro campos `u32`:

$$\text{Valor} = (-1)^{\text{sinal}} \times \left(\text{lo} + \text{mid} \cdot 2^{32} + \text{hi} \cdot 2^{64}\right) \times 10^{-\text{escala}}$$

- `lo: u32`: Bits `0..31` da mantissa positiva de 96 bits.
- `mid: u32`: Bits `32..63` da mantissa positiva de 96 bits.
- `hi: u32`: Bits `64..95` da mantissa positiva de 96 bits.
- `flags: u32`:
  - Bits `0..15`: Reservados (sempre zero).
  - Bits `16..23`: Escala decimal no intervalo `0..=28` (`MAX_SCALE = 28`).
  - Bits `24..30`: Reservados (sempre zero).
  - Bit `31` (`0x80000000`): Bit de sinal (`0` = positivo ou zero, `1` = negativo).

### 4.2 Invariantes Semânticos e Normalização

1. **Zero Canônico:** Qualquer `Decimal` com mantissa `lo == 0 && mid == 0 && hi == 0` é considerado zero independentemente do bit de sinal ou da escala. Comparações (`eq`, `cmp`) tratam `-0.0` e `+0.00` como idênticos (`cmp == 0`).
2. **Alinhamento de Escala:** Ao somar, subtrair ou comparar dois valores $A$ (escala $s_A$) e $B$ (escala $s_B$) onde $s_A < s_B$, a mantissa de $A$ é multiplicada por $10^{s_B - s_A}$ enquanto não houver overflow dos 96 bits. Caso a multiplicação exceda 96 bits, a mantissa de maior escala é dividida por potências de 10 com arredondamento `HalfEven`.
3. **Modos de Arredondamento (`RoundingMode`):**
   - `HalfEven` (Padrão Bancário / IEEE 754): Se a fração descartada for exatamente $0,5$, arredonda para o dígito par mais próximo.
   - `HalfUp`: Se a fração descartada for $\ge 0,5$, incrementa a magnitude (afasta de zero).
   - `Truncate`: Descarta os dígitos excedentes (arredonda em direção a zero).
   - `Floor`: Arredonda em direção a $-\infty$.
   - `Ceil`: Arredonda em direção a $+\infty$.

### 4.3 Arquitetura Proposta para `BigInt` e `BigDecimal` (`std.math.big`)

- **`BigInt`**:
  - Representação em magnitude e sinal (`sign: i8`, `limbs: Vec<u64>` em ordem little-endian, sem zeros à esquerda redundantes).
  - Algoritmos escalonados por tamanho:
    - Multiplicação escolar $O(N^2)$ para até 32 limbs; Karatsuba $O(N^{1.585})$ para operandos médios.
    - Divisão longa de Knuth (Algoritmo D, TAOCP Vol. 2).
- **`BigDecimal`**:
  - Composto por `unscaled: BigInt` e `scale: i64`.
  - Conversão sem perda a partir de `std.core.decimal.Decimal`.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

- **Preservação da Camada `std.core`:** `std.core.decimal` é escrito em Arandu sem `extern "C"`, I/O ou alocação explícita. `check-architecture`/`core_foundation` verificam fronteiras locais; suporte freestanding e ausência de heap no artefato exigem provas de link/storage por alvo.
- **Early-Cutoff no Salsa:** Queries e assinaturas compartilhadas devem conservar cutoff; o nome ou a camada do módulo não constituem prova de invalidação mínima.
- **Compatibilidade com CTFE (`comptime`):** Por não usar ponteiros brutos nem chamadas FFI, todas as operações de `Decimal` (incluindo `Decimal.fromStr`) são avaliáveis deterministicamente em tempo de compilação pelo interpretador AMIR CTFE.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

1. **Por que 128 bits (mantissa de 96 bits) em vez de `i64` com escala fixa?**
   - Um `i64` comporta no máximo $\approx 9,22 \times 10^{18}$ (18 dígitos). Ao multiplicar dois valores com 4 casas decimais, a escala intermediária dobra para 8 casas, estourando facilmente 64 bits em balanços corporativos. Com 96 bits ($7,92 \times 10^{28}$), há margem segura para 28 dígitos decimais.
2. **Por que não usar IEEE 754-2008 `decimal128` (DPD / BID)?**
   - Os formatos Densely Packed Decimal (DPD) e Binary Integer Decimal (BID) exigem decodificação bit-a-bit pesada em CPUs x86_64 e ARM64 que não possuem instruções de hardware dedicadas (presentes apenas em mainframes IBM POWER/zArchitecture). O layout com mantissa binária de 96 bits (`3 x u32`) permite usar diretamente instruções nativas de multiplicação/divisão de 64 bits da ALU.

---

## 7. Arte Prévia (Prior Art)

- **C# / .NET (`System.Decimal`):** Layout canônico `lo, mid, hi, flags` de 128 bits na stack; referência de mercado em sistemas financeiros.
- **Rust (`rust_decimal` e `bigdecimal`):** Separação clara entre o tipo de 128 bits `Copy` (`rust_decimal::Decimal`) e o tipo alocado na heap (`bigdecimal::BigDecimal`).
- **Python (`decimal` — PEP 327) e Mike Cowlishaw (*General Decimal Arithmetic Specification*):** Especificação formal dos modos de arredondamento (`ROUND_HALF_EVEN`, `ROUND_HALF_UP`, `ROUND_FLOOR`, `ROUND_CEILING`, `ROUND_DOWN`).
- **David Goldberg (1991):** *"What Every Computer Scientist Should Know About Floating-Point Arithmetic"*, ACM Computing Surveys.

---

## 8. Questões em Aberto (Unresolved Questions)

- A adição futura de sobrecarga de operadores (`+`, `-`, `*`, `/`, `==`) para tipos definidos na stdlib como `Decimal` dependerá da trilha de polimorfismo/interfaces operacionais no type checker. Até lá, os métodos explícitos (`.add()`, `.sub()`, `.mul()`, `.div()`, `.eq()`) garantem clareza e custo previsível.

---

## 9. Possibilidades Futuras (Future Possibilities)

- Sufixo literal `d` (ex: `let preco = 19.99d`) resolvido em `comptime` diretamente para `Decimal`.
- Implementação de `std.math.big.BigInt` e `std.math.big.BigDecimal` sobre `std.alloc.vec.Vec`.
- Formatação localizada de moedas e integração nativa com drivers SQL (`NUMERIC`).
