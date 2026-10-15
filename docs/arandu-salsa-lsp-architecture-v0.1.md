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
| `resolved_headers` / `header_signatures` | Resolução declarativa/imports e tipagem inicial sem resolução de corpos de funções; fronteira preparatória de seleção estática |
| `header_hir` / `ctfe_extern_hir` / `ctfe_header_func_amir` | Headers por módulo, declarações externas referenciadas e helpers isolados antes da resolução completa |
| `declaration_signatures` | Assinaturas declarativas; imports não solicitam contratos derivados de corpos nem MIR final |
| `item_typing` / `file_typing` | Checker canônico contra declarações; sem dependência dos contratos de fluxo |
| `prepare_hir` / `borrow_interfaces` | Produtor global legado para oráculos / contratos por unidades independentes antes da validação final |
| `local_symbols`, `exported_symbols`, `func_amir` / `instance_amir` | Reais; definições fonte e instâncias concretas têm APIs distintas |
| `ctfe_func_amir` / `ctfe_eval` | Unidades não genéricas com valores admitidos; chamadas importadas rastreadas por unidade |
| `ctfe_instance_amir` / `ctfe_eval_instance` | Unidades concretas; monomorphização canônica e identidade estrutural de callees |
| `ctfe_root_amir` / `ctfe_eval_root` | Tipagem inicial e avaliação de expressão/bloco isolados; seletores estruturais de instância e ocorrência |
| `item_static_branches` | Seleção pública de `comptime if` por função não genérica antes de resolução de corpos; mapa explícito de decisões, exclusão parent-first e recuperação de ciclos |
| `item_const_arguments` | Argumentos `comptime (expr)` independentes da instância em corpos ordinários, inclusive templates; congelados via headers/VM antes do checker puro, com `Const(u64)` e falhas explícitas, sem AMIR runtime |
| `item_staged_typing` | Obrigações públicas após tipagem inicial por item; materialização de valores antes de HIR runtime |
| `item_static_loops` | Domínios inteiros finitos via raízes AMIR; expansões não executam um interpretador de AST |
| `instance_staged_result` | Seleção, argumentos e raízes dependentes antes de resolver/tipar uma instância concreta; corpos residuais por ocorrência |
| `declaration_hir` / `declaration_context` | Headers por módulo e closure transitiva compartilhada, sem corpos |
| `function_hir` / `instance_hir` | Corpo isolado e especialização concreta; headers são ligados somente na instância |
| `runtime_raw_unit` / `instance_contracts` / `runtime_unit` | Lowering, convergência de contratos e validação separados por instância no caminho ativo |
| `runtime_program` | Descoberta determinística de dependências e composição canônica para `lower_amir`/backends |
| `liveness_facts` | Real (`arandu_mir::liveness`) |
| `block_dataflow_facts` | live/init/moved/stmt counts por bloco |
| `func_analysis_diags` / `block_diagnostics` / `file_ide_diagnostics` | F4 — diags IDE memoizados |
| DX.5 `RebuildLog` | Opt-in (`-Zexplain-rebuild`) |

`func_amir` de uma definição não genérica compartilha a função de `runtime_unit`,
sem solicitar composição global. A API fonte não aceita IDs sintéticos do
agregado: IDs desconhecidos/templates recuperam com corpo vazio, sem fallback
global. `file_func_symbols` enumera definições fonte ordinárias, não a lista
de funções de um executável. Instâncias concretas usam `instance_amir(Instance)`
e compartilham a AMIR/contexto de `runtime_unit`. O caminho
interno CTFE é distinto: `item_source_input` e `item_typing`, com
`declaration_signatures`/alvo, alimentam o lowering HIR canônico de apenas uma
função; sua AMIR possui pool próprio e descritores de valores resolvidos.
`ctfe_eval` rastreia unidades chamadas sob demanda, argumentos e orçamento.
Valor igual permite cutoff downstream; cancelamento usa unwind Salsa, não um
erro cacheado. Essa fronteira não encerra os gates AOT do marco 0.3 nem
encerra a superfície completa de `comptime`. Chamadas diretas a funções importadas
não genéricas com valores admitidos são suportadas, inclusive aliases/imports transitivos,
sem pedir interfaces de empréstimo, HIR global ou MIR final. Imports ausentes
ou cíclicos são rejeitados; a VM mantém seus limites e rejeições de efeitos.

