# RFC 0023: Modelo Portável de Inteiros Padrão e Tipos de Largura do Alvo

- **Número da RFC:** 0023
- **Título:** Modelo Portável de Inteiros Padrão e Tipos de Largura do Alvo
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-24
- **Status:** `Implemented`
- **Área Principal:** `Frontend` / `Middle-end` / `Backend` / `Stdlib`
- **PR da RFC:** N/A (In-Tree RFC)
- **Issue de Acompanhamento:** N/A
- **Documentos Relacionados:**
  - `docs/arandu-abi-layout-v0.1.md`
  - `docs/arandu-backend-contract-v0.1.md`
  - `docs/arandu-compiler-roadmap-v0.1.md` (TYP.3)
  - `docs/rfcs/0012-scientific-computing-and-data-architecture.md`

---

## 1. Resumo (Summary)

Esta RFC encerra a sobreposição entre três conceitos que hoje ocupam o mesmo
par de tipos `int`/`uint`. A decisão é:

1. **Inteiros comuns de largura fixa:** `int = i32` e `uint = u32`, iguais em
   todos os targets. Este é o contrato definitivo da linguagem, não uma
   hipótese condicionada a benchmark futuro.
2. **Inteiros de endereço:** `isize` e `usize` têm a largura do ponteiro do
   target e são usados exclusivamente para comprimento, capacidade, índice de
   memória e diferença de endereço.
3. **Literais inteiros:** permanecem sem largura concreta durante a
   inferência; o contexto escolhe o tipo. Sem contexto, o literal materializa
   `int` (32 bits) e valores fora da faixa produzem `T038`, nunca truncamento
   nem troca silenciosa de tipo pelo valor do literal.

Tipos `i8`…`i64` e `u8`…`u64` continuam representando largura explícita. Esta
RFC não altera a semântica de overflow, shifts nem conversões já definida
pelos RFCs de aritmética.

A escolha é de **contrato semântico**: largura e layout idênticos em todo
target eliminam comportamento dependente de arquitetura e churn de ABI entre
alvos. Medições futuras servem para detectar regressões e orientar uso de
tipos compactos; elas não reabrem a largura escolhida (§8). A quebra de ABI
deve ocorrer de forma versionada antes do congelamento da ABI pública.

---

## 2. Motivação (Motivation)

### 2.1 Estado anterior à implementação (motivação histórica)

Desde julho de 2026, `int`, `uint` e o fallback de literais inteiros usam a
largura do ponteiro: 32 bits em `ptr4` e 64 bits em `ptr8`. A decisão foi
consistente com a intenção de ter inteiros nativos e layouts coerentes com o
target. O roadmap chama essa regra de “`int` nativo (largura de ponteiro)” e
ela era implementada de ponta a ponta antes da migração:

- `LayoutEngine` resolve `Primitive::Int`/`Uint` pelo tamanho do ponteiro;
- `TargetInfo` modela a faixa de `int`/`uint` a partir do `TargetConfig`;
- o fallback de literais sem contexto materializa `Primitive::Int`;
- Cranelift, C e Wasm mapeiam `int`/`uint` para a largura do alvo.

Na prática, o mesmo par `int`/`uint` também é usado para cálculos ordinários,
retorno de `mem.sizeOf`/`alignOf`, comprimentos e componentes de ABI. Isso
causa quatro efeitos concretos:

- **Comportamento dependente do alvo:** um valor local sem anotação pode ser
  aceito em um target e rejeitado em outro, ainda que o cálculo não envolva
  ponteiros. O mesmo programa tem faixa e layout diferentes conforme o alvo.
- **Inconsistência interna já presente:** a geração WIT/WebAssembly do
  compilador já emite `i32`/`u32` fixos para `int`/`uint`, enquanto o layout
  do mesmo tipo em `ptr8` é de 8 bytes. O contrato interno já diverge da
  fronteira de componente.
- **Custo de densidade:** em targets de 64 bits, cada elemento `int`/`uint`
  ocupa 8 bytes. Isso pode aumentar structs e arrays quando os valores cabem
  em 32 bits; o efeito real depende do acesso, do cache, da vetorização e do
  código produzido — é um custo a medir, não um argumento decisivo.
- **Nome vs. contrato:** `int` comunica um inteiro geral, mas parte de seu
  contrato atual é o de `isize`; `uint` também carrega responsabilidades de
  `usize`.

O projeto já tem `DataLayout`, perfis de ponteiro 32/64 bits, validação de
literais por target e três backends. Isso torna possível dar identidade
explícita aos valores dependentes do espaço de endereçamento sem exigir que
todo inteiro comum varie com o target.

