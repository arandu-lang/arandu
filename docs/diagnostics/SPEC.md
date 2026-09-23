---
version: 0.0.1
last_revised: 2026-06
compiler_version: arandu 0.1.7
---

# Especificação e Catálogo de Diagnósticos de Erro do Arandu

Este documento estabelece a especificação de design, as regras de visualização, a arquitetura de registro e o catálogo de todos os códigos de diagnóstico do compilador **Arandu**. 

O sistema de diagnósticos do Arandu é projetado para ser o mais informativo, claro e robusto da indústria de compiladores, combinando os acertos das principais linguagens modernas e superando suas limitações.

---

## 1. Análise Comparativa de Compiladores Existentes

Para criar o melhor padrão de diagnósticos, analisamos os acertos e erros dos sistemas existentes na pasta `analise-erros`:

### 1.1 Rustc (`rustc_error_codes`)
*   **Acertos (Hits):**
    *   Uso de códigos únicos estruturados (ex: `E0382`) que facilitam a busca na internet e fóruns.
    *   Exibições gráficas no terminal com sublinhados coloridos apontando exatamente para os spans de código errados.
    *   Documentação rica em arquivos Markdown individuais com exemplos errados, corretos e explicações.
*   **Erros/Limitações (Misses):**
    *   Os códigos numéricos planos (ex: `E0308`) não dão nenhuma pista sobre qual fase do compilador falhou (Lexer, Parser, Resolutor de Nomes, Verificador de Tipos, etc.).
    *   Historicamente, as explicações longas dos erros ficavam desacopladas da definição do erro no código fonte, gerando riscos de dessincronização (corrigido recentemente com a introdução da macro `Diagnostic` derive).

### 1.2 GCC (`gcc-diagnostics`)
*   **Acertos (Hits):**
    *   Introdução de tipos especializados como `sorry, unimplemented:` para indicar claramente recursos válidos na especificação mas ainda não desenvolvidos.
    *   Modularização clara de renderizadores de saída (sinks de texto, HTML, SARIF).
    *   Suporte a caminhos de eventos (`path` diagnostics) para depurar bugs complexos de fluxo de controle, cruciais para análise de tempo de vida e ownership.
*   **Erros/Limitações (Misses):**
    *   Output linear tradicional poluído e difícil de processar sem ferramentas extras.
    *   Lógica interna complexa acoplada a estruturas C++.

### 1.3 Clang / LLVM (`DiagnosticSemaKinds.td`)
*   **Acertos (Hits):**
    *   Definição unificada via arquivos TableGen (`.td`), gerando os enums C++ e as mensagens de erro em tempo de compilação de forma centralizada.
    *   Uso de formatadores condicionais avançados (como `%select{a|b|c}0`) que permitem parametrizar a mensagem sem duplicar códigos de erro.
*   **Erros/Limitações (Misses):**
    *   A sintaxe do TableGen é extremamente densa e difícil de estender por contribuidores novatos.
    *   Não possui um banco de dados integrado de documentação/tutoriais acessíveis via CLI (como o `--explain` do Rustc).

### 1.4 TypeScript (`diagnosticMessages.json`)
*   **Acertos (Hits):**
    *   Dicionário JSON limpo e estruturado.
    *   Facilidade para internacionalização (i18n).
*   **Erros/Limitações (Misses):**
    *   Códigos numéricos puramente planos, sem contextualização visual e sem explicações detalhadas embutidas.

---

## 2. Abordagem e Design Superior do Arandu

O Arandu resolve esses problemas e melhora a experiência através de quatro pilares:

### 2.1 Códigos de Erro Semânticos e Categorizados
Para garantir máxima clareza e indicar exatamente onde e o que falhou, dividimos os códigos nos seguintes prefixos estruturados:
*   **`LX` (Análise Léxica):** Erros no fluxo de caracteres e formação de tokens (ex: `LX001`).
*   **`P` (Parser / Sintaxe):** Erros na estrutura gramatical e construção da AST (ex: `P001`).
*   **`M` (Módulos & Imports):** Resolução de dependências, caminhos de importação e namespaces (ex: `M001`).
*   **`N` (Name Resolution / Escopo):** Vinculação de identificadores a tipos e valores no grafo de escopos (ex: `N001`).
*   **`T` (Type Checker):** Verificação do sistema de tipos estático (ex: `T018`).
*   **`O` (Ownership & Memory):** Análise de move checker, tempo de vida de referências e borrow checking (ex: `O001`).
*   **`G` (Generics):** Restrições de interfaces e monomorfização recursiva (ex: `G001`).
*   **`W` (Warnings & Linting):** Avisos de boas práticas de estilo, código morto e lints de otimização (ex: `W001`).
*   **`U` (Unimplemented):** Recursos planejados pela linguagem mas ainda não implementados pelo compilador (similar ao `sorry, unimplemented` do GCC) (ex: `U001`).
*   **`L` (Lowering):** Tradução/lowerings de HIR/AMIR causados por caminhos do usuário (ex: `L001`).
*   **`ICE` (Internal Compiler Error):** Bugs de pânico do compilador, subcategorizados por componente para guiar os contribuidores (ex: `ICE-T-001`).

