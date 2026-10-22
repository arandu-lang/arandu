# Verificação de releases Arandu

Uma release oficial contém três archives, seus sidecars por algoritmo,
`release-manifest.json`, `BLAKE3SUMS` e `SHA256SUMS`. O manifest liga versão,
tag, commit, target, tamanho e hashes de cada archive. O workflow rejeita um
arquivo ausente, extra ou divergente antes de criar uma release pública.

## Evidência de 15/10/2026

A [release v0.1.10](https://github.com/arandu-lang/arandu/releases/tag/v0.1.10)
foi publicada às 18:08:32 UTC no commit
`7df12f7a36ed872ea6c04287cb4e536525bc5291`. O
[run 38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029)
aprovou packages, integração SDK/VSIX e Public RC install/smoke em Linux x86-64,
macOS ARM64 e Windows x86-64, incluindo os casos CTFE do smoke. Isso é evidência
desses artefatos e fluxos, não de cobertura completa de runtime ou paralelismo.

O empacotamento e o validador de archives são nativos em Rust. Ainda existem
fallbacks Python nos instaladores e testes Python no workflow. O instalador
publicado na 0.1.10 permite continuar sem checksum quando não encontra
verificador. A correção local da campanha 0.1.11 exige SHA-256, valida o
archive/BLAKE3 em staging e preserva a instalação anterior se a promoção
falhar. Seus 22 testes herméticos passaram; o pacote público da próxima
candidata ainda precisa ser exercitado nos runners nativos.

## Verificar os bytes

Baixe todos os assets da release e valide primeiro o checksum adequado ao host:

```bash
gh release download v0.1.10 --repo arandu-lang/arandu --dir arandu-release
cd arandu-release
sha256sum --check SHA256SUMS
```

Quem possui `b3sum` pode validar também:

```bash
b3sum --check BLAKE3SUMS
```

Os arquivos `*.sha256`, `*.sha256sum`, `*.blake3` e `*.blake3sum` permanecem
publicados para instalação direta e ferramentas que esperam sidecars.

## Verificar origem e workflow

Checksum prova que os bytes correspondem ao manifest; provenance prova que os
bytes foram atestados pelo workflow do repositório. Verifique o archive e o
manifest, restringindo a identidade ao repositório e ao workflow de release:

```bash
gh attestation verify arandu-0.1.10-x86_64-unknown-linux-gnu.tar.gz \
  --repo arandu-lang/arandu \
  --signer-workflow arandu-lang/arandu/.github/workflows/release.yml

gh attestation verify release-manifest.json \
  --repo arandu-lang/arandu \
  --signer-workflow arandu-lang/arandu/.github/workflows/release.yml
```

Para uma política ainda mais estrita, acrescente `--source-ref refs/tags/vX.Y.Z`
e compare `commit` no manifest com o commit imutável da tag.

## Publicação e correções

O workflow monta e valida o conjunto completo, cria uma release em draft,
anexa todos os assets, gera as attestations e somente então publica o draft.
Uma falha intermediária pode deixar um draft privado para inspeção, nunca uma
release pública parcial.

A configuração exigida para publicação imutável fica no GitHub:

1. `Settings` → `General` → `Releases`.
2. Selecione `Enable release immutability`.

A configuração vale apenas para releases futuras. Depois de publicada, uma
release imutável não permite substituir nem apagar tag/assets. Qualquer correção
de bytes exige nova versão e nova tag; nunca mova ou reutilize uma tag publicada.

## Dry-run

`Actions` → `Release` → `Run workflow` executa os mesmos builders, instalação,
smoke, conjunto exato, manifest e checksums. O resultado fica no artifact
`verified-release-set` do run. Execução manual não recebe permissões de
publicação, não cria release e não gera provenance oficial.
