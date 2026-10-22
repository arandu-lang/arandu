# RFC 0038: SIMD portável de 128 bits e segurança de alinhamento

- **Número:** 0038
- **Status:** `Draft`
- **Data de início:** 2026-10-16
- **Revisão de implementação:** 2026-10-18
- **Área:** Middle-end, backends e stdlib
- **Fila de entrega:** ST.2 no [roadmap canônico](../arandu-compiler-roadmap-v0.1.md)

## Estado e fronteira pública

O frontend liga os seis tipos canônicos de `std.core.simd` a
`ArType::Vector(VectorType)`. Declarações opacas são construídas por intrínsecos
com marcador ABI e assinatura exata; uma struct homônima não ganha essa
representação. Métodos, aliases e reexports preservam a identidade do símbolo.
A API fonte usa splat, aritmética, extração/inserção, comparação e seleção da
AMIR, em vez dos antigos campos escalares.

`Mask8x16`, `Mask16x8`, `Mask32x4` e `Mask64x2` preservam máscaras de vias
canônicas em armazenamento privado. `loadUnaligned` recebe uma fatia compartilhada.
`storeUnaligned` recebe `MutableSlice<Owner, T>`, que mantém um empréstimo
exclusivo de seu proprietário e não pode ser copiada. `Vec.asMutSlice` constrói
essa vista com segurança; `MutableSlice.fromRaw` é uma fronteira unsafe para
outros buffers. Uma `mut ref []T` isolada empresta apenas o descritor e não
prova exclusividade dos dados: o caminho `storeUnalignedSlice` exige unsafe e
essa exclusividade adicional explícita.

Ambos os stores verificam a faixa inteira antes de escrever. As operações usam
alinhamento escalar e não prometem uma única instrução de memória. Bounds não
substituem duração, proveniência ou exclusividade.

A RFC continua Draft: CTFE vetorial, ABI FFI vetorial estável, benchmarks e
execução dos artefatos instalados em todos os alvos não são inferidos dos testes
locais. A fila permanente está em ST.2 do roadmap.

## Motivação e limites das fontes

Vetores explícitos permitem expressar paralelismo sem depender exclusivamente
do reconhecimento heurístico de loops. Sua vantagem depende do algoritmo,
layout, tráfego de memória, alvo e custo de legalização; precisa de medição.
Não há garantia geral de uma instrução por operação ou de ganho sobre escalares.

