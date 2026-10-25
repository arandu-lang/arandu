# Arandu — Arquitetura da Standard Library (Stdlib)

> Documento de especificação de design e layout de pacotes do ecossistema de bibliotecas oficiais da linguagem Arandu.
>
> Objetivo de design: modularidade, tempo de compilação previsível e separação
> das camadas. Bare-metal, `no_std` geral e self-hosting dependem de contratos
> e provas próprios; não são garantias do SDK v0.1.10 nem requisitos aprovados
> automaticamente para a versão 1.0.

---

## Visão Geral e Contexto

Este é o desenho arquitetural das camadas `core`, `alloc`, `std` e extensões.
As árvores conceituais abaixo incluem módulos e APIs futuros; nomes como
`arandu_core` representam camadas, não crates Rust já publicados com esse contrato.
O inventário implementado vem do código em `stdlib/` e dos contratos técnicos.

### Base publicada e evolução local

O SDK v0.1.10 foi publicado e aprovado em Linux x86-64, macOS ARM64 e Windows
x86-64 no [run 38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029),
com integração SDK/VSIX e Public RC install/smoke, incluindo o corpus CTFE.
Essa prova não cobre toda a stdlib nem estabelece paridade async/paralelismo.

O reactor Linux epoll/io_uring, wakers, TCP e `SyncExecutor` já existem. O
executor ainda transporta resultados em bits i64 e seu host standalone C é
incompleto; registro de sockets no reactor não é suportado em Windows/macOS.
O contrato [async](./arandu-async-runtime-design-v0.1.md) detalha esses limites.
O [paralelismo estruturado](./arandu-structured-parallelism-v0.1.md) já admite
`AccumulatorInit` não-`Copy` e possui harness de escala versionado; drop glue
automático e promoção da matriz específica continuam limites distintos.

`std.fs.Dir` agora usa `openat2` no Linux com resolução confinada e sem
symlinks. As regressões de runtime e JIT/C com UBSan rejeitam caminhos
absolutos, parent traversal e fugas por symlinks; rename da raiz preserva
a autoridade do descritor aberto. Kernels Linux sem a syscall falham sem
fallback menos restritivo. macOS/BSD têm um recorte local de um componente por
capacidade, ainda sem prova de execução nativa; Windows (ST.12b) usa o mesmo
recorte com `NtCreateFile` relativo, também sem prova de execução nativa.
Isso não fecha toda a RFC 0016 nem
o lifecycle de I/O. A campanha local 0.1.11 acrescenta Q8.8/Q32.32, intrínsecos
u32/u64 de contagem/reversão de bits, operações checadas de Decimal e protótipos
de Box e hashes SHA-256/BLAKE3. O corpus de hashes e Decimal executou em
Cranelift, C com UBSan e Wasmtime; Box passou em Cranelift e C, incluindo drops
exatos e rejeição de escapes/aliases. Box depende do host de alocação alinhada
nativo; essa API ainda não possui wiring Wasm. O snapshot histórico passou nos
gates locais; essas implementações ainda exigem a matriz instalada e não
pertencem à prova publicada da 0.1.10. `core.atomic`
possui operações reais nos três backends. Ordens válidas são fortalecidas
para SeqCst e ordens inválidas são rejeitadas. Passaram as regressões de
contenção com seis threads nativas, incremento/CAS, larguras, wrap e ponteiros.
Os gates locais do snapshot passaram; a matriz instalada da candidata segue pendente. A existência de Arc não comprova
sincronização de tipos arbitrários: os bounds canônicos exigem Send e Sync do payload. Traps de abort já são emitidos diretamente, mas isso
não fecha sozinho o contrato integral anti-panic-bloat. As convenções da RFC
0026 não comprovam `close(): Result` e drop uniforme para todos os recursos.

### Evidência local desta revisão

A revisão de 18/10/2026 valida os crates e cenários afetados com nextest e
Clippy com warnings como erros. A rodada de integração de `arandu_middle`,
`arandu_typeck`, `arandu_mir`, `arandu_query`, dos três backends e da CLI passou
com **2.175 testes aprovados, zero falhas e três ignorados**, incluindo a tipagem
do grafo completo da stdlib. Inclui autoridade por arquivo, hashes/cutoff,
vetores fonte e acesso desalinhado, empréstimos transitivos de guardas/vistas,
privacidade de Vec, reconstrução unsafe de Box, Arc compartilhado e retenção JIT.
Os testes selecionados de runtime, documentação, web e LSP também passaram;
o cenário stdio sem manifesto e overlays anteriores à descoberta preservam a
origem confiável da stdlib e o texto aberto no editor.
O relatório da entrega registra os resultados executados; não se reutiliza o gate
global abaixo como prova de mudanças posteriores. Não houve publicação.

Como evidência histórica, o snapshot local de 17/10/2026 passou em fmt, check, Clippy com warnings como
erros, nextest (**3.184 aprovados; sete ignorados**, perfil `verification`, oito
threads), bijeção de 102 diagnósticos, rustdoc com warnings como erros e doctests.
Passaram os checks de arquitetura, taxonomia, LF e contrato dos 27 manifests,
além do determinismo de typeck com uma e oito threads. O corpus inclui a tipagem
do grafo completo da stdlib, regressões stdio/LSP, cutoff, SIMD nos três backends,
poda de imports Wasm, ParkingLot/timeout e confinamento de Dir em JIT/C.

Os quatro testes C SIMD também passaram com ASan/UBSan (modo nativo e fallback,
memória/ABI/shuffle e wrapping por via). O harness C ParkingLot passou em O0/O2
com UBSan. O runtime Rust passou em checks para Windows x86-64 e macOS ARM64/
x86-64, com `blake3/pure` e flags de linker do host removidas. Isso verifica
compilação Rust dessas branches, não execução nativa ou compilação dos shims C
nesses sistemas; o `Dir` Windows tem testes `#[cfg(windows)]` para o runner CI.

Como evidência histórica de 16/10, a extensão passou em 26 testes unitários,
nos cenários projeto/manifestless do Extension Host e em TypeScript; o instalador
passou em 22 testes, 11 subtestes e Ruff. O corpus Rc/Arc daquela revisão passou
em JIT otimizado e C O0/O2 com ASan/UBSan.

Essas evidências locais não publicam a 0.1.11 nem encerram ST.0–16. Os testes
manuais/endurance e os pacotes exatos nos runners nativos continuam sujeitos
aos gates da candidata. O contrato de versão ainda identifica a base 0.1.10;
não houve preparação ou promoção de uma nova release nesta revisão.

## Contratos locais de atomics e filesystem por capacidades

