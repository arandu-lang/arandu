# RFC 0040: Arquitetura de Formatação Algébrica, Formatter IR (Wadler-Leijen) e Resiliência CST (`arandu_fmt v2`)

- **Número da RFC:** 0040
- **Título:** Arquitetura de Formatação Algébrica, Formatter IR (Wadler-Leijen) e Resiliência CST (`arandu_fmt v2`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-18
- **Status:** `Draft`
- **Área Principal:** `Tooling` / `Frontend` (`arandu_fmt`)
- **Documentos Relacionados:**
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/rfcs/0010-cst-resilient-ide-typeck.md`
  - `docs/rfcs/0022-holistic-synthesis-differential-fuzzing.md`

---

## Fronteira de implementação e evidência

> **Estado auditado em 2026-10-18:** O crate `arandu_fmt` atual (v0.2) opera através
> de scanner léxico de linhas, indentação direta por contagem de chaves `{}`,
> normalização de dois-pontos `:` e cálculo de diffs mínimos para o LSP via
> `SyntaxTree` do Rowan. Esta RFC projeta a evolução para a v2: introdução de uma
> representação intermediária algébrica (`Doc` / Formatter IR), motor de layout
> bounded com medição de colunas (estilo Wadler-Leijen / Biome) e mapeamento
> composicional sobre a CST do Rowan.

---

## 1. Resumo (Summary)

Esta RFC analisa o estado da arte de ferramentas de formatação de código na indústria (Biome, Prettier, gofmt, rustfmt, black, zig fmt, clang-format) e a literatura acadêmica de *Pretty-Printing* (Wadler, Leijen, Hughes, Bernardy), propondo a arquitetura definitiva do formatador oficial do Arandu (**`arandu_fmt v2`**).

A proposta substitui o modelo heurístico baseado em linhas por um **Formatter IR Algébrico** com resolução linear de quebra de colunas (limite canônico de 100 caracteres), preservação estrita de comentários (*trivia attachment*) sobre a CST do Rowan e garantia matemática de **idempotência** ($\text{fmt}(\text{fmt}(x)) \equiv \text{fmt}(x)$).

---

## 2. Motivação (Motivation)

### 2.1 O Problema da Formatação Heurística Baseada em Linhas (v0.2 Atual)
O `arandu_fmt` atual é seguro (nunca destrói código inválido e preserva a posição do cursor no LSP com diffs cirúrgicos), mas possui duas limitações estruturais:
1. **Cegueira de Largura de Coluna (*Line Length Blindness*)**: O formatador não sabe se uma chamada com 6 parâmetros ou uma expressão encadeada estourará o limite visual da tela antes de imprimi-la. Cabe ao desenvolvedor decidir onde quebrar com `Enter`, o que gera inconsistências no repositório.
2. **Incapacidade de Alinhamento Multilinha Declarativo**: Não é possível expressar regras como *"se a lista de argumentos couber em uma linha, imprima `(a, b, c)`; se estourar 100 colunas, quebre em cascata indentada com vírgula final"*.

### 2.2 Análise Comparativa do Mercado: Erros e Acertos de Outras Linguagens

| Ferramenta | Acertos Notáveis | Falhas e Lições para o Arandu |
|---|---|---|
| **`gofmt` (Go)** | Criou o padrão da indústria: formatação única oficial, sem flags de configuração, encerrou debates de estilo. | **Sem limite de colunas**: Permite linhas ilegíveis de 300 caracteres; o programador precisa quebrar linhas manualmente. |
| **`rustfmt` (Rust)** | Consciência sintática profunda de tipos e macros. | **Configurabilidade excessiva** (centenas de flags dividem o ecossistema); problemas crônicos de não-idempotência em versões antigas; dificuldade de lidar com código quebrado durante a digitação. |
| **`black` (Python)** | Filosofia "uncompromising"; regra pragmática das 88 colunas; sem discussão de estilo. | **Lento** (escrito em Python); precisa re-parsear a AST e verificar semântica com `assert_equivalent` a cada execução. |
| **`clang-format` (C++)** | Incrivelmente rápido; enorme suporte industrial. | Baseado em heurísticas dinâmicas com penalidades; comentários frequentemente mudam de lugar ou são corrompidos; centenas de flags conflitantes. |
| **`zig fmt` (Zig)** | Simples, determinístico, rápido. | Depende da presença manual de *trailing comma* do programador para decidir se quebra em múltiplas linhas ou não. |
| **`Prettier` (JS)** | Popularizou a álgebra de Wadler-Leijen para linguagens imperativas; quebra de linha visualmente agradável. | Alocação excessiva em JavaScript; performance limitada em monorepos gigantes. |
| **`Biome.js` (JS/TS)** | **O padrão-ouro moderno**: implementou Wadler-Leijen em Rust sobre CST Rowan; alocação em arena; sub-milissegundo; diffs cirúrgicos. | *(Nenhuma falha arquitetural relevante — é o modelo canônico a ser adotado)*. |

---

## 3. Fundamentação Teórica e Literatura Acadêmica

A arquitetura do `arandu_fmt v2` apoia-se em quatro marcos científicos:

1. **Philip Wadler (1998/2003) — *"A Prettier Printer"***:
   Demonstrou que a formatação pode ser modelada como uma álgebra de combinadores monoidais, onde o operador `Group(d)` representa a escolha não-determinística entre a versão plana (`flat(d)`) e a versão quebrada em múltiplas linhas (`line(d)`). Provou que, se a escolha for feita por um algoritmo guloso limitado ao tamanho da linha ($K$), a complexidade é $O(N)$ linear.
2. **Daan Leijen (2001) — *"PPrint: A pretty-printer library"***:
   Generalizou o algoritmo de Wadler com operadores de indentação relativa (`nest`), preenchimento (`fill`) e separação de quebras suaves (`softline`).
3. **Jean-Philippe Bernardy (2017) — *"A Pretty But Not Greedy Printer"***:
   Mostrou como evitar escolhas gulosas locais que causam quebras de linha esteticamente ruins em expressões compostas aninhadas.
4. **Rowan CST (Rust-Analyzer / Biome)**:
   Arquitetura de nós verdes (*GreenNode* — imutáveis, compartilháveis e sem ponteiros para pais) e nós vermelhos (*RedNode* — com contexto de pai/offset), onde todo caractere de espaço, tabulação e comentário é preservado como **Trivia**.

---

## 4. Explicação em Nível de Guia (Guide-Level Explanation)

### 4.1 O Princípio da Linha Inteligente: 100 Colunas Canônicas
O desenvolvedor não precisa se preocupar com onde quebrar chamadas ou estruturas longas. O formatador decide a melhor apresentação visual:

```arandu
// Se a chamada for curta (cabe em 100 colunas):
let resultado = processarItem(config.origem, 42, "modo_seguro")

// Se passar de 100 colunas, o formatador automaticamente transforma em cascata indentada:
let resultado = processarItem(
    config.origem,
    42,
    "modo_seguro_com_nome_extenso_que_excede_o_limite_de_cem_colunas",
    callbackNotificacao,
)
```

### 4.2 Preservação e Anexação de Comentários (*Trivia*)
Comentários nunca são descartados ou reposicionados de forma ambígua:

```arandu
func calcular(
    // Comentário do parâmetro 'base'
    base: i32,
    /* multiplicador inline */ fator: i32,
): i32 {
    base * fator // Retorno
}
```

---

## 5. Explicação em Nível de Referência (Reference-Level Explanation)

### 5.1 O Formatter IR (`Doc`)
Em vez de manipular strings diretamente, o formatador compila a CST do Rowan em uma árvore de nós `Doc`:

```rust
pub enum Doc<'a> {
    /// Texto literal atômico (tokens, palavras-chave, identificadores).
    Text(&'a str),
    /// Aumenta o nível de indentação por 4 espaços caso o grupo envolvente quebre.
    Indent(Vec<Doc<'a>>),
    /// Tenta imprimir os filhos em uma única linha.
    /// Se a largura total do grupo exceder a coluna alvo (100),
    /// todas as quebras suaves (`SoftLine`) tornam-se quebras reais (`\n`).
    Group(Vec<Doc<'a>>),
    /// Quebra de linha incondicional.
    HardLine,
    /// Espaço ' ' se couber na mesma linha; '\n' se o grupo envolvente quebrar.
    SoftLine,
    /// Nada "" se couber na mesma linha; '\n' se o grupo envolvente quebrar.
    EmptyLineBreak,
    /// Concatenação linear de documentos.
    Concat(Vec<Doc<'a>>),
}
```

### 5.2 Algoritmo do Layout Solver (Wadler-Leijen em $O(N)$)
O renderizador mantém uma fila de documentos com sua indentação atual e a largura restante na linha:

```rust
struct PrintItem<'a> {
    indent: usize,
    mode: PrintMode, // Flat (linha única) ou Break (multilinha)
    doc: &'a Doc<'a>,
}

enum PrintMode {
    Flat,
    Break,
}
```

Para cada `Doc::Group`, o solver chama `fits(doc, remaining_width)`. Como a largura é limitada a 100 colunas, o backtracking verifica no máximo 100 caracteres à frente antes de decidir se o grupo quebra ou fica plano, garantindo tempo de execução linear estrito sem alocação dinâmica descontrolada.

### 5.3 Anexação de Trivia (Rowan GreenNode $\to$ Doc)
Cada nó da CST (`SyntaxNode`) possui tokens filhos e trivias:
1. **Leading Trivia**: Comentários e espaços que precedem o primeiro token do nó. São anexados imediatamente antes do `Doc` do nó.
2. **Trailing Trivia**: Comentários na mesma linha após o último token do nó (ex: `// inline`). São anexados com espaçamento fixo de 1 espaço após o token.
3. **Dangling Trivia**: Comentários dentro de um bloco vazio `{ /* vazio */ }`. O gerador de `Doc` posiciona esses comentários no corpo indentado.

### 5.4 Garantia Matemática de Idempotência
Uma suite de testes com **Property-Based Testing** e integração com o `AranduSmith` ([RFC 0022](0022-holistic-synthesis-differential-fuzzing.md)) validará:

$$\forall \text{ programa válido } P: \quad \text{format}(\text{format}(P)) == \text{format}(P)$$

---

## 6. Invariantes de Arquitetura e Desvantagens

- **Isolamento de Salsa:** O `arandu_fmt` permanece estritamente puro, sem dependências de Salsa, sem I/O e sem dependência do LSP, residindo isolado na raiz do workspace.
- **Resiliência a Erros:** O formatador consome a CST do Rowan. Mesmo que o código tenha erros sintáticos parciais, os nós válidos são formatados e os nós com erro têm sua indentação ajustada sem perda de texto.
- **Custo de Memória:** O uso de referências com lifetime `Doc<'a>` sobre o texto original e buffers reutilizados garante zero-copy durante a criação dos nós do Formatter IR.

---

## 7. Racional e Alternativas

- **Alternativa A: Continuar com o modelo heurístico de linhas (v0.2):**
  - *Rejeitada:* Incapaz de resolver quebras de linha automáticas e padronização de colunas.
- **Alternativa B: Utilizar uma biblioteca externa pronta (ex: pretty.rs):**
  - *Rejeitada:* Dependências externas violam a política do Arandu de manter a base leve e não possuem suporte nativo à CST do Rowan. Um motor de 400 linhas em Rust puro dentro de `arandu_fmt` é mais eficiente e moldado à sintaxe da linguagem.

---

## 8. Questões em Aberto

1. **Trailing Commas Canônicas:** Devemos adotar a regra do Prettier/Biome de sempre inserir vírgula final quando uma lista de argumentos/campos quebrar em múltiplas linhas? *(Recomendação: Sim, pois reduz ruído de diffs no git)*.
2. **Limite Padrão:** Fixar em 100 colunas canônicas (como Biome/Rust) ou permitir flag opcional no `Arandu.toml`? *(Recomendação: 100 colunas fixas sem configuração no v0.2)*.
