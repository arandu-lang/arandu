# RFC 0018: Emissor C Idiomático, Estruturado e Legível (*Pretty & Idiomatic C Codegen*)

- **Número da RFC:** 0018
- **Título:** Emissor C Idiomático, Estruturado e Legível (*Pretty & Idiomatic C Codegen*)
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-19
- **Status:** `Draft`
- **Área Principal:** `Backend` / `Codegen` (`arandu_backend_c`)
- **Documentos Relacionados:**
  - `docs/arandu-backend-contract-v0.1.md`
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/rfcs/0011-incremental-partitioned-aot-and-in-process-linker.md`
  - `docs/rfcs/0015-native-mobile-architecture-and-zero-copy-interop.md`
  - `docs/rfcs/0017-lean-freestanding-core-architecture.md`

---

## 1. Resumo (Summary)

Esta RFC especifica a evolução do backend C do Arandu (`arandu_backend_c`) de um gerador linear de baixo nível baseado em blocos básicos (`bb0:`, `bb1:`, `goto bb2;`, temporários `t0`, `t1`) para um **emissor C de alto nível, estruturado, idiomático e legível por humanos**.

A proposta estabelece:
1. **Reestruturação de Controle de Fluxo (*Control Flow Structuring*)**: Algoritmo de de-estruturação de CFG baseado em árvores de dominância e *Relooper*, reconstruindo blocos aninhados `if-else`, laços `while`/`for` e despachos `switch-case`, eliminando mais de 95% dos `goto` e rótulos de blocos básicos;
2. **Preservação dos Nomes e Escopos Originais de Variáveis**: Mapeamento do `SymbolTable` para recuperar os identificadores reais digitados pelo desenvolvedor (`contador`, `limite`, `buffer`), declarando variáveis no ponto exato de inicialização e respeitando escopos `{ ... }`;
3. **Coalescência de Expressões SSA (*Expression Inlining*)**: Reconstrução de expressões compostas `int resultado = (a + b) * c;` a partir de temporários de uso único não conflitantes;
4. **Conformidade Industrial com MISRA C**: Geração de código C estrito, livre de construções depreciadas, compatível com a Regra 15.1 do MISRA C (*"The goto statement should not be used"*), permitindo auditoria humana e integração direta em sistemas embarcados, automotivos e aeroespaciais.

---

## 2. Motivação (Motivation)

### 2.1 O Problema do "C como Assembly Disfarçado"

O backend C atual do Arandu consome diretamente a AMIR (*Arandu Mid-level Intermediate Representation*). Como a AMIR é um grafo de controle de fluxo de três endereços (CFG), a implementação original emitiu cada bloco básico como um rótulo literal e cada transição como um salto:

```c
// ESTADO ATUAL: Código gerado estilo máquina
int64_t contagem(int64_t limite) {
    int64_t t0;
    bool t1;
    t0 = limite;
bb0:
    t1 = t0 > 0;
    if (t1) {
        goto bb1;
    } else {
        goto bb2;
    }
bb1:
    t0 = t0 - 1;
    goto bb0;
bb2:
    return t0;
}
```

Embora esse código seja 100% correto do ponto de vista de execução da máquina, ele impõe graves desvantagens estratégicas:

1. **Rejeição em Auditorias e Code Review**: Nenhuma equipe de engenharia de software aceita integrar em sua base legada de C/C++ um arquivo `.c` ilegível, impossível de debugar com `printf` ou revisar em um Pull Request;
2. **Reprovação em Certificações Críticas (MISRA C / ISO 26262)**: Normas de segurança automotiva e médica proíbem saltos incondicionais arbitrários (`goto`), exigindo estruturas de controle aninhadas e determinísticas;
3. **Perda de Otimização dos Compiladores C (GCC/Clang)**: Otimizadores modernos de C contam com estruturas de laço explícitas (`for`, `while`) para aplicar vetorização automática (SIMD com AVX2/NEON), unrolling de laços e análise de dependência de ponteiros (`__restrict`). A representação espaguete com `goto` degrada a capacidade do GCC/Clang de inferir invariantes de laço.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 A Experiência do Desenvolvedor

Ao compilar um projeto Arandu para C através do comando:
```bash
arandu build --backend=c --output=libmotor.c
```
O compilador Arandu gerará código C limpo, com a mesma qualidade e elegância de código escrito manualmente por um engenheiro C sênior.

#### Exemplo 1: Laços e Condicionais

**Código Fonte Arandu:**
```arandu
public func calcularFibonacci(n: int): int {
    if n <= 1 {
        return n
    }
    let mut a = 0
    let mut b = 1
    let mut i = 2
    while i <= n {
        let proximo = a + b
        a = b
        b = proximo
        i = i + 1
    }
    return b
}
```

**Código C Gerado com a RFC 0018:**
```c
int64_t calcularFibonacci(int64_t n) {
    if (n <= 1) {
        return n;
    }
    
    int64_t a = 0;
    int64_t b = 1;
    int64_t i = 2;
    
    while (i <= n) {
        int64_t proximo = a + b;
        a = b;
        b = proximo;
        i = i + 1;
    }
    
    return b;
}
```

#### Exemplo 2: Structs, Enums e Mapeamento de Tipos

**Código Fonte Arandu:**
```arandu
public struct Vetor2D {
    x: float
    y: float
}

