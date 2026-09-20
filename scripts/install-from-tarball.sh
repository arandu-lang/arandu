#!/usr/bin/env bash
# Install from a package-release tarball with bootstrap SHA-256, staged BLAKE3
# verification and atomic publish.
#
# Usage:
#   ./scripts/install-from-tarball.sh dist/arandu-0.0.1-x86_64-unknown-linux-gnu.tar.gz
#   PREFIX=/opt/arandu ./scripts/install-from-tarball.sh ./arandu-….tar.gz
#
# Expects a SHA-256 sidecar for bootstrap without an existing Arandu install:
#   <archive>.sha256        # single hex line, preferred
#   <archive>.sha256sum     # "hex  filename" form
#
# A sidecar is mandatory unless ARANDU_ALLOW_UNVERIFIED=1 is explicit.

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <arandu-VERSION-TARGET.tar.gz>" >&2
  exit 2
fi

ARCHIVE="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
PREFIX="${PREFIX:-$HOME/.local/arandu}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ ! -f "$ARCHIVE" ]]; then
  echo "error: archive not found: $ARCHIVE" >&2
  exit 1
fi

# The extracted binary is preferred once staging exists. Monorepo/PATH fallbacks
# keep local developer packages compatible with the same verifier.
hash_file() {
  local f="$1"
  if [[ -n "${ARANDU_HASH_TOOL:-}" && -x "$ARANDU_HASH_TOOL" ]]; then
    "$ARANDU_HASH_TOOL" hash-file "$f"
  elif [[ -x "$ROOT/target/release/arandu_cli" ]]; then
    "$ROOT/target/release/arandu_cli" hash-file "$f"
  elif [[ -x "$ROOT/target/debug/arandu_cli" ]]; then
    "$ROOT/target/debug/arandu_cli" hash-file "$f"
  elif command -v arandu_cli >/dev/null 2>&1; then
    arandu_cli hash-file "$f"
  elif command -v arandu >/dev/null 2>&1; then
    arandu hash-file "$f"
  else
    echo "error: need arandu_cli hash-file to verify BLAKE3 (build monorepo or install once)" >&2
    exit 1
  fi
}

sha256_file() {
  local f="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$f" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$f" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$f" | awk '{print $NF}'
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$f"
  else
    echo "error: need sha256sum, shasum, openssl, or python3 to verify SHA-256 sidecar" >&2
    exit 1
  fi
}

EXPECTED_SHA256=""
if [[ -f "${ARCHIVE}.sha256" ]]; then
  EXPECTED_SHA256="$(tr -d '[:space:]' <"${ARCHIVE}.sha256")"
elif [[ -f "${ARCHIVE}.sha256sum" ]]; then
  EXPECTED_SHA256="$(awk '{print $1; exit}' "${ARCHIVE}.sha256sum")"
fi

if [[ -n "$EXPECTED_SHA256" ]]; then
  ACTUAL_SHA256="$(sha256_file "$ARCHIVE")"
  if [[ "$EXPECTED_SHA256" != "$ACTUAL_SHA256" ]]; then
    echo "error: SHA-256 mismatch for $(basename "$ARCHIVE")" >&2
    echo "  expected: $EXPECTED_SHA256" >&2
    echo "  actual:   $ACTUAL_SHA256" >&2
    echo "  archive corrupt or tampered — aborting" >&2
    exit 1
  fi
  echo "==> bootstrap SHA-256 ok ($ACTUAL_SHA256)"
else
  if [[ "${ARANDU_ALLOW_UNVERIFIED:-0}" != "1" ]]; then
    echo "error: missing SHA-256 sidecar (set ARANDU_ALLOW_UNVERIFIED=1 only for local development)" >&2
    exit 1
  fi
  echo "warning: unverified development install explicitly enabled" >&2
fi
ARCHIVE_NAME="$(basename "$ARCHIVE")"
if [[ "$ARCHIVE_NAME" =~ ^arandu-(.+)-(x86_64-unknown-linux-gnu|aarch64-apple-darwin)\.tar\.gz$ ]]; then
  PACKAGE_VERSION="${BASH_REMATCH[1]}"
  PACKAGE_TARGET="${BASH_REMATCH[2]}"
  PACKAGE_ROOT="arandu-${PACKAGE_VERSION}"
else
  echo "error: unsupported Arandu archive name: $ARCHIVE_NAME" >&2
  exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

echo "==> extracting (staging)"
tar -xzf "$ARCHIVE" -C "$STAGE"

