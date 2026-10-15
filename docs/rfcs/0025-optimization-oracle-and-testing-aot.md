# RFC 0025: Arandu Optimization Oracle & Testing (`AOT`): Diagnóstico Estruturado, Testes de Regressão de Codegen e Fuzzing Metamórfico de Otimizações

- **Número da RFC:** 0025
- **Título:** Arandu Optimization Oracle & Testing (`AOT`): Diagnóstico Estruturado, Testes de Regressão de Codegen e Fuzzing Metamórfico de Otimizações
- **Autor(es):** Bruno Bispo ([@BrunoF2P](https://github.com/BrunoF2P)) & Comunidade Arandu
- **Data de Início:** 2026-10-04
- **Status:** `Draft`
- **Área Principal:** `Middle-end` / `Backend` / `Tooling`
- **PR da RFC:** [Em criação]
- **Issue de Acompanhamento:** [Em criação]

---

## 1. Resumo (Summary)

Esta RFC propõe a criação de uma infraestrutura nativa e industrial de verificação, auditoria e descoberta de otimizações para a linguagem Arandu, denominada **`AOT` (Arandu Optimization Testing & Oracle)**.

Inspirada no modelo de **Optimization Remarks do LLVM**, nos testes de regressão de MIR/Codegen do Rustc com **FileCheck**, no fuzzing sintético direcionado por políticas do **YARPGen (Intel)** e na teoria de testes metamórficos por **Equivalência Módulo Entradas (EMI)**, o `AOT` transforma a investigação manual de código de máquina (arqueologia com `objdump`) em um subsistema declarativo, automatizado e à prova de regressões dentro do compilador.

O sistema divide-se em quatro pilares fundamentais:
1. **Arandu Optimization Remarks (`-Zremarks=opt`)**: Diagnóstico formal emitido por passes do compilador (inlining, escape analysis, LICM, vetorização, unrolling) detalhando decisões positivas (`OPT-PASSED`), custos e, crucialmente, justificativas exatas para otimizações perdidas (`OPT-MISSED`).
2. **Infraestrutura de Testes de Codegen (`arandu opt-test`)**: Diretivas em comentários (`// OPT-CHECK:`, `// ASM-NOT:`, `// STACK-MAX:`) que permitem aferir contratos de código de máquina e alocações (garantindo `heap_allocations = 0`, eliminação de `memcpy` ou frames de pilha restritos).
3. **Fuzzing Metamórfico Dirigido (`OptFuzz`)**: Síntese de variantes de código semanticamente equivalentes orientadas por políticas estritas (`InlinePolicy`, `EscapePolicy`, `OwnershipPolicy`, `GenRefPolicy`), expondo "anomalias de abstração" onde código equivalente produz saltos desproporcionais de complexidade no backend.
4. **Isolamento e Redução Mínima Automática**: Integração com o redutor estrutural de AST do Arandu para transformar programas sintetizados de centenas de linhas com anomalias de otimização em reproduções mínimas (10-15 linhas) prontas para a suíte de regressão.

---

## 2. Motivação (Motivation)

A auditoria recente sobre softwares reais de grande porte escritos em Arandu (`katu` — busca de texto de alta vazão, e `ita` — hasher criptográfico SHA-256) revelou que, embora o compilador gere código funcional e semanticamente correto, o backend comete **erros silenciosos de otimização de primeira grandeza**:

1. **Ausência de Inlining em Leaf Functions Críticas:**
   * No `ita`, cada rodada de compressão de 64 bytes gerou **16 chamadas completas de função com chaveamento de pilha (`call 24469 <sha256.beU32Buf>`)**, além de chamadas separadas para operadores lógicos triviais (`rotr32`).
   * No `katu`, o comparador de fatias emite um `call` a cada byte percorrido no buffer alvo.
2. **Alocações Ocultas na Heap por Falha na Análise de Escape (Heap Spilling):**
   * Em `katu::findMatches`, o desmonte em assembly revelou **4 chamadas para `ar_rt_raw_malloc` e 3 chamadas para `memcpy`** no prólogo de uma função projetada especificamente para ser *zero-allocation*. Variáveis agregadas e tuplas locais foram promovidas para a heap sem que o desenvolvedor soubesse.
3. **Invariantes não Erguidos (Lack of LICM / Register Hoisting):**
   * Em laços do tipo `while i < hlen`, o comprimento da fatia (`hlen`) é relido da memória da stack a cada iteração (`mov 0x8(%rsp), %rsi`), gerando pressão indevida no pipeline de L1 e impedindo o uso de registradores callee-saved (`%r12-%r15`).
4. **Custos Ocultos de Abstração:**
   * Abstrações idiomáticas da linguagem (como encapsular um tipo primitivo em um `Option<T>`, trocar um parâmetro por valor por um `ref T`, ou desestruturar uma tupla) alteram drasticamente o comportamento das fases de lowering para AMIR, gerando transições anômalas de `Stack -> Heap` ou inibindo vetorização.

Hoje, essas descobertas dependem exclusivamente de **inspeção manual de assembly** (`objdump -d`, `perf`, cálculos empíricos de ciclos). Se um engenheiro conserta o inliner hoje, uma pequena refatoração amanhã pode reintroduzir o problema sem que nenhum teste unitário falhe, pois a semântica permanece idêntica.

A promessa central do Arandu — abstrações ergonômicas de alto nível (ownership, GenRef, efeitos) com custo zero de runtime — exige que **desempenho e codegen sejam testados com a mesma rigidez aplicada à correção funcional**.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1. Diagnosticando Otimizações com Optimization Remarks

Ao compilar um projeto com otimizações habilitadas, o desenvolvedor pode acionar o flag `-Zremarks=opt` (ou `-Zremarks=opt-missed` para focar em problemas):

```bash
arandu build --release -Zremarks=opt
```

A saída do compilador expõe com clareza industrial cada decisão tomada pelos passes de otimização:

```text
remark: pass=inline func=sha256Sigma0 caller=sha256Compress decision=passed cost=12 threshold=35
remark: pass=inline func=beU32Buf caller=sha256Compress decision=missed cost=48 threshold=35 reason="cost exceeds threshold; callsite not in cold path"
remark: pass=escape var=temp_buffer func=findMatches decision=missed-heap reason="reference escapes lexical block via subslice projection"
remark: pass=licm expr="data.len" loop=findMatches:84 decision=missed reason="potential memory alias with mut ref"
```

A pergunta *"Por que diabos essa função minúscula não virou inline?"* deixa de exigir desmontagem de binário: o compilador informa a métrica de custo estimada, o limite configurado e o impedimento formal.

### 3.2. Escrevendo Testes de Regressão de Otimização (`arandu opt-test`)

O desenvolvedor pode criar fixtures de teste com diretivas formais de codegen em `tests/codegen/` ou diretamente na suíte do projeto:

```arandu
// tests/codegen/sha256_leaf_inlining.aru
//
// OPT-CHECK: pass=inline func=beU32Buf caller=sha256Compress status=passed
// OPT-CHECK: heap_allocations = 0
// ASM-NOT: call *beU32Buf*
// ASM-NOT: ar_rt_raw_malloc
// STACK-MAX: 128

module codegen_tests

import std.core.slice as slice

struct State {
    h: [8]u32,
    buf: [64]u8,
}

func beU32Buf(state: ref State, off: usize): u32 {
    let b = state.buf
    return (b[off] as u32 << 24) | (b[off + 1] as u32 << 16) | (b[off + 2] as u32 << 8) | (b[off + 3] as u32)
}

public func compressTest(state: mut ref State): void {
    let w0 = beU32Buf(state, 0)
    state.h[0] = state.h[0] + w0
}
```

Ao executar o runner:
```bash
$ arandu opt-test
[PASS] codegen/sha256_leaf_inlining (0 heap allocs, inlined, stack 64B <= 128B)
[FAIL] codegen/katu_find_matches
       - Error: prohibited symbol emitted in assembly: `ar_rt_raw_malloc`
       - At: src/searcher.aru:82
```

### 3.3. Fuzzing Metamórfico de Otimizações (`arandu opt-fuzz`)

Para caçar ativamente regressões e falhas de lowering antes que atinjam produção, o desenvolvedor ou o pipeline de CI executa o `opt-fuzz`:

```bash
$ arandu opt-fuzz --target escape --duration 5m
[OptFuzz] Policy: EscapePolicy (Memory Models: GenRef, Borrow, Value)
[OptFuzz] Generated 42,100 metamorphic pairs.
[OptFuzz] 🚨 ANOMALY FOUND (Candidate #18942)
  Baseline:  func process(v: Point) -> Stack (0 allocs, 16B frame)
  Variant:   func process(v: Option<Point>) -> Heap (1 malloc, 48B frame, 1 memcpy)
[OptFuzz] Invoking AST Reducer...
  Input: 382 LOC -> 12 LOC
[OptFuzz] Minimal reproducer generated:
  Saved to: tests/regressions/opt/opt_escape_option_aggregate.aru
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

O sistema `AOT` atua na fronteira entre a representação intermediária **AMIR (Arandu Middle IR)**, a máquina de dados do Salsa e o emissor do **Cranelift/C**:

```
                       Código Arandu (.aru)
                                │
                       ┌────────┴────────┐
                       ▼                 ▼
                   Base AST          Mutant AST (EMI)
                       │                 │
                   AMIR (O0)         AMIR (O2)
                       │                 │
              ┌────────┴────────┐        │
              ▼                 ▼        ▼
       Escape Analysis    Inliner Pass  LICM / Vectorizer
              │                 │        │
              └────────┬────────┘────────┘
                       ▼
            Optimization Remarks Log (JSON/Text)
                       │
              ┌────────┴────────┐
              ▼                 ▼
       Backend (Cranelift)   Code-Quality Oracle
              │                 │
        Assembly / ELF    Structural Metrics (Stack/Allocs/Calls)
              │                 │
              └────────┬────────┘
                       ▼
              Anomaly Evaluator
                       │ (Divergência detectada)
                       ▼
              AST Shrinker / Reducer
                       │
            Minimal Bug Report / Fixture
```

### 4.1. Estrutura dos Optimization Remarks

O compilador introduz o subsistema `arandu_middle::remarks`. Todo passe de otimização em AMIR deve reportar seus diagnósticos através de uma trait padronizada:

```rust
pub enum RemarkKind {
    Passed,
    Missed,
    Analysis,
}

pub struct OptimizationRemark {
    pub pass_name: &'static str,
    pub kind: RemarkKind,
    pub span: Span,
    pub symbol: Option<SymbolId>,
    pub message: String,
    pub cost: Option<u32>,
    pub threshold: Option<u32>,
    pub metadata: SmallVec<[(&'static str, MetricValue); 4]>,
}
```

* Os remarks não afetam os hashes incrementais do Salsa (são emitidos como side-channel diagnostics durante passes impuros de codegen/otimização).
* Podem ser exportados no formato binário compacto ou JSON estruturado para consumo por ferramentas de visualização e dashboards de performance de CI.

### 4.2. Diretivas de Asserção do `arandu opt-test`

O runner processa comentários estruturados anexados ao topo ou sobre funções nos arquivos `.aru`:

1. `// OPT-CHECK: pass=<name> [key=value]*`: Avalia se o remark correspondente foi emitido pelo middle-end.
2. `// ASM-CHECK: <regex>` / `// ASM-NOT: <regex>`: Realiza pattern matching pós-montagem no assembly gerado para o target corrente.
3. `// HEAP-ALLOCS: <n>`: Afere o número total de chamadas a `ar_rt_raw_malloc`, `ar_vec_malloc` ou equivalente no grafo de controle da função.
4. `// STACK-MAX: <bytes>`: Mede o deslocamento imediato de `%rsp` no prólogo da função (`sub $N, %rsp`).
5. `// CALL-COUNT: <n>`: Garante que chamadas dinâmicas ou indiretas foram eliminadas.

### 4.3. Motor Metamórfico e Políticas de Geração do `OptFuzz`

O `OptFuzz` estende a infraestrutura introduzida na RFC 0022 (`AranduSmith`), mas substitui a geração puramente sintática por **Políticas Especializadas de Estresse de Otimização**:

#### A. `InlinePolicy`
* Gera famílias de funções com variação combinatória de complexidade ciclomática, contagem de argumentos e profundidade de chamadas.
* Varre variantes: tipos primitivos vs compostos, passagem por valor (`own T`) vs referência (`ref T` e `mut ref T`).
* Compara se envolver uma chamada em um invólucro de camada única (`wrapper(x) -> inner(x)`) preserva inline e se gera chamadas espúrias.

#### B. `EscapePolicy`
* O coração do modelo de memória do Arandu. Foca em rastrear transições de dados através do sistema de referências e tempo de vida.
* Gera variações de fluxo:
  $$\text{Stack Frame} \longrightarrow \text{Projection (Field/Subslice)} \longrightarrow \text{Branch Join} \longrightarrow \text{Return/Escape}$$
* Mutações metamórficas:
  * $T \Longrightarrow \text{struct } S \{ \text{val}: T \}$
  * $T \Longrightarrow \text{Option}<T>$
  * $T \Longrightarrow \text{Result}<T, \text{Error}>$
  * $T \Longrightarrow [1]T$
* **Oráculo de Invariância de Escape:** Se um valor $T$ é comprovadamente local e vive apenas no escopo da função, encapsulá-lo em uma tupla ou struct de campo único **deve obrigatoriamente manter a alocação na stack**. Se a variante resultar em `heap_allocations > 0`, a anomalia é imediatamente reportada.

#### C. `OwnershipPolicy` & `GenRefPolicy`
* Estressa a interação entre empréstimos estáticos seguros e o mecanismo de fallback geracional (`GenRef`).
* Garante que fatias locais emprestadas de vetores ou strings que não sofrem realocação não incorram em checagens de geração redundantes ou retenções no stack frame.

#### D. `LoopInvariantPolicy` (LICM & Hoisting)
* Gera laços com leituras repetidas de propriedades de fatias e structs (`slice.len`, ponteiros base, discriminantes de enum).
* Cria cenários com presença de referências mutáveis concorrentes para testar a precisão da análise de alias (*alias analysis*): o compilador deve provar quando uma mutação não afeta o cabeçalho do slice.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1. Preservação dos 6 Invariantes Arquiteturais do Arandu

1. **Early-Cutoff em Queries Salsa:**
   * O sistema de remarks e métricas de codegen opera **apenas** nas folhas de execução (fase de emissão de código de máquina e linkagem). Ele não polui os nós intermediários de tipagem com dados não-determinísticos de tempo ou hardware.
2. **Ausência de I/O em Queries Puras:**
   * A coleta de estatísticas de otimização em memória é encapsulada em estruturas puras retornadas pelo pipeline de codegen, sem escritas intermediárias em disco durante o ciclo incremental de IDE.
3. **Dominância SSA/OSSA no AMIR:**
   * As diretivas de teste validam a preservação das formas normais de OSSA, especialmente após passes de simplificação de controle de fluxo e eliminação de nós mortos.
4. **Independência de Target (`TargetInfo`):**
   * As regras de `STACK-MAX` e `ASM-CHECK` são estritamente condicionadas ao `TargetInfo` da compilação (x86_64, aarch64, wasm32), evitando testes frágeis entre plataformas distintas.

### 5.2. Desvantagens e Mitigações

* **Fragilidade Potencial de Testes baseados em Assembly:**
  * *Risco:* Um ajuste legítimo de alocação de registradores pode quebrar um teste de regex ingênuo.
  * *Mitigação:* Priorizar asserções de alto nível via **Remarks** (`OPT-CHECK`) e **Métricas Estruturais** (`HEAP-ALLOCS = 0`, `CALL-COUNT = 0`). Restringir regexes de assembly (`ASM-NOT`) para instruções proibidas específicas (`ar_rt_raw_malloc`, `memcpy`, chamadas de pânico).
* **Sobrecarga de Tempo de Compilação com Remarks:**
  * *Risco:* Instrumentar todos os passes de AMIR pode degradar a velocidade do compilador.
  * *Mitigação:* O sistema de remarks permanece inativo (`Zero-Cost Flag`) por padrão; é compilado sob compilação condicional ou ativado estritamente quando `-Zremarks` é passado na linha de comando.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### 6.1. Por que não usar apenas Testes de Benchmark (`perf` / microbenchmarks)?
Microbenchmarks medindo tempo decorrido de CPU sofrem de **ruído estatístico severo** (mudanças de frequência de CPU/turboboost, ruído de SO, concorrência em instâncias de CI em nuvem). Testar se um commit piorou o SHA-256 em 3% através de timing gera falsos positivos constantes. Em contraste, testar que `call beU32Buf == 0` e `heap_allocations == 0` é **100% determinístico e à prova de ruído**.

### 6.2. Por que não adotar LLVM FileCheck como binário externo?
Integrar o FileCheck como uma ferramenta C++ externa criaria uma dependência indesejada no ecossistema de compilação do Arandu. O runner `arandu opt-test` será implementado nativamente em Rust dentro da árvore do compilador, com suporte de primeira classe a diagnósticos formatados e spans de código.

### 6.3. Status Quo
Manter a abordagem manual significa depender de desenvolvedores rodando `objdump` esporadicamente ao perceberem lentidão em suas bibliotecas. Isso inviabiliza a evolução confiável do backend do compilador.

---

## 7. Arte Prévia (Prior Art)

A arquitetura do `AOT` sintetiza décadas de pesquisa de ponta em engenharia de compiladores:

1. **LLVM Optimization Remarks:**
   * Introduzido pelo projeto LLVM para fornecer visibilidade sobre vetorização de loops e inlining. O formato YAML padronizado permite que ferramentas externas (como `opt-viewer.py`) gerem relatórios em HTML sobre oportunidades de otimização perdidas.
2. **Testes de MIR-Opt e Codegen do Rustc:**
   * O compilador Rust (`rustc`) mantém centenas de testes em `tests/mir-opt/` e `tests/codegen/` que compilam trechos curtos de código e verificam a forma canônica da IR ou do assembly final, garantindo que otimizações de abstração zero-cost não regridam entre versões.
3. **YARPGen (Yet Another Random Program Generator) — Intel:**
   * Desenvolvido por V. Livinskii, D. Babokin e Y. Sushko. Encontrou centenas de bugs de otimização em GCC e Clang ao abandonar a aleatoriedade pura em favor de geradores conscientes de padrões que ativam CSE, vetorização e transformações algébricas.
4. **EMI (Equivalence Modulo Inputs) — Z. Su et al. (UC Davis / ETH Zürich):**
   * Metodologia revolucionária de teste metamórfico que muta regiões de código mortas ou computacionalmente invariantes para detectar miscompilations e discrepâncias de otimização sem requerer oráculos manuais.
5. **Alive2 & Souper (Superotimização Formal):**
   * Ferramentas que provam formalmente a correção e a optimalidade de transformações em nível de representação intermediária.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Limiares de Inlining Globais vs Heurísticas Dinâmicas:**
   * Qual deve ser o teto numérico de custo inicial para leaf functions em AMIR antes que a heurística decida pelo não-inlining?
2. **Formato de Exportação dos Remarks em Larga Escala:**
   * Devemos adotar o formato JSON-Lines padrão ou criar um formato binário indexado próprio do Arandu para consumo pelo Language Server Protocol (LSP) no VS Code?
3. **Métricas de Stack por ABI:**
   * Como expressar limites de stack de forma portável entre arquiteturas onde o tamanho de registradores de ponteiro difere (x86_64 vs wasm32)?

---

## 9. Plano de Implementação em Fases (Roadmap)

| Fase | Entregável Principal | Componentes Envolvidos |
| :---: | :--- | :--- |
| **Fase 1** | **Infraestrutura de Remarks** | Implementação do flag `-Zremarks=opt` e instrumentação do Inliner e Escape Analysis em AMIR. |
| **Fase 2** | **Runner `arandu opt-test`** | Parser de diretivas (`// OPT-CHECK`, `// ASM-NOT`) e suíte inicial em `tests/codegen/` cobrindo os casos auditados em `katu` e `ita`. |
| **Fase 3** | **Geradores `OptFuzz` (Inline & Escape)** | Implementação das políticas de estresse sintético focadas nas abstrações de memória e chamadas do Arandu. |
| **Fase 4** | **Metamorphic Testing & Redutor** | Oráculo diferencial de codegen entre variantes equivalentes e integração do redutor para geração de fixtures mínimas. |
| **Fase 5** | **CI & Análise Noturna** | Automação no pipeline de CI com execução determinística em pull requests e fuzzing profundo contínuo. |
