# RFC 0022: Síntese Holística de Programas, Testes Diferenciais Multi-Backend e Fuzzing Incremental (`AranduSmith`)

- **Número da RFC:** 0022
- **Título:** Síntese Holística de Programas, Testes Diferenciais Multi-Backend e Fuzzing Incremental (`AranduSmith`)
- **Autor(es):** Bruno Bispo ([@BrunoF2P](https://github.com/BrunoF2P)) & Comunidade Arandu
- **Data de Início:** 2026-09-22
- **Status:** `Draft`
- **Área Principal:** `Tooling` / `Middle-end` / `Backend`
- **PR da RFC:** [Em criação]
- **Issue de Acompanhamento:** [Em criação]

---

## 1. Resumo (Summary)

Esta RFC propõe o **`AranduSmith`**: um framework de engenharia de confiabilidade de compiladores de ponta a ponta (*end-to-end*), baseado em **Síntese Dirigida por Tipos (Type-Directed Program Synthesis)**, **Testes Diferenciais Multi-Backend (Differential Testing)**, **Mutações por Equivalência de Entradas (EMI — Equivalence Modulo Inputs)** e **Fuzzing de Sessão Incremental Salsa**.

Em vez de limitar o fuzzing à injeção cega de bytes aleatórios no lexer — que em 99,9% dos casos é rejeitada nas primeiras linhas do parser sem jamais exercitar as camadas profundas do compilador —, o `AranduSmith` sintetiza programas Arandu sintática e semanticamente válidos por construção. Esses programas são paralelamente submetidos a todo o pipeline:

$$\text{CST} \longrightarrow \text{AST} \longrightarrow \text{Resolve} \longrightarrow \text{Typeck} \longrightarrow \text{AMIR} \longrightarrow \text{Otimizações (O0 / O1 / O2)} \longrightarrow \text{Backends (Cranelift, C, WASM)}$$

O framework compara a execução entre os 3 backends nativos e entre níveis de otimização, identifica automaticamente desvios semânticos (*miscompilations*), quebras de invariantes de dados, panics/ICEs, e aciona um **Redutor Hierárquico de AST (*AST Shrinker*)** para isolar o defeito em uma fixture de teste mínima e reproduzível.

---

## 2. Motivação (Motivation)

Compiladores modernos são sistemas complexos onde cada camada (Rowan CST, banco Salsa, tabelas de unificação DSU, grafo de controle SSA/OSSA, geradores de código nativo) possui invariantes formais delicados.

### Problemas Centrais Identificados:
1. **Fuzzing cego por mutação de bytes (`libFuzzer` / `Target::Pipeline` atual):**
   Gera ruído léxico. O parser do Arandu possui recuperação resiliente, o que protege contra panics na entrada, mas quase **nenhum** byte aleatório passa pelo `type_check` e pelo `lower_amir`. As fases de dataflow (SCCP, DCE, Inliner, Liveness, Definite Init) operam quase exclusivamente sobre a suíte estática de testes de regressão.
2. **Bugs de Otimização Silenciosos (*Miscompilations*):**
   Erros em que o compilador compila com sucesso (`Ok`), mas gera código de máquina com semântica alterada (ex.: eliminação prematura de atribuição em SCCP ou erro em branch threading do CFG) são invisíveis a testes de estresse sintático.
3. **Paridade entre Múltiplos Backends:**
   O Arandu oferece 3 backends de primeira classe (`arandu_backend_cranelift`, `arandu_backend_c` e `arandu_backend_wasm`). Garantir manualmente que a semântica de estruturas complexas (layouts de structs, GenRef, alinhamento de fatias, match joins) é idêntica nos três alvos é humanamente inviável.
4. **Fragilidade de Sessão Incremental:**
   O Salsa depende de *early-cutoff*. Nenhuma ferramenta industrial existente testa se uma sequência de edições incrementais randômicas (simulando um usuário digitando no VS Code) causa corrupção de cache, ciclo de query ilegítimo ou diagnósticos fantasmas.

A implementação desta RFC transforma o processo de descoberta de bugs: em vez de auditorias manuais exaustivas à procura de `.unwrap()`, o sistema sintetiza milhões de cenários adversariais por hora de forma autônoma.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

O `AranduSmith` é integrado à CLI de tarefas de desenvolvimento do repositório através do `xtask`:

```bash
# Executa uma campanha sequencial e determinística nos três backends
cargo run --locked -p xtask -- smith --iterations 1000 --seed 0 --target synthesized-all

# Compara níveis de otimização e backends para programas sintetizados
cargo run --locked -p xtask -- smith --iterations 1000 --seed 0 --target synthesized

# Executa o corpus EMI versionado nos três backends
cargo run --locked -p xtask -- smith --iterations 100 --seed 0 --target emi-corpus

# Exercita sessões incrementais de LSP em um único worker sequencial
cargo run --locked -p xtask -- smith --iterations 100 --seed 0 --target lsp-session
```

O comando `smith` aceita `--iterations`, `--seed` e `--target`; as campanhas
são sequenciais, param no primeiro panic e têm limite superior de iterações.
Os targets disponíveis são os nomes retornados por
`arandu_fuzz_support::Target::name()` (por exemplo `synthesized-all`,
`emi-corpus`, `incremental-cutoff` e `lsp-session`).

### O Fluxo de Diagnóstico e Redução:

Quando o oráculo detecta uma divergência ou falha, o `AranduSmith` não entrega um arquivo monolítico de 3.000 linhas. Ele executa redução automática via AST Shrinker e emite uma mensagem clara com uma fixture pronta para inclusão na suíte:

```text
[AranduSmith] BUG DETECTADO! (Iteration #412, Seed: 0x9f4a12c8)
Tipo: Miscompilation (Divergência entre Cranelift JIT e Backend C)
Entrada: args = []
  - Backend Cranelift: exit code 0, stdout: "resultado = 42\n"
  - Backend C (gcc):   exit code 0, stdout: "resultado = 0\n"

[AranduSmith] Iniciando Hierarchical AST Shrinker...
  Tamanho original: 342 nós AST (185 linhas)
  Passo 1 (Remoção de declarações mortas): 120 nós
  Passo 2 (Simplificação de expressões): 34 nós
  Passo 3 (Inlining e redução de escopo): 11 nós (8 linhas)
  Redução concluída em 1.4s!

[AranduSmith] Fixture mínima isolada salva em:
  tests/regressions/smith_20260922_c_cranelift_mismatch.aru

Conteúdo do teste mínimo:
------------------------------------------------------------
func main() {
    let mut x: int = 10;
    while x > 0 {
        if x == 5 { break; }
        x = x - 1;
    }
    let res = if x == 5 { 42 } else { 0 };
    println("resultado = {}", res);
}
------------------------------------------------------------
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

A arquitetura do `AranduSmith` é dividida em 5 subsistemas fundamentais localizados em `crates/arandu_fuzz_support/src/smith/`:

```
                       ┌─────────────────────────────────────┐
                       │     Seed Corpus / Pseudo-RNG        │
                       └──────────────────┬──────────────────┘
                                          │
                   ┌──────────────────────┴──────────────────────┐
                   ▼                                             ▼
     ┌───────────────────────────┐                 ┌───────────────────────────┐
     │ 1. Type-Directed          │                 │ 2. EMI Metamorphic        │
     │    AST Generator          │                 │    Mutator (Corpus)       │
     └─────────────┬─────────────┘                 └─────────────┬─────────────┘
                   │                                             │
                   └──────────────────────┬──────────────────────┘
                                          ▼
                       ┌─────────────────────────────────────┐
                       │  Arandu Program (AST / Source)      │
                       └──────────────────┬──────────────────┘
                                          │
         ┌────────────────────────────────┼────────────────────────────────┐
         ▼                                ▼                                ▼
┌──────────────────┐             ┌──────────────────┐             ┌──────────────────┐
│ Backend A:       │             │ Backend B:       │             │ Backend C:       │
│ Cranelift JIT    │             │ Backend C (GCC)  │             │ Backend WASM     │
│ (OptLevel: O0/O2)│             │ (OptLevel: O2)   │             │ (Node/Wasmtime)  │
└────────┬─────────┘             └────────┬─────────┘             └────────┬─────────┘
         │                                │                                │
         ▼                                ▼                                ▼
  Exit & Stdout                    Exit & Stdout                    Exit & Stdout
         │                                │                                │
         └────────────────────────────────┼────────────────────────────────┘
                                          ▼
                       ┌─────────────────────────────────────┐
                       │ 3. Differential Oracle Comparator   │
                       └──────────────────┬──────────────────┘
                                          │ (divergência / crash)
                                          ▼
                       ┌─────────────────────────────────────┐
                       │ 4. Hierarchical AST Shrinker        │
                       └──────────────────┬──────────────────┘
                                          │
                                          ▼
                       ┌─────────────────────────────────────┐
                       │ 5. Golden Regression Testcase       │
                       └─────────────────────────────────────┘
```

### 4.1. Gerador Sintético Dirigido por Tipos (`Type-Directed AST Synthesizer`)

Para ultrapassar a barreira de 99,9% de rejeição pelo `type_check` e `borrow_checker`, o gerador constrói a árvore de cima para baixo (*top-down*) utilizando uma tabela de símbolos abstrata de geração:

1. **Geração de Tipos e Definições:**
   - Primitivos: `int`, `uint`, `float`, `bool`, `str`, `char`.
   - Compostos: Tuplas, Vetores (`Vec<T>`), Structs com campos tipados, Enums algébricos com payloads.
2. **Ambiente de Tipagem Contextual (`ScopeEnv`):**
   - Ao gerar uma expressão em uma posição que exige o tipo `T`, o gerador seleciona com probabilidade ponderada:
     - Uma variável local em escopo com tipo exato `T` ou conversível.
     - Um literal válido de tipo `T`.
     - Uma chamada de função cujo retorno seja `T` (passando argumentos que satisfaçam seus parâmetros).
     - Uma expressão composta (ex: binária, if/else, match) cujo tipo resultante unifique com `T`.
3. **Prevenção Construtiva de Comportamento Indefinido (UB-Free by Design):**
   - **Divisão por Zero:** Todas as operações `/` e `%` são sintetizadas como operações seguras ou protegidas por guardas condicionais (`if divisor != 0`).
   - **Overflow Aritmético:** Uso preferencial de operações saturantes ou delimitadas (`(a % 1000) * (b % 1000)`).
   - **Acesso a Índices:** Índices em fatias e vetores são indexados módulo tamanho (`vec[idx % vec.len()]`).
   - **Convergência de Laços:** Laços `while` e `for` recebem contadores decrescentes de segurança para garantir terminação estrita e evitar loops infinitos nos testes.

O sintetizador implementado começa com um programa estruturado e compõe
expressões escalares por tipo; ele ainda não é um gerador arbitrário de AST com
`ScopeEnv` probabilístico. O programa cobre arrays e índices limitados,
retornos e desestruturação de tuplas, structs, payloads de enum, coleções
genéricas, `Option`/`Result`, ownership e laços limitados. Cada caminho novo
altera um resultado verificado por um oráculo independente, e as sementes
fixas são comparadas entre Cranelift, C e Wasm. A expansão para novos tipos e
formas de controle deve manter esse contrato de validade e resultado esperado.
As chamadas genéricas de pares e triplas também variam por seed entre
`int`, `uint`, `bool`, `float` e `char`; cada valor desestruturado é conferido
contra o valor calculado pelo gerador. Os argumentos dos pares e triplas passam
por chamadas aninhadas `identity<T>` da mesma especialização antes de chegar a
`make_pair<T, U>` ou `make_triple<T, U, V>`, e o teste diferencial cobre as
rotações de tipos nos três backends.

### 4.2. Mutador Metamórfico EMI (*Equivalence Modulo Inputs*)

Baseado nas pesquisas pioneiras de Le, Afshari e Su (PLDI 2014):
1. O mutador seleciona um programa válido da base (`stdlib/`, `examples/` ou `tests/`).
2. Compila e executa a referência; um trace JIT registra blocos AMIR executados, enquanto sondas sentinela selecionam um conjunto limitado de regiões mutáveis em `main`.
3. **Mutações EMI válidas:**
   - **Deleção de Código Morto:** A redução pode remover regiões e itens que não são necessários para reproduzir a falha.
   - **Injeção de Código Morto:** Insere código puro e limitado em regiões que a sonda não observou nesta execução, ou usa o fallback estático `if false { ... }`.
4. **Propriedade Metamórfica:** A saída observável do programa **deve ser matematicamente invariante**. Se o backend C ou Cranelift com `-O2` divergir após uma injeção em bloco inalcançável, detectou-se uma falha de análise no middle-end.

O estágio atual implementa sondagem dinâmica limitada para ramos `if`
aninhados, braços de `match` e handlers de bloco de `catch`, inclusive como
expressões em `main`: insere
temporariamente um `return` sentinela no bloco e executa Cranelift em O0/O1/O2.
Para corpos de `while` e `for`, a sonda declara uma flag antes do laço, marca a
entrada no corpo e verifica a flag depois dele. Isso evita inserir um retorno
inalcançável dentro de um CFG de laço, forma que já provocou ICE durante o
lowering. Se retorno, stdout e stderr do JIT permanecerem iguais à referência,
a região é tratada como não executada para essa entrada e recebe a mutação EMI.
São sondadas no máximo quatro regiões de origem por execução. Além dessas
sondas, o JIT O0 registra cada entrada em bloco AMIR como `(índice da função,
índice do bloco)`. Para provar uma região não executada, a saída observável
precisa permanecer igual e o trace original precisa aparecer como subsequência
ordenada do trace sondado; blocos extras da instrumentação são permitidos,
mas sondas inconclusivas não justificam mutação. O recorder guarda no máximo
65.536 hits por execução; traces truncados são inconclusivos. A sessão do módulo
JIT coleta hits de todas as threads que executam esse módulo. Cada bloco AMIR
mantém uma tabela fria com seu melhor span HIR de origem; entradas de branches,
laços, braços de `match` e handlers de bloco `catch` usam o span correspondente.
A seleção EMI
prioriza candidatos cujo menor span mapeado não apareceu no trace. Essa relação
é apenas uma dica de ordenação: a sonda sentinela continua sendo a prova antes
da mutação. Regiões dentro de closures,
blocos assíncronos ou `defer` são excluídas. Laços com `return` ou propagação
de erro (`?`) no corpo também não são sondados: esses caminhos podem pular a
checagem da flag após o laço. Quando nenhum candidato é provado inacessível,
usa-se o ramo estático `if false`. Blocos sintéticos de junção e verificação
herdam o span envolvente, mas não são candidatos EMI. Novos tipos de região só
devem entrar na seleção após receberem um span de origem específico.

### 4.3. Fuzzing de Sessão Incremental Salsa

Ao contrário de compiladores batch clássicos, o Arandu é um compilador de queries reativas. O `AranduSmith` possui um executor específico para Salsa:
1. Carrega um pacote com múltiplos arquivos interconectados no `DatabaseImpl`.
2. Gera mutações semânticas e sintáticas concorrentes:
   - **Edição Tipo 1 (Interna):** Modifica o corpo de uma função privada sem alterar assinatura. *Invariante checada:* `exported_symbols` não pode mudar seu fingerprint BLAKE3; nenhum arquivo importador pode ser recalculado.
   - **Edição Tipo 2 (Interface):** Altera a visibilidade ou tipo de parâmetro. *Invariante checada:* Dependências diretas devem invalidar deterministicamente.
   - **Edição Tipo 3 (Transient Incomplete):** Insere erros de sintaxe propositais (remoção de `}`, parênteses abertos) e imediatamente envia queries de IDE (completions, hover). *Invariante checada:* O compilador nunca pode entrar em `panic!`, corromper IDs geracionais ou sofrer deadlock de lock de threads.

### 4.4. Oráculo Diferencial Multi-Backend

O oráculo executa a triangulação:
$$\Delta(P) = \{ \text{Out}_{\text{Cranelift}}(P), \text{Out}_{\text{C}}(P), \text{Out}_{\text{WASM}}(P) \}$$
Se $|\Delta(P)| > 1$ ou se qualquer backend abortar anormalmente (`SIGSEGV`, `ICE`, `panic`), a falha é confirmada.

### 4.5. Redutor Hierárquico de AST (*Hierarchical AST Shrinker*)

Testes gerados por computação aleatória costumam conter dezenas de operações redundantes que não participam do defeito. O redutor executa delta-debugging na árvore sintática:
1. **Passo 1 (Remoção de Itens Top-Level):** Tenta remover funções, structs e imports desnecessários. Se o bug persistir, a versão reduzida é mantida.
2. **Passo 2 (Poda de Instruções em Blocos):** Remove statements de funções preservando a falha.
3. **Passo 3 (Simplificação de Expressões):** Converte subexpressões binárias complexas em constantes ou identificadores simples.
4. **Passo 4 (Emissão de Fixture Canônica):** O resultado reduzido é formatado com `arandu_fmt` e salvo com metadados da semente e comandos de reprodução direta.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### Preservação dos Invariantes de Arquitetura (`AGENTS.md`):
- **Pureza das Queries Salsa:** O gerador e o oráculo operam fora das queries Salsa rastreadas. As queries avaliadas permanecem determinísticas, livres de efeitos colaterais e sem operações de I/O em hot paths.
- **IDs Monotônicos:** O gerador incremental valida exaustivamente se `FileId` e `SymbolId` nunca são reciclados de forma errônea após desregistros de módulos.
- **Zero Panics / Zero ICEs:** Nenhuma entrada sintetizada, por mais bizarra que seja sua estrutura semântica, tem permissão para disparar `panic!` ou `unwrap()` em crates de produção. Falhas de coerência no compilador devem emitir diagnósticos `Diagnostic::ice(...)`.

### Desvantagens e Trade-offs:
- **Consumo de Recursos em CI:** Testes diferenciais compilando nativamente via GCC, Cranelift e runtime WASM exigem tempo de CPU considerável.
  - *Mitigação:* O runner padrão no CI de PRs executará um lote curto calibrado (ex.: 500 iterações determinísticas com seeds fixas), enquanto o modo contínuo de estresse rodará em workflow noturno (*nightly*).
- **Falsos Positivos de Flutuação Numérica:** Operações com pontos flutuantes (`float` / `f64`) podem apresentar discrepâncias mínimas de arredondamento entre o backend Cranelift (x86_64 FMA) e GCC/WASM.
  - *Mitigação:* O gerador usará comparações com margem epsilon para floats ou priorizará tipos discretos e exatos nas asserções de saída.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### Por que não apenas estender o `cargo-fuzz` / `libFuzzer` existente?
O `cargo-fuzz` com mutação pura de bytes é insubstituível para encontrar falhas de alocação de memória no lexer SIMD e no parser UTF-8. No entanto, sua taxa de geração de programas que atingem as fases posteriores (AMIR, otimizações, geração de código) converge para zero em linguagens de tipagem forte.

### Por que não adotar o `Csmith` diretamente via transpilação?
O Csmith foi projetado para C99. Ele não entende conceitos nativos do Arandu, tais como:
- Segurança de referências e fatias emprestadas (`borrowed views`).
- Modelo de memória baseado em `GenRef` e regiões alocadoras.
- Expressões de correspondência de padrões exaustivas (*pattern matching* com ADTs).
- Módulos e queries incrementais.

Desenvolver o `AranduSmith` internamente permite que a ferramenta evolua lado a lado com a especificação da linguagem.

---

## 7. Arte Prévia (Prior Art)

1. **Yang, Chen, Eide, Regehr (PLDI 2011):** *"Finding and Understanding Bugs in C Compilers"*. Pioneiro em demonstrar que geradores livres de comportamento indefinido por construção encontram centenas de bugs em compiladores consolidados (GCC e LLVM).
2. **Le, Afshari, Su (PLDI 2014):** *"Compiler Validation via Equivalence Modulo Inputs"*. Demonstrou a eficácia superior de mutar programas existentes sem alterar o resultado em entradas específicas.
3. **Livinskii, Babokin, Regehr (OOPSLA 2020):** *"Random Testing for C and C++ Compilers with YarpGen"*. Demonstrou que geração direcionada a paralelismo, escalares e transformações de laços revela falhas críticas nos backends modernos.
4. **Dewey, Roesch, Hardekopf (OOPSLA 2015):** *"Fuzzing the Rust Typechecker"*. Pioneiro na formalização de geração dirigida por tipos para sistemas de tipos lineares/afins.
5. **Rust `compiletest` & `miri`:** O ecossistema Rust utiliza testes diferenciais extensos para garantir que rustc com `-Zmir-opt-level=3` produza os mesmos efeitos semânticos que execuções interpretadas em Miri.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Paridade com Recursos Assíncronos:** Como o gerador de programas deve estruturar laços assíncronos (`async`/`await`) garantindo ausência de deadlocks artificiais nas tarefas do runtime?
2. **Heurística de Redução de AST:** Qual estratégia de ordenação no AST Shrinker oferece o melhor equilíbrio entre velocidade de convergência e tamanho mínimo da fixture?

---

## 9. Possibilidades Futuras (Future Possibilities)

- **Fuzzing de Otimização Formal via SMT (Alive2-style):** Tradução dos blocos básicos do AMIR antes e depois das otimizações para fórmulas SMT (Z3) para comprovar formalmente a equivalência semântica bit a bit.
- **Fuzzing Direcionado a Bindings C-ABI:** Geração automática de cabeçalhos C e chamadas cruzadas com `arandu_web` e `arandu_runtime` para validar que chamadas de funções externas preservam layouts de registradores e convenções de chamada (SysV vs Windows x64).

---

## 10. Plano de Implementação Incremental

A implementação será dividida em etapas que produzem alvos úteis antes de
introduzir execução diferencial cara ou redução automática. Os geradores e
oráculos ficam em `arandu_fuzz_support`; `xtask` orquestra corpus e processos,
sem executar I/O dentro de queries Salsa.

### Etapa 1 — Síntese dirigida por tipos

- Implementar uma gramática limitada por tipo esperado e orçamento de
  profundidade, usando somente variáveis inicializadas no escopo disponível.
- Começar com `int`, `uint`, `bool`, `float` e `char`; o gerador `char` escolhe
  escalares Unicode válidos em larguras UTF-8 diferentes (ASCII, latino, grego,
  CJK e emoji), evitando controles e bidirecionais, e usa comparações e valores
  esperados sem alocar strings por execução. Caracteres de controle e
  pontuação usam escapes; o lexer agora decodifica o escalar antes de interná-lo
  no AMIR, permitindo ao oráculo verificar semântica de escapes, não só a
  concordância dos backends. Substitutos Unicode são rejeitados. Divisores são
  literais não nulos
  e a faixa e profundidade dos operandos mantêm a aritmética longe dos limites
  inteiros. A gramática `uint` não subtrai abaixo de zero e limita valores a
  menos de 2.000; a gramática `float` usa apenas valores finitos exatamente
  representáveis em binário (soma, subtração, multiplicação e divisão por 2),
  permitindo comparar o resultado com um oráculo independente sem tolerância
  epsilon ou diferenças de arredondamento.
- Gerar condições com negação e combinadores booleanos, além de comparações
  relacionais dos tipos numéricos e de `char`. O teste diferencial seleciona
  seeds que cobrem cada forma, inclusive ordenação de caracteres, e executa os
  casos nos backends e níveis de otimização; uma regressão adicional percorre os
  seis operadores relacionais em `int`, `uint` e `float`, incluindo operandos
  inteiros negativos.
- Compor expressões escalares em um array de tamanho fixo, um agregado local e
  uma enum com payload; indexar somente posições dentro do limite e comparar
  os valores após construção, match e extração do payload. O campo `char` do
  agregado e `Choice.Letter(char)` também são lidos de volta; o variant passa
  por `Vec<Choice>` e por um match para exercitar layout e acesso nos backends.
- Expor seed determinística usando todos os bytes fornecidos pelo libFuzzer e
  ligar o target ao `cargo-fuzz` e ao corpus isolado de `xtask`. Seeds
  versionadas de exatamente 8 bytes preservam o mapeamento little-endian atual.
- Manter separados os testes de alcance estrutural e os de execução: a bateria
  de seeds valida parse/typecheck/lowering/AMIR; a triangulação de backends roda
  em testes diferenciais próprios, com limites de tempo por processo.
- Reservar códigos de saída para falhas dos guardrails que não possam coincidir
  com nenhum resultado válido do oráculo. O limite é derivado da profundidade,
  faixa dos operandos e limite do laço, com assertiva constante para que uma
  futura expansão do gerador exija recalibrar essa faixa.
- Critério de saída: muitas seeds reproduzíveis passam por parse, resolução,
  typecheck, lowering e validação de invariantes AMIR, sem ICE ou panic.

### Etapa 2 — Oráculo de otimização e triangulação de backends

- Executar o mesmo programa e entrada em O0/O1/O2; comparar status, stdout,
  stderr observável e resultado, com timeout e captura de crash por processo.
  O preflight de promoção usa grupo de processos no Unix e Job Object no Windows
  para encerrar descendentes quando o prazo global de 30 segundos expira.
- O pipeline atual documenta O1 e O2 como compartilhando o mesmo núcleo de
  simplificação. O Smith deve registrar essa cobertura comum sem apresentá-la
  como duas implementações independentes de otimização.
- Executar Cranelift, C e Wasm em cada nível O0/O1/O2. C é compilado a partir
  do AMIR já otimizado daquele nível; Wasm usa o executor Node configurável por
  `ARANDU_NODE`. Ausência de compilador C ou runner Wasm deve ser reportada,
  nunca contada como paridade aprovada.
- A síntese atual imprime `result-even` ou `result-odd` conforme o resultado e
  envia o mesmo marcador por `io.eprint`; o oráculo compara stdout e stderr
  byte a byte e valida o retorno contra a fórmula independente do gerador, em
  todos os backends e níveis. O JIT usa callbacks de captura locais à thread;
  C/Wasm escrevem o retorno em um arquivo lateral para deixar os dois canais
  disponíveis para comparação. A saída sintetizada usa literais para não
  alocar uma string a cada execução de fuzzing.
- Cada execução sintetizada também recebe dois argumentos fixos e consulta
  `std.env.argsLen()`, incluindo `argv[0]`; o oráculo verifica a contagem em
  Cranelift/C/Wasm. O primeiro argumento é vazio e o segundo contém texto; o
  programa lê `env.arg(1)` e `env.arg(2)` e compara seus conteúdos nos três
  backends. Assim a mesma regressão cobre strings de comprimento zero e a ABI
  `str` (ponteiro e comprimento), em vez de comparar apenas a palavra do
  ponteiro. No JIT e no Wasm, o argumento vazio usa o par `(NULL, 0)`. Esse caso
  encontrou uma chamada incondicional a `memcmp` no Cranelift; o lowering agora
  desvia por comprimento e só chama a libc quando o tamanho é positivo.
  Um caso diferencial separado interpola esse argumento vazio junto de texto
  não vazio e garante que o Cranelift também não chame `memcpy` com comprimento
  zero e origem nula.
- Normalizar apenas diferenças explicitamente contratuais, mantendo saída de
  usuário e código de saída como parte do oráculo.
- Critério de saída: qualquer desvio é reproduzível por seed, backend, nível
  de otimização e argumentos registrados.

### Etapa 3 — Fuzzing incremental e early-cutoff

- Expandir as sequências existentes de edição quente/fria para cobrir alteração
  de corpo privado, alteração de interface e estados sintaticamente incompletos.
- Usar métricas/eventos de query existentes para provar que corpo privado não
  recalcula importadores quando exports permanecem iguais; comparar resultados
  e diagnósticos da DB aquecida com uma DB fria após cada edição.
- Para ciclos de import, comparar também duas DBs frias construídas em ordens
  opostas de registro dos arquivos, normalizando somente `FileId`; diagnósticos
  e completions não podem depender da ordem de alocação dos IDs. Há regressões
  para ciclos de dois e três módulos; ambas têm seeds no manifesto e no corpus
  binário de `fuzz_module_graph`.
- Para protocolo LSP, testar por stdio e respeitar as filas limitadas, revisões
  e publicação de resultados stale já definidos pela arquitetura.
- Critério de saída: equivalência quente/fria e contadores de cutoff validados
  para sequências determinísticas e seeds variáveis.

### Etapa 4 — EMI com corpus controlado

- Implementação atual: `emi-corpus` seleciona por seed quatro regressões
  pequenas, casos de controle de fluxo e das operações de `std.alloc.vec`,
  `std.alloc.bitset`, `std.alloc.smallvec`, `std.alloc.arena` e
  `std.alloc.string`, e nove exemplos de
  `examples/minimal/`: Vec por funções e métodos, string, arena genérica,
  struct/enum, propagação de `Result`, interpolação e match de `Result`.
- O caso `smallvec-spill` cruza as transições inline→heap de 4→8 e 8→16
  elementos, verifica capacidade e leituras, consome o último valor com `pop` e
  destrói o armazenamento. O resultado conhecido é comparado em Cranelift
  O0/O1/O2, C e Wasm; as guardas de overflow mantêm a divisão sobre um valor
  local `uint` tipado para evitar a perda de tipo da constante castada no AMIR.
- Localiza por CST/AST até quatro regiões de controle em `main`, incluindo
  ramos `if`, braços de `match`, handlers de bloco `catch` e corpos de
  `while`/`for`, excluindo closures, blocos assíncronos e `defer`. Para
  `if`/`match`/`catch`, insere um retorno sentinela; para laços sem saída
  antecipada, marca a entrada no corpo e verifica a flag
  depois do loop para não criar retornos inalcançáveis no CFG. Laços com
  `return` ou `?` usam o fallback estático. O JIT instrumentado em O0 registra
  os blocos AMIR executados; cada candidato só é considerado não executado se
  também preservar o trace de referência como subsequência ordenada. Falhas de
  sonda ou traces incompatíveis são inconclusivos. Executa Cranelift em O0/O1/O2
  e só injeta a expressão pura numa região comprovadamente não executada. Se
  não encontrar uma região não executada, usa o fallback `if false`. Compara a
  fonte e a mutação em Cranelift O0/O1/O2, C e Wasm.
- Testes unitários percorrem todo o corpus. Seeds explícitas exercitam exemplos
  selecionados pelo guardrail `xtask`; todas as entradas também ficam no corpus
  binário do libFuzzer compilado pelo workspace `arandu_fuzz`. Os exemplos com
  retorno conhecido validam também o valor, stdout e stderr, evitando que uma
  falha comum aos três backends passe apenas pela comparação diferencial.
- Próximo incremento: ampliar a sondagem para outros construtos de controle e
  expandir a amostra de APIs da stdlib para além dos casos já cobertos por
  Vec, BitSet, SmallVec, Option, Result e HashMap.

### Etapa 5 — Shrinker hierárquico e regressões

- Reduzir primeiro itens e statements inteiros; depois simplificar expressões
  usando CST/AST canônico e preservar a propriedade de falha como oráculo.
- Toda tentativa de redução tem timeout/limite de tentativas e mantém a seed e
  o comando de reprodução; a versão mínima é gravada somente após confirmação
  reproduzível em uma segunda execução.
- Capturar panics Rust das fases de frontend, otimizador e backends como falhas
  com classe e escopo de otimização, para que também entrem no shrinker.
- Gerar um caso candidato em diretório temporário; promover para fixture de
  regressão é uma ação explícita, revisada junto do teste correspondente.
- Critério de saída: casos reduzidos continuam reproduzindo o mesmo tipo de
  falha e carregam metadados suficientes para repetição local e em CI.

### Estado atual da implementação

Em `crates/arandu_fuzz_support/src/smith.rs`, o target `synthesized` gera
programas determinísticos com expressões escalares `int`/`uint`/`bool`/`float`,
chamadas com parâmetros `int`/`bool` e uma função genérica `identity<T>` usada
com os quatro tipos escalares,
leitura através de empréstimos compartilhados e exclusivos, e retorno por
caminhos distintos, array de tamanho fixo, struct local com campos `int` e
`bool`, enum com variantes de payload `int` e `bool`, controle de fluxo e um
laço limitado, verificando diagnósticos, lowering e invariantes AMIR. As duas
chamadas ao helper usam condições opostas, forçando ambos os retornos em cada
programa. A seleção `base % 3`
percorre as três variantes do enum; o oráculo C/Wasm valida 12 seeds e confirma
cobertura das três escolhas. A geração é limitada por profundidade, mantém os
divisores não nulos e reduz os índices dinâmicos módulo o tamanho do array. Os
seeds alternam entre `while` decrescente e `for` C-style crescente, ambos com
limite derivado de `base % 5`; o teste diferencial exige executar os dois
formatos e valida a soma triangular pelo oráculo independente.
Cada nó de expressão sintetizado carrega também seu valor constante esperado;
um oráculo independente calcula o retorno final a partir dos valores da tupla,
do ramo de enum e do vetor. Assim, o target verifica cada backend contra um
resultado semântico conhecido além de comparar Cranelift/C/Wasm entre si,
detectando também falhas de código comum aos três.
O caminho `float` executa uma expressão limitada pela mesma profundidade e
confere seu valor em todos os backends/níveis de otimização. Sua aritmética usa
apenas operações binárias exatas com operandos pequenos, para que o oráculo
independente não dependa de tolerâncias numéricas.
O programa também instancia `Vec<int>`, faz nove inserções com tratamento de
falha em um laço limitado (forçando o crescimento e a realocação), verifica o
comprimento e valores de elementos contra expressões escalares independentes.
O caso reserva capacidade antes do crescimento, verifica o tamanho da view
`[]int` emprestada, exige `None`/`false` nos acessos fora dos limites, atualiza
um índice com `put`, valida o valor removido por `pop`, confere `pop` no vetor
vazio depois de `clear`, lê resultados como `Option<int>` e destrói o vetor em
todas as saídas. O harness registra os módulos `vec`, `slice`, `mem` e
`intrinsics` na
fronteira de setup; no Wasm, os imports da Vec delegam ao `cabi_realloc` do
próprio módulo, usando a heap linear e o ciclo de liberação do backend. O vetor
fica limitado a nove elementos.
O corpus e os testes cobrem seeds determinísticas; isso ainda não equivale a
milhões de casos por hora nem cobre todos os tipos e recursos da linguagem.
Um em cada 64 seeds do sintetizador inclui um `HashMap` com chaves tipadas e
valores variáveis: o caso força colisões, substituição, backward-shift após
remoção de cabeça e meio, crescimento de capacidade, limpeza e reutilização.
Na mesma frequência, o programa inclui um `BitSet` que cruza as fronteiras de
palavra 63/64 e 127/128, valida inserção idempotente, membership, contagem,
remoção sem afetar bits vizinhos, limpeza e reutilização. A posição adicional
varia com a seed em uma faixa limitada. Uma regressão compara duas seeds
distintas em Cranelift, C e Wasm nos três níveis de otimização, além de exigir
que o trecho gerado para as coleções realmente varie entre elas.

O corpus descobriu defeitos reais, agora corrigidos e cobertos por fixtures em
`tests/regressions/`: o DCE descartava definições alternativas de um temporário
de junção e a estruturação Wasm posicionava uma junção depois do fim lógico do
rótulo quando um braço irmão terminava em `Unreachable`. O DCE preserva agora
todas as definições possíveis de um temporário usado, e o stackifier ordena
braços de trap antes da junção irmã.

Um caso EMI com `Result<int, Err>` expôs duas falhas adicionais. O type checker
aceitava `?` dentro de uma função cujo retorno não podia carregar o erro; essa
forma agora emite T016, e os exemplos que precisam devolver `int` tratam o
`Result` explicitamente. Com uma função de retorno compatível, o mesmo caso
expôs uma otimização incorreta em O2: o GVN fundia leituras do payload `Ok`
e `Err` porque a chave considerava apenas base e índice do campo. A chave
agora inclui o tipo do resultado para operações unárias, binárias e acessos a
campos. Há uma regressão de AMIR para as duas leituras com tipos diferentes, e
o corpus EMI percorre Cranelift, C e Wasm em O0/O1/O2 com o caso corrigido.

A nova fixture `arena-lifecycle` cobre alinhamento, exaustão, reset, alocação
tipada, checkpoints e rewind de `Arena`/`ScratchArena`, validando também o
oráculo de retorno em Cranelift, C e Wasm. A fixture encontrou que o limite
`MAX_ARENA_CAPACITY` era inferido como `int` embora participasse de comparações
com `uint`; a condição chegava ao lowering com tipo `Error` e emitia `ICEGEN002`.
O limite agora declara seu tipo por cast. A coleta de assinaturas publica tipos
diretamente conhecidos por casts em constantes sem anotação, e o lowering
preserva esse tipo ao materializar o literal em AMIR. O primeiro resultado do
teste também revelou que o tamanho de `int` não deve ser fixado em 8 bytes; a
fixture compara a alocação com `mem.sizeOf<int>()`, respeitando o alvo.

A fixture `string-lifecycle` cobre o bridge de append, crescimento do buffer,
views `str`/bytes emprestadas, buscas por prefixo/subtexto/sufixo, comprimentos
UTF-8 para escalares de 1, 2 e 4 bytes, truncamento aceito/rejeitado em limites
de code point, falha de reserva por overflow, limpeza e destruição.
O runner Wasm agora implementa `ar_string_push_str` sobre a memória linear e o
`cabi_realloc`, validando faixas antes de copiar e preservando bytes sobrepostos.
Ao importar `std.alloc.string`, o corpus também encontrou exports Wasm
duplicados entre função livre e método associado. Exports de métodos agora
incluem o nome do tipo proprietário. A fixture encontrou ainda que a
dereferência Wasm de `ref str` tratava o ponteiro fino para o descritor como se
ele já carregasse `(data, len)` na pilha. O backend agora carrega os dois campos
do descritor; testes diferenciais cobrem comprimento, prefixo, busca por
subtexto, sufixo e acesso aos bytes em Cranelift, C e Wasm.

A fixture `num-boundaries` adiciona cobertura para operações checked e
saturating da `std.core.num`, incluindo os dois extremos assinados calculados
pela largura real do alvo. Ela encontrou que SCCP dobrava `-1 as uint >> 1`
como deslocamento aritmético, por não conhecer o signedness do valor
constant-propagated. Isso transformava `intMax()` em `-1` em O1 e fazia
`checkedMul(intMin(), -1)` aceitar overflow. SCCP agora deixa deslocamentos à
direita com bit 127 ligado sem dobrar enquanto a representação de constantes
não carregar o tipo; uma regressão de pass cobre a decisão e o caso EMI exige
paridade Cranelift/C/Wasm em O0/O1/O2.

A regressão estrutural do sintetizador percorre 24 seeds determinísticas no
mesmo `AnalysisHost` e `FileId`, atualizando o texto e validando diagnósticos,
lowering e invariantes AMIR em cada revisão. A execução de backends permanece
nos testes diferenciais próprios; assim, a validação de alcançabilidade não
precisa compilar e executar 24 programas em O0/O1/O2.

A nova geração de `float` também revelou uma classe não coberta: comparações
Wasm escolhiam o opcode pelo tipo de retorno (`bool`) em vez do tipo dos
operandos. O backend emitia `i32.ne` sobre valores `f64`, fazendo o Node
rejeitar o módulo. O lowering agora escolhe tipo e opcode pelo tipo dos
operandos em operações de comparação; a execução sintetizada compara `float`
com o oráculo em Cranelift, C e Wasm.

Uma regressão complementar com operandos constantes revelou que o Cranelift
usava o tipo de retorno `bool` como fallback quando nenhum dos lados era um
temporário. Isso levava literais `float` ao caminho de conversão para inteiro e
disparava um panic no emissor x64 do Cranelift. A inferência agora consulta o
tipo do literal no pool AMIR; o teste diferencial cobre os seis operadores de
comparação `float` (`==`, `!=`, `<`, `>`, `<=`, `>=`) em todos os backends e
níveis de otimização.

O corpus também revelou que a monomorfização colidia `vec.len<int>` com
`slice.len<int>` por usar apenas o nome curto da função, fazendo o backend C
chamar uma função `Vec*` com uma slice. A mangling de instâncias genéricas
inclui agora o nome qualificado do símbolo-fonte, com regressão multi-backend
em `tests/regressions/generic-module-name-collision.aru`.

Os alvos `synthesized-c` e `synthesized-wasm` comparam o valor de retorno com
Cranelift; `synthesized-all` executa Cranelift, C e Wasm para a mesma seed e
compara os três resultados em uma única invocação. O `xtask smith` executa cada
seed em um worker isolado; um JIT que não termina encerra somente o worker ao
vencer o orçamento da seed, sem prender a campanha. C usa `CC` (ou `cc`) e Wasm
usa Node (`ARANDU_NODE` para selecionar outro executável). A execução JIT
também compara O0/O1/O2. O1/O2 compartilham hoje o núcleo de simplificação,
portanto essa verificação compara níveis expostos pela API, não pipelines
independentes. O timeout de execução isolada do corpus protege o runner de
regressões que travem. O runner de regressões e o workflow de fuzz usam o mesmo
orçamento por classe de alvo: 2 segundos para frontend leve, 5 para análise
incremental, 10 para sessão LSP, 15 para síntese JIT e 120 para execuções
diferenciais C/Wasm e EMI. O orçamento diferencial comporta a compilação e
execução de todos os níveis/backends no hardware de desenvolvimento. Isso evita classificar a própria compilação dos
backends como timeout sem deixar uma execução sem limite. O suporte Smith
também limita a cinco segundos cada compilação C, execução nativa e execução
Node/Wasm, encerrando processos que travem. O runner drena stdout/stderr em
paralelo e limita a captura de cada canal a 1 MiB para que pipes cheios não
simulem timeout nem causem crescimento de memória sem limite; a saída de
sucesso precisa conter exatamente um inteiro terminado por newline, evitando
que logs extras escondam um resultado inválido.
Em Unix, comandos normais rodam em grupos de processos isolados; durante o
preflight, o verificador e seus subprocessos compartilham o grupo exclusivo do
preflight para que o limite externo encerre toda a árvore. O runner também limpa
descendentes que mantenham pipes abertos após a saída do processo principal.
No Windows, cada compilação e execução fica em um Job Object com
`KILL_ON_JOB_CLOSE`; ao encerrar o processo pai, o runner termina os descendentes
antes de drenar os pipes, evitando processos órfãos e captura bloqueada.

O alvo `incremental-cutoff` altera o corpo de um helper privado, confirma que
a superfície exportada não mudou e verifica pelo log Salsa que `type_check`
não foi executado novamente. O mesmo alvo também altera a assinatura pública
(trocando o tipo de retorno, a aridade dos parâmetros ou o nome exportado) e
exige um diagnóstico no consumidor e nova execução de seu `type_check`. Seeds
individuais preservam a reprodução de cada variante pública. A sequência
incremental também exercita edições públicas e corpos incompletos na dependência
e no consumidor, sempre comparando diagnósticos e completions da análise quente
com uma análise fria e rejeitando ICEs. Ao fim de cada sequência, compara também
os artefatos de `lower_amir` das duas DBs; isso cobre divergências semânticas e
de IR que não alterem diagnósticos nem completions. A comparação verifica a
estrutura completa das funções e os demais payloads do AMIR diretamente, sem
depender apenas da igualdade otimizada de `HashEq`; também compara o hash
semântico do typeck. A auditoria desse caminho ampliou o hash do typeck para
incluir referências resolvidas e metadados usados por lowering/codegen
(campos, variantes, genéricos, destrutores, efeitos, `unsafe` e `repr(C)`),
mantendo fora do hash de assinatura as referências de corpo e caches de
instanciação. O hash do AMIR também cobre agora operandos e metadados de cada
rvalue, argumentos de terminadores, tabela de locals/temps/parâmetros, arestas
de CFG, assinaturas de funções externas e vínculos de debug; isso evita
early-cutoff entre IRs com a mesma forma superficial e payloads diferentes.
São duas lowerings finais por arquivo — uma quente e uma
fria — em vez de repetir esse custo a cada edição. O target
recebe também operações locais de inserção, remoção e substituição, com posições
escolhidas apenas em limites UTF-8 e fragmentos
determinísticos derivados dos bytes da seed; cada operação volta a comparar o
estado quente com uma DB fria. Essa comparação encontrou e levou à correção de
três erros no splice de tokens do reparse: tokens válidos eram descartados
quando um item se deslocava para a esquerda, o `;` sintético do item podia ser
duplicado no limite, e o offset de EOF ignorava espaços finais. O parser agora
tem uma regressão com edições consecutivas que remove comentário Unicode no
início do arquivo e depois altera o corpo, comparando cada fluxo de tokens
incremental com um parse frio. O alvo de grafo de módulos também compara
completions quentes e frias após trocar, remover e recriar arquivos, incluindo
o prefixo de acesso `dep.` e o fim do documento. O alvo também registra os
símbolos antes de `unregister` e afirma que o `FileId` da nova identidade é
maior e que nenhum `SymbolId` antigo reaparece. O `shrinker`
em `shrinker.rs` já tenta remover
itens, statements e simplificar expressões através do CST, com limite de
tentativas e predicado fornecido pelo chamador. Na redução por delta debugging,
testa também os complementos dos chunks: conserva um grupo e remove as regiões
ao redor, o que permite eliminar trechos irrelevantes separados quando apagar
um único bloco contíguo não preserva a falha. Uma regressão cobre esse caso.
O redutor repete os níveis de item, statement e expressão até não encontrar
mais redução, pois simplificar um bloco pode tornar removível um item que antes
era necessário. Em expressões, tenta preservar subexpressões descendentes antes
de recorrer a literais; uma regressão garante que `(10 + 7) * 99` pode reduzir
para a parte `10 + 7` quando ela é necessária para reproduzir a falha.
Substituições só são aceitas quando diminuem o tamanho da fonte, garantindo
progresso e evitando alternância entre literais. Candidatos de expressão são
visitados sob demanda, em vez de materializar todas as substituições do AST;
assim, uma aceitação precoce ou o esgotamento do orçamento não retém uma lista
de candidatos de substituição proporcional a todas as expressões do programa.
O redutor identifica `while`/`for` no fluxo de tokens da CST. Só reduz o corpo
parcialmente quando prova o padrão simples `while counter > 0` com um decremento
incondicional `counter = counter - 1`; o cabeçalho e o decremento ficam
protegidos, enquanto statements sem uso do contador podem ser reduzidos. Laços
com condição, fluxo de controle ou progresso não reconhecidos ficam protegidos
por inteiro. Isso evita que um falso positivo no reconhecimento de progresso
transforme um candidato em loop infinito executado pelo JIT no mesmo processo.
O isolamento do JIT em subprocesso
continua necessário para impor timeout por tentativa e permanece trabalho
futuro.
Chunks e seus complementos também são visitados sob demanda: na granularidade
mais fina, isso evita materializar O(n²) ranges antes de consultar o orçamento
do oráculo.
Os mismatches de otimização e
paridade Cranelift/C/Wasm agora invocam a redução com orçamento limitado,
preservam a classe e o nível de otimização da falha e reexecutam o candidato
final antes de incluí-lo no panic do fuzz target. Isso impede que uma falha em
O2 seja considerada reduzida por outra discrepância do mesmo backend em O0.
Panics Rust durante frontend/lowering, otimização ou execução de backend são
convertidos em falhas com classe própria e escopo O0/O1/O2 antes de chegar ao
redutor; uma regressão confirma a captura e a elegibilidade para shrink. Falhas
de processos C/Node também são observadas pelo status de saída e timeout. Um
segfault no processo do próprio fuzz target ainda encerra esse worker e não
passa pelo redutor em processo; a fixture bruta continua sendo responsabilidade
do libFuzzer, e isolamento do JIT em subprocesso permanece trabalho futuro.
Depois de uma falha reduzível, o runner grava `candidate.aru`, a mutação EMI,
`reproducer.seed`, a entrada binária e `metadata.txt` com target, classe,
escopo, orçamento e comando de replay. Localmente o
diretório é temporário; no workflow semanal, o caminho fica sob
`arandu_fuzz/artifacts/<target>` e é enviado como artefato quando o target
falha. A promoção é explícita pelo comando
`cargo run --locked -p xtask -- promote-fuzz-artifact <artifact-dir> <nome>`:
ele exige `shrink_confirmed=true`, reexecuta uma cópia imutável da fonte e sua
mutação EMI em Cranelift/C/Wasm num subprocesso limitado a 30 segundos e,
somente após o preflight passar, cria a fixture, o seed de
origem e a proveniência em `tests/regressions/`, além de registrá-la no corpus
EMI compilado. No Unix o preflight usa um grupo de processos próprio; no Windows
usa Job Object com `KILL_ON_JOB_CLOSE`, para terminar os descendentes junto com
o verificador. A leitura de fontes é limitada a 1 MiB e a de metadados a 64
KiB, com leitura interrompida ao ultrapassar o limite. As mudanças ficam no
working tree para revisão antes do commit.

Os alvos `synthesized`, `synthesized-c`, `synthesized-wasm` e
`synthesized-all`, além da seed reproduzível, estão registrados em `arandu_fuzz`
e `tests/fuzz-regressions/manifest.tsv`. O redutor CST remove grupos de itens
top-level e statements irmãos em cada bloco, inclusive blocos aninhados, por
delta debugging; depois simplifica expressões com orçamento limitado. Falhas
de execução reduzíveis revalidam o candidato antes de exibi-lo,
e geram um pacote de reprodução fora de `tests/`; o fuzzing incremental exercita
mutações privadas, públicas e incompletas na camada de análise Salsa. Quando
uma sequência de edição falha na comparação com análise fria, um redutor por
delta debugging remove grupos de operações e simplifica bytes sob orçamento
fixo de 48 chamadas ao oráculo; a falha reporta a sequência hexadecimal
reduzida apenas quando ela continua reproduzível. O candidato final passa por
uma segunda execução fora do orçamento do shrinker; se não repetir a mesma classe
de falha, o relatório volta à sequência original e marca a redução como não
confirmada. O mesmo contrato vale para sessões LSP, cujo shrinker tem orçamento
de seis chamadas para limitar o custo de replays de servidor. O comando
`xtask promote-fuzz-sequence` aceita sequências reduzidas de Salsa/LSP, repete
o target em subprocesso com timeout, recusa replay falho ou conteúdo duplicado
e registra a cópia legível em `tests/fuzz-regressions/`, a entrada binária no
corpus do libFuzzer específico (`fuzz_incremental`, `fuzz_incremental_cutoff`
ou `fuzz_lsp_session`) e o manifesto. Se a instalação falhar, remove as cópias
parciais sem apagar arquivos preexistentes. A suíte
stdio agora também executa três sessões independentes de 24 edições cada, com
seeds determinísticas; cada sessão cobre conteúdo válido, erro semântico, texto
incompleto, Unicode e loop, intercalados com completion e cancelamento. As
respostas são drenadas e validadas antes de conferir que a revisão final limpa
os diagnósticos e nenhuma publicação stale aparece depois. Também foi
adicionado o target `fuzz_lsp_session`: cada entrada inicia o runtime LSP em
uma conexão em memória instrumentada pelo libFuzzer, cria um pacote temporário
com manifesto e três módulos e abre os três documentos com overlays. As edições
variam o chamador e funções exportadas, exercitam import inválido e incompleto,
alteram o nome do pacote no manifesto via `workspace/didChangeWatchedFiles` e
podem fechar o ciclo `main → math → data → main`; versões independentes,
close/reopen, completions/cancelamentos e diagnósticos finais vazios por URI,
ICEs e ausência de publicação stale são verificados. A seed reproduzível está em
`arandu_fuzz/corpus/fuzz_lsp_session/` e também em
`tests/fuzz-regressions/lsp-session.seed`, executada pelo guardrail do `xtask`;
entrada, cada sessão processa no máximo 24 edições, as edições pendentes e o
tempo de espera por resposta são limitados por um orçamento total de 30 segundos
e um teto de inatividade de oito segundos por mensagem. O
target reduz sequências de edição com até seis consultas adicionais ao oráculo,
preservando a classe do erro original. Cada replay encerra e aguarda o worker LSP
antes da próxima tentativa; se o encerramento falhar, a redução para imediatamente
para não acumular threads de servidor. O
resultado do worker distingue encerramento limpo, erro retornado e panic; erros e
panics do servidor permanecem falhas reproduzíveis e podem ser reduzidos. Apenas
um timeout real de shutdown interrompe a redução, pois a thread pode continuar
viva no processo. O
teste unitário do `xtask` exige que as duas cópias versionadas da seed sejam
idênticas byte a byte, e o teste da sessão garante que a entrada toda cabe no
orçamento de edições. O
gerador mantém versões crescentes por URI também após close/reopen, evitando que
um diagnostic da geração anterior seja confundido com a revisão final. O teste
de sessão detectou e corrigiu essa colisão no próprio oráculo. A redução da falha
de timeout chegou à sequência `e2`, que fecha e reabre um documento enquanto há
trabalho de IDE em voo. Ela revelou que `didClose` podia substituir/remover a
fonte Salsa sem cancelar primeiro os workers; o handler agora cancela as
requisições interativas antes da mutação, seguindo a barreira usada pelas demais
notificações que alteram o banco. A sequência mínima e a seed LSP completa são
regressões executadas no crate de fuzz support. As completions
finais só são solicitadas depois que os dois documentos aplicam e salvam suas
revisões finais; os resultados precisam ser respostas válidas da revisão atual,
e a conclusão após `math.` deve incluir o membro exportado `answer`.
teste E2E stdio cobre separadamente o framing e a execução do binário real. O
primeiro passo de EMI tenta sondar até quatro regiões em `main`: ramos `if`,
braços `match` e handlers de bloco `catch` usam retorno sentinela com Cranelift
em O0/O1/O2; corpos de
`while`/`for` sem `return` ou `?` marcam uma flag na entrada e a verificam
depois do laço, evitando introduzir retornos inalcançáveis no CFG e falsos
resultados quando uma saída antecipada pula a verificação. Regiões dentro de
closures, blocos assíncronos e `defer` também são excluídas; `if false` continua
como fallback. A mutação insere uma expressão inteira pura e limitada. O
alvo `emi-corpus`
seleciona por seed quatro regressões do compilador, um programa com laço
finito, programas de `Vec<int>` e `std.core.option` em `tests/emi-corpus/` e
nove exemplos reais de `examples/minimal/` para Vec, `std.core.str`,
`std.alloc.gen_arena`, structs, enums, `Result` e interpolação; compara o
resultado da fonte original e da mutante em Cranelift, C e Wasm em O0/O1/O2.
Os casos da stdlib cobrem reserva, inserção, atualização, remoção, limpeza e
`pop` em vetor vazio, construção, correspondência e métodos associados de
`Option.Some`/`None` e `Result.Ok`/`Err`, operações puras de string, além de
inserção, reciclagem
de slot com geração, rejeição de handle obsoleto e remoção na arena genérica.
Os testes unitários executam cada entrada desse
corpus nos três backends, e a seed reproduzível está no manifesto de fuzzing.
Falhas de oráculo são reduzidas com orçamento limitado e só são exibidas como
confirmadas após nova execução. A mutação e a seleção são determinísticas; o
trace de blocos AMIR prioriza regiões por spans de origem, mantendo a sonda como
verificação. A amostra de `stdlib/` e `examples/` ainda não é ampla.

O workflow semanal `.github/workflows/fuzz.yml` executa agora todos os 16
targets registrados no pacote fuzz, incluindo cutoff incremental, sessão LSP,
sintetizador e EMI. Targets com processos C/Wasm recebem 300 segundos; os de
análise incremental/LSP e síntese JIT recebem 900; os targets leves mantêm
1.800. Cada job tem limite de 45 minutos para incluir instalação e compilação,
além dos limites de entrada, timeout de caso e RSS do libFuzzer.
Os targets mantêm o limite de RSS de 1 GiB por processo. Cada target também parte
de seeds binárias versionadas em `arandu_fuzz/corpus/`;
apenas os casos novos descobertos durante a campanha ficam ignorados pelo Git.
Uma falha do target mantém o job vermelho após o upload do artefato de
reprodução, para que o achado não fique oculto como sucesso da campanha.

O sintetizador também instancia `Vec<uint>`, `Vec<core_result.Result<int, bool>>`,
`Vec<bool>`, `Vec<Choice>`, `Vec<char>` e `Vec<float>`. Os
programas cobrem reserva, inserção, leitura via `get`, comprimento da view via
`asSlice`, `pop` e inspeção de payload por `Option`. O novo `Vec<uint>` passa
por `make_vec → relay → transfer`, reserva, inserção, leitura da view, `get` e
`pop`, comparando cada valor com o resultado uint independente do gerador.
Assim, o retorno consumido e repassado de uma coleção não fica coberto só pelas
especializações `Vec<int>` e `Vec<uint>`. O `Vec<Result<int, bool>>` usa a
mesma cadeia e verifica `Ok` e `Err` dentro de `Option<Result<...>>`, com
`asSlice`, `get`, `pop` e `destroy`. O caso `Vec<char>` carrega
o escalar Unicode gerado através da transferência de ownership, da memória da
coleção e do retorno opcional, enquanto `Vec<float>` valida armazenagem e
recuperação do valor exato usado pelo oráculo numérico. O teste diferencial
executa esses programas nos três backends; as seeds de cobertura booleana também
selecionam `!`, `&&` e `||`. Os vetores passam por `transfer<T>(own value: T): T`, exercitando
movimentação até o retorno e o uso no chamador; todos os tipos de vetor
sintetizados passam também por `make_vec<T>(): vec.Vec<T>`, que cria o valor
dentro da função e o retorna ao chamador como owner antes da transferência. Os
vetores `int` também atravessam `make_vec → relay → transfer → main`, cobrindo
ownership consumido e retornado por duas funções genéricas consecutivas. Os
caminhos de sucesso liberam as coleções por `Vec.destroy()`, que consome o
owner. A síntese também coloca um `Vec<int>` não vazio em `OwnedPack`, retorna
o struct por `make_owned_pack`, transfere o valor por `transfer<T>` e valida
campo escalar e elemento do vetor antes da destruição recursiva automática.
Essa combinação cobre a transferência de propriedade de campos agregados, que
antes só aparecia no corpus EMI. Cada programa sintetizado também envolve o resultado calculado em
`Result<int, bool>` e `Result<char, bool>`: a primeira especialização escolhe
`Ok`/`Err` pelo booleano gerado, e a segunda executa explicitamente ambos os
estados. `Result<int, char>` também executa ambos os estados, verificando que o
tipo do erro é independente do tipo do valor. `isOk`, `isErr` e `unwrapOr` são
verificados contra valores independentes; payloads `char` passam pela API
genérica sem coerção para inteiro. O teste de lowering cobre 24 seeds
consultando parse, typecheck, diagnósticos de lowering e validação AMIR
diretamente; o oracle não executa a composição IDE por item, que repetia
análises fora do pipeline de compilação. A bateria diferencial exercita os
estados `Ok` e `Err` dessas especializações. Permanecem para incrementos
seguintes: outras formas de transferência e retorno de ownership e operações
de coleções adicionais; crescer o corpus EMI com programas de `stdlib/` e
`examples/` cuja condição morta possa ser provada; estender a promoção de
artefatos de falhas incrementais/LSP para as suítes específicas desses alvos.
A contagem e o conteúdo dos argumentos, o
retorno, stdout e stderr já são comparados, e o sucesso dos processos C/Wasm
é validado; os runners dependem de ferramentas externas presentes no ambiente.
