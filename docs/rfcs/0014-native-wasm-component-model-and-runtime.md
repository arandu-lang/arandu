# RFC 0014: Backend Wasm, Component Model e integração de runtime

- **Número da RFC:** 0014
- **Autor(es):** Equipe Arandu
- **Data de Início:** 2026-09-13
- **Status:** `Draft`; backend core e recorte de componentes implementados
- **Área Principal:** Backend / interop

## 1. Resumo

O crate `arandu_backend_wasm` já emite Wasm diretamente da AMIR, com
`wasm-encoder`, sem passar pelo backend C ou LLVM. Também existem geração WIT e
adaptação Canonical ABI para o subconjunto público suportado. Esta RFC permanece
aberta para a integração mais ampla de host, capacidades e concorrência; não é
uma proposta de criar um backend ainda ausente.

A motivação original de 13/09/2026 incluía emissão sub-15 ms, hot reload no
navegador, zero-copy universal e workers com memória compartilhada. São ideias
condicionadas a projeto e medição, não garantias obtidas pelo backend atual.
A fila de evolução pertence ao
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md).

## 2. Motivação e fronteiras

Emissão direta permite controlar layout e instruções sem uma toolchain C
intermediária. Component Model fornece uma fronteira tipada, mas não torna o
mesmo módulo automaticamente executável em qualquer navegador, engine, versão
WASI ou ambiente edge. Imports e capacidades precisam ser fornecidos pelo host.

A Canonical ABI pode alocar e copiar strings, listas e registros. Um descritor
`ptr + len` na memória linear não equivale a compartilhar o buffer JavaScript
sem cópia nem comprova ownership entre host e guest. O layout interno de AMIR e
a ABI canônica são contratos distintos, conectados por adapters.

## 3. Superfície implementada e uso

As ferramentas de inspeção existentes são:

```bash
arandu emit-wasm main.aru > main.wasm
arandu emit-component main.aru > main.component.wasm
```

Em pacote, `build --target wasm32-unknown-unknown` seleciona módulo core;
`build --target wasm32-wasip1` segue a seleção atual de componente do driver.
Esse nome de target não promete cobertura geral de todos os imports WASI.
`--size`/`-Oz` usa o perfil de tamanho; Binaryen é pós-processamento opcional
de **módulos core**. Componentes não passam pelo otimizador de módulos core.
A seleção exata fica em
[`commands/build.rs`](../../crates/arandu_cli/src/commands/build.rs) e
[`wasm_opt.rs`](../../crates/arandu_cli/src/wasm_opt.rs).

`emit_component` deriva WIT de funções públicas suportadas, prepara os exports
e encapsula o módulo core com `wit-component`. Quando não há funções públicas,
o caminho retorna o módulo core; o nome do comando sozinho não comprova que o
resultado tenha a camada component.

## 4. Arquitetura e evidência atual

| Área | Implementação canônica | Contrato e limite |
| --- | --- | --- |
| Emissão | [`lib.rs`](../../crates/arandu_backend_wasm/src/lib.rs), [`emit.rs`](../../crates/arandu_backend_wasm/src/emit.rs) | Emissão pura a partir de AMIR, símbolos, tipos e layout |
| CFG estruturado | [`stackify.rs`](../../crates/arandu_backend_wasm/src/stackify.rs) | RPO, dominadores, regiões de loop e scopes de branches preservando block args; não depende de `asyncify` |
| WIT | [`wit_gen.rs`](../../crates/arandu_backend_wasm/src/wit_gen.rs) | Projeção do subconjunto de tipos/exports admitidos, não toda interface arbitrária da linguagem |
| Component adapter | [`emit/component.rs`](../../crates/arandu_backend_wasm/src/emit/component.rs) | Metadata, nomes e assinaturas Canonical ABI derivados do WIT |
| Memória/ABI | [`canonical.rs`](../../crates/arandu_backend_wasm/src/canonical.rs), [`memory.rs`](../../crates/arandu_backend_wasm/src/memory.rs) | Alocação, alinhamento, realocação com cópia e post-return conforme o caminho; não há tamanho fixo universal do allocator |
| Dados e agregados | [`translator/aggregate.rs`](../../crates/arandu_backend_wasm/src/translator/aggregate.rs) | Storage próprio, backing estático quando provado e cópias exigidas por valor/lifetime |