public func magnitudeAoQuadrado(v: Vetor2D): float {
    return (v.x * v.x) + (v.y * v.y)
}
```

**Código C Gerado:**
```c
typedef struct Vetor2D {
    double x;
    double y;
} Vetor2D;

double magnitudeAoQuadrado(Vetor2D v) {
    return (v.x * v.x) + (v.y * v.y);
}
```

### 3.2 Opções de Emissão no CLI

O compilador Arandu oferecerá opções no comando de build:
* `--c-style=pretty` (**Padrão**): Emissão estruturada, com `if`/`while`/`for`, variáveis com nomes reais e expressões inlinadas;
* `--c-style=raw`: Emissão linear plana com blocos básicos (`bb0`, `goto`), útil para depuração interna de passes da AMIR e compiladores legados que preferem fluxo desconstruído.

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Arquitetura do Pipeline do Emissor C

A transformação de AMIR em C Estruturado ocorre em três passes dedicados dentro de `crates/arandu_backend_c`:

```text
       AmirFunc (CFG com BasicBlocks e Temporários SSA)
                              │
                              ▼
        Passo 1: SSA Expression Inlining & Coalescing
        (Une temporários de uso único em expressões compostas)
                              │
                              ▼
        Passo 2: Control Flow Structuring (Relooper / Dominance)
        (Converte o grafo de BasicBlocks em uma árvore de StructuredAST)
                              │
                              ▼
        Passo 3: Variable Scope & Name Resolution
        (Mapeia SymbolTable para nomes reais e define pontos de declaração)
                              │
                              ▼
                  Emissão de C Idiomático (.c e .h)
