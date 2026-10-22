# RFC 0033: Otimização Cooperativa na AMIR, Proveniência de Places (`AmirPlace`) e `MemorySSA` Virtual

- **Número da RFC:** 0033
- **Título:** Otimização Cooperativa na AMIR, Proveniência de Places (`AmirPlace`) e `MemorySSA` Virtual
- **Status:** `Draft` (proposta para a trilha revisável `0.4.x` / `OPT.1–7`)
- **Data de Início:** 2026-10-15
- **Autor(es):** Arquiteto Da Linguagem Arandu
- **Escopo:** `arandu_mir`, `arandu_middle`, `arandu_backend_cranelift`, `arandu_backend_c`, `arandu_backend_wasm`, `arandu_backend_llvm`

> **Estado auditado em 2026-10-16:** o `PassManager` atual seleciona O0/O1/O2/Os e executa fixpoint limitado a 100 iterações, com scratch `bumpalo` por função. O1 usa SCCP/DCE/SimplifyCFG; O2 contém field forwarding, GVN, SCCP, LICM restrito, DCE, tail merging e SimplifyCFG. A entrada checked O2 também executa scalar replacement Copy e self-tail calls restritos. Existe `LoopInfo` privado no LICM, não uma análise compartilhada de nesting. `AnalysisManager`, `PlaceAliasAnalysis`/`MemoryEffects` unificados, MemorySSA e promoção geral de heap continuam propostas. Fontes: [pipeline](../../crates/arandu_mir/src/pass_manager/pipeline.rs), [fixpoint](../../crates/arandu_mir/src/pass_manager/fixpoint.rs), [entrada checked](../../crates/arandu_mir/src/optimize.rs) e [LICM](../../crates/arandu_mir/src/licm.rs).

A única fila permanente é o [roadmap mestre](../arandu-compiler-roadmap-v0.1.md).
Esta RFC não altera seus marcos: `0.2.x` motor quente, `0.3.x` granularidade por
instância, `0.4.x` otimização com LLVM opcional se aceito, `0.5.x` ABI e `0.6.x`
ferramentas/cache. Não aprova passes nem promete ganho ou latência. O manager proposto dura
apenas a execução por função; análises auxiliares não são persistidas em Salsa. Snippets de
APIs abaixo são conceituais, não código existente/compilável no workspace.

---

## 1. Resumo Executivo

Para investigar melhorias de código nativo com semântica compartilhada entre o JIT diário (`arandu_backend_cranelift`) e os backends de produção (`arandu_backend_c`, `arandu_backend_wasm` e a proposta opcional LLVM da [RFC 0031](0031-llvm-mlir-backend-and-scoped-noalias-vectorization.md)), as otimizações de nível médio propostas atuam na **AMIR** (*Arandu Mid-level IR*).

