# Auditoria de Arquitetura e Performance v0.1

**Status:** registro histórico da auditoria rc.5 e das correções subsequentes.
**Leitura atualizada em 2026-10-15:** resultados e contagens abaixo pertencem
às rodadas identificadas, não a uma execução atual da suíte. Estado atual,
prioridades e gates vivem somente no [roadmap mestre](arandu-compiler-roadmap-v0.1.md).
O planejamento de versões é revisável e não aprova RFCs implicitamente;
extensões de pesquisa mantêm contratos próprios e não recebem promessa de
versão por este registro.

## Evidência da rodada rc.5

O registro histórico desta rodada parte de `bca57ea` na branch `prep/rc5`.
O flush da CLI já está integrado nesse baseline e sua validação Windows foi
informada pelo mantenedor.
As correções abaixo pertencem aos owners existentes, sem novas dependências,
mudanças de IDs, interners, allocator ou fusão de queries.

| Área | Causa demonstrada | Correção e regressão |
| --- | --- | --- |
| AMIR / SimplifyCFG | caminhada de Goto sem limite entra em ciclo; regressão excedeu timeout de 3 s | limite pelo número de blocos, preservando a aresta original em ciclos; cobre self-loop, ciclo de dois blocos e prefixo |
| AMIR / SimplifyCFG | sweep renumera blocos, mas omite `Suspend.resume`; a retomada é perdida | remapeia resume e preserva argumentos/parâmetros; match exaustivo força revisão ao adicionar terminadores |
| Contratos / middle | validador fatia parâmetros antes de validar a faixa | pré-validação de todas as faixas emite ICEGEN002, inclusive para alvo posterior e faixa extrema |
| Liveness | domínio vazio ainda percorre CFG; 1.100 blocos excediam guard de 1.000 visitas | solução vazia imediata para locals/temps vazios, preservando os guards do domínio não vazio |
| CFG / RPO | DFS recursivo aborta com 16.384 blocos em thread de 128 KiB | frames iterativos preservam ordem DFS, ciclos e exclusão de blocos inalcançáveis |
| Runtime Linux | polling sem timer pula epoll mesmo com sockets registrados | dispatch de readiness independente de timer; teste de wake e rearm one-shot |
| Infra de testes | testes TCP retornavam sucesso sem exercitar sockets quando portas fixas estavam ocupadas | portas efêmeras com falha explícita de setup |
| Infra CLI | suíte falhou ao publicar `product_gold` em diretório ocupado; helpers reutilizavam diretórios nomeados apenas por relógio (ou PID + relógio) | dez helpers passam a reserva atômica com PID + sequência e retry de colisão; regressão exercita 32 criações paralelas; SL_T.5 passa após a correção |
| Infra incremental | corpus de performance usava sintaxe inválida e substituía FileId em vez de editar o input | corpus Arandu válido de 50 módulos, parsing/typeck obrigatórios, `set_text`, zero execução em cache e cutoff comprovado por contagem de queries |
| Runtime / tarefas | cancelamento podia liberar o blob durante join; a guarda proposta também impedia aposentar slots concluídos | estados Pending/Running/Completed transferem ownership ao join; cancel em execução solicita aposentadoria; canais substituem sleep e testes verificam reutilização do slot |
| Parser / IDE | recuperação pulava tokens sem atualizar a supressão; falha de parse podia deixar Problems vazio | contar avanço de recuperação e publicar todos os erros por accumulator privado da query parse, sem repetir o lowering CST→AST |
| LSP | panics capturados não tinham contexto no log e URIs inválidas recebiam uma identidade fictícia | logging compartilhado em stderr, sem clone do payload; símbolos sem URI válida não são publicados; regressão stdio verifica respostas e revisão final |
| C / Cranelift | casos manuais podiam concordar no mesmo resultado incorreto | corpus determinístico gera 64 funções com aritmética, branches e chamadas; C e Cranelift precisam coincidir com um oráculo independente |
| Runtime de strings | helpers `ToStr` e concatenação alocam buffers sem drop glue no fat `str` | ASan/LSan encontrou quatro leaks e 20 bytes no caso reduzido; correção depende do contrato de ownership BC.1a, sem `free` local inseguro |

