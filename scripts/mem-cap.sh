#!/usr/bin/env bash
# Runs a command under a memory cap, so a build or test run that grows too big
# is killed alone (exit 137 or 143) instead of taking the whole WSL session with it
# (CLAUDE.md, Machine limits).
#
# The cap is a systemd scope (cgroup v2): NIARA_MEM_CAP of RAM (default 3G) plus
# NIARA_SWAP_CAP of swap (default 1G), for the command and all its children.
# Prints the enforced limit at the start and the peak memory at the end.
#
# Fails closed: when no capped scope can be created (no systemd user manager,
# e.g. after a WSL crash where WSL logs "Timed out waiting for user session"),
# or the scope's cgroup does not carry the limit, the command is NOT run and
# the script exits 1. Restore the user manager by opening a new WSL terminal
# (a login session starts user@<uid>.service), or once and for all with
# `sudo loginctl enable-linger "$USER"`.
#
# Usage: bash scripts/mem-cap.sh <command> [args...]
#   bash scripts/mem-cap.sh cargo test --no-fail-fast -- --test-threads=1
set -uo pipefail

[[ $# -gt 0 ]] || { echo "usage: $0 <command> [args...]" >&2; exit 2; }

if [[ "${NIARA_MEM_CAP_ACTIVE:-}" == 1 ]]; then
  # Inside the scope: check that the limit is really in place, run, then
  # report the scope's peak.
  cgroup="/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)"
  limit="$(cat "$cgroup/memory.max" 2>/dev/null || echo missing)"
  if [[ ! "$limit" =~ ^[0-9]+$ ]]; then
    echo "mem-cap: the scope's memory.max is '$limit', not a limit; refusing to run" >&2
    exit 1
  fi
  swap="$(cat "$cgroup/memory.swap.max" 2>/dev/null || echo missing)"
  echo "mem-cap: limit $((limit / 1048576)) MB RAM, swap ${swap} bytes ($cgroup)" >&2
  "$@"
  status=$?
  if [[ -r "$cgroup/memory.peak" ]]; then
    echo "mem-cap: peak $(($(cat "$cgroup/memory.peak") / 1048576)) MB" \
      "(cap ${NIARA_MEM_CAP}, swap peak $(($(cat "$cgroup/memory.swap.peak" 2>/dev/null || echo 0) / 1048576)) MB)" >&2
  fi
  exit "$status"
fi

export NIARA_MEM_CAP="${NIARA_MEM_CAP:-3G}"
export NIARA_SWAP_CAP="${NIARA_SWAP_CAP:-1G}"

if ! probe="$(systemd-run --user --scope --quiet -p MemoryMax=1G true 2>&1)"; then
  echo "mem-cap: cannot create a capped scope (systemd-run --user failed: ${probe:-no output})." >&2
  echo "mem-cap: NOT running '$*' without a memory cap. Open a new WSL terminal to start" >&2
  echo "mem-cap: the user manager, or run: sudo loginctl enable-linger \"\$USER\"" >&2
  exit 1
fi

status=0
# OOMPolicy=continue: the kernel OOM-kills the process that broke the cap
# (SIGKILL, exit 137) and the wrapper survives to report it. With the default
# (stop), systemd also SIGTERMs the wrapper and the caller only sees 143.
NIARA_MEM_CAP_ACTIVE=1 systemd-run --user --scope --quiet \
  -p "MemoryMax=${NIARA_MEM_CAP}" -p "MemorySwapMax=${NIARA_SWAP_CAP}" \
  -p OOMPolicy=continue \
  bash "$0" "$@" || status=$?
if [[ $status -eq 137 || $status -eq 143 ]]; then
  echo "mem-cap: killed (exit $status), most likely by the ${NIARA_MEM_CAP} memory cap" >&2
fi
exit "$status"
