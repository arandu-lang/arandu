# RFC 0015: Arquitetura mobile nativa e bindings de interoperabilidade

- **Número da RFC:** 0015
- **Autor(es):** Equipe Arandu
- **Data de Início:** 2026-09-13
- **Status:** `Draft`; toolchain mobile e geradores de bindings não implementados
- **Área Principal:** Backend / tooling / interop

## 1. Resumo

Esta RFC propõe compartilhar um núcleo Arandu entre Android e iOS, mantendo a
interface em Kotlin/Compose ou Swift/SwiftUI. O desenho pretende usar bibliotecas
nativas e bindings tipados, com acesso direto a buffers somente onde ownership,
lifetime, alinhamento e sincronização puderem ser demonstrados.

Não existe hoje um pipeline publicado de NDK/iOS, gerador Kotlin/Swift,
atributo `@mobile`, comando `arandu package --target mobile`, `.aar` ou
`.xcframework` com esse contrato. Os exemplos da proposta original descreviam
uma API hipotética; não devem ser usados como documentação de comandos ou
atributos disponíveis. A fila executiva pertence ao
[roadmap mestre](../arandu-compiler-roadmap-v0.1.md).

## 2. Fundação atual e lacuna mobile

A infraestrutura existente inclui layout dependente de alvo, identidade
explícita e backends nativos/Wasm. Em particular:

- [`TargetIdentity::from_triple`](../../crates/arandu_middle/src/db.rs) extrai
  `os` e `arch` de triples admitidas, incluindo identidades Android/iOS. O
  resultado não guarda uma descrição completa da ABI nem deduz `DataLayout`.
- [`DatabaseImpl`](../../crates/arandu_query/src/db.rs) expõe
  `set_target_identity` e `set_target_config`; identidade e layout devem ser
  fornecidos coerentemente pelo driver, antes das queries dependentes de alvo.
- [`target_config.rs`](../../crates/arandu_query/tests/target_config.rs)
  verifica configurações semânticas de alvo. Isso não valida linking,
  instalação, assinatura ou execução em aparelho.
- O [contrato de distribuição](../arandu-distribution-contract-v0.1.md)
  publica SDKs desktop para Linux x86-64, macOS ARM64 e Windows x86-64.
  macOS ARM64 não equivale a iOS AArch64: ABI, SDK e políticas de execução são
  diferentes, e sucesso num runner desktop não comprova suporte mobile.

Portanto, não falta criar do zero identidade de alvo/triple. Falta conectar e
validar a cadeia completa de código, ABI, toolchains e produto mobile.

## 3. Arquitetura proposta

O núcleo compartilhado deve expor uma superfície estreita de dados e operações.
A UI permanece sob os frameworks do host. Bindings deveriam derivar do modelo
semântico tipado e gerar artefatos determinísticos, evitando reparse paralelo
do texto. O dono de execução Salsa continua sendo `arandu_query`; escrita de
arquivos e invocação de Gradle/Xcode pertencem ao driver.

Não se fixa uma query `mobile_bindings_for_file` nem novos tipos de IR nesta
RFC como se já existissem. O contrato futuro deve especificar as dependências
exportadas que permitem cutoff de edições privadas, com testes de equivalência
incremental/clean e ausência de invalidação indevida dos bindings.

### Fronteira de dados

Zero-copy é uma propriedade a demonstrar por operação, não uma garantia geral:

- `DirectByteBuffer` e views de ponteiros Swift podem representar memória
  externa; não prolongam sozinhos o lifetime do owner Arandu.
- O contrato precisa dizer quem aloca/libera, quando o host pode reter a view,
  qual a mutabilidade permitida e como o handle é invalidado.
- Arrays Swift, strings, listas, objetos e enums podem exigir cópia/conversão.
  Não se promete mapear qualquer `ContiguousArray` diretamente ao storage Arandu.
- Referências geracionais não substituem uma prova de empréstimo nem validam
  automaticamente um ponteiro cru já entregue ao host.
- Layout C, alinhamento e largura de ponteiro devem ser definidos pelo alvo;
  a existência de `DataLayout` não estabelece ABI pública estável por si só.

### Erros, concorrência e host

O desenho deve explicitar a conversão de `Result` e o comportamento de falhas
fatais. Unwinding não pode atravessar uma fronteira estrangeira sem protocolo
aprovado. ICE do compilador e panic de aplicação são fenômenos distintos.

Execução fora da thread de UI é uma opção de integração, não garantia de
120 FPS ou zero jank. O runtime atual usa sincronização explícita e possui
limites de async/paralelismo descritos em seus contratos. Não existe um bridge
mobile SPSC lock-free demonstrado só pela existência de canais nativos.
`CriticalNative`/`FastNative`, se usados futuramente, devem respeitar a API e
versão de Android aplicáveis; não se promete custo de chamada equivalente a C
ou qualquer número de nanossegundos sem medição do bridge concreto.

## 4. Distribuição e evidência necessária

Uma proposta executável de suporte mobile precisa fechar, por plataforma:
ABI exportada, sysroot/SDK, linker, runtimes necessários, packaging e validação
em simulador/aparelho. Android NDK e Apple SDK/Xcode são dependências a integrar;
reconhecer a identidade do target não fornece essas ferramentas.

O escopo deve incluir vida útil dos buffers, thread affinity, falha/cancelamento,
teardown, símbolos e compatibilidade do binding com cada artefato. Assinatura,
provisionamento e restrições de código executável no iOS precisam de contrato
próprio. Esta RFC não promove suporte por cross-compilation local ou inferência
a partir do sucesso de SDKs desktop.

## 5. Invariantes e tradeoffs

- Preservar IDs estáveis, fronteiras de query e pureza da análise.
- Manter custos de cópia/alocação visíveis e admitir cópia quando necessária
  para uma interface segura.
- Separar aprovação de ABI de conveniências de bindings e packaging.
- Publicar apenas garantias verificadas em targets declarados. Ausência de GC
  global no núcleo não elimina ciclos de retenção ou pausas do host.

## 6. Possibilidades futuras

Bindings de eventos/async, pacotes para Gradle/SPM, hot reload e outros targets
podem ampliar o desenho após validação do núcleo de interop. Não constituem
entregas aprovadas nem justificam alegações de superioridade sobre outras
linguagens sem workload comparável. O roadmap mestre concentra o sequenciamento
condicionado a escopo e evidência.
