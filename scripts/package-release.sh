#!/usr/bin/env bash
# Build a versioned release tarball + BLAKE3 checksum (install gold bar #4).
#
# Output:
#   dist/arandu-$VERSION-$TARGET.tar.gz
#   dist/arandu-$VERSION-$TARGET.tar.gz.blake3   # single-line hex
#
# Tarball root:
#   arandu-$VERSION/
#     bin/arandu_cli, bin/arandu, bin/arandu-lsp
#     bin/arandu → arandu_cli
#     share/arandu/stdlib/
#     release-manifest.json, LICENSE-MIT, LICENSE-APACHE
#     BLAKE3SUMS
#
# Install with: ./scripts/install-from-tarball.sh dist/arandu-….tar.gz

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-}"
TARGET="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
OUT_DIR="${OUT_DIR:-$ROOT/dist}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"

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
    echo "error: need sha256sum, shasum, openssl, or python3 to compute SHA-256" >&2
    exit 1
  fi
}

if [[ -z "$VERSION" ]]; then
  VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/crates/arandu_cli/Cargo.toml" | head -1)"
fi
if [[ ! "$VERSION" =~ ^[0-9][0-9A-Za-z.-]*$ ]]; then
  echo "error: invalid release version: $(printf '%q' "$VERSION")" >&2
  exit 2
fi
if [[ ! "$TARGET" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]]; then
  echo "error: invalid release target: $(printf '%q' "$TARGET")" >&2
  exit 2
fi

NAME="arandu-${VERSION}"
ARCHIVE_BASE="arandu-${VERSION}-${TARGET}"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

echo "==> package-release VERSION=$VERSION TARGET=$TARGET"

cargo build --locked -p arandu_cli -p arandu_lsp -p arandu_runtime --release --manifest-path "$ROOT/Cargo.toml"
BIN="$ROOT/target/release/arandu_cli"
LSP="$ROOT/target/release/arandu-lsp"
RUNTIME="$ROOT/target/release/libarandu_runtime.a"

TREE="$STAGE/$NAME"
mkdir -p "$TREE/bin" "$TREE/lib/$TARGET" "$TREE/share/arandu"
install -m 755 "$BIN" "$TREE/bin/arandu_cli"
install -m 755 "$LSP" "$TREE/bin/arandu-lsp"
install -m 644 "$RUNTIME" "$TREE/lib/$TARGET/libarandu_runtime.a"
ln -sfn arandu_cli "$TREE/bin/arandu"
cp -a "$ROOT/stdlib" "$TREE/share/arandu/stdlib"
install -m 644 "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$TREE/"
cat >"$TREE/release-manifest.json" <<EOF
{
  "schema": 1,
  "version": "$VERSION",
  "target": "$TARGET",
  "components": [
    "arandu",
    "arandu-lsp",
    "runtime",
    "stdlib"
  ],
  "archive": "tar.gz"
}
EOF

(
  cd "$TREE"
  # shellcheck disable=SC2044
  for f in LICENSE-APACHE LICENSE-MIT bin/arandu_cli bin/arandu-lsp "lib/$TARGET/libarandu_runtime.a" release-manifest.json $(find share/arandu/stdlib -type f -name '*.aru' | sort); do
    hash="$("$BIN" hash-file "$TREE/$f")"
    printf '%s  %s\n' "$hash" "$f"
  done
) >"$TREE/BLAKE3SUMS"

mkdir -p "$OUT_DIR"
TAR="$OUT_DIR/${ARCHIVE_BASE}.tar.gz"
cargo run --locked -p xtask --manifest-path "$ROOT/Cargo.toml" -- package-archive \
  --source "$TREE" \
  --output "$TAR" \
  --epoch "$SOURCE_DATE_EPOCH"

cargo run --locked -p xtask --manifest-path "$ROOT/Cargo.toml" -- validate-archive \
  "$TAR" \
  --root "$NAME" \
  --target "$TARGET" \
  --version "$VERSION"

# Tarball integrity (BLAKE3 of the archive bytes).
HASH="$("$BIN" hash-file "$TAR")"
printf '%s\n' "$HASH" >"${TAR}.blake3"
# Also a "hash  filename" form for convenience.
printf '%s  %s\n' "$HASH" "$(basename "$TAR")" >"${TAR}.blake3sum"
SHA256="$(sha256_file "$TAR")"
printf '%s\n' "$SHA256" >"${TAR}.sha256"
printf '%s  %s\n' "$SHA256" "$(basename "$TAR")" >"${TAR}.sha256sum"

echo "==> wrote $TAR"
echo "    blake3 $HASH"
echo "    sidecar ${TAR}.blake3"
