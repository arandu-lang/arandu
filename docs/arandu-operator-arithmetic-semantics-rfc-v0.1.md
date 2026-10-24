# Semântica de operadores e aritmética v0.1

**Estado em 16/10/2026:** contrato descritivo do recorte implementado, com
limites conhecidos. A proposta original de trapping universal de overflow não
foi adotada como comportamento padrão de execução. Mudanças futuras dependem
de decisão explícita; a fila executiva fica no
[roadmap mestre](./arandu-compiler-roadmap-v0.1.md).

## Visão Geral e Contexto

Este documento distingue checagem de operadores, execução nos backends,
propagação de constantes e CTFE. Aceitar uma expressão no type checker não prova
paridade de todos os casos de borda de C, Cranelift e Wasm.

O texto original propunha traps para todo overflow, shifts dinâmicos saturados
e dispatch de operadores por `PartialEq`/`Ord`. Esses pontos eram propostas;
não descrevem integralmente o compilador atual e não devem fundamentar alegações
de eliminação de CWE-190 ou ausência geral de comportamento indefinido.

## Detalhes Técnicos da Implementação

### Tipos e operadores admitidos

O proprietário da checagem é
[`binary.rs`](../crates/arandu_typeck/src/type_checker/synth/expr/binary.rs).
O corpus
[`semantic_edge_cases.rs`](../crates/arandu_query/tests/semantic_edge_cases.rs)
registra aceitação e diagnósticos para as regras abaixo.

| Operação | Recorte atual |
| --- | --- |
| `+`, `-`, `*`, `/` | Tipos numéricos compatíveis; inteiros e floats têm regras distintas de execução |
| `%` | Inteiros; float é rejeitado com `T005` |
| Negação unária | Signed e float; unsigned é rejeitado com `T005` |
| `!`, `&&`, `\|\|` | Booleanos; `&&`/`\|\|` têm lowering de curto-circuito |
| `~`, `&`, `\|`, `^` | Inteiros |
| `<<`, `>>` | Inteiros; literal negativo ou fora da largura do operando esquerdo é rejeitado com `T038` |
| `==`, `!=` | Escalares admitidos, strings por conteúdo, referências/ponteiros e carriers conforme seu lowering |
| `<`, `<=`, `>`, `>=` | Numéricos e `char`; bool, string, structs e ponteiros crus são rejeitados |

`int`/`uint` têm 32 bits; `isize`/`usize` seguem o alvo. `char` representa
Unicode scalar value, excluindo surrogate codepoints. Layout não deve ser
inferido do host ao analisar outro target.

### Overflow: execução, SCCP e CTFE

As operações inteiras comuns `+`, `-` e `*` usam instruções modulares no
Cranelift (`iadd`, `isub`, `imul`) e Wasm. Não há checagem universal de overflow
inserida no lowering dessas operações. No C, o emissor produz operadores C;
a suíte de paridade usa `-fwrapv` para signed overflow. Quem compila a saída
`emit-c` deve preservar as opções exigidas pelo contrato; o texto C isolado não
prova semântica modular portátil em todas as toolchains.

Fontes:
[`lower_amir/ops.rs`](../crates/arandu_semantics/src/passes/lower_amir/ops.rs),
[`Cranelift compare.rs`](../crates/arandu_backend_cranelift/src/translator/compare.rs),
[`C expr.rs`](../crates/arandu_backend_c/src/emitter/expr.rs),
[`Wasm arith.rs`](../crates/arandu_backend_wasm/src/translator/arith.rs) e
[`parity_tests.rs`](../crates/arandu_backend_c/tests/parity_tests.rs).

`T038` valida a representabilidade de literais e contagens de shift; não é
uma garantia de rejeição estática de toda expressão aritmética que transborda.
O SCCP normaliza constantes conforme largura/sinal quando conhece o tipo e
preserva operações potencialmente trapping quando não demonstra avaliação
segura. O avaliador CTFE, por sua vez, verifica overflow e retorna erro no
subconjunto admitido; não troca silenciosamente esse erro por wrap. Essa
política de avaliação obrigatória é distinta da execução comum.