> [!NOTE]
> **Decisão de Design sobre o Formato de ICEs**: O formato de erro interno `ICE-[FASE]-[NUM]` (ex: `ICE-LX-001`) utiliza hífens intencionalmente para diferenciar de forma imediata erros de compilação de código do usuário de erros internos do compilador. Isso permite que parsers automáticos, LSPs e editores tratem pânicos de compilador de forma distinta, facilitando o auto-reporte de bugs de infraestrutura.

### 2.2 Níveis de Severidade e Supressão Estilo Java/Kotlin
O Arandu define formalmente quatro níveis de severidade para os diagnósticos de código do usuário:
1.  **Error (Erro):** Erro fatal de compilação. Interrompe a geração de código.
2.  **Warning (Aviso):** Código suspeito ou má prática (ex: `W001` - unused variable). Permite a compilação.
3.  **Note (Nota):** Informação contextual extra, geralmente vinculada a um erro ou aviso principal (ex: apontar para a definição original do struct).
4.  **Hint (Sugestão):** Dica acionável e inteligente, como correções ortográficas calculadas via algoritmo de Levenshtein ou correções sugeridas exibidas como um diff de código.

> [!IMPORTANT]
> **Comportamento de `-Werror`**: Todos os diagnósticos de severidade `Warning` podem ser promovidos a `Error` via flag `-Werror` (ou configuração equivalente em nível de projeto). As severidades `Note` e `Hint` nunca são promovíveis.

#### Diretivas de Controle Estilo Java/Kotlin:
As configurações de severidade de lints locais seguem anotações no estilo Java/Kotlin, aplicáveis a funções, structs, módulos ou blocos de código:
*   `@Suppress("nome_lint")` / `@Suppress("warnings")`: Ignora avisos específicos ou todos os warnings naquele escopo.
*   `@Deny("nome_lint")`: Promove um aviso específico para `Error` fatal naquele escopo.
*   `@Forbid("nome_lint")`: Proíbe qualquer sub-escopo de anular a restrição (impede `@Suppress` aninhado).

```arandu
@Suppress("shadowing")
func exemplo() {
    x = 10
    {
        x = 20 // Silencia o warning W004 (shadowing)
    }
}
```

### 2.3 Estrutura Programática Interna (Diagnostic Model)
Para garantir que renderizadores de terminal, exportadores de arquivo JSON/SARIF e servidores LSP consumam a mesma fonte de verdade, o compilador Arandu utiliza a seguinte estrutura interna tipada no crate `arandu_diagnostics`:

```rust
pub struct Diagnostic {
    pub code: String,                // Ex: "T018", "ICE-T-001"
    pub severity: Severity,          // Error, Warning, Note, Hint
    pub kind: DiagnosticKind,        // User, InternalCompilerError
    pub message: String,             // Mensagem textual de cabeçalho
    pub labels: Vec<Label>,          // Spans de código destacados com texto
    pub notes: Vec<String>,          // Notas adicionais de contexto
    pub hints: Vec<Hint>,            // Sugestões inteligentes de correção
}

pub enum Severity {
    Error,
    Warning,
    Note,
    Hint,
}

pub enum DiagnosticKind {
    User,
    InternalCompilerError,
}

pub struct Label {
    pub span: arandu_lexer::Span,
    pub message: String,
}

pub struct Hint {
    pub message: String,
    pub replacement: Option<CodeReplacement>, // Dica de autocorreção rápida (Quick-fix)
}
```

> [!NOTE]
> **ICE (Internal Compiler Error)**: Um ICE é classificado internamente como `DiagnosticKind::InternalCompilerError`, mas herda logicamente a severidade `Severity::Error` de forma a abortar imediatamente a execução, além de imprimir informações detalhadas de diagnóstico de infraestrutura.

### 2.4 Registro Verificado Automaticamente via Build Script (`build.rs`)
Para garantir escalabilidade e impedir que a documentação longa dos erros fique fora de sincronia ou que braços manuais gigantescos poluam a crate `arandu_diagnostics`, adotamos uma arquitetura automatizada baseada em `build.rs`:

