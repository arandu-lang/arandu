# RFC 0027: Redução de Código e Runtime Sob Demanda

- **Número:** 0027
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-10-12
- **Status:** `Implemented`
- **Área:** Backend / Runtime / Tooling
- **Contratos relacionados:** [backend](../arandu-backend-contract-v0.1.md),
  [CLI/LSP](../arandu-cli-lsp-contract-v0.1.md),
  [freestanding](0017-lean-freestanding-core-architecture.md),
  [emissão C](0018-pretty-idiomatic-c-codegen.md).

## 1. Contexto e objetivo

Inlining, funções inacessíveis, literais abandonados e helpers de runtime
podem aumentar o código emitido antes do trabalho do linker. Esta RFC reduz
esses custos e oferece um perfil explícito de tamanho, mantendo ownership,
layout, callbacks, destrutores e ABI.

O backend C já seleciona famílias de runtime sob demanda e compartilha
temporárias escalares por interferência. Esta implementação consolida a análise
de alcançabilidade no MIR, aplica-a à emissão AOT e ao perfil Wasm de tamanho,
congela interpolações literais e conecta `--size` / `-Oz` ao CLI.

Não existe um tamanho absoluto garantido para executáveis. Formato do objeto,
ABI, linker, bibliotecas utilizadas e conteúdo do programa determinam o
resultado. A RFC também não converte automaticamente campos de struct ou
cascatas de condições em arrays: isso pode alterar semântica e layout públicos.
O corpus SHA usa arrays explicitamente no código Arandu.

## 2. Uso

```bash
# Dentro de um pacote com arandu.toml:
arandu build --size
arandu build --target wasm32-unknown-unknown -Oz

# Ferramentas de inspeção para um arquivo:
arandu emit-c main.aru --size > main.c
arandu emit-wasm main.aru --size > main.wasm
arandu emit-component main.aru --size > main.component.wasm
```

`--size` e `-Oz` são aliases. O perfil é separado de `--release` e de
`--debug`; combinações incompatíveis são erros de uso. `run` mantém o JIT de
desenvolvimento e rejeita esses perfis de build. Artefatos e proveniência
incremental de tamanho usam `target/size`, com identidade de cache distinta dos
perfis `dev` e `release`.

O perfil aplica o pipeline AMIR `Os`, que conserva chamadas de helpers em vez
de executar o inliner de velocidade. Cranelift AOT recebe `speed_and_size`.
No Linux, o linker mantém a coleta de seções mortas e remove símbolos do
executável final. Isso reduz informação para depuração; `--debug` permanece
separado. A otimização participa também dos hashes das unidades de codegen.

Para builds de módulos Wasm core, o CLI pode executar Binaryen com `-Oz`
quando a ferramenta estiver disponível. Emissão direta por `emit-*` usa o
pipeline interno. Componentes conservam sua ABI canônica e não são enviados
ao otimizador de módulos core.

## 3. Contratos de implementação

### 3.1 Alcançabilidade compartilhada

`arandu_mir::reachability::live_functions` é uma análise pura de AMIR,
símbolos e layout. Em executáveis, as raízes são:

- `main`, funções públicas e funções com linkage interno;
- entradas explícitas de ferramentas, incluindo `@Test` e `@Benchmark`;
- funções transitivamente referenciadas, inclusive callbacks em argumentos
  de terminadores;
- destrutores implícitos dos tipos das funções alcançáveis.

Os visitors canônicos de statements e terminadores determinam as referências.
A emissão mantém a ordem do programa, sem depender da iteração de conjuntos
hash. Sem `main`, todas as definições são preservadas: inspeção de bibliotecas
e lookup privado pelo host continuam válidos.

C e Cranelift AOT usam esse fechamento. O JIT conserva todas as definições.
Wasm aplica a seleção explicitamente nas APIs de tamanho; as APIs existentes
mantêm seus exports de inspeção e o contrato de realocação canônica.

### 3.2 Runtime e dados Wasm sob demanda