[Highway](https://github.com/google/highway/blob/master/README.md) oferece vetores
completos e `FixedTag<T, N>`; não estabelece uma superioridade universal de
vetores fixos de 128 bits. [Rust portable SIMD](https://doc.rust-lang.org/core/simd/)
explicita diferenças de subnormais em alguns alvos e a possibilidade de uma
operação precisar de múltiplas instruções. Seu estado experimental não demonstra
que uma causa única, como monomorfização, explique a estabilização. Esta RFC não
usa cronologias ou percentuais de desempenho sem benchmark reproduzível.

## Tipos e layout

`VectorType` é uma enumeração fechada, em vez de um par arbitrário de tipo e
comprimento que cada consumidor precisaria revalidar:

| Forma | Tipo da via | Vias | Tamanho | Alinhamento |
| --- | --- | --- | --- | --- |
| F32x4 | f32 | 4 | 16 | 16 |
| F64x2 | f64 | 2 | 16 | 16 |
| I32x4 | i32 | 4 | 16 | 16 |
| I16x8 | i16 | 8 | 16 | 16 |
| U8x16 | u8 | 16 | 16 | 16 |
| I64x2 | i64 | 2 | 16 | 16 |

Não admite `float`, `int`, zero vias, larguras arbitrárias ou vetores de 256/512
bits. O `LayoutEngine` mantém 16/16 também em alvos de ponteiro de 32 bits e
ABI i686 com alinhamento escalar de 64 bits reduzido. Arrays e agregados devem
propagar alinhamento e padding. `TypeShape`, nomes de instância e hashes
incrementais/CGU conservam a identidade exata da forma.

São valores Copy sem identidade, empréstimos internos ou destructor. Não
concedem mutabilidade interior. CTFE rejeita operações vetoriais explicitamente
até existir representação/admissão e semântica próprias na VM.

## AMIR e semântica inicial

A AMIR possui nós puros dedicados:

- `VectorSplat { value }` difunde um escalar com tipo exato da via.
- `VectorOp { op, left, right }` recebe dois vetores da mesma forma.
- `VectorExtractLane { vector, lane }` produz o escalar da via.
- `VectorInsertLane { vector, lane, value }` substitui uma via estaticamente válida.
- `VectorCompare { op, left, right }` produz máscara `U8x16` com todos os bits
  da via verdadeira ligados, ou todos desligados.
- `VectorSelect { mask, if_true, if_false }` seleciona bits de vetores da mesma
  forma usando a máscara `U8x16`, sem fluxo condicional na AMIR.
- `VectorShuffle { left, right, mask: [u8; 16] }` seleciona bytes da concatenação
  de dois vetores; índices válidos são 0 a 31. É um shuffle **de bytes**, não uma
  máscara de vias. A ordem canônica dos bytes de cada via é little-endian.

Add/Sub existem para todas as formas. Mul existe exceto para U8x16. Div existe
somente para floats. And/Or/Xor existem somente para inteiros. Inteiros operam
com wrapping na largura da via; divisão inteira não faz parte deste recorte.
Comparações inteiras respeitam sinal, exceto U8x16, que é sem sinal. Eq/Lt/Le/
Gt/Ge são ordenadas para floats; Ne é verdadeira para pares com NaN. Zeros
com sinais diferentes comparam iguais. Máscaras expõem `allTrue`, `anyTrue`,
`testLane`, operações booleanas e construção a partir de bools.
Floats não usam fast-math nem fusão implícita; NaNs
seguem as permissões do backend, sem promessa de payload aritmético idêntico.

O validador rejeita incompatibilidade de forma/tipo, operações não admitidas,
lanes fora dos limites e índices de shuffle >=32 antes do codegen. Splat e
insert usam operandos escalares tipados; literais sem identidade de tipo devem
ser materializados em temporários antes desses nós.

Visitors compartilhados percorrem todos os operandos, incluindo inserção e
shuffle. SSA, inlining, DCE, liveness, reachability e hashes preservam seus usos.
Não há execução Salsa, filesystem ou efeitos de runtime nessa representação.

## Lowering e memória

### Cranelift

Cada forma corresponde a `F32X4`, `F64X2`, `I32X4`, `I16X8`, `I8X16` ou `I64X2`.
Splat, aritmética, comparação, seleção, extração/inserção e shuffle geram IR
vetorial. Shuffle usa
bitcast de 128 bits para bytes; não converte numericamente as vias. Locais com
endereço tomado recebem célula real de 16 bytes alinhada a 16. `BlackBox` usa
célula alinhada e barreira opaca de ponteiro, sem truncar para inteiro de 64 bits.
As alocações de backing do compilador usam um par de alocação/liberação
com alinhamento mínimo de 16, inclusive para células nullable. A legalização
pode emitir múltiplas instruções conforme o alvo.

O par de alocação raw usa um cabeçalho de 16 bytes para recuperar o layout na
liberação. Isso acrescenta memória e pode custar tempo; é uma correção de
alinhamento, não uma otimização. Um microbenchmark local comparou o par anterior
malloc/free e o novo par com sete rodadas de 200 mil alocações/liberações por
tamanho (1, 16, 64, 512 e 4096 bytes), além de 100 mil blocos de 64 bytes vivos.
As medianas de pares de 16 bytes foram 10,11 ns e 12,10 ns; para 4096 bytes,
28,56 ns e 75,03 ns. Sem pinning nem workload completo do compilador, esses
valores descrevem somente esse processo local e não autorizam uma promessa de
throughput, uma comparação entre backends ou uma regressão universal.

### C

Os tipos possuem membro nativo `__attribute__((vector_size(16)))` em GCC/Clang,
com acesso às vias por união. Inteiros usam membro vetorial sem sinal para
preservar wrapping. O fallback selecionável por `ARANDU_SCALAR_SIMD` usa vias
escalares e `_Alignas(16)`; operações inteiras evitam overflow de promoção e
copiam bits sem violar aliasing. Há asserts de tamanho e alinhamento.

Esse fallback evita exigir extensões vetoriais. **Não certifica MSVC/TCC**:
o emissor C existente usa outras extensões GNU, incluindo statement expressions.
A compatibilidade com cada compilador exige seu runner e configuração reais.
Não se declara ABI FFI vetorial estável entre compiladores C e Cranelift.

### WebAssembly

Vetores ocupam um slot `v128`, incluindo parâmetros e retornos internos.
Operações emitem instruções SIMD, cargas/stores usam 16 bytes, e constantes zero
usam `v128.const`. Células com endereço tomado continuam representadas por
ponteiro `i32`. O runtime que executa o módulo precisa suportar SIMD.

O Component Model não tem mapeamento canônico vetorial neste contrato.
Fronteiras públicas/importadas com vetores, inclusive aninhados, são rejeitadas
com U001. Exponha interfaces escalares/listas; não há fallback silencioso para
`u32`. A geração WIT avulsa usa a mesma validação e retorna `None` para
assinaturas vetoriais diretas ou aninhadas; `emit_component` fornece o diagnóstico.
SIMD interno ao componente não implica SIMD em sua interface WIT.

## Provas e próximos contratos

As regressões compartilham AMIR tipada entre JIT, C e Wasm: matriz de operações,
memória alinhada, inserção/extração, shuffle de dois inputs e passagem/retorno
entre funções. C exercita modo nativo e fallback; JIT exercita O0/O2; Wasm é
validado e executado em Wasmtime sem imports de host nesses casos puros.
Essas provas locais não substituem runners nativos ARM/macOS/Windows, benchmarks,
validação do SDK instalado nem semântica CTFE. O corpus fonte cobre as seis
formas, máscaras, NaN, zero com sinal, índices inválidos e limites de fatias;
a origem canônica também é testada através de aliases e reexports.
A fila e os critérios pendentes pertencem exclusivamente a ST.2 do roadmap.
