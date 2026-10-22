# RFC 0006: Armazenamento Achatado de HIR com IndexVec

- **Número da RFC:** 0006
- **Título:** Armazenamento Achatado de HIR com Arenas por Função e IndexVec
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-06-25
- **Status:** `Planned` (proposta integral; pools/IndexVec precursores já implementados)
- **Área Principal:** `Middle-end` (`arandu_semantics` / `arandu_middle`)
- **Substitui:** `docs/arandu-hir-indexvec-rfc.md` (resgatada do histórico de commits)

---

## Fronteira da implementação

`arandu_middle::hir::HirPools` já usa `IndexVec`, e nós/blocos tipados são
referenciados por IDs. Isso não é trabalho a começar na 0.2. A proposta integral
de arenas/ownership por função e seus ganhos exige comparação com o produtor
por instância atual. Não reintroduzir whole-crate monolítico nem duplicar IDs;
perfilar antes de mudar representação. A ordem de produto vive no
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md).

O restante registra o desenho e a motivação histórica. Tipos de pseudocódigo
não substituem os IDs/estruturas atuais, nem constituem decisão de migração.

## 1. Resumo (Summary)

Esta RFC propõe a evolução da representação intermediária de alto nível (AHIR) do Arandu: substituir a árvore recursiva tradicional alocada no heap (`Box<HirExpr>`, vetores aninhados com chamadas profundas de `.clone()`) por um armazenamento contíguo e plano governado por índices tipados em vetores indexados ([`IndexVec`](https://github.com/rust-lang/rust/pull/83842)).

A proposta organiza o HIR em arenas `HirBody` por função, onde cada nó de expressão, declaração ou bloco é referenciado por um identificador compacto de 32 bits (`ExprId`, `StmtId`, `BlockId`, `ArmId`).

---

## 2. Motivação (Motivation)

A implementação inicial do AHIR (v0.1) utilizava nós alocados individualmente via `Box<HirExpr>`. Embora simples para testes iniciais de unidade e lowering em arquivo único:
1. **Fragmentação de Memória (*Memory Churn*)**: Compilar projetos grandes ou pacotes multi-módulo gera centenas de milhares de pequenas alocações no heap para nós intermediários de curta duração.
2. **Localidade de Cache Pobre**: Percorrer árvores com ponteiros indiretos (`Box`) sofre penalidades constantes de *cache misses* no pipeline de otimização e construção da CFG.
3. **Custo de Clonagem**: Passar árvores completas entre passes sem mutação força clones caros de árvores profundas.

Inspirado na transição histórica do Rustc do HIR monolítico para o **THIR com IndexVec**, esta proposta adota vetores densos indexados por IDs tipados de 32 bits.

---

## 3. Explicação em Nível de Referência (Reference-Level Explanation)

### Identificadores Tipados (Newtypes sobre `u32`)

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExprId(pub u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StmtId(pub u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArmId(pub u32);
```

### Estrutura Plana do `HirBody`

Em vez de variantes de enum contendo campos como `lhs: Box<HirExpr>`, as variantes contêm apenas referências aos IDs no vetor:

```rust
pub struct HirBody {
    pub exprs: IndexVec<ExprId, HirExprData>,
    pub stmts: IndexVec<StmtId, HirStmtData>,
    pub blocks: IndexVec<BlockId, HirBlockData>,
}

pub enum HirExprKind {
    Literal(Literal),
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: ExprId,
        args: Box<[ExprId]>,
    },
    // Nós filhos são sempre ExprId, eliminando Box<HirExpr>
}

pub struct HirExprData {
    pub kind: HirExprKind,
    pub ty: ArType,
    pub span: Span,
}
```

---

## 4. Benefícios Arquiteturais

1. **Localidade de Dados Contígua**: Todos os nós de uma função ficam armazenados em slices contíguos na memória física, garantindo máxima eficiência de pré-busca (*CPU hardware prefetching*).
2. **Clonagem Leve e Compartilhamento**: Um `ExprId` tem 4 bytes de tamanho e implementa `Copy`. Operações de cópia e transporte na pilha tornam-se de custo zero.
3. **Estabilidade Incremental para Salsa**: Como os índices são gerados de forma monotônica e determinística dentro de cada `HirBody`, mudanças em funções adjacentes não invalidam os índices internos de outras funções.

---

## 5. Estratégia de Migração

* **Fase A**: Introduzir o módulo `hir_indexed` no crate `arandu_middle` em paralelo ao HIR existente, comparando saídas via testes dourados.
* **Fase B**: Migrar o lowering `lower_to_hir` para produzir exclusivamente `HirBody` indexado.
* **Fase C**: Adaptar a geração da AMIR (`lower_amir`) para consumir `HirBody` por ID.