O peso dos alvos não é hipotético: RFC 0014 define Wasm32 como backend de
primeira classe; RFC 0017 prioriza `arandu_core` freestanding e microcontroladores
como Cortex-M e RISC-V de poucos KiB de RAM. A especificação WebAssembly tem
`i32` e `i64` como larguras inteiras, e RV32 define registradores e endereço
inteiro de 32 bits. Essas fontes não provam que `i32` seja sempre mais rápido;
elas validam que 32 bits é uma base real do produto, não um palpite derivado
do host do desenvolvedor.

### 2.2 O problema em quatro classes de erro

Toda decisão de largura padrão incorre em algum custo. O critério desta RFC
é minimizar o total de erros reais documentados nas quatro classes abaixo,
para o perfil de alvos declarado pelo Arandu (desktop 64-bit, Wasm32,
Cortex-M/RV32):

| Classe | Descrição |
|---|---|
| (a) Overflow/estreitamento | Valor que não cabe no tipo, silencioso ou não |
| (b) Comportamento dependente do alvo | Mesmo código, resultado/faixa/layout diferentes por target |
| (c) Churn de ABI | Layout de struct e assinatura mudam entre alvos ou versões |
| (d) Atrito de conversão | Casts obrigatórios no código cotidiano |

A seção 7 confronta as três candidatas com essas classes usando evidência de
outras linguagens.

### 2.3 Objetivos

- Manter a inferência contextual de literais, sem exigir casts para casos
  simples como `take_u8(10)`.
- Fazer cálculos ordinários e APIs sem dependência de memória terem a mesma
  faixa e layout em todos os targets.
- Representar tamanhos e offsets sem truncamento em targets de 32 ou 64 bits.
- Manter larguras explícitas para FFI, formatos binários, SIMD e estruturas
  compactas.
- Garantir que apenas `usize`/`isize` dependam do target: o type checker não
  consulta `TargetInfo` para nenhuma outra decisão de tipo.
- Evitar alegações de performance sem benchmark representativo.

### 2.4 Não objetivos

- Alterar overflow, shifts, conversões ou regras de aritmética nesta RFC.
- Tornar cada literal ou inteiro de largura fixa arbitrariamente grande.
- Prometer que uma largura menor é sempre mais rápida.
- Definir uma ABI C universal. A ABI externa continua sendo específica do
  target e da ferramenta de link.
- Adotar fallback de literal por valor (inferir `i64` automaticamente quando
  o literal não cabe em `int`); a alternativa é analisada e rejeitada em §6,
  Opção F.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

Com o modelo adotado:

```arandu
func average(total: int, count: int): int {
    return total / count
}

func read_region(offset: usize, length: usize) { ... }

let answer = 42                 // int, mesma largura em todos os targets
let port: u16 = 8080            // tipo escolhido pelo contexto
let item_count: usize = 100     // largura adequada ao espaço de endereçamento
let sample: i64 = 9_000_000_000 // faixa explicitamente maior que int
```

O contexto continua determinando o tipo de literais sem anotação explícita:

```arandu
func set_byte(value: u8) { ... }
func allocate(bytes: usize) { ... }

set_byte(255)   // literal contextualizado como u8
allocate(4096)  // literal contextualizado como usize
let local = 12  // sem contexto: int (i32)
```

Uma variável já tipada não muda automaticamente para caber em uma chamada.
Conversões entre `int`, `usize` e tipos fixos continuam explícitas, salvo a
coerção de um literal que o type checker provar caber no tipo esperado.

### A regra dos três usos

O programador de Arandu escolhe entre três famílias conforme a semântica do
valor, nunca conforme o target:

| O que o valor é | Tipo | Exemplo |
|---|---|---|
| Conta, índice lógico, domínio geral | `int` / `uint` (32 bits) | `let retries = 3` |
| Faixa maior que 2³¹ do domínio | `i64` / `u64` explícito | timestamps em ms, IDs, acumuladores |
| Tamanho, capacidade, índice/offset de memória | `usize` / `isize` | `s.len`, `mem.sizeOf` |
| Dado de formato, protocolo, FFI | `iN` / `uN` explícito | `u16` em rede, `u32` em binário |

Como `int` é fixo em 32 bits, um literal de domínio grande falha de forma
idêntica em qualquer target e aponta o caminho certo:

```text
error[T038]: integer literal `10000000000000000000` does not fit in `int`
  help: use `i64`, `u64` or another type whose range includes this value
```

Em um target de 32 bits, um `usize` continua limitado à faixa daquele target;
em 64 bits, sua faixa acompanha o espaço de endereçamento. Isso é esperado:
`usize` é o único tipo cujo contrato declara dependência do alvo.

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Modelo de tipos adotado

| Tipo-fonte | Largura | Dependência do target | Uso principal |
|---|---:|---|---|
| `int` | 32 bits, signed | Não | Inteiro geral e fallback de literal |
| `uint` | 32 bits, unsigned | Não | Domínios explicitamente não negativos e operações modulares |
| `i8`…`i64`, `u8`…`u64` | Exata pelo nome | Não | Dados binários, FFI e largura explícita |
| `isize` | largura do ponteiro | Sim | Diferença/offset assinado de memória |
| `usize` | largura do ponteiro | Sim | Comprimento, capacidade, índice e tamanho |