Fontes e regressões:
[`sccp.rs`](../crates/arandu_mir/src/sccp.rs),
[`ctfe/mod.rs`](../crates/arandu_mir/src/ctfe/mod.rs) e
[`ctfe/tests.rs`](../crates/arandu_mir/src/ctfe/tests.rs).

### Divisão, resto e ponto flutuante

Divisão/resto **inteiros** por zero literal são rejeitados com `T040`.
`42.0 / 0.0` é aceito e conserva o comportamento IEEE de infinito/NaN;
`%` sobre floats é rejeitado com `T005`, inclusive com divisor zero.

Em execução, divisão e resto inteiros por divisor dinâmico zero causam trap.
Divisão signed de `MIN / -1` também causa trap, inclusive em `i8` e `i16`;
o resto `MIN % -1` retorna zero. A divisão válida trunca em direção a zero e
o resto signed tem o sinal do dividendo. Cranelift usa `sdiv`/`srem`; Wasm
acrescenta a checagem de overflow para signed estreito, cujo slot é `i32`.
O emissor C verifica zero e overflow antes de `/`, e desvia `% -1` para zero
sem avaliar uma operação C indefinida. Essas proteções independem de `-fwrapv`.
Os testes de paridade incluem subprocessos para traps e resultados nos níveis
AMIR O0/O1/O2/Os; a geração C de traps é executada também com -O0/-O1/-O2/-Os.
A comparação de valores
IEEE também não pode assumir `x == x` quando `x` pode ser NaN; há regressão em
[`ieee754_semantics.rs`](../crates/arandu_query/tests/ieee754_semantics.rs).

### Shifts e comparações

A validação estática cobre contagens literais. Em execução, contagens dinâmicas
são mascaradas por 31 para operações em slots de 32 bits e por 63 para slots de
64 bits. Inteiros de 8 e 16 bits são estendidos conforme sinal para um slot de
32 bits, deslocados, e truncados para o tipo de resultado. Isso significa que
um `u8` deslocado por 8 bits resulta em zero, enquanto uma contagem dinâmica 32
equivale a zero nesse slot. Uma contagem signed negativa usa os mesmos bits
baixos da máscara. Isso não adota a antiga proposta de shifts saturantes.

O shift direito signed estende o sinal; o unsigned preenche com zero. O shift
esquerdo conserva os bits baixos do resultado. No C, deslocamentos são feitos
por unsigned, com máscara explícita e reconstrução do sinal; não dependem de
`<<` signed negativo/transbordante nem de `>>` signed negativo da toolchain.
O type checker continua exigindo tipos compatíveis nos dois operandos.

Igualdade direta de structs, tuplas, arrays e slices não é admitida pelo filtro
atual do type checker; implementar uma interface não ativa automaticamente
sobrecarga de `==`. `Option` e `Result` têm lowering dedicado que pode comparar
recursivamente payloads agregados. Isso não libera `==` direto sobre qualquer
agregado. O proprietário desse tratamento é
[`lower_amir/equality.rs`](../crates/arandu_semantics/src/passes/lower_amir/equality.rs).
Ponteiros crus admitem igualdade de endereço, mas relações `<`/`>` são
rejeitadas mesmo em bloco `unsafe`; não se promete ordenação unsigned pública.

## PONTOS DE MELHORIA (O que não está no roadmap)

Paridade de casos de borda depende de tipo, alvo, nível de otimização e opções
da toolchain C. Os testes citados cobrem um corpus finito e não demonstram
correção geral das transformações ou segurança de toda aplicação.
Sobrecarga arbitrária, operadores saturantes e mudança global da política de
overflow exigem propostas próprias; não são entregas implícitas deste contrato.

## Futuro e Próximos Passos

A priorização pertence ao roadmap mestre. Qualquer ampliação deve explicitar
runtime versus CTFE, comportamento por target e regressões de paridade, sem
alterar silenciosamente o significado de programas existentes.