A falha local anterior de `run_tcp_async_wait_wake` foi isolada: criar um
socket na sandbox retorna `Operation not permitted`; o mesmo teste passa
fora dela. Essa evidência não justifica uma alteração semântica no TCP.

A campanha diferencial C/Cranelift adicionou um corpus reproduzível de 64
funções. Os operandos vêm de um PRNG com seed fixa e limites que evitam overflow
e divisão por zero; o teste calcula o valor esperado em Rust e exige o mesmo
retorno nos dois backends. Isso cobre soma, subtração, multiplicação, divisão,
comparações, branches e chamadas em uma unidade compilada, sem dependência nova.

Com `ARANDU_C_SANITIZERS=1`, os 30 casos sem strings passaram e quatro casos com
`ToStr`/interpolação falharam somente no LeakSanitizer. O caso reduzido aloca
20 bytes em quatro buffers: duas conversões e duas concatenações. O código
declara explicitamente “process-lifetime leak”, mas o contrato público diz que
o chamador possui o buffer sem representar esse ownership na AMIR. Inserir
`free` no helper seria incorreto porque o fat `str` ainda pode escapar ou ser
usado por uma concatenação. O fechamento exige distinguir views estáticas de
buffers owned e elaborar seu drop nos dois backends. Esse era o diagnóstico
da rodada rc.5; BC.1a foi posteriormente marcado como implementado no roadmap.
O relato histórico não constitui uma pendência atual nem substitui regressões
e sanitizers do contrato de ownership vigente.

Foram removidas cópias redundantes no DCE (listas de IDs já representadas por
faixas densas) e no GVN (cópias dos statements, listas intermediárias de IDs
e uma tabela de definição escrita mas nunca lida). A análise empresta dados;
a mutação continua posterior à coleta de decisões. Não se declara ganho de
latência de compilação a partir dessa remoção. As cópias necessárias para
materializar resultados emprestados das queries de descoberta foram mantidas.

O type checker passou a emprestar `ExprKind` pelo `&AstPool` já existente,
sem alterar assinaturas nem copiar o nó. `module_signatures` copia somente
as tabelas mutáveis que consome, sem clonar a documentação descartada. A seed
de `resolve` ainda exige cópia porque o resolver a modifica: o comentário
enganoso de `Arc::unwrap_or_clone` foi removido, sem eliminar o sharing Salsa.
Hashes de diagnósticos e enums usam códigos/discriminantes explícitos,
preservando determinismo e evitando formatação Debug temporária.

### Revisão das propostas antes dos commits

Foram rejeitadas substituições de abort por sucesso/zero para referências
geracionais e blobs inválidos: zero pode ser payload válido e esconder
violação do contrato unsafe. Também foi rejeitado um limite arbitrário de
leitura de string: sem comprimento válido ele não protege o ponteiro e pode
truncar entradas válidas. Os contratos anteriores dessas ABIs foram preservados.

A proposta de canal de resultados LSP limitado foi retirada: bloquear send
enquanto o worker ainda detém snapshot pode impedir a escrita Salsa; usar
try_send descartando rejeições/cancelamentos perde respostas de requests.
A fila de jobs continua limitada/coalescida. O canal de resultados permanece
sem limite até existir desenho que descarte snapshots antes da publicação,
preserve todas as respostas de requests e tenha prova de saturação/shutdown.
O teste stdio intercalado é evidência de respostas/revisões, não de saturação.

**Atualização da implementação (2026-10-15):** o registro acima descreve a rc.5.
O caminho atual em `arandu_lsp/src/lib.rs` usa canal de resultados bounded, com
capacidade 64 definida em `dispatcher::JOB_RESULT_CAPACITY`; rejeições do loop
são adiadas em vez de fazer send bloqueante ao canal que ele próprio drena.
Isso substitui o estado histórico unbounded, mas a interação de backpressure,
snapshots Salsa, mutações de inputs e shutdown continua exigindo evidência
específica de pressão. Limite de fila não prova por si só ausência de deadlock.

