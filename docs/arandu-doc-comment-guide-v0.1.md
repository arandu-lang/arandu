# Arandu — Guia de Comentários de Documentação (`doc comments`)

> Convenção oficial para documentar módulos e itens da linguagem Arandu.
>
> Objetivo: uma única fonte (`//!` + `///` nos `.aru`) que renderiza bem nos três
> consumidores — hover do LSP (popup Markdown estreito), `arandu doc` (HTML/Markdown/JSON)
> e páginas de referência do site (selos, assinatura, exemplos).

---

## Visão Geral e Contexto

Doc comments competem por atenção em três superfícies com restrições diferentes.
A regra de ouro: **a primeira linha resume, o topo nunca carrega tabela** —
tabelas e matrizes quebram no hover e não viram selo em nenhum renderer.

## Detalhes Técnicos da Implementação

### 1. Sintaxe: `//!` documenta o módulo, `///` documenta o item

- `//!` no **topo do arquivo, antes de `module X`**: anexa ao próprio módulo
  (caminho direto do `module_doc`). É o único lugar correto para o overview.
- `///` imediatamente acima de `struct`, `enum`, `func`, `const`, `type`,
  `interface`: documenta aquele item.
- `//!` em qualquer outro lugar e `///` solto entre `module X` e o primeiro
  item são órfãos: o `module_doc` os reaproveita como overview (fallback),
  mas prefira a posição canônica acima.
- `/** ... */` também é doc comment (bloco). `//` comum nunca é documentado.

### 2. Ordem das seções (o hover mostra o topo primeiro)

```aru
//! One-line summary in the imperative, under ~100 chars.
//!
//! ## Overview
//! 2-4 paragraphs of prose: what it is, when to use it, when NOT to use it.
//! No tables, no bullet lists in this block.
//!
//! ## Example
//! ```arandu
//! import std.core.exemplo as ex
//!
//! let r = ex.alguma_funcao(42)
//! ```
//!
//! ## Complexity
//! Time: O(1) amortized. Space: O(n).
//!
//! ## Allocations
//! Zero-Alloc. / Allocates once per call via the ambient allocator.
//!
//! ## Safety
//! Traps, preconditions and what is NOT checked.
//!
//! ## Errors
//! Which diagnostics (`T040`, `M001`) fire and under what condition.
//!
//! ## Platform
//! `native` · `wasm32` · `bare-metal`
//!
//! Caveats in prose (e.g. avoid `SeqCst` where there is no global barrier).
```

Seções reconhecidas pelo toolchain (case-insensitive): `Complexity`,
`Allocations` (`Allocation`), `Safety`, `Examples` (`Example`), `Errors`
(`Error`). Todo o resto cai em `Description`. Exemplos em `arandu` são
extraídos como doctests (`file_doctests`) — mantenha-os mínimos e rodáveis
(≤ 15 linhas, imports explícitos, sem dependência de rede/OS além do exemplo).

### 3. Plataforma sem tabela

Plataforma é **metadado, não tabela**. Formato: uma linha compacta de tags
craseadas (`` `native` · `wasm32` · `bare-metal` ``) seguida de ressalvas em
prosa. No hover aparece 1 linha limpa; no site as tags evoluem para selos
(como os selos de efeito); no `arandu doc` listam como estão.

Vocabulário: `native` (x86-64, aarch64), `wasm32` (note threads quando
aplicável), `bare-metal` (sem OS). Arquiteturas vão na prosa, nunca na tag.

### 4. Idioma: inglês na fonte

O código-fonte documenta em **inglês** (alinha com `matrix`, `vec`, `fs` e o
catálogo de erros, todos em EN). O português vive na camada do site (páginas
curadas e descrições curtas). Nunca misture os dois idiomas no mesmo bloco.

### 5. Anti-patterns

- `# Título` com o nome do módulo/item no corpo: todo renderer já titula.
  É a redundância mais comum — corte.
- **Nota sobre função mora na função.** Caveats, atenções e comportamentos
  específicos de uma função (`lenBytes` conta bytes, `unwrapOr` consome)
  vão no `///` daquela função — nunca no overview do módulo. O overview
  descreve o módulo; quem lê a função precisa entender a função.
- **Overview não duplica docs de itens.** Listar funções com descrições no
  overview (`abort()` — trap fatal...) duplica o que já está em cada item
  e apodrece na primeira divergência. Se cada item tem `///`, o overview
  não precisa de lista.
- **Toda API citada precisa existir.** Antes de commitar, confira nomes,
  assinaturas e exemplos contra o código (`arandu doc` faz o parse e acusa
  sintaxe, mas não intenção). Exemplos com funções inexistentes
  (`byteLen`, `writeAll`, `transmute`) já passaram batidos uma vez.
- Tabela de plataformas no topo: ilegível no hover, inútil como selo.
- Bullet points no bloco de Overview: prosa primeiro, listas só em seções
  técnicas (`Complexity`, `Errors`).
- `//!` em itens ou `///` solto no topo: funciona por fallback, mas esconde
  a intenção. Posição canônica sempre.

## PONTOS DE MELHORIA (O que não está no roadmap)

O formato atual documenta convenções, mas ainda não valida automaticamente a
posição de `//!`, títulos H1 redundantes ou exemplos de documentação. A extração
de metadados de plataforma para selos estruturados também não faz parte do
contrato atual. Essas ideias permanecem melhorias exploratórias: não são
pré-requisitos para hover, `arandu doc` ou renderização dos comentários atuais.

## Futuro e Próximos Passos

- Parse do micro-formato de plataforma em selos estruturados (site + JSON).
- Lint (`arandu check`/`doctor`) avisando `//!` fora do topo e H1 redundante.
- Doctests do `## Example` executados no CI por módulo.