```

### 4.2 Passo 1: Algoritmo de Coalescência de Expressões SSA

Na AMIR, instruções complexas são decompostas em atribuições atômicas:
$$\_1 = a \times b$$
$$\_2 = \_1 + c$$

O passador inspeciona cada atribuição `AmirStmt::Assign(lhs, rhs)`:
1. Calcula a contagem de usos do temporário `lhs`;
2. Se `lhs` possui **exatamente 1 uso** (`use_count == 1`);
3. E o ponto de uso reside no mesmo bloco básico;
4. E não existem operações com efeito colateral intervenientes entre a definição e o uso;
5. **Ação:** O rvalue de `lhs` é propagado diretamente para o operando do consumidor, eliminando a criação de uma variável intermediária.

### 4.3 Passo 2: Algoritmo de Reestruturação de CFG (*Relooper*)

Para converter o grafo arbitrário de blocos básicos em código C estruturado, o Arandu adota uma variação adaptada do algoritmo **Relooper** (Zakai, 2011) combinada com **Árvores de Dominância** (Hecht & Ullman, 1972):

Definimos uma AST intermediária estruturada para o emissor C:
```rust
pub enum StructuredStmt {
    Expr(String),
    Block(Vec<StructuredStmt>),
    If {
        cond: String,
        then_branch: Box<StructuredStmt>,
        else_branch: Option<Box<StructuredStmt>>,
    },
    While {
        cond: String,
        body: Box<StructuredStmt>,
    },
    DoWhile {
        body: Box<StructuredStmt>,
        cond: String,
    },
    Switch {
        discriminant: String,
        cases: Vec<(i64, StructuredStmt)>,
        default_case: Option<Box<StructuredStmt>>,
    },
    Break,
    Continue,
    Return(Option<String>),
    Goto(String), // Fallback restrito para grafos verdadeiramente irredutíveis
}
```

#### Regras de Reconhecimento de Padrões:
1. **Padrão If-Then / If-Then-Else**:
   - Um bloco $B$ termina com `AmirTerminator::Branch(cond, B_{\text{then}}, B_{\text{else}})`.
   - Se $B$ domina estritamente $B_{\text{then}}$ e $B_{\text{else}}$, e ambos convergem para um pós-dominador comum $B_{\text{merge}}$:
   - Emite: `StructuredStmt::If`.
2. **Padrão While (Laço com Pré-Condição)**:
   - Uma aresta de retorno (*back-edge*) de $B_{\text{tail}}$ para $B_{\text{head}}$ onde $B_{\text{head}}$ domina $B_{\text{tail}}$.
   - O terminador de $B_{\text{head}}$ testa a condição do laço: bifurca para o corpo do laço ou para a saída $B_{\text{exit}}$.
   - Emite: `StructuredStmt::While`.
3. **Padrão Switch-Case**:
   - O bloco termina com `AmirTerminator::SwitchInt`.
   - Emite: `StructuredStmt::Switch`.
4. **Grafos Irredutíveis (Fallback Conservador)**:
   - Em casos raros onde o fluxo de controle não puder ser reduzido sem duplicação excessiva de código (grafos irredutíveis criados por saltos cruzados), o emissor emite um `goto` rotulado restrito apenas para o salto específico, preservando o restante da função como blocos estruturados.

### 4.4 Passo 3: Preservação de Identificadores e Declaração no Ponto de Uso

No C clássico (C89), todas as variáveis precisavam ser declaradas no topo da função. No C moderno (C99/C11):
1. **Declaração Localizada**: As variáveis são declaradas no primeiro ponto em que recebem valor (`int64_t x = ...;`), reduzindo seu escopo para o bloco `{ ... }` correspondente;
2. **Mapeamento de Nomes**:
   - Para cada `LocalId` na AMIR, consulta-se o `SymbolId` de origem no `SymbolTable`;
   - Se o símbolo possui um identificador textual válido (`"limite"`), este é preservado;
   - Se houver colisão de nomes por sombreamento em escopos irmãos ou transformações SSA, anexa-se um sufixo numérico previsível (`limite_1`, `limite_2`);
   - Temporários residuais que não possuem nome no código fonte são prefixados de forma legível por tipo (`_temp_int`, `_cond_flag`).

### 4.5 Geração de Headers `.h` Canônicos

Ao compilar com `--emit=c`, o compilador gera em conjunto o arquivo de cabeçalho `.h` correspondente:
* Inclui guardas de cabeçalho `#ifndef NOME_H / #define NOME_H`;
* Inclui proteção para C++ `extern "C" { ... }`;
* Converte doc-comments de funções e structs do Arandu em comentários no formato Doxygen / Javadoc (`/** ... */`);
* Usa estritamente inteiros de tamanho fixo padrão de `<stdint.h>` (`int8_t`, `uint32_t`, `int64_t`).

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Preservação dos Invariantes de Arquitetura