No perfil Wasm, funções e imports inacessíveis são omitidos, junto dos
metadados WIT correspondentes. Literais são coletados com os mesmos visitors
nas funções emitidas, mantendo IDs estáveis e ordem determinística.

O corpo traduzido determina se chama alocação ou liberação, inclusive em frames
de agregados promovidos ao heap. O par de helpers do allocator é emitido quando
necessário. Um módulo core de tamanho sem essas operações não exporta
`cabi_realloc`. Componentes preservam helpers, `cabi_realloc` e os caminhos de
post-return necessários à ABI de strings; chamadas implícitas da ABI também
são obrigações de retenção.

C continua selecionando dependências transitivas das famílias de runtime,
incluindo GenRef, formatação, paths, ambiente e filesystem. A seleção ocorre
por operações emitidas, não apenas pela presença de um import. Seu pool de
literais permanece conservador; não se promete coleta completa de dados
estáticos nesse backend.

### 3.3 Interpolação literal

Antes da elaboração de ownership, o lowering reconhece interpolações cujas
partes são literais de string, inteiro, bool ou char, incluindo inteiros
negativos e interpolações literais aninhadas. O resultado entra no pool como
string estática, sem `ToStr`, concatenação alocada nem `Free` correspondente.

Inteiros são validados segundo seu tipo e `DataLayout`; a representação decimal
coincide com a conversão dinâmica. UTF-8 e NUL embutido conservam bytes e
comprimento. Limites de profundidade e tamanho fazem a análise retornar ao
lowering normal quando não consegue provar o resultado.

Expressões com efeitos e floats permanecem dinâmicos. Os backends ainda
precisam de um contrato único de formatação de float antes de uma transformação
compartilhada dessa conversão.

### 3.4 Dados constantes e temporárias

O serializador estático existente fornece backing imutável para arrays,
tuplas, structs e enums, usando layout do alvo. Agregados mutáveis continuam
recebendo storage próprio e cópia quando exigidos pela semântica de valor.
Não se cria uma variante AMIR paralela nem se muda o lifetime de strings.

O coloramento existente compartilha temporárias C de tipo exato e temporárias
Wasm de classe compatível quando a interferência permite. Locais do usuário,
endereços observáveis, parâmetros e retorno conservam suas restrições.
O fallback para funções grandes limita o custo do grafo. Não se exige um
número fixo de slots: liveness determina o mínimo seguro.

No Wasm, um agregado `Copy` serializado com definição única e um único uso
comprovado pode alimentar outro inicializador sem storage intermediário.
O ponteiro estático não escapa: o consumidor copia para seu próprio storage
mutável ou já contém todos os bytes no inicializador serializado. Retornos,
referências, múltiplos usos e consumidores não comprovados mantêm o caminho
normal. A prova considera também operandos de terminadores e o retorno
implícito em `TempId(0)`.

O allocator Wasm garante alinhamento de 16 bytes para payloads tanto em blocos
novos quanto reutilizados. Header e tamanho arredondado mantêm esse invariante;
overflow de arredondamento ou endereço falha antes da escrita. `cabi_realloc`
aceita alinhamentos potência de dois entre 1 e 16. Isso corrige retornos de
records com campos `u64` na ABI canônica, sem remover suas cópias obrigatórias.
O crescimento compara páginas diretamente até o limite de 65.536 páginas,
sem converter 4 GiB em um contador de bytes de 32 bits. A liberação limpa o
campo de geração separado do link da freelist; handles GenRef antigos não
permanecem válidos depois de liberar ou reutilizar uma célula.

O passe de pilha pode conservar um valor durante expressões escalares
independentes, dentro de uma janela limitada. As expressões devem balancear
seus próprios operandos antes do uso do valor conservado. Chamadas, memória,
controle de fluxo, operações potencialmente trapping e sobrescritas do local
são barreiras. Locais ainda usados conservam sua escrita via `local.tee`.

### 3.5 Imports e teardown nativos

A emissão de objetos mantém um catálogo de assinaturas dependentes do alvo,
mas declara imports de runtime somente quando a tradução emite uma chamada
ou referência. Isso cobre operações implícitas como alocação e cópia, sem
deixar símbolos undefined não usados extraírem membros da staticlib.
O JIT conserva o lookup eager e sua política de teardown.

