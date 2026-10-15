# RFC 0026: Convenções Canônicas de Nomenclatura e Gerenciamento de Recursos da Stdlib

- **Número da RFC:** 0026
- **Título:** Convenções Canônicas de Nomenclatura e Gerenciamento de Recursos da Stdlib
- **Autor(es):** Equipe Arandu ([@arandu-lang](https://github.com/arandu-lang))
- **Data de Início:** 2026-10-06
- **Status:** `Draft`
- **Área Principal:** `Stdlib` / `Frontend` / `Runtime`
- **PR da RFC:** N/A
- **Issue de Acompanhamento:** N/A

---

## 1. Resumo (Summary)

Esta RFC formaliza a gramática conceitual e as convenções de design de APIs para a Biblioteca Padrão (`stdlib`) da linguagem Arandu. Ela estabelece uma identidade coesa baseada em três pilares fundamentais:
1. **Convenções de Casing:** Adoção de casing previsível (tipos em `PascalCase`, callables em `camelCase`, módulos/arquivos em `snake_case`, constantes em `SCREAMING_SNAKE_CASE`).
2. **Gramática Semântica de Operações:** Vocabulário padronizado para construtores (`Type.new`, `Type.withX`, `Type.from`, `Type.fromX`), receptores de métodos (`value.method()`), conversões estruturadas (`asX`, `toX`, `intoX`), coleções (`len()`, `isEmpty()`, `clear()`, `push()`, `set()`) e operações falíveis explicitamente pareadas (`tryX()`).
3. **Ciclo de Vida Unificado e Fim do Double-Free:** Eliminação de métodos manuais de descarte de memória (`destroy()`) na API pública em favor do `drop` automático orientado a OSSA. Para recursos externos que possuem I/O e falha potencial (`File`, sockets), introduz-se o método explícito `close(): Result<void, IoError>` com semântica idempotente, onde `drop` atua como finalizador de salvaguarda sem risco de double-free.

---

## 2. Motivação (Motivation)

À medida que a biblioteca padrão do Arandu expandiu suas camadas (`core`, `alloc`, `std`), surgiram divergências de estilo e inconsistências de semântica que prejudicam a ergonomia do desenvolvedor e introduzem falhas de segurança:

1. **Inconsistência de Construtores:** Coexistem padrões funcionais e híbridos como `bitsetNew(n)`, `atomicBoolNew(v)`, `newChannel(cap)` e `Duration.fromSecs(s)`. Isso prejudica a autocompletação no LSP e quebra a expectativa do usuário.
2. **Fragmentação de Receptores:** Operações com receiver óbvio por vezes foram definidas como funções livres (`vec.get(ref v, i)` vs `v.get(i)`), poluindo o escopo de módulos e obscurecendo a descoberta de métodos na IDE.
3. **Ambiguidade em Métricas de Tamanho:** A convivência de `len()` e `lenBytes` em strings e coleções gera confusão sobre a unidade aferida (bytes vs. elementos vs. caracteres Unicode).
4. **Dupla Autoridade de Ciclo de Vida e Double-Free:** Estruturas de dados em `alloc` expunham tanto destruição manual (`vec.destroy()`) quanto o suporte a destruição automática por `drop` semântico do compilador. Ter dois mecanismos concorrentes para a mesma responsabilidade é a causa direta de bugs de double-free e use-after-free.
5. **Abreviações Opacas:** Nomes como `newCoreIoError` ou `errorFromPortable` introduzem ruído desnecessário, enquanto abreviações consagradas de sistemas necessitam de validação explícita.

Sem esta formalização, cada novo módulo da stdlib tende a criar seu próprio dialeto, degradando a consistência da linguagem antes da estabilização da versão 1.0.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

A identidade da API Arandu é resumida em um princípio cardeal:

> **"APIs no Arandu priorizam consistência semântica, navegabilidade via autocompletação e controle explícito de recursos com ciclo de vida inequívoco."**

### 3.1. Tabela de Convenções Canônicas

| Categoria | Regra | Exemplos Idiomáticos | Anti-padrões Rejeitados |
|---|---|---|---|
| **Tipos e Interfaces** | `PascalCase` | `HashMap`, `GenArena`, `IoError`, `BitSet` | `hash_map`, `io_error` |
| **Funções e Métodos** | `camelCase` | `push`, `withCapacity`, `charCount`, `tryReserve` | `with_capacity`, `push_val` |
| **Módulos e Arquivos** | `snake_case` | `hash_map.aru`, `gen_arena.aru`, `crypto` | `hashMap.aru`, `GenArena.aru` |
| **Constantes Globais** | `SCREAMING_SNAKE_CASE` | `MAX_CAPACITY`, `DEFAULT_BUFFER_SIZE` | `maxCapacity`, `MaxCapacity` |
| **Construtor Canônico** | `Type.new(...)` / `Type<T>.new(...)` (apenas args essenciais) | `Vec<int>.new()`, `HashMap<int, int>.new()`, `BitSet.new()`, `Instant.now()` | `bitsetNew(64)`, `vec.new<int>()` |
| **Construtor Configurado** | `Type.withX(...)` / `Type<T>.withX(...)` | `Vec<int>.withCapacity(16)`, `HashMap<str, User>.withCapacity(32)` | `Vec.newWithCap(128)`, `Vec.withCapacity<int>(16)` |
| **Conversão de Entrada** | `Type.from(x)` / `Type.fromX(x)` | `String.from("olá")`, `Duration.fromMillis(50)` | `durationFromSecs(10)`, `Duration.from(10)` |
| **Operação de Instância** | Método com receiver (`value.method()`) | `vec.push(x)`, `slice.len()`, `inicio.elapsed()` | `push(vec, x)`, `len(slice)` |
| **Conversão Emprestada** | `asX()` (zero-copy, view de referência) | `str.asBytes()`, `vec.asSlice()` | `str.bytes()`, `vec.toSlice()` |
| **Conversão com Cópia** | `toX()` (aloca/copia novo valor) | `str.toOwned()`, `num.toString()` | `str.asOwned()` |
| **Conversão Consumidora** | `intoX()` (move/consome `self`) | `buf.intoString()`, `vec.intoIter()` | `buf.consumeToString()` |
| **Métrica de Tamanho** | `len()` = contagem estrutural / bytes (O(1)) | `vec.len()`, `slice.len()`, `str.len()` | `lenBytes()`, `count()` |
| **Caracteres Unicode** | `charCount()` = Unicode scalar values (O(n)) | `str.charCount()` | `str.charLen()` |
| **Sequências vs Mapas** | Append com `push`, posicional com `set` / `get`, chave-valor com `put` | `vec.push(v)`, `vec.set(i, v)`, `map.put(k, v)` | `vec.put(i, v)`, `map.insert(k, v)` |
| **Falha Pareada** | Prefixo `tryX` pareado com `X` normal | `vec.reserve(n)` vs `vec.tryReserve(n)` | `File.tryOpen` (abrir já é falível) |
| **Abreviações Públicas** | Permitidas apenas se estabelecidas no domínio | `ptr`, `ref`, `len`, `cap`, `io`, `fs`, `os`, `tcp` | `newCoreIoError`, `mkPortErr` |

### 3.2. Construtores: `new`, `withCapacity`, `from` / `fromX` e Genéricos no Tipo

O parâmetro genérico pertence conceitualmente ao tipo que está sendo construído, não ao método construtor:
```arandu
let mut items = Vec<int>.new()
let mut prealloc = Vec<int>.withCapacity(16)

let mut scores = HashMap<int, int>.new()
let mut users = HashMap<str, User>.withCapacity(32)

let empty = String.new()
let buf = String.withCapacity(128)
let greeting = String.from("olá")
```

| Forma | Semântica |
|---|---|
| `new()` | Valor vazio ou padrão |
| `withCapacity(n)` | Valor vazio com capacidade pré-alocada |
| `from(x)` / `fromX(x)` | Construção/conversão a partir de outro valor |

- Utilize `Type.from(value)` quando o tipo de origem determinar inequivocamente a conversão:
  ```arandu
  let s = String.from("olá")
  let p = Path.from("/etc/hosts")
  ```
- Utilize `Type.fromX(value)` quando `X` carregar informação semântica ou unidade indispensável para a segurança e clareza da operação:
  ```arandu
  // Claro e inequívoco sobre a escala de tempo
  let timeout = Duration.fromSeconds(30)
  let retry = Duration.fromMillis(250)
  
  // Decodificação validada
  let text = String.fromUtf8(byte_slice)
  ```

### 3.3. Receptores: "Se Pertence a um Objeto, É Método"

Funções livres são reservadas unicamente para casos onde não existe um receptor semântico único evidente:
```arandu
// Funções livres válidas (operação sem dono único):
let m = math.min(a, b)
mem.swap(mut ref x, mut ref y)
let h = hash.combine(h1, h2)

// Operações sobre dados são métodos:
vec.push(42)
if !vec.isEmpty() {
    let first = vec.get(0)
}
```

### 3.4. Ciclo de Vida: Modelo Unificado Sem Double-Free

O Arandu estabelece três regras estritas para liberação de memória e recursos:

1. **Memória Gerenciada por Ownership (`Vec`, `String`, `HashMap`, etc.):**
   - O compilador insere a destruição automática (`drop`) ao final do escopo de vida da variável (OSSA).
   - O método `destroy()` é **removido da API pública**. A existência de dois mecanismos de destruição para a mesma entidade é proibida.
2. **Recursos Externos de Sistema com Falha (`File`, `TcpStream`, `DirListing`):**
   - Recursos que podem falhar durante finalização de I/O (ex.: `close()` ou `flush()` reportando erro do SO) expõem um método explícito:
     ```arandu
     pub func close(mut self): Result<void, IoError>
     ```
   - Se o chamador não invocar `close()`, o finalizador `drop` do escopo executa a liberação *best-effort* de salvaguarda de forma silenciosa.
3. **Garantia de Idempotência:**
   - Uma vez chamado `close()`, a transição de estado interna torna qualquer subsequente `drop` um *no-op* absoluto. Isso extingue o risco de *double-free* por construção.

```arandu
// Exemplo de uso de recurso externo:
func processFile(path: ref str): Result<void, IoError> {
    let mut file = File.open(path)?
    file.writeAll("dados importantes")?
    
    // Fechamento explícito capturando eventuais erros de flush/SO:
    file.close()?
    return Result.ok(())
    // Ao sair do escopo, o drop de file é um no-op seguro.
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1. Regra do Espelho e Consistência entre Coleções

Para assegurar previsibilidade no ecossistema, tipos com papéis análogos devem implementar o mesmo conjunto básico de nomes e assinaturas:

- `len(ref self): usize`
- `isEmpty(ref self): bool`
- `clear(mut ref self): void`
- `withCapacity(capacity: usize): Self`
- `reserve(mut ref self, additional: usize): void`
- `tryReserve(mut ref self, additional: usize): Result<void, AllocError>`

### 4.2. Eliminação de `lenBytes` e Semântica de `String.len()` vs `String.charCount()`

- Em coleções (`Vec<T>`, `[]T`, `[T; N]`): `len()` retorna a contagem de elementos do tipo `T` em O(1).
- Em texto (`ref str`, `String`): `len()` retorna a quantidade de **bytes UTF-8** em O(1), e **não** a quantidade de caracteres. Isso evita esconder uma travessia O(n) atrás de `len()` e é essencial para cálculos de buffers, limites de slicing e interoperabilidade de baixo nível.
- Para contagem de Unicode scalar values (`char`), utiliza-se exclusivamente `charCount()`, que deixa explícito o trabalho O(n) de decodificação UTF-8:
  ```arandu
  let s = String.from("olá")

  s.len()       // 4 bytes UTF-8 (O(1))
  s.charCount() // 3 Unicode scalars (O(n))
  ```
- O identificador histórico `lenBytes` é descontinuado e removido.

### 4.3. Semântica de `tryX`

O prefixo `try` é restrito a operações que possuem uma variante regular direta:
- `push(v)` (aborta/panica em OOM/falha fatal) ↔ `tryPush(v)` (retorna `Result<void, AllocError>`)
- `reserve(n)` ↔ `tryReserve(n)`

Funções cuja natureza é primariamente falível (como `File.open`, `Socket.bind`, `int.parse`) **não** recebem prefixo `try`, pois seu retorno já é intrinsecamente um `Result`.

### 4.4. Regras para o Compilador e Lints

Para acelerar a migração e impedir regressões, o compilador e ferramentas de análise estática incorporarão verificações estruturais:
- **Lint de Construtores Legados:** Emissão de aviso ou erro quando uma função pública coincidir com o padrão de sufixo `*New` (ex.: `bitsetNew`) ou prefixo `new*` fora de métodos estáticos.
- **Lint de Descarte Explícito:** Rejeição de métodos públicos chamados `destroy` em tipos possuídos que implementam semântica de `drop`.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

- **Early-Cutoff e Salsa:** A padronização de nomes melhora a estabilidade de interfaces públicas exportadas (`exported_symbols`), reduzindo hashes voláteis causados por APIs ad-hoc.
- **Segurança de Memória:** A eliminação de `destroy()` público fecha brechas graves de OSSA onde um valor destruído manualmente antes do fim do escopo poderia sofrer dereferência espúria ou double-drop.
- **Desvantagem / Impacto:** Exige refatoração de código cliente existente na stdlib e em testes que utilizavam funções como `bitsetNew`, `Vec.put` e chamadas manuais a `destroy()`.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

- **Alternativa 1 (Manter `destroy()` manual ao lado de `drop`):**
  - *Rejeitada:* Mantém viva a condição de corrida entre descarte manual e automático, violando os princípios de segurança de memória e OSSA do compilador.
- **Alternativa 2 (Abreviações totalmente banidas):**
  - *Rejeitada:* Banir siglas consagradas como `io`, `tcp`, `fs`, `utf8` ou `len` tornaria a linguagem prolixa em excesso sem ganho prático de legibilidade.
- **Alternativa 3 (Adotar Rust `snake_case` em funções):**
  - *Rejeitada:* O Arandu já tem seu ecossistema, ferramentas de syntax highlighting e base de código alinhadas com `camelCase` para métodos/funções (estilo Zig). A mudança de convenção de casing causaria atrito massivo sem ganho semântico.

---

## 7. Arte Prévia (Prior Art)

- **Rust RFC 430 (Finalizing naming conventions):** Estabeleceu a base de construtores `new`, `with_capacity`, convenções de conversão `as_`, `to_`, `into_` e ciclo de vida estrito.
- **Zig Naming Conventions:** Demonstrou com sucesso a combinação de `camelCase` para callables e `PascalCase` para tipos em linguagens de sistemas de alta performance.
- **Swift API Design Guidelines:** Reforça o papel de autocompletação guiada por métodos de instância em vez de funções livres genéricas.

---

## 8. Questões em Aberto (Unresolved Questions)

- Determinar se estruturas internas e alocadores de baixo nível ainda em migração devem manter temporariamente `@internal @Unsafe func destroy()` enquanto a cobertura geracional (GenRef) completa a transição para todos os targets.

---

## 9. Possibilidades Futuras (Future Possibilities)

- Implementação de um assistente de migração automatizado (`arandu fix`) capaz de reescrever chamadas obsoletas (`bitsetNew(n)` → `BitSet.new(n)`) com base nas novas convenções.
