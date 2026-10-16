#!/usr/bin/env bash
# Arandu SDK standalone installer.
#
# Usage:
#   curl -sSf https://arandu-lang.dev/install | bash
#   curl -sSf https://raw.githubusercontent.com/arandu-lang/arandu/main/scripts/install.sh | bash
#   ARANDU_VERSION=X.Y.Z[-rc.N] bash install.sh
#
set -euo pipefail

REPO="arandu-lang/arandu"
PREFIX="${PREFIX:-$HOME/.local/arandu}"

echo "==> Arandu installer"

# 1. Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      *)
        echo "error: unsupported Linux architecture: $ARCH" >&2
        exit 1
        ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      *)
        echo "error: unsupported macOS architecture: $ARCH (requires Apple Silicon aarch64)" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "error: unsupported operating system: $OS (on Windows, use install-from-zip.ps1)" >&2
    exit 1
    ;;
esac

# 2. Determine version to install
VERSION="${ARANDU_VERSION:-}"
if [[ -z "$VERSION" ]]; then
  # Follow the stable release channel without pinning an obsolete candidate.
  LATEST_URL="https://github.com/${REPO}/releases/latest"
  if command -v curl >/dev/null 2>&1; then
    RELEASE_URL="$(curl -sSIfL -o /dev/null -w '%{url_effective}' "$LATEST_URL")"
  elif command -v wget >/dev/null 2>&1; then
    RELEASE_URL="$(wget --server-response --spider "$LATEST_URL" 2>&1 | awk '/^[[:space:]]*Location:/ { location=$2 } END { print location }')"
  else
    echo "error: curl or wget is required to discover the latest release" >&2
    exit 1
  fi
  case "$RELEASE_URL" in
    "https://github.com/${REPO}/releases/tag/v"*)
      VERSION="${RELEASE_URL##*/v}"
      ;;
    *)
      echo "error: latest release did not resolve to a versioned Arandu tag" >&2
      exit 1
      ;;
  esac
fi

VERSION="${VERSION#v}"
if [[ ! "$VERSION" =~ ^[0-9][0-9A-Za-z.-]*$ ]]; then
  echo "error: invalid Arandu version" >&2
  exit 1
fi
TAG="v${VERSION}"

ARCHIVE_NAME="arandu-${VERSION}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE_NAME}"
SHA256_URL="${DOWNLOAD_URL}.sha256"

