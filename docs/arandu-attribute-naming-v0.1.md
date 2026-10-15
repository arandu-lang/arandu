# Arandu — Contrato de nomes de anotações v0.1

**Status:** implementado; aliases legados mantidos durante a janela de migração
**Escopo:** anotações declarativas públicas iniciadas por `@`; nomes internos e CLI não mudam
**Dono:** lexer/parser preservam a grafia, semântica reconhece anotações e o
formatter apenas mantém a apresentação

## Visão Geral e Contexto

Este registro concluído fixa PascalCase como grafia pública de anotações e
define a janela de migração das grafias legadas.

## Detalhes Técnicos da Implementação

### Decisão

Toda anotação pública do Arandu usa **PascalCase** e é sensível a maiúsculas e
minúsculas:

```arandu
@Test
@Link("m")
@Suppress("shadowing")
@NoFallback
@NoSuspend
@Specialize
@Repr(C)
@Destructor
```

Uma anotação é metadado declarativo reconhecido pelo compilador, não uma
chamada de função em runtime. PascalCase torna essa natureza nominal visível e
mantém o contrato coerente com `@Test`, `@Link`, `@Suppress`, `@Deny` e
`@Forbid`, que já aparecem no projeto.

Nomes compostos não usam `_` nem `-`. Siglas seguem a capitalização de uma
palavra para evitar duas grafias equivalentes: `@FfiExport`, não `@FFIExport`.

### Fronteiras de nomenclatura

Cada camada segue a convenção natural de seu domínio. Elas não precisam ter a
mesma grafia:

| Superfície | Convenção | Exemplo |
| --- | --- | --- |
| Anotação declarativa Arandu | `@PascalCase` | `@NoFallback` |
| Chamada de módulo Arandu | `camelCase` | `mem.sizeOf<T>()` |
| AST/HIR e Rust | `snake_case` | `no_fallback` |
| CLI | `--kebab-case` | `--no-generational-fallback` |
| Manifesto/configuração | `snake_case` | `no_fallback = true` |
| Código de lint passado como string | `snake_case` | `@Suppress("unused_import")` |

O conteúdo de uma string não participa da regra do identificador da anotação.
Argumentos que são tipos, valores ou símbolos continuam seguindo a convenção
da categoria correspondente.

### Operações em expressões

A partir da revisão de contrato da 0.1.10, a superfície preferida para layout é
`mem.sizeOf<T>()` e `mem.alignOf<T>()`, com `import std.core.mem as mem`.
São intrínsecos resolvidos pela declaração `extern "arandu-intrinsic"` da stdlib;
uma função comum chamada `sizeOf` não passa a ser intrínseca por sua grafia.

`@sizeOf(Type)` e `@alignOf(Type)`, introduzidos na 0.1.9, são expressões de
layout distintas das anotações declarativas no CST/AST. Continuam aceitos por
compatibilidade, sem renomeação automática pelo formatter e sem novo aviso de
depreciação nesta revisão. Não se introduzem aliases `@SizeOf`/`@AlignOf`.
Novos exemplos usam `mem.*`; testes da sintaxe anterior continuam cobrindo-a.
Uma remoção futura exige política de migração anunciada e paridade comprovada
em CTFE, constantes globais e headers/dimensões dependentes.

Para operações futuras em expressões, a direção de design é uma API de módulo,
reservando novas formas `@PascalCase` para anotações declarativas. Os nomes
`embed.bytes(path)`, `embed.str(path)` e `types.info<T>()` são **propostas**, não
APIs disponíveis. Sua definição depende de contratos próprios:

- Inclusão de assets: caminhos e raiz permitidos, representação e comprimento
  do resultado, UTF-8 para texto, limites de recursos e diagnósticos. Leitura
  pertence à camada que registra inputs; queries Salsa e a VM não fazem I/O.
  Conteúdo e dependências precisam ser inputs explícitos, com invalidação e
  cutoff testados quando o arquivo muda ou desaparece.
- Reflexão: identidade estrutural, visibilidade, domínio congelado retornado,
  orçamento e layout do alvo. A grafia proposta não habilita reflexão ou
  geração de declarações da RFC 0024.

Esta decisão não acrescenta módulos `std.embed`/`std.types`, inclusão de arquivos
ou a sintaxe de array `[comptime]u8` à linguagem.

### Registro canônico

| Anotação | Situação | Grafias não canônicas conhecidas |
| --- | --- | --- |
| `@Link` | implementada | `@link` |
| `@Test` | implementada para descoberta SL_T.0 | função livre síncrona, não genérica, sem parâmetros; `void` ou `Result<void, E>` |
| `@Benchmark` | implementada em SL_T.4 | função livre síncrona, não genérica, `func name(mut bench: testing.Benchmark): void` |
| `@Suppress` | contrato de diagnóstico | — |
| `@Deny` | contrato de diagnóstico | — |
| `@Forbid` | contrato de diagnóstico | — |
| `@NoFallback` | implementada sob nome legado | `@no_fallback`, `@no_generational_fallback` |
| `@NoSuspend` | planejada | `@nosuspend` |
| `@Specialize` | planejada | `@specialize` |
| `@Repr` | planejada | `@repr` |
| `@Destructor` | implementada para métodos consumidores | — |

