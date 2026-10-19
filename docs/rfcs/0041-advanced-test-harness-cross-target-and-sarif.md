# RFC 0041: Harness de Testes Avançado, Execução Cross-Target (WASI/Wasmtime), Snapshot Testing, Cache Salsa e Diagnósticos SARIF (`arandu_testing_contract v2`)

- **Número da RFC:** 0041
- **Título:** Harness de Testes Avançado, Execução Cross-Target (WASI/Wasmtime), Snapshot Testing, Cache Salsa e Diagnósticos SARIF (`arandu_testing_contract v2`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-18
- **Status:** `Draft`
- **Área Principal:** `Tooling` / `Testing` / `CLI` (`arandu_cli`, `arandu_testing_contract`)
- **Documentos Relacionados:**
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/rfcs/0003-structured-parallelism.md`
  - `docs/rfcs/0005-incremental-query-system-salsa.md`
  - `docs/rfcs/0014-native-wasm-component-model-and-runtime.md`
  - `docs/rfcs/0022-holistic-synthesis-differential-fuzzing.md`

---

## Fronteira de implementação e evidência

> **Estado auditado em 2026-10-18:** O `arandu test` atual (v0.1) executa exclusivamente
> através do dev JIT do Cranelift no host (`CraneliftDev`), com isolamento em
> processos filhos (`--harness-child`), saída JSON/JUnit e discovery estruturado
> para IDEs. O `arandu check` valida via Salsa queries emitindo diagnósticos de
> terminal via Miette. Esta RFC especifica a expansão integrada para:
> 1. Execução de testes em alvos Wasm/WASI através de engine Wasmtime embutida;
> 2. Asserções de snapshot testing (`assert_snapshot`) com atualização controlada;
> 3. Cache criptográfico de resultados de testes baseado em Salsa e BLAKE3 (`cached`);
> 4. Retentativas para testes instáveis (`--retries N`) e grupos de exclusão mútua (`@Test(group = "...")`);
> 5. Verificação estática e dinâmica de zero-alocação (`@Test(maxAllocs = 0)`);
> 6. Death tests determinísticos para validação de traps de hardware (`@Test(shouldTrap = true)`);
> 7. Fuzzing baseado em propriedades integrado ao CLI (`--fuzz`);
> 8. Emissão de relatórios SARIF v2.1.0 no `arandu check` para CI/CD;
> 9. Execução paralela multithread de testes (`--jobs N`).

---

## 1. Resumo (Summary)

Esta RFC consolida as melhores inovações dos principais runners de teste modernos (**Cargo Nextest, Go Test, Vitest e Google Test**), unindo-as à arquitetura nativa do Arandu (Salsa incremental, modelo de memória OSSA e isolamento de processos).

A proposta estabelece um ambiente de testes de alta performance governado pelo `arandu_testing_contract v2`, garantindo:
1. **Cache Criptográfico Incremental**: Se o código-fonte de um teste e de suas dependências transitivas não mudaram, o resultado é servido instantaneamente do cache (`(cached) [0.000s]`);
2. **Cross-Testing Wasm/WASI Nativo**: Execução transparente em sandbox Wasmtime sem ferramentas externas;
3. **Robustez de CI**: Retentativas de testes *flaky* (`--retries`) e grupos de concorrência com exclusão mútua (`group = "serial"`);
4. **Disciplina de Recursos OSSA**: Asserções de zero-alocação de heap (`maxAllocs = 0`) e validação de traps fatais de hardware (`shouldTrap = true`);
5. **Produtividade de Desenvolvimento**: Snapshot testing com revisão visual de diffs e exportação de diagnósticos no padrão SARIF v2.1.0.

---

## 2. Motivação (Motivation)

### 2.1 Por que Copiar os Melhores Runners do Mundo?
1. **O Modelo do `go test` (Test Caching)**:
   Em projetos grandes com milhares de testes, reexecutar testes de módulos que não foram alterados é um desperdício massivo de tempo e energia. O Arandu já possui o motor Salsa com content-addressing via BLAKE3; ligar esse motor ao runner de testes permite tempos de resposta sub-segundo em monorepos.
2. **O Modelo do `cargo nextest` (Isolamento, Retries e Grupos)**:
   No `cargo test` tradicional, um teste que altera variáveis de ambiente globais ou vaza sockets corrompe os testes vizinhos. O modelo *process-per-test* combinado com retentativas automáticas (`--retries`) e grupos de exclusão mútua resolve definitivamente a fragilidade de testes em CI.
3. **O Modelo do `Vitest / Jest` (Snapshots e Ergonomia)**:
   A validação de estruturas complexas (JSON, HTML, ASTs, relatórios) torna-se trivial com snapshots versionáveis e diffs coloridos inline.
4. **A Necessidade do Modelo de Sistemas (Zero-Allocs e Death Tests)**:
   Em sistemas embarcados, drivers de rede (eBPF) e engines de alta frequência, garantir que um loop crítico **não aloque memória na heap** é um requisito não-funcional inegociável. Além disso, provar que uma operação proibida aborta com o trap correto (`UD2`) é fundamental para a segurança do compilador.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Cache Criptográfico de Testes (`(cached)`)
Ao executar os testes sem alterações nos arquivos fonte:

```bash
$ arandu test
test tests/math_test.aru::testarSoma       ... ok (cached) [0.000s]
test tests/net_test.aru::testarConexao     ... ok (cached) [0.000s]
test tests/parser_test.aru::testarLexer    ... ok [0.014s] (1 executado, 2 em cache)

Suíte de testes concluída em 0.016s.
```

Para forçar a reexecução completa de todos os testes ignorando o cache:
```bash
$ arandu test --no-cache
```

### 3.2 Grupos de Concorrência e Retentativas (`--retries`)
Para testes que dependem de portas de rede ou hardware e que não podem rodar em paralelo entre si:

```arandu
import std.testing as t

// Teste pertencente ao grupo "porta_serial": roda isolado de outros do mesmo grupo
@Test(group = "porta_serial")
public func testarComunicacaoPorta() {
    let dev = abrirDispositivoSerial("/dev/ttyUSB0")
    t.assert(dev.estaAtivo())
}
```

No terminal, se houver instabilidade passageira de rede:
```bash
$ arandu test --retries 2
test tests/net_test.aru::testarDownload ... PASSED (flaky, 2 retries)
```

### 3.3 Verificação de Zero-Alocação de Heap
Garantindo que um algoritmo de alta performance permaneça livre de alocações:

```arandu
import std.testing as t

@Test(maxAllocs = 0)
public func testarProcessamentoCritico() {
    var buffer: [1024]u8
    processarPacoteEmLinha(mut ref buffer)
    // Se qualquer função interna fizer 'alloc' ou expandir String na heap,
    // o teste FALHA com relatório da alocação ofensora!
}
```

### 3.4 Death Tests (Validação de Traps e Aborts)
Provando que uma violação de invariante aborta o hardware com segurança:

```arandu
@Test(shouldTrap = true)
public func testarEstouroDeAritmeticaSegura() {
    let a: i32 = 2147483647
    let b: i32 = 1
    let c = a + b // Overflow em debug mode: aborta com trap UD2/BRK!
}
```

### 3.5 Snapshot Testing com Revisão
```arandu
import std.testing as t

@Test
public func testarRenderizacaoTemplate() {
    let html = renderizarComponente()
    t.assertSnapshot(html, "template_principal")
}
```

Se a saída for alterada e aprovada pelo desenvolvedor:
```bash
$ arandu test --update-snapshots
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Algoritmo do Cache de Testes com Salsa
1. Para cada caso de teste descoberto (`DiscoveryCase`), o compilador consulta a query Salsa `test_execution_digest(test_id)`;
2. O digest é o hash BLAKE3 de:
   - Hash do binário de teste compilado;
   - Hashes de conteúdo de todos os arquivos do módulo e suas dependências;
   - Variáveis de ambiente declaradas e flags de compilação (`target`, `opt_level`);
3. Se o digest coincidir com o registro em `target/.arandu_cache/test_manifest.db` e o arquivo de snapshot (se houver) não tiver mudado, o teste é marcado como `Cached` e seu resultado anterior é emitido instantaneamente.

### 4.2 Monitor de Alocações (`AllocTracker`)
Para implementar `@Test(maxAllocs = N)`:
1. No modo `--harness-child`, o runtime do Arandu instala um hook no alocador do sistema (`arandu_alloc` / jemalloc / mimalloc);
2. Antes de invocar a função `@Test`, o contador de bytes e blocos alocados na thread é zerado;
3. Se o total de alocações exceder $N$, o teste falha reportando:
   ```text
   AssertionError: memory allocation budget exceeded.
   Expected at most 0 allocs, but 3 allocations (144 bytes) were recorded.
   Offending trace: String.concat() at parser.aru:45
   ```

### 4.3 Agendador de Grupos de Exclusão Mútua
O scheduler de testes do CLI mantém filas de trabalho segregadas:
- **Fila Paralela Global**: Testes sem grupo atribuído rodam com concorrência $N = \text{threads}$;
- **Filas de Grupo (`NamedGroup`)**: Testes com o mesmo `group = "nome"` adquirem um lock de grupo antes de serem despachados para um worker, garantindo execução estritamente sequencial entre membros do mesmo grupo sem travar os testes independentes.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

- **Pureza das Queries Salsa:** A query de digest de testes é pura e determinística. O armazenamento do histórico em disco é gerenciado exclusivamente pela camada do CLI/driver fora das queries puras.
- **Isolamento de Processos Preservado:** Death tests e checagens de alocação rodam estritamente em processos filhos (`--harness-child`), prevenindo poluição de memória ou interceptação perigosa de sinais no processo principal.

---

## 6. Racional e Comparativo com Outros Runners

| Recurso | Cargo test (padrão) | Cargo Nextest | Go test | **Arandu test (RFC 0041)** |
|---|---|---|---|---|
| **Modelo de Execução** | Threads compartilhadas | Process-per-test | Processos por pacote | **Process-per-test isolado (`--harness-child`)** |
| **Test Caching** | ❌ Não possui | ❌ Não possui | ✅ Por pacote (SHA-256) | ✅ **Fino por teste (Salsa + BLAKE3)** |
| **Wasm Cross-Testing** | ❌ Manual | ❌ Manual | ❌ Manual via Node | ✅ **Embutido nativo (Wasmtime)** |
| **Retentativas (Flaky)** | ❌ Não | ✅ Sim (`--retries`) | ❌ Não | ✅ **Sim (`--retries N`)** |
| **Grupos de Exclusão** | ❌ Não | ✅ Sim (`test-groups`) | ❌ Não | ✅ **Sim (`@Test(group = "...")`)** |
| **Verificação de Alocação** | ❌ Manual | ❌ Manual | ✅ `-benchmem` | ✅ **Declarativo (`maxAllocs = 0`)** |
| **Death Tests** | ❌ Limitado | ❌ Limitado | ❌ Não | ✅ **Nativo (`shouldTrap = true`)** |
| **Snapshot Testing** | ❌ Requer crate | ❌ Requer crate | ❌ Manual | ✅ **Nativo (`assertSnapshot`)** |
| **Relatório SARIF** | ❌ Não | ❌ Requer plugin | ❌ Não | ✅ **Nativo no `check`** |

---

## 7. Questões em Aberto

1. **Invalidação de Cache por Acesso a I/O Externo:** Testes que acessam o sistema de arquivos ou rede devem declarar `@Effects(FileSystem, Network)` para que o cache seja automaticamente desabilitado para eles? *(Recomendação: Sim, testes com efeitos de ambiente não devem usar cache por padrão)*.
2. **Limite de Retentativas Global:** Limitar `--retries` a no máximo 5 para evitar mascarar bugs estruturais graves em CI.
