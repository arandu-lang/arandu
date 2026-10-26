# Contrato de ferramentas e scripts

> **Estado em 15/10/2026:** empacotamento e validação nativos implementados
> (RFC 0020); remoção integral de Python e fechamento fail-closed dos
> instaladores ainda incompletos. A fila executiva pertence ao roadmap mestre.

Este é o mapa oficial de automação. Regras de compilação, resolução,
lockfile, integridade e release pertencem ao Rust (`xtask` ou crates). Scripts
não podem reimplementar essas regras; apenas adaptam o ambiente do sistema e
encaminham a validação para o comando canônico.

## Visão Geral e Contexto

Este contrato define quais automações pertencem a Rust/xtask e quais wrappers
de sistema continuam necessários durante o bootstrap.

## Detalhes Técnicos da Implementação

| Grupo | Arquivos | Responsabilidade |
| --- | --- | --- |
| Tarefas semânticas | `cargo run -p xtask -- ...` | Diagnósticos, corpus, release contract e campanhas do compilador |
| Empacotamento | `package-release.sh`, `package-release.ps1` → `xtask package-archive` / `prepare-release-assets` | Preparar staging e produzir archives e manifests via Rust |
| Integridade de archives | `xtask validate-archive`, `arandu archive validate` | Validação nativa de estrutura, segurança e manifest; Python legado serve de oracle de testes |
| Instalação | `install-from-tarball.sh`, `install-from-zip.ps1`, `install-local.sh` | SHA-256 bootstrap, staging, PATH e diretórios nativos |
| Smoke/reprodutibilidade | `smoke_distribution.py`, `test_release_archives.py`, `check-package-reproducibility.sh` | Exercitar artefatos públicos em runners nativos |
| Diagnósticos | `check-diag-docs.sh`, `check-diag-determinism.sh` | Wrappers de CI para `xtask` e testes de concorrência |

### Política por sistema

- Bash é o wrapper de Linux/macOS; PowerShell é o wrapper de Windows.
- Python permanece nos smokes e oracles de teste e como fallback SHA-256 em
  instaladores. Não é requisito do SDK instalado nem o motor de empacotamento
  de produção; não decide identidade de dependência.
- O mesmo caso de uso deve produzir o mesmo resultado sem depender do shell.
  CI deve chamar o comando canônico diretamente quando houver equivalente.
- Um novo script exige dono, plataforma, comando canônico que valida sua saída
  e um teste. Duplicação de parser ou regra de segurança é proibida.

### Migração gradual

1. Adicionar a regra no crate/Rust responsável.
2. Cobrir a regra com teste unitário e um teste de integração multiplataforma.
3. Reduzir o script a preparação de argumentos e tradução de erros.
4. Manter o wrapper antigo durante uma versão e removê-lo somente após a CI
   usar o comando Rust em Windows, Linux e macOS.

## PONTOS DE MELHORIA (O que não está no roadmap)

Wrappers ainda formam uma superfície ampla de manutenção multiplataforma. Eles
não podem duplicar regras de lockfile, integridade, resolução ou release.

`scripts/install.sh` ainda aceita continuar sem verificar SHA-256 quando nenhum
verificador é encontrado. Esse comportamento fail-open permanece dívida de
segurança; o contrato não o considera validação bem-sucedida. Já o instalador
de tarball falha sem verificador e usa o próprio CLI do staging para validar
estrutura e BLAKE3 antes de publicar a instalação.

## Futuro e Próximos Passos

A fila de evolução está no roadmap mestre. Alterações dos wrappers devem
preservar bootstrap sem instalação prévia, falha explícita de integridade e
provas nativas sobre os artefatos públicos exatos.