O caminho concreto `ctfe_instance_amir` reutiliza `instance_hir` e a máquina
de monomorphização existente, sem consultar contratos finais nem composição.
Cada unidade traduz IDs sintéticos locais de chamadas para `FunctionInstance`
estrutural, preservando pools e layout próprios. A VM admite a closure retida,
inclusive helpers não executados, com fuel/cancelamento e handles limitados.
`instance_hir` ainda depende da tipagem inicial do item; não se pode chamá-lo
de dentro dessa tipagem para resolver obrigações CTFE sem um staging explícito.

Raízes isoladas de expressão/bloco têm APIs puras e queries estreitas. O
[recorte público escalar](arandu-comptime-core-v0.1.md) usa essas mesmas queries.
O checker do bloco tem destino de retorno próprio e unifica retornos/cauda com
o solver canônico; efeitos e limites de loops não vazam para o proprietário.
O lowering empresta a AST e o contexto HIR declarativo existente, sem reconstruir
headers nem criar símbolos chamáveis. A mesma SSA/CFG materializa retornos,
e saídas não unitárias exigem resultado definido em todos os caminhos.
`evaluate_unit` não registra a raiz como callee do símbolo proprietário.
`CtfeRoot` identifica arquivo/proprietário, um caminho local de seleção na
AST canônica e um tipo esperado escalar independente de pools. O seletor é
validado e limitado; não é um ID persistente, offset de texto nem nova sintaxe.
`ctfe_root_amir` usa `item_source_input`/declarações, o checker inicial puro e
os headers memoizados de `declaration_hir`; não consulta a tipagem do corpo
proprietário. Leituras e escritas em locais/parâmetros externos são capturas
runtime, mesmo se inicializados com literal. Locais declarados no bloco
pertencem à raiz; constantes globais mantêm seu checker canônico.
`ctfe_eval_root` consulta callees não genéricos na admissão e inclui orçamento
na chave. Mudanças runtime com spans preservados podem cortar antes da VM;
mudanças que deslocam spans podem revalidá-la, mas valor igual corta os
consumidores. Erros guardam spans atuais; cancelamento desenrola Salsa sem
memoizar uma falha da linguagem. O grafo não pede `function_hir`/`instance_hir`,
contratos de empréstimo ou AMIR final para tipar/avaliar a raiz.
`materialize_ctfe_scalar`, a ponte histórica no lowering HIR puro, reconstrói
valores escalares, arrays/tuplas/structs Copy e views literais usando tipos do
contexto destino e span fornecido pelo caller. Não
transporta IDs de pools, endereços ou handles da VM. Exige tipo exato e largura
de alvo compatível; não insere coerções nem reinterpreta `usize` entre ptr4/ptr8.
As provas internas substituem uma chamada por esse literal e removem o helper
antes de executar AMIR ordinária em C/Cranelift/Wasm, em O0/O1/O2. A seleção da
substituição nessas provas ainda é test-only, não um provider público residual.
O parser CST/AST agora preserva a raiz explícita. O checker inicial registra
obrigações sem executar queries; `item_staged_typing` avalia cada raiz e publica
valores por span completo de origem, sem IDs de pools estrangeiros. Assim,
memos retidos após edição irmã e raízes de arquivos distintos não confundem
índices de arenas. HIR usa a ponte pura e literais/constructores ordinários;
floats retêm encodings IEEE tipados até os três backends. Helpers CTFE
consultam `item_typing` inicial, não a visão staged;
staging aninhado/não materializado falha fechado, sem ciclos Salsa. Resultados
e layout entram no hash de corpo, não no hash de declarações exportadas. Itens
sem staging compartilham o memo inicial sem varredura da arena.
Parser/formatter, keyword semântico, completion e fallback TextMate reconhecem
esse recorte. Seleção estática em match e templates usa a mesma continuação
lexical. A seleção em escopos de lambda não habilita o sistema geral de closures:
tipagem e lowering dessas expressões conservam U001 até o marco 0.3, conforme
a delimitação de escopo aprovada pelo mantenedor.