O corpus corrigido também expõe custo real: em uma execução Linux/dev,
50 módulos válidos levaram aproximadamente 5,46 s a frio, 37 µs em cache e
2,87 s para validar importadores após editar a dependência. Nenhum corpo de
importador foi rechecado, mas a latência de validação ainda exige perfil.
Esses tempos são observações de uma execução, não budgets nem comparação
antes/depois: o corpus anterior era inválido.

O probe `borrow_interface_workload_measurement` usa 64 funções válidas que
retornam empréstimos. Em Linux/debug, uma execução isolada observou 72,22 ms
a frio, 22,84 µs em cache e 43,16 ms após editar um literal sem deslocar spans;
pico de RSS do processo: 16.716 KiB. Apenas um `item_body_typeck` executou;
`lower_amir` e `borrow_interfaces` executaram uma vez cada. O summary manteve
seu conteúdo. Logo, existe cutoff; o custo restante está no cálculo do resumo.
Um probe anterior que inseria texto e deslocava spans reexecutou 64 corpos,
mostrando que as duas classes de edição precisam ser medidas separadamente.
Não se mediram alocações completas nem perfil release nesta rodada, portanto
esses números não autorizam reestruturar o pipeline por uma promessa de ganho.

O RPO iterativo elimina dependência da profundidade do CFG na pilha nativa,
ao custo de um vetor auxiliar de frames proporcional à profundidade visitada.
No microbenchmark reproduzível `rpo_cfg_workload_measurement`, em Linux/dev,
medianas de sete amostras de 2.000 travessias de 256 blocos foram 29,52 →
48,96 ms (cadeia) e 34,98 → 66,90 ms (ramificações). É uma correção de
robustez com custo observado, não ganho de velocidade. Esses números sem
otimização não estabelecem impacto no compilador release.

### Limites desta evidência

Validação local Linux desta rodada: `cargo fmt --all -- --check`,
`cargo check --workspace --locked`, Clippy com todos os targets/features e
`-D warnings`, `cargo test --workspace --locked` (1.609 passaram, zero falhas,
seis ignorados), `check-diag-docs` (88 códigos) e rustdoc com `-D warnings`,
executados nessa ordem. Também passaram `check-architecture`,
`check-line-endings` e `check-diag-determinism.sh arandu_typeck 8`.
A suíte inclui `architecture_invariants`, `salsa_imports`, `item_body_cutoff`,
`ide_diag_delta`, `block_delta` e `run_tcp_async_wait_wake`.
Os probes RPO e borrow ignorados foram executados separadamente. Miri não
foi executado: a toolchain instalada não inclui esse componente. Os casos condicionados
a Windows/macOS continuam dependendo de execução nativa; esta validação
não promove esses alvos nem cobre os demais probes ignorados.

A revisão também inspecionou fronteiras CST/AST/query, layout dos backends,
fila de workers LSP e workflows. Isso não equivale à leitura exaustiva de cada
linha nem prova ausência de bugs. Merges de CFG
reconstroem a tabela inteira; O2 continua experimental. Paridade de registro
de sockets em macOS/Windows exige backend próprio e testes nativos. A fila de
resultados LSP merece análise de pressão separada da fila limitada de jobs.
Esses pontos não foram apresentados como regressões reproduzidas nesta rodada.

As extrações CLI/LSP/runner já integradas estão descritas abaixo como estado
implementado. A fila de estabilização permanece somente no roadmap mestre.

## Visão Geral e Contexto

Esta auditoria pausa expansão funcional e verifica ownership entre crates,
efeitos, incrementalidade, portabilidade, concentração de módulos e sinais de
alocação. Ela não declara ganho de performance a partir de contagem de linhas
ou de `.clone()`: essas contagens apenas selecionam locais para inspeção.

