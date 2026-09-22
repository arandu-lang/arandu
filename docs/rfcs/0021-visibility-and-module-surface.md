# RFC 0021: Visibilidade Granular e Superfície de Módulo

- **Número da RFC:** 0021
- **Título:** Visibilidade Granular e Superfície de Módulo (`internal`, `sealed`, re-exports)
- **Autor(es):** Bruno ([@arandu-lang](https://github.com/arandu-lang))
- **Data de Início:** 2026-09-21
- **Status:** `Draft`
- **Área Principal:** `Frontend` / `Middle-end` / `Tooling`
- **PR da RFC:** —
- **Issue de Acompanhamento:** —

---

## 1. Resumo (Summary)

Esta RFC especifica a evolução do sistema de visibilidade do Arandu de binário
(`public` / implicitamente privado) para quatro níveis ortogonais — `public`,
`internal`, `module` (default) e `private` — além de `sealed interface`,
re-exports e submódulos inline. O objetivo é habilitar o modelo de encapsulamento
necessário para projetos colaborativos e corporativos: APIs estáveis para
consumidores externos, APIs ricas para parceiros dentro do mesmo pacote e
detalhes de implementação inacessíveis fora do arquivo.

---

## 2. Motivação (Motivation)

### Problema atual

O Arandu tem hoje dois níveis de visibilidade:

- `public` — símbolo exportado pelo módulo, visível a qualquer importador.
- implicitamente privado — acessível apenas dentro do arquivo `.aru` corrente.

Isso cria um gap para código colaborativo/corporativo:

1. **Sem package-private.** Não existe "visível dentro do pacote mas não fora".
   Toda API interna que precisa ser compartilhada entre módulos do mesmo pacote
   é forçosamente pública para o mundo.

2. **Sem hierarquia fechada.** Interfaces não podem impedir que implementadores
   externos as satisfaçam, o que impede padrões como `sealed class` (Kotlin/Swift)
   e sum types simulados com interfaces.

3. **Sem re-exports.** Um módulo fachada não pode importar símbolos de módulos
   internos e re-expô-los como sua própria API, forçando o código a ser
   estruturado de acordo com a organização de arquivos em vez do design de API.

4. **Sem submódulos inline.** Arquivos grandes não podem ser organizados em
   namespaces internos sem criar arquivos separados.

### Resultado se não implementado

Projetos corporativos em crescimento ficam presos entre duas opções ruins:
publicar APIs internas como `public` (poluindo a superfície e criando
compromissos de compatibilidade involuntários) ou duplicar código para evitar
compartilhamento entre módulos.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Os quatro níveis de visibilidade

```arandu
module self.math.vector

// public — visível a qualquer importador externo
public func dot(a: Vec3, b: Vec3): f32 { … }

// internal — visível dentro do mesmo pacote (self.*), não fora
internal func dotUnchecked(a: Vec3, b: Vec3): f32 { … }

// (padrão, sem keyword) — visível apenas neste módulo/arquivo
func dot_impl(a: Vec3, b: Vec3): f32 { … }

// private — visível apenas no bloco/escopo declarante (campos, helpers locais)
struct Vec3 {
    public x: f32
    public y: f32
    public z: f32
    private _norm_cache: f32   // inacessível mesmo dentro do módulo
}
```

| Keyword | Quem vê | Equivalente em outras linguagens |
|---|---|---|
| `public` | qualquer importador | `pub` (Rust), `public` (Java/Kotlin) |
| `internal` | módulos do mesmo pacote | `pub(crate)` (Rust), `internal` (C#), package-private (Java) |
| _(nenhuma)_ | módulo/arquivo corrente | `pub(super)` / `mod`-private (Rust) |
| `private` | bloco/struct declarante | `private` (Java/Kotlin), `fileprivate` (Swift) |

### 3.2 `sealed interface` — hierarquia fechada

```arandu
// math/src/expr.aru
sealed interface Expr {
    // Somente tipos do mesmo pacote podem implementar Expr.
    // O compilador pode exaustivamente verificar `match`.
}

public struct Num { public val: f32 }
public struct Add { public left: Expr, public right: Expr }

impl Num: Expr {}
impl Add: Expr {}
```

Código externo ao pacote pode usar `Expr` mas não pode implementá-la:

```arandu
// outro pacote — ERRO
struct MyExpr {}
impl MyExpr: Expr {}   // E: `Expr` é sealed; implementadores externos são proibidos
```

O compilador infere exaustividade de `match` sobre tipos `sealed` sem
`else`/`_ =>` obrigatório:

```arandu
func eval(e: Expr): f32 {
    match e {
        Num(n) => n.val
        Add(a) => eval(a.left) + eval(a.right)
        // sem `_` — exaustivo porque Expr é sealed e todos os casos estão cobertos
    }
}
```

### 3.3 Re-exports — fachada de módulo

```arandu
// math/src/lib.aru  (raiz do target lib)
module self.lib

// Re-exporta símbolos selecionados de módulos internos como se fossem locais
public use self.vector.{ Vec2, Vec3, dot, cross }
public use self.matrix.{ Mat4, identity, transpose }
// self.internal.simd_ops NÃO aparece aqui — permanece inacessível externamente
```

Consumidores importam da fachada sem conhecer a organização interna:

```arandu
import math as math

let v = math.Vec3 { x: 1.0, y: 0.0, z: 0.0 }
let d = math.dot(v, v)
```

### 3.4 Submódulos inline

Para organização de arquivos grandes sem criar arquivos adicionais:

```arandu
module self.renderer

// Submódulo inline — namespace independente dentro do mesmo arquivo
module pipeline {
    internal struct PipelineState { … }
    internal func buildPipeline(desc: PipelineDesc): PipelineState { … }
}

module commands {
    internal struct CommandBuffer { … }
}

// Uso interno
func render(state: pipeline.PipelineState, cmds: commands.CommandBuffer): void { … }
```

### 3.5 Diagnósticos

```
error[N008]: `dotUnchecked` is `internal` and cannot be used outside package `math`
  --> src/main.aru:12:5
   |
12 |     let d = math_internal.dotUnchecked(a, b)
   |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^ defined in `math`, accessible only within that package
   |
note: use the public API `math.dot` instead

error[N009]: cannot implement `sealed` interface `Expr` outside its defining package
  --> src/ext.aru:5:1
   |
 5 | impl MyExpr: Expr {}
   | ^^^^^^^^^^^^^^^^^^^^ `Expr` is sealed to package `math`
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 AST e Parser

O enum `Visibility` em [`arandu_parser/src/ast/decl.rs`](../../crates/arandu_parser/src/ast/decl.rs) evolui de:

```rust
pub enum Visibility {
    Private,
    Public,
}
```

para:

```rust
pub enum Visibility {
    /// `private` — restrito ao bloco/struct declarante.
    Private,
    /// Padrão (sem keyword) — restrito ao módulo/arquivo corrente.
    Module,
    /// `internal` — restrito ao pacote corrente (todos os `self.*`).
    Internal,
    /// `public` — sem restrição.
    Public,
}
```

`sealed` é um atributo de `InterfaceDecl`, não uma variante de `Visibility`:

```rust
pub struct InterfaceDecl {
    pub span: Span,
    pub visibility: Visibility,
    pub sealed: bool,          // `sealed interface`
    pub name: SmolStr,
    pub generic_params: …,
    pub items: …,
}
```

`public use` (re-export) é uma variante nova de `TopLevelDecl`:

```rust
pub enum TopLevelDecl {
    // …existentes…
    ReExport {
        span: Span,
        visibility: Visibility,
        path: ImportPath,
        items: SmallVec<[ReExportItem; 4]>,
    },
}
```

### 4.2 Resolução de nomes (`arandu_resolve`)

A função `is_public` em
[`collect.rs`](../../crates/arandu_resolve/src/name_resolution/collect.rs)
evolui para `effective_visibility(vis: Visibility, context: ResolveContext)`:

```rust
pub fn symbol_accessible(sym: &Symbol, from: ModuleId, pkg_map: &PackageModuleMap) -> bool {
    match sym.visibility {
        Visibility::Public   => true,
        Visibility::Internal => pkg_map.same_package(sym.module, from),
        Visibility::Module   => sym.module == from,
        Visibility::Private  => false, // nunca cruzável por resolução normal
    }
}
```

`exported_symbols` agora produz dois níveis separados:
- `ExportedPublic` — símbolos `public`, disponíveis para qualquer importador.
- `ExportedInternal` — símbolos `internal`, disponíveis apenas para módulos
  do mesmo `PackageId`.

Ambos participam do early-cutoff Salsa: uma mudança em `internal` de um módulo
invalida apenas os importadores dentro do mesmo pacote; não invalida importadores
externos.

**`sealed` no resolve:**

`impl T: SealedInterface` verifica `same_package(impl_site, interface_defining_module)`;
caso falhe, emite `N009SealedImplOutsidePackage`.

**Re-exports:**

`public use self.vector.{ Vec3, dot }` coloca os símbolos referenciados em
`ExportedPublic` do módulo declarante, com rastreamento de origem para
goto-definition e diagnósticos de ciclos.

### 4.3 `SymbolTable` e `Symbol`

```rust
pub struct Symbol {
    pub id: SymbolId,
    pub name: SmolStr,
    pub kind: SymbolKind,
    pub span: Span,
    pub scope: ScopeId,
    pub visibility: Visibility,      // ← substituição de is_public: bool
    pub lang_item: Option<LangItem>,
    pub is_sealed: bool,             // ← para interfaces sealed
}
```

`is_public: bool` é substituído por `visibility: Visibility` — a leitura de
`is_public` em qualquer ponto do compilador deve ser substituída por uma
comparação explícita de visibilidade.

### 4.4 Typeck (`arandu_typeck`)

Nenhuma nova regra de tipos. O typeck delega verificação de acessibilidade ao
resolver antes de sintetizar tipos. `sealed` é verificado no resolve; o typeck
assume que o check já passou.

Exaustividade de `match` sobre `sealed interface` é adicionada ao verificador
de exaustividade existente: o compilador conhece o conjunto fechado de
implementadores internos (coletados em `collected_sealed_impls`) e verifica
cobertura sem necessidade de `_`.

### 4.5 Salsa e early-cutoff

Dois novos accumulators em `arandu_middle/src/db.rs`:

```rust
#[salsa::accumulator]
pub struct PublicExports(ExportedSymbolTable);

#[salsa::accumulator]
pub struct InternalExports(ExportedSymbolTable);
```

A query `exported_symbols` de um arquivo retorna apenas `PublicExports`.
Uma nova query `internal_exports` retorna `InternalExports`, consultada
somente quando o importador pertence ao mesmo `PackageId`. Isso garante:

- Editar um símbolo `internal` não invalida importadores externos.
- Editar um símbolo `public` invalida importadores conforme hoje.

### 4.6 Novos DiagCodes

| Código | Mensagem |
|---|---|
| `N008InternalOutsidePackage` | símbolo `internal` usado fora do pacote declarante |
| `N009SealedImplOutsidePackage` | implementação de `sealed interface` fora do pacote |
| `N010ReExportNarrowing` | re-export com visibilidade mais restrita que o símbolo original (erro) |
| `N011CyclicReExport` | ciclo detectado em re-exports |

Cada código exige entrada em `DiagCode`, mapeamento, catálogo em
`docs/diagnostics/SPEC.md` e `docs/errors/<CODIGO>.md`.

### 4.7 LSP e IDE

- Completion filtra símbolos por `symbol_accessible(sym, current_module, pkg_map)`.
- Hover mostra a visibilidade efetiva: `[public]`, `[internal]`, `[module]`.
- Quick-fix para `N008`: sugere usar API pública alternativa quando disponível.
- Semantic tokens: `internal` recebe o modificador `defaultLibrary` como
  indicação visual; não são cores fixas — o tema decide.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### Invariantes afetados

| Invariante | Impacto | Preservação |
|---|---|---|
| `exported_symbols` early-cutoff | `internal` cria segundo nível de export | dois accumulators Salsa independentes; importadores externos não são invalidados por mudanças `internal` |
| Determinismo de diagnósticos | `N008`/`N009` dependem de `PackageId` | `PackageId` é computado deterministicamente antes da análise; sem HashMap instável |
| `SymbolId` monotônico | `private` em campos pode criar símbolos extras | IDs continuam alocados monotonicamente; visibilidade é metadata, não muda o ID |
| Queries puras | `symbol_accessible` lê apenas dados já em Salsa | sem I/O, sem mutação global |

### Custo de complexidade

- **Compilação:** O second-level export check acrescenta uma lookup por módulo
  importador quando o símbolo é `internal`. Custo O(1) com `PackageId` já no map.
- **Cognitivo:** Quatro níveis são mais para aprender. Justificado pelo ganho de
  expressividade; `private` em campos é análogo ao Java, familiar a programadores
  corporativos.
- **Superfície de invalidação:** Reduzida em relação ao estado atual — mudanças
  em `internal` deixam de invalidar importadores externos que hoje eram
  incorretamente invalidados (porque o símbolo era forçado a `public`).

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### Por que quatro níveis e não três?

- **Sem `private` em campos:** forçaria todo dado de struct a ser
  acessível no módulo, impossibilitando invariantes de struct sem getters.
- **Sem `internal`:** obriga autores a publicar APIs internas como `public`,
  criando compromissos de compatibilidade não intencionais.
- **Sem `module` (default):** a ausência de keyword sendo module-private é
  análoga ao Rust (`mod`-private por default) e elimina boilerplate.

### Alternativa: `pub(package)` em vez de `internal`

Rust usa `pub(crate)`. O Arandu preferiu `internal` por ser mais legível em
contextos corporativos (C#, Kotlin) e não introduzir a sintaxe de qualificador
de parênteses que é não-idiomática para Arandu.

### Alternativa: sem `sealed`, usar `@Sealed` anotação

Anotações são opacas para o compilador sem infraestrutura de proc-macro. `sealed`
como keyword de `interface` permite que o typeck e o verificador de exaustividade
consumam a informação sem parsing de atributos.

### Manter o status quo

Projetos crescem e expõem involuntariamente APIs internas como `public`. O único
caminho de mitigação hoje é estruturar cada detalhe em arquivos separados e
confiar na disciplina — o que não escala em times.

---

## 7. Arte Prévia (Prior Art)

| Linguagem | Mecanismo | Lição para Arandu |
|---|---|---|
| **Rust** | `pub(crate)`, `pub(super)`, `pub(in path)` | Granularidade alta; sintaxe de parênteses é poderosa mas verbosa |
| **Kotlin** | `internal`, `private`, `protected`, `public` | `internal` é exatamente package-scoped; excelente modelo |
| **Java** | package-private (default), `protected`, `public`, `private` | Default package-private funciona bem em times pequenos; `protected` não se aplica a Arandu sem herança de structs |
| **Swift** | `internal` (default), `public`, `open`, `private`, `fileprivate` | `fileprivate` ↔ `module` de Arandu; `open` permite subclassing — não aplicável |
| **C#** | `internal`, `public`, `private`, `protected internal` | `internal` é o modelo mais próximo do uso corporativo |
| **Go** | maiúscula = exportado, minúscula = package-private | Elegante mas perde legibilidade explícita |
| **Zig** | `pub` / privado; sem package-private hoje | Mesmo gap que Arandu tem hoje |

**`sealed`:**
- Kotlin: `sealed class` — exaustividade de `when` garantida.
- Swift: `indirect enum` + `protocol` — fechamento parcial.
- Java 17+: `sealed class/interface` — implementadores declarados explicitamente.
- Arandu adota o modelo Kotlin por ser mais conciso e não exigir declaração
  explícita de implementadores (o compilador os descobre nos módulos do pacote).

---

## 8. Questões em Aberto (Unresolved Questions)

1. **`protected`?** Vale a pena um quinto nível (`protected` = acessível em
   tipos que satisfazem a interface) quando Arandu não tem herança de structs?
   Adiado: depende de como interfaces com implementação default evoluem.

2. **Re-exports transitivos.** `public use math.{ Vec3 }` em `lib.aru` reexporta
   para consumidores. Se eles re-exportam novamente, o rastreamento de origem
   para goto-definition precisa de profundidade. Limite a ser decidido.

3. **`internal` em parâmetros genéricos.** Um tipo `internal` pode aparecer na
   assinatura de uma função `public`? Provavelmente um aviso ou erro — a assinatura
   seria inutilizável externamente. Definir o diagnóstico exato.

4. **Migração de `is_public: bool` → `Visibility`.** Todos os sites do compilador
   que leem `is_public` precisam ser atualizados. A mudança é mecânica mas grande.
   Estratégia: PR de migração isolado antes das novas features.

5. **`private` em campos de struct vs. `module`.** Hoje campos sem keyword são
   acessíveis em todo o módulo. Introduzir `private` em campos implica que o
   módulo corrente não pode mais acessar campos default. É isso que queremos?
   Alternativa: campos sem keyword = module-accessible; `private` = struct-only.

---

## 9. Possibilidades Futuras (Future Possibilities)

- **`pub(in self.geometry)`** — visibilidade restrita a um submódulo específico,
  análogo ao `pub(in path)` do Rust. Útil para pacotes com hierarquia profunda.
- **Visibilidade em re-exports com narrowing:** `internal use` re-exporta
  para o pacote sem expor publicamente.
- **`@Opaque` em structs públicos** — expõe o tipo como identidade opaca (sem
  campos acessíveis) para bindings de FFI com layout controlado.
- **Editor/LSP:** painel de superfície de API mostrando o que um módulo exporta
  `public` vs. `internal`, com diff ao editar declarações.
- **`arandu doc --scope=internal`** — gerar documentação da API interna para
  consumo por times parceiros dentro de um monorepo.