TMPDIR="$(mktemp -d)"
cleanup() {
  # Restore an interrupted replacement before removing temporary files.
  if [[ -n "${BACKUP:-}" && ( -e "$BACKUP" || -L "$BACKUP" ) && ! -e "$VERSION_DIR" && ! -L "$VERSION_DIR" ]]; then
    mv "$BACKUP" "$VERSION_DIR"
  fi
  [[ -z "${PUBLISH_STAGE:-}" ]] || rm -rf "$PUBLISH_STAGE"
  rm -rf "$TMPDIR"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "==> downloading Arandu ${VERSION} (${TARGET})"
if command -v curl >/dev/null 2>&1; then
  curl -sSLf -o "$TMPDIR/$ARCHIVE_NAME" "$DOWNLOAD_URL"
  curl -sSLf -o "$TMPDIR/${ARCHIVE_NAME}.sha256" "$SHA256_URL"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$TMPDIR/$ARCHIVE_NAME" "$DOWNLOAD_URL"
  wget -q -O "$TMPDIR/${ARCHIVE_NAME}.sha256" "$SHA256_URL"
else
  echo "error: curl or wget is required to download Arandu" >&2
  exit 1
fi

echo "==> verifying SHA-256 checksum"
EXPECTED_SHA256="$(awk 'NF { count++; if (count == 1) print $1 } END { if (count != 1) exit 1 }' "$TMPDIR/${ARCHIVE_NAME}.sha256")" || {
  echo "error: malformed SHA-256 sidecar" >&2
  exit 1
}
if [[ ! "$EXPECTED_SHA256" =~ ^[0-9a-fA-F]{64}$ ]]; then
  echo "error: malformed SHA-256 sidecar (expected 64 hexadecimal characters)" >&2
  exit 1
fi
EXPECTED_SHA256="$(printf '%s' "$EXPECTED_SHA256" | tr 'A-F' 'a-f')"
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA256="$(sha256sum "$TMPDIR/$ARCHIVE_NAME" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL_SHA256="$(shasum -a 256 "$TMPDIR/$ARCHIVE_NAME" | awk '{print $1}')"
elif command -v openssl >/dev/null 2>&1; then
  ACTUAL_SHA256="$(openssl dgst -sha256 "$TMPDIR/$ARCHIVE_NAME" | awk '{print $NF}')"
elif command -v python3 >/dev/null 2>&1; then
  ACTUAL_SHA256="$(python3 -c 'import hashlib,sys
digest = hashlib.sha256()
with open(sys.argv[1], "rb") as archive:
    for chunk in iter(lambda: archive.read(1024 * 1024), b""):
        digest.update(chunk)
print(digest.hexdigest())' "$TMPDIR/$ARCHIVE_NAME")"
else
  echo "error: need sha256sum, shasum, openssl, or python3 to verify SHA-256" >&2
  exit 1
fi
ACTUAL_SHA256="$(printf '%s' "$ACTUAL_SHA256" | tr 'A-F' 'a-f')"

if [[ "$EXPECTED_SHA256" != "$ACTUAL_SHA256" ]]; then
  echo "error: SHA-256 checksum mismatch" >&2
  echo "  expected: $EXPECTED_SHA256" >&2
  echo "  actual:   $ACTUAL_SHA256" >&2
  exit 1
fi
echo "    SHA-256 ok ($ACTUAL_SHA256)"

STAGE="$TMPDIR/extracted"
mkdir -p "$STAGE"
tar -xzf "$TMPDIR/$ARCHIVE_NAME" -C "$STAGE"

VERSION_NAME="arandu-${VERSION}"
VERSION_DIR="$PREFIX/$VERSION_NAME"
STAGE_TREE="$STAGE/$VERSION_NAME"

if [[ ! -x "$STAGE_TREE/bin/arandu" ]]; then
  echo "error: corrupt package: binary missing from archive" >&2
  exit 1
fi

# The downloaded archive is authenticated before executing its staged CLI.
# Validate the package identity and internal files before modifying PREFIX.
echo "==> validating archive structure and manifest (native)"
"$STAGE_TREE/bin/arandu" archive validate "$TMPDIR/$ARCHIVE_NAME" \
  --root "$VERSION_NAME" --target "$TARGET" --version "$VERSION"
REPORTED_VERSION="$("$STAGE_TREE/bin/arandu" --version)"
if [[ "$REPORTED_VERSION" != "arandu $VERSION" ]]; then
  echo "error: binary version '$REPORTED_VERSION' does not match package version $VERSION" >&2
  exit 1
fi
if [[ ! -f "$STAGE_TREE/BLAKE3SUMS" ]]; then
  echo "error: archive missing BLAKE3SUMS" >&2
  exit 1
fi
echo "==> verifying in-tree BLAKE3SUMS"
SUM_COUNT=0
while read -r hash path || [[ -n "${hash:-}" ]]; do
  [[ -z "${hash:-}" || "$hash" =~ ^# ]] && continue
  if [[ ! "$hash" =~ ^[0-9a-f]{64}$ || -z "$path" || "$path" == /* || "/$path/" == *"/../"* || "/$path/" == *"/./"* ]]; then
    echo "error: malformed BLAKE3SUMS entry" >&2
    exit 1
  fi
  actual="$("$STAGE_TREE/bin/arandu" hash-file "$STAGE_TREE/$path")"
  if [[ "$hash" != "$actual" ]]; then
    echo "error: BLAKE3SUMS mismatch for $path" >&2
    exit 1
  fi
  SUM_COUNT=$((SUM_COUNT + 1))
done <"$STAGE_TREE/BLAKE3SUMS"
if [[ "$SUM_COUNT" -eq 0 ]]; then
  echo "error: empty BLAKE3SUMS" >&2
  exit 1
fi

echo "==> installing into $PREFIX"
mkdir -p "$PREFIX" "$PREFIX/bin"
# Move onto the destination filesystem before touching an existing version, so
# publishing the prepared tree is a rename even when /tmp is another mount.
PUBLISH_STAGE="$(mktemp -d "$PREFIX/.staging.XXXXXX")"
mv "$STAGE_TREE" "$PUBLISH_STAGE/$VERSION_NAME"

if [[ -e "$VERSION_DIR" || -L "$VERSION_DIR" ]]; then
  BACKUP="${VERSION_DIR}.old.$$"
  mv "$VERSION_DIR" "$BACKUP"
fi
if [[ "${ARANDU_TEST_FAIL_PUBLISH:-0}" == "1" ]] || ! mv "$PUBLISH_STAGE/$VERSION_NAME" "$VERSION_DIR"; then
  [[ -n "${BACKUP:-}" && ( -e "$BACKUP" || -L "$BACKUP" ) ]] && mv "$BACKUP" "$VERSION_DIR"
  echo "error: publish failed; previous installation restored" >&2
  exit 1
fi
[[ -n "${BACKUP:-}" && ( -e "$BACKUP" || -L "$BACKUP" ) ]] && rm -rf "$BACKUP"

ln -sfn "$VERSION_NAME" "$PREFIX/current"
ln -sfn "../current/bin/arandu" "$PREFIX/bin/arandu"
ln -sfn "../current/bin/arandu_cli" "$PREFIX/bin/arandu_cli"
ln -sfn "../current/bin/arandu-lsp" "$PREFIX/bin/arandu-lsp"

echo "==> configuring PATH"
PATH_ENTRY="export PATH=\"$PREFIX/bin:\$PATH\" # Arandu SDK"

add_to_profile() {
  local f="$1"
  if [[ -f "$f" ]]; then
    if grep -Fq "$PREFIX/bin" "$f"; then
      return 0
    fi
    printf '\n%s\n' "$PATH_ENTRY" >>"$f"
    echo "    added to $f"
  fi
}

add_to_profile "$HOME/.zshrc"
add_to_profile "$HOME/.bashrc"
add_to_profile "$HOME/.profile"

echo "==> running doctor"
"$PREFIX/bin/arandu" doctor

echo ""
echo "Arandu ${VERSION} successfully installed under ${PREFIX}!"
echo ""
echo "To get started in your current terminal session, run:"
echo "  export PATH=\"$PREFIX/bin:\$PATH\""
echo "  arandu --version"