O resultado geral é saudável. Frontend, semântica pura, query engine,
backends, runtime e frontends CLI/LSP têm ownership distinguível. Não foi
encontrado filesystem I/O em `src/` dos crates puros de compilação, nem
providers Salsa fora de `arandu_query`. A exceção declarativa existente em
`arandu_middle/src/db.rs` abriga tipos compartilhados da DB, sem executar
queries.

## Detalhes Técnicos da Implementação

### Fronteiras e dependências

| Camada | Dono | Resultado da auditoria |
| --- | --- | --- |
| texto/CST/AST | lexer e parser | CST-first preservado; nenhuma tipagem/resolução deslocada ao parser |
| contratos/IR | middle | HIR, AMIR, IDs e layout continuam independentes dos frontends |
| semântica | resolve, typeck, mir, semantics | fontes puras; nenhum acesso direto ao filesystem em produção |
| incremental | query | providers, host, snapshots e projeções continuam concentrados |
| emissão | codegen e backends | contratos de teste/ABI compartilhados sem dependência da CLI |
| efeitos | CLI, LSP, runtime e xtask | I/O, processos, cache, rede e publicação permanecem nas bordas |

Foi removida a dependência direta e sem uso de Salsa em `arandu_lsp`. O novo
`xtask check-architecture`, executado no S0, rejeita dependência/uso Salsa fora
do owner e do contrato estreito de `middle`, e rejeita I/O de filesystem nos
sources dos crates puros. `arandu_base/src/tracing_bridge.rs` é a exceção
explícita: somente o sink de self-profile grava o arquivo solicitado pela CLI.

### Tamanho e coesão

O retrato inicial dos arquivos monolíticos ficou desatualizado após as
extrações. No baseline desta rodada, `arandu_cli/src/main.rs` tem 23 linhas
e delega para `commands::run` e `pipeline::finish`. Parsing/despacho de
lifecycle vive em `commands/project.rs`; implementação em `project/`.

O runner usa `test_runner/` com `process`, `ipc`, `benchmark`, `baseline`,
`reporters`, `statistics` e `types`; o protocolo compartilhado continua em
`arandu_codegen`. O LSP usa `ide/` por capacidade, com `types` e
`presentation` compartilhados. Essas extrações estão implementadas, não são
pendências para a próxima mudança funcional. Novas separações devem resolver
acoplamento demonstrado; tamanho isolado não justifica outra refatoração.

### Heap, clones e strings

A inspeção encontrou maior concentração de `.clone()` na orquestração da
CLI, linking do HIR, monomorfização, grafo de pacotes e filas incrementais.
Nos hot paths query/IR, o projeto já usa `Arc`, interning, `SmolStr`,
`rustc_hash`, resultados por item e sharing Salsa. Nos frontends, muitas cópias
são ownership necessário para atravessar threads, processos ou DTOs JSON/LSP.

Nenhuma troca ampla por `Cow`, `SmallVec`, novos interners ou outro allocator
foi aplicada: isso alteraria complexidade/tamanho sem evidência. Cópias
obviamente mortas podem ser removidas em revisão local; estruturas de dados e
hot paths exigem workload representativo, perfil de alocação e benchmark antes
e depois.

### Memória persistente e scratch do compilador

Pools e coleções persistentes do compilador usam o allocator padrão e ownership
Rust, com IDs e compartilhamento explícito; isso não implica reservas virtuais
manuais nem arenas uniformes para todas as fases. Dados temporários podem ter
uma vida menor que a unidade consultada: o MIR já usa `bumpalo` no SCCP,
SimplifyCFG e move checker. O
[`pass_manager/fixpoint.rs`](../crates/arandu_mir/src/pass_manager/fixpoint.rs)
cria uma arena por função e a reseta entre rodadas mutantes; o
[`move_checker.rs`](../crates/arandu_mir/src/move_checker.rs) mantém suas arenas
locais à análise e materializa os resultados que precisam sobreviver a ela.
Scratch não pode escapar para resultados persistentes ou memos Salsa.