As razões da escolha `int = i32`, em ordem de peso:

1. **Portabilidade (classe b):** largura e layout idênticos em x86_64,
   Wasm32, Cortex-M e RV32. Nenhum programa muda de faixa ou de layout ao
   mudar de alvo. Rust documenta o mesmo raciocínio na RFC 0212: um tipo
   independente de plataforma “resulta em menos diferenças entre as
   plataformas em que o programador não se importa com o tipo inteiro”.
2. **ABI e fronteiras (classe c):** `int` mapeia para `int32_t`/`i32` em
   qualquer ABI-alvo (ILP32, LP64, LLP64, RISC-V, ARM) sem tradução; structs
   que contêm `int` têm o mesmo layout em qualquer alvo e qualquer versão do
   compilador. O alvo de componente WIT já assume 32 bits hoje.
3. **Direção do estreitamento (classe a):** com `int` de 32 bits, a
   conversão `int` → `usize` é *widening* seguro em todos os alvos; a
   conversão perigosa `usize` → `int` só ocorre quando o valor é
   genuinamente grande e, portanto, exige decisão consciente. Com um default
   de 64 bits, toda indexação em alvos de 32 bits cairia na direção
   troncante — exatamente a classe de vulnerabilidade sistematizada por
   Böge et al., CCS 2016 (§7.2).
4. **Alvos reais do produto:** Wasm32 (RFC 0014) e RV32/Cortex-M (RFC 0017)
   têm `i32` como largura natural; a especificação RISC-V afirma que “in both
   RV32 and RV64 C compilers, the C type `int` is 32 bits wide”. Em 32 bits,
   `i64` é legal mas não é tipo legal em alvos estreitos (LLVM: “`i64` is
   usually not legal on 32-bit targets”), exigindo legalização em par de
   registradores.

`i32` é escolha de contrato, não de performance. Não há promessa de que
operações escalares `i32` sejam mais rápidas em CPUs de 64 bits. Cálculos que
precisem de faixa maior declaram `i64`; endereço e comprimentos usam
`usize`/`isize`, não um inteiro generalizado para cobrir qualquer caso.

### 4.2 Inferência de literais

O literal continua sendo `IntLiteral` enquanto estiver sujeito a inferência.
As regras são:

1. Se o contexto fornece um tipo inteiro concreto, atribuir esse tipo quando
   o valor couber; caso contrário, emitir `T038IntegerLiteralOutOfRange`.
2. Se a expressão ainda está em uma variável de tipo não resolvida, preservar
   a variável e propagar restrições conforme TYP.3.
3. Ao finalizar uma variável livre sem restrições, materializar `int` fixo.
4. Validar o limite no tipo final. Nunca truncar o literal para fazê-lo caber.
5. Para valores fora da faixa, exigir tipo explícito (`i64`, `u64`…).

Os valores literais podem ser armazenados no compilador em representação
suficientemente ampla para detectar overflow antes de materializar o tipo. Isso
é estado de compilação, não um tipo inteiro ilimitado de runtime.

**Decisão correlata (Opção F, §6):** o tipo de um literal não depende de seu
valor. `let x = 100` e `let x = 5_000_000_000` não produzem tipos diferentes
por inferência automática: a segunda falha com `T038` e pede `i64` explícito.
Isso mantém a forma do código determinando o tipo, e não o número escrito.

### 4.3 `usize` / `isize` e APIs de memória

As APIs cuja unidade é byte, elemento, capacidade, índice ou offset usam
`usize` ou `isize` quando a semântica exige alcance proporcional ao target.
Em particular:

- `mem.sizeOf<T>()` e `mem.alignOf<T>()` retornam `usize`;
- `str.len`, slices, arrays dinâmicos e capacidades usam `usize`;
- APIs de alocação, buffers, arquivos e coleções usam `usize`;
- offsets de ponteiros e diferenças entre endereços usam `isize`;
- limites do objeto definidos por `DataLayout` usam `usize`.

`usize` não é o tipo padrão de contadores matemáticos. Sua faixa dependente do
target aparece apenas onde o programa manipula tamanho ou endereçamento. A
biblioteca padrão decide individualmente se contadores de domínio (por
exemplo, quantidade de tentativas) usam `int`, `u32` ou `i64`.

**Regra de contenção:** `usize`/`isize` são proibidos em:

- formatos de dados serializados e snapshots persistidos;
- interfaces FFI públicas e declarações `extern` exportadas;
- interfaces de componente/WIT e qualquer fronteira versionada entre
  módulos publicados.

