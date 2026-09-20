#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"
OUT_DIR="$WORK/first" bash "$ROOT/scripts/package-release.sh"
OUT_DIR="$WORK/second" bash "$ROOT/scripts/package-release.sh"

FIRST="$(find "$WORK/first" -maxdepth 1 -name '*.tar.gz' -print -quit)"
SECOND="$(find "$WORK/second" -maxdepth 1 -name '*.tar.gz' -print -quit)"
test -n "$FIRST"
test -n "$SECOND"
cmp "$FIRST" "$SECOND"
echo "check-package-reproducibility: ok ($(basename "$FIRST"), epoch=$SOURCE_DATE_EPOCH)"