```text
crates/arandu_diagnostics/
  ├── build.rs             ← Escaneia docs/errors/ e gera registry_gen.rs em build-time
  └── src/
      ├── lib.rs
      └── registry.rs      ← Inclui o arquivo gerado contendo o mapeamento de explicações
```

#### Mecanismo de Validação e Geração:
1.  Durante a etapa de build, o `build.rs` lê o diretório `docs/errors/` e extrai todos os códigos documentados (arquivos no formato `[CÓDIGO].md`).
2.  O script valida a **bijetividade (sincronia 1:1)** entre as variantes do enum `DiagCode` e os arquivos `.md` existentes. Se houver algum código declarado sem sua respectiva documentação em português (ou vice-versa), o build **falha imediatamente**, exibindo uma mensagem limpa:
    ```text
    error: missing documentation file docs/errors/T018.md for declared diagnostic code T018
    ```
3.  Se a validação passar, o script gera automaticamente o arquivo `registry_gen.rs` na pasta `OUT_DIR` com o seguinte padrão:
    ```rust
    // Código gerado automaticamente pelo build.rs - Não edite manualmente
    pub fn get_explanation(code: &str) -> Option<&'static str> {
        match code {
            "T018" => Some(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/errors/T018.md"))),
            "O001" => Some(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/errors/O001.md"))),
            _ => None,
        }
    }
    ```

### 2.5 Extensões e Melhorias Futuras Planejadas
*   **Recovery Diagnostics (Autocorreção)**: Sugestões acionáveis estruturadas no tipo `Hint::replacement` contendo as coordenadas exatas do código a ser alterado, permitindo que LSPs em editores apliquem quick-fixes automáticos.
*   **Sinks SARIF / JSON**: Exportação estruturada para integração contínua (CI) e integração direta com o VS Code.
*   **Ciclo de Vida de Códigos**: Introdução formal de atributos no catálogo para controlar a obsolescência de erros (`introduced_in`, `deprecated_in`, `removed_in`), impedindo que modificações quebrem parsers de logs legados.

---

## 3. Catálogo e Índice de Diagnósticos

Abaixo estão listados todos os diagnósticos mapeados para o compilador Arandu. As mensagens de erro principais estão especificadas em **Inglês**, enquanto a descrição, severidade padrão e versão de introdução estão documentadas em **Português**.

### 3.1 Categoria LX: Análise Léxica (`LX000` - `LX099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **LX001** | `unterminated string literal` | Error | `0.1.0` | Uma string literal foi aberta com aspas mas o arquivo terminou ou a linha quebrou antes de ser fechada. |
| **LX002** | `invalid Unicode character: '{char}'` | Error | `0.1.0` | O arquivo contém caracteres inválidos fora da especificação Unicode aceita para identificadores ou operadores. |
| **LX003** | `invalid number literal: '{literal}'` | Error | `0.1.0` | Formatação de número malformada (ex: múltiplos pontos decimais `1.2.3` ou sufixo inválido). |
| **LX004** | `unescaped bidirectional Unicode control character: '{char}'` | Error | `0.1.0` | Tentativa de utilizar caracteres de controle bidirecional Unicode invisíveis (Trojan Source, CWE-1307) em comentários ou código. |

---

### 3.2 Categoria P: Parser e Sintaxe (`P000` - `P099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **P001** |  `'{expectation}' (found '{found}')` no formato `expected … (found …)` | Error | `0.1.0` | Erro geral do parser Pratt indicando que um token específico era esperado mas outro foi encontrado. A mensagem usa grafia voltada ao usuário (ex: `expected = (found Mensagem)`), nunca nomes internos de tokens (`EQUAL`/`IDENT_TYPE`). |
| **P002** | `unclosed block: expected '}', found EOF` | Error | `0.1.0` | Um bloco `{ ... }` ou escopo de função foi aberto mas nunca fechado no final do arquivo. |
| **P003** | `invalid assignment operator: '{op}'` | Error | `0.1.0` | Uso de operador de atribuição inválido ou malformado na gramática. |
| **P004** | `expected identifier, found '{token}'` | Error | `0.1.0` | O parser esperava encontrar um nome (identificador de variável/função) mas encontrou uma palavra-chave ou símbolo. |
| **P005** | `expected expression, found '{token}'` | Error | `0.1.0` | O parser Pratt falhou ao tentar iniciar a análise de uma expressão devido a um token inesperado. |
| **P006** | `malformed attribute: '@{name}'` | Error | `0.1.0` | Um atributo ou anotação especial da linguagem foi declarado de forma inválida ou sem os parâmetros obrigatórios. |

---

