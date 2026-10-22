# RFC 0021: Visibilidade Granular e Superfície de Módulo

- **Número da RFC:** 0021
- **Título:** Visibilidade Granular e Superfície de Módulo (`internal`, `sealed`, re-exports)
- **Autor(es):** Bruno ([@arandu-lang](https://github.com/arandu-lang))
- **Data de Início:** 2026-09-21
- **Status:** `Implemented`
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

### Motivação histórica de 21/09/2026

Antes desta implementação, o Arandu tinha dois níveis de visibilidade:

- `public` — símbolo exportado pelo módulo, visível a qualquer importador.
- implicitamente privado — acessível apenas dentro do arquivo `.aru` corrente.

Isso motivou a evolução abaixo; não descreve mais o inventário atual:

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
public sealed interface Expr {
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
        Num { val } => val
        Add { left, right } => eval(left) + eval(right)
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
error[N016]: `dotUnchecked` is `internal` and cannot be used outside package `math`
  --> src/main.aru:12:5
   |
12 |     let d = math_internal.dotUnchecked(a, b)
   |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^ defined in `math`, accessible only within that package
   |
note: use the public API `math.dot` instead

error[N017]: cannot implement `sealed` interface `Expr` outside its defining package
  --> src/ext.aru:5:1
   |
 5 | impl MyExpr: Expr {}
   | ^^^^^^^^^^^^^^^^^^^^ `Expr` is sealed to package `math`
```

---

## 4. Implementação e evidência em 15/10/2026

### 4.1 AST, resolução e identidade

O parser preserva `Visibility::{Private, Module, Internal, Public}`,
`sealed interface`, `public use`/`internal use` e módulos inline.
`Symbol.visibility` substituiu o antigo `is_public`; a identidade de interfaces
seladas e implementações explícitas é armazenada em `SymbolTable` por IDs.
O pseudocódigo da proposta original não é uma API Rust pública adicional.

Fontes:
[`ast/decl.rs`](../../crates/arandu_parser/src/ast/decl.rs),
[`symbol_table.rs`](../../crates/arandu_middle/src/symbol_table.rs) e
[`name_resolution/collect.rs`](../../crates/arandu_resolve/src/name_resolution/collect.rs).

### 4.2 Exports e early-cutoff

`exported_symbols` e `internal_symbols` são queries tracked separadas em
[`arandu_query/src/passes.rs`](../../crates/arandu_query/src/passes.rs).
Importadores de outro pacote não recebem símbolos `internal`; o mapa de
pacotes registrado na sessão determina a identidade de pacote. As superfícies
públicas e internas participam dos contratos hash-estáveis do resolver.

Não existem accumulators `PublicExports`/`InternalExports` ou uma query pública
`internal_exports` conforme o pseudocódigo original. `arandu_middle/src/db.rs`
declara o trait compartilhado; `arandu_query` continua dono dos providers.
Re-exports retêm a identidade do símbolo de origem, com diagnósticos de
ampliação de visibilidade e ciclos. Esse mecanismo não autoriza descoberta de
filesystem dentro das queries semânticas.

### 4.3 Interfaces seladas e campos privados

`sealed` exige uma aresta explícita `impl Tipo: Interface`; satisfazer somente
os métodos estruturalmente não contorna essa exigência. O resolver verifica a
fronteira de pacote, e o type checker consulta as implementações registradas.
Exaustividade de `match` sobre o conjunto explícito de implementadores já tem
integração e regressões, incluindo implementadores em outro módulo do pacote.

Isso não introduz automaticamente valores existenciais `dyn`, vtables ou
representação geral de dispatch dinâmico. A promoção de uma nova forma de
interface/valor exige contrato próprio; reconhecer a exaustividade no typeck
não comprova execução de toda forma hipotética de interface.

Campos `private` são identificados por símbolo e `T041` cobre acesso,
inicialização, padrões e struct update fora dos métodos do tipo proprietário.
Campos sem modificador conservam a regra de acesso do módulo.

Fonte: [`types/interfaces.rs`](../../crates/arandu_typeck/src/type_checker/types/interfaces.rs).

### 4.4 Diagnósticos públicos

| Código existente | Contrato |
| --- | --- |
| `N016InternalOutsidePackage` | Uso de símbolo internal fora do pacote declarante |
| `N017SealedImplOutsidePackage` | Implementação de interface sealed fora do pacote |
| `N018ReExportNarrowing` | Re-export mais amplo que a visibilidade da origem |
| `N019CyclicReExport` | Ciclo de re-exports |

`N008`/`N009` eram nomes incorretos nos exemplos originais desta RFC e não devem
ser usados para essas regras. A fonte canônica é
[`DiagCode`](../../crates/arandu_diagnostics/src/codes.rs), com catálogo e
[catálogo de diagnósticos](../diagnostics/SPEC.md) correspondentes.

### 4.5 Regressões e fronteira de tooling

[`package_modules_p4.rs`](../../crates/arandu_query/tests/package_modules_p4.rs)
cobre exports internos por pacote, N016/N017, sealed explícito/exaustividade,
re-exports públicos/internos, N018/N019, módulos inline e namespaces de métodos.
[`cli_rfc0021_stdlib_facade.rs`](../../crates/arandu_cli/tests/cli_suite/cli_rfc0021_stdlib_facade.rs)
exercita a fachada real da stdlib com `public use`.
[`const_arguments.rs`](../../crates/arandu_query/tests/const_arguments.rs)
cobre argumentos comptime e dependências em membros de módulos inline.

Painel de API, quick-fix sugerindo uma alternativa pública e decoração específica
de `internal` são possibilidades de tooling; a implementação do resolver não
comprova que essas experiências estejam entregues. Tokens e apresentação devem
seguir o contrato LSP e as escolhas do tema.

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### Invariantes afetados

| Invariante | Impacto | Preservação |
|---|---|---|
| `exported_symbols` early-cutoff | `internal` cria segundo nível de export | queries tracked separadas; importadores externos não são invalidados por mudanças `internal` |
| Determinismo de diagnósticos | `N016`/`N017` dependem de `PackageId` | `PackageId` é computado deterministicamente antes da análise; sem HashMap instável |
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
- Arandu descobre as arestas explícitas `impl Tipo: Interface` registradas
  nos módulos do pacote; não há conformidade puramente estrutural para `sealed`.

---

## 8. Questões de evolução

As questões abaixo não alteram o estatuto implementado do recorte acima.
A fila executiva e novas decisões pertencem ao roadmap mestre.

1. **`protected`?** Vale a pena um quinto nível (`protected` = acessível em
   tipos que satisfazem a interface) quando Arandu não tem herança de structs?
   Adiado: depende de como interfaces com implementação default evoluem.

2. **Re-exports transitivos.** `public use math.{ Vec3 }` em `lib.aru` reexporta
   para consumidores. Se eles re-exportam novamente, o rastreamento de origem
   para goto-definition precisa de profundidade. Limite a ser decidido.

3. **`internal` em parâmetros genéricos.** Um tipo `internal` pode aparecer na
   assinatura de uma função `public`? Provavelmente um aviso ou erro — a assinatura
   seria inutilizável externamente. Definir o diagnóstico exato.

4. **Migração de `is_public: bool` → `Visibility`.** Resolvida nesta
   implementação: o campo duplicado foi removido e os consumidores foram
   migrados para comparações de `Visibility`.

5. **`private` em campos de struct vs. `module`.** Decidido: campos sem keyword
   permanecem acessíveis no módulo; `private` restringe a métodos do tipo que
   declara o campo.

---

## 9. Possibilidades Futuras (Future Possibilities)

- **`pub(in self.geometry)`** — visibilidade restrita a um submódulo específico,
  análogo ao `pub(in path)` do Rust. Útil para pacotes com hierarquia profunda.
- **`@Opaque` em structs públicos** — expõe o tipo como identidade opaca (sem
  campos acessíveis) para bindings de FFI com layout controlado.
- **Editor/LSP:** painel de superfície de API mostrando o que um módulo exporta
  `public` vs. `internal`, com diff ao editar declarações.
- **`arandu doc --scope=internal`** — gerar documentação da API interna para
  consumo por times parceiros dentro de um monorepo.
