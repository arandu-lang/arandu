# RFC 0029: Computação Heterogênea em GPU, Vistas Particionadas e Backend SPIR-V (`arandu_backend_spirv`)

- **Número da RFC:** 0029
- **Título:** Computação Heterogênea em GPU, Vistas Particionadas e Backend SPIR-V (`arandu_backend_spirv`)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-10-15
- **Status:** `Draft`
- **Área Principal:** `Backend` / `GPU` / `Heterogeneous Computing` (`arandu_backend_spirv`)
- **Documentos Relacionados:**
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/rfcs/0002-canonical-attribute-naming.md`
  - `docs/rfcs/0007-semantic-memory-model.md`
  - `docs/rfcs/0009-borrowed-views-safety.md`
  - `docs/rfcs/0012-scientific-computing-and-data-architecture.md`
  - `docs/rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md`

> **Estado auditado em 2026-10-15:** Proposta exploratória (`Draft`), sem crate
> de backend GPU implementado na árvore atual. A fila executiva e a alocação de
> marcos vivem no [roadmap mestre](../arandu-compiler-roadmap-v0.1.md).
> **Nota técnica fundamental:** um empréstimo local `mut ref` dentro de uma
> única invocação SIMT **não** constitui prova automática de ausência de
> *data races* entre threads distintas da malha de execução; essa propriedade
> exige vistas particionadas por escopo de execução, análise de uniformidade de
> barreiras e contratos explícitos de espaço de endereçamento descritos abaixo.

---

## 1. Resumo (Summary)

Esta RFC estabelece a arquitetura proposta para **Computação Heterogênea em GPU de Fonte Única (*Single-Source GPU Computing*)** no Arandu e especifica o futuro backend **`arandu_backend_spirv`**. A proposta permite escrever kernels de computação na sintaxe canônica do Arandu (anotados com `@Kernel`) e compilá-los diretamente a partir da AMIR para o formato binário **SPIR-V (Standard Portable Intermediate Representation)** da Khronos Group, visando execução nativa em **Vulkan Compute, WebGPU e OpenCL**.

Diferente de abordagens tradicionais que ou permitem *data races* irrestritos (CUDA/C++) ou obrigam o programador a usar tipos embrulhados verbosos, esta RFC incorpora os avanços da literatura recente de linguagens seguras para GPU (*Descend* [ACM TOPLAS 2024], *Futhark* e a análise de uniformidade do *WebGPU/WGSL*) mantendo a **sintaxe natural de fatias do Arandu (`ref [T]` e `mut ref [T]`)**:
1. **Verificação Afim de Disjunção de Índices sobre `mut ref [T]` (*Affine Thread-Index Partitioning*)**: O programador escreve código Arandu normal (`out[i] = a * x[i] + y[i]`), e o verificador da AMIR prova estaticamente que o índice de escrita `i` em um parâmetro `mut ref` é uma função injetiva de `@gpuGlobalId` (garantindo que duas threads distintas nunca escrevam no mesmo elemento sem precisar de tipos especiais na assinatura!);
2. **Análise Estática de Uniformidade e Convergência de Barreiras**: Rastreamento de fluxo de dados uniforme vs. divergente (contaminado por `@gpuLocalId` / `@gpuGlobalId`), rejeitando em compilação chamadas a `@gpuWorkgroupBarrier()` dentro de desvios condicionais divergentes (que causam deadlock de hardware ou comportamento indefinido em SIMT);
3. **Espaços de Endereçamento e Layouts Físicos Explícitos (`Std430` / `Std140`)**: Tipagem estrita das classes de armazenamento SPIR-V (`StorageBuffer`, `Uniform`, `Workgroup`, `Function`) e validação de alinhamento/padding determinístico entre Host e Device no `LayoutEngine`;
4. **Emissão Direta AMIR $\rightarrow$ SPIR-V Estruturado**: Construção de módulos SPIR-V estruturados (`OpLoopMerge`, `OpSelectionMerge`) sem passar pelo backend LLVM genérico.

---

## 2. Motivação (Motivation)

### 2.1 Os Erros Históricos das Linguagens de GPU
1. **CUDA / HIP / OpenCL C++ (Insegurança de Memória e Sincronização)**:
   Em CUDA, qualquer thread pode escrever em `out[qualquer_indice]`, acessar memória `__shared__` antes ou depois de `__syncthreads()` sem verificação estática, ou executar `__syncthreads()` dentro de um `if (threadIdx.x < 16)` divergente, travando a GPU inteira.
2. **A Armadilha do Borrow Checker de CPU Sem Análise de Índice SIMT**:
   Em uma CPU, receber `out: mut ref [f32]` em uma função garante acesso exclusivo porque há apenas **um** chamador executando aquela função naquele instante. Em uma GPU (modelo SIMT), a função `@Kernel` é instanciada simultaneamente por milhares de invocações no *grid* recebendo os mesmos buffers! Se a linguagem permitisse indexação constante `out[0] = 1.0` apenas porque o parâmetro é `mut ref`, todas as threads escreveriam no índice `0` ao mesmo tempo. Por outro lado, obrigar o programador a substituir `mut ref [f32]` por tipos burocráticos como `gpu.DistributedMut<f32>` e métodos `.claimThreadSlot()` polui o código e destrói a ergonomia.
3. **A Fragilidade de Compilar para SPIR-V via LLVM (*rust-gpu*)**:
   O LLVM realiza transformações de CFG que destroem a estrutura de controle exigida pelo SPIR-V (`OpSelectionMerge` e `OpLoopMerge`), além de introduzir ponteiros físicos onde o SPIR-V lógico (`Logical GLSL450` / `VulkanMemoryModel`) exige cadeias de acesso estruturadas (`OpAccessChain`).

### 2.2 O Estado da Arte que o Arandu Absorve (Por Baixo do Capô)
* **Descend (Köpcke et al., ACM TOPLAS 2024) + Análise Afim de Índices**: O Arandu absorve a prova matemática de particionamento por escopo de execução do *Descend*, mas realiza essa verificação **automaticamente sobre expressões de indexação `out[idx]` na AMIR**! Se `idx` é provado estaticamente injetivo em relação a `@gpuGlobalId`, a escrita `out[idx] = ...` é aceita diretamente sobre `mut ref [T]`.
* **WebGPU / WGSL Uniformity Analysis**: Formalizou uma análise estática de fluxo de controle e dados para provar que barreiras coletivas (`@gpuWorkgroupBarrier()`) só são chamadas quando todas as invocações do *workgroup* estão convergidas.
* **Futhark**: Comprovou o poder de primitivas paralelas estruturadas e dimensões estáticas para geração de código GPU livre de corridas.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 Sintaxe Limpa: Fatias Padrão (`ref [T]` e `mut ref [T]`) com Disjunção Verificada

Em um `@Kernel` do Arandu, você usa exatamente os mesmos tipos da linguagem (`ref [f32]`, `mut ref [f32]`, `ref [N]f32`) que já usa na CPU:

```arandu
@Kernel(workgroupSize = (64, 1, 1))
public func saxpy(
    a: f32,
    x: ref [f32],
    y: ref [f32],
    out: mut ref [f32]
) {
    let i = @gpuGlobalId(0)
    if i < out.len() {
        out[i] = a * x[i] + y[i]
    }
}
```

Como o compilador garante a ausência de *data races* sem poluir a sintaxe?
* `x` e `y` são `ref [f32]` (somente leitura na memória global da GPU): qualquer thread pode ler qualquer índice (`i`, `i + 1`, vizinhos de convolução) livremente.
* `out` é `mut ref [f32]`: o passe de verificação SIMT na AMIR analisa `out[i]` e comprova que `i == @gpuGlobalId(0)` é **estritamente injetivo por thread** (duas threads distintas nunca produzem o mesmo `i`). Se o programador escrever por engano `out[0] = 1.0` (índice constante invariante à thread), o compilador emite um erro claro apontando a colisão de escrita entre threads!

### 3.2 Memória Compartilhada de Workgroup e Verificação de Barreiras

Quando threads de um mesmo *workgroup* cooperam usando memória rápida on-chip (`@Workgroup`), a sintaxe continua direta e natural:

```arandu
@Kernel(workgroupSize = (64, 1, 1))
public func reduceBlock(
    input: ref [64]f32,
    blockOut: mut ref [f32]
) {
    @Workgroup var scratch: [64]f32
    let lid = @gpuLocalId(0)
    let wid = @gpuWorkgroupId(0)

    // 1. Cada thread escreve exclusivamente na sua posição `lid` da memória compartilhada
    scratch[lid] = input[lid]

    // 2. Barreira coletiva em fluxo de controle UNIFORME:
    // O compilador verifica que a barreira não está dentro de um `if` divergente
    // e libera a leitura de todo o array `scratch` pelas threads do bloco.
    @gpuWorkgroupBarrier()

    // 3. Apenas a thread líder (lid == 0) escreve a soma final do workgroup em `blockOut[wid]`
    if lid == 0 {
        let mut acc: f32 = 0.0
        for i in 0..64 {
            acc = acc + scratch[i]
        }
        blockOut[wid] = acc
    }
}
```

Se o desenvolvedor tentar colocar `@gpuWorkgroupBarrier()` dentro de um `if` dependente de `@gpuLocalId(0)`:
```text
error[G0002]: collective workgroup barrier inside non-uniform control flow
  --> src/kernel.aru:14:9
   |
