# RFC 0012: Arquitetura da Stack de Computação Científica, Numérica e de Dados (`arandu_math`, `arandu_data`, `arandu_science`)

- **Número da RFC:** 0012
- **Título:** Arquitetura da Stack de Computação Científica, Numérica e de Dados (`arandu_math`, `arandu_data`, `arandu_science`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-09-12
- **Status:** `Draft`
- **Área Principal:** `Ecosystem (Out-of-Tree Packages)` / `Stdlib Minimalista`
- **PR da RFC:** N/A (In-Tree RFC)
- **Issue de Acompanhamento:** N/A

---

## 1. Resumo (Summary)

Esta RFC estabelece a arquitetura formal, os princípios de design de memória, a taxonomia de pacotes e a especificação técnica para a infraestrutura de computação numérica, processamento analítico de dados e modelagem científica do ecossistema Arandu.

**Decisão de Triagem da Standard Library (v0.1):**
Em alinhamento com a filosofia de compilador enxuto e de alta velocidade (seguindo os acertos históricos de Rust e Python), o Arandu adota uma **separação estrita entre o compilador e pacotes científicos**:
- **In-Tree (`stdlib/core/math` e `stdlib/core/simd`)**: O repositório central do compilador é responsável **exclusivamente** pela matemática escalar fundamental (aritmética IEEE-754, funções elementares da `libm`, trigonometria, constantes numéricas) e tipos primitivos de registradores SIMD (`f32x4`, `u8x16`). Nada de matrizes pesadas, tensores ou dependências externas entra no compilador.
- **Out-of-Tree / Ecossistema Externo (`arandu_math`, `arandu_data`, `arandu_science`)**: Esta RFC serve como **blueprint arquitetural canônico** para crates externas independentes mantidas pelo ecossistema/comunidade. Nenhum desses pacotes bloqueia a estabilização do compilador Arandu v1.0 nem reside na árvore padrão de crates.

A proposta sintetiza e aprimora as melhores decisões arquiteturais de 14 ecossistemas de referência (**NumPy, Julia, JAX, Eigen, xtensor, nalgebra, faer, LAPACK/oneMKL, OpenBLAS, Apache Arrow, Polars, SciPy Sparse, GraphBLAS e FFTW**), eliminando deliberadamente suas dívidas técnicas históricas.

### 1.1 Estado da implementação (2026-09-20)

SCI.1 está **parcial**, não concluído. `stdlib/math` funciona hoje como área de
incubação in-tree para views bidimensionais strided, `Matrix`, `StaticMatrix`,
operações com destino e `ScratchArena`; a decisão arquitetural continua sendo
extrair a stack pesada para pacotes out-of-tree antes da estabilização. Ainda
não existem `Array<T, N, A>` N-dimensional, prova instrumentada de zero
alocação, fusão/autovetorização SIMD, microkernel GEMM bloqueado nem conector
BLAS. O GEMM atual é o kernel escalar de referência e `Matrix` usa a ABI de
alocação do runtime. SCI.2–SCI.4 permanecem bloqueados até esses contratos e a
representação de agregados no backend estarem assentados.

A fundação baseia-se em quatro pilares inegociáveis:
1. **Modularidade estrita em camadas**: O núcleo `arandu_core::math` permanece estritamente escalar, livre de heap e `no_std`. As camadas de alto desempenho residem em pacotes modulares externos oficiais: `arandu_math` (tensores e álgebra linear densa/esparsa), `arandu_data` (processamento colunar, Apache Arrow e DataFrames lazy) e `arandu_science` (solvers de EDOs, processamento de sinais e grafos sobre semirings).
2. **Separação irrevogável entre Tensores (`math`) e Dados Tabulares (`data`)**: `arandu_math` opera sobre views multidimensionais leves (`ArrayView<T, N>`) com strides arbitrários e densidade homogênea (zero overhead de nulidade em hot loops). `arandu_data` opera sobre estruturas SoA colunares, validity bitmaps e chunks (`RecordBatch`), adotando a **Arrow C Data Interface** como padrão universal de interoperabilidade zero-copy.
3. **Contrato de Memória "Zero Hidden Allocation" & Destination-Passing Style**: Eliminação de alocações e temporários em laços críticos através de operações com destino explícito (`*_into`), fusão de laços elementwise sem *expression templates* de C++, e alocação de scratch explícita e reutilizável (`ScratchArena` / `MemStack`).
4. **Semântica de Tipos e Determinismo Estrito**: Rejeição de promoções numéricas implícitas baseadas em valor; adoção de um lattice de tipos formal e previsível; RNG puramente desacoplado e counter-based (Philox/PCG) sem estado global; e ausência de variáveis globais mutáveis para seleção de backends BLAS.

---

## 2. Motivação (Motivation)

A computação científica moderna enfrenta uma bifurcação crônica entre duas abordagens imperfeitas:

1. **Linguagens interpretadas/dinâmicas com colcha de retalhos em C/C++ (ex: Python/NumPy)**:
   - Dependem de despacho dinâmico e empacotamento de objetos para orquestrar bibliotecas compiladas.
   - Geram milhões de alocações temporárias na heap (`a + b * c` aloca arrays intermediários).
   - Sofrem com o legado de regras de coerção numérica imprevisíveis (resolvidas parcialmente pelo NEP 50) e dtypes heterogêneos lentos (`object`).
   - Apresentam inconsistências profundas entre estruturas de Tensores e estruturas de DataFrames (Pandas sofrendo com modelo de ponteiros e falta de representação colunar de nulos).
2. **Linguagens de sistemas com templates hipertrofiados (ex: C++/Eigen/xtensor)**:
   - Utilizam *expression templates* para adiar e fundir operações, resultando em tempos de compilação gigantescos, consumo colossal de memória no compilador, mensagens de erro ilegíveis e riscos de *dangling references*.
   - Dificuldade para provar ausência de aliasing entre ponteiros (`restrict` raramente é garantido pelo sistema de tipos), forçando o compilador a emitir verificações redundantes em runtime ou impedindo autovetorização SIMD agressiva.

O Arandu possui características nativas ideais para computação científica de alto desempenho:
- **Ownership Semântico (OSSA)** e empréstimos exclusivos (`mut ref`): garantem não-aliasing absoluto por construção de tipos, permitindo autovetorização SIMD sem salvaguardas em runtime.
- **Stack-first & No hidden allocation**: controle total sobre quando e onde a memória é alocada.
- **Data-Oriented Design (DOD)**: alinhamento natural com SoA, bitsets e estruturas densas contíguas.
- **Compilador incremental com AMIR**: capacidade de reconhecer padrões de laço e aplicar fusão no nível de IR sem sobrecarregar o frontend com metaprogramação.

Esta RFC projeta uma stack científica de raiz, sem as amarras históricas dos anos 1990/2000, oferecendo o desempenho do C/Fortran/BLAS com a segurança, determinismo e clareza arquitetural do Arandu.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1. A Arquitetura em Quatro Camadas

A stack científica do Arandu é estritamente particionada:

```text
arandu_science  ──► Solvers (ODE), Sinais, Otimização, GraphBLAS (Grafos sobre Semirings)
       │
arandu_data     ──► Arrow Colunar, Validity Bitmaps, Offsets, RecordBatch, DataFrames, Lazy Plans
       │
arandu_math     ──► Array, ArrayView, Strides, StaticMatrix, GEMM/BLAS, Scratch, Sparse, FFT
       │
arandu_core     ──► math::scalar, math::complex, simd::*, numeric traits (Zero heap, no_std)
```

1. **`arandu_core::math`**: Funções elementares escalares (`sin`, `cos`, `sqrt`), limites de inteiros e floats, números complexos primitivos (`Complex<T>`), vetores SIMD fundamentais (`simd::f32x8`) e traits numéricos (`Numeric`, `Signed`, `Float`). Não aloca na heap e funciona em microcontroladores bare-metal.
2. **`arandu_math`**: Manipulação de tensores densos multidimensionais, views de strides sem cópia, álgebra linear, matrizes esparsas, geração de números pseudo-aleatórios e FFT.
3. **`arandu_data`**: Dados analíticos e tabulares no formato Apache Arrow, colunas SoA com vetores de validade por bitmap, offsets contíguos de strings, chunks de streaming, e motor de execução lazy para consultas analíticas.
4. **`arandu_science`**: Algoritmos de aplicação científica complexa construídos sobre as camadas anteriores: resolução de equações diferenciais, processamento digital de sinais e análise de grafos em larga escala usando álgebra linear esparsa.

---

### 3.2. O que Copiar e o que Evitar

A tabela a seguir orienta todas as decisões técnicas desta RFC:

| Ecossistema de Referência | O que Adotar no Arandu | O que Evitar Rigorosamente |
| :--- | :--- | :--- |
| **NumPy** | Modelo de `shape` + `strides` + `views`, broadcasting canônico, kernels elementwise vetorizados. | Regras históricas de promoção por valor, dtypes heterogêneos (`object`), temporários ocultos em expressões encadeadas. |
| **Julia** | Fusão de laços elementwise, operações com destino explícito (`...into` / in-place). | Dependência mandatória de JIT dinâmico em tempo de execução para obter desempenho de código de máquina. |
| **Eigen / xtensor** | Eliminação de cópias intermediárias e fusão conceitual de kernels. | O "inferno de *expression templates*" em C++, tempos de compilação descontrolados e problemas de ciclo de vida de referências. |
| **C++ `mdspan` / ndarray** | Views multidimensionais não-proprietárias com layout e strides explícitos; slicing sem cópia. | Fazer fatiamento (*slicing*) criar novos buffers e alocar memória implicitamente. |
| **nalgebra** | Separação explícita entre matrizes estáticas na stack e matrizes dinâmicas na heap. | Colocar matrizes gigantescas na stack de forma implícita e descontrolada. |
| **faer / LAPACK / oneMKL** | O chamador fornece memória scratch/workspace reutilizável (`MemStack` / `LWORK`). | Inserir chamadas a `malloc()` escondidas no meio de algoritmos de álgebra linear. |
| **OpenBLAS** | Cache blocking, register packing, microkernels especializados por microarquitetura (GotoBLAS). | Implementações ingênuas de multiplicação de matrizes com laço triplo $O(n^3)$. |
| **Apache Arrow** | Layout SoA colunar, validity bitmaps (1 bit por nulo), buffers contíguos de offsets, zero-copy IPC via Arrow C Data Interface. | Representação de tabelas baseada em objetos, ponteiros por célula ou `Option<T>` em cada elemento. |
| **Polars / DataFusion** | Planos lógicos preguiçosos (*lazy plans*), otimizador com pushdown de predicados e projeções, streaming em chunks. | Materializar e copiar DataFrames completos intermediários a cada etapa de transformação. |
| **SciPy Sparse** | Formatos especializados: COO para construção flexível, CSR/CSC/BSR para computação matemática de alta densidade. | Tentar forçar um único formato esparso canônico para construção e cálculo. |
| **GraphBLAS** | Grafos modelados através de álgebra linear esparsa sobre semirings. | Grafos modelados como coleções gigantescas de objetos `Node*` e `Edge*` cheios de ponteiros. |
| **FFTW** | Conceito de `Plan` (planejar uma vez com `estimate`/`measure`, executar milhares de vezes). | Recalcular tabelas de twiddle factors e realocar buffers de trabalho a cada chamada de FFT. |
| **JAX / Array API** | Lattice formal, comutativo e previsível de promoção de tipos numéricos. | Coerções implícitas surpreendentes entre famílias incompatíveis (ex: float com inteiro sem cast explícito). |
| **NumPy Random / Philox** | Separação estrita entre gerador de bits de baixo nível e distribuições estatísticas; geradores counter-based. | Instâncias globais compartilhadas de RNG com estado mutável oculto. |

---

### 3.3. Arrays e Views: Geometria de Memória Sem Alocação

O núcleo de `arandu_math` separa posse de dados de visualização:

```arandu
// Array proprietário que gerencia o buffer na heap usando o alocador especificado
let arr: Array<f32, 2> = Array::zeros([1024, 1024], allocator)

// Views não-proprietárias: peso de apenas 3 palavras de máquina (ponteiro, shape, strides)
let view: ArrayView<f32, 2> = arr.view()

// Slicing puro: zero cópias, zero alocações na heap
let sub_view: ArrayView<f32, 2> = view.slice([0..512, 0..512])

// View mutável exclusiva: protegida pelas regras de empréstimo do Arandu
let mut_sub: ArrayViewMut<f32, 2> = arr.view_mut().slice([512..1024, 0..512])
```

#### Layouts e Strides
Uma view é descrita matematicamente por:
$$\text{offset}(i_0, i_1, \dots, i_{N-1}) = \sum_{k=0}^{N-1} i_k \cdot \text{stride}_k$$

Suporta layouts canônicos:
- `Layout.RowMajor` (C-contiguous)
- `Layout.ColumnMajor` (Fortran-contiguous)
- `Layout.Strided` (passos arbitrários resultantes de slicing, transposição ou subsampling)

---

### 3.4. Destination-Passing Style (DPS) & Scratch Memory

Para garantir ausência total de alocações em hot loops de simulações, robótica e machine learning, as operações numéricas fundamentais expõem APIs de destino explícito:

```arandu
// Contrato estrito: heap allocations = 0, temporários = 0, passes = 1
math.addInto(&mut out, view_a, view_b)
math.mulAddInto(&mut out, view_a, view_b, view_c)
```

Como `out` exige uma referência exclusiva (`&mut`), o compilador Arandu prova em tempo de compilação que não há sobreposição de memória (*no-aliasing*) com `view_a`, `view_b` ou `view_c`, emitindo código SIMD vetorizado sem gerar branches defensivos de verificação de ponteiros.

#### Gerenciamento de Memória Temporária (Scratch Arena)
Algoritmos densos de álgebra linear (como decomposição LU, QR, SVD e GEMM de alto desempenho) frequentemente requerem memória temporária para blocos de packing. No Arandu, essa memória nunca é requisitada silenciosamente ao sistema operacional:

```arandu
import std.math.linalg as linalg
import std.alloc.arena as arena

// 1. Pergunta ao algoritmo quanto workspace temporário é exigido para as dimensões
let scratch_bytes = linalg.gemmScratchRequirements(matrix_a, matrix_b)

// 2. Reserva a memória uma única vez em uma ScratchArena
let mut scratch = arena.ScratchArena::withCapacity(scratch_bytes, allocator)

// 3. Executa centenas de milhares de iterações sem NENHUMA alocação intermediária
for step in 0..100_000 {
    scratch.reset() // O(1): apenas reseta o ponteiro de bump
    linalg.gemmInto(matrix_a, matrix_b, &mut result, &mut scratch)
}
```

---

### 3.5. Matrizes Estáticas na Stack vs Matrizes Dinâmicas

Evita-se a ambiguidade entre pequenos vetores de transformações geométricas e matrizes dinâmicas de dados:

```arandu
// Matriz de dimensões estáticas: reside integralmente na stack
// sizeof = 4 * 4 * 4 = 64 bytes | Alocações na heap = 0
let transform: StaticMatrix<f32, 4, 4> = StaticMatrix::identity()

// Matriz dinâmica: cabeçalho na stack, buffer alocado na heap via Allocator
let big_matrix: Matrix<f32, &GlobalAllocator> = Matrix::zeros([2048, 2048], &allocator)
```

---

### 3.6. `arandu_data`: O Padrão Apache Arrow e DataFrames Lazy

Para tabelas analíticas, o Arandu adota a representação SoA (Structure of Arrays) compatível com o Apache Arrow:

```arandu
import std.data.schema as schema
import std.data.dataframe as df

// Construção de esquema tipado
let user_schema = schema.Schema::new([
    schema.Field::new("user_id", schema.Type::I64, nullable: false),
    schema.Field::new("age", schema.Type::I32, nullable: true),
    schema.Field::new("username", schema.Type::Utf8, nullable: false)
])

// RecordBatch colunar:
// - user_id: buffer plano contíguo de i64 (sem overhead de objetos)
// - age: buffer contíguo de i32 + Validity Bitmap (1 bit por registro para indicar nulo)
// - username: buffer contíguo de bytes UTF-8 + buffer de offsets i32
```

#### Zero-Copy FFI via Arrow C Data Interface
Qualquer `RecordBatch` ou `DataFrame` do Arandu implementa a [Arrow C Data Interface](https://arrow.apache.org/docs/format/CDataInterface.html) (`ArrowArray` e `ArrowSchema`). Isso permite transferir terabytes de dados tabulares entre Arandu, Python (PyArrow/Polars/DuckDB) e C++ sem cópia de dados e sem acoplar bibliotecas externas.

#### Regra de Inicialização e Segurança
```text
UninitBuffer  ──(fill)──►  InitializedBuffer  ──(validação estática)──►  Arrow Exportable
```
Memória não inicializada é permitida exclusivamente durante a construção interna para ganho de throughput. O sistema de tipos do Arandu proíbe a exportação via FFI/IPC de qualquer buffer que não tenha passado pela transição tipada para estado completamente inicializado, impedindo vazamento acidental de dados da memória do processo.

#### Motor de Consultas Lazy (Estilo Polars/DataFusion)
Consultas sobre conjuntos de dados analíticos não materializam buffers intermediários:

```arandu
let plan = df.scanParquet("vendas_100gb.parquet")
    .filter(col("valor") > 1000.0)
    .select(["id_cliente", "valor", "data"])
    .groupBy("id_cliente")
    .aggregate([col("valor").sum().alias("total_vendas")])

// Execução otimizada em streaming por batches
let result = plan.collect(allocator)
```

O otimizador aplica:
1. **Predicate Pushdown**: avalia o filtro `valor > 1000.0` diretamente durante o decodificador de páginas do arquivo Parquet/CSV.
2. **Projection Pushdown**: decodifica e carrega para a memória apenas as colunas `id_cliente`, `valor` e `data`, ignorando dezenas de outras colunas presentes no arquivo.
3. **Chunked Streaming**: processa os dados em lotes (ex: 64.000 linhas por batch), mantendo a pegada de memória residente (RSS) controlada e constante, mesmo para arquivos maiores que a RAM da máquina.

---

### 3.7. Regras Canônicas de Promoção Numérica

Diferente do NumPy histórico (que realizava promoções surpreendentes dependendo do valor escalar de uma variável em runtime), o Arandu adota um lattice rígido inspirado no JAX e na especificação moderna [Array API](https://data-apis.org/array-api/latest/API_specification/type_promotion.html):

```text
       f64
        ▲
        │
       f32
        ▲
        │ (cast explícito obrigatório)
   ┌────┴────┐
  i64       u64
   ▲         ▲
  i32       u32
   ▲         ▲
  i16       u16
   ▲         ▲
  i8        u8
```

- **Literais contextuais**: Em `arr_f32 + 2.0`, o literal `2.0` recebe o tipo contextual `f32`.
- **Proibição de coerção heterogênea implícita**: `arr_i64 + arr_f32` gera erro de compilação diagnóstica. É obrigatório escrever expressamente `arr_i64.cast<f32>() + arr_f32`.
- **Sem promoção dependente de valor**: O tipo do resultado depende unicamente dos tipos estáticos dos operandos, jamais do valor numérico em tempo de execução.

---

### 3.8. Estruturas Esparsas e Grafos via GraphBLAS

`arandu_science` e `arandu_math` rejeitam o modelo de grafos orientados a ponteiros (`Node*`, `Edge*`), modelando grafos através de matrizes esparsas:

```arandu
import std.math.sparse as sparse
import std.science.graph as graph

// 1. Construção rápida e flexível usando formato Coordinate (COO)
let mut builder = sparse.CooBuilder<f32>::new(n_vertices, n_vertices)
builder.push(0, 1, 1.0)
builder.push(1, 2, 1.0)

// 2. Congelamento em Compressed Sparse Row (CSR) para cálculo imutável de alto desempenho
let adj_matrix: sparse.CsrMatrix<f32> = builder.buildCsr(allocator)

// 3. Algoritmos de grafos executados via Semirings (GraphBLAS)
// BFS, PageRank e Caminhos Mínimos expressos como multiplicações esparsas (SpMV / SpGEMM)
let ranks = graph.pageRank(adj_matrix, damping: 0.85, max_iterations: 100)
```

---

### 3.9. FFT Baseada no Conceito de Planos (FFTW)

Seguindo o design comprovado do FFTW, o cálculo de transformadas rápidas de Fourier é desacoplado em fase de planejamento e fase de execução:

```arandu
import std.math.fft as fft

// Cria o plano de execução uma única vez
// Strategy.Estimate: inicialização rápida
// Strategy.Measure: benchmarking de microkernels para máxima vazão em HPC
let plan = fft.FftPlan1D<f32>::new(size: 4096, strategy: .measure, allocator)?

// Reutiliza o plano para milhões de sinais com zero alocações adicionais
for signal in batch_of_signals {
    plan.executeInto(signal, &mut spectrum_output)
}
```

---

### 3.10. Geração de Números Pseudo-Aleatórios (RNG)

O módulo de aleatoriedade rejeita estado global mutável e separa o gerador de bits das distribuições:

```arandu
import std.math.random as random

// Gerador counter-based Philox: estado minúsculo, ideal para SIMD, GPU e threads paralelas
let rng = random.Philox4x32::new(seed: 42, stream: 0)

// Distribuições utilizam o gerador explicitamente
let normal = random.NormalDistribution::new(mean: 0.0, std_dev: 1.0)
normal.fillInto(&mut buffer, &mut rng)
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1. Impacto no Pipeline do Compilador

#### Frontend e AST
- Introdução de açúcares sintáticos de fatiamento multidimensional `arr[i, j, k]` e intervalos `start..end:step`.
- Suporte canônico a literais de matrizes estáticas: `[[1.0, 0.0], [0.0, 1.0]]`.

#### Inferência de Tipos & Typeck
- Validação formal do lattice de tipos numéricos.
- Checagem de dimensões estáticas em `StaticMatrix<T, M, N>` através do sistema de *const generics*.
- Verificação estrita de que argumentos de destino em funções `*_into` possuem empréstimo exclusivo (`&mut`) sem sobreposição com os empréstimos compartilhados (`&`) das entradas.

#### AMIR e Vetorização
- Reconhecimento de padrões de pipelines elementwise para fusão de laços:
  ```arandu
  zip(a, b, c).map(|x, y, z| x + y * z).writeInto(&mut out)
  ```
  O pass manager da AMIR identifica sequências lineares de operações elementwise sobre layouts contíguos e sintetiza um único laço vetorizado diretamente em SSA, sem gerar nenhum vetor intermediário na stack ou heap.
- Geração de metadados de *no-alias* para os backends C e Cranelift, permitindo a emissão de instruções AVX2, AVX-512 e ARM NEON sem verificações condicionais em runtime.

#### Backends e Integração com BLAS
- Implementação de microkernels de packing e blocking nativos no Arandu (arquitetura GotoBLAS/OpenBLAS) para a camada intermediária de GEMM.
- Conector de FFI de baixo overhead para chamar bibliotecas de sistema (OpenBLAS, BLIS, Apple Accelerate, Intel oneMKL) sem custos de marshalling de dados. A seleção de backend é fornecida através de um descritor de contexto ou resolução em link-time, sem variáveis globais.

---

### 4.2. Contrato de Alocação e Testes de Regressão

Para cumprir o princípio fundamental de *Zero Hidden Allocation*, os testes de benchmark da suíte `SL_T` incorporarão um interceptor de alocações:

```arandu
test "gemm_zero_allocation_guarantee" {
    let mut scratch = linalg.gemmScratch(a, b, allocator)

    let tracker = alloc.startAllocationTracker()
    linalg.gemmInto(a, b, &mut c, &mut scratch)
    let stats = tracker.stop()

    assert_eq(stats.total_allocations, 0)
    assert_eq(stats.bytes_allocated, 0)
}
```

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### Preservação dos Invariantes do Compilador
1. **Sem I/O em Queries Puras**: As operações matemáticas e analíticas são funções puras e determinísticas.
2. **Determinismo Numérico**: Reduções paralelas de ponto flutuante (ex: `sum()`, `dot()`) adotam algoritmos de acumulação com ordem estável (como somatórios em árvore de altura fixa ou compensação de Kahan), evitando discrepâncias no último bit geradas pelo número de threads da máquina executora.
3. **Ordem Independente de Hash**: Nenhuma iteração sobre estruturas esparsas ou agrupamentos de DataFrame expõe ordem não-determinística ao usuário.

### Custos de Complexidade
- A separação estrita entre matrizes na stack (`StaticMatrix`) e na heap (`Matrix`) exige que o usuário decida conscientemente a representação de dados apropriada, aumentando a curva de aprendizado inicial em troca de controle e performance previsível.
- A exigência de fornecer um buffer de scratch explícito em algoritmos avançados adiciona linhas de código no setup, compensadas por ganhos drásticos de throughput em loops contínuos.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

- **Alternativa Rejeitada: Unificar Tensores e DataFrames**: Tentar criar um `Tensor` genérico com suporte a nulos ou um `DataFrame` que suporte strides multidimensionais arbitrários. Isso foi rejeitado porque os dois domínios possuem requisitos de hardware incompatíveis: tensores exigem densidade contígua e aritmética vetorial pura; dados tabulares exigem bitmasks de nulidade, esquemas heterogêneos e compressão por coluna.
- **Alternativa Rejeitada: C++ Expression Templates**: Rejeitada terminantemente para proteger a velocidade do compilador Arandu e evitar mensagens de erro convolutas.
- **Alternativa Rejeitada: Estado Global para Configuração de BLAS / RNG**: Rejeitada por violar o princípio de concorrência pura, thread-safety e determinismo arquitetural do Arandu.

---

## 7. Arte Prévia (Prior Art)

- **faer-rs**: Pioneira no modelo moderno de álgebra linear em Rust com `MemStack` explícito e microkernels de alta performance sem dependência de C/Fortran.
- **Apache Arrow & Polars**: O estado da arte global em computação colunar analítica, processamento em streaming e interoperabilidade zero-copy.
- **C++ `std::mdspan`**: Estabeleceu o padrão de ouro para abstração multidimensional não-proprietária e desacoplada da política de alocação de memória.
- **JAX / Array API Standard**: Demonstrou a viabilidade de eliminar o débito técnico de coerção de tipos do NumPy através de lattices formais.
- **GraphBLAS**: Provou que análise de grafos em larga escala atinge máxima performance quando formulada como álgebra linear esparsa.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Sintaxe de Fatiamento Avançado**: Qual a notação ideal para sub-slices de strides não-unitários (`arr[0..100:2]`) de forma a manter o parser Rowan simples e sem ambiguidades léxicas?
2. **Estratégia de Redução Paralela**: Qual o limiar de tamanho de bloco em que o overhead de compensação de Kahan/árvore de somas balanceada se torna insignificante em relação ao ganho de autovetorização?

---

## 9. Possibilidades Futuras e Marcos do Roadmap (Future Possibilities & Milestones)

A implementação da stack científica será executada através de quatro marcos graduais:

Os itens de SCI.1 abaixo são critérios de saída; somente a fundação descrita na
seção 1.1 está presente na árvore atual.

- **SCI.1 — Arandu Math v1**:
  - Implementação de `Array<T, N, A>`, `ArrayView<T, N>` e `ArrayViewMut<T, N>`.
  - Separação entre `StaticMatrix<T, M, N>` e `Matrix<T, A>`.
  - Funções com destino explícito (`*_into`) e suporte a `ScratchArena`.
  - Operações elementwise com autovetorização SIMD e reduções determinísticas.
  - Microkernels bloqueados nativos para GEMM e conector FFI modular para BLAS/MKL.
- **SCI.2 — Arandu Data v1**:
  - Estruturas colunares compatíveis com layout Apache Arrow (`RecordBatch`, validity bitmaps, offset buffers para strings).
  - SoA explícito (`StructArray<T>`).
  - Exportação e importação zero-copy via **Arrow C Data Interface**.
  - Leitores/escritores de streaming para CSV, Arrow IPC e Parquet.
- **SCI.3 — Arandu Compute**:
  - Motor de execução de consultas preguiçosas (*lazy query engine*).
  - Otimizador lógico e físico com *predicate pushdown*, *projection pushdown* e *slice pushdown*.
  - Processamento em streaming por lotes (*batch streaming execution*).
- **SCI.4 — Arandu Science**:
  - Matrizes esparsas em formatos COO, CSR, CSC e BSR.
  - Implementação do padrão GraphBLAS e semirings para análise de grafos e redes complexas.
  - Transformada rápida de Fourier baseada em planos de execução (`FftPlan`, estratégias `estimate`/`measure`).
  - Gerador pseudo-aleatório counter-based (Philox) e distribuições estatísticas desacopladas.
  - Solvers para equações diferenciais ordinárias (ODEs) e processamento de sinais digitais.
