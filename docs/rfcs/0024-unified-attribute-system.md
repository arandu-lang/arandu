# RFC 0024: Sistema Unificado de Atributos — `@` como mecanismo comptime de primeira classe

- **Número da RFC:** 0024
- **Título:** Sistema Unificado de Atributos — `@` como mecanismo comptime de primeira classe
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-03
- **Status:** `Draft`
- **Área Principal:** `Frontend` / `Middle-end` / `Stdlib`
- **Depende de:** RFC 0002 (convenção `@PascalCase`), RFC 0013 (CTFE e comptime core)
- **PR da RFC:** N/A (In-Tree RFC)
- **Issue de Acompanhamento:** N/A

---

## 1. Resumo

Esta RFC define o modelo semântico completo de atributos no Arandu. Um atributo
`@Foo(args...)` aplicado a uma declaração é açúcar sintático para uma chamada
comptime `Foo.apply(target, args...)`, onde `target` é uma representação
reflexiva da declaração anotada. O compilador fornece apenas três primitivos:
resolução de `@`, execução comptime e a API de reflection. Todo o resto —
`@Derive`, `@Test`, `@Deprecated`, `@Json`, `@Orm.Table` — é implementado em
stdlib ou em bibliotecas de terceiros, sem privilégio especial em relação ao
código do usuário.

Este design unifica atributos intrínsecos (`@Link`, `@Effects`) e atributos
de biblioteca (`@Derive`, `@Test`) sob a mesma categoria sintática, eliminando
a distinção visível entre "magia do compilador" e "código de usuário".

---

## 2. Motivação

### 2.1. Problema atual

Hoje, `@Effects`, `@Link`, `@Test`, `@Benchmark`, `@Suppress` e similares são
intrínsecos do compilador — não há mecanismo para o usuário definir atributos
próprios com semântica comparável. Isso cria uma assimetria invisível:

```arandu
@Link("m")          // intrínseco — compilador conhece explicitamente
@Derive(Debug)      // futuro — seria diferente? biblioteca? novo mecanismo?
@Orm.Table("users") // framework — como se registraria?
```

Sem um modelo unificado, cada novo tipo de atributo (derivação, testes, ORM,
HTTP routing, deprecated, serialização) exigiria extensão do compilador ou um
sistema paralelo de macros.

### 2.2. Objetivo

Definir uma única regra que cubra todos os casos:

> `@Foo(args...)` sobre uma declaração `D` é equivalente a executar
> `Foo.apply(D, args...)` em comptime, antes da resolução do restante
> do item.

Isso torna o compilador um fornecedor de **mecanismo**, não de **política**.
`@Derive`, `@Test`, `@Deprecated` passam a ser funções comptime da stdlib,
implementáveis por qualquer biblioteca sem acesso especial ao compilador.

### 2.3. Casos de uso que se tornam possíveis

- `@Derive(Debug, Eq, Hash)` — stdlib gera impls automaticamente via reflection
- `@Test`, `@Benchmark` — stdlib registra símbolos em registros comptime
- `@Deprecated("use X")` — stdlib anexa metadata ao símbolo para lint
- `@Json(camelCase: true)` — biblioteca externa gera serialização customizada
- `@Orm.Table("users")` — framework de banco injeta métodos no struct
- `@Http.Get("/users/:id")` — framework web registra rotas em tabela comptime
- `@Packed`, `@Align(16)` — atributos de layout, eventualmente user-definíveis

---

## 3. Explicação em Nível de Guia

### 3.1. Visão do usuário final

O usuário escreve atributos acima das declarações, em `@PascalCase`, exatamente
como hoje:

```arandu
@Derive(Debug, Eq, Hash)
struct Point {
    x: float
    y: float
}

@Test
func parserHandlesEmpty() {
    let result = parse("")
    assert(result.isOk())
}

@Deprecated("use parseNew instead")
func parseOld(src: str): Ast { ... }

@Json(camelCase: true)
struct ApiResponse {
    userId:   uint
    userName: str
}
```

