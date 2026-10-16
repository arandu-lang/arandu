# RFC 0013: CTFE determinístico e `comptime` core via AMIR

- **Número da RFC:** 0013
- **Título:** CTFE determinístico e `comptime` core via AMIR
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-09-12
- **Status:** `Draft`
- **Área Principal:** `Frontend` / `Middle-end` / `Incrementalidade`
- **PR da RFC:** N/A (In-Tree RFC)
- **Issue de Acompanhamento:** N/A

> Esta RFC completa continua sendo uma proposta. O recorte público escalar de
> expressões/blocos, posteriormente ampliado para valores congelados,
> instâncias concretas e expansão estática com aprovação do mantenedor,
> está implementado no contrato de
> [comptime core](../arandu-comptime-core-v0.1.md). Exemplos e recursos além desse
> contrato continuam ilustrativos; isso não aceita nem implementa a RFC inteira.

---

## 1. Resumo

Esta proposta define uma primeira etapa de avaliação de expressões Arandu em
tempo de compilação (CTFE), executadas sobre AMIR e integradas ao grafo
incremental. O objetivo é permitir que código puro e explicitamente requerido
em compilação produza valores constantes, sem introduzir uma linguagem de macro
separada nem efeitos ocultos no compilador.

O núcleo iniciado na 0.1.9 e distribuído na 0.1.10 é deliberadamente menor
que uma plataforma completa de metaprogramação:

1. Um domínio canônico de valores CTFE e um interpretador determinístico de um
   subconjunto documentado da AMIR.
2. `comptime` em expressões e blocos, `comptime if` e iteração finita sobre
   valores conhecidos em compilação.
3. Integração de parâmetros de valor com os const generics escalares já
   existentes, evitando mecanismos independentes e preservando compatibilidade.
4. Intrínsecos de layout `mem.sizeOf<T>()` e `mem.alignOf<T>()`, calculados para o
   layout configurado do alvo suportado. As formas `@sizeOf(Type)` e
   `@alignOf(Type)` da 0.1.9 permanecem compatíveis.
5. Memoização no Salsa, orçamento de execução, cancelamento cooperativo e
   diagnósticos estruturados.

Reflexão estrutural ampla (API proposta `types.info<T>()`), capabilities além
da identidade explícita de OS/arquitetura já entregue, memória virtual geral,
`quote`, splicing, `@Derive`, geração de items, inclusão de arquivos e JIT não
fazem parte do primeiro núcleo. Cada um depende
de contratos adicionais e deve ser avaliado em etapa ou RFC própria.

## 2. Motivação

CTFE pode sustentar avaliação de constantes, especialização limitada por valores,
validação estática e, posteriormente, reflexão e geração de código. Seu valor
arquitetural está em reutilizar a semântica tipada da linguagem e não em compilar
um valor isolado antes do backend.

O projeto já tem componentes que ajudam — AMIR, queries Salsa, `DataLayout` e
const generics. A implementação do núcleo acrescentou uma VM limitada com
valores escalares, agregados Copy fechados, strings imutáveis e floats IEEE
determinísticos. Expressões/blocos públicos, `comptime if`, argumentos
`count<comptime (expression)>()`, staging em instâncias concretas e expansão
finita de `comptime for` estão conectados. Intrínsecos públicos de layout
compartilham o `LayoutEngine`;
isso não constitui metaprogramação completa nem um modelo completo de alvo.
O [contrato público](../arandu-comptime-core-v0.1.md) consolida a campanha
encerrada e delimita a implementação. Em particular:

- `TargetInfo` do type checker atualmente expressa apenas a largura de ponteiro;
- `TargetConfig` fornece layout e identidade explícita de OS/arquitetura; isso
  não constitui um modelo completo de ABI/capabilities nem suporte a todo triple;
- argumentos constantes tipados incluem valores Copy admitidos, com domínio
  declarado e identidade estrutural verificados; dimensões e cabeçalhos
  calculados são congelados por contratos concretos antes de tipar o consumidor;
- `func_amir` fonte e `instance_amir` concreto já consultam unidades independentes;
  o programa agregado é um compositor final, não seu produtor.