`resolved_headers` executa o pipeline puro de imports e resolve declarações,
atributos, constantes e assinaturas, sem visitar corpos de funções. Guarda
scopes revisionais e o estado de uso/aliases dos imports. `resolve` continua
desse memo, visitando cada corpo uma vez e só então emitindo warnings de imports
não utilizados. Não há segundo resolvedor, reparse ou nova identidade pública.
Headers de parâmetros são alocados antes dos locais de todos os corpos: seus
IDs ficam iguais na continuação e não mudam ao inserir locais num corpo anterior.
IDs exportados continuam vindos do coletor local original. A ordem de alocação
dos locais revisionais mudou deliberadamente; não é ABI nem identidade de CGU.

`header_signatures` usa o checker declarativo canônico e recorre apenas à mesma
fronteira nos imports, com recuperação de ciclos e alvo explícito. Não chama
`resolve`, contratos de empréstimo ou AMIR. Edições de corpo com spans/headers
preservados cortam consumidores; deslocamentos de spans ou mudanças de índices
da arena podem revalidar o header. A seleção estática consulta essa fronteira
antes de resolver os ramos; `declaration_signatures` mantém sua visão compatível com
referências resolvidas dos corpos para consumidores atuais.

O seletor interno `IfCondition` conecta essa fronteira à VM escalar existente:
continua o resolvedor puro apenas na expressão, exige `bool`, rejeita captura
de parâmetros runtime e não visita nenhum ramo do proprietário. `header_hir`
reutiliza o produtor declarativo e o checker de constantes; helpers solicitados
pela VM usam `ctfe_header_func_amir`, com resolução/tipagem de somente seu corpo.
`ctfe_extern_hir` retém apenas o container externo de cada símbolo referenciado,
incluindo a identidade ABI dos intrínsecos de layout. A VM não adivinha um
intrínseco pelo nome nem precisa solicitar seu corpo externo. A composição
reutiliza o linker canônico sem trazer spans/corpos de funções irmãs para a
unidade CTFE; regressões exigem cutoff mesmo quando um corpo irmão muda de tamanho.
Não consulta `resolve`, `declaration_signatures`, `item_typing`, contratos ou
AMIR runtime final. A extração de `item_source_input` agora usa identidades do
header, não resolução completa. Não há parser, checker ou intérprete paralelo.

A continuação pre-body usa a AST canônica atual junto dos headers, não a arena
que um memo de item pode ter retido de outra revisão. Isso permite revalidar o
lowering após uma edição de sibling; AMIR/valor iguais cortam VM e consumidores.
Budget, alvo, caminho local e proprietário fazem parte das dependências/chaves;
erros e cancelamento não produzem decisão arbitrária. `StaticIfCondition`
publica o mapa de seleção parent-first. `InInstance` fornece a substituição
estrutural concreta; `InIteration` distingue índices de loops finitos sem
confundir valores pelo mesmo span. Capturas runtime continuam proibidas.

O produtor concreto resolve/tipa apenas corpos selecionados, usando os mesmos
passes puros. Corpos de loop dependentes são verificados por ocorrência e
ligados ao pool HIR do proprietário; corpos independentes são compartilhados.
`comptime_bodies` conserva a ordem do domínio congelado, e o lowering AMIR
emite blocos acíclicos com locais SSA novos e cleanup lexical para desvios.
Locais exclusivos de ocorrência são remapeados por unidade no compositor,
preservando seus escopos de nomes e os IDs fonte de declarações/instâncias.
Limites de ocorrências, obrigações e trabalho residual são finitos; seleção
estática não reinicia o orçamento público inteiro a cada iteração.

