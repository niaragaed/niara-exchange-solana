#!/usr/bin/env bash
# Runs a command under a memory cap, so a build or test run that grows too big
# is killed alone (exit 137 or 143) instead of taking the whole WSL session with it
# (CLAUDE.md, Machine limits).
#
# The cap is a systemd scope (cgroup v2): NIARA_MEM_CAP of RAM (default 3G) plus
# NIARA_SWAP_CAP of swap (default 1G), for the command and all its children.
# Prints the peak memory at the end. Where `systemd-run --user` is not
# available the command runs without a cap, with a warning.
#
# Usage: bash scripts/mem-cap.sh <command> [args...]
#   bash scripts/mem-cap.sh cargo test --no-fail-fast -- --test-threads=1
set -uo pipefail

[[ $# -gt 0 ]] || { echo "usage: $0 <command> [args...]" >&2; exit 2; }

if [[ "${NIARA_MEM_CAP_ACTIVE:-}" == 1 ]]; then
  # Inside the scope: run, then report the scope's peak.
  "$@"
  status=$?
  cgroup="/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)"
  if [[ -r "$cgroup/memory.peak" ]]; then
    echo "mem-cap: peak $(($(cat "$cgroup/memory.peak") / 1048576)) MB" \
      "(cap ${NIARA_MEM_CAP}, swap peak $(($(cat "$cgroup/memory.swap.peak" 2>/dev/null || echo 0) / 1048576)) MB)" >&2
  fi
  exit "$status"
fi

export NIARA_MEM_CAP="${NIARA_MEM_CAP:-3G}"
export NIARA_SWAP_CAP="${NIARA_SWAP_CAP:-1G}"

if ! systemd-run --user --scope --quiet -p MemoryMax=1G true 2>/dev/null; then
  echo "mem-cap: systemd-run --user is not available; running WITHOUT a memory cap" >&2
  exec "$@"
fi

status=0
NIARA_MEM_CAP_ACTIVE=1 systemd-run --user --scope --quiet \
  -p "MemoryMax=${NIARA_MEM_CAP}" -p "MemorySwapMax=${NIARA_SWAP_CAP}" \
  bash "$0" "$@" || status=$?
if [[ $status -eq 137 || $status -eq 143 ]]; then
  echo "mem-cap: killed (exit $status), most likely by the ${NIARA_MEM_CAP} memory cap" >&2
fi
exit "$status"
