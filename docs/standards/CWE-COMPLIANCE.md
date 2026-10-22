# Mapeamento de mitigações e limites de segurança — Arandu

**Estado em 15/10/2026:** inventário técnico de mecanismos e limites conhecidos.
Este documento não é certificação CWE, SEI CERT ou ISO/IEC 25010, nem uma prova
de eliminação de classes de vulnerabilidades. O nome histórico do arquivo é
mantido para preservar referências.

## Visão Geral e Contexto

O compilador, o runtime e o SDK têm superfícies distintas: código Arandu seguro,
blocos `unsafe`, FFI, implementações Rust/C, ferramentas de build, caches e
instaladores. Uma regra da linguagem não prova a correção do runtime ou de um
backend. `unsafe` e código estrangeiro dependem das obrigações do chamador;
checks, ownership e layout precisam conservar seu contrato em cada target.

A distribuição nativa v0.1.10 passou packages, integração SDK/VSIX e Public RC
install/smoke em Linux x86-64, macOS ARM64 e Windows x86-64 no
[run 38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029),
verificado em 15/10/2026. Isso valida os fluxos executados desses artefatos,
incluindo o corpus CTFE, sem certificar todos os comportamentos de runtime,
freestanding ou plataformas não distribuídas.

## Detalhes Técnicos da Implementação

### Memória, tipos e fluxo de controle

| CWEs relacionadas | Mitigação existente | Limite da afirmação |
| --- | --- | --- |
| CWE-416, CWE-415, CWE-825 | Move checker, loans, liveness, return-borrow summaries e verificações geracionais no fallback GenRef | Não prova ausência de UAF/double-free em `unsafe`, FFI, todos os carriers ou runtime; representação/lifetime de agregados e drop não trivial têm limites próprios |
| CWE-476 | Referências seguras e representação tipada de ausência | Ponteiros crus e valores recebidos de FFI continuam exigindo validação; nulidade não é a única condição de validade |
| CWE-787, CWE-125, CWE-129 | Checks de indexação e APIs como `Slice.get` retornando `Option<ref T>` | Depende de tipo/layout do alvo e codegen; ponteiro e comprimento recebidos de fonte insegura podem violar a precondição do buffer |
| CWE-119, CWE-120 | Strings/slices com comprimento explícito e APIs seguras de coleção | Um par `(ptr, len)` sozinho não prova provenance, capacidade, lifetime ou ausência de overflow |
| CWE-843, CWE-704 | Tipagem, enums discriminados, exaustividade e restrições de operações inseguras | Cast explícito não é prova de validade; ABI/FFI e casts de ponteiros têm obrigações adicionais |
| CWE-590, CWE-763 | Ownership e checagens de operações de memória inseguras | O sistema não certifica desalocadores estrangeiros nem todo gerenciamento manual |
| CWE-457 | Análise CFG de definite initialization (`O008`) | Exige preservação de usos, block args e dominância pelos passes |
| CWE-478 | Exaustividade de `match` (`T024`) | Não demonstra que a lógica de cada braço é correta |
| CWE-674 | Limites de instanciação e rejeição de tipos de tamanho infinito | Não impede toda recursão de execução nem esgotamento da pilha |
| CWE-570, CWE-571, CWE-670 | Diagnósticos de código morto/inalcançável no recorte analisável | Não detecta toda condição invariável nem todos os erros lógicos |

Fontes:
[`move_checker.rs`](../../crates/arandu_mir/src/move_checker.rs),
[`definite_init.rs`](../../crates/arandu_mir/src/definite_init.rs),
[`borrow_interface.rs`](../../crates/arandu_mir/src/borrow_interface.rs),
[`drop_elaborate.rs`](../../crates/arandu_mir/src/drop_elaborate.rs),
[`GenRef`](../../crates/arandu_runtime/src/genref.rs) e
[contrato de memória](../arandu-semantic-memory-model-v0.1.md).
O [catálogo de diagnósticos](../diagnostics/SPEC.md) define os códigos públicos.

### Aritmética e lógica numérica

| CWEs relacionadas | Mitigação existente | Limite da afirmação |
| --- | --- | --- |
| CWE-190, CWE-191 | Validação de literais, negação unsigned rejeitada, operações CTFE checked e normalização tipada de constantes | `+`, `-`, `*` comuns não possuem trapping universal de overflow; Cranelift/Wasm são modulares e C depende também das opções da toolchain |
| CWE-369 | `T040` para divisão/resto inteiros por zero literal | Float por zero conserva IEEE; divisão inteira dinâmica inválida não tem trap uniforme garantido em C/Cranelift/Wasm |
| CWE-197, CWE-681 | Regras de coerção e casts explícitos | `as` pode perder bits/precisão; intenção explícita não prova ausência de bug |
| CWE-480, CWE-481 | Atribuição como statement e condições booleanas | Reduz confusão sintática; não prova lógica correta |
| CWE-193 | Iteradores, ranges e checks de acesso | Off-by-one continua possível em algoritmos e limites escolhidos pelo programa |

O [contrato de operadores](../arandu-operator-arithmetic-semantics-rfc-v0.1.md)
separa implementação de propostas antigas de overflow/shifts. Evidências:
[`semantic_edge_cases.rs`](../../crates/arandu_query/tests/semantic_edge_cases.rs),
[`ieee754_semantics.rs`](../../crates/arandu_query/tests/ieee754_semantics.rs) e
[`CTFE`](../../crates/arandu_mir/src/ctfe/tests.rs).

