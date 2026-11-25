#!/usr/bin/env bash
# Regression gate: diagnostic-related test output must not depend on harness
# parallelism (RFC A1 finalize/determinism).
#
# Runs the same package tests with --test-threads=1 and --test-threads=N and
# requires identical exit status + normalized stdout/stderr.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PKG="${1:-arandu_typeck}"
THREADS_N="${2:-8}"
SUITE=(--lib)
if [[ "${3:-}" == --test ]]; then
  if [[ -z "${4:-}" ]]; then
    echo "error: --test requires a test target name" >&2
    exit 2
  fi
  SUITE=(--test "$4")
elif [[ -n "${3:-}" ]]; then
  echo "error: unknown argument '$3' (expected --test <name>)" >&2
  exit 2
fi
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

normalize() {
  # Drop cargo progress / timing / ANSI noise that differs across runs.
  # CARGO_TERM_COLOR=always (CI) injects CSI sequences around "ok"/"FAILED".
  # Collapse suite summaries to fixed tokens so "finished in Xs" never diffs.
  sed -E 's/\x1B\[[0-9;]*[A-Za-z]//g' \
    | { grep -E '^(test result:|error\[|error:)' || true; } \
    | sed -E \
      -e 's/test result: ok\..*/test result: ok./' \
      -e 's/test result: FAILED\..*/test result: FAILED./' \
    | sort -u
}

echo "==> determinism: cargo test --locked -p ${PKG} ${SUITE[*]} -- --test-threads=1"
set +e
cargo test --locked -p "$PKG" "${SUITE[@]}" -- --test-threads=1 --quiet >"$TMP/t1.raw" 2>&1
EC1=$?
set -e
normalize <"$TMP/t1.raw" >"$TMP/t1.norm"

echo "==> determinism: cargo test --locked -p ${PKG} ${SUITE[*]} -- --test-threads=${THREADS_N}"
set +e
cargo test --locked -p "$PKG" "${SUITE[@]}" -- --test-threads="$THREADS_N" --quiet >"$TMP/tN.raw" 2>&1
ECN=$?
set -e
normalize <"$TMP/tN.raw" >"$TMP/tN.norm"

if [[ "$EC1" -ne 0 || "$ECN" -ne 0 ]]; then
  echo "error: test package failed (threads=1 exit=$EC1, threads=$THREADS_N exit=$ECN)" >&2
  tail -40 "$TMP/t1.raw" >&2 || true
  tail -40 "$TMP/tN.raw" >&2 || true
  exit 1
fi

if ! diff -u "$TMP/t1.norm" "$TMP/tN.norm" >"$TMP/diff.txt"; then
  echo "error: diagnostic/test output diverged under different --test-threads" >&2
  cat "$TMP/diff.txt" >&2
  exit 1
fi

echo "check-diag-determinism: ok (${PKG}, threads 1 vs ${THREADS_N})"