O shutdown de GenRef em `main` depende do fechamento alcançável do programa,
incluindo funções públicas, ferramentas, callbacks e destrutores. Código
estrangeiro desconhecido e chamadas indiretas conservam o shutdown. A política
participa da identidade de `main` nas unidades de codegen: editar um helper
para usar GenRef deve invalidar esse epílogo mesmo sem editar `main`.

Os DTOs de resultados de testes e benchmarks pertencem ao crate folha
`arandu_testing_contract`. O codegen reexporta esses mesmos tipos; o runtime
depende diretamente do contrato, sem trazer IR ou Salsa. O gate de arquitetura
verifica esse desacoplamento inclusive nas dependências transitivas.

## 4. Invariantes e decisões

- Análises não fazem I/O nem executam Salsa. O CLI possui os efeitos de
  filesystem, otimização externa e linkagem.
- A poda não altera parsing, resolução, tipagem, diagnósticos nem a superfície
  exportada usada pelo early-cutoff. Código inválido não se torna válido porque
  uma função deixou de ser emitida.
- Visitors, dominância, argumentos de salto e ownership continuam canônicos.
  Referências indiretas, drops e ABI são obrigações de retenção.
- Build de tamanho tem identidade própria; cache de outro perfil não prova que
  o artefato atual está pronto.
- O modo continua hosted. Freestanding é o contrato separado da RFC 0017.
  Strip e coleta de seções não removem dependências de SO usadas pelo programa.
- Determinismo significa mesmos bytes para a mesma entrada, configuração e
  toolchain; backends e alvos diferentes podem produzir artefatos diferentes.

Delegar toda a poda a LTO conserva o custo de traduzir código morto. Por outro
lado, conservar apenas chamadas diretas perde callbacks e operações implícitas
da ABI. O fechamento conservador atende ambos sem novo formato de runtime
ou dependência.

## 5. Validação e medições

Regressões cobrem callbacks em terminadores, ferramentas, destrutores e
bibliotecas sem `main`; omissão de funções em objetos AOT; execução e
determinismo Wasm; ABI real de strings em componentes; interpolações com UTF-8,
NUL e spelling de inteiros; equivalência de `Os` e otimizações de velocidade;
e isolamento de perfis e cache pelo CLI.

Os gates incluem formato, check, Clippy com warnings negados, testes,
documentos de diagnóstico e rustdoc. Arquitetura e finais de linha também são
verificados. A validação local é no Linux; a matriz nativa continua necessária
para promover suporte em outros sistemas.

Medições registram fonte, perfil, toolchain, bytes e execução com resultado
independente. Linhas C não medem ocupação de cache nem ganho proporcional de
tempo. Indexação preserva bounds checks; não há promessa de uma instrução ou
um ciclo por acesso.

### 5.1 Evidência Linux x86-64 — 2026-10-14

Esta primeira medição precede a seleção de imports nativos e a eliminação
de storage intermediário descritas acima; permanece como baseline histórico.

Toolchain: Rust 1.97.1, GCC 16.2.1 e Node 24.20.0. O corpus
[`sha256-array.aru`](../../tests/perf/sha256-array.aru) valida oito vetores SHA-256
com resultados independentes; seu SHA-256 de fonte é
`82617e21ac768f4558d8a13e10d19a6b64a9005531863f18cab20637b31ae9f7`.
Todos os artefatos abaixo foram executados com sucesso. Wasm foi emitido
diretamente e executado no Node, sem Binaryen nem imports de host.

| Programa / artefato | Perfil de velocidade | Perfil de tamanho |
| --- | ---: | ---: |
| Hello, Cranelift AOT | `build --release`: 11.019.304 B | `build --size`: 431.696 B |
| SHA, Cranelift AOT | `build --release`: 11.011.264 B | `build --size`: 429.704 B |
| SHA, Wasm core | `emit-wasm --opt`: 9.683 B | `emit-wasm --size`: 8.097 B |
| SHA, fonte C | `emit-c --opt`: 36.455 B / 1.074 linhas | `emit-c --size`: 35.773 B / 1.088 linhas |
| SHA, executável C | GCC `-O2`: 14.536 B | GCC `-Os`: 14.536 B |

