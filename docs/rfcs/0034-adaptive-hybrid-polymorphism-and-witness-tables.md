# RFC 0034: Polimorfismo Híbrido Adaptativo, *Witness Tables* e *Existential Containers* sem Heap Implícito

- **Número da RFC:** 0034
- **Título:** Polimorfismo Híbrido Adaptativo, *Witness Tables* e *Existential Containers* sem Heap Implícito
- **Status:** `Draft` (investigação `TYP.NEXT` / `GEN`; escopo depende de aceitação)
- **Data de Início:** 2026-10-15
- **Autor(es):** Arquiteto Da Linguagem Arandu
- **Escopo:** `arandu_parser`, `arandu_typeck`, `arandu_middle`, `arandu_mir`, `arandu_backend_cranelift`, `arandu_backend_c`, `arandu_backend_wasm`

> **Estado auditado em 2026-10-16:** interfaces estáticas e instâncias concretas com monomorfização existem. A composição runtime usa `FunctionInstance` estrutural e unidades por instância; não há `dyn Interface`, VWT/IWT, existential container, polymorphization adaptativa ou witness-passing implementados por esta RFC. Fontes: [interfaces](../../crates/arandu_typeck/src/type_checker/types/interfaces.rs), [instanciação](../../crates/arandu_query/src/runtime.rs), [composição](../../crates/arandu_query/src/runtime/program.rs), [tipos](../../crates/arandu_middle/src/types/ar_type.rs) e [AMIR](../../crates/arandu_middle/src/amir/value.rs).

O [roadmap mestre](../arandu-compiler-roadmap-v0.1.md) é a única fila permanente:
`0.2.x` motor quente; `0.3.x` granularidade por instância; `0.4.x` otimização e
LLVM opcional se aceito; `0.5.x` contratos ABI; `0.6.x` ferramentas/cache.
Isso não reserva `dyn` para `0.3.x` nem aprova esta RFC. Layouts, atributos,
sintaxe e pseudocódigo abaixo são propostas, sem ABI estável ou ganhos medidos.

---

## 1. Resumo Executivo

Polimorfismo pode trocar especialização por compartilhamento de código, com
custos de build, tamanho, indireção e representação. Monomorfização não implica
custo quadrático nem impede toda compilação separada; um valor heterogêneo não
exige heap em todos os desenhos possíveis. Existenciais inline são uma opção
cujo custo/segurança precisam de medição e contrato próprios.

A proposta investiga o **Polimorfismo Híbrido Adaptativo**:
- **Genéricos Estáticos (`fn foo<T: Interface>(x: T)`) com *Polymorphization* e *Witness Table Sharing* (`GEN`)**: Propõe-se especializar funções quentes/pequenas e investigar compartilhamento de implementação parametrizada por `(ValueWitnessTable, InterfaceWitnessTable)` para funções genéricas grandes/frias onde o tipo `T` é manipulado de forma uniforme, buscando reduzir duplicação sem prometer eliminar code bloat ou preservar toda otimização.
- **Visões Existenciais Emprestadas (`ref dyn Interface` e `mut ref dyn Interface`)**: Representação candidata de **2 palavras de ponteiro** (`(data_ptr, iwt_ptr)`), sem alocação requerida pela coerção de uma referência já válida; lifetime/exclusividade ainda precisam ser comprovados.
- **Containers Existenciais Lineares (`own dyn Interface`) sem Heap Implícito**: O layout candidato usa cinco palavras, com capacidade/alinhamento por alvo, e armazena valores de até 3 palavras (`24 bytes` em 64-bit) no storage inline do container (*Small Object Optimization* — SOO). Se um domínio específico precisar de um buffer inline maior sem usar heap, uma opção proposta é anotar a declaração da interface ou `type` alias com `@InlineSize(64)`, sob contrato explícito de identidade/layout. Mesmo quando omitido da grafia de uma assinatura, o tamanho faz parte do tipo/ABI; não pode variar silenciosamente entre módulos.

