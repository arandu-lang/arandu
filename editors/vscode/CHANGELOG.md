# Changelog

## 0.1.11

- Keep `move || ...` thread closures on the explicit ThreadJob path: the typechecker validates Send captures and rejects the closure with U001.
- Retire socket, reactor, supervisor, and waker ids by generation so a closed handle cannot act on a reused slot.
- Require the outer SHA-256 sidecar and the internal BLAKE3SUMS before an installer publishes.
- Split LLVM codegen, the C task runtime, and structural hashing without changing emitted IR, C, or hash bytes.

## 0.1.10

- Align the extension, language server and SDK release version to 0.1.10.

## 0.1.9

- Release candidate promotion preparation.

## 0.1.8

- Release candidate promotion preparation.

## 0.1.7

- Release candidate promotion preparation.

## 0.1.0-rc.7

- Release candidate promotion preparation.

## 0.1.0-rc.6

- Release candidate promotion preparation.

## 0.1.0-rc.5

- Release candidate promotion preparation.

## 0.1.0-rc.4

- Release candidate promotion preparation.

## 0.1.0-rc.3

- Release candidate promotion preparation.

## 0.1.0-rc.1

- Align the language client with the first installable Arandu SDK release candidate.
- Resolve `arandu-lsp` from the versioned SDK while preserving explicit path overrides.
- Require VS Code 1.92+ (Node.js 20 extension host) and update the transitive
  `brace-expansion` dependency to 5.0.9 after two high-severity DoS advisories.

## 0.0.1

- Initial release of the Arandu VS Code language client.
- Integrated out-of-the-box support for `arandu-lsp`.
- Added TextMate Grammar for basic offline syntax highlighting.
- Added language configuration settings for auto-brackets, auto-closing, and indentation rules.
