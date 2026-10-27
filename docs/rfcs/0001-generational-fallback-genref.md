# RFC 0001: Fallback Geracional Controlado com GenRef

- **Número da RFC:** 0001
- **Título:** Fallback Geracional Controlado (GenRef) em Linguagem Stack-First com Ownership
- **Autor(es):** Bruno Bispo dos Santos
- **Data de Início:** 2026-09-11
- **Status:** `Frozen-R0` (Contrato empírico congelado para medição acadêmica)
- **Área Principal:** `Middle-end` / `Runtime` / `Memória`
- **Substitui:** Contrato preliminar MVP F2.3

---

## 1. Resumo (Summary)

O Arandu adota um modelo de gerenciamento de memória prioritariamente em pilha (*stack-first*), governado por regras estritas de posse e empréstimo estático (*ownership & borrowing*). Quando uma referência precisa escapar de seu escopo léxico ou compor estruturas dinâmicas cíclicas e não pode ser provada segura em tempo de compilação, o compilador recorre ao **GenRef** (*Generational Reference*): um mecanismo determinístico de fallback geracional baseado em arenas indexadas e chaves geracionais de 64 bits.

O GenRef garante segurança de memória sem recorrer a um *Tracing Garbage Collector* com pausas estocásticas (*Stop-The-World*) e sem o custo de sincronização atômica de contadores de referência (*Atomic Reference Counting* - ARC) em caminhos concorrentes.

---

## 2. Fronteira Congelada de Medição R0 (TCC Experimental Boundary)

> [!IMPORTANT]
> **Norma de Congelamento Empírico do Protótipo R0**:
> Para viabilizar a aferição científica rigorosa, reprodutível e independente no Trabalho de Conclusão de Curso (TCC) e na publicação acadêmica (SBLP / ERBASE), a superfície do **R0** está **oficialmente congelada nesta RFC**.
>
> **O que está rigorosamente congelado para o R0**:
> 1. **Contrato Público de Handles**:
>    - O tipo `GenRef<T>` é um handle opaco lógico de 64 bits composto por `{ arena_id: 16, arena_gen: 16, slot_idx: 16, slot_gen: 16 }`.
>    - É estritamente proibido código de usuário acessar, mascarar ou manipular bits internos do handle.
> 2. **Erros Tipados de Resolução**:
>    - Falhas de resolução e ciclo de vida retornam variantes tipadas estritas (`StaleGeneration`, `InvalidArena`, `DestroyedArena`, `CapacityExceeded`, `AllocationFailure`), sem sentinelas de ponteiro nulo.
> 3. **Semântica de `@NoFallback` e Diagnósticos `O004` / `O010`**:
>    - Funções marcadas com `@NoFallback` ou compilações com a flag `--no-generational-fallback` promovem o diagnóstico informativo de escape `O004` imediatamente a erro fatal de compilação.
>    - Retorno de referências puramente locais (`ref`) sem arena associada continua sendo violação semântica rígida reportada em `O010`.
> 4. **Confinamento e Escopo de Execução**:
>    - Operação síncrona, estritamente confinada à thread chamadora (*thread-confined*, `!Send + !Sync`).
>    - Paridade de execução e armadilha determinística (*trap*) validada de forma idêntica entre os backends C e Cranelift (apenas *host-only*, sem pretensão de concorrência preemptiva multi-core nos handles).
> 5. **Métricas Observáveis**:
>    - A estrutura de métricas `ArenaRegistry::metrics` e o relatório de compilação `--genref-report` fornecem contagens estáticas e dinâmicas exatas de alocações, reusos de slot, checks executados e desativações.

**O que NÃO está congelado pelo R0** (permanece em livre evolução no restante do Arandu):
* Biblioteca padrão (`SL_S-Core`, `SL_S-Host`, Effect System `A2`);
* Runtime assíncrono cooperativo (`SL_R`);
* Servidor de linguagem (LSP) e capacidades de IDE;
* Passes de otimização de AMIR (DCE, LICM, inlining) e do backend que não alterem os invariantes de checagem do GenRef;
* Pesquisa pós-graduação (análise de fluxo incremental de borrow checking).

---

### Emenda de soundness do R0 (0.2.0, AGG.1)

A auditoria AGG.1 reproduziu uma miscompilação na promoção implícita
gerenciada pelo compilador. Um borrow de payload `Copy` gravado em memória
(campo de agregado, ou através de outra referência) era promovido: o local
passava a ser um `GenRef` e o handle era gravado num slot tipado como `&T`.
Referências são ponteiros crus em runtime, então toda releitura por essa
memória, na mesma função ou em outra, interpretava o handle como ponteiro.
Cranelift JIT, Cranelift AOT e LLVM devolviam os bits do handle em vez do
valor (por exemplo, `*holder.value` dava `2` para um valor `5`), e o backend C
emitia C inválido. Só um deref pelo próprio temporário SSA lia o valor certo.

Decisão (soundness first): esses escapes passam a ser `O004` com severidade
de erro, como já ocorria com borrows projetados, e o pipeline não executa mais
a promoção implícita (`arandu_mir::gen_promote` permanece, sem chamadores de
produção). O congelamento do R0 continua valendo para handles explícitos
(`GenArena`/`GenRef`), erros tipados, `@NoFallback`, `O010`, confinamento e
métricas. O que muda é que a promoção implícita de referências guardadas em
memória deixa de existir no R0, em vez de produzir resultados errados. Borrows
cujo payload não é promovido continuam como nota.

Reabilitar a promoção exige uma representação de primeira classe para
referências que podem carregar handle (R1). Regressão:
`arandu_query/tests/genref_gold_pipeline.rs::o004_rejects_a_promotable_borrow_stored_in_memory`.