### 3.3 Categoria M: Módulos e Imports (`M000` - `M099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **M001** | `unresolved import: cannot find '{name}' in module '{module}'` | Error | `0.1.0` | Falha ao tentar importar um membro específico ou sub-módulo que não existe no caminho de importação. (Antigo `N006`). |
| **M004** | `implicit local import is deprecated in package mode` | Warning | `0.1.0` | Migração estruturada de imports locais bare para a raiz explícita `self`. |
| **M005** | `quoted filesystem import is forbidden in package mode` | Error | `0.1.0` | Impede caminhos/URLs fora do grafo declarado no manifesto. |
| **M002** | `undefined namespace member: '{member}' not found in namespace '{namespace}'` | Error | `0.1.0` | Acesso a um membro inexistente dentro de um namespace importado. (Antigo `N009`). |
| **M003** | `namespace '{name}' used as a value` | Error | `0.1.0` | Tentativa de avaliar um namespace/módulo diretamente como se fosse uma variável ou objeto. (Antigo `N008`). |

---

### 3.4 Categoria N: Resolução de Nomes, Escopo e Semântica Inicial (`N000` - `N099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **N001** | `undefined value: '{name}' is not defined in this scope` | Error | `0.1.0` | Tentativa de usar uma variável, constante ou função que não existe ou não está visível no escopo atual. |
| **N002** | `undefined type: '{name}' is not defined in this scope` | Error | `0.1.0` | Tentativa de usar um struct, enum ou interface que não existe no escopo. |
| **N003** | `redefined name: '{name}' is already defined in this scope` | Error | `0.1.0` | Declaração duplicada de uma variável, função ou tipo no mesmo escopo local ou global. |
| **N004** | `type '{name}' is used as a value` | Error | `0.1.0` | Uso incorreto de um identificador de tipo (como `User`) em uma posição onde um valor era esperado. |
| **N005** | `value '{name}' is used as a type` | Error | `0.1.0` | Uso incorreto de um identificador de variável ou valor em uma posição onde um tipo era esperado. |
| **N006** | `import name conflict: '{name}' is already defined in this scope` | Error | `0.1.0` | Um import introduz um nome que já foi vinculado no mesmo escopo (por outro import ou declaração). |
| **N007** | `undefined assignment target: cannot assign to '{name}'` | Error | `0.1.0` | Tentativa de atribuir um valor a algo que não é um local de memória gravável. |
| **N008** | *[Movido → M003]* | - | `0.1.0` | *Código de namespace usado como valor movido para categoria de módulos.* |
| **N009** | *[Movido → M002]* | - | `0.1.0` | *Código de membro de namespace não encontrado movido para categoria de módulos.* |
| **N010** | `undefined associated function: '{name}' not found on type '{type}'` | Error | `0.1.0` | Chamada de função associada estática que não foi declarada no struct ou tipo correspondente (ex: `User.newFunc()`). **Diferença de T018**: Ocorre na fase de Name Resolution antes da verificação de tipos, quando o receptor ainda é um identificador não resolvido para um tipo concreto. |
| **N011** | `break/continue statement used outside of a loop` | Error | `0.1.0` | Uso de comandos de controle de fluxo de iteração fora de escopos de laço válidos (`for`/`while`). (Antigo `T022`). |
| **N012** | `unknown annotation '@{name}'` | Error | `0.1.0` | Nome de anotação não reconhecido ou reservado cuja semântica ainda não está disponível. |
| **N013** | `annotation '@{name}' cannot be applied to a {target}` | Error | `0.1.0` | Anotação conhecida aplicada a uma categoria de declaração fora de seus alvos explícitos. |
| **N014** | `annotation '@{name}' expects {arguments}` | Error | `0.1.0` | Lista ou forma dos argumentos não corresponde ao contrato da anotação. |
| **N015** | `annotation '@{name}' cannot be repeated` | Error | `0.1.0` | Uma anotação de cardinalidade única aparece mais de uma vez no mesmo alvo. |

---