O Hello importa `io` e imprime `hello, arandu`. Sua fonte C ocupa 1.638 bytes
e 64 linhas em ambos os perfis; os executáveis GCC ocupam 14.480 bytes. Os
executáveis C foram ligados com `-ffunction-sections -fdata-sections
-Wl,--gc-sections -s -lm -pthread` e `-fwrapv`.

Os builds Cranelift utilizaram a staticlib Rust do runtime compilada em modo
de desenvolvimento. A diferença nativa inclui a remoção dos símbolos dessa
biblioteca; não representa uma redução proporcional de instruções executadas
nem mede o tamanho de um SDK distribuído. O Wasm SHA caiu 16,4% neste corpus.
Com strip equivalente, o SHA nativo ocupava 429.192 B no perfil de velocidade
e 429.704 B no perfil de tamanho: ainda não havia redução nativa demonstrada.
O executável C manteve seu tamanho, mas sua seção `.text` caiu de 3.425 para
2.349 B; layout e alinhamento do ELF esconderam essa diferença. Bytes emitidos,
linhas, seções executáveis e bytes finais precisam ser medidos separadamente.

As regressões verificam também reconstrução byte-idêntica e reutilização de
cache entre os aliases `--size` / `-Oz`, sem reutilizar o perfil `release`.

### 5.2 Após seleção real de runtime e storage Wasm

Na mesma toolchain e com a mesma fonte, usando diretórios de projeto e caches
novos, as medições passaram a ser:

| Artefato / métrica | Antes | Depois |
| --- | ---: | ---: |
| SHA, Cranelift `--size`, ELF stripped | 429.704 B | 11.440 B |
| SHA, seção `.text` desse ELF | 288.819 B | 4.330 B |
| Hello, Cranelift `--size`, ELF stripped | 431.696 B | 325.192 B |
| SHA, Wasm `--size` | 8.097 B | 7.711 B |
| SHA, seção Code Wasm | 5.679 B | 5.293 B |
| SHA, seção Data Wasm | 2.025 B | 2.025 B |

O ganho incremental no SHA foi 97,3% no ELF nativo e 4,8% no Wasm. Todos os
artefatos executaram os oito vetores independentes. A comparação nativa já
remove símbolos de ambos os lados: a queda agora é código e dependências
retidos anteriormente, não apenas debug. A correção de alinhamento Wasm também
está incluída nestes resultados, com suas verificações adicionais.

O perfil de velocidade nativo novo ocupa 10.944 B após strip equivalente,
contra 11.440 B no perfil de tamanho; `speed_and_size` não garante o menor
resultado em cada programa. A seleção de runtime beneficia ambos. O Hello
continua com infraestrutura legítima de IO e tratamento de panic do Rust;
não se removem essas garantias para alcançar um limite arbitrário de bytes.
O backend C conservou os artefatos byte-idênticos nesta etapa.

Regressões adicionais executam cópias mutadas independentemente, referências,
argumentos de salto, `Move`, retornos de records com quatro `u64` em componentes,
realocação, reutilização de blocos e invalidação de GenRef após liberação
genérica. O fragmento de crescimento é executado com casos nos limites de
65.535 e 65.536 páginas, sem reservar uma memória de 4 GiB para esse teste.
A transformação de pilha é comparada à
emissão original com valores extremos e operações não comutativas; o limite
físico da janela também é coberto. Objetos nativos verificam ausência de imports
sem uso e retenção de cleanup por helpers, públicos, callbacks e foreign code;
o hash CGU de `main` acompanha a alteração transitiva dessa necessidade.

## 6. Futuro

Coleta mais precisa de literais C, políticas compartilhadas de formatação de
floats, LTO configurável e runtime freestanding são trabalhos separados.
Especializações por instruções criptográficas do alvo exigem perfil e corpus
próprios. Nenhum deles é pré-condição oculta do perfil aqui descrito.