A regra é uniforme: `@Foo(args...)` sobre uma declaração D invoca `Foo.apply(D, args...)` em tempo de compilação. O usuário não precisa saber se `Foo` é da stdlib, do compilador, ou de uma biblioteca de terceiros — todos usam a mesma sintaxe.

### 3.2. Atributos com múltiplos argumentos de tipo — `@Derive`

`@Derive` é um atributo da stdlib cuja implementação itera sobre as interfaces
fornecidas e chama o protocolo `derive` de cada uma:

```arandu
@Derive(Debug, Eq, Hash)
struct Color { r: u8  g: u8  b: u8 }
```

É equivalente a:

```arandu
// gerado pelo compilador, invisível ao usuário:
comptime Derive.apply(Color, Debug, Eq, Hash)
// que internamente faz:
//   Debug.derive(Color)
//   Eq.derive(Color)
//   Hash.derive(Color)
```

O resultado são as implementações geradas, injetadas no escopo do módulo como
se o usuário tivesse escrito os métodos manualmente. Se o usuário definir
manualmente o mesmo método, o manual tem precedência — o `derive` omite
silenciosamente o membro já existente.

### 3.3. Atributos com argumentos de valor — `@Json`

```arandu
@Json(camelCase: true)
struct Response {
    userId: uint
    name:   str
}
```

É equivalente a:

```arandu
comptime Json.apply(Response, camelCase: true)
```

`Json.apply` usa reflection para gerar `toJson()` e `fromJson()` com os campos
renomeados conforme a opção. O usuário não precisa chamar nada manualmente.

### 3.4. Atributos de registro — `@Test` e `@Benchmark`

```arandu
@Test
func addsCorrectly() {
    assert(add(1, 2) == 3)
}
```

`Test.apply` não gera código novo — registra o símbolo em um registro comptime
que o test runner consulta. O compilador não precisa conhecer `@Test` — é apenas
uma chamada comptime que chega ao stdlib antes da emissão de código.

### 3.5. Atributos de metadata pura — `@Deprecated`

```arandu
@Deprecated("use parseNew instead")
func parseOld(src: str): Ast { ... }
```

`Deprecated.apply` anexa metadata ao símbolo via a API de reflection. O
compilador emite um diagnóstico em todo site de uso que referenciar um símbolo
marcado como deprecated.

### 3.6. Atributos de terceiros — `@Orm.Table`

```arandu
import orm

@Orm.Table("orders")
@Derive(Debug)
struct Order {
    id:    uint
    total: float
    paid:  bool
}
```

`Orm.Table` é um atributo definido pelo pacote `orm`, sem nenhum privilégio
especial no compilador. Sua implementação usa a mesma API de reflection e
geração de código que `@Derive` usa.

---

## 4. Explicação em Nível de Referência

### 4.1. Protocolo canônico de atributos

Todo atributo executável implementa uma `comptime func apply`:

```arandu
// Protocolo mínimo — qualquer alvo
comptime func Foo.apply(comptime target: Declaration): void { ... }

// Com argumentos posicionais
comptime func Json.apply(
    comptime target: Declaration,
    camelCase: bool
): void { ... }

// Restrito a um tipo de alvo específico
comptime func Test.apply(comptime target: Function): void { ... }
comptime func Packed.apply(comptime target: Struct): void { ... }
```

O compilador verifica o tipo do `target` na assinatura e emite erro estruturado
se o atributo for aplicado a um alvo incompatível:

```
error[A001]: @Packed cannot be applied to a function
  ┌─ main.aru:3:1
  │
3 │ @Packed
  │ ^^^^^^^
4 │ func hash(x: uint): uint { ... }
  │
  note: @Packed expects: Struct
        found: Function
```

### 4.2. Hierarquia de `Declaration`

A API de reflection expõe uma hierarquia de tipos que representam declarações
inspecionáveis em comptime:

```
Declaration
├── TypeDecl
│   ├── Struct       — campos, visibilidade, generics
│   ├── Enum         — variantes, payloads
│   └── Interface    — métodos obrigatórios, defaults
├── Function         — parâmetros, retorno, efeitos, corpo
├── Variable         — tipo, mutabilidade, inicializador
└── Module           — itens exportados, imports
```

Cada variante expõe campos de reflection adequados ao seu tipo:

```arandu
// Struct
comptime func deriveDebug(comptime T: Struct): impl {
    return impl {
        func debug(shared self): str {
            var out = T.name ++ " { "
            comptime for field in T.fields() {
                out += field.name ++ ": "
                out += self[field.name].debug()
                out += ", "
            }
            return out ++ "}"
        }
    }
}

// Function
comptime func Test.apply(comptime target: Function): void {
    TestRegistry.add(target.symbol, target.qualifiedName)
}
```

### 4.3. O protocolo `derive` de interfaces

Interfaces que suportam derivação automática implementam `comptime func derive`:

```arandu
interface Debug {
    func debug(shared self): str

    comptime func derive(comptime T: type): impl {
        return impl {
            func debug(shared self): str {
                // implementação padrão via reflection
            }
        }
    }
}
```

`@Derive(Debug)` sobre `struct Point` resulta em `Debug.derive(Point)`, que
retorna um bloco `impl` injetado no escopo do módulo. A interface que não
implementar `comptime func derive` não pode aparecer em `@Derive`; o compilador
emite erro estruturado:

```
error[A002]: Debug cannot be derived — interface does not declare comptime func derive
```

### 4.4. Implementação stdlib de `@Derive`

```arandu
// stdlib/derive.aru
comptime func Derive.apply(
    comptime T: type,
    comptime interfaces: ...type
): void {
    comptime for I in interfaces {
        if !I.hasComptimeMember("derive") {
            compileError(I.name ++ " cannot be derived: missing comptime func derive")
        }
        I.derive(T)
    }
}
```

O compilador não conhece `Derive`. Ele conhece apenas a regra: `@Foo(args...)` →
`Foo.apply(target, args...)`. Todo o protocolo vive na stdlib.

### 4.5. Ordem de execução com múltiplos atributos

Atributos empilhados executam **de cima para baixo**, em sequência, antes da
resolução de nomes do item anotado:

```arandu
@Orm.Table("users")   // executa primeiro
@Derive(Debug, Eq)    // executa segundo
struct User { ... }
```

Isso garante que atributos que injetam metadata (como `@Orm.Table`) são visíveis
para atributos subsequentes que possam ler essa metadata.

### 4.6. Declaração de efeitos em atributos

Atributos comptime que precisam de acesso além do grafo de compilação devem
declarar seus efeitos explicitamente, integrando-se ao sistema de effects do
Arandu:

```arandu
// Atributo puro — só reflection, sem I/O
comptime func Derive.apply(
    comptime T: type,
    comptime interfaces: ...type
): void @Effects(reflection) { ... }

// Atributo que lê schema de arquivo — declara leitura de filesystem
comptime func Orm.Table.apply(
    comptime target: Struct,
    tableName: str
): void @Effects(reflection, fsRead) { ... }
```

Atributos sem declaração de efeitos são considerados puros e podem ser
memoizados pelo Salsa normalmente.

### 4.7. Atributos intrínsecos como casos especiais do mesmo protocolo

`@Link`, `@Effects`, `@Packed` e similares continuam sendo resolvidos pelo
compilador diretamente (bootstrapping — a própria API de reflection precisa
existir antes de ser usável por atributos). Sua semântica é equivalente ao
protocolo acima, mas sua implementação está no compilador por necessidade, não
por privilégio. Do ponto de vista do usuário, todos os atributos são iguais.

### 4.8. Impacto no pipeline incremental (Salsa)

- Fragmentos gerados por `apply` são inputs Salsa com identidade estável:
  `SymbolId::Generated { origin: ExprId, index: u32 }`
