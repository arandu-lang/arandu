# Arandu — Salsa, LSP e Identidades (v0.1)

**Status:** caminho arquitetural e campanha L0–L3 implementados; maturidade e
resíduos vivem no [roadmap mestre](./arandu-compiler-roadmap-v0.1.md), e a
superfície pública na [matriz de capacidades](./arandu-lsp-capabilities-v0.1.md).
**Dono do grafo de queries:** `arandu_query` apenas.

## Visão Geral e Contexto

O documento registra o ownership da incrementalidade e as identidades que
impedem o LSP de publicar resultados de buffers/revisões obsoletos.

## Detalhes Técnicos da Implementação

### Salsa toca / não toca

| Crate | Papel | Salsa? |
|-------|--------|--------|
| `arandu_query` | `ArandCompilerDb`, `DatabaseImpl`, `SourceFile`, `AnalysisHost`, `#[salsa::tracked]` | **Dono** |
| `arandu_middle` | `SourceDatabase` trait, tipos, AMIR/HIR, IDs densos | Interface + dados |
| `arandu_resolve` / `arandu_typeck` / `arandu_mir` | Lógica pura; fronteira só | Fronteira só |
| `arandu_lexer` / `arandu_parser` / `arandu_base` / backends | Puros | **Nunca** |
| `arandu_cli` / `arandu_lsp` | Orquestram DB + edits | Cliente do grafo |

### Queries tracked

| Query | Estado |
|-------|--------|
| `parse`, `resolve`, `module_signatures`, `type_check`, `lower_amir` | Reais |
| `local_symbols`, `exported_symbols`, `func_amir` | Reais |
| `liveness_facts` | Real (`arandu_mir::liveness`) |
| `block_dataflow_facts` | live/init/moved/stmt counts por bloco |
| `func_analysis_diags` / `block_diagnostics` / `file_ide_diagnostics` | F4 — diags IDE memoizados |
| DX.5 `RebuildLog` | Opt-in (`-Zexplain-rebuild`) |

### I/O de fonte

- typeck/resolve: proibido `fs::read` (guardrail `architecture_invariants`).
- Registro: CLI/LSP carregam bytes na borda e registram `SourceFile` antes da
  análise. `DatabaseImpl::resolve_module_path` apenas traduz identidades lógicas
  por inputs Salsa e consulta o registro; nunca lê filesystem nem percorre o cwd.
- Workers LSP **não** registram arquivos; só a main.

### Três identidades

| ID | Geracional? | Função |
|----|-------------|--------|
| `DocumentId` (`slotmap`) | **Sim** | Buffer LSP; close → stale |
| `FileId` + densos | **Não** | Análise na revisão atual |
| `AnalysisRevision` | Sim (host) | Handles IDE não atravessam edit |

`LspSymbolId { symbol, revision }` — resolve só se `revision == snap.revision`.

**Deadlock Salsa:** nunca segurar `AnalysisSnapshot` / clone de `DatabaseImpl` na **mesma** thread que chama `set_text` (Storage espera clones == 1).

### Legado

| Item | Status |
|------|--------|
| `CompileSession` | **Removido** |
| `symbol_span` dummy | **Span real** + `try_get` safe |
| tower-lsp / tokio no path de query | **Removidos** do `arandu_lsp` |

### LSP gold (implementado)

1. Main síncrona (`lsp-server`) + `Vfs` debounce 100 ms.  
2. Workers: `AnalysisSnapshot` (clone Storage) → diags/goto; publish só se DocumentId vivo e revision match.  
3. didChange **não** commita Salsa por tecla; flush no debounce / didSave / goto.  
4. Diagnostics via `file_ide_diagnostics` (F4); fingerprint blake3 evita republish no-op.  
5. CST-first Rowan: `syntax_tree` tenta reparse do ITEM tocado e reutiliza os green nodes irmãos; fallback seguro faz parse completo.
6. `initialize` conclui antes de I/O do workspace; a descoberta determinística e
   limitada ocorre em worker, e cada fonte retorna à main para registro na DB.
