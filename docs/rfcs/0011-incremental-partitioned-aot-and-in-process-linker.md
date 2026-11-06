# RFC 0011: Pipeline AOT Incremental, Codegen Particionado e Linker In-Process Determinístico

- **Número da RFC:** 0011
- **Título:** Pipeline AOT Incremental, Codegen Particionado (CGUs) e Linker In-Process Determinístico
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-12
- **Status:** `Draft`
- **Área Principal:** `Backend` / `Incrementalidade` / `Tooling`
- **PR da RFC:** [Pendente]
- **Issue de Acompanhamento:** [Pendente]

---

## 1. Resumo (Summary)

Esta RFC estabelece a evolução da compilação AOT (*Ahead-Of-Time*) do Arandu, estendendo a garantia de **Early Cutoff** e **Determinismo estrito** do frontend/Salsa até a geração do executável binário final.

A proposta introduz quatro mecanismos arquiteturais integrados:
1. **Cutoff entre Execuções do CLI (Fase 0):** fingerprint persistente e verificável da closure de inputs e do artefato. O grafo Salsa e suas estruturas internas não são serializados.
2. **Codegen Particionado em CGUs (*Codegen Units*) no Cranelift (Fase 1):** Quebra da geração de `.o` monolítica em unidades discretas por item/função, indexadas por hash semântico de entrada (AMIR da função + assinaturas dependidas).
3. **Linker In-Process via `mmap` para Linux ELF (Fase 2):** Emissor/patcher ELF nativo embutido que elimina a dependência de processos externos (`cc`, `mold`, `ld`) no ciclo de desenvolvimento, aplicando patch em staging quando o layout é compatível, sem promessa atual de tempo fixo.
4. **Determinismo Ponta a Ponta e Harness de Verificação (Fases 3 e 4):** Garantia de reprodutibilidade bit a bit do binário final, independente de contagem de threads ou ordem de compilação, com harness automatizado de medição de cache-bust.

---

## 2. Motivação (Motivation)

Na motivação original de 2026-09-12, queries incrementais em memória
(`crates/arandu_query`, incluindo `item_body_cutoff.rs`) já preservavam cutoff,
mas o caminho AOT ainda tinha duas barreiras. O estado auditado em 2026-10-15
está na seção 8; a descrição monolítica abaixo é histórica:

1. **Amnésia entre Sessões do CLI:** Cada comando `arandu build` instancia uma nova `DatabaseImpl`, executa em lote e descarta toda a memória. O early cutoff existe no LSP (processo longo), mas o desenvolvedor no terminal sempre paga o custo de compilação fria.
2. **O Abismo da Última Milha (The Linker Bottleneck):** Ao final da compilação, o compilador emite um único arquivo `.o` volumoso e invoca um linker de sistema externo via fork/exec (`cc`/`clang`/GNU `ld`). O linker externo não tem ciência do grafo Salsa e re-linka 100% dos símbolos, tabelas e bibliotecas a cada pequena edição.

A hipótese original era que o link poderia dominar o custo de uma pequena
edição semântica. O relatório por fase deve comprovar essa distribuição no
workload medido; não há comparação nesta RFC que sustente liderança de
performance entre linguagens.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

A intenção é preservar o comando habitual, com cache/patch ou fallback. A saída e os tempos abaixo são ilustrativos; os gates medidos vivem no roadmap.

### Uso Cotidiano

```bash
# Primeira compilação: cria sessão/CGUs no layout target/<perfil>/incremental/
$ arandu build
Compilando projeto (frio: 48 funções, 3 módulos)
Linkando artefato final: target/debug/meu_app [142ms]

# O desenvolvedor edita apenas o corpo de uma função
$ arandu build
Reutilizando cache incremental (47 funções em cache, 1 modificada)
Codegen Cranelift: 1 CGU regerada
In-process linker: patch exato ou fallback seguro
```

