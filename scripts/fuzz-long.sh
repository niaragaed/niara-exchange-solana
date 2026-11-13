#!/usr/bin/env bash
# Long run of the invariant fuzzer: only tests/test_invariants.rs, with
# PROPTEST_CASES=256 and one test thread (CLAUDE.md, Machine limits).
#
# All cargo output goes to ~/fuzz-256.log (overwritten on every run); the
# terminal only shows the start time, the end time and the result. Follow the
# run from another terminal with:  tail -f ~/fuzz-256.log
#
# This is a heavy process: run nothing else heavy (anchor build, npm install,
# another cargo test, a local validator) while it is running. It runs through
# scripts/mem-cap.sh, like scripts/build.sh: if it grows past the cap it is
# killed alone instead of taking the WSL session with it, and the log ends
# with the peak memory.
#
# It does not build. The tests load target/deploy/niara_exchange.so, so the
# script stops if that file is missing or older than the program sources.
#
# Usage: bash scripts/fuzz-long.sh
set -uo pipefail

cd "$(dirname "$0")/.."

CASES=256
LOG="$HOME/fuzz-256.log"
SO="target/deploy/niara_exchange.so"
SRC="programs/niara_exchange"

if [[ ! -s "$SO" ]]; then
  echo "missing $SO: run 'bash scripts/build.sh' first" >&2
  exit 1
fi
stale="$(find "$SRC/src" "$SRC/Cargo.toml" Cargo.toml Cargo.lock -newer "$SO" -print -quit)"
if [[ -n "$stale" ]]; then
  echo "$SO is older than $stale: run 'bash scripts/build.sh' first" >&2
  exit 1
fi

# A closed terminal must not stop the run; the log file is the record.
trap '' HUP
say() { echo "$@" 2>/dev/null || true; }
stamp() { date '+%Y-%m-%d %H:%M:%S %z'; }

start="$(stamp)"
start_secs=$SECONDS
cmd="PROPTEST_CASES=$CASES bash scripts/mem-cap.sh cargo test --test test_invariants -- --test-threads=1"
{
  echo "start:   $start"
  echo "commit:  $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "command: $cmd"
  echo
} >"$LOG"
say "start: $start"
say "log:   $LOG"

PROPTEST_CASES=$CASES bash scripts/mem-cap.sh \
  cargo test --test test_invariants -- --test-threads=1 >>"$LOG" 2>&1
status=$?

end="$(stamp)"
elapsed=$((SECONDS - start_secs))
if [[ $status -eq 0 ]]; then result="PASSED"; else result="FAILED (exit $status)"; fi
{
  echo
  echo "end:     $end"
  echo "elapsed: $((elapsed / 60)) min $((elapsed % 60)) s"
  echo "result:  $result"
} >>"$LOG"
say "end:   $end ($((elapsed / 60)) min $((elapsed % 60)) s)"
say "result: $result"
exit $status