`declaration_signatures` é a fronteira inicial dessa separação: importa somente
as assinaturas declarativas, reutilizando o checker e os inputs existentes.
Contratos inferíveis da própria declaração continuam presentes, mas não são
substitutos da análise de fluxo. `module_signatures` mantém a visão compatível
usada pelos consumidores de ownership: compõe as declarações com
`borrow_interfaces` dos imports. `item_body_typeck` compartilha o memo de
`item_typing` e recompõe essa metadata, sem rodar outro checker. A recuperação
IDE preserva a composição. Uma mudança somente na origem retornada recompõe
ownership, mas não retipa o caller se os tipos declarados continuarem iguais.
A validação de atributos/testes também consulta somente declarações, para não
reintroduzir indiretamente a dependência de fluxo em `file_typing`.

`borrow_interfaces` consulta `instance_contracts` somente para definições com
retorno potencialmente emprestado. Publica IDs fonte para funções ordinárias e
chaves estruturais para instâncias; IDs sintéticos de outro domínio nunca são
contratos públicos. Templates não instanciados conservam apenas metadata
declarativa de recuperação, não uma prova de fluxo. A validação de cada
instância concreta consulta o ponto fixo real. A query não solicita
`prepare_hir`, `lower_amir`, promoção de escapes ou validação final.

`prepare_hir` e os produtores puros globais permanecem como compatibilidade e
oráculo. Seu fingerprint conservador cobre fontes vinculadas, tipos e
diagnósticos. O runtime ativo não depende desse estágio. Testes de staging
provam consultas declarativas sem corpos, cutoff entre declaração e origem,
recursão, imports transitivos e rejeição final de um sibling inseguro.

### Runtime por instância e composição ativa

`function_hir` retém somente o corpo selecionado. `declaration_context` liga
uma vez os headers transitivos de um módulo, usando a mesma rotina canônica de
declarações para assinaturas, constantes, modos de parâmetros/receptor e
metadata nominal. `instance_hir` combina essa closure compartilhada com o corpo;
o memo de função não duplica headers nem os copia novamente para especialização.
Não há outro parser ou checker. Cada contexto concreto continua com seu domínio
de interner, necessário às mutações de especialização/lowering.

`FunctionInstance` preserva o `SymbolId` composto da definição e argumentos
`TypeShape` estruturais. `TypeId` e ranges de argumentos são locais ao interner,
nunca identidades entre unidades. A leitura/escrita estrutural é limitada a
128 níveis e 4096 nós, incluindo expansão de DAGs; IDs/ranges inválidos falham.
O hash inclui filhos, variantes e o valor completo de argumentos constantes.

`instance_hir` especializa somente o corpo solicitado usando a máquina de
substituição existente. Callees genéricos ganham apenas assinaturas concretas
e um mapa de símbolos locais para chaves estruturais; suas AMIRs não são
dependências do lowering do caller. `runtime_raw_unit` conserva contexto de
tipos/símbolos, literais e metadata de debug próprios. Não certifica segurança.

Destruidores implícitos são descobertos também na closure estrutural de campos
e payloads concretos, sem consultar corpos de callees. Assim, um retorno de
`BitSet` já carrega a obrigação de destruir seu `Vec<u64>` interno no caller.
A visita é determinística e limitada a 4096 tipos e aos bounds de `TypeShape`.
Move checking e drop-on-assign consultam a mesma representação canônica de
caminhos; a rota de dereferência é restaurada ao materializar drops. Regressões
exercitam rehash com buffer vivo, campos retornados e ASan/LSan no backend C.