### Garantia de Reprodutibilidade
A equivalência byte a byte é verificada com os mesmos inputs, toolchain/runtime, target, opções e demais entradas relevantes. Não se promete o mesmo binário entre targets/ABIs diferentes:

```bash
$ sha256sum target/debug/meu_app
a8f5c3... target/debug/meu_app
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### Fase 0: Persistência Incremental entre Execuções do CLI
- **Estratégia de Armazenamento:** estado JSON atômico com schema versionado. Cada input registra `mtime`, tamanho, BLAKE3 de conteúdo e, para `.aru` válido, um hash do fluxo de tokens sem trivia/doc comments.
- **Closure de Inputs:** manifesto, lockfile, fontes do pacote e dependências locais/remotas materializadas, stdlib, executável do compilador e archive da runtime.
- **Invalidação Segura:** o caminho do artefato deve ser relativo ao perfil e não conter `..`. Primeiro, toda a closure de inputs deve cortar; somente então o executável é relido e seu BLAKE3 deve coincidir antes de aceitar o cutoff integral. Assim, uma mudança semântica não paga o hash de um artefato que será substituído. Uma edição apenas documental termina antes das queries; uma edição semântica cria uma DB nova e segue para análise/codegen. O grafo Salsa em memória não é persistido.

### Fase 1: Codegen Particionado (CGUs) no Cranelift
- **Unidade de Codegen (CGU):** Uma CGU corresponde a uma função de nível superior (`SymbolId`) ou a um agregado estático de inicializadores.
- **Hash de Entrada da CGU:**
  $$\text{CGU\_Hash} = \text{BLAKE3}(\text{schema} \parallel \text{toolchain} \parallel \text{AMIR canônica} \parallel \text{layouts} \parallel \text{literais} \parallel \text{assinaturas chamadas} \parallel \text{target} \parallel \text{opt})$$
- **Cache de Objeto:** o nome usa o digest completo. Um sidecar atômico registra o digest do objeto; hits exigem hash, formato relocável e arquitetura corretos. O GC remove órfãos antigos quando os objetos ultrapassam 500 MiB.
- **Cutoff de Artefato:** no Linux x86-64, se todas as CGUs são hits, o executável corrente passa sua verificação BLAKE3 e o layout prova igualdade exata entre a closure anterior e a atual, o CLI reutiliza o artefato sem chamar um linker. A remoção de uma CGU também invalida essa prova. Caso qualquer CGU ou a closure mude, segue para patch seguro ou link completo.

### Fase 2: Linker In-Process via `mmap` (Linux ELF x86_64)
- **Layout Verificado:** o ELF recém-linkado fornece os offsets exatos de `.text`, símbolos e GOT. O arquivo de layout é cache não confiável e inclui o BLAKE3 integral do executável.
- **Sobrescrita Cirúrgica (In-Place Patching):**
  1. O CLI copia o executável verificado para staging e o patcher mapeia essa cópia com `mmap(..., PROT_READ | PROT_WRITE, MAP_SHARED)`; o publicado permanece imutável.
  2. Somente funções com tamanho exatamente igual ao slot anterior podem ser copiadas. Crescimento, redução ou mudança estrutural faz fallback.
  3. As relocações daquela função são recalculadas contra os endereços absolutos já estabilizados das outras funções e da runtime.
  4. Relocações para `.rodata`, seções ou formas desconhecidas nunca são aproximadas: fazem fallback para link completo.
- **Metadados ELF:** builds de desenvolvimento patcháveis desabilitam `.note.gnu.build-id`, pois o GNU `ld` documenta que o identificador não muda quando outro programa altera o arquivo depois do link ([GNU ld, `--build-id`](https://sourceware.org/binutils/docs/ld.html)).
- **Fallback Automático:** Para releases de produção (`--release`) ou arquiteturas não suportadas pelo linker embutido (ex: Windows MSVC, macOS), o CLI utiliza o caminho de emissão tradicional via linker do sistema (`cc`/`mold`).

### Fase 3: Auditoria de Determinismo Ponta a Ponta
- Coleções que influenciam bytes usam ordem canônica; `SymbolId` conserva o par `{file_id, local_id}` e não se confunde com o alocador monotônico de FileId.
- Proibição absoluta de iteração sobre coleções com hashing não-determinístico (`std::collections::HashMap`) em rotas que influenciem a geração de bytes. Adoção mandatória de `IndexMap`, `BTreeMap` ou vetores ordenados por ID canônico.

### Fase 4: Harness de Medição Científica
- Criação de uma tarefa `xtask bench-incremental` que:
  1. Simula fluxos de edição reais (mutação de docstring, mutação de corpo de função privada, mutação de assinatura pública, adição de novo símbolo).
  2. Valida se o SHA-256 do binário resultante é idêntico sob diferentes contagens de threads (`RAYON_NUM_THREADS=1` vs `RAYON_NUM_THREADS=16`).
  3. Mede a taxa de cache-hit/cache-bust, a latência de ponta a ponta e os
     timers internos por fase em um relatório estruturado.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### Preservação dos Invariantes de `AGENTS.md`
- **Invariante 1 (Pureza das Queries):** As queries do Salsa continuam estritamente puras, sem I/O ou efeitos colaterais. A camada de persistência em disco vive no orquestrador do CLI/DB (`arandu_query` / `arandu_cli`), nunca dentro de funções `#[salsa::tracked]`.
- **Invariante 2 (Estabilidade de Identidades):** `SymbolId` permanece `{ file_id, local_id }`. Nenhuma identidade efêmera de linker ou de thread pode vazar para o modelo intermediário.
- **Invariante 3 (Early Cutoff Preservado):** A particionamento em CGUs reforça o early-cutoff: edições no corpo de uma função não podem invalidar o código de máquina de funções vizinhas.