12 |     if @gpuLocalId(0) < 32 {
   |        ------------------- condition depends on thread-divergent value `@gpuLocalId(0)`
13 |         ...
14 |         @gpuWorkgroupBarrier()
   |         ^^^^^^^^^^^^^^^^^^^^^^ barrier must be reached uniformly by all invocations in the workgroup
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Classes de Armazenamento SPIR-V e Regras de Escrita Afim
O sistema de tipos para `@Kernel` mapeia automaticamente os tipos nativos do Arandu para os espaços de endereçamento exigidos pelo modelo de memória lógico do SPIR-V (`OpMemoryModel Logical Vulkan` / `GLSL450`):

| Construção Arandu | Storage Class SPIR-V | Permissão e Regra de Verificação SIMT |
| :--- | :--- | :--- |
| Parâmetro `ref [T]` / `ref [N]T` | `StorageBuffer` (`NonWritable`) | Somente-leitura compartilhado por todo o grid |
| Parâmetro `mut ref [T]` / `mut ref [N]T` | `StorageBuffer` | Escrita permitida em índices afins injetivos $a \cdot \text{globalId} + b$ (ou em `wid` sob guarda `localId == 0`) |
| Parâmetro `ref Atomic<T>` / `[Atomic<T>]` | `StorageBuffer` | Acesso concorrente mediado por operações atômicas (`OpAtomic*`) |
| Parâmetro escalar / struct `Copy` por valor | `Uniform` / `PushConstant` | Constantes de despacho somente-leitura para todo o grid |
| `@Workgroup var nome: [N]T` | `Workgroup` | Escrita indexada por `localId`; leitura global liberada após `@gpuWorkgroupBarrier()` |
| Variáveis locais `let` / `let mut` | `Function` | Registradores privados da invocação individual |

Escritas com indexação dependente de dados em runtime (ex.: *scatter* indireto `out[indices[i]] = val`) exigem operações atômicas ou um bloco `unsafe`, mantendo 95% dos kernels numéricos e de IA completamente limpos e seguros sem tipos extras.

### 4.2 Análise de Uniformidade na AMIR (*Uniformity Lattice*)
Antes da emissão SPIR-V, um passe de dataflow forward na AMIR classifica cada `LocalId` no reticulado:
$$\text{Uniform} \sqsubset \text{SubgroupUniform} \sqsubset \text{Divergent}$$
* **Fontes `Divergent`**: `gpu.globalId`, `gpu.localId`, `gpu.subgroupInvocationId`, e leituras de memória indexadas por valores `Divergent`.
* **Fontes `Uniform`**: parâmetros `gpu.Uniform[T]`, constantes `comptime`, `gpu.workgroupId` (uniforme dentro do workgroup).
* **Propagação de Controle**: qualquer bloco básico cuja execução dependa de um terminador `Branch` com condição `Divergent` é marcado como `DivergentControlFlow`.
* **Regra de Barreira**: instruções `OpControlBarrier` e operações coletivas de subgrupo são rejeitadas se o bloco básico atual estiver em `DivergentControlFlow`.

### 4.3 Estruturador de CFG e Emissão Binária SPIR-V
Diferente de CPUs, o SPIR-V exige fluxo de controle estruturado explícito:
* Antes de cada `Branch` condicional que não seja cabeçalho de loop, o emissor calcula o bloco de pós-dominância imediata (*immediate post-dominator*) na árvore de pós-dominadores da AMIR e emite `OpSelectionMerge %merge_block None`.
* Em cabeçalhos de laços naturais (`OPT.2 LoopInfo`), o emissor emite `OpLoopMerge %exit_block %continue_block None`.
* CFGs irredutíveis são rejeitados com diagnóstico estruturado para funções `@Kernel`.

### 4.4 Validação de Layout Host-Device (`Std430` / `Std140`)
Tipos agregados que cruzam a fronteira Host $\leftrightarrow$ GPU devem possuir layout determinístico e livre de reordenação implícita de campos (`A4.0`):
* Tipos passados em `StorageBuffer` ou `Uniform` exigem `@Repr("C")` ou `@GpuLayout(Std430)` / `@GpuLayout(Std140)`.
* O `LayoutEngine` calcula os *ArrayStrides*, *MatrixStrides* e `Offset` de cada campo e emite as decorações `OpDecorate %struct_type Block`, `OpMemberDecorate ... Offset`.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Invariantes Preservados
* **Fronteira Limpa de Crates**: `arandu_backend_spirv` depende apenas de `arandu_middle` e `arandu_mir`. Nenhuma dependência de drivers Vulkan/CUDA entra no compilador!
* **Pureza das Queries**: A validação de `@Kernel` (efeitos, ausência de recursão, análise de uniformidade e layout `Std430`) é 100% determinística e memoizada por função/instância.

### 5.2 Limitações e Desvantagens
* **Padrões de Indexação Irregulares (Grafos Esparsos)**: Padrões onde a disjunção de índices depende de dados em runtime (ex: *scatter* indireto `out[indices[i]] = val`) não podem ser provados estaticamente por `DistributedMut` e exigem o uso de `gpu.AtomicStorage[T]` ou de um bloco `unsafe`.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

* **Alternativa A: Confiar apenas em `mut ref [N]T` comum sem vistas particionadas:**
  * *Rejeitada*: Daria uma falsa sensação de segurança ao programador, permitindo *data races* triviais entre threads de GPU em código seguro.
* **Alternativa B: Compilar via LLVM NVPTX como único alvo:**
  * *Rejeitada*: Prenderia o Arandu a GPUs Nvidia. O SPIR-V cobre Vulkan (Linux/Windows/Android), WebGPU (navegadores e Wasm) e pode ser traduzido para Metal (macOS/iOS) e DirectX 12 (DXIL).

---

## 7. Arte Prévia (Prior Art)

* **Descend (*"Descend: A Safe GPU Systems Programming Language"*, Köpcke et al., ACM TOPLAS 2024)**: Base teórica direta para escopos de execução, vistas particionadas e rastreamento de empréstimos através de barreiras `workgroupBarrier`.
* **WebGPU WGSL Specification (W3C)**: Referência formal para a análise estática de uniformidade de fluxo de controle e convergência de barreiras.
* **Futhark & Rise/Elevate**: Referências em paralelismo funcional de arrays e garantias estáticas de acesso.
* **Khronos SPIR-V Specification & `spirv-val` (`SPIRV-Tools`)**: Especificação normativa do formato binário e validador estrutural de referência.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Particionamento N-Dimensional (`2D`/`3D` Tiles)**: Qual a superfície ergonômica ideal em `std.gpu` para dividir matrizes 2D em *tiles* de `Workgroup` com bordas (*halo regions*) para convoluções e stencils?
2. **Runtime de Despacho**: O runtime de host (`std.gpu.device` sobre `wgpu`/Vulkan) viverá em pacote externo oficial out-of-tree (seguindo o modelo da `RFC 0012`) enquanto o compilador mantém apenas os tipos intrínsecos de `std.core.gpu` e o emissor `arandu_backend_spirv`?

---

## 9. Possibilidades Futuras (Future Possibilities)

* Suporte a *Subgroup Operations* (`OpGroupNonUniform*`) e matrizes cooperativas (*Tensor Cores* / `KHR_cooperative_matrix`).
* Emissor direto secundário para PTX/NVPTX ou Metal Shading Language quando necessário para plataformas sem camada Vulkan/WebGPU.
