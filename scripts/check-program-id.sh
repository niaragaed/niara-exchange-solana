#!/usr/bin/env bash
# Checks that every place naming the program ID agrees with the backed-up
# program keypair. Run it before every deploy (docs/devnet-deploy.md §2).
#
# Why: `anchor build` silently creates a NEW target/deploy/<program>-keypair.json
# when the file is missing (fresh clone, `cargo clean`, deleted target/). The
# build still succeeds, but that keypair has a different address, so a deploy
# would land on a program ID that `declare_id!` does not match.
#
# Prints public keys only, never key material. Sends nothing to any network.
#
# Usage: scripts/check-program-id.sh
#   NIARA_BACKUP_DIR  backup folder (default: the Windows backup, seen from WSL)
set -euo pipefail

cd "$(dirname "$0")/.."

PROGRAM=niara_exchange
BACKUP_DIR="${NIARA_BACKUP_DIR:-/mnt/c/Users/henri/niara-backup/niara-exchange-solana}"
BACKUP="$BACKUP_DIR/${PROGRAM}-keypair.json"
LOCAL="target/deploy/${PROGRAM}-keypair.json"
IDL="target/idl/${PROGRAM}.json"

failures=0
ok() { printf '  ok    %-34s %s\n' "$1" "$2"; }
bad() {
  printf '  FAIL  %-34s %s\n' "$1" "$2" >&2
  failures=$((failures + 1))
}
check() { # label, value
  if [[ "$2" == "$EXPECTED" ]]; then ok "$1" "$2"; else bad "$1" "${2:-<missing>} (expected $EXPECTED)"; fi
}

# The source of truth is the address compiled into the program.
EXPECTED="$(sed -n 's/^declare_id!("\([1-9A-HJ-NP-Za-km-z]\{32,44\}\)");.*/\1/p' \
  "programs/${PROGRAM}/src/lib.rs")"
[[ -n "$EXPECTED" ]] || { echo "could not read declare_id! from lib.rs" >&2; exit 1; }
echo "declare_id! (programs/${PROGRAM}/src/lib.rs): $EXPECTED"

# Anchor.toml: every [programs.<cluster>] entry.
while IFS= read -r line; do
  cluster="${line%%|*}"
  check "Anchor.toml [programs.${cluster}]" "${line#*|}"
done < <(awk -v program="$PROGRAM" '
  /^\[programs\./ { cluster = $0; gsub(/^\[programs\.|\]$/, "", cluster); next }
  /^\[/ { cluster = "" }
  cluster != "" && $1 == program { gsub(/"/, "", $3); print cluster "|" $3 }
' Anchor.toml)

# Keypairs: the address they hold, and proof that the secret key matches it.
check_keypair() { # label, path
  local label="$1" path="$2" pubkey
  if [[ ! -f "$path" ]]; then
    bad "$label" "not found: $path"
    return
  fi
  pubkey="$(solana-keygen pubkey "$path")"
  check "$label" "$pubkey"
  if solana-keygen verify "$EXPECTED" "$path" >/dev/null 2>&1; then
    ok "$label signs for it" "yes"
  else
    bad "$label signs for it" "no: the secret key does not match $EXPECTED"
  fi
}
check_keypair "target/deploy keypair" "$LOCAL"
check_keypair "backup keypair" "$BACKUP"

if [[ -f "$LOCAL" && -f "$BACKUP" ]]; then
  if cmp -s "$LOCAL" "$BACKUP"; then
    ok "target/deploy == backup (bytes)" "identical"
  else
    bad "target/deploy == backup (bytes)" "files differ"
  fi
fi

# IDL, when built: the address clients will call.
if [[ -f "$IDL" ]]; then
  check "IDL address ($IDL)" \
    "$(sed -n 's/^  "address": "\([^"]*\)",\{0,1\}$/\1/p' "$IDL" | head -n 1)"
else
  echo "  skip  IDL not built yet ($IDL); run scripts/build.sh"
fi

if [[ $failures -gt 0 ]]; then
  echo "program ID check FAILED: $failures mismatch(es). Do not deploy." >&2
  echo "Restore the keypair: cp \"$BACKUP\" \"$LOCAL\" && chmod 600 \"$LOCAL\"" >&2
  exit 1
fi
echo "program ID check OK: $EXPECTED"