### Desvantagens e Trade-offs
- **Complexidade de Linker ELF:** Implementar relocações ELF x86_64 exige manutenção cuidadosa de modelos de chamada e GOT/PLT para a runtime do Arandu.
- **Espaço em Disco:** Diretórios de cache incremental exigem políticas rigorosas de expiração (LRU/GC) para evitar crescimento descontrolado.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### Alternativa A: Chamar sempre o `mold` como processo externo
- *Por que foi rejeitada como solução primária?* Embora o `mold` seja extremamente rápido para links em lote (batch), ele ainda exige serializar arquivos `.o` em disco, invocar um novo processo pelo kernel do Linux e remapear toda a runtime da linguagem a cada comando. Um linker in-process corta o tempo de link de ~100ms para ~3ms.

### Alternativa B: Persistir o grafo de queries e resultados intermediários
- *Por que foi rejeitada nesta fase?* Exigiria um formato estável para HIR/AMIR, tipos internados e dependências de query. O `rustc` usa fingerprints estáveis para reencontrar nós do grafo de uma sessão anterior ([Rust Compiler Development Guide](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation-in-detail.html)); o Arandu adota inicialmente um protocolo menor, verificável e fail-closed para cutoff integral e objetos CGU.

---

## 7. Arte Prévia (Prior Art)