- `ctfe_func_amir` baixa apenas o corpo selecionado, sem lowering global,
  e consulta a closure retida de callees importados na admissão;
  funções genéricas são rejeitadas nessa API por símbolo.
- `ctfe_instance_amir` é o caminho interno para instâncias escalares concretas:
  reutiliza `instance_hir`/monomorphização e traduz IDs sintéticos locais para
  `FunctionInstance`. O spelling público de parâmetros já reutiliza const
  generics; templates selecionam staging após substituição concreta.
  Cabeçalhos dependentes usam `concrete_header_contract`; ampliação de defaults
  ou dependências não admitidas exige o contrato correspondente.
- `declaration_signatures` já permite consultar imports sem baixar corpos;
  a visão compatível `module_signatures` compõe contratos de empréstimo
  projetados antes da validação final. Contratos usam unidades por instância;
  retenção/latência e composição final continuam gates separados da
  granularidade do produtor.

Essas limitações orientam a divisão em etapas. Não se deve prometer que CTFE
evitará toda reexecução incremental: Salsa pode cortar propagação quando uma
saída permanece igual, mas a query de avaliação pode precisar rodar novamente
quando uma dependência semântica muda.

## 3. Guia proposto

Os exemplos abaixo demonstram a intenção da feature, não congelam a gramática.

### 3.1. Expressões e blocos

```arandu
let table_size: usize = comptime choose_table_size()

comptime {
    assert(table_size > 0)
}
```

Uma avaliação CTFE só pode chamar operações e funções admitidas pelo subconjunto
de execução. Chamadas com I/O, efeitos de runtime ou acesso ambiental ao sistema
de arquivos são rejeitadas; não são executadas parcialmente.

Contrato de retorno confirmado pelo mantenedor em 2026-10-01: o bloco
`comptime { ... }` cria um destino de retorno próprio. `return val;` encerra
somente essa avaliação, não a função runtime que contém o bloco. Retornos em
ramos/loops internos pertencem à mesma avaliação; uma avaliação aninhada ou
função chamada possui seu próprio destino.

Todos os retornos explícitos devem unificar com a expressão final produtora de
valor, mesmo que algum deles seja inalcançável. O resultado usa o contexto de
tipo esperado, quando houver; sem contexto, é inferido desses valores pelo
checker canônico. `return;` só é válido quando esse resultado é unidade
(`void` na representação atual; `()` é a notação semântica de unidade, não um
novo tipo primitivo). Um bloco sem cauda produtora de valor é unitário quando
chega ao fim; se ele deve produzir um valor, todo caminho de saída precisa
devolvê-lo. `break`/`continue` não podem cruzar a fronteira da avaliação.

A API interna trata um `;` explícito como descarte do valor da expressão,
independentemente do tipo esperado. Essa é a proposta para a nova gramática de
bloco, não uma alteração do tratamento legado de caudas de funções ordinárias;
deve ser revista junto da aceitação da superfície pública.

O contrato possui provas na API de tipagem/lowering de blocos isolados
e no grafo interno `ctfe_root_amir → ctfe_eval_root`: a tipagem inicial da raiz
é independente do corpo runtime proprietário e os callees são rastreados pela
avaliação. A sintaxe pública se conecta a essas raízes e materializa o resultado
no corpo residual. Os seletores de raiz usados pelas provas continuam sendo
caminhos internos na AST canônica, não uma forma pública alternativa de escrever
`comptime`.

### 3.2. Decisões e iteração estáticas

```arandu
comptime if (TABLE_SIZE <= 256) {
    use_small_table()
} else {
    use_large_table()
}

comptime for index in 0..TABLE_SIZE {
    initialize_entry(index)
}
```

`comptime for` começa limitado a intervalos e agregados finitos conhecidos pelo
interpretador. Fuel limita também a expansão resultante; não há iteração
arbitrária ou permissão para travar o compilador.

O desenho deve distinguir avaliação integral de especialização: um bloco
`comptime { ... }` executa seu corpo em compilação; um `comptime if` em corpo
runtime avalia a condição para selecionar código residual; um `comptime for`
nesse contexto avalia o domínio para especializar instruções em ordem. O corpo
residual pode usar valores/efeitos runtime e não é executado pela VM. Dentro de
um bloco inteiramente CTFE, os corpos selecionados também executam em compilação.
A política entregue resolve/tipa somente o ramo estático selecionado; ambos
continuam sujeitos ao parser. Desvios e saídas da expansão finita preservam os
scopes e drops definidos no contrato público. Ampliações exigem contrato próprio.

