# RFC 0005: Sistema de Queries Incrementais (Salsa) para o Compilador Arandu

- **Número da RFC:** 0005
- **Título:** Sistema de Queries Incrementais (Salsa) e o 6º Invariante de Identidade Única
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-07-05
- **Status:** `Implemented` no recorte de análise incremental
- **Área Principal:** Incrementalidade / Compiler Infrastructure (`A1`)
- **Substitui:** o desenho anterior preservado no histórico Git

## 1. Resumo

Salsa memoiza análise orientada a demanda e preserva early-cutoff quando uma
saída relevante permanece igual após edição. `arandu_query` possui a execução
Salsa; resolução, tipagem, análises MIR e backends mantêm seus algoritmos puros.
LSP usa snapshots e inputs versionados; cada build CLI batch ainda cria DB nova.
Motor quente e granularidade de produto pertencem aos marcos 0.2/0.3 do
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md).

Implementação e testes de um recorte não provam invalidação O(1), latência
universal ou avaliação mínima de todos os blocos. Essas propriedades exigem
workload, instrumentação e contrato próprios.

## 2. Identidades e ownership

`FileId`, `SymbolId { file_id, local_id }`, IDs de blocos/tipos e chaves
estruturais de instância/CTFE conservam suas identidades semânticas. IDs
internos de memoização Salsa não as substituem por offsets ou hashes efêmeros.
`DocumentId` geracional e `AnalysisRevision` são identidades de buffers/handles
LSP distintas de `FileId`/`SymbolId`.

A exceção declarativa é [`arandu_middle/src/db.rs`](../../crates/arandu_middle/src/db.rs):
inputs/accumulator compartilhados e `SourceDatabase`, sem providers. Os
providers e `DatabaseImpl` pertencem a `arandu_query`. Nenhum consumidor deve
recriar uma segunda DB ou um alocador paralelo de IDs para evitar essa fronteira.

## 3. Inputs, queries e cutoff

`SourceFile` declara identidade, `Arc<str>` e caminho. Alvo/layout, limites
CTFE, configuração de módulos/pacotes e listagens de diretório são entradas
explícitas; a análise não obtém esse estado lendo filesystem ou ambiente.
Registro, leitura e mutação pertencem a CLI/LSP/host.

`local_symbols`, `exported_symbols`, `internal_symbols`, declarações e corpos
por item são fronteiras deliberadas. Editar um corpo deve preservar o cutoff
do importador quando a superfície exportada permanece igual. Separar assinaturas,
efeitos/contratos observáveis e spans evita invalidar consumidores por dados
incidentais, sem omitir dependências reais.

O texto anterior atribuía `salsa::Durability::HIGH` automaticamente à stdlib e
vendor. Esse comportamento não é uma garantia implementada neste workspace;
o cutoff atual decorre dos inputs e resultados efetivamente rastreados. Uma
política de durability futura precisa de fontes, teste e benchmark próprios.

## 4. Granularidade de dataflow e ciclos

[`dataflow.rs`](../../crates/arandu_query/src/dataflow.rs) publica
`liveness_facts` por função e `block_dataflow_facts` por bloco. As saídas Salsa
são resumos compactos/hash-estáveis, como cardinalidades live-in/live-out,
definite-init/move e quantidade de statements. Não são todos os bitsets ou uma
prova semântica baseada apenas em contagens.

Os algoritmos completos vivem em `arandu_mir`: worklists resolvem os ciclos do
CFG até fixpoint. A query de bloco projeta/combina resultados dessas análises;
não chama recursivamente uma query para cada predecessor. Recursão Salsa ingênua
sobre backedges não substitui uma análise cíclica convergente.

```text
AMIR da função
    → análise MIR pura por worklist/fixpoint
    → resumo por função/bloco
    → memoização/cutoff e consumidores IDE
```

Uma edição pode executar novamente uma análise por função mesmo quando o
resumo de um bloco irmão permanece igual e corta consumidores posteriores.
Não se promete que editar bb3 executará somente bb3. Os testes
[`block_delta`](../../crates/arandu_query/tests/block_delta.rs) e
[`item_body_cutoff`](../../crates/arandu_query/tests/item_body_cutoff.rs)
congelam recortes observados de recomputação e equivalência.

## 5. Liveness, ownership e backends

Borrow/move checking e transformações MIR compartilham algoritmos de liveness
e visitors canônicos. A query Salsa `liveness_facts` contém cardinalidades e
não entrega os registradores físicos ao Cranelift. O backend possui suas
próprias análises/alocador; não há garantia de um único bitset memoizado
compartilhado entre Salsa e a alocação física de registradores.

O manager de análises cooperativas mais amplo é proposta da
[RFC 0033](0033-amir-cooperative-optimization-place-provenance-and-memoryssa.md),
separada da memoização entre revisões Salsa.

## 6. Determinismo e snapshots

Queries preservam entradas/dependências, ordenação canônica e saída hash-estável.
Ordenar somente file/start/code não basta para provar determinismo de todos os
labels, notes e replacements; a cadeia inteira deve preservar essas informações
e ser exercitada com contagens diferentes de threads.

Workers analisam snapshots. A thread de mutação não pode conservar snapshot da
mesma DB durante `set_text`; revisões e DocumentId condicionam publicação. Limites
de jobs e canal de resultados têm contratos distintos, com pressão/cancelamento
e shutdown exercitados separadamente. O detalhe vivo está na
[arquitetura Salsa/LSP](../arandu-salsa-lsp-architecture-v0.1.md).

## 7. Evidência e evolução

As regressões de [arquitetura](../../crates/arandu_query/tests/architecture_invariants.rs),
[imports](../../crates/arandu_query/tests/salsa_imports.rs) e
[diagnósticos por item](../../crates/arandu_query/tests/ide_diag_delta.rs)
acompanham essas fronteiras. O gate de determinismo compara diagnósticos com
uma e oito threads; resultado histórico não valida mudanças posteriores.

Esta RFC implementada não entrega persistência do grafo Salsa, daemon, hidratação
binária de IR ou todos os gates p95. A fila permanente e as dependências ficam
no roadmap; este documento registra arquitetura e limites atuais.