- O grafo de dependência inclui: o atributo aplicado, os argumentos, o tipo
  anotado e as dependências da implementação do `apply`
- Editar um método `manual` de um tipo não reinvalida o `@Derive` —
  Salsa vê os mesmos inputs e corta a re-execução
- Editar a implementação do `apply` invalida todos os itens que usam aquele atributo

---

## 5. Invariantes de Arquitetura e Desvantagens

### 5.1. Invariantes preservados

- **Pureza de queries**: `apply` comptime é pura (sem I/O por padrão); efeitos
  são declarados explicitamente via `@Effects`. Queries tracked não adquirem
  efeitos colaterais.
- **`SymbolId` monotônico**: `Generated` é uma variante adicional, não reutiliza
  IDs existentes. O alocador de IDs gerados é monotônico por sessão.
- **Early-cutoff**: fragmentos gerados com output idêntico não invalidam
  consumidores. O hash do `impl` gerado entra no hash do item.
- **Ausência de I/O em queries puras**: atributos que declaram `fsRead` ou
  efeitos equivalentes são excluídos da memoização normal.

### 5.2. Desvantagens e riscos

- **Bootstrapping**: reflection precisa existir antes dos atributos que a usam.
  `@Link`, `@Effects` e intrínsecos de layout permanecem hardcoded durante as
  fases iniciais.
- **Ciclos de atributo**: `@Derive(Debug)` em um struct cujo campo usa um tipo
  que também depende de `Debug` pode criar ciclo. O sistema de ciclos do Salsa
  precisa cobrir esse caso com `ResolutionResult`.
- **Higiene**: nomes gerados por `apply` entram no namespace do módulo. A regra
  "manual vence" evita conflito, mas a stdlib deve documentar quais nomes cada
  atributo pode injetar.
- **Diagnósticos de atributo**: erros emitidos dentro de `apply` devem apontar
  para o site de uso `@Foo(...)`, não para a implementação da stdlib.

---

## 6. Racional e Alternativas

### 6.1. Por que não proc-macros separadas (estilo Rust)?

Proc-macros em Rust operam sobre `TokenStream` — uma representação de baixo nível
sem tipos. Isso obriga autores de macro a re-parsear e re-tipar o código
internamente, duplicando trabalho. O modelo do Arandu opera sobre `Declaration`
tipada — o compilador já fez o trabalho de parsing e tipagem, e a macro recebe
uma representação de alto nível diretamente.

Além disso, proc-macros em Rust são crates separadas, compiladas com uma toolchain
diferente. No Arandu, atributos são comptime functions da mesma linguagem, no
mesmo pacote, sem sistema paralelo.

### 6.2. Por que não `with Debug, Eq` como cláusula de struct?

`with` consumiria uma keyword extremamente genérica para um caso específico
(derivação), e criaria ambiguidade futura com composição de interfaces, herança
ou constraints de tipo. `@Derive(Debug, Eq)` é mais explícito no que faz e
escala para qualquer número de interfaces sem verbosidade adicional. Ver §2 da
discussão de design.

### 6.3. Por que não `@deriveDebug` em vez de `@Derive(Debug)`?

`@deriveDebug` escala mal — cinco interfaces viram cinco linhas de atributo.
Além disso, viola a separação entre "o quê" (derivar Debug) e "como" (chamar
`Debug.derive`), expondo detalhe de implementação na superfície da linguagem.
`@Derive(Debug, Eq, Hash, Clone, Ord)` é uma linha e lê como declaração de
intenção.

---

## 7. Arte Prévia