### 3.3. Parâmetros de valor

```arandu
func matrix<T, comptime ROWS: usize, comptime COLS: usize>() {
    // dimensões conhecidas durante a instanciação
}
```

A forma exata da sintaxe permanece em aberto. A semântica deve se integrar aos
const generics escalares existentes e não criar uma segunda representação de
argumentos constantes. Mudanças à sintaxe atual exigem plano de compatibilidade.

### 3.4. Layout do alvo

```arandu
import std.core.mem as mem

func bytes<T>(): usize { return mem.sizeOf<T>() }
func alignment<T>(): usize { return mem.alignOf<T>() }
```

A revisão de contrato da 0.1.10 prefere a API de módulo. As expressões
`@sizeOf(Type)`/`@alignOf(Type)` continuam aceitas por compatibilidade; não se
adicionam aliases `@SizeOf`/`@AlignOf`. A convenção `@PascalCase` da RFC 0002
se aplica às anotações declarativas. Remoção de sintaxe anterior exige migração
explícita e prova de paridade em todas as posições suportadas.

Esses intrínsecos usam somente o layout do alvo selecionado e suportado pelo
backend. A sintaxe `target.os`, `target.arch`, `target.abi` ou
`target.has(feature)` não é definida nesta etapa: requer uma identidade de alvo
validada, além do layout.

## 4. Desenho e etapas propostas

