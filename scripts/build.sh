#!/usr/bin/env bash
# Build wrapper that verifies artifacts, not just the exit code.
#
# `anchor build` exits non-zero on compilation errors (verified with 1.2.2), but
# a failed or partial build can still leave a stale `.so` / IDL from an earlier
# run that tests would silently load. This script removes the expected
# artifacts, runs the build, and fails unless:
#   - `anchor build` exits 0,
#   - the log contains no compiler error lines,
#   - the program binary, IDL and TypeScript types exist again and are newer
#     than the build start.
#
# The build runs in two steps to stay inside the machine's memory (CLAUDE.md,
# Machine limits):
#   1. `anchor build --no-idl`: the program binary only.
#   2. `anchor idl build -- --lib`: the IDL and the TypeScript types.
# A plain `anchor build` generates the IDL with `cargo test --features
# idl-build` and no target filter, which recompiles and links every
# integration test (13 LiteSVM binaries) just to run the IDL printers that
# live in the library. That step took WSL down twice on 2026-11-12. With
# `--lib` and one cargo job it peaks at about 1.6 GB, and the output is the
# same IDL.
# Both steps run under scripts/mem-cap.sh, so a step that still grows too big
# is killed alone instead of taking the WSL session with it.
#
# Usage: scripts/build.sh [extra `anchor build` args]
set -euo pipefail

cd "$(dirname "$0")/.."

PROGRAM=niara_exchange
SO="target/deploy/${PROGRAM}.so"
IDL="target/idl/${PROGRAM}.json"
TYPES="target/types/${PROGRAM}.ts"

LOG="$(mktemp)"
MARKER="$(mktemp)"
trap 'rm -f "$LOG" "$MARKER"' EXIT

fail() {
  echo "build check FAILED: $*" >&2
  exit 1
}

rm -f "$SO" "$IDL" "$TYPES"
touch "$MARKER"

echo "[1/2] program binary (anchor build --no-idl)"
status=0
NO_DNA=1 bash scripts/mem-cap.sh anchor build --no-idl "$@" 2>&1 | tee "$LOG" || status=$?
[[ $status -eq 0 ]] || fail "anchor build exited with status $status"

echo "[2/2] IDL and TypeScript types (anchor idl build -- --lib)"
status=0
NO_DNA=1 CARGO_BUILD_JOBS=1 bash scripts/mem-cap.sh \
  anchor idl build --out "$IDL" --out-ts "$TYPES" -- --lib 2>&1 | tee -a "$LOG" || status=$?
[[ $status -eq 0 ]] || fail "anchor idl build exited with status $status"

if grep -qE '^error(\[E[0-9]+\])?:' "$LOG"; then
  fail "compiler errors in build output"
fi
for artifact in "$SO" "$IDL" "$TYPES"; do
  [[ -s "$artifact" ]] || fail "$artifact is missing or empty"
  [[ "$artifact" -nt "$MARKER" ]] || fail "$artifact is older than the build start"
done

echo "build check OK: $SO ($(stat -c %s "$SO") bytes), $IDL, $TYPES"
