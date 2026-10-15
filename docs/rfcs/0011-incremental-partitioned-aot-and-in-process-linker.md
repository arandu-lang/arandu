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
3. **Linker In-Process via `mmap` para Linux ELF (Fase 2):** Emissor/patcher ELF nativo embutido que elimina a dependência de processos externos (`cc`, `mold`, `ld`) no ciclo de desenvolvimento, permitindo sobrescrita in-place de seções de máquina em tempo sub-10ms.
4. **Determinismo Ponta a Ponta e Harness de Verificação (Fases 3 e 4):** Garantia de reprodutibilidade bit a bit do binário final, independente de contagem de threads ou ordem de compilação, com harness automatizado de medição de cache-bust.

---

## 2. Motivação (Motivation)

Atualmente, o Arandu possui um dos sistemas de queries incrementais mais granulares da indústria em memória (`crates/arandu_query`), provado em testes como `item_body_cutoff.rs`. No entanto, esse poder sofre de duas barreiras críticas:

1. **Amnésia entre Sessões do CLI:** Cada comando `arandu build` instancia uma nova `DatabaseImpl`, executa em lote e descarta toda a memória. O early cutoff existe no LSP (processo longo), mas o desenvolvedor no terminal sempre paga o custo de compilação fria.
2. **O Abismo da Última Milha (The Linker Bottleneck):** Ao final da compilação, o compilador emite um único arquivo `.o` volumoso e invoca um linker de sistema externo via fork/exec (`cc`/`clang`/GNU `ld`). O linker externo não tem ciência do grafo Salsa e re-linka 100% dos símbolos, tabelas e bibliotecas a cada pequena edição.

Como resultado, enquanto a análise semântica de uma função alterada leva **2 ms**, o link final consome centenas de milissegundos a vários segundos. Fechar esse elo transforma o ciclo de desenvolvimento do Arandu no mais rápido entre as linguagens compiladas de sistemas.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

Para o desenvolvedor Arandu, a experiência é completamente transparente e instantânea.

### Uso Cotidiano

```bash
# Primeira compilação (fria): cria o cache em .arandu/incremental/
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
O mesmo comando executado em outra máquina ou no CI com o mesmo código-fonte produzirá um binário com o mesmo hash criptográfico:

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
  1. O compilador abre o executável existente com `mmap(..., PROT_READ | PROT_WRITE, MAP_SHARED)`.
  2. Somente funções com tamanho exatamente igual ao slot anterior podem ser copiadas. Crescimento, redução ou mudança estrutural faz fallback.
  3. As relocações daquela função são recalculadas contra os endereços absolutos já estabilizados das outras funções e da runtime.
  4. Relocações para `.rodata`, seções ou formas desconhecidas nunca são aproximadas: fazem fallback para link completo.
- **Metadados ELF:** builds de desenvolvimento patcháveis desabilitam `.note.gnu.build-id`, pois o GNU `ld` documenta que o identificador não muda quando outro programa altera o arquivo depois do link ([GNU ld, `--build-id`](https://sourceware.org/binutils/docs/ld.html)).
- **Fallback Automático:** Para releases de produção (`--release`) ou arquiteturas não suportadas pelo linker embutido (ex: Windows MSVC, macOS), o CLI utiliza o caminho de emissão tradicional via linker do sistema (`cc`/`mold`).

### Fase 3: Auditoria de Determinismo Ponta a Ponta
- Todo o pipeline de AMIR, alocação de IDs e emissão de código de máquina deve obedecer à ordenação monotônica do `SymbolId`.
- Proibição absoluta de iteração sobre coleções com hashing não-determinístico (`std::collections::HashMap`) em rotas que influenciem a geração de bytes. Adoção mandatória de `IndexMap`, `BTreeMap` ou vetores ordenados por ID canônico.

### Fase 4: Harness de Medição Científica
- Criação de uma tarefa `xtask bench-incremental` que:
  1. Simula fluxos de edição reais (mutação de docstring, mutação de corpo de função privada, mutação de assinatura pública, adição de novo símbolo).
  2. Valida se o SHA-256 do binário resultante é idêntico sob diferentes contagens de threads (`--threads=1` vs `--threads=16`).
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

- Cutoff integral verifica a closure de inputs e o digest do executável; cache adulterado é miss e é reparado.
- Em um miss seguro, fingerprints já capturados são transferidos para a nova sessão e o digest calculado pela publicação é reutilizado; compilador, runtime, stdlib e executável não são relidos apenas para duplicar hashes. Um conjunto integral de hits de CGU reutiliza o executável verificado sem relink.
- O hash CGU v3 não usa `Debug`, spans, offsets de pools ou IDs sintéticos de composição como identidade de máquina. Cobre AMIR exaustivamente, layouts concretos/`repr(C)`, literais por conteúdo, assinaturas de callees, a closure de declarações fornecida ao `ObjectModule`, target, otimização, Cranelift e compilador. Hashing e emissão compartilham a closure por função (referências, callbacks e destruidores implícitos), não a superfície inteira do programa. Adicionar uma instância ou remover declaração não relacionada preserva objetos irmãos, com regressões byte a byte; mudanças de ABI/layout dependido invalidam o caller. A closure final do link continua verificada: remover uma CGU pode reutilizar os objetos restantes, mas não o executável que ainda a continha.
- O patch ELF valida digest, geometria de `.text`, ranges, tamanho exato e relocações antes do `mmap`; `msync` e `munmap` são verificados. `.rodata` e metadata hostil exercitam fallback em testes.
- O teste ELF compara o executável incremental byte a byte com clean build para o mesmo source final. Linkers cuja geometria ou metadata não satisfazem a prova exata exercitam o fallback explícito e determinístico em vez de transformar indisponibilidade da otimização em erro de compilação.
- `cargo run --locked -p xtask -- bench-incremental --verify-determinism` mede as cinco mutações, registra timings por fase no schema 2 e falha se SHA-256 divergir entre diretórios e `RAYON_NUM_THREADS=1/16`. A metodologia vive em [`docs/benchmarks/`](../benchmarks/README.md).
- A meta de latência sub-10 ms ainda não foi atingida; por isso esta RFC permanece `Draft` e os tempos não são gate em runners compartilhados.

## 9. Questões em Aberto (Unresolved Questions)

1. **Persistência semântica entre processos:** vale introduzir um daemon/servidor de build para conservar a DB Salsa, ou um formato estável de resultados por item?
2. **Latência:** quais custos de inicialização, leitura integral de integridade e análise dominam o ciclo após o cutoff correto?

---

## 10. Possibilidades Futuras (Future Possibilities)

- Suporte a Mach-O (macOS) e PE/COFF (Windows) no linker in-process.
- Publicação de artigo científico ou relatório técnico detalhando as métricas empíricas de determinismo e latência do pipeline incremental completo.
