# RFC 0020: Empacotamento Nativo, Validação de Arquivos e Erradicação de Dependências Python via `xtask` e CLI (*Native Packaging, Archive Validation & Zero-Python Toolchain*)

- **Número da RFC:** 0020
- **Título:** Empacotamento Nativo, Validação de Arquivos e Erradicação de Dependências Python via `xtask` e CLI (*Native Packaging, Archive Validation & Zero-Python Toolchain*)
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-09-20
- **Status:** `Draft`
- **Área Principal:** `Tooling` / `Distribution` / `CI` (`xtask`, `arandu_cli`, `scripts`)
- **Documentos Relacionados:**
  - `docs/arandu-distribution-contract-v0.1.md`
  - `docs/arandu-compiler-roadmap-v0.1.md` (Marco `DIST` / S3)
  - `docs/rfcs/0004-project-package-lifecycle.md`
  - `docs/rfcs/0019-zero-bloat-target-and-shared-cache.md`

---

## 1. Resumo (Summary)

Esta RFC estabelece a arquitetura e o plano de migração definitiva para eliminar todas as dependências de interpretadores externos (especificamente **Python 3**) dos pipelines de compilação, empacotamento, validação de distribuição, testes e instalação do compilador Arandu.

A proposta transfere a responsabilidade de empacotamento determinístico e validação de segurança para código Rust nativo compilado dentro do workspace:
1. **Comandos Nativos no `xtask`**: Criação dos subcomandos `package-archive`, `validate-archive` e `prepare-release-assets` dentro do crate `xtask`, utilizando implementações nativas e reproduzíveis baseadas em `tar`, `flate2` e `zip`.
2. **Subcomando Nativo no CLI (`arandu archive validate`)**: Implementação do marco de distribuição `DIST` previsto no roadmap oficial, permitindo que o binário compilado do Arandu valide a integridade de archives de forma autocontida sem necessidade de runtimes de terceiros.
3. **Hardening dos Scripts de Instalação**: Remoção de qualquer chamada obrigatória a `python3` nos instaladores shell (`install.sh`, `install-from-tarball.sh`), substituindo por cascatas de ferramentas utilitárias padrão do sistema (`sha256sum`, `shasum`, `openssl`) e validação em staging via `arandu hash-file` com `BLAKE3SUMS`.
4. **Preservação de Scripts Python Exclusivamente como "Oracles" de Teste**: Os scripts Python legados (`reproducible_tar.py`, `reproducible_zip.py`) são movidos para `scripts/oracles/` com a função estrita de *oracles diferenciais*, validando paridade bit-a-bit durante a janela de migração antes do seu arquivamento final.

---

## 2. Motivação (Motivation)

### 2.1 O Modelo Autocontido vs Dependências Ocultas de Runtime