Os IDs CT.0–CT.5 seguem a ordem canônica do
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md#fila-de-execução).
O [contrato público](../arandu-comptime-core-v0.1.md#published-distribution-evidence-and-remaining-boundaries)
consolida implementação, evidência publicada e limites da campanha encerrada.
CT.0 continua distinguindo aprovação do núcleo e aceitação da proposta ampla.

### 4.1. CT.0 — decisões necessárias antes da implementação

Decisão de staging confirmada pelo mantenedor em 2026-10-01: `comptime if`
verifica a sintaxe de ambos os ramos, mas resolve e tipa somente o selecionado.
O ramo descartado pode mencionar nomes ou tipos ausentes no alvo. Essa decisão
não aceita retroativamente as demais questões abertas. A implementação usa
uma fronteira de seleção anterior à resolução residual.
O contrato de retorno local/unificação/unidade da seção 3.1 também foi
confirmado; as demais questões e a aceitação formal da RFC continuam abertas.

Recorte público inicial confirmado pelo mantenedor em 2026-10-01: começar por
expressões/blocos `comptime` escalares (`bool`, inteiros admitidos e unidade
representada por `void`). Agregados, seleção `comptime if` e expansão
`comptime for` são cortes seguintes, sem mudar os contratos de retorno e de
ramo descartado já confirmados. Essa aprovação é do recorte, não da RFC inteira
nem uma aceitação dos recursos futuros descritos nesta proposta.
Tipos fora do recorte devem produzir diagnóstico semântico determinístico,
registrado em `DiagCode`, nunca ICE ou execução de fallback. As regressões do
corte público precisam incluir overflow, divisão por zero e resultado unitário.
O contrato público documenta a gramática, os limites fixos e os diagnósticos
entregues; a configuração pública de orçamento foi posteriormente conectada
à CLI/LSP/web, conforme o contrato público.

Para cada ampliação do núcleo, a RFC precisa fechar ou preservar:

1. O domínio de `ConstValue`, incluindo representabilidade, igualdade, hashing e
   serialização determinística para os tipos aceitos.
2. Quais rvalues, operações, chamadas e instruções AMIR podem ser interpretados.
3. A política para overflow, divisão inválida, recursão, alocação e valores não
   inicializados.
4. Como parâmetros de valor interagem com const generics, monomorphização e
   inferência, sem quebrar programas existentes.
5. O modelo de alvo inicial e as operações de layout válidas para cada backend.
6. Chaves e dependências da query de avaliação, incluindo contexto declarativo
   compartilhado e composição final, sem reentrar na validação de runtime.
7. Orçamento de instruções/frames/valores/expansão, cancelamento, contexto
   CLI/LSP e diagnósticos públicos; medir antes de fixar os defaults.
8. O grafo de staging para valores necessários durante tipagem, seleção de
   ramos e instanciação, sem `type_check → lower_amir final → type_check`.
9. Gramática e semântica separadas para avaliação integral, seleção de ramo e
   expansão finita; regras de capturas, desvios, scopes e ownership.

Se uma decisão exigir suporte que não existe, o item fica fora do núcleo até
essa dependência ser entregue; não deve ser simulado com dados do host.

Uma base interna já implementa o domínio escalar em `arandu_middle::ctfe` e
operações verificadas em `arandu_mir::ctfe`: `bool`, `void` e inteiros tipados,
larguras do alvo, conversões sem truncamento e codificação canônica v1 sem IDs
de pools. Shifts à esquerda usam multiplicação matemática verificada; à direita,
extensão de sinal para assinados e zeros para não assinados. Contagens inválidas,
overflow e divisão/resto inválidos produzem erros internos estruturados. A ponte
para `Const(u64)` é sem perda e rejeita negativos. A VM interpreta CFG com
chamadas/locais e aplica limites compartilhados e cancelamento. A integração
pública traduz falhas em diagnósticos estruturados e congela valores no AMIR
residual; ela não introduz um runtime CTFE nos programas gerados. O contrato
técnico registra a implementação, os limites e as provas. A RFC completa
permanece `Draft`.

### 4.2. CT.1 — alvo e layout

O banco recebe configuração explícita e validada do alvo antes das queries
semânticas que dependem dela. O descritor deve separar identidade de alvo e
`DataLayout`; um layout sintético não pode ser apresentado como triple ou
suporte de codegen nativo. CLI/LSP/web configuram a borda; a VM consome os dados
canônicos que typeck e `LayoutEngine` também usam.

OS, arquitetura, ABI e capabilities só podem ser expostos quando forem obtidos
de uma configuração explícita suportada — nunca inferidos do host durante a
avaliação. Esses campos públicos e cross compilation geral não são necessários
para a primeira entrega. Comparar larguras e alinhamentos, incluindo layouts
nos quais tamanho e alinhamento diferem, antes de expor resultados na linguagem.

### 4.3. CT.2 — valores e interpretador

O interpretador é uma função pura da AMIR, argumentos constantes, configuração
de alvo e orçamento. O conjunto inicial deve ser pequeno, explicitamente
enumerado e alinhado ao que a AMIR representa sem efeitos observáveis. O ponto de
partida recomendado é valores escalares e agregados imutáveis suportados pela
AMIR; ponteiros arbitrários, chamadas externas e efeitos ficam excluídos.

A primeira entrega da VM já inclui fuel, limites de frames/valores e um hook
de cancelamento cooperativo. A stack de execução não depende da stack nativa
sem limite. Mutação de locais da avaliação é distinta de efeitos observáveis
externos; CT.0 define as operações e tipos permitidos. A VM deve tratar largura
e sinal dos inteiros conforme tipo e alvo, sem adotar a representação do host.

Uma operação não suportada retorna um erro CTFE estruturado com span; nunca
causa panic no compilador nem é silenciosamente tratada como constante.
Implementar um modelo completo de memória virtual à maneira de Miri não é
pré-requisito para esse subconjunto e não deve ser introduzido sem necessidade
demonstrada.

### 4.4. CT.3 — superfície de linguagem e staging

Após o domínio de valores e a semântica de execução estarem testados, adicionar
as formas aprovadas de `comptime` em expressão/bloco, `comptime if` e iteração
finita. O type checker precisa rejeitar no ponto de origem construções que não
possam ser avaliadas com segurança, sem depender de falha tardia no backend.

Parâmetros `comptime` reutilizam a representação dos const generics atuais para
inteiros escalares no primeiro passo. Ampliação para tipos como argumentos,
valores arbitrários ou políticas é uma decisão futura, não implícita nesta RFC.
Inteiros não negativos mantêm a representação compatível `ArType::Const(u64)`;
inteiros assinados, booleanos e valores Copy fechados admitidos usam chaves
congeladas estruturais. O valor completo participa da igualdade; o digest usado
no nome emitido não substitui essa identidade. Conversões e domínio declarado
são verificados sem truncamento, reutilizando o mesmo monomorphizer. Preservar
a sintaxe `<const N: uint>` e `[N]T` faz parte da compatibilidade. O
[contrato de argumentos calculados](../arandu-comptime-core-v0.1.md#computed-generic-arguments)
e `arandu_query/tests/comptime_aggregates.rs` registram essas fronteiras.

Expressões no corpo podem usar um tipo esperado já conhecido; constantes que
determinam tipos, assinaturas ou ramos exigem tipagem/lowering das unidades de
avaliação antes da tipagem residual. A query de CTFE não pode usar `func_amir`
como atalho quando isso reentra no `lower_amir` final, dependente de typeck.
CT.0 precisa definir essa fronteira e os diagnósticos de ciclo; isso não exige
entregar toda a granularidade do pipeline AOT da RFC 0011.

### 4.5. CT.4 — Salsa, fuel e LSP

A avaliação é memoizada por query pura em `arandu_query`; crates de typeck, MIR e
backends não passam a possuir Salsa. A query deve depender de entradas
semânticas explícitas e retornar resultado estável e comparável.

Garantias exigidas:

- determinismo para as mesmas AMIR, argumentos, layout e configuração;
- limite de passos aplicado em cada operação/salto/chamada relevante;
- limites de frames, valores e expansão além do número de instruções;
- cancelamento cooperativo, especialmente durante análise interativa do LSP;
- cancelamento separado de erro semântico, sem resultado cancelado memoizado;
- nenhum I/O ou efeito global dentro da query;
- early-cutoff testado sobre o resultado, sem prometer que mudanças em
  dependências não reexecutam o interpretador;
- preservar a correção sem usar o MIR final durante tipagem inicial.
  As queries internas por função/instância já são independentes; completar
  o staging público não equivale a fechar todos os gates AOT da RFC 0011.

CT.4 acompanha cada corte público de CT.3; não se habilita uma forma no editor
para só depois implementar seu cancelamento e sua análise incremental.

Fuel, defaults CLI/LSP e opções de configuração só são congelados após benchmark
e testes de responsividade; os números apresentados em versões anteriores desta
proposta eram exemplos, não contrato.

### 4.6. CT.5 — reflexão mínima e critérios de saída do núcleo

A superfície preferida `mem.sizeOf<T>()`/`mem.alignOf<T>()` e as expressões
compatíveis `@sizeOf`/`@alignOf` reutilizam as operações canônicas de
`LayoutEngine` e a classificação compartilhada dos intrínsecos. A API proposta
`types.info<T>()`, com campos, métodos,
atributos ou acesso dinâmico por nome fica para etapa posterior, com contrato
próprio para identidade e visibilidade de tipos.

Cada release que amplia o núcleo precisa preservar testes que demonstrem
os contratos abaixo. A 0.1.10 já publicou o núcleo e executou smoke CTFE
instalado nos três sistemas; isso não aceita toda a RFC nem substitui os gates
de uma nova candidata:

1. Semântica e diagnósticos estáveis para todos os casos suportados e rejeitados.
2. Equivalência entre análise incremental e clean para os valores CTFE.
3. Resultados determinísticos entre execuções e hosts; resultados dependentes de
   layout variam conforme o layout-alvo selecionado, não o host.
4. Cancelamento e fuel funcionando sem bloquear indefinidamente CLI ou LSP.
5. Paridade com execução de runtime para o subconjunto puro compartilhado, onde
   ambos os modos forem definidos.
6. Regressões de invalidation/cutoff que comprovem quais queries são refeitas e
   quais consumidores downstream são preservados.
7. Diagnósticos novos registrados em `DiagCode`, catálogo e documentação de
   erros conforme as regras do workspace.

## 5. O que não faz parte desta RFC de núcleo

Os itens seguintes permanecem possibilidades futuras e exigem desenho separado:

- `quote`, `${...}`, geração de declarações, `@Derive` e DSLs;
- reflexão estrutural completa e `val.@field(name)`;
- APIs de target OS/arch/ABI/capabilities além do descritor suportado;
- inclusão de assets (APIs propostas `embed.bytes`/`embed.str`) ou acesso
  indireto a arquivos;
- interpretação geral de ponteiros, memória virtual completa ou equivalência
  integral com Miri;
- execução JIT de CTFE.

Geração de itens precisa resolver higiene, atribuição de `SymbolId`, resolução de
nomes, re-typecheck, ciclos e invalidação; não é uma extensão pequena do avaliador
de constantes.

## 6. Invariantes e custos

1. A execução é determinística e não usa estado global mutável.
2. Queries são puras, não fazem I/O e permanecem em `arandu_query`.
3. Valores dependentes de layout usam dados explícitos do alvo e nunca o host.
4. Fuel/cancelamento são obrigatórios antes de habilitar CTFE no LSP.
5. A AMIR VM não pode divergir silenciosamente da semântica das instruções que
   interpreta; o subconjunto suportado fica documentado e testado.
6. O escopo da VM cresce por necessidade comprovada, sem antecipar alocadores,
   virtual memory ou otimizações específicas de desempenho.

O custo principal é manter o interpretador coerente com typeck, AMIR, layout,
ownership e incrementalidade. O ganho de ergonomia não justifica enfraquecer
diagnósticos, aumentar invalidações sem medição ou criar uma segunda linguagem de
metaprogramação antes do núcleo estar estável.

## 7. Alternativas consideradas

| Alternativa | Vantagem | Custo/risco | Situação |
| --- | --- | --- | --- |
| Avaliar AST diretamente | Começo aparentemente simples | Duplica semântica e precede contratos tipados/AMIR | Não recomendada como arquitetura final |
| Reutilizar somente folding atual | Escopo pequeno | Não executa blocos ou chamadas CTFE | Adequado como etapa de preparação, insuficiente como núcleo |
| VM geral com memória virtual desde o início | Base para ponteiros e programas mais ricos | Grande superfície de segurança antes de haver casos que a exijam | Adiada até necessidade demonstrada |
| Macros/geração de código na primeira entrega | Maior expressividade inicial | Higiene, resolução, ciclos e expansão complexos | Fora do núcleo |

## 8. Arte prévia

Zig, Rust/Miri, Rust procedural macros, Circle C++ e D oferecem experiências
relevantes para avaliar ergonomia, interpretação, reflexão e expansão. Esta RFC
não afirma superioridade sobre essas abordagens nem quantifica substituição de
macros; comparações futuras precisam de workloads e critérios reproduzíveis.

## 9. Questões em aberto

O domínio entregue, a gramática pública, os limites e as regressões de cutoff
estão no contrato público; não são decisões inteiramente pendentes. A proposta
ampla ainda precisa definir:

1. O domínio de tipos como valores e sua identidade independente de pools.
2. Visibilidade, higiene e diagnósticos da reflexão e da geração de itens.
3. Contratos de efeitos e de inputs explícitos para recursos externos.
4. ABI/capabilities e alvos de codegen além da identidade de alvo já exposta.
5. Workloads e budgets de responsividade para novas formas de staging.

## 10. Possibilidades futuras

Após o núcleo demonstrar semântica, segurança e incrementalidade, podem ser
propostas RFCs para reflexão estrutural, capabilities por alvo, inclusão
determinística de recursos e geração higiênica de itens. O avanço de cada etapa
depende de casos de uso concretos e de contratos específicos; nada disso é
prometido pelo núcleo publicado ou como gate obrigatório da 0.2.

## 10.1. Dependências propostas para expansão

A fila permanente e os horizontes de versão pertencem ao
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md). Esta seção descreve
somente dependências técnicas propostas; não cria compromissos paralelos.
A 0.2 concentra-se no motor incremental quente. Tipos como valores, reflexão
e atributos de terceiros não são seus gates obrigatórios.

O núcleo entregue é a base C1. Um recorte incremental de introspecção/tipos
como valores (C2) pode ser avaliado em 0.3.x se aceito com contrato próprio,
separadamente do contrato de closures. Geração estrutural (C3) e derivação/
atributos (C4) dependem de identidade, higiene, visibilidade, limites e cutoff
comprovados; são possibilidades de 0.4+ condicionais, sem promessa de entrega.
Quoting/splicing e staging de módulos continuam propostas sem versão fixada.

### 10.1.1. Proposta C2 — Tipos como valores de primeira classe

Se C2 for aceita, tipos poderão ter representação como valores manipuláveis
no interpretador CTFE. O exemplo de formato abaixo é ilustrativo; um `TypeId`
local de pool não pode ser a identidade canônica persistente dessa descrição:

```rust
// Extensão em ConstValue:
pub enum ConstValue {
    Integer(ConstInt),
    Bool(bool),
    Void,
    TypeDesc(TypeId),   // tipo como valor de compilação
}
```

Isso habilita funções genéricas que escolhem tipos com base em lógica de compilação:

```arandu
// Tipo numérico de tamanho mínimo capaz de armazenar N bits:
comptime func MinInt<comptime Bits: uint>: type {
    comptime if Bits <= 8 {
        return i8
    } else if Bits <= 16 {
        return i16
    } else if Bits <= 32 {
        return i32
    } else {
        return i64
    }
}

type PacketId = MinInt<12>   // PacketId é i16
```

### 10.1.2. Proposta C3 — Geração estrutural e reflection API

C3 propõe que `comptime func` retorne descrições de novos tipos estruturais
e que tipos existentes exponham membros por reflexão. Os exemplos seguintes
não são sintaxe executável do núcleo atual:

```arandu
// Geração de struct de vetor com dimensão calculada:
comptime func VecN<comptime N: uint>: type {
    return struct {
        data: [N]float

        func dot(shared self, other: VecN<N>): float {
            let mut sum = 0.0
            comptime for i in 0..N {
                sum += self.data[i] * other.data[i]
            }
            return sum
        }
    }
}

type Vec3 = VecN<3>   // Vec3 tem data: [3]float e método dot()
```

A API de reflection expõe introspecção estrutural:

```arandu
// Inspeção de campos em tempo de compilação:
comptime func printFields<comptime T: type>(): void {
    comptime for field in T.fields() {
        // field.name: str
        // field.ty: type
        // field.offset: usize
    }
}
```

### 10.1.3. Proposta C4 — Geração de declarações e atributos

A proposta C4 conectaria a geração de código à sintaxe de atributos declarativos
estabelecida pela [RFC 0024](0024-unified-attribute-system.md). Um atributo
`@Foo(args...)` aplicado a uma declaração D executa `Foo.apply(D, args...)` em
comptime, e pode injetar métodos, implementações de interface ou metadata
diretamente no módulo:

```arandu
// Derivação automática de Debug via reflection — sem compilador conhecer Debug:
@Derive(Debug, Eq, Hash)
struct Point {
    x: float
    y: float
}

// O resultado é idêntico a escrever manualmente:
// func Point.debug(shared self): str { ... }
// func Point.eq(shared self, other: Point): bool { ... }
```

A geração imperativa no módulo atende a casos onde o alvo não é uma declaração
única (como schemas de banco ou clientes de API):

```arandu
// Metaprogramação pesada — geração de tabela SQL e métodos de persistência:
comptime Sql.table(Point, tableName: "points")
```

### 10.1.4. Invariantes de compilação e integridade incremental

Para que a geração de AST em comptime não comprometa a arquitetura do Arandu,
três garantias são obrigatórias:

1. **Identidades estáveis para código gerado**: a origem estrutural da expansão
   precisa mapear para o `SymbolId { file_id, local_id }` existente, sem reutilizar
   IDs ou persistir offsets/IDs de pools como identidade. Uma variante de ID
   gerado exigiria discussão arquitetural explícita; não está aprovada aqui.
2. **Higiene e precedência ("manual vence")**: quando um atributo gera um método
   ou membro cujo nome já foi definido explicitamente pelo usuário, a
   implementação manual tem precedência estrita e o gerador omite
   silenciosamente o membro conflitante. Não há substituição silenciosa de código
   do usuário.
3. **Pureza e isolamento de efeitos**: metaprogramação em comptime é pura por
   padrão. Uma anotação de efeito não autoriza I/O dentro de queries: qualquer
   recurso externo exigiria contrato próprio, leitura na borda do driver e
   inputs Salsa explícitos. A avaliação continua pura e determinística.
