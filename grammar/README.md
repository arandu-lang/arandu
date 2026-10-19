# Arandu Formal Grammar

Esta pasta contém a especificação formal completa da linguagem Arandu em notação EBNF (ISO/IEC 14977-inspired), estruturada segundo as melhores práticas de especificação de linguagens de sistemas (Rust Reference e Go Specification).

## Arquivos

| Arquivo | Descrição |
| --- | --- |
| [`lexer.ebnf`](./lexer.ebnf) | **Gramática Léxica:** Transforma o fluxo de caracteres Unicode em tokens terminais (`IDENT_VALUE`, `IDENT_TYPE`, `INT_LIT`, palavras-chave). Define a regra determinística de Inserção Automática de Ponto e Vírgula (ASI) e comentários aninhados. |
| [`parser.ebnf`](./parser.ebnf) | **Gramática Sintática:** Consome os tokens gerados pelo lexer e especifica a estrutura canônica da árvore sintática (AST/CST): declarações (`let`, `func`, `struct`, `impl`), tipos, statements (`if`, `for`, `match`), metaprogramação (`comptime`), patterns e hierarquia estratificada de precedência de expressões. |

## Relação com o Compilador

O compilador Arandu utiliza um parser manual (*hand-written*) baseado em CST-first com **Rowan** e um **Pratt parser** para expressões com recuperação de erros:
- CST & Parser: [`crates/arandu_parser`](../crates/arandu_parser/)
- Lexer: [`crates/arandu_lexer`](../crates/arandu_lexer/)
- Ponto de entrada unificado legado: [`arandu-grammar-v0.6.ebnf`](../arandu-grammar-v0.6.ebnf)