---

## 2. Motivação e Lições de Outras Linguagens

### 2.1. Hipóteses de memória e compartilhamento

A política pretendida rejeita coerção owned que não caiba no container em vez
de alocar implicitamente. Isso não torna todo código sem heap: o container pode
estar numa coleção alocada explicitamente, e o payload pode possuir recursos.
Retorno por valor pode usar sret/memória segundo a ABI, sem garantia de stack ou
registradores. Referências emprestadas e ownership inline são contratos distintos.

### 2.2. Comparações externas

A [referência Rust de trait objects](https://doc.rust-lang.org/reference/types/trait-object.html)
distingue objetos sem tamanho conhecido e referências/vtables. Ela não implica
heap obrigatório para toda estratégia de armazenamento heterogêneo. Esta RFC
não mede escape em Go, representação Swift ou bloat de outro compilador; essas
comparações não fundamentam ganho de produto Arandu.

---

## 3. Design Técnico Detalhado

### 3.1. Regras de *Object Safety* (Segurança de Existencial)

Uma `interface I` no Arandu pode ser usada como tipo existencial (`dyn I`) se e somente se satisfizer as regras de **Segurança de Despacho Dinâmico** a serem implementadas em `arandu_typeck`:
1. **Receptor Explícito Despachável**: Todo método de `I` (exceto métodos marcados com `comptime` ou restritos a `where Self: Sized`) deve receber `self: ref Self`, `self: mut ref Self` ou `self: own Self` como primeiro parâmetro.
2. **Ausência de `Self` por Valor em Posições Covariantes/Contravariantes sem Limite**: Um método despachado dinamicamente não pode receber outro argumento do tipo `Self` (pois o chamador de `dyn I` não sabe se dois `dyn I` possuem o mesmo tipo concreto em tempo de compilação) nem retornar `Self` por valor, a menos que retorne um existencial `own dyn I`.
3. **Ausência de Parâmetros de Tipo Genéricos no Método**: Métodos individuais despachados via *Witness Table* não podem introduzir novos parâmetros `<U>` de tempo de execução (métodos `comptime` continuam permitidos apenas em contexto estático `<T: I>`).

No desenho proposto, `dyn I` inválido deverá produzir um diagnóstico estruturado apontando exatamente qual método impede o despacho dinâmico.

---

### 3.2. Representação em Memória e ABI de `dyn Interface`

A proposta distingue entre **empréstimo existencial** (2 palavras) e **propriedade existencial inline** (5 palavras por padrão), com representação/ABI ainda a validar. `DataLayout`/`TargetInfo` determinam
ponteiro, alinhamento, padding e escalares; os tamanhos do desenho não
substituem layout por alvo:

#### 3.2.1. Empréstimos Existenciais: `ref dyn I` e `mut ref dyn I` (2 Palavras)
Para referências emprestadas, o valor já reside em storage válido do chamador. A coerção proposta deve preservar a origem e janela do empréstimo, inclusive retorno, reborrow e chamadas indiretas:

```text
+-----------------------+-----------------------+
| data_ptr: *const ()   | iwt_ptr: *const IWT_I |  (16 bytes em 64-bit / 8 bytes em 32-bit)
+-----------------------+-----------------------+
```
- **Forma candidata da coerção**: empacota o endereço da referência existente e o ponteiro para a tabela estática imutável `IWT` (*Interface Witness Table*) do par `(ConcreteType, Interface)` em storage estático do alvo. Não inclui custo de produzir o payload nem prova latência do produto.
- **Integração OSSA**: Um `ref dyn I` ou `mut ref dyn I` carrega exatamente a mesma `LoanWindow` (`F2.2`) que uma referência comum `ref T` / `mut ref T`. A implementação precisará rastrear o owner concreto e a região após erasure; callbacks, escape, mutabilidade e retorno emprestado devem manter a mesma restrição. O desenho da tabela isolado não fornece essa prova.

#### 3.2.2. Container Existencial Linear: `own dyn I`
Quando o código transfere a **propriedade linear (`own`)** de um valor heterogêneo sem alocação implícita para a coerção (ex.: retornar um iterador ou handler polimórfico por valor, ou armazenar uma lista de comandos na pilha), propõe-se um **Container Existencial Inline**:

```text
Layout candidato ilustrativo para ponteiros de 8 bytes e alinhamento 8:
+-----------------------------------------------------------------------+
| inline_storage: [usize; 3]  (24 bytes em 64-bit / 12 bytes em 32-bit) |
+-----------------------------------+-----------------------------------+
| vwt_ptr: *const ValueWitnessTable | iwt_ptr: *const IWT_I             |
+-----------------------------------+-----------------------------------+
```

```arandu
// Proposta: retorna container por valor; passagem física depende da ABI.
func criarFiltro(modo: Modo): own dyn AudioFilter {
    if modo == Modo.PassaBaixa {
        return FiltroPassaBaixa { corte: 440.0 }
    }
    return FiltroPassaAlta { ganho: 1.5 }
}
```

- **Política proposta de capacidade e alinhamento sem fallback heap implícito**:
  - Ao converter um valor `v: own ConcreteType` para `own dyn I`, `arandu_typeck` verifica **em tempo de compilação** (usando `DataLayout` / `TargetInfo`):
    $$\text{sizeOf}(\text{ConcreteType}) \le \text{InlineCapacity}(I) \quad \land \quad \text{alignOf}(\text{ConcreteType}) \le \text{InlineAlign}(I)$$
  - Se `ConcreteType` exceder a capacidade ou alinhamento do alvo, **o compilador NUNCA chama `malloc` nos bastidores**. Em vez disso, emite um erro de compilação com duas sugestões acionáveis (*quick fixes*):
    1. Anotar a declaração da interface (ou de um `type` alias) com `@InlineSize(64)` se o domínio exigir objetos maiores na stack.
    2. Alocar explicitamente no heap (em ambientes `S-Host` / `S-Std`): `own Box<dyn I>`, se essa forma e seus destruidores forem aceitos em contrato próprio; não presumir duas palavras como ABI universal.

---

### 3.3. Tabelas de Testemunhas: `ValueWitnessTable` (VWT) e `InterfaceWitnessTable` (IWT)

A proposta separa metadados em tabelas imutáveis, sem adicionar vptr ao payload concreto. Placement `.rodata`, relocações e identidade de tabela dependem do alvo/linker; deduplicação e carregamento dinâmico exigem contrato, sem usar igualdade de endereços como identidade universal de tipo.

#### 3.3.1. `ValueWitnessTable` (VWT) — Operações de Ciclo de Vida OSSA
Proposta por tipo concreto que participa de polimorfismo dinâmico ou genérico compartilhado:
```c
typedef struct ArValueWitnessTable {
    size_t size;                  // Tamanho em bytes no TargetInfo
    size_t stride;                // Passo em arrays alinhados
    size_t align;                 // Alinhamento em bytes
    void (*drop_in_place)(void* slot);              // Destrutor OSSA (NULL se trivialmente descartável)
    void (*move_initialize)(void* dst, void* src);  // Movimentação linear bitwise ou customizada
} ArValueWitnessTable;
```
- Se um tipo não possui destrutor nem campos com limpeza (`drop_in_place == NULL`), o descarte de `own dyn I` ao sair de escopo pode omitir a chamada se o contrato permitir; a forma emitida e o custo precisam de inspeção, sem promessa por construção.

#### 3.3.2. `InterfaceWitnessTable` (IWT) — Despacho de Métodos da Interface
Proposta para cada satisfação estática de interface, preservando as regras estruturais atuais; `impl Interface for ConcreteType` seria sintaxe adicional a avaliar, não mecanismo já existente:
```c
typedef struct ArInterfaceWitnessTable_Drawable {
    const ArValueWitnessTable* vwt; // Ponteiro direto para a VWT do tipo concreto
    // Ponteiros para thunks dos métodos da interface na ordem canônica de declaração:
    void (*draw)(const void* self_ptr, Canvas* canvas);
    Rect (*bounds)(const void* self_ptr);
} ArInterfaceWitnessTable_Drawable;
```
- **Suporte a Super-Interfaces (*Upcasting*)**: Se `interface Sub: Base`, a `IWT_Sub` inclui no cabeçalho um ponteiro estático para `IWT_Base`, permitindo converter `ref dyn Sub` em `ref dyn Base` por acesso à tabela base proposta; superinterfaces múltiplas exigem resolver identidade e coerção, sem garantia universal de uma única indireção.

---

### 3.4. `GEN` — Polimorfismo Híbrido Adaptativo (Monomorfização vs. *Witness-Passing*)

Para funções genéricas estáticas, propõe-se investigar especialização e compartilhamento. A decisão pode exigir cooperação entre queries/semântica/MIR/codegen; não mover tipagem ou identidade de instância para um passe MIR:

1. **Caminho Rápido — Monomorfização + Devirtualização (Padrão para Funções Pequenas/Quentes)**:
   - Se o corpo da função na AMIR tem custo estimado $\le \text{INLINE\_THRESHOLD}$ (ex.: $\le 60$ instruções AMIR), ou se a função usa `T` em laços internos de cálculo numérico/vetorizável (`@Kernel`, arrays contíguos `[N]T`), o compilador gera uma instância especializada para `ConcreteType`.
   - Chamadas com alvo conhecido podem ser diretas; inlining/vetorização ainda dependem do workload, efeitos e passes admitidos.
2. **Caminho Compacto — *Polymorphization* e *Witness-Passing* (Contra *Code Bloat*)**:
   - **Análise de Polimorfização (inspirada em `rustc` polymorphization)**: Se uma função auxiliar interna (ou um sufixo de função) recebe `<T>` mas não depende do layout nem dos métodos de `T` (ex.: manipula apenas metadados após extrair um ponteiro ou comprimento), pode ser compartilhada após provar independência de layout, drop, efeitos, identidade de tipo e atributos; extrair sufixo exige CFG/ABI próprios. Não basta ignorar campos no texto.
   - **Compartilhamento por *Witness Table* em Funções Grandes**: O perfil atual possui `--size`; qualquer política futura de threshold deve ser medida. Quando uma função genérica grande manipula `T` exclusivamente por referência (`ref T` / `mut ref T`) e chamadas de métodos de `Interface`, o compilador pode emitir um único corpo polimórfico que recebe um parâmetro implícito `iwt: *const IWT_Interface`, se o corpo e sua ABI de retorno/cleanup forem independentes do tipo concreto. Sem essa prova, manter monomorfização.
   - **Devirtualização Especulativa Guiada por Perfil (PGO)**: Quando um ponto de chamada via `dyn Interface` ou *Witness Table* é dominado por 1 ou 2 tipos concretos no perfil, um experimento PGO poderia emitir guarda e fallback semanticamente equivalente (`if iwt_ptr == &IWT_FastType { FastType::method(...) } else { call_indirect(...) }`).

---

### 3.5. Representação na AMIR

Para manter os backends simples ($M + N$), as seguintes operações conceituais devem ser avaliadas; três variantes não bastam por si só para tipagem, verificação e ABI:
- `Rvalue::PackExistential { value: Operand, vwt: StaticSymbol, iwt: StaticSymbol, inline_size: u32 }`: Constrói um `ref dyn I`, `mut ref dyn I` ou `own dyn I`.
- `Rvalue::ExtractWitnessMethod { existential: Place, method_index: u32 }`: Extrai o ponteiro de dados e o ponteiro de função da `IWT`.
- Chamada indireta tipada proposta (a AMIR atual possui `AmirStmt::Call`, não `Terminator::CallIndirect`), coordenada com a [RFC 0032](0032-closures-ossa-capture-modes-and-first-class-functions.md): invocaria método ou destrutor `drop_in_place` da `VWT`.

---

## 4. Invariantes Arquiteturais e Guardrails

1. **Proibição Absoluta de Alocação Implícita em `dyn Interface`**:
   - Nenhuma conversão para `dyn Interface` pode invocar o alocador global. Em modo `@Freestanding` (`S-Core`, [RFC 0017](0017-lean-freestanding-core-architecture.md)), a admissão dependerá de subset, layout/alinhamento e drop dos tipos concretos, suporte do backend e runner. Freestanding não certifica todos os containers ou métodos por consequência.
2. **Determinismo de Layout de `IWT` e Early-Cutoff em Salsa**:
   - A ordem proposta dos slots deve ser canônica, com política para métodos herdados/diamonds e alterações de versão, nunca baseada na ordem de compilação. Não acrescentar metadata runtime ao hash de exports por conveniência. Uma edição de corpo que preserve assinatura/layout/contratos públicos deve preservar consumidores independentes; mudanças de efeitos, borrow summaries ou closure podem exigir invalidação. Testar cutoff antes de prometer não invalidação universal.

---

## 5. Questões de Contrato e Gates

- **Object safety:** completar associados, métodos genéricos, Self, consts,
  superinterfaces/diamonds, receptores consumidores e retorno emprestado. Um
  `own Self` exige thunk que invalide a origem e execute cleanup exatamente uma
  vez. Retornar outro existencial também precisa caber no container destino.
- **ABI e identidade:** decidir capacidade/alinhamento como parte do tipo,
  compatibilidade entre módulos e upcast sem perda de owner, versionamento de
  VWT/IWT, calling convention dos thunks e fronteira de exportação. O exemplo
  IWT já contém ponteiro VWT: duplicá-lo no container é escolha a justificar.
- **Move/drop:** garantir estado uninitialized/moved, alinhamento, overlapping
  moves e proibição de cópia bitwise de payload não-Copy. Mover container pode
  invalidar ponteiros internos; tipos não relocáveis/self-borrow exigem rejeição
  ou representação própria. Size/stride não garantem operação válida.
- **Compartilhamento:** thresholds de 60 instruções e buffers de três palavras
  são hipóteses, não políticas aprovadas. Medir compile time, RSS, tamanho,
  execução e call overhead antes/depois. PGO é opt-in; perfis incompatíveis e
  alvos desconhecidos mantêm caminho correto e determinístico.
- **Incrementalidade e alcance:** chaves canônicas incluem tipo concreto,
  interface, layout/ABI, métodos, drops e estratégia de compartilhamento; nunca
  endereço runtime de tabela. Tabelas/thunks viram raízes explícitas no alcance
  e CGU; testar clean/incremental e mudança de instância sem invalidar irmãs.
- **IR e backends:** variantes novas atualizam visitors, hashing, DCE/liveness,
  move/borrow, CTFE-admission e os três backends atuais. Não supor que coordenar
  com a RFC 0032 entregue closures ou calls indiretas automaticamente.
- **Validação:** corpus positivo/negativo de tamanho/alinhamento, no-heap,
  lifetime/escape, reborrow/upcast, consumo/drop, arrays e chamadas; ABI em runners
  nativos e paridade C/Cranelift/Wasm. O tier dinâmico só é liberado após contrato
  aceito; esta revisão documental não executa nem certifica esses gates.

## 6. Referências

- [Rust trait objects](https://doc.rust-lang.org/reference/types/trait-object.html)
  e [dyn compatibility](https://doc.rust-lang.org/reference/items/traits.html#dyn-compatibility):
  referências de desenho, sem copiar suas regras como contrato Arandu.
- [Tipos atuais](../../crates/arandu_middle/src/types/ar_type.rs),
  [AMIR atual](../../crates/arandu_middle/src/amir/stmt.rs) e
  [instâncias runtime](../../crates/arandu_query/src/runtime.rs): fronteiras a
  evoluir, sem witness tables existentes.