`core.atomic` usa declarações de identidade explícita na ABI
`arandu-intrinsic`; nomes coincidentes em funções comuns não recebem lowering
especial. O marcador de identidade é preservado no símbolo e exigido pelo
type checker, lowering, CTFE e backends; o nome apenas seleciona a operação e
seu contrato de assinatura. A mesma regra vale para os demais intrínsecos. O
barrier `blackBox` é reservado ao item canônico `std.testing.blackBox`, não a
qualquer função com esse nome. A tipagem rejeita assinaturas incompatíveis
antes do AMIR. Os intrínsecos de memória são `unsafe`: exigem armazenamento
vivo, naturalmente alinhado e acessado segundo o protocolo atômico durante compartilhamento.
Os campos dos wrappers são privados; suas escritas continuam exigindo
`mut ref self`. Não há promoção automática a mutabilidade interior compartilhada
ou transporte `Send`/`Sync`.

C usa builtins `__atomic_*` e uma asserção de suporte lock-free da largura no
alvo. Cranelift usa instruções atômicas; Wasm usa opcodes atômicos, exigindo
suporte do engine. A memória Wasm atual não é compartilhada: essa prova não
habilita threads Wasm. Todos os acessos e CAS usam SeqCst, inclusive quando o
chamador pede uma ordem mais fraca válida. `compilerFence` em Cranelift/Wasm
é conservadoramente uma fence de hardware; em C usa signal fence.

`Dir` mantém o descritor privado. A abertura da raiz é autoridade ambiente;
operações relativas admitem componentes normais, recusando caminho absoluto,
`..`, componentes vazios e NUL. No Linux, as aberturas usam
`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`. Criação e
remoção pinam o diretório pai e operam sobre uma única folha. Remover uma
folha symlink remove o link, sem seguir seu alvo. Um pai já aberto conserva
sua autoridade se for renomeado; não se promete confinamento por nome após
esse rename. Não há fallback para `openat` comum quando `openat2` falha.
No macOS e nos BSDs suportados pelo código, cada chamada relativa aceita
somente um componente e usa `openat` com `O_NOFOLLOW | O_CLOEXEC`;
diretórios exigem também `O_DIRECTORY` e `fstat` no descritor aberto.
`Dir.openDir` deriva explicitamente a próxima capacidade; caminhos com `/`
falham nesses alvos. Não se oferece uma caminhada multiplataforma com
verificação atômica de ancestralidade entre várias syscalls. No Windows o token
é um descritor CRT (`_open_osfhandle`) que possui o `HANDLE` sem truncamento;
aberturas relativas usam `NtCreateFile` com `RootDirectory`, `OBJ_DONT_REPARSE`
e `FILE_OPEN_REPARSE_POINT`, rejeitando reparse points, ADS, dispositivos e
ponto/espaço finais (RFC 0016 §4.2.3).
`Dir.openDir` exige diretório; opções de append ainda são rejeitadas, assim
como criar/truncar sem escrita. `File.fd` é privado; `File.close` invalida o
descritor com `-1` e o destructor fecha somente descritores ainda abertos,
evitando double close. Não há API pública para adotar handles arbitrários.
Os testes da lógica no Linux não promovem suporte nativo macOS/BSD.

A correção local de `Rc.tryUnwrap` zera a contagem forte antes de mover o
payload, impedindo que `Weak.upgrade` ressuscite o valor extraído. O corpus
de lifecycle cobre unwrap e último drop de Rc/Arc com weaks vivos e payloads
não-Copy; a contagem exata de seis drops foi observada em JIT comum/otimizado e C
O0/O2, incluindo ASan/UBSan com detecção de vazamentos e uso de stack expirado.

`Arc` usa CAS com limite de contagem antes de overflow e um sentinel da
contagem fraca para testar unicidade com exclusão de upgrades concorrentes.
`getMut` continua exigindo empréstimo exclusivo do handle. Arc/ArcWeak canônicos
só são Send/Sync para T:Send+Sync. Isso não concede transporte a Rc, ponteiros
crus, empréstimos de stack ou recursos arbitrários com destructor.

`std.sync.mutex` e `std.sync.rwlock` encapsulam UnsafeCell canônica, não-Copy e
não-Sync. Mutex exige T:Send; RwLock exige T:Send para posse e T:Send+Sync para
compartilhamento. As guardas não são Copy nem Send e retêm o empréstimo do lock.
Leitores entram por ref compartilhada; escritores usam uma fila FIFO, alternada
com lotes de leitores pendentes. Estado e monitores ficam em tabelas por
endereço, não em uma palavra mágica de quatro bytes. Fairness pressupõe término
das seções críticas e progresso do escalonador. Leitura recursiva só é aceita
sem escritor pendente; reentrada bloqueante incompatível aborta com diagnóstico
em vez de deadlockar silenciosamente.

`Condvar.wait` e `waitTimeout` emprestam exclusivamente uma MutexGuard, liberam
a aquisição e sempre a readquirem. A sequência validada sob o bucket fecha a
janela entre liberação e registro; notify recusa wrap antes de publicar a nova
geração. O timeout da condição não limita o tempo posterior de reaquisição.
`waitNotification*` expõe separadamente o evento de sequência. Isso é nativo;
um worker de executor cooperativo não deve bloquear esperando esse protocolo.

`std.thread.spawn<A,R,J>` recebe argumentos e ThreadJob possuídos e Send, incluindo
capturas descritas por campos do job. `spawn(move || ...)` sem parâmetros e com
capturas próprias Send sintetiza essa struct e o método `run()`; closures gerais
continuam rejeitadas. Em alvo bare-metal, `std.thread` e `std.fs` produzem T099.
join devolve o resultado possuído. Drop
aguarda e destrói resultados não reclamados. A barreira de descarregamento JIT
espera execução, descendentes, joins e drop glue ativos antes de liberar código.
O runtime não interrompe threads ou instruções Arandu de forma assíncrona.
`std.sync.cancel_token.CancelToken` oferece um sinal atômico compartilhável;
jobs e corrotinas devem cloná-lo e consultá-lo em checkpoints cooperativos.
Ele não interrompe chamadas bloqueantes nem adiciona checagens implícitas em
`await`. O token interno de `WorkerTask` continua sendo observado apenas antes
da execução. `SyncExecutor.cancel` descarta uma tarefa pendente ou aposenta o
handle depois que uma tarefa em execução retorna; não interrompe nem sinaliza
essa tarefa.
Esse drop pode bloquear indefinidamente se a thread não terminar. Se a própria
thread tentar dropar seu handle, a operação retorna sem consumi-lo; o handle
permanece disponível para o dono ou para a barreira de descarregamento JIT.
Closures gerais não estão implementadas. Traps/abort da linguagem encerram o
processo; ThreadError não promete recuperar unwinding que o compilador não emite.
`park` consome o token de unpark; `parkTimeout` usa prazo monotônico e pode ser
interrompido por unpark, consumindo o mesmo token coalescente.

