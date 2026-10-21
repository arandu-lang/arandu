# RFC 0032: Closures, Modos de Captura sob OSSA e Funções de Primeira Classe (`LAMBDA`)

- **Número da RFC:** 0032
- **Título:** Closures, Modos de Captura sob OSSA e Funções de Primeira Classe (`LAMBDA`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-15
- **Status:** `Draft`
- **Área Principal:** `Frontend` / `Middle-end` / `OSSA` / `Backend`
- **Documentos Relacionados:**
  - [Roadmap mestre](../arandu-compiler-roadmap-v0.1.md)
  - [Modelo semântico de memória](../arandu-semantic-memory-model-v0.1.md)
  - [GenRef R0](0001-generational-fallback-genref.md)
  - [Paralelismo estruturado](0003-structured-parallelism.md)
  - [Proposta de memória semântica](0007-semantic-memory-model.md)
  - [Polimorfismo e witness tables propostos](0034-adaptive-hybrid-polymorphism-and-witness-tables.md)
  - [Efeitos e capacidades propostos](0035-effect-system-polymorphism-and-capability-authority.md)

> **Estado auditado em 2026-10-21:** proposta para avaliação no horizonte 0.3.x,
> com contrato próprio e aceitação ainda pendente. Não bloqueia a 0.2.
> O parser representa lambdas e o staging preserva seleção/escopo. A tipagem
> geral de closures produz U001 e o lowering AMIR mantém essa barreira.
> A exceção já baixada é `std.thread.spawn(move || ...)`, sem parâmetros e com
> capturas próprias Send: vira uma struct de captura e `run()`, não uma closure
> de primeira classe. Chamadas indiretas gerais na linguagem produzem T033.
> Tipos de função, `FunctionRef` e callbacks internos existentes não habilitam
> automaticamente closures executáveis ou funções de primeira classe de uso geral.

---

## 1. Resumo

Esta RFC propõe closures com capturas verificadas pelo OSSA e valores chamáveis
com ambiente explícito, preservando as fronteiras parser → resolve → typeck →
AMIR → backend. Sintaxe, classificação de chamadas e ABI abaixo são desenho
proposto, não APIs liberadas pela implementação atual.

Os objetivos são:

1. Parâmetros de callback não escapantes com escrita simples, proposta como
   `f: fn(A): B`, sem exigir anotação de armazenamento em cada chamada.
2. Capturas por caminho de projeção, distinguindo `Copy`, `Borrow`, `BorrowMut`
   e `Move`, sem enfraquecer as regras existentes de aliases e destruição.
3. Ambientes proprietários explícitos, com a grafia proposta `own fn(A): B`,
   para usos que sobrevivam à chamada original quando seus recursos permitirem.
4. Conversão de closures em ambiente e thunk tipados, com paridade demonstrada
   em C, Cranelift e Wasm antes de habilitar a superfície correspondente.

A referência de projeto inclui parâmetros não escapantes de Swift, captura
por projeção da Rust RFC 2229 e rastreamento de capturas/capacidades de Scala 3
Capture Checking e Effekt. Essas inspirações não provam a segurança ou os custos
da proposta Arandu.

## 2. Motivação

Callbacks que acessam estado local precisam preservar seu lifetime; ambientes
proprietários precisam destruir suas capturas exatamente uma vez. Um ponteiro
para código não representa sozinho essas duas obrigações. O objetivo é evitar
que a ergonomia esconda captura de recursos, mutabilidade, escape ou alocação.

Uma representação candidata para callbacks emprestados é `(code, env)`.
Quando o ambiente tiver tamanho conhecido e lifetime limitado, ele poderá usar
storage do chamador ou ser eliminado por especialização. Isso não garante
stack em todos os casos nem zero heap: tamanho, alinhamento, recursão, suspensão,
retornos e retenção precisam ser admitidos pelo contrato de armazenamento.
Inlining e desvirtualização são otimizações posteriores, não pressupostos de
correção nem garantias de O1/O2.

## 3. Guia proposto

Os exemplos desta seção ilustram intenção de sintaxe. Não são exemplos que o
compilador atual promete aceitar ou executar.

```arandu
func somar(numeros: []i32): i32 {
    let mut acumulado: i32 = 0
    numeros.forEach(n => { acumulado = acumulado + n })
    return acumulado
}

func criarSomador(base: i32): own fn(i32): i32 {
    return own (x: i32) => x + base
}
```

`fn(A): B` em posição de parâmetro propõe um callback cujo ambiente não pode
ser retido além do lifetime admitido. O callee não poderá retorná-lo, armazená-lo
em um objeto mais duradouro ou encaminhá-lo a outro parâmetro sem contrato
compatível. Suspensão e execução concorrente exigem provas adicionais; a palavra
“não escapante” não concede essas capacidades.

`own fn(A): B` propõe propriedade do ambiente e de suas capturas movidas. Isso
não implica que a closure só possa ser chamada uma vez: mover um `Buffer` para
o ambiente e apenas lê-lo pode permitir várias chamadas; consumi-lo durante a
chamada exige classificação consumível. Retornar uma closure proprietária não
transforma uma referência capturada em referência de lifetime ilimitado.

O prefixo proposto `own` força transferência de valores afins para o ambiente,
sem duplicar owners; campos Copy podem ser copiados conforme a política de
captura aceita. A proposta ainda precisa definir a interação entre essa grafia,
inferência e modificadores explícitos antes de congelar a gramática.

## 4. Contrato de capturas

### 4.1 Modos e comportamento observável

| Modo proposto | Conteúdo do ambiente | Obrigação |
| --- | --- | --- |
| `Copy` | snapshot do valor no momento da criação | tipo Copy e cópia sem duplicação de recurso afim; alterações posteriores do original não alteram o snapshot |
| `Borrow` | referência compartilhada ao lugar capturado | origem viva durante todos os usos; nenhuma mutação conflitante |
| `BorrowMut` | referência exclusiva ao lugar capturado | exclusividade pelo intervalo de liveness do ambiente e de seus usos derivados |
| `Move` | valor transferido para o ambiente | origem movida, drops elaborados uma vez e armazenamento que sobreviva a todos os usos |

Capturar um valor Copy por snapshot ou por empréstimo pode mudar o resultado
observável. A inferência não poderá trocar os dois apenas por custo de cópia.
A política padrão e as formas explícitas para escolher entre eles precisam de
aceitação e fixtures próprias.

Para cada uso externo, a análise deverá distinguir leitura, mutação, empréstimo
e consumo. Caminhos sobrepostos colapsam para um ancestral que preserve todas
as obrigações; não existe uma ordem linear de modos que resolva todo conflito
entre mover e emprestar. Campos comprovadamente disjuntos podem permanecer
separados. Índices dinâmicos, dereferences e aliases sem prova continuam
conservadores. Mover parcialmente um tipo com destrutor explícito permanece
sujeito à restrição existente de exigir o valor íntegro no destrutor.

### 4.2 Captura e capacidade de chamada

A propriedade do ambiente é distinta do modo de chamar:

- Leitura requer acesso compartilhado ao ambiente quando todos os usos permitirem.
- Mutação requer acesso exclusivo e impede chamadas concorrentes conflitantes.
- Consumo de captura não-Copy exige transferência na chamada; uma segunda
  chamada deve ser rejeitada, inclusive após joins e chamadas indiretas.

Cada função de ordem superior deverá declarar um contrato verificável para o
callback. A assinatura não pode prometer chamadas repetidas e depois aceitar
um ambiente consumível. Compartilhar ou transferir entre threads exige contrato
de recursos/capacidades; GenRef R0 é thread-confined e não resolve esse requisito.

## 5. Ambiente, storage e lowering propostos

### 5.1 Ambiente e lifetime

Cada lambda teria uma descrição de ambiente tipada e um thunk independente,
com assinatura conceitual `(env, args...) -> result`. A ordem de capturas deve
ser canônica; informações de origem e projeções precisam sobreviver até os
passes que verificam ownership e elaboram destruição.

Um ambiente local não escapante pode usar um local AMIR com `StorageLive`,
`StorageDead`, definite-init e drops. Resultados que apontem para capturas
precisam de contrato de retorno emprestado compatível com o lifetime do ambiente.
Uma referência derivada que continue viva também prolonga o empréstimo; “última
chamada da closure” isoladamente não determina quando liberar o ambiente.

Ambientes proprietários retornados podem precisar de transporte por valor,
destino fornecido pelo chamador ou container com allocator explícito. A escolha
ainda exige layout/ABI e prova de escape. Um par de duas palavras não contém
por si só tamanho, alinhamento e destrutor de qualquer ambiente apagado; essas
obrigações precisam de especialização ou metadata definida. Não há um limite
SOO de 24 bytes aceito por esta RFC.

Referências capturadas só podem escapar se suas origens sobrevivem ao destino,
com prova no contrato público. Não se converte automaticamente um empréstimo
inválido em GenRef. Uso explícito de handles existentes mantém o contrato R0 e
não permite devolver um ponteiro para frame ou shadow stack já encerrado.

### 5.2 Chamada e representação por backend

Uma operação de chamada indireta tipada é proposta, mas sua forma exata como
statement ou terminador ainda precisa de decisão de IR. A representação atual
usa `AmirStmt::Call`; esta RFC não declara uma variante `CallIndirect` existente.
Qualquer extensão precisa atualizar os visitors canônicos, validação SSA/OSSA,
liveness, DCE, efeitos, reachability, hashes, remapeamento, drop glue e backends.

| Backend | Caminho candidato | Prova exigida |
| --- | --- | --- |
| Cranelift | `call_indirect` com contexto oculto e assinatura de alvo | calling convention, argumentos/retornos agregados, ownership do ambiente e adapters host |
| C | struct de callback tipada com função e contexto | protótipo compatível, layout/alinhamento, aliasing e lifetime de contexto; sem converter todo tipo de ponteiro indiscriminadamente |
| Wasm | índice em tabela e `call_indirect` com tipo validado | assinatura/tabela, bounds, memória linear do ambiente e transferência antes de restaurar shadow stack |

O índice da tabela Wasm não é um ponteiro nativo. A forma `(code, env)` e suas
larguras são dependentes do alvo; não se promete um layout universal de duas
palavras para todo chamável proprietário. Funções sem captura podem usar um
adapter com contexto vazio, mas coerção e invocação públicas continuam pendentes.
A existência de chamadas/callbacks internos não demonstra essa superfície.

## 6. Incrementalidade, diagnóstico e gates

A origem de cada ambiente/thunk deve ser determinística e ligada ao owner,
preservando `SymbolId { file_id, local_id }`. Índices locais de lambdas precisam
de invalidação correta quando a estrutura do corpo muda; offsets ou contadores
globais dependentes de threads não são identidade suficiente de cache.

Edições de corpo sem mudança de superfície podem preservar `exported_symbols`.
Alterar tipo inferido, efeito ou contrato público de retorno/captura pode exigir
invalidação. Isso precisa de regressões clean/incremental, não de uma promessa
de que editar qualquer lambda nunca afetará importadores.

A habilitação de um recorte requer casos aceitos e rejeitados de captura Copy,
move, borrow compartilhado/exclusivo, projeções sobrepostas/disjuntas, retorno
emprestado, escape, drop sem chamada e após chamada consumível, branches, loops,
recursão e cancelamento de análise. A integração deve demonstrar paridade nos
backends admitidos e acompanhar clientes IDE. Novos diagnósticos precisam de
`DiagCode`, catálogo e docs; O010/O004 não são códigos reservados por esta RFC.

A fila e a decisão de versão pertencem somente ao roadmap. Runtime de closures
e admissão de chamadas indiretas em CTFE são contratos distintos: estabilizar
um deles não habilita o outro. A VM exigiria ambientes puros, identidades
congeladas e contabilidade compartilhada de fuel/frames/valores, sem ponteiros
do runtime ou do host.

## 7. Alternativas e arte prévia

Alocar todos os ambientes em heap, especializar todo callback ou usar um
callback apagado são alternativas com custos distintos. A proposta prefere
admitir caminhos delimitados e medir compile-time, tamanho e runtime antes de
escolher mecanismos globais. Nenhuma alternativa foi implementada ou rejeitada
formalmente apenas pela inclusão desta RFC.

As referências de desenho permanecem Swift (parâmetros não escapantes), Rust
RFC 2229 (capturas disjuntas), Scala 3 Capture Checking e Effekt (capturas e
capacidades). Comparações quantitativas ou alegações sobre outros compiladores
precisam de fontes e workloads próprios; não são gates de correção do Arandu.

## 8. Questões em aberto e possibilidades

- Gramática final de parâmetros não escapantes, ambientes proprietários e modos
  explícitos de captura, com política observável para snapshots Copy.
- Como expressar leitura, mutação e consumo no contrato de chamada sem esconder
  restrições exigidas pelo corpo da função de ordem superior.
- Representação de ambientes apagados, metadata de destruição, limites de
  storage e contratos de suspensão/threads/FFI.
- Recorte inicial de runtime e eventual admissão separada em CTFE.
- Especialização e desvirtualização com alvo comprovadamente conhecido: precisam
  de benchmark e preservação de efeitos/retornos. Uma chamada não equivale a um
  `Goto` simples; argumentos, continuação e ABI devem permanecer corretos.