- **Rust (`rustc`):** Pioneiro no sistema Red-Green de queries e no particionamento em Codegen Units (CGUs). Serviu de lição sobre o perigo de corrupção de cache e a necessidade de isolar a persistência do grafo interno.
- **Zig:** Demonstrou com sucesso o poder de linkers in-process com edição in-place em binários mapeados na memória (`mmap`), reduzindo o tempo de debug link a zero perceptível.
- **Go (`cmd/link` e `GOCACHE`):** o comando documenta cache concorrente, invalidação por fontes/compilador/opções e limpeza periódica ([documentação de `cmd/go`](https://pkg.go.dev/cmd/go#hdr-Build_and_test_caching)).
- **Mold (Rui Ueyama):** Demonstrou técnicas de layout de seções pré-computado e paralelismo sem travas para escrita em arquivos ELF.

---

## 8. Estado da Implementação e Evidências

O recorte implementado na 0.2.0 (motor quente) vale para builds nativos de
desenvolvimento com Cranelift em Linux x86-64 ELF. O restante segue batch.

### 8.1 Cutoff, CGUs e patch (antes da 0.2.0)

- Cutoff integral verifica a closure de inputs e o digest do executável; cache adulterado é miss e é reparado.
- Em um miss seguro, fingerprints já capturados são transferidos para a nova sessão e o digest calculado pela publicação é reutilizado. Um conjunto integral de hits de CGU reutiliza o executável verificado sem relink.
- O hash CGU não usa `Debug`, spans, offsets de pools ou IDs sintéticos de composição como identidade de máquina. Cobre AMIR, layouts concretos/`repr(C)`, literais por conteúdo, assinaturas de callees, a closure de declarações da função, target, otimização, Cranelift e compilador. Adicionar uma instância ou remover declaração não relacionada preserva objetos irmãos; mudanças de ABI/layout dependido invalidam o caller.
- O patch ELF valida digest, geometria de `.text`, ranges, tamanho exato e relocações antes de escrever; o executável incremental é comparado byte a byte com o clean build do mesmo source final.

### 8.2 Sessão viva (HOT.2)

- A sessão (configuração → inputs → análise → codegen → publicação) vive em `arandu_cli` (`commands/build.rs`, `BuildSession`) e mantém a DB entre builds. Queries continuam puras; o grafo Salsa nunca é serializado.
- Toolchain, target, opções, stdlib, conjunto de fontes ou manifesto diferentes reabrem a sessão. O refresh lê só as entradas cujo stamp (`dev/ino/size/mtime_ns/ctime`) mudou ou caiu na janela de 2 s antes do scan anterior ("racy git").
- Os fingerprints de entrada que um build registra são capturados antes de compilar e fixados no texto que a sessão compilou: um arquivo salvo depois da leitura nunca é registrado como entrada do artefato publicado (`recorded_inputs_are_the_texts_the_build_compiled`).

### 8.3 Serviço local (HOT.3) e recuperação (HOT.5)

- Um daemon por projeto e toolchain (`daemon/`), com até quatro sessões em LRU chaveadas por diretório e argumentos. Endpoint `$XDG_RUNTIME_DIR/arandu/<hash>.sock` (ou `/tmp/arandu-$UID/`, criado com 0700), modo 0600, credenciais do par verificadas; o lock `.arandu/daemon.lock` prova que não há outro daemon.
- Identidade: versão, protocolo, executável (`dev/ino/size/mtime/ctime`) e variáveis `ARANDU_*`/de build. Divergência faz o daemon antigo sair e o cliente iniciar um novo.
- Protocolo: frames `[magic][versão][tamanho][serde_json]`; diagnósticos viajam como DTO estruturado e o cliente os renderiza. Ler a requisição e escrever a resposta custa ≤ 0,15 ms.
- O accept espera em `poll(2)` no listener e num socket de despertar, sem polling por intervalo; a ociosidade (15 min por padrão) e a parada acordam o mesmo laço.
- Fila por chave: pedidos que esperam o mesmo build recebem um só build com as entradas mais novas; uma requisição nova interrompe o build em curso num checkpoint antes da publicação (no máximo três vezes seguidas). Publicação e registros não são canceláveis, e o cliente só recebe a resposta depois deles.
- Reciclagem por RSS (4 GiB por padrão), até 32 conexões, `--no-daemon`, `ARANDU_DAEMON=0` e batch por padrão com `CI` definida.
- Fail-closed: socket quebrado, identidade divergente, daemon morto ou falha injetada (`ARANDU_DAEMON_FAULT`) levam ao batch; estado publicado só é reutilizado depois de revalidado.

### 8.4 Publicação e imagem dev (HOT.4)

- O executável publicado é mapeado somente leitura; o BLAKE3 de pré-patch é dispensado quando a identidade do arquivo coincide com a que a sessão publicou.
- Slot padding só no perfil dev x86-64 ELF: cada função ocupa `slot_capacity` (classes de 16 bytes, ~1,25× cada), preenchida com `0xCC`. A capacidade depende só do tamanho do código, então o clean build reproduz o layout de um executável patcheado. O patch exige a mesma classe; mudar de classe (crescer ou encolher) faz o link completo.
- A base do patch é copiada por leitura e escrita, não por `copy_file_range`, que no btrfs vira reflink e espera o writeback do executável recém-publicado.
- Durabilidade: saídas derivadas (objetos CGU, sidecars, layout ELF, estado e fingerprint de sessão, compiler-artifacts) são escritas por `replace_derived`, sem `fsync`, porque cada uma é validada por conteúdo na leitura. Um crash custa um rebuild, nunca um resultado errado (`a_torn_build_output_is_rebuilt_instead_of_trusted`). O lockfile, fonte de verdade, continua durável.
- O sidecar de cada CGU registra os bytes de código antes do padding (`code_bytes`), que o benchmark lê para medir quanto uma edição muda cada função.

### 8.5 Medição

`xtask bench-incremental` mede oito classes de edição (inclusive `let` novo) em corpora gerados, com p50/p95, CPU, RSS, caminho de publicação, taxa de patch, delta de bytes por função e retenção de slot, e verifica determinismo entre caminhos e `RAYON_NUM_THREADS=1/16`. Metodologia e resultados em [`docs/benchmarks/`](../benchmarks/README.md).

Endurance (HOT.6): 1000 edições pelo mesmo daemon no corpus medium, com rajadas de 8 clientes: RSS de 244 MB após o build frio e 397 MB após 1000 edições (~12 KB por edição com o programa crescendo), 18 threads do início ao fim, rajadas coalescidas em 92–190 ms, artefato final igual ao do build frio em batch.

### 8.6 Limites

- macOS e Windows fazem link completo; Windows usa batch (sem named pipes na 0.2.0).
- O gate de p95 ≤ 100 ms em edição de corpo não foi certificado no host de referência; nesta máquina o p95 oscilou entre 83 e 134 ms entre sessões, com p50 de 64 a 120 ms.
- Um `let` novo não rebaixa importadores, mas retipa os itens do próprio arquivo (~2,5× uma edição de mesmo tamanho); os locais relativos ao item ficam para a 0.7.x ([RFC 0042](0042-item-relative-spans-and-ast-id-map.md) §8.13).
- Mudança de assinatura pública reexecuta as units que ligam o fechamento transitivo de declarações (~1,2–1,9 s no medium); cortar no chamador direto exige dependências por símbolo (0.3.x).
- A composição ainda faz trabalho por unit (merge do contexto de tipos) e a verificação pull do Salsa visita ~7,5 mil memos por edição.
- CAS global, LLVM e o restante do backlog não são requisitos do motor quente. Esta RFC permanece `Draft`; a fila e os critérios de saída vivem no [roadmap mestre](../arandu-compiler-roadmap-v0.1.md).

## 9. Questões em Aberto (Unresolved Questions)

1. **Gate de latência:** declarar o host de referência e decidir se o p95 da edição de corpo é limite ou registro; o harness já mede o que o gate precisa.
2. **Outras plataformas:** patch in-place para Mach-O e PE/COFF e endpoint por named pipe no Windows.
3. **Composição incremental:** reaproveitar a composição por arquivo quando nenhuma unit dele mudou, sem regredir a regra de último escritor do registro de símbolos.

---

## 10. Possibilidades Futuras (Future Possibilities)

- Suporte a Mach-O (macOS) e PE/COFF (Windows) no linker in-process.
- Publicação de artigo científico ou relatório técnico detalhando as métricas empíricas de determinismo e latência do pipeline incremental completo.
