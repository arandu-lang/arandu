# Arandu — Efeitos, autoridade e recursos

**Estado:** representação/anotações e observação de efeitos implementadas;
modelo completo de capabilities, inferência transitiva e enforcement ainda
`partial`. Este documento preserva o desenho A2 e distingue sua base atual das
expansões propostas. A ordem de entrega pertence ao
[roadmap mestre](arandu-compiler-roadmap-v0.1.md).

## Visão Geral e Contexto

Efeitos descrevem operações realizadas por uma função; autoridade descreve o
que o ambiente permite que ela faça. `unsafe` permite operações que exigem
prova manual de memória, não concede acesso a rede, arquivos ou processos.
Um checksum identifica bytes; não prova que uma biblioteca é segura nem
restringe a autoridade do código nativo.

## Detalhes Técnicos da Implementação

### Base implementada

[`EffectFlags`](../crates/arandu_middle/src/effects.rs) usa conjuntos de bits
com nomes canônicos, união/diferença e ordenação determinística. As famílias
atuais incluem autoridade (`Net`, `FileRead`, `FileWrite`, `Environment`,
`Process`, `Foreign`), recursos (`Heap`, `Blocking`, `Suspend`, `Thread`) e
propriedades (`Pure`, `Readonly`, `NoAlloc`, `NoThrow`, `NoSuspend`).

O [type checker](../crates/arandu_typeck/src/type_checker/check/func.rs)
registra efeitos observados, compara o recorte declarado com operações
observadas e emite T039 quando incompatíveis. Calls consultam efeitos conhecidos
do callee; externs acrescentam `Foreign`, e operações específicas registram
heap/suspensão. `TypeInfo::function_effects` participa de documentação/hash.
Isso não comprova sozinho inferência interprocedural completa até fixpoint
para todo ciclo/import/instância, nem segurança de uma FFI classificada pelo nome.

Políticas de manifesto/lockfile e metadados de dependências existem no
[contrato de pacotes](arandu-project-package-lifecycle-gold-v0.1.md).
Parsing/canonicalização dessas políticas não prova enforcement de sandbox.

### Desenho preservado de A2

A evolução proposta mantém três dimensões independentes:

| Dimensão | Intenção do contrato |
| --- | --- |
| Autoridade | rede, leitura/escrita de arquivos, ambiente, processos, FFI e autoridade desconhecida |
| Recursos | heap, bloqueio, suspensão e threads, sem equiparar uso de recurso a concessão de autoridade |
| Semântica | mutação observável, não determinismo e falhas tipadas; pureza não significa que uma operação trapping possa ser especulada |

O desenho completo exige inferência sobre código resolvido/tipado e propagação
interprocedural conservadora até fixpoint. Anotações declaram um teto verificável;
omiti-las não deve ocultar comportamento. A futura classificação de
`UnknownCapability` deve se propagar e ser bloqueada por políticas rigorosas;
esse nome ainda **não** é uma flag reconhecida por `EffectFlags::from_name`.

FFI desconhecida exige uma política explícita: `Foreign` já existe; autoridade
desconhecida e risco de implementação unsafe devem ser tratados conservadoramente
no modelo ampliado. Uso interno auditável de unsafe não transforma toda API em
uma capability pública, mas tampouco é prova para otimizar acessos com alias
unknown. Resumo público e riscos de implementação são resultados distintos.

`Pure`/`Readonly`/`NoAlloc`/`NoThrow`/`NoSuspend` precisam de definição por
operação, chamada e alvo antes de justificar transformações. Readonly não
reclassifica automaticamente um borrow exclusivo; NoThrow não significa que
todo trap fatal, OOM ou falha estrangeira seja recuperável ou impossível.

A direção de metadata de pacote preserva o resumo público, riscos e mudanças
de autoridade/recursos entre versões, por exemplo `{}` → `{Net}` ou `{Heap}`.
O manifesto limita a política e o lockfile registra dados observados; assinatura,
validação da inferência e enforcement são obrigações distintas. A autoridade
real depende do runtime/SO/sandbox, não apenas da análise estática.

## PONTOS DE MELHORIA (O que não está no roadmap)

Não usar flags positivas e restrições negativas como se fossem uma única prova
de pureza. Extensões precisam definir monotonicidade, ciclos, unknown, causalidade
do diagnóstico e estabilidade do resumo público para preservar cutoff. Mudanças
somente internas não devem invalidar importadores quando o contrato permanece.

O desenho ampliado é a [RFC 0035](rfcs/0035-effect-system-polymorphism-and-capability-authority.md),
ainda `Draft`; este contrato descreve a base e a fronteira de implementação.

## Futuro e Próximos Passos

A trilha A2/capabilities do roadmap ordena: contratos de efeitos intrínsecos e
externs, inferência/summaries determinísticos, diagnósticos causais, metadata e
diff de políticas, e enforcement testado. Nenhuma etapa se torna implementada
apenas por existir uma flag, anotação ou chave no manifesto.