1. **Paridade Semântica Estrita**: O código C estruturado deve ter comportamento de execução 100% idêntico ao código emitido pelo backend Cranelift e ao modo `raw`;
2. **Determinismo Byte a Byte**: A ordem de emissão de variáveis, blocos e expressões é puramente determinística. Nenhuma iteração sobre tabelas de hash não ordenadas pode alterar a saída;
3. **Sem Efeitos Colaterais em Queries Salsa**: O processo de reestruturação consome a AMIR e a `SymbolTable` de forma pura, instrumentado com `#[tracing::instrument]`.

### 5.2 Desvantagens e Custos de Complexidade

* **Custo em Tempo de Compilação no Backend C**: O cálculo da árvore de dominância e reestruturação do CFG adiciona uma etapa de análise ao backend C. No entanto, como o backend C é tipicamente usado para builds finais de release e empacotamento, esse pequeno custo em milissegundos é amplamente compensado pelo ganho de legibilidade e otimização downstream pelo GCC/Clang.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### Alternativa 1: Manter a Emissão Baseada em `goto` como Padrão
* **Por que foi rejeitada:** Torna o Arandu inviável para uso sério em indústrias que exigem auditoria de código, inviabiliza certificação MISRA C e afasta desenvolvedores C/C++ que procuram uma linguagem moderna capaz de gerar C limpo e adotável.

### Alternativa 2: Emitir C Diretamente da AST (Pulando a AMIR)
* **Por que foi rejeitada:** Quebraria o princípio fundamental de arquitetura do Arandu. Se o C fosse gerado da AST, ele não se beneficiaria do Typeck completo, das regras de OSSA, do borrow checker e das otimizações de nível médio (SimplifyCFG, DCE). A abordagem de reestruturar a partir da AMIR garante que todo o rigor semântico do Arandu seja preservado.

---

## 7. Arte Prévia e Literatura Científica (Prior Art)

1. **Zakai, A. (2011).** *"Emscripten: an LLVM-to-JavaScript compiler."* In *Proceedings of the ACM international conference companion on Object oriented programming systems languages and applications companion*.
   - Criador do algoritmo Relooper, que provou pela primeira vez a viabilidade de reconstruir código estruturado de alta performance a partir de CFGs planas do LLVM.
2. **Schwartz, E. J., Lee, J., Woo, M., & Brumley, D. (2013).** *"Native x86 Decompilation using Semantics-Preserving Structural Analysis and Iterative Control-Flow Structuring (Phoenix)."* In *USENIX Security 13*.
   - Referência clássica em análise de fluxo e estruturação de laços a partir de grafos de blocos básicos.
3. **Compilador Nim (`nim c`)**:
   - O Nim é amplamente elogiado na indústria por compilar para C legível e idiomático, permitindo que bibliotecas escritas em Nim sejam integradas em projetos C sem atrito.
4. **MISRA C:2012 / Guidelines for the use of the C language in critical systems**:
   - A diretriz mais rigorosa do mundo para código C em sistemas críticos, com exigências formais de eliminação de `goto` e controle de fluxo estruturado.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Estratégia de Quebra de Expressões Muito Longas**: Para expressões com dezenas de operadores combinados, estabelecer um limite heurístico de profundidade de árvore para reinserir temporários locais (`int temp = ...;`), evitando linhas C excessivamente longas que prejudiquem a leitura.
2. **Formatação Automática com Clang-Format**: Avaliar a integração nativa ou opcional de um formatador de estilo (estilo LLVM ou GNU) diretamente no pipeline de emissão.

---

## 9. Possibilidades Futuras (Future Possibilities)

1. **Exportação de Bibliotecas C com um Clique (`arandu build --export-c-lib`)**: Geração automática de arquivo `.c` e `.h` empacotados para inclusão direta em projetos CMake, Meson ou Makefiles.
2. **Relatório de Conformidade MISRA Automatizado**: Ferramenta de verificação integrada no `xtask` que analisa o código C gerado pelo Arandu com `cppcheck` ou `flawfinder` provando conformidade estrita com MISRA C.