### 3.5 Categoria T: Verificador de Tipos (`T000` - `T199`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **T001** | `cannot infer type: type annotation needed for '{name}'` | Error | `0.1.0` | O compilador não possui informações suficientes para deduzir o tipo de uma variável e exige declaração explícita. |
| **T002** | `incompatible assignment: expected '{expected}', found '{found}'` | Error | `0.1.0` | Tentativa de atribuir um tipo incompatível a uma variável já tipada. |
| **T003** | `incompatible argument: expected '{expected}', found '{found}'` | Error | `0.1.0` | Passagem de um argumento com tipo incorreto para uma chamada de função. |
| **T004** | `incompatible return type: expected '{expected}', found '{found}'` | Error | `0.1.0` | O tipo da expressão de retorno dentro da função não condiz com a assinatura declarada. |
| **T005** | `operator '{op}' is not applicable to type '{type}'` | Error | `0.1.0` | Tentativa de usar operador matemático ou lógico em tipos não suportados. |
| **T006** | `type '{type}' is not nullable` | Error | `0.1.0` | Atribuição de `null` ou uso de operador de navegação segura em um tipo que não é explicitamente opcional/nulo. |
| **T007** | `type mismatch: 'if' and 'else' branches have incompatible types: '{then_ty}' and '{else_ty}'` | Error | `0.1.0` | As ramificações de uma expressão condicional `if/else` avaliam para tipos diferentes (Type Mismatch). |
| **T008** | `type mismatch: match arm has type '{arm_ty}', expected '{expected_ty}'` | Error | `0.1.0` | Um braço do bloco `match` retorna um tipo inconsistente com a expressão esperada (Type Mismatch). |
| **T009** | `condition is not a boolean: expected 'bool', found '{type}'` | Error | `0.1.0` | A expressão condicional de um `if` ou `while` não avalia para o tipo booleano primário. |
| **T010** | `invalid cast: cannot cast type '{from_ty}' to '{to_ty}'` | Error | `0.1.0` | Conversão explícita de tipos (`as`) inválida ou não suportada pelas regras de coerção da linguagem. |
| **T011** | `generic constraint not satisfied: '{type}' does not satisfy constraint '{constraint}'` | Error | `0.1.0` | Um parâmetro genérico passado não atende às restrições da cláusula `where` declarada. |
| **T012** | `wrong argument count: expected {expected}, found {found}` | Error | `0.1.0` | A chamada de função ou método recebeu um número incorreto de parâmetros. |
| **T013** | `unknown named argument: '{name}'` | Error | `0.1.0` | Passagem de parâmetro nomeado que não corresponde a nenhum argumento na assinatura do método. |
| **T014** | `invalid variadic type: expected '{expected}', found '{found}'` | Error | `0.1.0` | Passagem incorreta de argumentos para uma assinatura de função variádica. |
| **T015** | `implicit widening of '{from_ty}' to '{to_ty}' is not allowed` | Warning | `0.1.0` | Tentativa de realizar coerção implícita que pode causar perda de precisão ou overflow (ex: `i32` para `i16`). |
| **T016** | `try operator '?' cannot be used on type '{type}'` | Error | `0.1.0` | O operador de desempacotamento seguro `?` foi aplicado a um tipo que não é `Result` ou `Option`. |
| **T017** | `cannot index type '{type}' with index of type '{index_ty}'` | Error | `0.1.0` | Tentativa de indexar um array ou coleção com um tipo não inteiro. |
| **T018** | `no field '{field}' on type '{type}'` | Error | `0.1.0` | Acesso a um campo inexistente em uma instância de struct ou união. **Diferença de N010**: Ocorre na verificação de tipos após o receptor ser resolvido para um tipo concreto específico. |
| **T019** | *[Movido → W006]* | - | `0.1.0` | *Código de resultado não tratado movido para a categoria de warnings e lints.* |
| **T020** | *[Reservado / Obsoleto]* | - | `0.1.0` | *Código reservado para manter o alinhamento de sequenciamento de commits legados.* |
| **T021** | `method requires 'self' receiver` | Error | `0.1.0` | Tentativa de chamar um método de instância como função estática sem passar a referência de `self`. |
| **T022** | *[Movido → N011]* | - | `0.1.0` | *Código de fluxo de iteração fora de laço movido para Name Resolution.* |
| **T023** | *[Movido → O011]* | - | `0.1.0` | *Código de free em tipo não-ponteiro movido para Ownership/Memória.* |
| **T024** | `non-exhaustive match: patterns not covered: {missing}` | Error | `0.1.0` | O bloco `match` não cobriu todas as variantes possíveis do tipo avaliado. |
| **T025** | `interface not satisfied: type '{type}' does not implement interface '{interface}'` | Error | `0.1.0` | Falha ao passar uma estrutura em um contexto de tipo de interface. **Diferença de G003**: Aplica-se a contextos não-genéricos de passagem/atribuição direta de valores. Falhas de restrições em cláusulas `where` genéricas usarão futuramente `G003`. |
| **T026** | `cannot assign twice to immutable variable '{name}'` | Error | `0.1.0` | Tentativa de reatribuir valor a uma variável local ou parâmetro não declarado como `mut`. |
| **T027** | `missing fields {fields} in struct initializer` | Error | `0.1.0` | Instanciação de struct omitindo campos obrigatórios. |
| **T028** | `field '{name}' initialized more than once` | Error | `0.1.0` | O mesmo campo foi inicializado múltiplas vezes no mesmo literal de struct. |
| **T029** | `recursive type '{name}' has infinite size` | Error | `0.1.0` | Definição de struct recursiva sem indicação de ponteiro (indireção). |
| **T030** | `field '{name}' is already declared in struct '{struct_name}'` | Error | `0.1.0` | Declaração de múltiplos campos com o mesmo nome na definição de um struct. |
| **T031** | *[Reservado / Obsoleto]* | - | `0.1.0` | *Código reservado para mutação futura de valores sob referências somente-leitura.* |
| **T032** | `await requires a coroutine value` | Error | `0.1.0` | O operando de `await` não é uma coroutine válida. |
| **T033** | `indirect call is not supported` | Error | `0.1.0` | A chamada não possui um alvo direto suportado pelo contrato atual. |
| **T034** | `cannot format value as str` | Error | `0.1.0` | O tipo não participa do contrato atual de apresentação textual. |
| **T035** | `invalid @Destructor contract` | Error | `0.1.0` | O método anotado não é um destrutor consumidor válido ou o tipo já possui outro destrutor. |
| **T036** | `invalid @Test contract` | Error | `0.1.0` | A função de teste não satisfaz o contrato síncrono, não genérico, sem parâmetros e com retorno `void` ou `Result<void, E>`. |
| **T037** | `invalid @Benchmark contract` | Error | `0.1.0` | A função de benchmark não satisfaz o contrato síncrono, não genérico, com um contexto `mut testing.Benchmark` e retorno `void`. |
| **T038** | `integer literal does not fit in the expected type` | Error | `0.1.0` | Um literal inteiro contextual excede o intervalo representável pelo tipo inteiro esperado. |
| **T039** | `function performs undeclared or denied effect '{effect}'` | Error | `0.1.0` | A função executa um efeito não declarado em `@Effects(...)` ou proibido pela política de efeitos do manifesto. |
| **T040** | `attempt to divide by zero` | Error | `0.1.0` | Tentativa de realizar divisão ou cálculo de resto (`%`) com divisor zero em tempo de compilação. |

