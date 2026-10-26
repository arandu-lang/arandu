# RFC 0035: Efeitos, polimorfismo contextual e autoridade por capacidades

- **Número da RFC:** 0035
- **Título:** Efeitos, polimorfismo contextual e autoridade por capacidades
- **Status:** `Draft`; expansão A2/capabilities proposta na trilha 0.5.x
- **Data original:** 2026-10-15
- **Revisão de escopo:** 2026-10-16
- **Autor:** Arquiteto da Linguagem Arandu
- **Escopo:** `arandu_middle`, `arandu_typeck`, `arandu_query`, MIR e tooling

## 1. Estado atual e intenção

O [contrato de efeitos](../arandu-effects-v0.1.md) é a referência para a base
implementada e seus limites. O compilador já representa `EffectFlags`, observa
operações e efeitos conhecidos de chamadas, verifica o recorte declarado e
registra efeitos de funções. Isso não constitui o modelo completo de inferência
transitiva, polimorfismo de callbacks ou autoridade por capacidades desta RFC.

A proposta preserva três dimensões: autoridade externa, recursos de execução e
propriedades semânticas. Seu objetivo é permitir contratos verificáveis e
resumos públicos estáveis, sem exigir anotações em cada chamada comum. As
formas de anotação novas, DTOs, queries e diagnósticos discutidos aqui são
design, não APIs já disponíveis.

O [roadmap mestre](../arandu-compiler-roadmap-v0.1.md) concentra a fila: 0.2 é
motor quente; instâncias/closures e reflexão delimitada têm trilhas condicionais
em 0.3; capabilities/inferência ampliada pertencem à direção 0.5. Esta RFC não
transforma essas direções em promessa de implementação integral por versão.

## 2. Fundação implementada

[`EffectFlags`](../../crates/arandu_middle/src/effects.rs) é um bitset `u32`
com união/diferença e nomes determinísticos. A API atual contém `Net`,
`FileRead`, `FileWrite`, `Environment`, `Process`, `Foreign`, `Heap`, `Blocking`,
`Suspend`, `Thread` e marcadores `Pure`, `Readonly`, `NoAlloc`, `NoThrow`,
`NoSuspend`. Não existe atualmente o `EffectSet::PURE = 0` do pseudocódigo
original: `NONE` é zero e `PURE` é um bit distinto. Os bits/nomes antigos desta
proposta não podem substituir silenciosamente essa representação.

O [type checker](../../crates/arandu_typeck/src/type_checker/check/func.rs)
compara efeitos observados com o recorte declarado, emite `T039` e atualiza
`TypeInfo::function_effects`. O corpus
[`effect_system.rs`](../../crates/arandu_query/tests/effect_system.rs)
exercita a implementação. A presença desse mecanismo não comprova fixpoint
interprocedural completo em todos os ciclos, imports e instâncias, nem uma
classificação exaustiva de toda operação implícita de backend/runtime.

### CTFE e unsafe

A admissão [CTFE](../../crates/arandu_mir/src/ctfe/vm/admission.rs) e seus
budgets possuem contrato próprio. A VM valida a closure retida e as operações
admitidas; uma anotação de pureza sozinha não autoriza executar FFI, filesystem,
rede ou uma operação não suportada durante avaliação. Não existe acesso
ambiental ao host concedido por marcar uma função como pura.

`unsafe` permite operações de memória que exigem prova manual; não concede
capacidade de abrir arquivos, usar rede ou criar processos. `Foreign` registra
uma chamada externa, mas não demonstra a segurança ou a autoridade efetiva da
implementação estrangeira. O modelo ampliado precisa tratar desconhecidos de
forma conservadora; `UnknownCapability` ainda não é um nome reconhecido por
`EffectFlags::from_name`.

## 3. Design proposto

### 3.1 Três dimensões e pureza

| Dimensão | Pergunta do contrato |
| --- | --- |
| Autoridade | A função usa arquivos, rede, ambiente, processos ou autoridade externa desconhecida? |
| Recursos | A operação pode alocar, bloquear, suspender ou criar trabalho concorrente? |
| Semântica | Há mutação observável, não determinismo ou falhas que limitem reordenação/especulação? |

Mutação local não deve ser confundida automaticamente com autoridade externa.
Porém, modificar um argumento `mut ref` é observável pelo chamador: não torna a
função referencialmente transparente nem justifica CSE/LICM de chamadas por si
só. Bounds checks, traps, alias, ownership e ordem dos efeitos continuam
obrigações separadas. `Pure`, `Readonly` e `NoThrow` precisam de definição por
operação e alvo; não são certificados gerais de terminação ou ausência de OOM.

### 3.2 Polimorfismo contextual de callbacks