O [Contrato de Distribuição Arandu v0.1](file:///home/bruno/Documentos/Desenvolvimento/Arandu-Lang/docs/arandu-distribution-contract-v0.1.md) estipula formalmente:
> *"Modelo: SDK autocontido, semelhante ao Flutter: CLI, servidor de linguagem e biblioteca padrão formam uma unidade versionada; a extensão do editor é um cliente separado."*

No entanto, o pipeline atual de desenvolvimento e distribuição acumulou dependências circunstanciais de Python:
- **`scripts/reproducible_tar.py`** (185 linhas): Gera `.tar.gz` canônicos com mtime fixado, ordenação lexicográfica e uid/gid zerados;
- **`scripts/reproducible_zip.py`** (88 linhas): Gera `.zip` canônicos para Windows;
- **`scripts/prepare_release_assets.py`** (113 linhas): Valida hashes cruzados, gera `release-manifest.json`, `SHA256SUMS` e `BLAKE3SUMS`;
- **`scripts/smoke_distribution.py`** (183 linhas): Descompacta e roda smoke tests de SDK fora do checkout;
- **Snippets inline em Bash e PowerShell**: `install-from-tarball.sh` falha fatalmente se `python3` não estiver no `PATH` (linhas 52 e 93); `package-release.sh` usa Python para ler `Cargo.toml` e gerar JSON.

### 2.2 Por Que Isso é um Débito Técnico Crítico?

1. **Falha em Ambientes Mínimos e Contêineres de CI**:
   - Imagens Docker enxutas (como Alpine Linux, Ubuntu Core ou contêineres de compilação sem pacotes extras) **não trazem Python instalado por padrão**. Um desenvolvedor ou pipeline que baixa o tarball do Arandu para instalar em um servidor é forçado a instalar 50+ MB de interpretador Python apenas para rodar o script de instalação.
2. **Inconsistência Multiplataforma**:
   - No Windows, a invocação varia entre `python`, `python3` ou o launcher `py.exe`; se o usuário não configurou o PATH, o comando tenta abrir a Microsoft Store.
   - No macOS, a Apple removeu o Python nativo do sistema a partir do macOS 12 Monterey, forçando o prompt de instalação das ferramentas de linha de comando do Xcode.
3. **Redundância Arquitetural**:
   - O Arandu já possui um runner nativo de automação de workspace em Rust (`xtask`). O `xtask` já possui as bibliotecas `sha2`, `serde_json` e `arandu_middle`, e já implementa validações de release como `check-release-contract` e `check-slt6-sdk`.
   - Manter scripts em Python paralelos ao `xtask` fragmenta o ferramental do projeto e viola a homogeneidade da engenharia do compilador.
4. **Roadmap DIST (Fase 6)**:
   - O roadmap já previa explicitamente: `[ ] DIST Validadores TAR/ZIP nativos no CLI (arandu archive validate), removendo Python dos instaladores e containers; manter Python apenas como oracle de testes`. Esta RFC torna esse marco executável imediatamente.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 A Nova Experiência de Empacotamento e Release

Para quem desenvolve o compilador ou opera os workflows de CI/CD, todos os scripts manuais são consolidados em comandos nativos via `cargo run -p xtask`:

```bash
# 1. Empacotar um tarball canônico reproduzível (Linux/macOS):
cargo run -p xtask -- package-archive \
  --source dist/staging/arandu-0.1.7 \
  --output dist/arandu-0.1.7-x86_64-unknown-linux-gnu.tar.gz \
  --epoch "$SOURCE_DATE_EPOCH"

# 2. Validar o archive gerado contra as regras de segurança do contrato:
cargo run -p xtask -- validate-archive \
  dist/arandu-0.1.7-x86_64-unknown-linux-gnu.tar.gz \
  --root arandu-0.1.7 \
  --target x86_64-unknown-linux-gnu \
  --version 0.1.7

# 3. Preparar a suíte completa de assets de release (hashes e manifestos):
cargo run -p xtask -- prepare-release-assets dist/release-assets \
  --version 0.1.7 \
  --tag v0.1.7 \
  --commit 4b825dc642cb6eb9a060e54bf8d69288fbee4904
```

### 3.2 A Nova Experiência de Instalação (Zero Python)

Ao instalar o Arandu em uma máquina limpa através do tarball:

```bash
./scripts/install-from-tarball.sh dist/arandu-0.1.7-x86_64-unknown-linux-gnu.tar.gz
```

O script:
1. Verifica o hash SHA-256 usando `sha256sum`, `shasum` ou `openssl`;
2. Descompacta o tarball em um diretório temporário isolado (`staging`);
3. Executa o próprio binário extraído `$STAGE/bin/arandu hash-file` para verificar o arquivo interno `BLAKE3SUMS`;
4. Move atomicamente para `/usr/local/` (ou prefixo configurado).

**Nenhum interpretador Python, Perl ou runtime dinâmico é invocado em nenhum momento.**

### 3.3 Validação de Arquivo Direto pelo CLI

Qualquer usuário ou script de automação pode validar a estrutura interna de um archive baixado usando o próprio compilador:

```bash
arandu archive validate arandu-0.1.7-x86_64-unknown-linux-gnu.tar.gz
# Saída:
# archive ok: 124 files, 1 symlink, valid release-manifest.json (target: x86_64-unknown-linux-gnu)
```

---

## 4. Detalhes Técnicos de Implementação (Reference-Level Explanation)

### 4.1 Arquitetura de Módulos no `xtask`

A estrutura do crate `xtask` é expandida para incorporar os subsistemas de empacotamento:

```text
xtask/
 ├── Cargo.toml                  <-- Adiciona tar = "0.4", flate2 = "1.0", zip = "2.2", blake3
 └── src/
      ├── main.rs                <-- Roteia "package-archive", "validate-archive", "prepare-release-assets"
      ├── archive/
      │    ├── mod.rs            <-- Trait comum de validação e modelo canônico
      │    ├── tar.rs            <-- Geração canônica .tar.gz reproduzível
      │    ├── zip.rs            <-- Geração canônica .zip reproduzível (Windows)
      │    └── validator.rs      <-- Regras de segurança (path traversal, taxonomias, manifest)
      └── release_assets.rs      <-- Agregação de sidecars, SHA256SUMS, BLAKE3SUMS e manifest
```

### 4.2 Invariantes Canônicos do Gerador de `.tar.gz` (`xtask/src/archive/tar.rs`)

Para que o tarball gerado em Rust tenha garantia de paridade bit-a-bit e seja 100% determinístico:
1. **Cabeçalho Tar (`tar::Header`)**:
   - `mtime`: clamp estrito em `SOURCE_DATE_EPOCH`;
   - `uid` e `gid`: estritamente `0`;
   - `uname` e `gname`: strings vazias `""`;
   - `mode`:
     - Diretórios: `0o755`;
     - Arquivos sob `/bin/`: `0o755`;
     - Arquivos regulares: `0o644`;
     - Symlinks: `0o777`.
2. **Ordenação de Entradas**:
   - Todas as entradas do diretório fonte são coletadas e ordenadas em **ordem lexicográfica crescente** pelo caminho relativo POSIX (`root/bin/...` antes de `root/lib/...`).
3. **Compressão Gzip (`flate2::write::GzEncoder`)**:
   - `header.mtime`: fixado em `SOURCE_DATE_EPOCH` (evitando que o cabeçalho gzip registre o timestamp do relógio da máquina);
   - `header.filename`: omitido (vazio);
   - `Compression::best()` (nível 9), garantindo reproducibilidade consistente.

### 4.3 Invariantes Canônicos do Gerador de `.zip` (`xtask/src/archive/zip.rs`)

1. **Data e Hora DOS**:
   - Convertido a partir de `SOURCE_DATE_EPOCH`, com limite mínimo em `1980-01-01 00:00:00 UTC` (exigência do formato ZIP/DOS).
2. **Atributos de Arquivo (Unix Permissions)**:
   - `external_attributes`: codificado com permissões Unix nos 16 bits superiores (`(0o755 << 16)` para executáveis em `bin/`, `(0o644 << 16)` para o restante).
3. **Compressão**:
   - `CompressionMethod::Deflated`, nível de compressão 9.

### 4.4 Validador Estrutural de Segurança (`xtask/src/archive/validator.rs`)

O validador é compartilhado entre `tar` e `zip` e rejeita qualquer arquivo que viole as regras de contenção:
- **Prevenção de Zip Slip / Path Traversal**: Rejeita qualquer caminho que contenha `..`, barras iniciais `/` ou barras invertidas `\`.
- **Raiz Única Canônica**: Todas as entradas devem residir estritamente sob o prefixo `arandu-<version>/`.
- **Taxonomia Autorizada**: Somente são permitidos arquivos nos diretórios:
  - `bin/arandu`, `bin/arandu_cli`, `bin/arandu-lsp` (com sufixo `.exe` no Windows);
  - `lib/<target>/libarandu_runtime.a` (ou `.lib` no Windows);
  - `share/arandu/stdlib/**/*.aru`;
  - `BLAKE3SUMS`, `LICENSE-MIT`, `LICENSE-APACHE`, `release-manifest.json`.
- **Validação Semântica do `release-manifest.json`**:
  - `schema: 1`;
  - `version: <versao-da-release>`;
  - `target: <target-triplet>`;
  - `components: ["arandu", "arandu-lsp", "runtime", "stdlib"]`.

### 4.5 Cascatas Nativas nos Scripts Shell de Instalação

Nos arquivos `scripts/install.sh` e `scripts/install-from-tarball.sh`:
- A função de cálculo de SHA-256 passa a usar a cascata nativa universal:

```bash
compute_sha256() {
  local target_file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$target_file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$target_file" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$target_file" | awk '{print $NF}'
  else
    echo "error: no sha256 utility found (requires sha256sum, shasum, or openssl)" >&2
    exit 1
  fi
}
```

- A validação de integridade após extração é delegada ao próprio binário em staging:
```bash
# Executa a verificação dos hashes BLAKE3 de cada arquivo descompactado
"$STAGE/$PACKAGE_ROOT/bin/arandu" hash-file ...
```

---

## 5. Invariantes de Arquitetura — Não Violar

1. **Bootstrap Independente**: O instalador de tarball nunca pode assumir que o sistema possui Python, Cargo ou o repositório Git. O archive externo é validado via SHA-256 padrão de sistema e o conteúdo interno via BLAKE3 pelo próprio binário Arandu extraído.
2. **Determinismo Bit-a-Bit**: O `.tar.gz` ou `.zip` gerado pelo `xtask` deve produzir o mesmo hash BLAKE3 e SHA256 em execuções repetidas sob o mesmo `SOURCE_DATE_EPOCH`.
3. **Isolamento de Erros e Defesa em Profundidade**: O validador rejeita archives com permissões anômalas, symlinks que apontem para fora da raiz ou caminhos não normalizados antes de qualquer arquivo ser copiado para o sistema de destino.
4. **Preservação de Oracles**: Durante o período de teste e validação da RFC, os scripts Python originais devem ser mantidos sob `scripts/oracles/` para testes de paridade comparativa (`xtask test-archive-parity`), garantindo que não haja regressão inadvertida de formato.

---

## 6. Plano de Rollout em 4 Fases

| Fase | Ações Principais | Critério de Aceite |
| :--- | :--- | :--- |
| **Fase 1: Motor `xtask`** | Implementar `xtask/src/archive/` com `tar`, `flate2`, `zip` e subcomandos `package-archive` e `validate-archive`. | `cargo test -p xtask` valida criação de tar e zip sintéticos com paridade contra oracles Python. |
| **Fase 2: Asset Prep** | Implementar `xtask/src/release_assets.rs` (`prepare-release-assets`). | Gera `BLAKE3SUMS`, `SHA256SUMS` e `release-manifest.json` com saída idêntica ao script Python. |
| **Fase 3: Shell & CI** | Atualizar `package-release.sh`, `package-release.ps1`, `install-from-tarball.sh` e `.github/workflows/release.yml`. | Workflows de CI e empacotamento passam sem invocar `python3` para empacotar ou instalar. |
| **Fase 4: CLI Nativo** | Implementar `arandu archive validate` em `arandu_cli` e mover scripts Python para `scripts/oracles/`. | Marco `DIST` concluído; Python completamente erradicado do fluxo de produção. |

---

## 7. Arte Prévia e Referências

- **Cargo / `cargo-dist`**: Utiliza implementações puramente em Rust (`flate2`, `tar`, `zip`) para gerar artefatos reproduzíveis multiplataforma sem dependência de scripts shell/python.
- **Reproducible Builds Project**: [Especificação canônica de timestamps e normalização de metadados em arquivos TAR/ZIP](https://reproducible-builds.org/docs/archives/).
- **Flutter SDK**: Modelo de distribuição autocontida de SDK onde o próprio executável central gerencia sua integridade e atualizações.