Esta tabela registra nomes, não promove itens planejados a funcionalidades
implementadas. A disponibilidade de cada anotação continua pertencendo ao
roadmap da fase que implementa sua semântica.

### Contrato do compilador

1. O lexer produz `@` e o identificador sem normalizar sua grafia.
2. O parser preserva o nome original no CST e AST; não conhece a lista de
   anotações embutidas.
3. A fase semântica resolve nomes canônicos de forma exata e é a única dona da
   tabela de anotações embutidas e de seus alvos/argumentos válidos.
4. Uma grafia legada conhecida nunca pode ser silenciosamente interpretada
   como outra anotação. Durante a migração, ela deve gerar diagnóstico com
   replacement estruturado para o nome canônico.
5. Uma anotação desconhecida ou aplicada ao alvo errado deve recuperar com
   diagnóstico; código de produção não pode fazer `panic`, `unwrap` ou
   `expect` por causa dela.
6. O formatter não renomeia anotações. Mudança semântica/canônica pertence a
   quick fix ou comando de migração explícito.
7. Hover e completion exibem somente a grafia canônica e explicam alvos,
   argumentos e estabilidade da anotação.

### Migração das grafias legadas

A migração ocorre antes de declarar o contrato estável:

1. [x] Implementar uma fonte única de metadados para anotações embutidas: nome
   canônico, aliases temporários, alvos, argumentos e estado de estabilidade.
2. [x] Fazer `@NoFallback` produzir exatamente o HIR `no_fallback` já existente.
3. [x] Aceitar temporariamente os aliases implementados, emitindo aviso com
   replacement estruturado; não alterar texto automaticamente no formatter.
4. [x] Atualizar exemplos, documentação, snippets, completion e semantic tokens.
5. [ ] Remover aliases somente em uma fronteira de versão anunciada nas release
   notes e coberta por teste de migração.

Documentação nova usa apenas a grafia canônica. Documentos históricos e testes
de migração podem citar a forma antiga quando ela for necessária para explicar
o estado de uma versão. Os aliases permanecem na linha `0.1.x` e só podem ser
removidos em `0.2.0`, com release notes e regressão de migração.

### LSP e editor

- Anotações recebem classificação semântica consistente; o servidor não fixa
  cores e o tema continua responsável pela aparência.
- Completion após `@` oferece somente nomes aplicáveis ao alvo atual, com
  snippets para argumentos obrigatórios.
- A grafia legada produz quick fix a partir de replacement estruturado, nunca
  por parsing da mensagem do diagnóstico.
- Rename de símbolos comuns não renomeia anotações embutidas.
- Hover não expõe nomes internos como `no_fallback` nem detalhes de HIR.

### Testes obrigatórios para a migração

| Camada | Prova |
| --- | --- |
| Lexer/CST | preserva bytes e caixa de `@NoFallback` |
| Parser/AST | aceita anotação com e sem argumentos e mantém o span real |
| Semântica | nome canônico baixa para o mesmo comportamento interno |
| Diagnóstico | alias legado oferece replacement exato e determinístico |
| Formatter | é idempotente e não altera nomes semanticamente |
| LSP stdio | completion, hover, diagnóstico e quick fix usam UTF-16 correto |
| Extension Host | sugestão e correção funcionam no VS Code real |
| Portabilidade | fixtures mantêm os mesmos bytes em Windows, Linux e macOS |

### Referências de mercado

- Java modela anotações como tipos e usa nomes como `@Override`,
  `@Deprecated` e `@SuppressWarnings`:
  <https://docs.oracle.com/javase/tutorial/java/annotations/predefined.html>.
- Kotlin declara `annotation class Fancy` e a aplica como `@Fancy`:
  <https://kotlinlang.org/docs/annotations.html>.
- C# também modela atributos como classes e recomenda PascalCase para tipos:
  <https://learn.microsoft.com/dotnet/csharp/language-reference/language-specification/attributes>.
- Decorators TypeScript são expressões que normalmente referenciam funções,
  razão pela qual exemplos como `@sealed` usam camelCase:
  <https://www.typescriptlang.org/docs/handbook/decorators>.

O Arandu segue a família Java/Kotlin: suas anotações são metadados nominais do
compilador. A escolha não implica copiar retenção runtime, reflection ou o
modelo extensível dessas linguagens.

### Fora de escopo

- macros e annotations definidas pelo usuário;
- reflection ou retenção runtime;
- alterar nomes Rust, campos de HIR ou flags CLI;
- implementar a semântica de anotações apenas planejadas;
- escolher cores específicas para o editor.

## PONTOS DE MELHORIA (O que não está no roadmap)

Aliases legados aumentam temporariamente a superfície de reconhecimento; sua
remoção exige migração documentada e diagnóstico claro, não quebra silenciosa.

## Futuro e Próximos Passos

Remover aliases somente na edição/versionamento definidos pelo roadmap e
manter lexer, semântica, formatter e semantic tokens sincronizados.