O registro histórico anterior à consolidação anotava a retirada de
`VmReservation`/`BumpArena` manuais: eram código sem consumidores nas fases e
continham falhas de bounds/overflow e granularidade de commit no Windows.
Essa retirada não é trabalho pendente nem motivo para reintroduzir um allocator
próprio. O uso atual de `bumpalo` tampouco torna implementadas arenas NUMA,
allocators por worker ou snapshots apoiados em CoW do sistema operacional.
Qualquer ampliação precisa preservar lifetimes e cutoff e demonstrar benefício
no workload medido; não existe uma obrigação geral de migrar pools para arenas.

### Documentação e portabilidade

A taxonomia agora distingue roadmap, contrato, arquitetura, decisão concluída,
diagnóstico e release. Quatro roadmaps SL_T concluídos foram removidos e seu
conteúdo foi consolidado no contrato de testes/benchmarks. Project/Package e
GenRef deixaram de parecer campanhas abertas. O contrato de texto UTF-8/LF foi
materializado em `.gitattributes`, `.editorconfig`, xtask e CI.

## PONTOS DE MELHORIA (O que não está no roadmap)

Esta seção preserva decisões históricas e limites de evidência, não uma fila
concorrente de implementação. Pendências executivas pertencem ao roadmap.

### Decisões de estabilização rc.5

| Tema | Decisão e critério |
| --- | --- |
| Modularização CLI/runner/LSP | Preservar as extrações existentes e seus testes; remover da lista de pendências o trabalho já implementado. |
| Cobertura macOS | Decisão da rc.5: adicionar `macos-portability` com `cargo test --workspace --locked` nativo, selecionado pelo mesmo escopo de produto que Linux/Windows e exigido pelo S0. O fechamento exigia uma execução verde no PR; este registro não certifica o estado atual da CI. |
| Contrato do gate | Falha, cancelamento ou skip inesperado do macOS bloqueiam S0; alteração sem código aceita somente skip intencional. SDK empacotado permanece uma evidência separada. |
| Nome `arandu_package` | Manter o alias de library nesta candidata. Extrair um package Cargo só se houver benefício demonstrado de dependências/build ou fronteira funcional; mudança cosmética não estabiliza execução. |
| Guardrail arquitetural | Preservar a checagem atual. Evolução para grafo Cargo/AST exige casos concretos de falso positivo/negativo e regressões, sem remover a proteção existente. |
| Heap e latência | Usar corpus válido e preservar cutoff. Alteração estrutural exige medição antes/depois de tempo, recomputações, RSS e alocações; o microbenchmark debug do RPO não demonstra custo release. |

O objetivo dessa decisão era cobertura obrigatória de uma plataforma anteriormente
representada principalmente pelo SDK. O custo previsto era um job macOS por mudança de
produto, executado em paralelo. Não há promessa de paridade do reactor de
sockets: testes devem verificar o contrato suportado e a rejeição explícita
das operações ainda indisponíveis.

## Futuro e Próximos Passos