Os seis tipos canônicos de `std.core.simd` usam ArType::Vector desde a fonte.
Máscaras privadas, comparações e seleção possuem nós AMIR e lowering nos três
backends. Loads recebem []T; stores seguros usam MutableSlice<Owner,T>, afim e
ligada a mut ref Owner. Vec.asMutSlice mantém o proprietário exclusivamente
emprestado; raw constructors e stores por descritor compartilhado exigem unsafe.
Isso impede tratar a cópia de uma fatia imutável como permissão de escrita.
Bounds e acessos escalares permitem endereços sem alinhamento vetorial. Backing
do compilador tem alinhamento mínimo 16, com par de liberação correspondente.

Identidades privilegiadas e a ABI arandu-intrinsic exigem autoridade de origem
concedida pelo loader para cada arquivo da stdlib. Um cabeçalho module std.*
não concede sozinho Send/Sync, representação SIMD ou permissão de intrínsecos.
O input de autoridade é por arquivo e seus marcadores entram no hash semântico
das assinaturas, preservando cutoff entre arquivos independentes.

## Contratos locais de matemática e aleatoriedade

As regressões numéricas usam resultados independentes do host `f64`, mais
representações IEEE exatas para potências de dois subnormais. Cobrem conversões
acima de 32 bits, arredondamento perto de meio e de 2^52, zero com sinal,
subnormais, extremos finitos, NaN e infinitos. `trunc` usa `i64` na faixa
convertível; `round` compara a parte fracionária sem somar 0.5 ao operando;
`exp` divide a escala nos extremos e `cbrt` evita formar cubos intermediários.
Essa prova dos perfis atuais com float de 64 bits não estabelece um limite
universal de ULP, redução trigonométrica de argumentos grandes ou suporte
numérico equivalente para perfis float de 32 bits.