### Frontend e código-fonte

| CWEs relacionadas | Mitigação existente | Limite da afirmação |
| --- | --- | --- |
| CWE-1307 / Trojan Source | Lexer diagnostica os controles BiDi cobertos por `LX004`, inclusive em comentários e strings | Não é prova contra toda forma de engano visual ou de apresentação do editor |
| CWE-1007 | Identificadores seguem Unicode XID | XID não elimina homoglyphs e não restringe automaticamente mistura de scripts; não há garantia geral contra identificadores visualmente confundíveis |
| CWE-134 | Interpolação tipada evita tratar texto comum como format string C arbitrária | FFI e APIs de formatação estrangeiras continuam fora dessa garantia |
| CWE-754, CWE-391 | `W006` sinaliza `Result` não tratado no recorte aplicável | Warning não garante tratamento correto de erro nem torna descarte explícito seguro |

Fontes:
[`lexer/ident.rs`](../../crates/arandu_lexer/src/lexer/ident.rs),
[`lexer/comment.rs`](../../crates/arandu_lexer/src/lexer/comment.rs),
[`lexer/string.rs`](../../crates/arandu_lexer/src/lexer/string.rs) e
[contrato do lexer](../arandu-lexer-v0.1.md).

### Concorrência e recursos

| CWEs relacionadas | Mitigação existente | Limite da afirmação |
| --- | --- | --- |
| CWE-362 | Bounds `Send`/`Sync`, armazenamento elegível, escopo de jobs e sincronização explícita | Não prova ausência geral de data races em FFI/unsafe ou todos os runtimes; `TaskHandle<T>` cooperativo não é prova de ownership entre threads |
| CWE-772, CWE-401 | Elaboração de drops e cleanup nos caminhos modelados | Não existe garantia geral de zero leaks; payloads não triviais, recursos externos e falha/cancelamento exigem provas específicas |
| CWE-833 | Filas bounded, execução inline aninhada e join de workers evitam classes concretas de deadlock | O runtime usa locks/canais bloqueantes; não é globalmente lock-free nem deadlock-proof |

Fontes:
[`worker_scheduler`](../../crates/arandu_runtime/src/worker_scheduler/pool.rs),
[`waker_runtime.rs`](../../crates/arandu_runtime/src/waker_runtime.rs),
[paralelismo](../arandu-structured-parallelism-v0.1.md) e
[async](../arandu-async-runtime-design-v0.1.md).

### Compilador, cache e cadeia de distribuição

| CWEs relacionadas | Mitigação existente | Limite da afirmação |
| --- | --- | --- |
| CWE-22, CWE-59 | Políticas de imports, validação de pacote e detecção de ciclos de symlink | O VFS atual não é sandbox de filesystem nem implementa `Dir`/openat2 da RFC 0016; canonicalização isolada não elimina TOCTOU |
| CWE-78, CWE-88 | Invocações nativas com `Command` e argumentos separados evitam interpolação de shell nesses caminhos | Ainda é necessário validar opções, executável escolhido e wrappers; não é prova universal contra argument injection |
| CWE-403 | Fechamento e política de handles dependentes da API/plataforma | Não há comprovação universal de ausência de descritores herdados em todo subprocesso |
| CWE-732, CWE-377 | Staging, diretórios temporários e publicação atômica em caminhos implementados | Permissões, ambiente host e races precisam de testes por plataforma; não há imunidade geral a symlinks |
| CWE-502 | Digests, envelopes versionados e validação fail-closed de caches/artefatos cobertos | Hash detecta divergência, não autentica conteúdo hostil. Dumps `.air`/`.amir`/`.ameta` atuais não reidratam AST/HIR nem persistem o grafo Salsa |
| CWE-400, CWE-770 | Budgets CTFE/instanciação, filas limitadas e early-cutoff | Reduzem workloads de risco; não garantem tempo/memória constantes nem resistência geral a DoS |
| Integridade do bootstrap | SHA-256 externo, validação nativa do archive e BLAKE3 em staging | `scripts/install.sh` ainda continua sem checksum se não encontra verificador; dívida fail-open conhecida, não corrigida por este documento |

Fontes:
[`vfs.rs`](../../crates/arandu_query/src/vfs.rs),
[`incremental`](../../crates/arandu_cli/src/incremental/mod.rs),
[`install.sh`](../../scripts/install.sh),
[distribuição](../arandu-distribution-contract-v0.1.md) e
[RFC 0016](../rfcs/0016-capability-safe-filesystem-and-path-resolution.md).

## PONTOS DE MELHORIA (O que não está no roadmap)

Métricas de alocação, latência e invalidação precisam de workload, configuração
e medição. `SmallVec`, arenas ou Salsa não demonstram zero alocação, ausência
de leaks ou cutoff O(1) para toda edição. Recuperação de sintaxe e fuzzing
reduzem regressões, mas não provam ausência de panic/ICE para toda entrada.

CWE/CERT e ISO/IEC 25010 podem orientar revisões; declarar conformidade exigiria
escopo, metodologia, rastreabilidade e avaliação próprios. Este inventário não
substitui essa avaliação, os testes de regressão nem auditoria de código.

## Futuro e Próximos Passos

A fila executiva e critérios de promoção pertencem ao
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md). Uma ampliação de garantia
precisa indicar superfície segura, target/backend, evidência e fallback,
incluindo os casos em que a ferramenta deve falhar explicitamente.