A ordem e os critérios de fechamento pertencem à
[fila do roadmap mestre](arandu-compiler-roadmap-v0.1.md#fila-de-execução).
Soak do SL_T, perfil do corpus representativo e resultados nativos permanecem
evidências necessárias; esta seção não mantém outra fila de implementação.

### Validação de mercado

O alvo `fuzz_module_graph` amplia o oracle incremental para remoção, recriação,
renomeação, imports ausentes, assinaturas e ciclos de três módulos. A suíte
limitada executa 100 pares com restauração e 32 sequências de 24 operações:
1.268 edições. O writer atualiza `DirectoryListing`; a DB fria recebe apenas o
estado final de cada passo. A comparação normaliza identidades de arquivos por
caminho, preservando spans, ordem, mensagens, labels e replacements. O sentinel
sem fonte usado pela recuperação de ciclos permanece distinto dos arquivos.

A sequência reduzida `03 08` encontrou uma função ausente na AST incremental.
Adicionar um import e alterar o corpo dentro do intervalo do item antigo
produzia um único nó CST contendo dois itens. O teste posterior de quantidade
de nós não detectava isso, pois a substituição já forçava um único nó. O reparse
agora verifica a quantidade de itens nos tokens novos, equilíbrio de chaves e
erros lexicais antes da substituição; mudanças de fronteira usam parse completo.
Edições locais válidas mantêm o reuso dos irmãos, coberto pelo teste de identidade
green existente. A regressão compara CST e tokens com parse frio, incluindo
import novo, chave removida e comentário não terminado. A seed reduzida é
reexecutada pelo runner isolado, além da matriz de fuzzing existente.

O alvo `fuzz_incremental` complementa fuzzing de crashes com um oracle
diferencial: após cada edição, compara o conteúdo completo dos diagnósticos
IDE de dois módulos em uma DB aquecida e uma DB nova. A ordem de registro
permanece igual para preservar a correspondência dos IDs; a ordem de demanda
alterna. O gerador cobre corpo e assinatura exportada, renomeação de símbolo,
recuperação sintática, símbolo ausente, Unicode, função irmã e restauração.
Cada entrada executa no máximo 24 edições, sem snapshots retidos na mutação.

A suíte comum executa todos os 100 pares de operações seguidos de restauração
e uma sequência de 15 edições: 415 edições e 830 comparações. O mesmo código
atende libFuzzer e o replay isolado de `check-fuzz-regressions`. A seed inicial
é preventiva, não representa um bug encontrado. A campanha longa usa o
workflow de fuzzing existente; uma falha deve ser reproduzida e reduzida antes
de virar regressão e correção da causa raiz.

Esse oracle encontra desacordo incremental, não prova que ambas as análises
estão semanticamente corretas. Ainda não cobre alteração do grafo de arquivos,
execução dos backends nem todas as capacidades LSP. A fila de expansão segue
no roadmap único. A abordagem adapta a comparação independente do
[Csmith](https://github.com/csmith-project/csmith) e a redução de casos do
[guia de fuzzing do rustc](https://rustc-dev-guide.rust-lang.org/fuzzing.html).

Na validação Linux desta campanha, a primeira execução do workspace falhou em
`stdio_open_document_stays_interactive_during_discovery`: completion recebeu
ContentModified (-32801). A repetição completa e 30 execuções isoladas passaram.
A investigação seguinte reproduziu a ordenação incorreta com canais: enquanto
o worker aguarda liberação, a descoberta já está pronta e o seletor antigo a
consome. O registro pode avançar a revisão antes da entrega do resultado.

O dispatcher agora mantém até 64 identidades de requests admitidos até a
entrega terminal. Nesse intervalo, deixa a descoberta no canal limitado
existente e mantém protocolo, resultados e debounce ativos. Reloads de pacote
também aguardam, em um único slot que conserva o plano concluído mais recente.
Sucesso, erro, panic capturado e cancelamento liberam a barreira; rejeições por
saturação não entram nela. O teste de ordenação falhou antes da correção e
passou depois, sem sleeps. A checagem de revisão permanece: uma edição real
ainda causa ContentModified para resultado obsoleto. Não houve relaxamento do
teste stdio. O marco correspondente é DX.6a do roadmap.

O modelo segue o red-green incremental do rustc: pureza, fingerprints estáveis
e projeções pequenas evitam propagação falsa. O rust-analyzer confirma a
necessidade de cancelamento/snapshots ao aplicar mudanças Salsa. O critério de
layering do LLVM reforça dependências explícitas e acíclicas entre bibliotecas.
Para heap, o Rust Performance Book recomenda que contagens de clone/alocação
levem a profiling, e não a substituições automáticas; `rustc-perf` reforça o
uso de corpus contínuo para atribuir regressões.

- [rustc incremental compilation](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html)
- [rust-analyzer architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)
- [LLVM library layering](https://llvm.org/docs/CodingStandards.html#library-layering)
- [Rust Performance Book: heap allocations](https://nnethercote.github.io/perf-book/heap-allocations.html)
- [rustc-perf](https://github.com/rust-lang/rustc-perf)