7. O scheduler mantém no máximo 64 jobs pendentes, serve a fila interativa
   antes da fila ampla, coalesce diagnósticos por `DocumentId` e cancela
   requests obsoletos antes de uma revisão nova. `$/cancelRequest` responde
   com `RequestCancelled`, inclusive quando o job ainda não começou.
   Cada commit de fonte avança a revisão compartilhada da análise; por isso,
   diagnósticos pendentes de todos os documentos abertos são coalescidos e
   reagendados após commits, saves, opens e closes. Resultados da revisão
   anterior continuam descartados, mas não deixam um importador aberto sem
   diagnóstico da revisão atual.
8. O servidor negocia UTF-16 explicitamente e todas as conversões entre bytes
   UTF-8 e posições LSP passam pelo mesmo `LineIndex`; semantic tokens usam
   comprimentos UTF-16 e são divididos por linha.
9. Edições recebidas dentro do debounce compõem sobre o buffer pendente da VFS,
   inclusive múltiplas mudanças por notificação, Unicode, arquivo vazio e EOF.
10. `IdeDiagnostic` preserva labels, notes, hints e replacements nas queries;
    o wire publica versão, `codeDescription`, `relatedInformation`, tags e
    `Diagnostic.data`. Quick fixes consomem apenas replacements estruturados.
11. Hover, completion e signature help compartilham apresentação de assinatura,
    tipos e doc comments; nenhum DTO expõe `Debug` de IR ou `SymbolId`.
12. Fontes conhecidas do workspace e overlays abertos têm autoridades distintas:
    overlay vence enquanto aberto, `didClose` restaura o disco e invalida o
    `DocumentId`, e create/delete/rename usam filtros `**/*.aru`. URI Windows
    padrão e caminho verbatim convergem para uma identidade; `FileId` removido
    nunca é reutilizado.
13. Depois do handshake, a descoberta em background instala manifesto,
    `ModuleRoots`, stdlib e `DirectoryListing` na thread escritora. Mudanças
    estruturais atualizam uma única listagem Salsa e reanalisam importadores
    abertos; `resolve` declara essa listagem como dependência explícita, enquanto
    edições somente de corpo preservam o cutoff de exports. Chaves absoluta,
    qualificada e relativa podem apontar ao mesmo `SourceFile`, sem perder o
    índice reverso enquanto algum alias continuar vivo. Workspaces com
    dependências locais usam o mesmo resolvedor determinístico da CLI; eventos
    de manifesto recompõem o grafo em job de background coalescido e atualizam
    as identidades existentes de `ProjectManifest`, `ModuleRoots` e
    `PackageModuleMap` em uma única revisão. Manifesto inválido mantém o último
    grafo válido e não interrompe recursos interativos.
    Dependências Git remotas são materializadas fora de Salsa pela biblioteca
    compartilhada `arandu_package`; o LSP opera apenas com lock e cache
    revalidado, sem rede durante descoberta ou reload.
14. Rename usa análise pura em `arandu_query`: a gramática lexical rejeita
    nomes reservados/inválidos, scopes relacionados bloqueiam conflitos e os
    spans vêm dos tokens do CST cruzados com a identidade semântica. O LSP
    revalida no pedido efetivo, produz edits multi-file determinísticos e deixa
    qualquer preview para o cliente.
15. Formatação permanece pura em `arandu_fmt` e canônica, sem depender das
    preferências transitórias do cliente. O wire converte edits UTF-8 mínimos
    por linha/hunk para UTF-16; a extensão define o formatter padrão, mas mantém
    `editor.formatOnSave` desligado até opção explícita do usuário.
16. Concorrência multi-documento é provada em três fronteiras: snapshots Salsa
    paralelos preservam arquivo/revisão, o scheduler cancela somente a chave
    solicitada e o stdio aceita respostas fora de ordem sem misturar documentos.
17. Performance interativa é medida no processo stdio real sobre corpus
    versionado: warm-up e 21 amostras produzem p50/p95 de diagnóstico,
    completion, goto e rename; cada resposta é validada antes de entrar na
    amostra e o relatório identifica commit, SO e arquitetura.