A proposta pretende compor os efeitos intrínsecos de uma função de ordem
superior com os dos callbacks efetivamente invocados. Um `map` que aloca mantém
o efeito de heap mesmo quando o callback é puro; um callback que lê arquivos
acrescenta essa autoridade ao resumo da chamada.

A representação ainda deve definir callbacks armazenados, chamadas indiretas,
capturas, genéricos e desconhecidos. Uma máscara fixa de 16 parâmetros não é
solução aprovada para aridades arbitrárias. A sintaxe hipotética de restrição
de callback e os métodos de coleção dos exemplos originais dependem dos
contratos de closures e tipos, incluindo a
[RFC 0032](0032-closures-ossa-capture-modes-and-first-class-functions.md).

### 3.3 Inferência e cutoff

O desenho separa uma observação local por item de resumos interprocedurais.
Footprints devem conter efeitos diretos e callees em ordem determinística;
uma edição sem mudança do resumo pode permitir cutoff. Mudanças de corpo que
alteram efeitos observáveis devem invalidar os consumidores corretos.

Ciclos exigem propagação monotônica até fixpoint sobre um domínio conservador e
finito. Tarjan pode organizar SCCs, mas não há garantia de convergência em duas
iterações. O contrato deve cobrir ciclos, callbacks/externs desconhecidos,
instâncias e causalidade. Todas as queries permanecem em `arandu_query`, sem
I/O, e resumos não devem projetar/deep-clonar o programa inteiro.

Anotações explícitas limitam e verificam o resumo; não podem fixá-lo de forma a
esconder comportamento. A fronteira exportada deve manter identidade e hash
estáveis apenas quando o contrato não mudou.

### 3.4 Diagnósticos e integração com otimizações/IDE

O diagnóstico ampliado deve explicar a operação e uma cadeia causal
reproduzível de chamadas. Nomes como `E_EFFECT` do exemplo inicial eram
placeholders; nenhum novo código é reservado por esta RFC. Qualquer diagnóstico
novo exige `DiagCode`, catálogo e documentação em inglês, além de regressões.

Hover e actions podem apresentar resumos compreensíveis. Isso exige DTOs
estruturados, spans reais e testes de protocolo/editor. O otimizador não deve
inferir `ReadOnlyArgs` apenas de um bit de pureza: precisa conservar efeitos de
memória, alias, potencial de trap e argumentos observáveis, conforme a
[RFC 0033](0033-amir-cooperative-optimization-place-provenance-and-memoryssa.md).

### 3.5 Capabilities e enforcement

Objetos úteis de domínio, como o `Dir` proposto na
[RFC 0016](0016-capability-safe-filesystem-and-path-resolution.md), podem
representar autoridade delimitada. Essa API ainda não existe; o filesystem
atual é baseado em caminhos e o VFS não é sandbox.

Inferência estática e políticas de manifesto podem descrever/limitar contratos,
mas não confinam automaticamente código nativo, FFI ou uma dependência hostil.
A implementação precisa definir a fronteira confiável e o enforcement no
runtime/SO, incluindo o tratamento de autoridade desconhecida. Parsear uma
chave de manifesto ou registrar dados no lockfile não implementa essa barreira.
`unsafe` tampouco pode servir como substituto ou bypass de autoridade.

## 4. Guardrails e critérios de evolução

Preservar pureza das queries, cutoff por item, ordenação determinística,
convergência e resumos públicos canônicos. A eficiência de união de bits não
prova custo constante da inferência nem ausência de alocação no pipeline.
Novos efeitos/restrições devem ter semântica e testes por target; promessas de
`@Kernel`, eBPF, freestanding e real-time dependem de suas propostas próprias.

A fila permanece no roadmap. A ampliação exige evidência de propagação e
unknown conservador, diagnósticos causais, equivalência incremental/clean e
política efetivamente aplicada, separando inferência de isolamento do host.

## 5. Referências e Literatura Base

1. **Brachthäuser, J. I., Schuster, P., & Ostermann, K. (2020)** — *Effects as Capabilities: Effect Handlers and Lightweight Effect Polymorphism*. ACM OOPSLA (base do polimorfismo contextual de efeitos sem variáveis de efeito explícitas).
2. **Odersky, M., Boruch-Gruszecki, A., Lee, E., Lhoták, O., & Brachthäuser, J. I. (2022–2024)** — *Scoped Capabilities for Polymorphic Effects / Capture Checking in Scala 3*. EPFL.
3. **Leijen, D. (2014–2017)** — *Koka: Programming with Row Polymorphic Effect Types & Type Directed Compilation of Row-Typed Algebraic Effects*. Microsoft Research / POPL.
4. **Miller, M. S. (2006)** — *Robust Composition: Towards a Unified Approach to Access Control and Concurrency Control (Object-Capability Model)*. PhD Thesis, Johns Hopkins University.