A justificativa é empírica: o próprio Go recusa `int` (pointer-width) em
`encoding/binary` — `binary.Size` retorna `-1` para valores não fixos —,
porque persistir largura dependente de alvo converte uma decisão local em
dado irreversível. No Arandu, violar essa regra reintroduziria a classe (c)
pela porta dos fundos, ao custo de dados corrompidos entre alvos. `usize` pode
entrar e sair de formatos fixos apenas por conversão explícita com validação
de faixa.

### 4.4 Layout e backends

- `LayoutEngine` trata `int`/`uint` como 4 bytes em todos os perfis.
- `usize`/`isize` usam `DataLayout.pointer` em tamanho e alinhamento.
- O C backend emite `int32_t`/`uint32_t` para `int`/`uint`, e os tipos
  inteiros da largura do target para `usize`/`isize`.
- Cranelift e Wasm usam tipos IR com larguras/sinal correspondentes,
  inserindo extensões e truncamentos apenas nos limites tipados já validados.
- A classificação ABI consome os layouts concretos, sem inferir tipo da
  largura do host do compilador.
- `IntLiteral` não sobrevive à materialização: todo literal sem contexto
  vira `int` antes do layout, com faixa validada.
- Layout de structs e assinatura de funções que contenham os tipos alterados
  mudam; este é um contrato de linguagem/ABI versionado.

Não se presume que `int32_t` ou `int64_t` seja mais rápido apenas por
refletir uma largura fixa. A geração de instruções depende do backend e do
target.

### 4.5 Migração da superfície existente

Antes da implementação, inventariar usos semânticos de `int`/`uint` e
classificá-los em: aritmética geral, dimensão/endereço, protocolo/ABI, ou
representação compacta. A migração é:

- comprimentos e medidas de memória → `usize`;
- offsets assinados e diferenças de ponteiro → `isize`;
- inteiros gerais → `int`/`uint` fixos, se a faixa couber;
- campos de tamanho fixo e formatos → `iN`/`uN` explícitos;
- chamadas externas → tipos definidos pelo ABI da declaração/importação.

Pontos específicos já identificados:

- `stdlib/core/mem.aru`: `sizeOf`/`alignOf` de `uint` para `usize`;
- `stdlib/core/num.aru`: `intMax()` é hoje `((-1 as uint) >> 1) as int`,
  ou seja, dependente do target — passa a ser constante fixa de `i32`;
- `stdlib/core/{slice,str,fmt}`, `alloc/*`, `math/*`, `std/{io,fs}`: `len`,
  `capacity` e contagens de memória de `uint` para `usize`;
- runtime `extern "C"` (`vec/fs/os/rt_runtime`): assinaturas devem acompanhar
  os novos tipos, mantendo o casamento byte a byte com a declaração `.aru`;
- testes `target_config` e fixtures que afirmam `sizeOf<int>() == 8` em
  `ptr8` passam a afirmar `4` em qualquer alvo;
- docs de contrato: `arandu-abi-layout-v0.1.md` (tabela de primitivos),
  `arandu-backend-contract-v0.1.md` (perfis e linha de `int`/`uint`) e o
  roadmap (TYP.3.3).

Conversões existentes que hoje são identidade entre `uint` e medidas de
memória podem passar a ser explícitas. A migração oferece diagnósticos
direcionados e, se viável, quick fixes estruturados; não reescreve fontes ou
snapshots em massa sem revisão.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### Invariantes preservados

- O type checker consulta `TargetInfo` somente para `usize`/`isize` e para
  operações genuinamente dependentes do target. Com esta RFC, esse número
  tende a zero para `int`/`uint`, reduzindo a superfície de invalidação por
  mudança de `TargetConfig`.
- `DataLayout` continua sendo a fonte canônica de tamanho/alinhamento.
- C, Cranelift e Wasm recebem a mesma semântica validada pelo type checker.
- Inferência incremental continua por item; o tipo final do item incorpora o
  `TargetConfig` apenas quando realmente depende dele.
- Literais fora do range produzem diagnóstico antes de chegar a AMIR/backend.

### Custos e riscos

- `int`/`uint` deixam de representar diretamente o tamanho máximo de um
  objeto em target de 64 bits. Código que os usa para tamanhos adota `usize`.
- A largura fixa de `int` é uma decisão pública difícil de reverter depois de
  estabilizar ABI e bibliotecas — razão pela qual a mudança é versionada
  antes do congelamento da ABI.
- Operações com `usize` podem exigir conversões em APIs que hoje aceitam `int`.
- `int32` satura em 2³¹−1: domínios como timestamps em milissegundos
  (≈1,7×10¹²), IDs de 64 bits e contadores de eventos exigem `i64`
  explícito desde já. É o mesmo custo documentado em Rust (que resolve com
  `i64` explícito) e em Go (`time.Duration` é `int64` por especificação).
  `T038` e a regra dos três usos são a mitigação.