Esta RFC formaliza o subsistema cooperativo de otimização da AMIR (`OPT.1–7`), estruturado sobre sete pilares:
1. **`AnalysisManager` Cooperativo e Arenas Efêmeras (`OPT.1`)**: Cache de análises por função (`CfgPreds`, `DomTree`, `LoopInfo`, `DefUseIndex`, `MemorySSA`) com invalidação seletiva via `PreservedAnalyses` e alocação temporária em `bumpalo::Bump`.
2. **Detecção de Laços Naturais (`LoopInfo`) e LICM (`OPT.2`)**: Identificação de *back-edges* dominados, cabeçalhos de laço e *preheaders*, habilitando *Loop-Invariant Code Motion* seguro quanto a especulação (`Rvalue::can_speculate()`).
3. **Proveniência de Places (`AmirPlace`) e `PlaceAliasAnalysis` (`OPT.3`)**: Elevação da semântica OSSA (`own`, `mut ref`, `ref`, `F2.2 LoanWindow`) para um oráculo unificado de disjunção de memória (`NoAlias`, `MustAlias`, `MayAlias`) e resumos `ModRef` por chamada (`MemoryEffects`).
4. **Motor Compartilhado de *Forward Dataflow* (`OPT.4`)**: engine extraída primeiro de definite-init/move-state, com worklist determinística, boundary/arestas e convergência. SCCP, ranges e não-nulidade só migram se seus contratos forem compatíveis e houver benefício medido.
5. **Canonização Algébrica Idempotente e GVN (`OPT.5`)**: Regras de simplificação sensíveis a tipo (`TargetInfo`) sem explosão combinatória, seguidas de *Global Value Numbering* sobre a `DomTree`.
6. **Otimizações de Memória em Camadas e `MemorySSA` Virtual (`OPT.6`)**: Experimento local ao bloco para DSE/*Load Forwarding*, sem ativação default antecipada e construção sob demanda de uma tabela lateral `MemorySSA` (*MemoryDef*, *MemoryUse*, *MemoryPhi*) para eliminação global de cargas/escritas redundantes.
7. **Recursão em Cauda (`Self-TCO`), *Tree-Shaking* e Promoção de Escape (`OPT.7`)**: Rebaixamento de auto-recursão em cauda para header explícito compatível com a ABI de entrada, poda por closure alcançável a partir de raízes explícitas e rebaixamento de alocações que não escapam para a pilha.

---

## 2. Motivação e Lições da Literatura / Outras Linguagens

### 2.1. Hipóteses a medir

Passes podem reconstruir estruturas que outros já calcularam, mas não há perfil
nesta RFC que atribua 40% do MIR a esse custo. Um manager pode economizar trabalho
e também reter memória/incrementar invalidação; medir construções, hits, CPU/RSS
e latência antes/depois é parte da decisão.

MemorySSA virtual mantém fatos auxiliares fora das instruções executáveis.
Sua presença não torna consultas de clobber O(1) em geral nem prova DSE. Regras
de canonicalização precisam convergir e preservar traps, efeitos e semântica
numérica; citar outro compilador não constitui evidência para Arandu.

### 2.2. Fronteira OSSA e alias

`AmirPlace` contém local e projeções, não uma prova universal de proveniência.
Locais distintos sem `Deref` podem ser storages distintos; depois de uma
indireção, o local contém um ponteiro, não identifica sozinho o objeto alvo.
Reborrows, campos que contêm ponteiros, globals e chamadas podem compartilhar
memória. Permissão de acesso por ownership não equivale a `NoAlias` entre todo
par de acessos. [Representação atual](../../crates/arandu_middle/src/amir/value.rs).

---

## 3. Design Técnico Detalhado

### 3.1. `OPT.1` — `AnalysisManager` Cooperativo, Invalidação Seletiva e Arenas `bumpalo`

Propõe-se estender o `PassManager` existente com cache por função. A scratch arena atual é reiniciada entre rodadas; análises retidas entre rodadas devem possuir armazenamento de duração adequada. Não manter referências penduradas após `Bump::reset`:

```rust
pub struct PreservedAnalyses {
    bits: u32,
}

impl PreservedAnalyses {
    pub const NONE: Self = Self { bits: 0 };
    pub const ALL: Self = Self { bits: u32::MAX };
    pub const CFG: u32        = 1 << 0; // CfgPreds, RPO order
    pub const DOM_TREE: u32   = 1 << 1; // DomTree, PostDomTree
    pub const LOOP_INFO: u32  = 1 << 2; // LoopInfo
    pub const DEF_USE: u32    = 1 << 3; // DefUseIndex
    pub const MEMORY_SSA: u32 = 1 << 4; // Virtual MemorySSA side-table
}
```

#### Grafo de Dependência e Invalidação Automática
O `AnalysisManager` impõe transitividade de invalidação:
- Se um passe não preserva `CFG` (ex.: `simplify_cfg` funde blocos ou remove arestas mortas), o gerenciador invalida automaticamente `DOM_TREE`, `LOOP_INFO` e `MEMORY_SSA`.
- Preservação é resultado da mutação efetiva. Reescrever operandos pode
  invalidar def-use, proveniência e MemorySSA mesmo com CFG intacto. Dobrar
  terminadores invalida CFG e dependências. Alterar IDs/ranges também conta.
- `MemorySSA` depende de CFG/dominadores e fatos de alias/efeitos. Unknown ou
  summary alterado invalida consultas dependentes. Testar invalidação transitiva
  com contagens e resultados, não igualdade incidental de endereços.
- **Convergência:** preservar o fixpoint limitado e o diagnóstico ICE atuais.
  Duas rodadas fixas não provam estabilidade; nova ordem/budget exige oráculo,
  detecção de oscilação e medição. Passes individuais/worklists também têm
  limites e cancelamento quando aplicável; não remover guardrails para ganhar tempo.

---

### 3.2. `OPT.2` — `LoopInfo` (Laços Naturais) e LICM Seguro quanto a Especulação

#### 3.2.1. Construção de `LoopInfo`
A análise compartilhada proposta partiria dos dominadores existentes para varrer todas as arestas $u \to h$ do CFG:
- Se $h$ domina $u$ ($h \text{ dom } u$), a aresta $u \to h$ é um **back-edge** e $h$ é um **cabeçalho de laço natural** (*loop header*).
- O corpo do laço $L(h)$ é fechado por busca reversa no CFG a partir de $u$ até encontrar $h$.
- Unir backedges do mesmo header; ordenar corpo/latches deterministicamente. Modelar nesting, parent/children e loop mais interno. CFG irredutível exige fallback conservador. Um predecessor externo único não prova preheader seguro: validar/splitar aresta, terminador e parâmetros/argumentos sem mudar semântica. [Terminologia LLVM](https://llvm.org/docs/LoopTerminology.html).

#### 3.2.2. *Loop-Invariant Code Motion* (LICM) e `Rvalue::can_speculate()`
O LICM atual move apenas operações não trapping sobre Copy e mantém divisões,
shifts, loads, endereços, moves e calls no lugar. Expandir esse recorte exige:

1. operandos dominando o destino e resultado com definição compatível;
2. invariância e execução especulativa segura **ou** prova de execução obrigatória
   que considere zero iterações, loops infinitos, saídas/traps anteriores e
   ordem de efeitos. Dominar exits isoladamente não prova must-execute;
3. preservar Move/drop, borrows, lifetime e efeitos. Div/mod exigem divisor
   não zero **e** tratamento de MIN/−1; overflow, casts, shifts e índices seguem
   o tipo/alvo, sem wildcard que classifique operação nova como segura;
4. para loads, provar validade/alinhamento no destino, ausência de Mod por todos
   os caminhos/calls, e respeitar free, atomics/volatile e fronteiras de trap;
5. reconstruir statements/ranges pela API central, mantendo `DenseRange` e
   `AmirStmtKind` coerentes. Não presumir push contíguo no último bloco.

`can_speculate` é contrato proposto type-aware, não o enum genérico ilustrado
como existente. Loads, refs e endereços não são automaticamente especuláveis.

---

### 3.3. `OPT.3` — Proveniência de `AmirPlace`, `PlaceAliasAnalysis` e `MemoryEffects`

Para evitar que cada passe (SROA, CSE, LICM, DSE, Backend LLVM) reimplemente regras ad-hoc sobre locais e projeções, propõe-se centralizar em `arandu_mir` a semântica de memória em dois contratos puros:

#### 3.3.1. Raiz de Proveniência (`ProvenanceRoot`) e Camadas de Consulta
Propõe-se uma origem consultável por ponto de programa, distinguindo storage
local, pointee de parâmetro, empréstimo derivado e origem desconhecida/escapada.
Uma classificação `ParamExclusive` não garante disjunção de todos os parâmetros;
`ParamShared` não garante imutabilidade física contra aliases/FFI desconhecidos.
Projeções após `Deref` requerem nova prova de objeto.

`alias(p1, p2, point)` retorna:

- `NoAlias` somente com storage/ranges distintos provados no mesmo domínio,
  tipos/layout válidos e ausência de indireção não resolvida. Campos distintos
  exigem não sobreposição; índices distintos exigem validade/tamanho e overflow
  tratados. Variantes/payloads e regiões de tamanho zero têm contrato próprio;
- `MustAlias` exige identidade de memória e extensão comprovadas; igualdade de
  sintaxe com índice/ponteiro mutável não basta;
- `MayAlias` em todos os demais casos, inclusive raw/escaped, relações derivadas
  não demonstradas e calls sem resumo.

`LoanWindow` pode refinar a consulta, mas um acesso não derivado do mesmo loan
não vira `NoAlias` automaticamente. Reborrows podem apontar para a mesma memória
em tempos distintos; a validade dos acessos deve ser mantida separada da consulta.
Não se promete complexidade somente pela profundidade da projeção.

### 3.3.2. Resumo `MemoryEffects` (`ModRef`) para Chamadas e Intrínsecos
O resumo proposto distingue reads/writes por argumentos, globais e memória
não acessível, além de alloc/free/drop, escape, I/O, atomics/volatile e possível
trap. Summaries recursivos/importados exigem fixpoint determinístico, tratamento
de ciclos e unknown conservador; o passe puro consome fatos, sem executar Salsa
ou filesystem. O default é `Unknown`/clobber conservador, inclusive chamada externa ou
indireta sem contrato; não mapear todos os externos para memória inacessível.
Máscaras de argumentos precisam ser dimensionadas, sem limite silencioso de 32.

Um endereço local não escapado pode permitir concluir que a call não alcança
seu storage somente se argumentos, callbacks, aliases derivados e lifetime
confirmarem essa prova. Isso não autoriza mover/remover efeitos da call nem
traps de avaliações anteriores.

---

### 3.4. `OPT.4` — Motor Compartilhado de *Forward Dataflow* em RPO

A primeira extração proposta parte de definite-init e move-state, uma análise por vez. O contrato cobre entry/exit, unreachable, join, transferência por bloco/aresta e argumentos de `Goto`/`Branch`/`SwitchInt`/`Suspend`. O trait abaixo é esboço incompleto; SCCP/ranges dependem de contrato específico:

```rust
pub trait ForwardDataflowLattice {
    type Value: Clone + PartialEq;
    fn bottom(&self) -> Self::Value;
    fn join(&self, a: &Self::Value, b: &Self::Value) -> Self::Value;
    fn transfer_stmt(&mut self, block: BlockId, stmt: &Stmt, state: &mut Self::Value);
    fn transfer_terminator(&mut self, block: BlockId, term: &Terminator) -> SmallVec<[(BlockId, Self::Value); 2]>;
}
```
- **Execução Conjunta (SCCP + Intervalos de Inteiros)**:
  - O reticulado produto rastreia simultaneamente se uma aresta de CFG é executável (`ExecutableEdge`) e se cada valor SSA é $\bot$ (`Unreached`), `Const(Scalar)`, `Range { min: i128, max: i128, non_zero: bool }`, ou $\top$ (`Overdefined`).
  - **Eliminação de Verificações de Limites (*Bounds Check Elimination*) e Divisão por Zero**: Uma condição `idx < len` fornece limite superior; para índice signed é preciso provar também `idx >= 0`, identidade de len/objeto e ausência de overflow. `d.non_zero` não cobre MIN/−1. Ranges seguem largura/sinal do alvo, convergência e widening/budgets quando necessário; checks só são removidos com todas as precondições provadas.

---

### 3.5. `OPT.5` — Canonização Algébrica Idempotente e GVN Dominador

#### 3.5.1. Regras de Canonização Estritamente Redutoras de Custo
Para evitar os ciclos infinitos de reescrita estudados na literatura de *InstCombine*, as regras propostas de `inst_simplify` deverão obedecer a uma **ordem bem-fundada estrita** (reduzem o número de instruções ou usam uma ordem canônica secundária que não se desfaça). Custo local não prova convergência entre passes; comparar snapshots/medidas de progresso e manter o guardrail global:
- **Comutatividade Canônica**: Em operações comutativas (`Add`, `Mul`, `BitAnd`, `BitOr`, `BitXor`, `Eq`, `Ne`), usa-se uma ordem total de operandos, incluindo constantes e IDs revisionais locais, sem usá-los como identidade persistente; os operandos são ordenados numa direção única, garantindo que `a + b` e `b + a` recebam o mesmo *Value Number*.
- **Identidades e Absorções Seguras por Tipo (`TargetInfo`)**:
  - Inteiros, somente com tipo/Copy e usos válidos, sem apagar avaliações observáveis de produtores: `x + 0 -> x`, `x - 0 -> x`, `x - x -> 0`, `x * 0 -> 0`, `x * 1 -> x`, `x & 0 -> 0`, `x & !0 -> x`, `x | 0 -> x`, `x ^ 0 -> x`, `x ^ x -> 0`.
  - Ponto Flutuante (`Float` / `f32` / `f64`): **Nunca** aplica `x + 0.0 -> x` (pois `-0.0 + 0.0 == +0.0`), `x * 0.0 -> 0.0` (pois `NaN * 0.0 == NaN` e `-1.0 * 0.0 == -0.0`) ou `x - x -> 0.0` (`Inf - Inf == NaN`), a menos que um contrato explícito permita outras regras. Usar a semântica IEEE e `TargetInfo.float_size` do tipo/alvo, sem fast-math implícito.
  - Potências de 2: `u * 2^k -> u << k`, `u / 2^k -> u >> k` e `u % 2^k -> u & (2^k - 1)` **somente** para tipos inteiros sem sinal (`uint`, `u8`..`u64`) ou com prova equivalente para signed. Multiplicação para shift ainda exige preservar overflow/trap, largura e valor de k válidos; divisão signed não se reduz por positividade isolada sem essas condições.

#### 3.5.2. *Global Value Numbering* (GVN) sobre a `DomTree`
Percorrendo a `DomTree` em pré-ordem com uma tabela hash com escopo lexical (`ScopedHashMap<ValueExpr, ValueId>`):
- Expressões puras idênticas dominadas por uma definição anterior em qualquer bloco ancestral são substituídas diretamente pelo `ValueId` dominante.
- Ao sair da subárvore de dominância de um bloco $B$, as entradas inseridas em $B$ são removidas; custo e retenção devem ser medidos, sem promessa O(1).

---

### 3.6. `OPT.6` — Otimizações de Memória em Camadas e `MemorySSA` Virtual

Para equilibrar velocidade de compilação no modo dev e qualidade máxima de código em `--release`, `OPT.6` é proposto em duas camadas:

1. **Camada 1 — Experimento Local ao Bloco (Ativação após gate)**:
   - Mantém um mapa pequeno `AvailablePlaces: SmallVec<[(AmirPlace, Operand, bool /* dirty */); 8]>` por bloco básico.
   - **Load-After-Store / Load-After-Load Forwarding**: Se `place` é escrito com `val` e lido logo adiante no mesmo bloco sem intervenção de escrita com `MayAlias`, a leitura só é substituída se valor/tipo/extensão forem compatíveis, `Copy` for permitido e valor/objeto continuarem válidos. Não transformar Move, atomic/volatile ou leitura trapping em cópia sem prova.
   - **Dead Store Elimination (DSE) Intra-Bloco**: Se `place` recebe duas escritas consecutivas no mesmo bloco sem leitura ou chamada com `MayAlias` entre elas, a primeira escrita só é removida com cobertura integral e ausência de observabilidade, drop/free, escape ou trap afetado. Escritas parciais e aliases desconhecidos bloqueiam a transformação.
2. **Camada 2 — `MemorySSA` Virtual em Tabela Lateral (Perfil de Otimização Global)**:
   - Sem adicionar nenhuma instrução nova ao tabela densa de statements da AMIR, constrói um grafo esparso de versões de estado de memória:
     - `LiveOnEntry`: Estado inicial da memória na entrada da função.
     - `MemoryDef { id, defining_stmt, clobbers_prev: MemoryVersionId }`: Associado a cada escrita em memória ou chamada com efeito `Mod`.
     - `MemoryUse { stmt, reaching_def: MemoryVersionId }`: associado à leitura e a uma versão de memória alcançável. O walker pode precisar atravessar defs e phis/ciclos para consultar clobbers conservadores; um único def dominante não representa todos os caminhos.
     - `MemoryPhi { block, args }`: Nos joins do CFG, com argumentos alinhados aos predecessores, incluindo backedges. O grafo pode conter ciclos por loops; construção e walkers precisam de conjuntos visitados/limites.
   - Fatos são locais à análise e não viram parâmetros executáveis, dumps ou
     chaves persistentes sem decisão explícita. Alterar IR/CFG/efeitos invalida a
     análise; atualização incremental exige validator próprio, senão reconstruir.
   - **Transformações candidatas**:
     - **Global Load Forwarding**: Se um `MemoryUse` de `p` resolve seu `reaching_def` para um `MemoryDef` dominante que escreveu `v` em um lugar `MustAlias` com `p`, a leitura pode ser substituída por `v` apenas com tipo/extensão, dominância, disponibilidade e semântica de acesso compatíveis. `MemoryUse` é dependência, não prova automática do valor lido.
     - **Global Dead Store Elimination (Pós-Dominância)**: Ausência de `MemoryUse` não prova store morto. Exigir sobrescrita completa ou fim de lifetime em todos os caminhos relevantes, sem leitura/escape/call/cleanup observável; cobrir saídas anormais, loops e atomics/volatile. Pós-dominância isolada não fornece todas essas provas.

---

### 3.7. `OPT.7` — Auto-Recursão em Cauda (`Self-TCO`), *Tree-Shaking* e Promoção de Escape

1. **Auto-Recursão em Cauda (`Self-TCO`)**:
   - Quando um bloco termina com `Call { callee: self_fn, args, dest }` seguido apenas de `Return(dest)`, e **nenhum** local `own` da ativação atual possui destrutor pendente nem teve seu endereço emprestado para `args`, o par `Call + Return` é transformado em:
     - Atribuição paralela dos novos `args` aos parâmetros do bloco de entrada `bb_loop_entry`.
     - `Goto` com argumentos alinhados aos parâmetros do header. Os nomes do esboço não são a sintaxe atual da AMIR.
   - Transforma recursão em cauda em um laço iterativo $O(1)$ de pilha, invalidando CFG/dominadores/loops/MemorySSA para recomputação antes de outros passes. O recorte atual é restrito; não garante TCO mútua nem zero spill.
2. **Promoção de Alocação Não-Escapante (*Escape Promotion*)**:
   - Se a análise de escape intra-procedural provar que uma alocação dinâmica ou caixa temporária não escapa por retorno, não é armazenada em estrutura externa e tem tamanho estático conhecido $\le 256\text{ bytes}$, ela é candidata a slot de frame somente após provar lifetime, alinhamento, cleanup, falha de alocação e comportamento em loops/suspensão. 256 bytes é hipótese de budget a medir, não limiar implementado; SROA não garante registrador físico.
3. ***Dead Function / Global Elimination* (*Tree-Shaking* no `AmirProgram`)**:
   - Já existe [alcance compartilhado](../../crates/arandu_mir/src/reachability.rs)
     de funções, callbacks, destruidores e raízes de tooling. A proposta amplia
     contratos de raízes/globais; não reimplementa esse recorte nem poda
     bibliotecas inspecionáveis supondo `main` obrigatório.
   - A partir das raízes exportadas (`main`, funções `@Export`/`@WasmExport`, futuras entradas de `InterfaceWitnessTable`, callbacks/destruidores e raízes de tooling), uma travessia em largura marca apenas as funções e constantes globais alcançáveis, podando funções auxiliares ou instâncias genéricas mortas antes de invocar Cranelift, C, Wasm ou LLVM.

---

## 4. Invariantes Arquiteturais e Guardrails

1. **Pureza e Determinismo em Salsa**:
   - Todo o pipeline `OPT.1–7` roda dentro de `arandu_mir` usando índices determinísticos (`BlockId`, `LocalId`, `IndexMap`/`BTreeMap` ou vetores indexados por ID denso). A ordem de iteração de `HashMap` nunca afeta a numeração de blocos ou locais.
2. **Validação SSA/OSSA Pós-Otimização**:
   - O caminho checked atual chama o validator a cada passe; a proposta preserva esse guardrail e amplia regressões em debug/teste, verificando que toda definição domina seus usos e que os argumentos de `Goto`/`Branch`/`SwitchInt`/`Suspend` permanecem estritamente alinhados aos parâmetros do bloco de destino.
3. **Sem Duplicação nos Backends**:
   - Como `OPT.1–7` opera na AMIR, backends existentes consomem a AMIR transformada, mas cada alvo continua exigindo paridade de ABI/efeitos e legalização. LLVM/eBPF/SPIR-V são propostas sem implementação; não recebem cobertura por consequência.

---

## 5. Gates e Referências

Antes de habilitar uma mudança: regressões de CFG/ciclos, dominância, block
params, metadata e visitors; diamonds, loops aninhados/irredutíveis, traps,
Copy/Move/drop, borrows e calls. Comparar O0/O1/O2 com oráculo independente e
backends atuais, sem atualizar snapshots em massa. O2 permanece experimental.
Medições devem separar compile-time, construções/hits de análises, CPU/RSS,
tamanho e execução. Nenhum ganho foi demonstrado por esta RFC.

- [LLVM MemorySSA](https://llvm.org/docs/MemorySSA.html): modelo virtual e walkers;
  não demonstra sozinho eliminação correta nem custo constante para Arandu.
- [LLVM Loop Terminology](https://llvm.org/docs/LoopTerminology.html): loops e
  formas canônicas como referência, não análise já compartilhada no workspace.
- [PassManager atual](../../crates/arandu_mir/src/pass_manager/mod.rs),
  [SCCP](../../crates/arandu_mir/src/sccp.rs),
  [GVN](../../crates/arandu_mir/src/gvn.rs) e
  [escape atual](../../crates/arandu_mir/src/escape_analysis.rs): base a preservar.