---

### 3.6 Categoria O: Ownership, Borrow Checking e Memória (`O000` - `O099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **O001** | `use of moved value: '{name}'` | Error | `0.1.0` | Tentativa de ler ou acessar uma variável cujo valor já foi movido (ownership transferida) em uma linha anterior. |
| **O002** | `cannot move '{name}' while borrowed` | Error | `0.1.0` | Move/consume de valor com empréstimo ativo (M2). |
| **O003** | `mutable borrow conflict on '{name}'` | Error | `0.1.0` | Empréstimos exclusivos sobrepostos (`mut ref` vs qualquer) ou mutação sob empréstimo compartilhado. Múltiplos `ref` compartilhados são permitidos. |
| **O004** | `generational fallback: '{name}' escapes stack-limited borrow window` | Info | `0.1.0` | Nota de fallback geracional/escape (G2/F2.3) — **não** conflito shared. |
| **O005** | `double free detected for '{name}'` | Error | `0.1.0` | Análise estática do CFG detectou que o mesmo objeto sob ownership exclusiva seria destruído ou liberado mais de uma vez. |
| **O006** | `cannot destroy '{name}' while borrowed` | Error | `0.1.0` | Destroy/free/drop com empréstimo ainda ativo (double-free estático; M2). |
| **O007** | `inconsistent move status for '{name}' between branches` | Error | `0.1.0` | Uma variável é movida em uma ramificação condicional (ex: `if`), mas não na outra, deixando seu estado pós-bloco ambíguo. |
| **O008** | `use of possibly uninitialized variable: '{name}'` | Error | `0.1.0` | Tentativa de ler uma variável local antes de garantir sua atribuição/inicialização em todos os caminhos do fluxo de controle. |
| **O009** | `lifetime mismatch: lifetime of '{expected}' does not match lifetime of '{found}'` | Error | `0.1.0` | As restrições de tempo de vida de referências genéricas não conferem na passagem de argumentos ou atribuição. |
| **O010** | `escape of borrowed value: returning reference to local variable '{name}'` | Error | `0.1.0` | Retorno de uma referência para um objeto alocado na pilha local da função corrente, o que causaria memória corrompida. |
| **O011** | `free requires pointer type: cannot free expression of type '{type}'` | Error | `0.1.0` | O comando de desalocação explícita `free` foi chamado em uma variável que não é um ponteiro bruto (`*mut` ou `*const`). (Antigo `T023`). |
| **O012** | `` `alloc` requires an `unsafe` block `` | Error | `0.1.0` | Alocação direta de memória bruta na heap via `alloc` exige contexto explícito `unsafe`. |
| **O013** | `` call to extern function requires an `unsafe` block `` | Error | `0.1.0` | Chamadas para funções `extern` e funções anotadas com `@Unsafe` exigem bloco `unsafe`. |
| **O014** | `` `free` requires an `unsafe` block `` | Error | `0.1.0` | Desalocação manual de memória via `free` é operação insegura e exige bloco `unsafe`. |