`instance_contracts` percorre a closure de chamadas com retorno emprestado,
limitada a 4096 unidades e com cancelamento entre unidades/transferências.
Resolve contratos por ponto fixo usando a transferência canônica do MIR em
cada domínio de tipos. SCCs começam sem origens inferidas: compatibilidade de
assinatura não demonstra que uma recursão devolve empréstimo de um formal.
Os summaries publicados usam caminhos/índices formais, sem IDs de interner.
`runtime_unit` compartilha a AMIR bruta memoizada e produz uma cópia owned da
função para anotar chamadas e aplicar a mesma validação M2/escape/promoção do
caminho global. Não copia `AmirProgram` nem corpos de outras funções.

Testes exigem cutoff do lowering do caller ao editar um callee, atualização da
origem emprestada sem rebaixar o caller, recursão com origens reais, rejeição de
recursão sem prova e O010 para retorno local. A especialização visita também
init/step de loops C-style. Pools são compostos pela rotina MIR compartilhada;
visitors mutáveis incluem projeções e todos os argumentos de terminadores.

`runtime_program` parte das funções não genéricas do arquivo de entrada e
descobre as unidades importadas/instanciadas alcançadas por referências de
função (incluindo callbacks, não só calls) e destruidores implícitos. Headers
dos módulos de argumentos nominais concretos são vinculados sem ler seus
corpos. Imports e corpos alcançados inválidos impedem a publicação de AMIR
executável; validar o caller não certifica seus callees. A closure é limitada a
4096 instâncias e consulta cancelamento durante descoberta e composição.

O compositor puro no MIR preserva IDs fonte compostos, aloca símbolos
sintéticos por chave estrutural e traduz tipos, metadata e literais para um
domínio agregado único. Visitors compartilhados cobrem SSA, projeções e
argumentos de salto. Pseudo-tipos numéricos inferidos são defaultados antes de
formar chaves; nomes nominais qualificados e aridades evitam aliases nativos.
`lower_amir` compartilha o resultado agregado por Arc. Backends continuam
consumindo a mesma AMIR, sem saber como Salsa produziu os corpos.

Na composição, `TypeInfo::merge_codegen_context` traduz a metadata emprestada,
sem clonar o interner inteiro de cada unidade. Headers nominais de uma mesma
revisão são traduzidos uma vez por identidade fonte; funções/locais gerados,
destruidores concretos e efeitos são remapeados. Summaries provados são
instalados após todos os headers. Esse modo não pode mesclar revisões distintas;
o merge ordinário continua substituindo headers alterados.

CGUs usam uma closure de declarações por função, compartilhada entre hashing
e emissão do `ObjectModule`. Ela cobre referências em statements/terminadores,
callbacks e destruidores implícitos da closure de tipos. Callees contribuem
assinatura/layout, não corpo. Pool IDs, IDs sintéticos de composição e símbolos
de debug locais não são identidade de máquina: literais/tipos são codificados
por conteúdo e símbolos válidos pelos nomes nativos qualificados. `SymbolId`
composto continua intacto no IR. Drop shims usam o mesmo nome nativo estável.
O schema CGU v3 invalida caches antigos, preservando verificações de objetos,
target, toolchain, ABI/layout (inclusive `repr(C)`) e closure final do link.
Remover uma CGU pode reutilizar objetos restantes, mas obriga relink do
executável quando sua closure mudou. Regressões comparam objetos byte a byte
e exercitam uma nova instância genérica com apenas dois misses e três hits.

Análises IDE usam tipos, símbolos e contrato da própria unidade. Uma função
inválida pode reter AMIR de análise para diagnósticos por bloco, mas nunca é
publicada para execução. Quick fixes estruturados continuam preservados.

**Limite:** a entrega final aos backends continua agregada; isso não é um
produtor global de corpos nem uma dependência global dos objetos CGU. Contextos
concretos ainda retêm metadata em domínios locais. Cutoff de nova instância e
mudanças não relacionadas têm regressões, mas não encerram o marco 0.3. A DB
batch continua nova a cada processo. Latência p95, retenção de contextos e
validação nativa Windows/macOS continuam gates distintos.