18. Folding e selection range caminham exclusivamente o CST congelado;
    document highlight reutiliza `prepare_rename`/`rename_occurrences` para
    obter identidade semântica e spans exatos. O servidor não infere
    read/write por texto quando o resolve ainda não classifica o acesso.
19. A descoberta do workspace começa somente após o handshake completo, emite
    `window/workDoneProgress/create` seguido por `$/progress` begin/end quando o
    cliente declara suporte e publica estados `indexing`/`ready` para a UI. A
    extensão limita reinícios automáticos e o Extension Host mata o processo
    real para provar recuperação, diagnóstico e completion após o restart.
20. A campanha L3 stdio intercala 119 revisões com requests interativos, drena
    toda resposta exigindo sucesso ou cancelamento LSP conhecido e então aplica
    uma revisão-oráculo válida. Nenhum diagnóstico de revisão anterior pode ser
    publicado depois do oráculo; completion e shutdown devem continuar vivos.
21. Summaries públicos de borrowed return fazem parte do hash de
    `module_signatures`: editar somente o corpo preserva o cutoff dos callers,
    enquanto mudar a dependência formal invalida seus corpos. O diagnóstico por
    item usa esse mesmo summary; O002/O003/O006/O010 mantêm labels e notes no
    wire, e uma revisão posterior nunca publica o resultado ownership stale.
22. Descoberta inicial só registra arquivos quando não há respostas interativas
    admitidas aguardando entrega. O acompanhamento por `RequestId` é limitado a
    64 entradas e termina também em erro/cancelamento; não depende do instante
    em que o worker retira o job da fila. O canal de descoberta mantém seu limite
    de oito eventos. Reloads de pacote aguardam no máximo em um slot coalescido.
    O loop continua recebendo protocolo, resultados e debounce, e a checagem de
    revisão rejeita resultados invalidados por edições reais. Tráfego interativo
    contínuo pode adiar a descoberta até haver uma janela sem respostas pendentes.

### F4 / P3 — delta on-type

- `block_dataflow_facts`: live/init/moved/stmt por bloco.  
- **`item_ide_diagnostics`**: diags de typeck **por item** (`item_body_typeck`) + AMIR se func.  
- **`file_ide_diagnostics`**: union barata dos memos de item + signatures.  
- Early cutoff entre itens (testes `item_body_cutoff`, `ide_diag_delta`).  
- Typeck monólito substituído por compose P1/P2; wire LSP ainda manda lista full (protocolo).

### P5 — CST-first (rowan)

- **Canônico:** `syntax_tree(file)` a partir do texto (ITEM por heurística de keywords).  
- **`parse(file)`** = `lower_syntax_to_program(syntax_tree)` — AST só como lower do CST.  
- **`reparse_subtree`**: re-lex só o ITEM tocado + `replace_child` (green dos irmãos reutilizado); fallback full `parse_syntax`.  
- **`syntax_tree` Salsa**: cache por file + `single_contiguous_edit` → `reparse_subtree`.  
- **Lower sem re-lex**: tokens no `SyntaxTree`; `parse_token_stream`.  
- **LSP semantic tokens** via query `file_highlights` (CST + resolve → `HlKind`; `textDocument/semanticTokens/full`).  
- Fingerprint de item (`item_source_input`) usa texto do ITEM CST.  
- Typeck/resolve consomem AST **somente** via lower do CST (`parse` ← `syntax_tree`).

### Guardrails / testes

- `architecture_invariants`, `doc_store` stale, `analysis` revision stale, `vfs` debounce, `block_delta`.

## PONTOS DE MELHORIA (O que não está no roadmap)

`arandu_middle/src/db.rs` declara inputs e o trait compartilhado por necessidade
de tipos, embora `arandu_query` continue único owner de providers/execução. O
guardrail atual é lexical e deliberadamente estreito.

## Futuro e Próximos Passos

Medir latência p50/p95 e recomputações por workload antes de mudar granularidade;
manter filas limitadas, cancelamento e early-cutoff por item.
