#!/usr/bin/env bash
# Rehearsal of the devnet setup and demo against a throwaway local validator.
# Nothing here touches devnet or any other network.
#
# Starts solana-test-validator on 127.0.0.1 with the built program loaded as an
# upgradeable program (its upgrade authority is a throwaway local deployer, as
# initialize_config requires), then runs:
#   scripts/setup.ts   (twice: the second run must find everything in place)
#   scripts/demo.ts    (flows a, b and c)
#   scripts/demo-refusals.ts (six on-chain refusals, each asserted by its exact
#                       Anchor error; takes about 2 minutes: it waits for a quote to expire)
#   the safety checks  (wrong cluster, wrong deployer and a keys folder inside
#                       the repository must all be refused)
# and stops the validator. The ledger and every key live in a temporary folder
# that is deleted at the end.
#
# The validator is a heavy process (CLAUDE.md, Machine limits): do not run a
# build or cargo test at the same time.
#
# Usage: bash scripts/localnet-demo.sh [extra scripts/demo.ts args, e.g. --flow a]
set -euo pipefail

cd "$(dirname "$0")/.."

PROGRAM=niara_exchange
SO="target/deploy/${PROGRAM}.so"
IDL="target/idl/${PROGRAM}.json"
RPC_PORT=8899
RPC="http://127.0.0.1:${RPC_PORT}"

fail() {
  echo "localnet demo FAILED: $*" >&2
  exit 1
}

[[ -s "$SO" && -s "$IDL" ]] || fail "$SO or $IDL is missing: run 'bash scripts/build.sh' first"
[[ -d node_modules ]] || fail "node_modules is missing: run 'npm ci --ignore-scripts' first"
if curl -s -o /dev/null --max-time 2 "$RPC"; then
  fail "something already answers on $RPC; stop it first"
fi

PROGRAM_ID="$(sed -n 's/^  "address": "\([^"]*\)",\{0,1\}$/\1/p' "$IDL" | head -n 1)"
[[ -n "$PROGRAM_ID" ]] || fail "could not read the program address from $IDL"

WORK="$(mktemp -d)"
VALIDATOR_PID=""
cleanup() {
  if [[ -n "$VALIDATOR_PID" ]] && kill -0 "$VALIDATOR_PID" 2>/dev/null; then
    kill "$VALIDATOR_PID" 2>/dev/null || true
    wait "$VALIDATOR_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# The scripts read only these: never the Solana CLI config, never devnet.
unset NIARA_RPC_URL
export NIARA_KEYS_DIR="$WORK/keys"
export NIARA_DEPLOYER="$WORK/keys/deployer.json"
mkdir -m 700 "$NIARA_KEYS_DIR"
solana-keygen new --no-bip39-passphrase --silent --outfile "$NIARA_DEPLOYER"
DEPLOYER="$(solana-keygen pubkey "$NIARA_DEPLOYER")"

echo "starting solana-test-validator (program $PROGRAM_ID, upgrade authority $DEPLOYER)"
solana-test-validator \
  --ledger "$WORK/ledger" \
  --reset \
  --quiet \
  --bind-address 127.0.0.1 \
  --rpc-port "$RPC_PORT" \
  --mint "$DEPLOYER" \
  --upgradeable-program "$PROGRAM_ID" "$SO" "$DEPLOYER" \
  >"$WORK/validator.out" 2>&1 &
VALIDATOR_PID=$!

health='{"jsonrpc":"2.0","id":1,"method":"getHealth"}'
for _ in $(seq 1 90); do
  kill -0 "$VALIDATOR_PID" 2>/dev/null || {
    tail -n 20 "$WORK/validator.out" >&2
    fail "the validator exited during start-up"
  }
  if curl -s --max-time 2 -H 'Content-Type: application/json' -d "$health" "$RPC" | grep -q '"result":"ok"'; then
    ready=1
    break
  fi
  sleep 1
done
[[ "${ready:-0}" -eq 1 ]] || fail "the validator did not become healthy in 90 s"

echo
echo "##### setup (first run) #####"
node scripts/setup.ts --cluster localnet
echo
echo "##### setup (second run: must change nothing) #####"
node scripts/setup.ts --cluster localnet
echo
echo "##### demo #####"
node scripts/demo.ts --cluster localnet "$@"
echo
echo "##### on-chain refusals #####"
node scripts/demo-refusals.ts --cluster localnet

echo
echo "##### safety checks (each command below must be refused) #####"
refused() { # label, expected message, command...
  local label="$1" expected="$2" output
  shift 2
  if output="$("$@" 2>&1)"; then
    fail "$label: the command succeeded"
  fi
  grep -qF -- "$expected" <<<"$output" || fail "$label: refused, but not with \"$expected\": $output"
  echo "  ok: $label"
}
refused "--cluster devnet against a non-devnet RPC" "the RPC is not devnet" \
  env NIARA_RPC_URL="$RPC" node scripts/setup.ts --cluster devnet
refused "demo with --cluster devnet against a non-devnet RPC" "the RPC is not devnet" \
  env NIARA_RPC_URL="$RPC" node scripts/demo.ts --cluster devnet
refused "--cluster localnet with a remote RPC URL" "needs a local RPC URL" \
  env NIARA_RPC_URL="https://api.devnet.solana.com" node scripts/setup.ts --cluster localnet
solana-keygen new --no-bip39-passphrase --silent --outfile "$WORK/other-deployer.json"
refused "a deployer that is not the upgrade authority" "not the deployer" \
  env NIARA_DEPLOYER="$WORK/other-deployer.json" node scripts/setup.ts --cluster localnet
refused "a keys folder inside the repository" "inside the repository" \
  env NIARA_KEYS_DIR="$PWD/keys-inside-repo" node scripts/setup.ts --cluster localnet
[[ ! -e keys-inside-repo ]] || fail "the refused run created keys-inside-repo/"

echo
echo "localnet demo OK (validator peak memory: $(awk '/VmHWM/ {print int($2 / 1024) " MB"}' "/proc/$VALIDATOR_PID/status" 2>/dev/null || echo unknown))"