`std.random.Xoshiro256PlusPlus` segue a fórmula xoshiro256++ e expande inclusive
a seed zero por SplitMix64, conforme as
[referências dos autores](https://prng.di.unimi.it/xoshiro256plusplus.c).
O protótipo local anterior usava a fórmula `xoshiro256**` apesar do nome `xoshiro256++`; a correção
altera seu fluxo determinístico antes da promoção da 0.1.11. A faixa inteira
completa usa aritmética unsigned, sem transbordar o cálculo do intervalo.
Vetores determinísticos e casos com rejeição são regressões de implementação;
não constituem execução local de BigCrush nem prova criptográfica.

A bridge de entropia retorna erro se a fonte do SO falhar e limpa qualquer
saída parcial. Unix usa `/dev/urandom`; Windows usa `BCryptGenRandom` em blocos
que respeitam o comprimento DWORD. Não há fallback para relógio, `rand()` ou
seed fixa em uma chamada que reporte entropia do SO. `tryFromEntropy` expõe
falhas de alocação, indisponibilidade e estado inválido; `fromEntropy` aborta
se não puder inicializar. O PRNG continua inadequado para criptografia. Os
imports de entropia/alocação Wasm ainda não estão integrados. O runtime passou em `cargo check -p arandu_runtime --locked --features
blake3/pure` para Windows x86-64 e macOS ARM64/x86-64. A configuração padrão
Windows não pôde ser checada neste host Linux porque o build de BLAKE3 exige
`ml64.exe`. Essa checagem de Rust com a variante pura não comprova o build
padrão, o C gerado ou a execução de BCrypt no Windows. Compilação cruzada não
substitui os runners nativos da matriz de distribuição.

## Detalhes Técnicos da Implementação

### 🧠 Filosofia Central do Projeto

O Arandu prioriza acima de tudo:
1. **Previsibilidade semântica**: Sem mágica implícita de compilador ou runtime;
2. **Locality de memória**: Acesso linear a dados e otimização agressiva de cache;
3. **Throughput de compilação**: perseguir o gate p95 ≤100 ms do build quente em host/workload definidos, sem promessa atual para todo edit-run;
4. **Baixo overhead**: Binários finais extremamente compactos e com zero overhead desnecessário;
5. **Tooling de alta qualidade**: Diagnósticos bonitos, ricos e integrados;
6. **Incrementalidade**: Grafo de queries fins do compilador para evitar retrabalho;
7. **Controle explícito**: O desenvolvedor tem controle absoluto sobre alocação de memória e runtime.

O projeto **NÃO** busca:
* Mágicas ou açúcares sintáticos complexos e implícitos que escondem custo de execução;
* Abstrações invisíveis custosas na heap ou que prejudiquem análise estática;
* Runtimes monolíticos ou acoplados de forma irreversível à linguagem;
* Otimizações obscuras em tempo de compilação impossíveis de auditar ou diagnosticar.

Isso define a identidade e direção de engenharia inteira da linguagem.

---

### 💎 Core Invariants (Bússola do Ecossistema)

As diretrizes abaixo orientam aceitação de APIs e otimizações. Não são certificação integral da stdlib atual: scratch/agregados, runtime, efeitos e alocadores têm exclusões documentadas. Ganhos exigem medição e a fronteira implementada é a declarada nos contratos:

1. **No hidden allocation**: Alocações dinâmicas na heap nunca acontecem de forma implícita ou escondida. Se uma função aloca, ela deve receber um alocador explícito ou ser semanticamente óbvia (ex: retornar um tipo contido em `arandu_alloc`).
2. **No implicit runtime**: O usuário não paga por um runtime assíncrono ou modelo de concorrência global se não o utilizar.
3. **ID-based compiler architecture**: Grafos, nós sintáticos e entidades de IR evitam ponteiros puros de 64 bits em prol de compactos IDs de 32 bits indexando arenas densas.
4. **Stack-first execution**: promover storage quando lifetime/layout e budgets provarem segurança. O fallback pode usar heap por limite de representação mesmo sem escape semântico; não prometer toda coroutine/agregado na stack.
5. **Zero-cost abstractions**: Abstrações de alto nível (iteradores, enums, colorless async) devem compilar no mesmo código assembly gerado por implementações equivalentes de baixo nível escritas manualmente.
6. **Async is semantic, runtime is optional**: O suporte a `await` e corrotinas é resolvido semanticamente pelo compilador. Executores e reatores de I/O são modulares e substituíveis.
7. **no_std compatibility first**: a fronteira pretendida do core exclui host/heap globais; comprovar fechamento de símbolos, startup e runtime em cada alvo antes de publicar suporte freestanding.
8. **Cache locality over abstraction purity**: Prioriza-se layouts contíguos de memória (SoA, small vectors, bitsets) mesmo que isso exija quebrar paradigmas clássicos orientados a objetos.

---

### 📐 Filosofia de Camadas (The Layered Stdlib)

Para evitar os problemas comuns de linguagens de sistemas clássicas (como o acoplamento excessivo à alocação dinâmica no topo da stdlib ou a inclusão de frameworks mutáveis e pesados que envelhecem mal), a biblioteca oficial do Arandu é dividida em **quatro camadas estritas e isoladas**.

```text
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_ext                        │  ← Gráficos, Física, Renderer, ECS
 └──────────────────────────┬─────────────────────────────┘
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_std                        │  ← OS, Filesystem, Net, Async Runtime
 └──────────────────────────┬─────────────────────────────┘
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_alloc                      │  ← Arenas, Box, Vec, Strings, HashMaps
 └──────────────────────────┬─────────────────────────────┘
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_core                       │  ← Zero heap, Zero OS, Zero Threads
 └────────────────────────────────────────────────────────┘
```

---

### 1. `arandu_core` (A Camada Fundamental)

Esta camada é **estritamente livre de dependências externas**. Ela não assume a existência de um sistema operacional, de memória dinâmica (heap) global, nem de suporte a threads de kernel. É o bloco básico de construção para qualquer ambiente, incluindo bootloaders, kernels e firmware de microcontroladores de baixíssimo consumo.

### Estrutura de Módulos
```text
arandu_core
 ├─ mem          # Operações unsafe de memória, alinhamento e layout de tipos
 ├─ pointer      # Identidade segura de ponteiros (`null`/`isNull`)
 ├─ option       # Opcionalidade canônica (Option<T>)
 ├─ result       # Tratamento de erro monádico (Result<T, E>)
 ├─ iter         # Iteradores puros, adaptadores lazy combinatoriais
 ├─ slice        # Views contíguas de memória indexáveis ([T])
 ├─ math         # Operações matemáticas básicas, trig, float/integer bounds
 ├─ cmp          # Ord, Eq, PartialEq, comparação de dados
 ├─ hash         # Interfaces de hashing e algoritmos puros de hashing de bloco
 ├─ future       # Trait Future básico (implementado automaticamente pelo compilador para Coroutine[T])
 ├─ task         # Context, Waker, RawWaker (abstrações de execução)
 ├─ pin          # Abstrações de pinning de memória (Pin/Unpin gerais; corrotinas usam OSSA Indices livres de Pin)
 ├─ cell         # Interior mutabilidade controlada (Cell, RefCell, UnsafeCell)
 ├─ marker       # Marcadores fundamentais do compilador (Send, Sync, Copy, Sized, PhantomData)
 ├─ borrow       # Abstrações de empréstimo (Borrow, BorrowMut)
 ├─ fmt          # Formatação e diagnostics de baixo nível (Debug, Display, formatting engines)
 ├─ panic        # Handlers básicos de pânico e asserções estáticas
 ├─ intrinsics   # abort, abortGenerationalMismatch (traps; zero heap)
 ├─ simd         # Seis vetores nativos, máscaras e acesso a vistas exclusivas (RFC 0038)
 ├─ atomic       # Lowering local C/Cranelift/Wasm; todos os acessos SeqCst
 └─ arch         # Especificações arquiteturais específicas (x86_64, AArch64, RISC-V)
```

> [!IMPORTANT]
> **Semântica async atual:** o compilador possui `Coroutine[T]`, `Poll[T]` e
> lowering de suspensão. A interface `Future` completa continua aberta;
> `Waker`/`Context` atuais pertencem a `std.runtime.waker`. A árvore conceitual
> de `core` não implica que a integração automática futura já esteja pronta.

---

### 2. `arandu_alloc` (Gerenciamento de Memória & Layouts)

Esta camada introduz o conceito de alocação de memória dinâmica. Ela depende do `arandu_core`, mas **não exige suporte a sistema operacional** (pode rodar no bare-metal se um alocador físico for fornecido à API).

### Estrutura de Módulos
```text
arandu_alloc
 ├─ allocator_api      # Traits para custom allocators (Arena, Slab, Bump, Global)
 ├─ arena              # Alocadores do tipo Bump, Growable Arena, Scratch Arenas
 ├─ slab               # Alocadores de tamanho fixo com reciclagem rápida por Free Lists
 ├─ gen_arena          # GenSlot / GenRef / GenArena — fallback geracional F2.3 (não em core)
 ├─ rc                 # Referência compartilhada thread-local (Reference Counting)
 ├─ arc                # Contagem atômica, bounds Send/Sync e lifecycle locais; promoção pendente
 ├─ boxed              # Ponteiro exclusivo alocado na heap (`Box<T>`)
 ├─ vec                # Vetor dinâmico contíguo reajustável (`Vec<T>`)
 ├─ smallvec           # Vetores otimizados com inline-stack storage temporário (elimina heap allocs pequenos)
 ├─ string             # String dinâmica UTF-8 editável (`String`)
 ├─ hashmap            # Tabela Hash de alta performance (Robin Hood / Swiss Table)
 ├─ bitset             # Primitivas e estruturas genéricas de conjuntos de bits (dense arrays, roaring bitmaps)
 └─ arena_collections  # Coleções otimizadas para viver estritamente dentro de Arenas
```

> **F2.3 / GenRef:** `gen_arena` is the only place dynamic generational tables live.
> Trap on mismatch is `std.core.intrinsics.abortGenerationalMismatch`.
> Compiler ABI: `docs/arandu-genref-gold-rfc-v0.1.md`.

### Filosofia de Alocadores Customizados
A API `allocator_api` e os alocadores por arena existem. O parâmetro de alocador
por coleção é uma direção arquitetural; não está integrado uniformemente em
`Vec`, `String`, `HashMap` e `Box`. O `Box<T>` local usa o host de alocação
alinhada e ainda não aceita um alocador fornecido pelo chamador. Exemplos de
coleções com alocador genérico dependem de uma futura API e não representam
assinaturas disponíveis.

---

### 3. `arandu_std` (O Sistema Operacional & Rede)

Esta camada expõe serviços clássicos fornecidos por sistemas operacionais modernos. Ela unifica as APIs assíncronas com as primitivas do OS.

### Estrutura de Módulos
```text
arandu_std
 ├─ io           # Leitura e escrita síncrona/assíncrona (Read, Write, BufRead)
 ├─ fs           # Acesso ao sistema de arquivos (File, Metadata, Permissions)
 ├─ process      # Spawning de processos, redirecionamento de I/O, IPC
 ├─ env          # Variáveis de ambiente, caminhos de executáveis, argumentos CLI
 ├─ path         # Manipulação e validação de caminhos de arquivos (Path, PathBuf)
 ├─ time         # Medição de tempo monotônico e absoluto, instantes, durações
 ├─ random       # PRNG xoshiro256++ e acesso falível à entropia do SO
 ├─ thread       # IDs/park e spawn/JoinHandle; ThreadJob estático ou move || sem parâmetros
 ├─ ffi          # Conversão de tipos, ponteiros C, carregamento dinâmico (dlopen)
 ├─ collections  # Coleções avançadas (BTreeMap, Deque, LinkedList) dependentes de alocação e OS
 ├─ json         # Parser e serializador JSON ultra rápido, stream-based, cache-friendly
 ├─ xml          # Parser XML minimalista focado em conformidade e velocidade
 ├─ crypto       # Criptografia modular (hashes, cifras, TLS/X.509)
 ├─ testing      # Testes e benchmarks nativos; ver arandu-testing-benchmark-harness-v0.1.md
 ├─ os           # Módulos específicos de sistema operacional (Linux, Windows, macOS)
 │   ├─ descriptors # Handles puros, file descriptors, pipe descriptors
 │   ├─ memory      # Controle avançado de memória virtual (mmap, VirtualAlloc, commit/reserve)
 │   └─ syscalls    # Interface direta a raw syscalls da plataforma
 ├─ net          # Sub-sistema de rede unificado
 │   ├─ tcp      # Sockets TCP (Listen, Stream)
 │   ├─ udp      # Sockets UDP (Datagrams, Multicast)
 │   ├─ dns      # Resolução de nomes assíncrona
 │   ├─ http     # Cliente e servidor HTTP/1.1 e HTTP/2 embarcado de baixo overhead
 │   └─ websocket# Suporte nativo a WebSocket sobre a pilha HTTP/TCP
 ├─ sync         # Primitivas de sincronização avançadas
 │   ├─ cancel_token # Sinal atômico compartilhado; cancelamento cooperativo
 │   ├─ mutex    # Exclusão nativa pela fase FIFO de escritores do ParkingLot
 │   ├─ rwlock   # Fases phase-fair; escritores FIFO e leitores admitidos em lote
 │   ├─ condvar  # Wait/timeout por sequência, notify e cancelamento POSIX diferido
 │   ├─ barrier  # Barreiras de sincronização de threads
 │   ├─ once     # Inicialização lazy e thread-safe única
 │   ├─ channel  # Canais de comunicação (MPMC, SPSC, flume-style)
 │   └─ parking  # Interface low-level de thread parking (para custom mutexes)
 └─ runtime      # O motor de concorrência assíncrona da linguagem
     ├─ scheduler# Agendamento cooperativo/work-stealing de tarefas com afinidade NUMA
     ├─ executor # Executor explícito; pool global não é contrato atual
     ├─ reactor  # Despachante de eventos de I/O do SO
     ├─ epoll    # Reactor backend para Linux
     ├─ kqueue   # Reactor backend para macOS/BSD
     ├─ iocp     # Reactor backend para Windows
     └─ io_uring # Reactor de alta vazão moderno para Linux kernel ≥ 5.1
```

---

### Superfície pública adicionada na 0.1.11

A campanha 0.1.11 adicionou açúcar sintático e APIs que mudam como a stdlib é
chamada. A sintaxe é responsabilidade de lexer/parser/typeck; a lista abaixo
registra apenas o contrato que o usuário da stdlib observa.

Linguagem:

- argumentos rotulados e shorthand: `cfg.bump(delta: 20)` e `connect(port:, host:)`
  passam os valores posicionalmente, então nenhuma assinatura de stdlib muda;
- default de campo de struct: `struct Config { port: int = 8080 }` permite
  `Config { label: "api" }`; o valor default é dobrado no typeck;
- `.len` em `[]T`, `[T; N]` e `str`, sem chamada de função;
- literais de byte: `b'A'` é `u8` e `b"..."` é `[]u8` com os bytes preservados,
  inclusive fora de UTF-8;
- `|>`, que apenas reescreve a chamada (placeholder `_` ou primeiro argumento);
- `let <pattern> = ... else { ... }`, `guard ... else { ... }` e `.Variant`.

`core.num`:

```text
parse(s: str): Option<int>          int.parse(s: str): Option<int>
parseU64(s: str): Option<u64>       u64.parse(s: str): Option<u64>
```

`core.slice` (métodos sobre `[]T`):

```text
len, isEmpty, asPtr
get(index): Option<ref T>        first(): Option<ref T>   last(): Option<ref T>
subslice(start, count): Option<[]T>
readU16Le/readU32Le/readU64Le(offset): Option<u16|u32|u64>   // apenas []u8
```

`core.iter` (adaptadores lazy, todos consumidos por `next`):

```text
take(count)   skip(count)   stepBy(step)   enumerate()   chain(other)   zip(other)
count()       last()        nth(n)         iter(slice)
```

`std.fs` (escrita confinada à autoridade do `Dir`):

```text
Dir.writeBytes(subpath, bytes): Result<usize, io.IoError>
Dir.writeFile(subpath, content): Result<usize, io.IoError>
writeBytes(path, bytes)        write(path, content)   // raiz ambiente
```

Os adaptadores de `core.iter` e os métodos de `core.slice` devolvem views e
iteradores que preservam a dependência do owner; `get`, `first`, `last` e
`subslice` são `Option`, não ponteiros nus.

---

### 4. `arandu_ext` (Extensões Opcionais de Aplicação)

`arandu_ext` é uma proposta de organização de extensões opcionais. A árvore
abaixo não corresponde a frameworks já implementados ou distribuídos no SDK.
Cada extensão exige pacote, contrato de dependências e validação próprios.

### Estrutura de Módulos
```text
arandu_ext
 ├─ ecs           # Entity-Component-System nativo e de alta performance
 ├─ game          # Utilitários de loop de jogo, time steps fixos, input handling
 ├─ serialization # Parsers rápidos para JSON, XML e formatos estruturados de dados (arandu_data)
 ├─ renderer      # Abstração de hardware gráfico de baixo overhead (WGPU/Vulkan/D3D12 wrapper)
 ├─ audio         # Mixagem, decodificação de áudio (WAV, OGG, MP3) e output espacial
 ├─ media         # Carregamento e manipulação de texturas, imagens (PNG, JPG) e codecs básicos
 ├─ gui           # Framework declarativo e imediato de interface gráfica 2D
 └─ physics       # Integração com física 2D/3D (colisões, gravidade, corpos rígidos)
```

---

### 🔬 Stack de Computação Científica e de Dados (RFC 0012) — Triagem Arquitetural

Conforme formalizado na [RFC 0012](rfcs/0012-scientific-computing-and-data-architecture.md), o Arandu rejeita transformar a stdlib básica em um monólito numérico pesado:

* **In-Tree (`stdlib/core/math` e `stdlib/core/simd`)**: Apenas operações escalares puras (IEEE-754, trigonometria elementar, constantes `PI`/`TAU`/`E`) e tipos primitivos de registradores SIMD (`f32x4`, `u8x16`) pertencem à biblioteca padrão do compilador. A matemática tem implementação escalar local; SIMD possui tipos fonte canônicos, representação/layout/AMIR e lowering nos três backends (RFC 0038). O requisito de ausência de heap/OS exige implementação interna ou intrínsecos com fechamento de símbolos comprovado; chamadas à `libm` do host não satisfazem esse contrato automaticamente.
* **Out-of-Tree / Ecossistema Externo (`arandu_math`, `arandu_data`, `arandu_science`)**: Todo o ferramental pesado de tensores N-dimensionais, matrizes esparsas, BLAS/LAPACK, processamento colunar Apache Arrow, DataFrames e solvers de EDOs reside em pacotes modulares externos do ecossistema, completamente fora da árvore de código do compilador v0.1.

```text
stdlib (In-Tree)    ──► core::math (escalar, contrato numérico puro) e core::simd (tipos primitivos)
       │ (fronteira desacoplada)
arandu_math (Crate) ──► Array/ArrayView (strides), StaticMatrix, GEMM/BLAS, ScratchArena, Sparse, FFT
arandu_data (Crate) ──► Apache Arrow, RecordBatch, Validity Bitmaps, DataFrames, Lazy Engine
arandu_science (Crate) Solvers (ODE), Sinais, Otimização, GraphBLAS (Grafos sobre Semirings)
```

* **Invariante de Isolamento**: O compilador `arandu` e programas de uso geral jamais dependem de pacotes científicos externos;
* **Zero Hidden Allocation**: Operações de destino explícito (`*_into`) e scratch reutilizável (`ScratchArena`);
* **Fronteira Estrita Tensores vs Dados Tabulares**: `arandu_math` opera sobre views densas com strides; `arandu_data` opera sobre estruturas SoA colunares, validity bitmaps e chunks compatíveis com a Arrow C Data Interface.

---

### 🎯 Modularidade Criptográfica (`crypto`)

A implementação local de `std.crypto.hash` contém SHA-256 e BLAKE3 unkeyed de
32 bytes, com estado incremental e `finish()` que preserva o estado. Os vetores
independentes cobrem padding, blocos, chunks, árvore e atualizações após
snapshots. SHA-3, cifras, assinaturas, certificados e TLS continuam propostas
de módulos independentes:

```text
crypto
 ├─ hash    # SHA-256, SHA-3, BLAKE3 (Compila rápido, usado em hashing de dados)
 ├─ aes     # Cifras simétricas por hardware
 ├─ rsa     # Assinaturas clássicas e chaves assimétricas
 ├─ ed25519 # Algoritmos modernos de curvas elípticas compactas
 ├─ tls     # Interface de sockets seguros com TLS 1.3
 └─ x509    # Validação de certificados digitais
```
Os hashes locais são escalares e não importam OpenSSL, entropia ou bibliotecas
do SO. Não fornecem autenticação, keyed hashing, XOF, criptografia de mensagens
ou certificação FIPS. O isolamento de dependências dos demais módulos precisa
ser demonstrado quando eles forem implementados; não há tamanho de binário ou
aceleração por hardware garantidos por esta árvore conceitual.

---

### 🚫 O Que NÃO Pertence à Stdlib

Para preservar a estabilidade da API, o tempo de compilação, o tamanho de binários finais e a modularidade global, o Arandu ativamente evita incluir na stdlib básica os seguintes componentes:
* **Game engines** e loops especializados de gameplay;
* **ECS frameworks** de aplicação específica;
* **GUI frameworks** declarativos ou imediatos;
* **ORMs** (Object-Relational Mapping);
* **Web frameworks** complexos de alto nível (ex: routers MVC, templates);
* **Serializadores pesados** de domínio específico;
* **Renderers** 2D ou 3D acoplados a hardware gráfico;
* **Matemática especializada** de domínio complexo (ex: CAD, ML/AI, criptografia customizada avançada).

Todos estes componentes residem estritamente nas extensões de ecossistema (`arandu_ext`) ou em pacotes de terceiros distribuídos por gerenciadores de dependência.

---

### Self-hosting como possibilidade de evolução

Self-hosting é uma possibilidade de longo prazo. A versão 1.0 deve representar
um contrato estável e uma matriz suportada comprovada; reescrever o compilador
ou abandonar Rust não é condição já aprovada para isso. Uma proposta de
self-hosting precisaria definir bootstrap, equivalência e recuperação de forma
própria. A antiga meta de cold-build inferior a três segundos não possui corpus,
host e metodologia que a tornem um gate vigente.

O planejamento por versões, condicionado à aprovação de escopo e evidência,
fica no roadmap mestre: 0.2 motor quente; 0.3 granularidade por instância e
closures em contrato próprio; 0.4 release optimizer; 0.5 ABI/interop/runtime;
0.6 tooling/cache; 0.7–0.9 estabilização; 1.0 contrato estável. Essa direção não
aprova automaticamente todos os módulos conceituais deste documento.

## Relatório final AUD.5 — segurança de borrowed views

**Decisão após BV.1–BV.3:** borrowed views estruturais são **suportadas** na
superfície pública implementada. `[]T` é o tipo canônico de slice seguro;
`Vec.asSlice`, `String.asStr`, `String.asBytes`, `Slice.get`, `first`, `last`,
`subslice` e a iteração mínima preservam a dependência do owner no compilador.
O ABI continua sendo somente `ptr + len` e não leva lifetime para o runtime.

Essa decisão promove o componente de borrowed views, não toda a campanha
`SL_S-Core`: allocator genérico, drop de payloads não triviais e evidência
nativa específica dessas operações continuam sendo gates independentes do
smoke já aprovado da distribuição.

### Matriz de capacidade

| Superfície | Classificação | Evidência e limite |
| --- | --- | --- |
| Borrow local `ref T`/`mut ref T` | **Suportado** | loans têm owner, kind, holders e live range; conflitos de move, mutação e destroy emitem O002/O003/O006 |
| Retorno direto ou união de parâmetros emprestados | **Suportado em BV.1** | interface derivada do fluxo contém somente origens alcançáveis; retorno de local é O010 |
| Forwarding, import e especialização genérica | **Suportado em BV.1** | testes CLI/query cobrem composição, recursão, import, genérico e hash canônico |
| Incrementalidade e IDE | **Suportado para o contrato BV.1** | mudança do summary invalida caller; mudança só de corpo para no `borrow_interfaces` HashEq |
| `Use`, `Load`, `Store` e block args intraprocedurais | **Suportado em BV.2** | holders estruturais atravessam construção, projeção, overwrite e joins por fixpoint |
| Receiver/método, projeções de campo e suspensão | **Suportado no contrato modelado** | receivers preservam origem; borrow absoluto escondido em carrier não atravessa `await` |
| Caminhos de `Option`, `Result`, tuple, struct e enum | **Suportado em BV.2** | carriers herdam não-escapabilidade e copy/move estruturalmente |
| `[]T` e views de `String` | **Suportado em BV.3** | API segura usa proveniência compile-time; construção raw fica confinada aos intrínsecos internos |
| Retorno com múltiplas origens possíveis | **Suportado em BV.1** | branches fazem união apenas das fontes que chegam ao retorno; candidato só compatível pelo tipo é excluído |
| Closure escapável, global/heap e ida-e-volta por ponteiro cru | **Ausente** | não existe vínculo estático recuperável; ponteiro cru continua obrigação `unsafe` |

“Ausente” não significa que o compilador aceita silenciosamente o caso. Nos
casos que alcançam a superfície segura, a política é **fail-closed**: rejeitar
é preferível a fabricar uma origem, promover para GenRef ou transformar um
ponteiro tipado em referência aparentemente segura.

### Resultado dos gates

| Gate da auditoria | Resultado |
| --- | --- |
| Origem direta preservada por call/forwarding/genérico/import | **Passou** |
| O002/O003/O006/O010 nascem e desaparecem após cache hit | **Passou** |
| Summary participa de hash, invalidação e early-cutoff Salsa | **Passou** |
| Diagnóstico CLI/LSP equivalente, estruturado e sem publicação stale | **Passou** |
| Relatório AMIR determinístico localiza fronteiras de perda | **Passou** |
| Carriers, CFG, projeções, múltiplas origens e suspensão | **Passou para as formas públicas existentes** |
| Views seguras `Slice.get`, `String.asStr` e `String.asBytes` | **Passou** |
| Paridade C/Cranelift do corpus BV.3 | **Passou localmente; matriz nativa do PR é a evidência de promoção** |

### Decisão arquitetural permanente

1. A sintaxe canônica permanece `T`/`own T`, `ref T` e `mut ref T`; não será
   adicionada sintaxe de lifetime enquanto um caso real não exigir isso.
2. `ReturnBorrowSummary` estrutural e de múltiplas fontes é a fronteira pública
   desde BV.1; não contém spans, `TypeId` ou ordem incidental de mapa.
3. `Option`/`Result` e demais carriers propagam a dependência não escapável em
   construção, match, cópia, phi e chamada.
4. O layout `ptr + len` de `[]T` e `ref str` é independente da prova: origem,
   exclusividade e live range existem apenas até a validação anterior ao backend.
5. GenRef, RC/ARC e heap promotion não substituem uma prova de borrowed view.
6. APIs de ponteiro só contam como alternativa quando forem publicamente
   `unsafe` e documentarem a obrigação do chamador.

### Dívida técnica e sequência futura

- manter construção raw de slice inacessível ao código seguro até a linguagem
  possuir funções públicas `unsafe` com contrato verificável;
- ampliar o corpus quando closures escapáveis se tornarem sintaxe pública;
- provar os artefatos nos runners nativos da distribuição antes de marcar a
  campanha inteira como Gold;
- adicionar views mutáveis somente com um RFC separado para `mut []T` e regras
  explícitas de exclusividade e iteração.

## PONTOS DE MELHORIA (O que não está no roadmap)

Grande parte deste documento ainda é design, não promessa implementada. Metas
numéricas de self-hosting precisam de corpus, hardware e protocolo de benchmark
definidos antes de se tornarem gate.

O primeiro corte de `SL_S-Core` combina implementação e convenções de design.
As regras de nomenclatura abaixo orientam a API; as promessas uniformes de
recursos da RFC 0026 ainda são propostas, conforme a auditoria acima:

- **Convenções Canônicas de Nomenclatura e Design de API (RFC 0026):**
  - **Casing:** Tipos/interfaces em `PascalCase`, callables/métodos em `camelCase`, módulos/arquivos em `snake_case`, constantes em `SCREAMING_SNAKE_CASE`;
  - **Construtores e Genéricos no Tipo:** Forma canônica única `Type.new(...)` / `Type<T>.new(...)` (apenas args essenciais), `Type.withX(...)` / `Type<T>.withX(...)` para configurações (`Vec<int>.withCapacity(16)`, `HashMap<str, User>.withCapacity(32)`), `Type.from(x)` para conversão inequívoca de tipo (`String.from("olá")`) e `Type.fromX(x)` quando a escala/semântica for indispensável (`Duration.fromMillis(50)`, `String.fromUtf8(bytes)`). Operações associadas sem instância existente permanecem no tipo (`Instant.now()`), enquanto operações sobre uma instância existente são métodos (`inicio.elapsed()`). Padrões legados `*New`, `new*` ou funções livres duplicadas (`bitsetNew`, `atomicBoolNew`, `newChannel`, `vec.new`) são removidos da API pública;
  - **Receptores:** "Se a operação pertence a um valor, ela é método" (`vec.push(x)`, `slice.len()`, `inicio.elapsed()`). Funções livres são reservadas para operações sem dono semântico único (`math.min(a, b)`, `mem.swap(a, b)`, `hash.combine(a, b)`);
  - **Conversões:** `asX()` (view/empréstimo de custo zero), `toX()` (produz valor com cópia/alocação), `intoX()` (move e consome `self`);
  - **Métricas de Tamanho:** `len()` retorna a contagem de elementos para coleções e a quantidade de **bytes UTF-8** em O(1) para `str`/`String` (ex.: `String.from("olá").len()` == `4`). `charCount()` retorna a contagem de Unicode scalar values em O(n) (ex.: `String.from("olá").charCount()` == `3`). `lenBytes` é descontinuado;
  - **Sequências vs. Mapas:** `put` é restrito a dicionários/mapas chave-valor (`map.put(k, v)`). Em sequências (`Vec`), usa-se `push(v)` para append e `set(i, v)` / `get(i)` para acesso indexado;
  - **Operações Falíveis:** O prefixo `tryX()` é reservado para variantes não-panicking de operações que possuem uma contraparte normal (`push` vs `tryPush`, `reserve` vs `tryReserve`). Operações primariamente falíveis (`File.open`) não utilizam prefixo `try`;
  - **Abreviações Públicas:** Abreviações consagradas do domínio de sistemas são aceitas (`ptr`, `ref`, `len`, `cap`, `io`, `fs`, `os`, `cpu`, `gpu`, `utf8`, `tcp`, `udp`, `http`). Abreviações de detalhes de implementação (`newCoreIoError`, `mkPortErr`) são expressamente proibidas;
  - **Regra do Espelho:** Tipos análogos compartilham o mesmo vocabulário e assinaturas (`len()`, `isEmpty()`, `clear()`, `withCapacity()`).

- **Gerenciamento de Recursos e Eliminação de Double-Free (RFC 0026):**
  1. *Memória Pura (`Vec`, `String`, `HashMap`):* O gerenciamento de ciclo de vida é exclusivo do compilador via destruição automática (`drop` em OSSA). Métodos manuais `destroy()` são banidos da API pública normal;
  2. *Recursos Externos de I/O (`File`, sockets, `DirListing`):* A convenção proposta é `close(): Result<void, IoError>` com `drop` best-effort. `File.close` e `DirListing.close` ainda retornam `void`; não se promete essa uniformidade como implementada;
  3. *Idempotência:* A proposta exige que o `drop` após `close()` seja um no-op; essa garantia precisa de prova por tipo e não decorre apenas da convenção de nomes.

- **Checklist de PR para Módulos da Stdlib:**
  1. *Construtor:* O tipo expõe `Type.new(...)` / `Type<T>.new(...)`, `Type.withX(...)` / `Type<T>.withX(...)` ou `Type.from[X](...)` em vez de funções livres?
  2. *Receiver:* Toda operação pertencente conceitualmente à entidade está modelada como método (`value.method(...)`)?
  3. *Tamanho:* A medição segue `len()` (elementos/bytes UTF-8 em O(1)) e `charCount()` (Unicode scalars em O(n)), sem inventar `lenBytes` ou variantes ad-hoc?
  4. *Abreviações:* Os nomes públicos evitam acrônimos de implementação interna, admitindo apenas jargões consolidados do domínio?
  5. *Liberação:* O tipo delega liberação de memória estritamente ao `drop` (sem `destroy()` público) ou implementa `close(): Result<void, E>` idempotente para recursos de I/O?

- `io.print(str)` escreve bytes em stdout sem newline; `io.println` continua
  acrescentando newline. Ambos usam o ABI de string existente, sem montar uma
  `String` intermediária. No core Wasm o host fornece `io.print(ptr, len)`;
- `std.core.str.asBytes(ref str)` expõe a view compartilhada de bytes UTF-8 sem
  importar intrínsecos na aplicação. O parâmetro de referência preserva a origem;
  literais podem ser passados diretamente, como `strings.asBytes("hello")`;
  um wrapper que recebe `str` por valor e retorna `[]u8` ainda não demonstra
  essa origem no contrato atual e é rejeitado, não liberado por exceção;
- `std.fs.readToBytes(path)` retorna `Result<Vec<u8>, IoError>`, adotando o buffer
  do host sem conversão textual. `vec.adoptBytes` é uma API `@Unsafe` de
  implementação, não uma obrigação imposta ao usuário de `readToBytes`;
- `Vec.get(index)` retorna `Option<ref T>` sem exigir `Copy`; uma referência
  ainda em uso bloqueia crescimento, substituição e destruição do vetor;
- `char` representa um Unicode scalar em 32 bits em layout, C e Cranelift;
- o cast `u8`/`byte` para `char` mapeia U+0000..U+00FF e não decodifica UTF-8;
  valores inteiros mais largos continuam exigindo conversão validada;
- `Option`/`Result` consomem `self` ao retirar payloads possuídos;
- `[]T` é uma borrowed view segura, copiável localmente e não escapável; seu ABI
  é `ptr + len`, enquanto a origem fica em summaries/holders compile-time.
  `Slice.get` retorna `Option<ref T>` e a construção raw não é API pública;
- `ref T`/`mut ref T` são os tipos de referência segura da linguagem; `*expr` é a
  operação de dereference correspondente. Para `ptr[T]`, `*expr` permanece
  permitido somente em `unsafe`;
- a auditoria AUD.0–AUD.5 possui evidência AMIR determinística para loans,
  holders, escapes e perdas de origem. Retornos diretos são suportados por
  `ReturnBorrowSummary`; BV.1 adicionou múltiplas origens, BV.2 propagou holders
  estruturais e BV.3 publicou as APIs de `[]T` e `String`;
- `std.core.pointer` expõe somente identidade segura. Aritmética, leitura e
  escrita de ponteiros permanecem intrínsecos `unsafe`;
- aritmética checked de `int` testa limites antes da operação e deriva os
  limites da largura do target.

Ainda impedem a promoção de toda `SL_S-Core` para Gold: allocator genérico real,
política uniforme de OOM nas coleções, drop de payloads não triviais e cobertura
nativa específica dessas operações. Os targets publicados já possuem smoke
de distribuição; isso não substitui essa cobertura. `String.pushStr` já é fallible e
mantém o valor original em overflow/OOM.
O alias histórico `std.core.ptr` foi removido porque oferecia aritmética de
ponteiro através de uma função aparentemente segura; uma compatibilidade futura
só pode voltar junto de funções públicas `unsafe` no contrato da linguagem.

- `std.fs` e `std.env`: `readToString`, `readDir` e `arg(index)` possuem
  suporte completo em Cranelift JIT e C backend (`emit-c`). O contrato de buffer
  de `ar_fs_read_all` transfere ownership ao Arandu (`strings.adoptOwned`) com
  zero-copy; `readDir` empacota entradas em um blob DOD contíguo compacto
  (`[count][blob_len][entries][name descriptors][names]`) sem `stat` extra por
  entrada no Linux; cada descritor de nome permanece estável, permitindo
  múltiplos `ref str` simultâneos sem mutar a listagem;
  códigos de erro são normalizados para o intervalo portável `0-8` (`IoErrorKind`).

## Futuro e Próximos Passos

Implementar a stdlib na ordem do roadmap mestre, publicando apenas módulos com
contrato, tests/benchmarks e suporte coerente aos targets declarados.
