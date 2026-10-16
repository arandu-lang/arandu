# Guia de Migração de Inteiros — RFC 0023 (série 0.1.x)

## Visão Geral e Contexto

Na série **0.1.x atual**, `int` e `uint` são inteiros de **32 bits fixos**
no modelo de tipos/layout, inclusive em perfis de ponteiro 32/64 bits.
Isso não publica SDK Cortex-M/RISC-V: a matriz nativa suportada continua a do
[contrato de distribuição](arandu-distribution-contract-v0.1.md).
Antes da migração da RFC 0023, `int` seguia a largura do ponteiro: 32 bits em
`ptr4` e 64 bits em `ptr8`.

| Tipo | Tamanho | Faixa | Papel |
| :--- | :--- | :--- | :--- |
| `int` | **32 bits — todos os alvos** | −2.147.483.648 a 2.147.483.647 | Aritmética geral, contadores, variáveis de loop |
| `uint` | **32 bits — todos os alvos** | 0 a 4.294.967.295 | Contagens sem sinal, máscaras de bits, discriminantes de enum |
| `isize` | Largura de ponteiro (4 ou 8 bytes) | Dependente do alvo | Diferença de ponteiros com sinal, offsets de bytes |
| `usize` | Largura de ponteiro (4 ou 8 bytes) | Dependente do alvo | Comprimentos de arrays, índices de slices, tamanhos de alocação |

`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64` permanecem inalterados.

Motivação da mudança:

- **ABI portável.** `int` que cruza uma fronteira de componente ou FFI agora
  tem o mesmo tamanho em todos os alvos — sem diferença oculta.
- **Literais validados.** Um literal fora da faixa produz T038 em qualquer
  perfil; isso não promete trap para todo overflow aritmético em runtime.
- **Alinhamento com o Wasm Component Model.** O WIT define `s32`/`u32` para
  inteiros genéricos; o antigo `int` de 64 bits em hosts de 64 bits era
  incompatível sem anotação explícita.

## Detalhes Técnicos da Implementação

### Diagnóstico T038

Quando um literal inteiro não cabe no tipo inferido, o compilador emite T038
(nunca trunca nem promove silenciosamente):

```
error[T038]: literal inteiro 9999999999 fora do intervalo de `int` (i32)
  --> src/main.aru:4:18
   |
 4 |   let x: int = 9999999999
   |                ^^^^^^^^^^^ valor excede −2.147.483.648..2.147.483.647
   |
   = ajuda: use `i64` se precisar de um inteiro de 64 bits com sinal:
            let x: i64 = 9999999999
```

### ABI e layout

`int` e `uint` têm o mesmo layout que `i32` e `u32` em todos os alvos:

| Tipo | C equivalente | Cranelift IR | Tamanho |
| :--- | :--- | :--- | :--- |
| `int` | `int32_t` | `I32` | 4 bytes |
| `uint` | `uint32_t` | `I32` | 4 bytes |
| `isize` | `intptr_t` | `I32` / `I64` | 4 ou 8 bytes |
| `usize` | `uintptr_t` | `I32` / `I64` | 4 ou 8 bytes |

### Padrões de migração

**Comprimento de array e índice de slice:**

```arandu
// Antes — assumia uint = usize
let n: uint = arr.len()

// Depois
let n: usize = arr.len()   // sempre largura de ponteiro; correto para len/índice
```

`arr.len()` retorna `usize`. Atribuí-lo a `uint` requer cast explícito e pode
truncar em alocações grandes.

**Literais grandes em contexto `int`:**

```arandu
// Literal fora da faixa de int:
let big: int = 5_000_000_000

// Depois:
let big: i64   = 5_000_000_000   // 64 bits explícito
let ts:  i64   = timestamp_ns    // nanosegundos precisam de i64
let off: isize = byte_offset     // diferença de ponteiro com sinal
```

**Campos de struct que carregam tamanho ou offset:**

```arandu
// Antes (semântica de usize, agora sempre 32 bits):
struct Buf { len: uint; data: ptr[u8] }

// Depois:
struct Buf { len: usize; data: ptr[u8] }   // largura de ponteiro para alocações
```

**Código que já usa `i32`/`u32` ou valores dentro de 32 bits: sem alteração.**

### Verificação do código existente

```bash
grep -rn ':\s*uint\s*=' src/   # uint carregando .len() ou offsets
grep -rn ':\s*int\s*='  src/   # int com potencial overflow
grep -rn '[0-9_]\{10,\}' src/  # literais ≥ 10 dígitos
```

Execute `arandu check` — todos os erros T038 aparecem no primeiro passo.

## PONTOS DE MELHORIA (O que não está no roadmap)

- **Quick fix automático do LSP:** hoje o T038 sugere a correção no texto do
  diagnóstico; um quick fix estruturado (replacement) que troca `int` por
  `i64` automaticamente reduziria o atrito da migração em editores com suporte
  a code actions.
- **Regra lint W-xxx para `uint` recebendo `.len()`:** emitir um aviso
  proativo quando `uint` recebe o retorno de um método conhecido que produz
  `usize`, antes de o usuário encontrar um panic ou truncamento em produção.
- **Migração automatizada:** um `arandu fix --rule=int-to-i32` que reescreve
  as ocorrências mais comuns com evidência de segurança (apenas variáveis locais
  cujo valor foi provado dentro de 32 bits pelo typeck).

## Futuro e Próximos Passos

- O contrato `int = i32` é estável e não muda. Nenhuma flag de compatibilidade
  retroativa será adicionada.
- `isize`/`usize` são os tipos canonicamente corretos para comprimentos,
  índices e offsets. Todos os métodos de coleção da stdlib (`len`, `cap`, etc.)
  continuam retornando `usize`.
- Quick fix estruturado do T038 está classificado como débito de DX
  (ver `arandu-architecture-audit-v0.1.md`) e pode entrar em qualquer versão
  de patch sem afetar o contrato semântico.

---

Referências: [RFC 0023](./rfcs/0023-portable-default-integer-model.md) ·
[ABI/Layout](./arandu-abi-layout-v0.1.md) · [docs/errors/T038.md](./errors/T038.md)
