# RFC 0002: Convenção Canônica de Anotações (@PascalCase)

- **Número da RFC:** 0002
- **Título:** Convenção Canônica de Anotações (@PascalCase) e Encerramento de Aliases Legados
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-11
- **Status:** `Implemented` (Com migração de aliases legados concluída)
- **Área Principal:** `Frontend` / `Sintaxe`
- **Substitui:** Contrato preliminar de anotações em minúsculas

---

## 1. Resumo (Summary)

Esta RFC estabelece a convenção oficial e imutável para anotações públicas na linguagem Arandu: toda anotação declarada pelo usuário ou fornecida pelo compilador utiliza estritamente a grafia **`@PascalCase`** (ex: `@NoFallback`, `@Test`, `@Benchmark`, `@Link`, `@Suppress`, `@Destructor`, `@Effects`).

A revisão de contrato da 0.1.10 delimita essa convenção às anotações
declarativas. Operações em expressões preferem APIs de módulo, como
`mem.sizeOf<T>()` e `mem.alignOf<T>()`. As expressões anteriores `@sizeOf(Type)`
e `@alignOf(Type)` permanecem compatíveis; não são anotações declarativas nem
ganham aliases PascalCase. Consulte a política e os limites no
[contrato de nomes](../arandu-attribute-naming-v0.1.md#operações-em-expressões).

Fica formalizado o encerramento definitivo da tolerância a aliases legados em `snake_case` (`@no_fallback`, `@nosuspend`, `@specialize`, etc.), consolidando diagnósticos estruturados com sugestões imediatas de substituição.

---

## 2. Motivação (Motivation)

Uma anotação no Arandu é um metadado semântico de primeira classe reconhecido pelo compilador (orientando o type checker, gerador de código, executor de testes ou sistema de efeitos), e não uma chamada ordinária de função em tempo de execução.

Permitir grafias ambíguas como `@no_fallback` e `@NoFallback` simultaneamente no código-fonte introduz fragmentação de estilo, dificulta ferramentas de busca e formatação (`arandu fmt`) e enfraquece a clareza estética da linguagem. O padrão `PascalCase` torna visível a natureza declarativa e nominal da anotação.

---

## 3. Matriz de Nomenclatura por Camada

Cada camada do ecossistema segue a convenção natural e idiomática de seu domínio:

| Superfície | Convenção | Exemplo Canônico |
| :--- | :--- | :--- |
| **Anotação declarativa Arandu** | `@PascalCase` | `@NoFallback`, `@Test`, `@Effects` |
| **AST / HIR e Rust Interno** | `snake_case` | `no_fallback`, `effects` |
| **Linha de Comando (CLI)** | `--kebab-case` | `--no-generational-fallback` |
| **Manifesto (`Arandu.toml`)** | `snake_case` | `no_fallback = true` |
| **Identificador de Lints (Strings)** | `snake_case` | `@Suppress("unused_variable")` |

---

## 4. Registro Canônico de Anotações

| Anotação | Alvo Válido | Argumentos | Propósito |
| :--- | :--- | :--- | :--- |
| `@Test` | Função livre | Nenhum | Declara um teste unitário descoberto pelo comando `arandu test`. |
| `@Benchmark` | Função livre | Nenhum | Declara um micro-benchmark executado por `arandu bench`. |
| `@NoFallback` | Função | Nenhum | Proíbe alocação geracional em arena; falhas de empréstimo viram erros imediatos. |
| `@Destructor` | Método consumidor | Nenhum | Declara a rotina consumidora de limpeza (`own self`) para tipos não triviais. |
| `@Link` | Declaração externa | 1 String | Vincula uma biblioteca de sistema externa via linker. |
| `@Effects` | Função | Lista de Efeitos | Declara estaticamente as capacidades e efeitos da função (`A2`). |
| `@Suppress` | Escopo léxico | 1 String | Suprime a emissão de um aviso ou diagnóstico de lint. |
| `@Deny` | Escopo léxico | 1 String | Promove um lint específico a erro impeditivo de compilação. |
| `@Forbid` | Escopo léxico | 1 String | Proíbe escopos internos de suprimirem o lint especificado. |

---

## 5. Invariantes de Compilação e Recuperação de Erros

1. **Parser Agnóstico**: O lexer produz o token `@` seguido do identificador. O parser registra o texto original na árvore CST/AST sem validar semântica.
2. **Semântica Autoritária**: O crate `arandu_semantics` é o único dono do catálogo de anotações válidas, seus alvos e regras de argumentos.
3. **Erros sem Pânico**: O uso de uma anotação desconhecida, em alvo incorreto ou com argumentos incompatíveis emite diagnósticos tipados (`N012`, `N013`, `N014`, `N015`) com spans exatos e recupera a árvore, jamais disparando `panic!` no compilador.
