# Arandu LSP and VS Code support matrix (v0.1)

This document is the support contract for the published 0.1.x scope, exercised by the [0.1.10 release](releases/0.1.10.md).
“Supported” means that the native SDK archive is installed into an empty prefix,
the packaged VSIX is installed through the VS Code CLI, and the Extension Host
exercises the installed `arandu-lsp`. A successful build on another target does
not extend this matrix.

## Visão Geral e Contexto

Esta matriz é o contrato público do servidor de linguagem e da extensão VS
Code nos hosts realmente exercitados pelo SDK.

## Detalhes Técnicos da Implementação

### Supported desktop hosts

| Host | SDK artifact | VS Code | Evidence |
|---|---|---|---|
| Windows x86-64 | `.zip`, MSVC | Desktop 1.92+ | native release runner, isolated SDK smoke, installed VSIX Extension Host |
| Linux x86-64 | `.tar.gz`, glibc | Desktop 1.92+ | native release runner, isolated SDK smoke, installed VSIX under Xvfb |
| macOS Apple Silicon | `.tar.gz`, ARM64 | Desktop 1.92+ | native release runner, isolated SDK smoke, installed VSIX Extension Host |

Not currently supported: Windows ARM64/x86, Linux ARM64/x86, Intel macOS binary
archives, musl Linux, VS Code for the Web, remote extension hosts, VSCodium and
other LSP editors as product integrations. The protocol server may work in some
of these environments, but they have no Gold claim until a native gate exists.
There is no MSI, PKG, DEB or RPM installer in `0.1`; the versioned archive
installers are the supported SDK delivery mechanism.

### Advertised LSP capabilities

| Area | Capability | Status and limitation |
|---|---|---|
| Text | incremental sync, UTF-16 | supported; UTF-8/UTF-32 negotiation is not advertised |
| Diagnostics | push diagnostics, quick fixes | supported; no workspace/pull diagnostics |
| Navigation | definition, type definition, references, document highlight | supported; highlights do not yet distinguish read/write |
| Structure | document/workspace symbols, folding, selection range | supported; workspace symbols use the progressively discovered index |
| IntelliSense | completion, hover, signature help | supported; no auto-import and no completion resolve phase |
| Refactoring | prepare rename and multi-file rename | supported with lexical/conflict validation |
| Presentation | full/range semantic tokens | supported; no semantic-token delta protocol |
| Formatting | whole-document and range formatting | supported and canonical; no on-type formatting |
| Hints | inlay hints | supported; no resolve phase |
| Editing | code actions | structured quick fixes only |
| Workspace | create/rename/delete notifications for `*.aru` | supported; one package root is selected deterministically in multi-root windows |
| Lifecycle | progress, status, logs, bounded crash restart | supported by the VS Code extension |

Not advertised in the current scope: declaration/implementation requests,
call hierarchy, type hierarchy, CodeLens, document links, document
colors, linked editing, inline values, execute-command, notebooks and file
operation “will” requests.

### Latency and verification scope

Item-level cutoff is tested; it does not promise universal sub-10ms responses.
The initialize/interactive budgets and ignored endurance/performance campaigns
are distinct. A passing installed VSIX smoke does not run every ignored campaign.
The reference implementation of advertised capabilities is
[`capabilities.rs`](../crates/arandu_lsp/src/capabilities.rs), with stdio and
Extension Host regressions. Keep this matrix synchronized when capabilities change.

### Verification

- `cargo test -p arandu_lsp --locked` freezes the initialize response and stdio
  behavior, including Unicode, cancellation, progress and lifecycle.
- `npm test` runs lint, unit tests and the development Extension Host on the
  minimum VS Code version.
- `npm run test:installed` packages a VSIX, installs it with the VS Code CLI in
  an empty extensions directory and runs the same editor contract.
- `.github/workflows/release.yml` runs the installed VSIX against the installed
  public SDK layout on every supported native host.

References: [LSP 3.18 specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.18/specification/),
[VS Code extension testing](https://code.visualstudio.com/api/working-with-extensions/testing-extension),
[VS Code extension CI](https://code.visualstudio.com/api/working-with-extensions/continuous-integration),
[installing a VSIX](https://code.visualstudio.com/docs/configure/extensions/extension-marketplace#_install-from-a-vsix).

## PONTOS DE MELHORIA (O que não está no roadmap)

Uma capability anunciada exige teste stdio/Extension Host; compilação isolada
ou teste TypeScript unitário não basta para promovê-la.

## Futuro e Próximos Passos

Ampliar hosts/capabilities apenas quando a mesma cadeia instalada, incluindo
VSIX e `arandu-lsp`, passar na matriz de release.