- A largura escolhida **não** altera a semântica de overflow: Java e Go têm
  `int` de 32/64 bits e ainda assim definem overflow silencioso por
  especificação. Se o Arandu quiser endurecer overflow, isso pertence ao
  RFC de aritmética, não a este.
- A mudança altera layout, assinatura de função, FFI e objetos compilados;
  cache e artefatos antigos não podem ser reutilizados entre contratos.
- Risco residual de vazamento: se `usize` escapar para formatos persistidos
  ou fronteiras publicadas, a classe (c) que esta RFC elimina retorna. A
  regra de §4.3 existe para isso e deve virar verificação em
  `check-architecture`/lint (§9).
- Nenhuma dessas consequências prova, sem medição, um ganho ou perda de
  velocidade de execução.

---

## 6. Racional e Alternativas (Rationale & Alternativas)

### Opção A — manter `int` e `uint` pointer-width (status quo)

**Acertos:** integra com ponteiros e comprimentos; ergonomia familiar a Go e
Swift; não exige família separada de tipos.

**Custos:** implementação das classes (b) e (c) em máxima intensidade —
programas mudam de faixa e layout conforme o target; o nome `int` esconde a
dependência; mistura aritmética geral e endereçamento. Evidência: o `int` de
Go tem 8 bytes em amd64 e 4 em 386 na própria tabela da ABI de Go, com
`int64` mudando de align 8 para 4; o alvo `GOARCH=wasm` do Go tem ponteiros
de 64 bits num mundo de hosts de 32, forçando casts manuais em structs WASI
(issue golang/go#63131, proposta aceita de criar `GOARCH=wasm32` com `int`
de 32 bits); no Zig, código que compila em x86_64 deixa de compilar em
`-target i386-linux` por causa de `usize` (ziglang/zig#10669), e mantenedores
afirmam que isso é by design.

**Veredito:** rejeitada. É a implementação atual e é justamente ela que esta
RFC corrige.

### Opção B — `int` pointer-width e `isize`/`usize` como aliases

Aliases não separam semânticas e mantêm a ambiguidade atual. Para
diferenciação útil, os tipos precisam ser distintos e as APIs precisam
migrar.

**Veredito:** rejeitada por não resolver o problema.

### Opção C — inteiros comuns fixos e `usize`/`isize` separados (**adotada**)

Separa matemática geral de endereçamento, mantém inferência contextual de
literais e dá layout previsível aos tipos comuns. A decisão é `int = i32` e
`uint = u32`: corresponde ao alvo 32-bit explicitamente importante para o
projeto, mantém a mesma faixa em todos os targets e reduz o footprint
padrão de arrays e structs. `i64`/`u64` permanecem explícitos para domínios
que precisem da faixa maior.

É o mesmo split que Rust (`i32` + `usize`), C# (`int` + `nint`/`nuint`),
Java/Kotlin (`int` + APIs long) e Hare (`int` + `size`/`uintptr`) adotaram
independentemente (§7.1). O custo é adicionar dois tipos de endereço e fazer
uma migração ampla (§4.5).

**Veredito:** adotada.

### Opção D — literal sem contexto como inteiro arbitrário de runtime

Remove um limite default, como o `comptime_int` do Zig, mas requer definir
bigint, representação, custo de runtime, interoperabilidade e regras de
inferência tardia. Não é proporcional ao problema atual. A RFC conserva
literais sem largura somente durante compilação; valores runtime permanecem
de largura finita.

**Veredito:** rejeitada como modelo de runtime; a representação ampla de
literais em compilação (§4.2) captura o benefício sem o custo.

### Opção E — `int` sempre `i64` e tamanhos em `usize`

Dá ampla faixa padrão e semântica independente do target, mas:

- **Nenhuma linguagem popular usa `int` fixo de 64 bits.** O único modelo
  documentado com `int` de 64 bits é o ILP64, restrito a máquinas Cray; onde
  64 bits é o default, ele é um tipo explícito (`i64`, `long`, `int64`).
- Em alvos de 32 bits, `i64` não é tipo legal (LLVM) e toda operação de
  índice/tamanho envolveria `int`(64) → `usize`(32), a direção troncante,
  no alvo com menos memória — a classe (a) em sua pior configuração.
- Dobra o footprint de arrays e structs frente a `i32` no alvo de menor
  memória, contradizendo o objetivo de RFC 0017.

`i64` continua disponível onde a faixa é requisito do domínio; não é o
default.

**Veredito:** rejeitada.

### Opção F — fallback de literal por valor (`int` se couber, `i64` senão)

É a regra do Kotlin (“infere `Int` se o valor couber; caso contrário,
`Long`”) e evitaria o `T038` para constantes grandes. Contra:

- o tipo de uma variável passa a depender do valor escrito, não da forma do
  código: `let retries = 3` e `let retries = 3_000_000_000` têm tipos
  diferentes sem que a sintaxe indique isso;
- cria divergência silenciosa de tipo entre trechos vizinhos e complica
  sobreposição de literais em coleções e operadores binários;
- Rust avaliou e manteve o fallback fixo com `T038` equivalente, com a
  medição de que o fallback quase nunca é o que decide o tipo em código
  real (RFC 0212, §“Lack of bugs”).

A ergonomia de `T038` é coberta pela mensagem com `help:` apontando `i64`.

**Veredito:** rejeitada.

### Status quo sem decisão (não implementar)

Mantém as classes (b) e (c) ativas e adia a migração, tornando o custo de
troca maior a cada release. O roadmap já registra TYP.3 e a migração fica
mais cara com a expansão da stdlib.

**Veredito:** rejeitada.

---

## 7. Arte Prévia (Prior Art)

### 7.1 Linguagens: escolhas e lições

| Linguagem | Estratégia | Acerto a aproveitar | Limite a evitar |
|---|---|---|---|
| **Rust** | Literais inferidos pelo contexto; subdeterminados usam `i32` (RFC 0212). `usize`/`isize` separados. | Contexto evita sufixos; tamanhos de memória têm tipos próprios; decisão revisada com rationale escrito. | O default `i32` pode surpreender quem assume word-size, mas é explícito e não varia por target. |
| **C#** | `int` = 32 bits fixo; `nint`/`nuint` pointer-width nomeados. | Modelo híbrido idêntico ao adotado aqui, com o tipo nativo fora do caminho comum. | Requer disciplina para não misturar `nint` em domínios fixos. |
| **Java / Kotlin / Scala** | `int`/`Int` = 32 bits fixo; `long` explícito. Kotlin infere `Long` quando o literal não cabe. | Faixa estável e previsível; `long` é tipo de domínio grande explícito. | Overflow silencioso por especificação (JLS 4.2) — decisão de semântica, não de largura. |
| **Go** | `int` é 32 ou 64 bits; constantes sem tipo têm precisão arbitrária até contextualizarem. | Constantes flexíveis; conversões explícitas obrigatórias. | `int` varia por arquitetura (ABI: 8 bytes em amd64, 4 em 386); `int` é banido de formatos de dados (`encoding/binary`); `GOARCH=wasm` com ponteiros de 64 bits forçou a proposta `wasm32` (golang/go#63131). |
| **Swift** | `Int` pointer-width; `Int32`/`Int64` fixos; conversões explícitas. | Integração ergonômica com índices e APIs de plataforma. | Inteiro comum não é semanticamente estável entre targets; atrito em fronteiras 32/64. |
| **Zig** | `comptime_int` sem largura como literal; runtime exige `iN`/`uN` conhecidos. | Não escolher cedo demais a largura de constante; falhar se a conversão não couber. | `usize` pointer-width quebra portabilidade ao adicionar um alvo 32-bit por design (ziglang/zig#10669); `usize` para tamanhos estourou em alvo ARM 32-bit (ziglang/zig#17596). |
| **Hare** | `int`/`uint` implementation-defined com piso de 32 bits; `size`/`uintptr` separados. | Separação explícita entre inteiro geral e tamanho/ponteiro. | Largura do `int` ainda é impl-defined, o que mantém a classe (b). |
| **Odin** | `int`/`uint` ≥ largura do ponteiro; `uintptr` separado. | Orientação clara de usar `int` por default. | Cria um alvo inteiro dedicado (`wasm64p32`) só para desacoplar `int` de 64 de ponteiros de 32 em Wasm — complexidade que o Arandu evita com `int` fixo. |
| **Carbon** | Sem `int` nu: `iN`/`uN` sempre explícitos; `int` do C mapeia para `i32`. | Confirma que o tipo “natural” de referência para interop é 32 bits. | Exige anotação constante; custo de ergonomia que o Arandu não quer para o caso comum. |
| **Dart** | `int` de 64 bits no papel, mas representação por plataforma. | — | Prova de que escolher 64 não elimina a classe (b): no web, `2⁶³` vira `9223372036854776000` e `-1 >> 0` vira `4294967295`. |
| **C** | Larguras dependem da implementação; `stdint.h` para largura exata; promoções implícitas. | Tipos explícitos para protocolos e ABI. | Não copiar promoções implícitas nem a dependência de data model (ILP32/LP64/LLP64: `long` é 32 bits em Win64 e 64 em Linux). |

### 7.2 Fontes normativas e evidência consultada

- [Rust Reference: literal expressions](https://doc.rust-lang.org/reference/expressions/literal-expr.html)
  — “If the program context under-constrains the type, it defaults to the
  signed 32-bit integer `i32`.”
- [Rust RFC 0212: Restore integer inference fallback](https://github.com/rust-lang/rfcs/blob/master/text/0212-restore-int-fallback.md)
  — única rationale escrita e revisada de uma escolha de default nesta área:
  “there does not exist a compelling reason for having a signed
  pointer-sized integer type as the default”; e, sobre risco do fallback
  32-bit, “there has not been a single bug exposed by removing the fallback
  to the `int` type”.
- [Go Language Specification: numeric types e constants](https://go.dev/ref/spec)
  — `int`/`uint` com “implementation-specific sizes”; `int32` e `int` são
  tipos distintos mesmo quando têm o mesmo tamanho.
- [Go Internal ABI specification](https://go.googlesource.com/go/+/refs/heads/master/src/cmd/compile/abi-internal.md)
  — tabela de layout: `int, uint` 8/8 em 64-bit e 4/4 em 32-bit; `int64`
  muda de align 8 para 4 — churn de ABI entre alvos.
- [encoding/binary](https://pkg.go.dev/encoding/binary) — valores não
  fixos (incluindo `int`) não são serializáveis: precedente direto da
  regra de §4.3.
- [golang/go#63131: create GOARCH=wasm32](https://github.com/golang/go/issues/63131)
  — proposta aceita; descreve os casts manuais (`uint32(uintptr(...))`)
  exigidos porque `GOARCH=wasm` tem ponteiros de 64 bits em mundo de 32.
- [Zig #10669: cast to usize poses a portability hazard](https://github.com/ziglang/zig/issues/10669)
  e [#17596: usize para tamanhos de memória em 32-bit](https://github.com/ziglang/zig/issues/17596)
  — a classe (b) documentada como by design e como estouro real de faixa.
- [Swift `Int`](https://developer.apple.com/documentation/swift/int) —
  `Int` igual a `Int32` ou `Int64` conforme a plataforma.
- [Kotlin: numbers](https://kotlinlang.org/docs/numbers.html) — regra de
  inferência por valor (Opção F) e `Int` = 32 bits.
- [Dart: number representation](https://dart.dev/resources/language/number-representation)
  — divergência nativo/web documentada com tabela de valores.
- [Zig Language Reference: integers](https://ziglang.org/documentation/master/#Integers)
  — `comptime_int` e `usize`/`isize` (Opção D).
- [Hare Language Specification](https://harelang.org/specification.pdf) —
  `int`/`uint` implementation-defined ≥ 32 bits; `size` e `uintptr`
  separados.
- [Odin FAQ: What is the size of `int`?](https://odin-lang.org/docs/faq/) —
  `size_of(int) >= size_of(uintptr)`; alvo `wasm64p32`.
- [Carbon design](https://docs.carbon-lang.dev/docs/design) — sem `int` nu;
  `int` do C mapeia para `i32`.
- [WG14 N1570, C11](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n1570.pdf),
  6.3.1.1 e 7.20 — promoções e `stdint.h`.
- [RISC-V calling convention](https://riscv.org/wp-content/uploads/2024/12/riscv-calling.pdf)
  — “In both RV32 and RV64 C compilers, the C type `int` is 32 bits wide”;
  [tabela de tipos RV32 (ILP32)](https://docs.riscv.org/reference/abi/v1.0/riscv-cc-c-cpp-type-details.html).
- [WebAssembly Core Specification: numeric types](https://webassembly.github.io/spec/core/syntax/types.html)
  — só `i32`/`i64`; endereços são `i32` por default (`memory64` opt-in).
- [LLVM LangRef: integer type](https://llvm.org/docs/LangRef.html#integer-type)
  e TargetLowering — “`i64` is usually not legal on 32-bit targets”.
- RFCs locais do produto: [0014](0014-native-wasm-component-model-and-runtime.md),
  [0017](0017-lean-freestanding-core-architecture.md) e
  [0012](0012-scientific-computing-and-data-architecture.md).

### 7.3 Literatura e documentação de compilador/hardware

- Alexander Böge, Tobias Scharnowski, Nilay Onur, Christian Rossow, John
  Shields, David Starner, “Twice the Bits, Twice the Trouble: Vulnerabilities
  Induced by Migrating to 64-Bit Platforms”, CCS 2016,
  [DOI 10.1145/2976749.2978403](https://doi.org/10.1145/2976749.2978403).
  Primeiro estudo sistemático de como conversões entre data models de 32 e
  64 bits induzem vulnerabilidades reais em software de alta qualidade. É a
  base empírica da classe (a)/direção de estreitamento em §4.1.
- David Brooks e Margaret Martonosi, [“Dynamically Exploiting Narrow Width
  Operands to Improve Processor Power and Performance”](https://doi.org/10.1109/HPCA.1999.744314),
  HPCA 1999 — operações estreitas são exploráveis em energia/desempenho; não
  prova que declarar tipos estreitos em qualquer programa acelera a execução.
- [LLVM IR sobre tipos inteiros](https://llvm.org/docs/LangRef.html#integer-type)
  — tipo-fonte, largura de IR e instrução física são camadas distintas.
- Agner Fog, [Optimizing software in C++](https://www.agner.org/optimize/optimizing_cpp.pdf)
  — localidade de cache com elementos menores é orientação, não garantia.

### 7.4 Síntese: as três candidatas contra as quatro classes

| Classe | `int` = 32 fixo (**adotada**) | `int` = 64 fixo | `int` = pointer-width |
|---|---|---|---|
| (a) Overflow/estreitamento | Risco de saturação em domínios > 2³¹, contido com `i64` explícito e `T038`; direção perigosa só em valores genuinamente grandes | Transfere o risco para alvos 32-bit: toda indexação vira estreitamento (CCS 2016) | Máximo: 31 bits úteis em qualquer alvo 32-bit (Zig #17596) |
| (b) Dependência do alvo | **Zero** — mesmo código, mesma faixa, mesmo layout em todos os alvos | Zero só se todos os alvos forem simétricos em 64 (Dart é o contra-exemplo) | **Máximo por definição** (Go wasm, Zig #10669, Swift) |
| (c) Churn de ABI | **Zero entre alvos** — `int32_t`/`i32` em qualquer ABI-alvo e no WIT | Zero entre alvos, mas `i64` é tipo não-legal em 32-bit (LLVM) | Alto: layouts e aligns mudam por alvo (ABI de Go) |
| (d) Atrito de conversão | Baixo; `int`→`usize` é widening seguro em ambos os alvos; em Wasm32/RV32 `int` e `usize` têm a mesma largura | Baixo em 64-bit, **alto em Wasm32/RV32** (índices: 64→32) | Baixo para memória, alto para domínios 64-bit fixos (timestamps, IDs, formatos) |

Padrão convergente: toda linguagem que evita (b) e (c) separa um tipo geral
fixo de um tipo de memória pointer-width — Rust, C#, Java/Kotlin, Hare. A
exceção é Go, que convive com (b), (c) e contenções adicionais (conversões
obrigatórias, `int` banido de formatos) e ainda precisou propor `GOARCH=wasm32`.

Nenhuma fonte determina universalmente o melhor default. A escolha aqui é de
contrato para o escopo do Arandu: `i32` atende o perfil 32-bit de
Wasm/embedded, mantém semântica estável nos demais targets e evita impor 64
bits a todo valor padrão. `i64` é explícito para domínios que precisem da
faixa (RFC 0012 já exemplifica IDs e colunas `i64`).

---

## 8. Questões em Aberto e Plano de Validação

A largura escolhida (`int = i32`, `uint = u32`) **não é reaberta** por
resultado de benchmark: medições servem para detectar regressões e orientar
recomendações por domínio, não para escolher largura. Depois da decisão
semântica, medir o custo da migração e comparar o novo contrato com o
baseline atual em alvos nativos 64-bit e em compilação 32-bit estrutural. O
conjunto deve incluir:

- arrays de inteiros lidos/escritos sequencialmente e com acesso irregular;
- loops escalares de contagem e redução;
- structs/tuplas com campos inteiros e arrays embutidos;
- workload científico/colunar representativo;
- aritmética próxima de limites, com a mesma semântica de overflow;
- tamanho de código, bytes de memória tocados, throughput e ciclos/instruções
  quando disponíveis.

Os testes devem compilar a mesma fonte em C e Cranelift quando suportado e
validar resultados idênticos. Relatórios devem especificar CPU, compilador C,
flags, target, tamanho dos dados, warmup, repetições e dispersão.
Microbenchmarks isolados não podem ser generalizados para toda a linguagem.

Questões de implementação restantes, que não reabrem a largura escolhida:

1. Definir nomes, conversões e overflow de `usize`/`isize` em alvos futuros
   que não sejam 32/64 bits.
2. Definir versão de linguagem/ABI e ferramenta de migração antes de alterar
   binários ou bibliotecas publicados.
3. Auditar todo uso atual de `int`/`uint` em stdlib, runtime, intrinsics,
   enums, tags, FFI e formatos serializados; `uint` de endereço migra para
   `usize`.
4. Converter a regra de contenção de `usize` (§4.3) em verificação
   automática (`check-architecture` ou lint) e cobrir com teste de
   regressão.

---

## 9. Possibilidades Futuras (Future Possibilities)

- Tipos de bits arbitrários `iN`/`uN` para protocolos e hardware, se houver
  demanda e suporte consistente nos backends.
- Lints para APIs que confundem contagem de domínio com tamanho de memória e
  para `usize` em fronteiras versionadas (materialização da §4.3).
- Newtypes para unidades específicas (bytes, elementos, timestamps), acima de
  `usize`/`isize`, sem mudar as regras de layout primitivas.
- Um benchmark permanente da linguagem para decisões de largura e layout,
  separado do benchmark do próprio compilador.