[`e2e_component.rs`](../../crates/arandu_backend_wasm/tests/e2e_component.rs)
contém validação, execução em Wasmtime, strings, records, allocator e
realocação. Esses testes são evidência do corpus coberto, não de compatibilidade
universal em browsers ou hosts de componentes.

A memória linear Wasm impõe limites ao endereço global da memória, mas isso não
valida automaticamente cada objeto, geração, lifetime ou autoridade do host.
GenRef continua exigindo seus checks; não se pode removê-los apenas porque o
Wasm possui bounds checking. O allocator atual inclui reutilização e alinhamento,
sem promessa de zero fragmentação ou ausência de alocação residual.

## 5. Integração futura de runtime

Workers de navegador, memória compartilhada, atomics e sincronização de jobs
não estão entregues como extensão completa de `std.parallel` ao browser.
Uma eventual implementação deve definir inicialização do host, suporte da
engine, isolamento necessário, ownership, cancelamento, shutdown e testes de
progresso. Tipagem e bounds checking não provam risco zero de races.

Hot reload e um compilador no browser também exigem protocolo, persistência e
workload próprios. Salsa em memória não garante emissão sub-15 ms, atualização
por função de todo módulo ou carregamento instantâneo. Não há gate vigente de
15 ms ou tamanho máximo de 20 KiB para um componente arbitrário.

## 6. Invariantes e tradeoffs

- O backend permanece consumidor puro; `arandu_query` é o dono da execução
  Salsa, e o driver controla filesystem e otimização externa.
- Layout é do alvo wasm32. Não se pode usar ABI host como substituta da
  Canonical ABI nem publicar ponteiros emprestados sem contrato de lifetime.
- Falhas de representação/validação devem emitir diagnósticos. Isso é requisito
  de implementação, não prova universal de ausência de ICE/panic.
- Determinismo requer mesmas entradas, target, opções e toolchain; um hash
  coincidente não valida todos os efeitos do programa.
- Imports de host e custos da ABI devem permanecer explícitos. Um componente
  pode precisar de adapters/runtime; não há promessa de import JavaScript
  direto e sem glue em todo navegador.

## 7. Possibilidades futuras

Capacidades WASI ampliadas, integração browser e GPU podem ser propostas
separadas. A existência de emissão core/component não aprova essas entregas nem
fixa automaticamente uma versão para cada uma. Prioridades e gates ficam no
roadmap, com evidências específicas por host.


## Alcance e imports: atualização de 2026-10-17

Todos os perfis core Wasm e Component Model aplicam a mesma closure de funções
antes de construir imports. As raízes atuais são `main`, funções públicas/internas
e entrypoints de tooling; bibliotecas sem `main` preservam definições privadas
para lookup do host. `@Export` não é uma sintaxe introduzida por esta mudança.
Referências a funções e dependências de drop são conservadas; metadados
incompletos mantêm dependências conservadoramente. Chamadas indiretas genéricas
ainda não têm lowering Wasm e não são habilitadas por essa análise. WIT usa a
mesma seleção de externs vivos. O perfil padrão conserva `cabi_realloc` e
`memory`; size pode omitir helpers não usados. PRNG determinístico e operações
puras podem instanciar sem imports; efeitos vivos continuam exigindo hosts.

O núcleo SIMD de 128 bits da RFC 0038 usa `v128` em funções e memória core.
Fronteiras vetoriais públicas/importadas de componentes são rejeitadas até haver
um mapeamento canônico explícito; não são rebaixadas silenciosamente para `u32`.
