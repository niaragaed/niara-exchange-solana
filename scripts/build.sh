#!/usr/bin/env bash
# Build wrapper that verifies artifacts, not just the exit code.
#
# `anchor build` exits non-zero on compilation errors (verified with 1.2.2), but
# a failed or partial build can still leave a stale `.so` / IDL from an earlier
# run that tests would silently load. This script removes the expected
# artifacts, runs the build, and fails unless:
#   - `anchor build` exits 0,
#   - the log contains no compiler error lines,
#   - the program binary and IDL exist again and are newer than the build start.
#
# Usage: scripts/build.sh [extra `anchor build` args]
set -euo pipefail

cd "$(dirname "$0")/.."

PROGRAM=niara_exchange
SO="target/deploy/${PROGRAM}.so"
IDL="target/idl/${PROGRAM}.json"

LOG="$(mktemp)"
MARKER="$(mktemp)"
trap 'rm -f "$LOG" "$MARKER"' EXIT

rm -f "$SO" "$IDL"
touch "$MARKER"

status=0
NO_DNA=1 anchor build "$@" 2>&1 | tee "$LOG" || status=$?

fail() {
  echo "build check FAILED: $*" >&2
  exit 1
}

[[ $status -eq 0 ]] || fail "anchor build exited with status $status"
if grep -qE '^error(\[E[0-9]+\])?:' "$LOG"; then
  fail "compiler errors in build output"
fi
for artifact in "$SO" "$IDL"; do
  [[ -s "$artifact" ]] || fail "$artifact is missing or empty"
  [[ "$artifact" -nt "$MARKER" ]] || fail "$artifact is older than the build start"
done

echo "build check OK: $SO ($(stat -c %s "$SO") bytes), $IDL"
