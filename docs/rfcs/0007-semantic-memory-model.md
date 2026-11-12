# RFC 0007: Modelo Semântico de Memória e Empréstimos (Semantic Memory Model)

- **Número da RFC:** 0007
- **Título:** Modelo Semântico de Memória, OSSA e Interfaces Estruturais de Empréstimo
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-08-15
- **Status:** `Implemented`
- **Área Principal:** `Middle-end` / `Memória` (`arandu_mir`, `arandu_typeck`)
- **Documento Relacionado:** `docs/arandu-semantic-memory-model-v0.1.md`

---

## 1. Resumo (Summary)

O Arandu trata a gestão de memória não como uma anotação decorativa de tipos com parâmetros de tempo de vida sintáticos explícitos (`'a`), mas como uma **consequência semântica demonstrável no fluxo do programa**. Esta RFC formaliza o Modelo Semântico de Memória do Arandu, que conecta diretamente Ownership SSA (OSSA), análise de fluxo de vida (*liveness*), detecção de escape, representação física de dados e diagnósticos fail-closed na AMIR.

---

## 2. A Árvore de Decisão Semântica de Memória

```text
OSSA demonstra janela de vida local?
          │
          ├─ SIM ──→ Empréstimo Direto (Stack-First)
          │          Custo de runtime: ZERO. Ponteiro físico na pilha.
          │
          └─ NÃO
             ├─ Referência interna de corrotina representável?
             │      └─→ RelativeBorrow (endereço relativo no frame atual)
             │
             ├─ Owner escapável representável?
             │      └─→ GenRef + Diagnóstico O004 (Fallback geracional determinístico)
             │
             └─ Relação sem representação segura demonstrável
                    └─→ Diagnóstico Fail-Closed antes do Backend (O010 / O002 / O003)
```

---

## 3. Sintaxe Pública e Vocabulário de Tipos

A linguagem expõe apenas três formas fundamentais de relacionamento com valores:

1. **`own T` (ou simplesmente `T`)**: Posse exclusiva do recurso. Pode ser movido ou destruído.
2. **`ref T`**: Empréstimo imutável compartilhado. Permite leitura concorrente, proíbe mutação e destruição do owner enquanto a janela estiver ativa.
3. **`mut ref T`**: Empréstimo mutável exclusivo. Garante acesso único para escrita durante a janela de uso.

> [!NOTE]
> Não existem tempos de vida explícitos na sintaxe da linguagem. O compilador infere as janelas de empréstimo a partir do grafo de controle (CFG) e da análise de pontos de programa na AMIR.

---

## 4. OSSA: Rastreamento Fino de Estados de Posse

A AMIR modela formalmente o ciclo de vida através de fatos operacionais:
* `Borrow` e `BorrowMut`: Iniciam uma janela de empréstimo associada ao owner raiz.
* `Destroy`: Materializa a destruição consumidora de uma variável no final de seu escopo.
* `StorageLive` e `StorageDead`: Delimitam o espaço físico da variável na pilha.

### Movimentos condicionais (2026-11-11)

Um valor movido num caminho e não em outro (`if c { consume(s) }`, um braço
de `match` que liga um payload ao lado de outro que o ignora) chega à junção
nem possuído nem movido, e a limpeza depois dela não pode destruí-lo. A
elaboração de drops resolve isso antes de tudo (`drop_elaborate::settle`):
em cada aresta que entra numa junção onde o valor ainda é possuído, ele é
destruído ali, num bloco próprio da aresta, e a junção passa a vê-lo movido
em todos os caminhos. É a forma estática dos drop flags do rustc, e é sound
porque um valor possivelmente movido não pode ser usado depois da junção
(`O007`). Um payload alcançado por uma variante de enum só existe quando
ela é a ativa, então sua destruição na aresta é guardada pelo discriminante.
Antes disso esses valores vazavam.

Mover uma parte de um valor cujo tipo tem `@Destructor` é `O018`: o
destrutor roda sobre o valor completo.

### Conflitos de Empréstimo Reportados

O passador `arandu_mir::borrow_check` percorre a AMIR pós-otimização e emite diagnósticos estritos caso invariantes sejam violados:
* **`O002` (*Move while borrowed*)**: Tentativa de mover ou transferir posse de um valor enquanto uma referência ativa (`ref` ou `mut ref`) aponta para ele.
* **`O003` (*Borrow conflict*)**: Tentativa de criar um empréstimo mutável enquanto já existem empréstimos compartilhados ativos, ou múltiplos empréstimos mutáveis simultâneos.
* **`O006` (*Destroy while borrowed*)**: Destruição de escopo atingindo um valor com empréstimos pendentes.

---

## 5. Invariantes Arquiteturais

1. **Unicidade de Motor de Liveness**: O compilador não possui um segundo motor léxico de lifetimes. O borrow checker consome exatamente as mesmas live ranges calculadas para a AMIR.
2. **Fail-Closed em Ambiguidade**: Se o fluxo de controle bifurcar e o compilador não puder provar de qual raiz um empréstimo de retorno se origina, a compilação é imediatamente interrompida com diagnóstico claro, nunca permitindo código inseguro alcançar o gerador de código.