O workload informativo `runtime_workload` compara o produtor puro global
legado e o ativo no mesmo checkout: 32 funções com `Vec<int>`, helper genérico,
stdlib registrada, cold/warm e edição privada de corpo. Ambos os caminhos têm
uma fronteira Salsa memoizada, inclusive no teste legado. Executar cada variante
em processo separado (após compilar o teste; GNU time é opcional):

```sh
cargo test --locked -p arandu_query --test runtime_workload --no-run
ARANDU_WORKLOAD_PRODUCER=legacy /usr/bin/time -v cargo test --locked -p arandu_query --test runtime_workload -- --ignored --nocapture --test-threads=1
ARANDU_WORKLOAD_PRODUCER=units /usr/bin/time -v cargo test --locked -p arandu_query --test runtime_workload -- --ignored --nocapture --test-threads=1
```

O tempo é informativo, não budget imposto à CI. O teste exige exatamente uma
HIR e unidade raw/final reexecutadas na edição privada. Para separar o custo
da composição pura, usar `ARANDU_WORKLOAD_COMPOSE_ONLY=1`. O produtor legado é
um oráculo dentro do checkout atual, não um binário histórico. Para RSS/CPU,
preferir executar diretamente o binário de teste informado por `--no-run`,
excluindo compilação/link Rust e o processo Cargo da medida.

Medição informativa de 2026-09-30: Linux x86-64, Xeon E5-2667 v2, perfil debug,
cinco processos isolados por produtor, uma thread de testes, log Salsa opt-in.
Medianas (sem compilar o teste na janela):

| Medida | Global legado | Unidades |
| --- | ---: | ---: |
| Produção fria | 225,7 ms | 702,2 ms |
| Memo quente | 7 µs | 7 µs |
| Edição privada | 85,5 ms | 64,8 ms |
| CPU user + system, sessão cold/warm/edit | 0,317 s | 0,778 s |
| Pico RSS, sessão cold/warm/edit | 21,7 MiB | 29,4 MiB |

Não há ganho global de performance demonstrado: o build frio e a retenção
regrediram neste workload, enquanto a edição privada melhorou. A composição
isolada ficou próxima de 34 ms em uma sondagem separada. Reduzir duplicação de
headers/contextos exige perfil adicional, preservando IDs locais e cutoff.
Estas amostras não certificam p95 ≤ 10 ms, release otimizada, projeto grande
ou suporte nativo de outros sistemas.

Refinamento de 2026-10-01, mesmo workload/host debug, cinco pares intercalados
em processos isolados antes/depois, sem compilação Rust dentro da janela.
CPU/RSS medidos por `getrusage(RUSAGE_CHILDREN)` de um supervisor novo por
amostra (o pico de um filho anterior não contamina o próximo). Medianas:

| Medida | Antes do refinamento | Depois |
| --- | ---: | ---: |
| Produção fria | 701,4 ms | 673,0 ms |
| Memo quente | 8 µs | 8 µs |
| Edição privada | 65,2 ms | 49,8 ms |
| CPU user + system, sessão cold/warm/edit | 0,774 s | 0,726 s |
| Pico RSS, sessão cold/warm/edit | 29,18 MiB | 26,87 MiB |

A composição isolada caiu de aproximadamente 34 ms na sondagem anterior para
20,4 ms em uma nova sondagem. São medições informativas, não prova de budget
p95 ou desempenho geral: o cold ainda custa mais que o produtor global legado.
O teste mantém exatamente uma HIR e unidade raw/final reexecutadas na edição.

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
   Enquanto o documento tiver texto pendente, diagnósticos não são agendados
   nem publicados para ele: a versão do cliente já pode ter avançado sem que
   o texto correspondente esteja no snapshot Salsa. Após o flush, a análise é
   reagendada normalmente. Testes de interleaving sem sleeps cobrem refresh
   durante debounce e a publicação apenas depois do commit.
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
- **`item_ide_diagnostics`**: typeck **por item** (`item_typing`) + AMIR/contratos no domínio próprio de runtime se func.
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
