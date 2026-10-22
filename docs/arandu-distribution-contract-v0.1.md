# Contrato de distribuição Arandu v0.1

**Estado:** S3 Gold; contrato normativo da distribuição nativa v0.1.
**Modelo:** SDK autocontido, semelhante ao Flutter: CLI, servidor de linguagem
e biblioteca padrão formam uma unidade versionada; a extensão do editor é um
cliente separado.

## Visão Geral e Contexto

O contrato define o SDK autocontido, sua identidade, verificação e matriz
nativa para que o usuário não dependa do monorepo ou de uma instalação Rust.

## Detalhes Técnicos da Implementação

### Promessa v0.x

O primeiro canal público é `0.1.0-rc.N`. Uma RC pode ser substituída apenas por
uma nova versão e nunca pela movimentação da mesma tag. `0.1.z` preserva os
comandos, formatos e superfícies explicitamente publicados neste documento;
uma quebra deliberada exige `0.2.0`. APIs Rust entre crates, HIR/AMIR internos,
queries Salsa e formatos não documentados não são API pública estável.

`0.1.0` só pode ser promovido quando os três hosts obrigatórios passarem o gate
de distribuição a partir dos archives publicados, sem checkout, Cargo ou
toolchain de desenvolvimento; nenhuma RC pode manter bloqueador conhecido.

### Componentes do SDK

Cada distribuição contém versões compatíveis de:

- `arandu` (`arandu.exe` no Windows), a CLI;
- `arandu-lsp` (`arandu-lsp.exe` no Windows), usado por editores;
- a biblioteca estática interna do runtime para o target do SDK;
- a árvore completa da stdlib;
- licença, manifest de release e hashes do conteúdo.

A extensão VS Code não incorpora outra cópia do compilador. Ela localiza o
`arandu-lsp` instalado, fala LSP e mantém seu próprio ciclo de publicação, mas
sua versão de compatibilidade é validada antes de uma release do SDK.

### Matriz inicial

| Host | Target Rust | Formato obrigatório | Estado no S3-A |
| --- | --- | --- | --- |
| Linux x86-64 glibc | `x86_64-unknown-linux-gnu` | `.tar.gz` | S3-C gold |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` | S3-C gold |
| Windows x86-64 | `x86_64-pc-windows-msvc` | `.zip` | S3-C gold |

`.pkg`, `.msi`, Homebrew, Winget/Scoop e repositórios Linux são conveniências
posteriores. Eles não substituem os archives portáveis nem podem ampliar a
matriz suportada sem o mesmo smoke nativo. macOS Intel, Linux musl/ARM e
Windows ARM não possuem binário prometido no beta inicial.

### Comandos e dependências do host

Após colocar `bin` no `PATH`, o nome do comando é `arandu` em Bash, Zsh,
PowerShell e CMD. O sufixo `.exe` é transparente no Windows.

| Operação publicada | Ferramenta externa exigida |
| --- | --- |
| `arandu --version`, `doctor`, `new`, `check`, `fmt` | nenhuma |
| `arandu run` pelo Cranelift JIT do host | nenhuma |
| `arandu build` para executável AOT do host | linker nativo: MSVC Build Tools, `cc` ou Clang |
| executar `arandu-lsp` | nenhuma; o editor/cliente LSP é separado |
| gerar fonte com `emit-c` | nenhuma |
| compilar e ligar a fonte C gerada | GCC/Clang compatível; caminho experimental |
| compilar o próprio Arandu a partir da fonte | Rust fixado e toolchain nativo do host |

No AOT e no build da fonte, Windows MSVC requer Visual Studio Build Tools e Windows SDK;
macOS requer Xcode Command Line Tools; Linux requer linker, headers libc e
GCC/Clang. Isso não impede `check`, `run` JIT ou o LSP numa instalação binária,
mas `build` falha de forma operacional e preserva o último artefato válido.
O SDK inclui a biblioteca estática do runtime correspondente ao target.

No Windows, a CLI prepara objetos, runtime e saída do linker em um diretório
temporário com caminhos completos abaixo de `MAX_PATH`. Argumentos relativos
isoladamente não removem esse limite do MSVC. Os nomes e hashes do cache de CGU
permanecem intactos; somente o executável ligado com sucesso volta ao staging
da publicação atômica. O diretório temporário é removido em sucesso ou erro.
Se o próprio diretório temporário for longo demais, `build` solicita um `TEMP`
mais curto e gravável. Essa preparação também abrange o linker configurado
explicitamente e o fallback `rustc` exclusivo dos checkouts de desenvolvimento.

### Limites explícitos de v0.1

- `run` é host-JIT; `build` gera executável AOT apenas para o host. Não há
  promessa de cross-compilation nem de executar um artefato em outro target.
- A ABI da biblioteca estática do runtime é interna e versionada junto com o
  compilador; ela não é uma ABI pública para link manual.
- LLVM/LTO/PGO, ABI estável, freestanding, self-hosting e debugger estão fora
  do contrato. `build --release` usa AMIR O2 e Cranelift AOT otimizado para
  velocidade; isso não promete paridade de qualidade com um futuro LLVM.
- O backend C é experimental; gerar C não significa suporte ao ABI/toolchain
  MSVC.
- Binários não prometem MSRV. A fonte só é validada com `rust-toolchain.toml`.
- “Suportado” exige instalação e smoke em host limpo, não apenas build no CI.

### Identidade e verificação

A tag `vX.Y.Z[-rc.N]`, as versões dos crates publicáveis, CLI, LSP e extensão
devem coincidir. `cargo run --locked -p xtask -- check-release-contract` impede
divergência antes do empacotamento. O manifest interno, SHA-256 do archive,
BLAKE3 do conteúdo em staging, provenance e conjunto completo de assets
integram a cadeia atual de publicação. `xtask package-archive`,
`validate-archive` e `prepare-release-assets`, mais `arandu archive validate`,
são implementações Rust ativas (RFC 0020).

Em 15/10/2026, a release v0.1.10 foi publicada às 18:08:32 UTC no commit
`7df12f7a36ed872ea6c04287cb4e536525bc5291`; o
[run 38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029)
aprovou SDK/VSIX e Public RC install/smoke dos três alvos, incluindo CTFE no
corpus instalado. Essa evidência não promove automaticamente todas as
superfícies async, de paralelismo ou de stdlib para Gold.

## PONTOS DE MELHORIA (O que não está no roadmap)

Bash e PowerShell preparam staging e chamam os comandos Rust; Python permanece
em testes/smokes e como fallback de SHA-256. Não é requisito do executável SDK
nem do empacotamento nativo. A erradicação integral de Python não está concluída.

Na implementação local da campanha 0.1.11, `scripts/install.sh` rejeita a
instalação sem verificador SHA-256 e oferece `sha256sum`/`shasum`/`openssl`/Python.
Também valida manifest/archive com o CLI em staging e verifica `BLAKE3SUMS`
antes de substituir a instalação. Publicação interrompida restaura a árvore
anterior. O harness `scripts/test_install.py` cobre esses caminhos sem rede;
a correção ainda exige promoção no pacote exato, não altera a prova histórica
da release 0.1.10. Python é dependência do harness, não do bootstrap quando
existe uma das alternativas nativas.

## Futuro e Próximos Passos

A fila de correções e evolução permanece no roadmap mestre. Promover novos installers/hosts
somente com smoke fora do checkout sobre os artefatos públicos exatos.