| Linguagem | Mecanismo | Observação |
|---|---|---|
| Rust | `#[derive(Debug)]` + proc-macro | poderoso, mas sistema paralelo com TokenStream |
| Haskell | `deriving (Show, Eq, Ord)` | sintaxe integrada, não extensível por usuário |
| Python | decoradores `@decorator` | extensível, mas sem tipos e sem execução em compilação |
| Zig | `comptime` inline, sem atributos | arquiteturalmente puro, ergonomia menor |
| Swift | `@propertyWrapper`, `@resultBuilder` | extensível, mas protocolos específicos por caso |
| Kotlin | `@annotation` + annotation processors | extensível via processador externo (APT/KSP) |
| Java | `@Annotation` + annotation processors | poderoso, mas processadores são programas externos |

O modelo do Arandu é único na combinação de: mesma linguagem, mesmo compilador,
tipos reflexivos de alto nível, memoização incremental e sistema de effects.

---

## 8. Questões em Aberto

1. **API de reflection exata**: quais campos `Struct`, `Function`, `Enum` expõem?
   Os nomes, tipos, visibilidades e anotações existentes de campos precisam de
   especificação formal antes da implementação.

2. **Atributos em expressões**: `@Inline` poderia ser aplicado a uma expressão
   de closure ou a um bloco? O modelo atual cobre apenas declarações de nível
   de módulo/item.

3. **Atributos condicionais**: `@Derive(comptime if target == "wasm" { Json } else { Cbor })`
   — é desejável? Qual a semântica de `apply` quando o argumento é comptime if?

4. **Ordem de atributos quando há dependência**: se `@Orm.Table` lê metadata
   injetada por `@Schema`, a ordem top-to-bottom é suficiente, ou é necessário
   um mecanismo de dependência explícita entre atributos?

5. **Bootstrapping exato**: quais atributos intrínsecos (`@Link`, `@Effects`,
   `@Packed`, `@Align`) permanecem hardcoded indefinidamente vs. migram para
   stdlib quando a API de reflection estiver disponível?

---

## 9. Possibilidades Futuras

### 9.1. Atributos em membros de struct

```arandu
struct Config {
    @Json(name: "max_retries")
    maxRetries: uint

    @Deprecated("use timeoutMs")
    timeout: uint
}
```

### 9.2. Atributos em parâmetros

```arandu
func register(@Validated user: User): Result<void, Err> { ... }
```

### 9.3. Composição de atributos

```arandu
// atributo que aplica outros atributos
comptime func ApiModel.apply(comptime T: Struct): void {
    Derive.apply(T, Debug, Eq)
    Json.apply(T, camelCase: true)
    Orm.Table.apply(T, T.name.toLower())
}

@ApiModel
struct User { id: uint  name: str }
```

### 9.4. Atributos sobre `impl` blocks

Quando `impl` blocks forem suportados como declaração de primeira classe,
atributos poderão anotar implementações completas de interface:

```arandu
@Override(Debug)
impl Debug for Point {
    func debug(shared self): str { "ponto" }
}
```

### 9.5. Verificação estática de capabilities de atributo

```arandu
// Editor e compilador podem avisar antes da execução:
// "@Orm.Table requires fsRead — declare @Effects(fsRead) no módulo caller"
```

---

## 10. Plano de implementação por fase

| Fase | Versão | O que é implementado |
|---|---|---|
| 0 | 0.1.9 | `@` intrínsecos hardcoded (`@Link`, `@Effects`, `@Test`, `@Deprecated` como metadata simples) |
| 1 | 0.2 | `type` como valor comptime, `T.name`, `T.fields()` — fundação da reflection |
| 2 | 0.3 | `Foo.apply(target, args...)` como protocolo; `@Derive` implementado em stdlib; `SymbolId::Generated` |
| 3 | 0.4 | Atributos de terceiros sem recompilação do compilador; `@Orm.Table`, `@Http.Get` |
| 4 | 0.5+ | Atributos em membros, parâmetros, `impl` blocks; composição de atributos |

---

*Esta RFC depende da implementação do comptime de tipos (RFC 0013, fase 0.2) como
pré-requisito. Nenhuma mudança de compilador é necessária na 0.1.9 para alinhar
com este design — os atributos intrínsecos atuais são compatíveis com o protocolo
futuro.*