# Expect single top-level arandu-VERSION/
# Portable: no mapfile/process-substitution (macOS /bin/bash is 3.2).
TOP_COUNT=0
TREE=""
for d in "$STAGE"/*; do
  [[ -d "$d" ]] || continue
  TOP_COUNT=$((TOP_COUNT + 1))
  TREE="$d"
done
if [[ "$TOP_COUNT" -ne 1 || -z "$TREE" ]]; then
  echo "error: archive must contain exactly one top-level directory" >&2
  exit 1
fi
VERSION_NAME="$(basename "$TREE")"
VERSION_DIR="$PREFIX/$VERSION_NAME"

if [[ ! -x "$TREE/bin/arandu_cli" && ! -x "$TREE/bin/arandu" ]]; then
  echo "error: archive missing bin/arandu_cli" >&2
  exit 1
fi
if [[ ! -d "$TREE/share/arandu/stdlib" ]]; then
  echo "error: archive missing share/arandu/stdlib" >&2
  exit 1
fi
if [[ ! -x "$TREE/bin/arandu-lsp" ]]; then
  echo "error: archive missing bin/arandu-lsp" >&2
  exit 1
fi
if [[ ! -f "$TREE/lib/$PACKAGE_TARGET/libarandu_runtime.a" ]]; then
  echo "error: archive missing target runtime library" >&2
  exit 1
fi

echo "==> validating archive structure and manifest (native)"
"$TREE/bin/arandu" archive validate "$ARCHIVE" \
  --root "$PACKAGE_ROOT" --target "$PACKAGE_TARGET" --version "$PACKAGE_VERSION"

# The archive is already authenticated. Use its staged CLI to verify BLAKE3SUMS
# before anything is moved into the installation prefix.
ARANDU_HASH_TOOL="$TREE/bin/arandu"
REPORTED_VERSION="$("$TREE/bin/arandu" --version)"
if [[ "$REPORTED_VERSION" != "arandu $PACKAGE_VERSION" ]]; then
  echo "error: binary version '$REPORTED_VERSION' does not match package version $PACKAGE_VERSION" >&2
  exit 1
fi

# Optional: verify in-tree BLAKE3SUMS against extracted files.
if [[ -f "$TREE/BLAKE3SUMS" ]]; then
  echo "==> verifying in-tree BLAKE3SUMS"
  while read -r hash path; do
    [[ -z "${hash:-}" || "$hash" =~ ^# ]] && continue
    actual="$(hash_file "$TREE/$path")"
    if [[ "$hash" != "$actual" ]]; then
      echo "error: BLAKE3SUMS mismatch for $path" >&2
      echo "  expected $hash" >&2
      echo "  actual   $actual" >&2
      exit 1
    fi
  done <"$TREE/BLAKE3SUMS"
fi

echo "==> atomic publish → $VERSION_DIR"
mkdir -p "$PREFIX" "$PREFIX/bin"
if [[ -e "$VERSION_DIR" || -L "$VERSION_DIR" ]]; then
  BACKUP="${VERSION_DIR}.old.$$"
  rm -rf "$BACKUP"
  mv "$VERSION_DIR" "$BACKUP"
fi
if [[ "${ARANDU_TEST_FAIL_PUBLISH:-0}" == "1" ]] || ! mv "$TREE" "$VERSION_DIR"; then
  [[ -n "${BACKUP:-}" && -e "$BACKUP" ]] && mv "$BACKUP" "$VERSION_DIR"
  echo "error: publish failed; previous installation restored" >&2
  exit 1
fi
[[ -n "${BACKUP:-}" && -e "$BACKUP" ]] && rm -rf "$BACKUP"

ln -sfn "$VERSION_NAME" "$PREFIX/current"
ln -sfn "../current/bin/arandu" "$PREFIX/bin/arandu"
ln -sfn "../current/bin/arandu_cli" "$PREFIX/bin/arandu_cli"

echo "==> doctor"
env -u ARANDU_STDLIB PATH="$PREFIX/bin:/usr/bin:/bin" \
  "$PREFIX/bin/arandu" doctor

echo "installed $VERSION_NAME under $PREFIX"

if [[ "${ARANDU_NO_MODIFY_PATH:-0}" == "1" ]]; then
  echo "PATH unchanged; add: export PATH=\"$PREFIX/bin:\$PATH\""
else
  PROFILE="${ARANDU_PROFILE:-$HOME/.profile}"
  PATH_LINE="export PATH=\"$PREFIX/bin:\$PATH\" # Arandu SDK"
  if [[ -f "$PROFILE" ]] && grep -Fqx "$PATH_LINE" "$PROFILE"; then
    echo "$PREFIX/bin is already configured in $PROFILE"
  else
    printf '\n%s\n' "$PATH_LINE" >>"$PROFILE"
    echo "added $PREFIX/bin to PATH in $PROFILE (open a new shell)"
  fi
fi