---

## 3. Motivação (Motivation)

Sistemas contemporâneos enfrentam um dilema histórico no gerenciamento de memória:
* **Linguagens Manuais (C/C++)**: Alto desempenho e controle previsível, mas responsáveis por ~70% das vulnerabilidades críticas de segurança da indústria (*Use-After-Free*, *Double-Free*).
* **Linguagens com GC (Java, Go, C#)**: Eliminação total de *dangling pointers*, porém com sobrecarga imprevisível de memória (*heap overhead* de 1.5x a 3x) e pausas de varredura.
* **Linguagens com Ownership Estático Puro (Rust)**: Custo zero de abstração e ausência de GC, mas extrema rigidez ao representar grafos de controle, grafos de roteamento ou árvores com ponteiros pai/filho, frequentemente forçando o uso de `Rc<RefCell<T>>` ou bibliotecas não padronizadas de arenas.

O Arandu soluciona essa dicotomia através do princípio da **Promoção por Falha Controlada**: o compilador tenta provar a localidade léxica na pilha. Caso não consiga, ele não rejeita o programa nem introduz um coletor de lixo opaco — ele eleva o empréstimo para uma referência geracional determinística em arena, notificando o desenvolvedor através do diagnóstico `O004`.

---

## 4. Explicação em Nível de Guia (Guide-Level Explanation)

No código Arandu, a criação de objetos e referências segue a sintaxe padrão:

```arandu
struct Node {
    id: int,
    next: GenRef<Node>?,
}

func build_graph(mut arena: GenArena<Node>): GenRef<Node> {
    let first = arena.alloc(Node { id: 1, next: nil });
    let second = arena.alloc(Node { id: 2, next: first });
    return second;
}
```

Ao compilar o código acima, o compilador identifica que a referência `second` sobrevive ao encerramento de `build_graph`:
1. É emitido o diagnóstico de observabilidade `O004`:
   ```text
   info[O004]: value promoted to generational fallback (GenRef)
      --> graph.aru:6:5
       |
     6 |     return second;
       |     ^^^^^^^^^^^^^^ value escapes lexical scope; managed via arena
       |
       = note: to forbid heap promotion and ensure zero-cost stack residency, annotate function with @NoFallback
   ```
2. Se o desenvolvedor exigir estritamente que a função opere sem alocações dinâmicas, anota-se a função:
   ```arandu
   @NoFallback
   func compute_critical(x: int): int {
       // Qualquer escape aqui dispara um erro de compilação imediato
   }
   ```

---

## 5. Explicação em Nível de Referência (Reference-Level Explanation)

### Layout Físico do Handle (64-bit)

```
 64                     48                     32                     16                    0
┌──────────────────────┬──────────────────────┬──────────────────────┬──────────────────────┐
│       Arena ID       │   Arena Generation   │      Slot Index      │   Slot Generation    │
│       (16 bits)      │      (16 bits)       │      (16 bits)       │      (16 bits)       │
└──────────────────────┴──────────────────────┴──────────────────────┴──────────────────────┘
```

* **Zero é Inválido**: O valor `0x0000_0000_0000_0000` denota formalmente um handle nulo/inválido.
* **Sem Wraparound**: Contadores geracionais atingindo `0xFFFF` forçam a aposentadoria permanente do slot ou da arena, eliminando o clássico problema *ABA*.

### Resolução em Linha (Cranelift / C Backend)

A validação de um `GenRef` na fase **R0** consiste na chamada runtime segura `ar_gen_resolve_raw`. Na evolução subsequente (**R1**), a validação será expandida diretamente na AMIR para um teste inlinado de 3 instruções assembly x86_64:

```text
movzx   eax, WORD PTR [rdi + slot_offset]      ; 1. lê a geração atual do slot no array contíguo
cmp     ax, cx                                 ; 2. compara com a geração esperada presente no handle
jne     .handle_trap_stale_generation          ; 3. desvia deterministicamente caso haja divergência
```

---

## 6. Invariantes de Arquitetura e Segurança

1. **Thread Confinement**: Toda estrutura `ArenaRegistry<T>` e `GenArena<T>` é `!Send + !Sync`. Referências geracionais não podem transitar entre threads do sistema operacional sem canal de passagem ou cópia profunda.
2. **Deterministic Trapping**: O acesso desreferenciado a um slot reciclado ou arena liberada nunca causa leitura de memória corrompida (*undefined behavior*). Ele interrompe a execução com trap determinístico e registro estruturado.
3. **Isolamento de Queries Salsa**: O cálculo de escape e verificação de lifetimes no middle-end é uma query pura e determinística em `arandu_query`, sem efeitos colaterais de I/O.

---

## 7. Campanhas de Validação e Testes Reproduzíveis

O contrato do R0 é validado através de suítes contínuas de integração:
* **Miri**: Validação de proveniência estrita e alinhamento simbólico (`-Zmiri-strict-provenance -Zmiri-symbolic-alignment-check`).
* **AddressSanitizer (ASan) & UndefinedBehaviorSanitizer (UBSan)**: Cobertura total da biblioteca runtime C emitida.
* **Endurance Testing**: Teste de 1.000.000 de ciclos de reciclagem de slots comprovando aposentadoria sem reciclagem de geração (ausência de anomalias ABA).

```bash
cargo test --locked -p arandu_runtime million_cycle_endurance_retires_without_aba
CC=gcc ARANDU_C_SANITIZERS=1 cargo test --locked -p arandu_backend_c --test parity_tests
```