---

### 3.7 Categoria G: Genéricos e Instanciação (`G000` - `G099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **G001** | `generic instantiation cycle detected: '{cycle}'` | Error | `0.1.0` | Recursão infinita detectada na resolução de tipos genéricos (monomorfização recursiva que não termina). |
| **G002** | `generic instantiation limit reached: maximum recursion depth exceeded` | Error | `0.1.0` | O compilador interrompeu a expansão de genéricos por atingir o limite de segurança de profundidade de tipos. |

---

### 3.8 Categoria W: Warnings, Estilo e Linting (`W000` - `W099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **W001** | `variable '{name}' is assigned but never used` | Warning | `0.1.0` | Declaração de variável que nunca é lida no decorrer do bloco de execução corrente. |
| **W002** | `dead code: '{name}' is declared but never used` | Warning | `0.1.0` | Funções, structs, enums ou interfaces definidos no módulo que nunca são acessados. |
| **W003** | `unreachable code after statement` | Warning | `0.1.0` | Trechos de código localizados logo após instruções de terminação incondicional (como `return`, `break` ou `continue`). |
| **W004** | `variable shadowing: '{name}' shadows a variable defined in an outer scope` | Warning | `0.1.0` | Re-declaração de uma variável em um escopo filho que oculta a variável homônima do escopo pai. |
| **W005** | `unnecessary mutability: variable '{name}' does not need to be mutable` | Warning | `0.1.0` | Variável declarada como `mut` mas que nunca sofreu reatribuições ou modificações mutáveis. |
| **W006** | `unhandled result: value of type 'Result' must be checked or propagated` | Warning | `0.1.0` | Uma chamada que retorna `Result<T, E>` foi descartada silenciosamente. (Movido do antigo `T019`). |
| **W007** | `unused import: '{name}'` | Warning | `0.1.0` | Um módulo ou símbolo foi importado no topo do arquivo mas nunca foi referenciado ou consumido no código. |
| **W008** | `legacy annotation name '@{old}'; use '@{canonical}'` | Warning | `0.1.0` | Grafia temporariamente aceita para migração; inclui replacement estruturado sobre apenas o identificador da anotação. |

> [!NOTE]
> **Decisão de Design sobre Shadows (`W004`)**: O shadowing de variáveis é classificado como um `Warning` (e não `Error`) no Arandu para permitir flexibilidade de escrita em escopos muito curtos de closures e lambdas iteradoras. No entanto, é fortemente alertado por padrão para evitar que desenvolvedores ocultem nomes acidentais em escopos maiores do compilador. Pode ser silenciado localmente com `@Suppress("shadowing")`.

---

### 3.9 Categoria U: Recursos Não Implementados (Unimplemented) (`U000` - `U099`)

Esta categoria agrupa recursos previstos pela sintaxe da linguagem ou sua especificação, mas que ainda não foram desenvolvidos pelo compilador (similar ao `sorry, unimplemented` do GCC). Não representa quebra lógica do compilador (ICE).

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **U001** | `feature not yet supported: '{feature}'` | Error | `0.1.0` | Tentativa de utilizar um recurso de sintaxe válido, mas que a versão atual do compilador não suporta ou não gera código correspondente. (Antigo `L002`). |

---

### 3.10 Categoria L: Lowering (`L000` - `L099`)

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **L001** | `lowering error: unresolved symbol '{name}'` | Error | `0.1.0` | Erro na fase de lowering de AST para AHIR/AMIR devido a um símbolo pendente não resolvido pelas fases anteriores. |
| **L002** | *[Movido → U001]* | - | `0.1.0` | *Compilador encontrou recurso sintático válido mas não implementado. Migrado para a categoria de recursos não implementados.* |

---

### 3.11 Categoria ICE: Erros Internos do Compilador (`ICE-[COMPONENTE]-000`)

Erros de pânico gerados devido a falhas do próprio compilador Arandu. O sufixo de fase no código aponta a área problemática:

| Código | Mensagem Principal no Compilador | Severidade Padrão | Introduzido em | Descrição e Contexto |
| :--- | :--- | :--- | :--- | :--- |
| **ICE-LX-001** | `internal lexer error: invalid character boundary match` | ICE (Fatal) | `0.1.0` | Corrupção de ponteiro de bytes durante processamento UTF-8 no analisador léxico. |
| **ICE-P-001** | `internal parser error: corrupt AST node generation` | ICE (Fatal) | `0.1.0` | Geração de nós órfãos ou inconsistências estruturais no analisador sintático Pratt. |
| **ICE-N-001** | `internal name resolution error: duplicate ID assignment` | ICE (Fatal) | `0.1.0` | Atribuição de identificadores de símbolos duplicados no grafo global de escopo. |
| **ICE-T-001** | `internal type checker error: unification variable collision` | ICE (Fatal) | `0.1.0` | Falha estrutural ou colisão de escopos de tipos genéricos durante unificação de tipos de Hindley-Milner. |
| **ICE-O-001** | `internal ownership error: lifetime solver state corrupted` | ICE (Fatal) | `0.1.0` | O resolvedor de lifetimes entrou em inconsistência lógica ao calcular o tempo de vida. |
| **ICE-L-001** | `internal lowering error: unexpected AST state during lowering` | ICE (Fatal) | `0.1.0` | Pânico provocado por discrepâncias de tipos durante a conversão da AST para HIR/AMIR. |
| **ICE-GEN-001** | `internal monomorphization error: instantiation limit loop error` | ICE (Fatal) | `0.1.0` | Colapso no cálculo de dependências ou ordenação topológica das monomorfizações de genéricos. |
| **ICE-GEN-002** | `internal IR error: AMIR validation failed` | ICE (Fatal) | `0.1.0` | Falha de validação estrutural no AMIR (invariantes SSA, tipos, parâmetros ou arestas de CFG corrompidas). |


---

## 4. Evolução Futura e Manutenção

Para adicionar um novo código de erro ao compilador Arandu, siga exatamente as seguintes regras e procedimentos:

### 4.1 Regras de Numeração e Sequenciamento
*   **Sequenciamento Linear**: Novos códigos sempre devem utilizar o próximo número sequencial disponível dentro da categoria correspondente (ex: se o último erro em `T` foi `T025`, o próximo deve ser `T026`).
*   **Invariabilidade de Lacunas**: Lacunas no sequenciamento lógico geradas por remoções ou migrações históricas de códigos **nunca devem ser preenchidas retroativamente**.
*   **Preservação e Rastreabilidade**: Quando um código é descontinuado ou movido para outra categoria, ele não é excluído. Em vez disso, sua linha na tabela correspondente é marcada com `[Reservado / Obsoleto]` ou `[Movido → CÓDIGO]` para manter a rastreabilidade histórica em issues, discussões do GitHub e commits legados.

### 4.2 Roteiro de Modificações no Código (Erros Normais e Warnings)
1.  **Declaração do Enum**:
    Edite o arquivo `crates/arandu_diagnostics/src/lib.rs` e adicione a nova variante ao enum `DiagCode`. Por exemplo:
    ```rust
    pub enum DiagCode {
        // ...
        T026NewTypeError,
    }
    ```
2.  **Mapeamento de String**:
    No mesmo arquivo `crates/arandu_diagnostics/src/lib.rs`, implemente o mapeamento da string na função `as_str()` e adicione a variante à constante `ALL`:
    ```rust
    DiagCode::T026NewTypeError => "T026",
    ```
3.  **Criação do Documento Humano**:
    Crie o arquivo Markdown com a explicação detalhada em inglês (conforme exigido pelo `AGENTS.md`) sob o caminho `docs/errors/T026.md`. Certifique-se de que ele contém:
    *   Título descrevendo o erro.
    *   Exemplo de código incorreto.
    *   Explicação semântica.
    *   Exemplo de código corrigido.
4.  **Compilação**:
    Execute o comando `cargo build`. O build script (`build.rs`) da crate `arandu_diagnostics` irá escanear o diretório, validar a correspondência 1:1 e gerar o mapeamento em `registry_gen.rs` de forma transparente. Se o arquivo `.md` não for encontrado, a compilação travará.

### 4.3 Adicionando um Novo ICE
ICEs seguem as mesmas etapas de registro em código da seção 4.2, mas com as seguintes diferenças importantes:
*   O código de erro gerado segue o formato contendo hífen: `ICE-[FASE]-[NNN]`.
*   **Não requer** a criação de um arquivo Markdown associado em `docs/errors/` (já que representam falhas internas de desenvolvimento, e não erros de sintaxe do desenvolvedor que necessitam de tutoriais de correção).
*   O build script `build.rs` detecta automaticamente a string prefixada com `ICE-` e **ignora/pula a validação de bijetividade** para essa variante, permitindo compilar normalmente sem o arquivo `.md`.
