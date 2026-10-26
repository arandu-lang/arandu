# RFC 0000: [Título da Proposta]

- **Número da RFC:** 0000
- **Título:** [Título descritivo da proposta]
- **Autor(es):** [Nome do Autor] ([@github-user](https://github.com/github-user))
- **Data de Início:** AAAA-MM-DD
- **Status:** `Draft` | `Planned` | `Accepted` | `Implemented` | `Frozen-R0` | `Superseded` | `Rejected`
- **Área Principal:** `Frontend` | `Middle-end` | `Backend` | `Runtime` | `Stdlib` | `Tooling`
- **PR da RFC:** [Link para o PR]
- **Issue de Acompanhamento:** [Link para a issue do Arandu]

---

## Fronteira de implementação e evidência

Ao preencher, declare a revisão/data, partes existentes, partes propostas e
limites de alvo/ABI. Marque snippets/comandos ainda ausentes como ilustrativos.
Status da RFC não é maturidade nem publicação; preserve o recorte quando
houver implementação parcial. A ordem de versões fica no roadmap mestre.
Ganhos de tempo, memória, tamanho e garantias de segurança exigem workload,
configuração e evidência; não copie metas como se fossem resultados.

## 1. Resumo (Summary)

Uma explicação breve, em um ou dois parágrafos, do que a funcionalidade ou decisão propõe e qual impacto prático ela causa no ecossistema Arandu.

---

## 2. Motivação (Motivation)

Por que estamos fazendo isso?
- Qual problema real de engenharia, ergonomia de linguagem ou segurança de memória esta RFC resolve?
- Quais casos de uso se tornam possíveis ou significativamente melhores?
- Qual é o resultado esperado se esta RFC **não** for aceita?

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

Explique a funcionalidade como se estivesse escrevendo a documentação oficial do Arandu ou um tutorial para desenvolvedores:
- Introduza os novos conceitos e sintaxes gradualmente.
- Forneça exemplos de código claros e idiomáticos.
- Explique como um programador Arandu pensa sobre este recurso e quando deve usá-lo.
- Se houver mensagens de erro ou diagnósticos associados, mostre como eles devem aparecer no terminal.

```arandu
// Exemplo ilustrativo de código Arandu
func exemplo(): void {
    // Demonstração da proposta
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

Este é o detalhamento técnico profundo da implementação para mantenedores do compilador:
- **Impacto no Pipeline:**
  - *CST/AST*: Novas regras de sintaxe ou nós da árvore.
  - *Resolução de Nomes*: Escopos, tabelas de símbolos e tratamento de ciclos.
  - *Inferência de Tipos / Typeck*: Regras formais de checagem, unificação e coerção.
  - *HIR / AMIR*: Como o recurso se comporta em SSA/OSSA (terminadores, rvalues, blocos básicos).
  - *Backends*: Geração de código em C, Cranelift e Wasm (ABI, registradores, chamadas de runtime).
- **Semântica de Runtime:**
  - Alocações, layouts de structs, concorrência e confinamento de threads.
- **Corner Cases e Casos Limítrofes:**
  - Como o compilador se recupera de código inválido sem entrar em pânico (`panic!`/`unwrap`).

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

- **Invariantes Afetados:** Como esta proposta preserva os 6 Invariantes Arquiteturais do Arandu (early-cutoff de queries Salsa, ausência de I/O em queries puras, `SymbolId` composto e alocação monotônica de `FileId`, dominância SSA/OSSA, layout dependente de `TargetInfo`)?
- **Custo de Complexidade:** Qual é o custo em tempo de compilação, tamanho do binário gerado e ergonomia cognitiva para o usuário?
- **Superfície de Invalidação Incremental:** A mudança aumenta a frequência de recompilação no LSP?

---

## 6. Racional e Alternativas (Rationale & Alternatives)

- Por que este design específico é o melhor em comparação com outras alternativas viáveis?
- Quais outras abordagens foram consideradas e por que foram rejeitadas?
- Qual o impacto de manter o status quo (não implementar)?

---

## 7. Arte Prévia (Prior Art)

Como outras linguagens e compiladores modernos resolvem este problema?
- Rust (ex: borrow checker, RFC process)
- Zig (ex: comptime, manual memory com allocators)
- Swift (ex: ownership, ARC)
- Go, C++, etc.
- Artigos acadêmicos relevantes (PLDI, POPL, OOPSLA, SBLP).

---

## 8. Questões em Aberto (Unresolved Questions)

- Quais partes do design ainda precisam ser validadas experimentalmente antes da estabilização?
- Quais decisões foram deliberadamente adiadas para RFCs futuras?

---

## 9. Possibilidades Futuras (Future Possibilities)

Evoluções naturais deste design que se tornam possíveis no futuro, mas que estão fora do escopo imediato desta RFC.
