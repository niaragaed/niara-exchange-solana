# Devnet deploy runbook: `niara_exchange`

**Testnet only.** Solana devnet, test tokens, no real assets, no real money, no
regulatory authorization. The scripts refuse any cluster other than a local
validator or devnet (they check the genesis hash).

Status on 2026-11-12: **nothing has been deployed or sent to devnet yet.**
Everything below was rehearsed against a local validator only: the setup and
demo scripts (`bash scripts/localnet-demo.sh`, §11), and once by hand the CLI
commands of §2 and §4 to §10 (first deploy with the program keypair, `show`,
`dump`, upgrade by address, moving the upgrade authority and back, refilling
the funder), with `--url http://127.0.0.1:8899` in place of `--url devnet`.
The first real run is the first test on devnet itself (faucet, rate limits).

Program ID: `B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v`

## Conventions

- Every command names its cluster and its keypair (`--url devnet --keypair ...`).
  Never rely on the machine-wide Solana CLI config, and do not change it with
  `solana config set`: on this machine it points at devnet with the
  **Register** deployer (`~/.config/solana/niara-deployer.json`), which must
  not be used for this product.
- Paths are WSL paths. `C:\Users\henri\niara-backup\niara-exchange-solana` is
  `/mnt/c/Users/henri/niara-backup/niara-exchange-solana` from WSL.
- One heavy process at a time (CLAUDE.md, Machine limits): build, `cargo test`,
  `npm ci` and the local validator never run together.

```bash
# Used by the commands below. Set them in the terminal you deploy from.
PROGRAM_ID=B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v
DEPLOYER=~/.config/solana/niara-exchange-deployer.json
BACKUP=/mnt/c/Users/henri/niara-backup/niara-exchange-solana
```

## 1. Keys

| Key | File | What it controls | If lost |
|---|---|---|---|
| Program keypair | `target/deploy/niara_exchange-keypair.json` | Fixes the program ID. Signs once, at the first deploy; upgrades do not need it | Before the first deploy: the program ID is lost (new ID, new `declare_id!`). After it: nothing |
| **Deployer** (dedicated to this product) | `~/.config/solana/niara-exchange-deployer.json` | Pays the deploy; **upgrade authority**; `Config.admin` (pause, timelocked changes); the only key that can `initialize_config` | The program can never be upgraded again and the admin functions are gone |
| Role keys, created by `scripts/setup.ts` | `~/.config/niara-exchange/devnet/` | `listing-authority`, `settlement-operator`, `treasury`, `mock-mint-authority` (mints MockUSDT/MockBRL), `demo-funder`, and the two mock mint keypairs | Listing authority, operator and treasury can be replaced by the admin through the 60 s timelock. The mock mint authority cannot: new mock mints and a fee-mint switch would be needed |

The program keypair is already backed up in `$BACKUP/niara_exchange-keypair.json`.

### Create the deployer keypair (once)

```bash
solana-keygen new --outfile ~/.config/solana/niara-exchange-deployer.json
```

It asks for an optional BIP39 passphrase and prints the public key and a
12-word seed phrase **once**. Write the seed phrase (and the passphrase, if
any) on paper. The command refuses to overwrite an existing file; never add
`--force`.

Back it up and check the copy:

```bash
cp "$DEPLOYER" "$BACKUP/niara-exchange-deployer.json"
cmp "$DEPLOYER" "$BACKUP/niara-exchange-deployer.json" && echo "backup identical"
solana-keygen pubkey "$DEPLOYER"                          # the deployer address
solana-keygen pubkey ~/.config/solana/niara-deployer.json  # Register: must be a DIFFERENT address
```

The backup folder is on the same disk as the working copy, so it protects
against a deleted file or a broken WSL, not against a lost disk: the paper
seed phrase is the second copy.

### Back up the role keys (after §8)

```bash
cp -r ~/.config/niara-exchange/devnet "$BACKUP/devnet-keys"
```

None of these files ever goes into the repository (`.gitignore` blocks the
usual names, the scripts refuse a keys folder inside the repo; still review
`git status` before every commit).

## 2. Check the program ID

Run before every deploy and after every build:

```bash
bash scripts/check-program-id.sh
```

It prints public keys only and sends nothing to any network. It compares,
against `declare_id!` in `programs/niara_exchange/src/lib.rs`:

- every `[programs.<cluster>]` entry in `Anchor.toml`;
- the address of `target/deploy/niara_exchange-keypair.json`, and that its
  secret key really signs for that address;
- the same two checks for the backup,
  `C:\Users\henri\niara-backup\niara-exchange-solana\niara_exchange-keypair.json`;
- that the two keypair files are byte-identical;
- the `address` in the built IDL.

It must end with `program ID check OK: B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v`.

Why: `anchor build` silently creates a **new** program keypair when
`target/deploy/niara_exchange-keypair.json` is missing (fresh clone,
`cargo clean`, deleted `target/`). The build succeeds, but a deploy would land
on an address that `declare_id!` does not match. If the check fails, restore
the keypair and run it again; never "fix" it with `anchor keys sync`, which
would rewrite `declare_id!` to the wrong address:

```bash
cp "$BACKUP/niara_exchange-keypair.json" target/deploy/niara_exchange-keypair.json
chmod 600 target/deploy/niara_exchange-keypair.json
```

To check by hand that the backup is the key of the program ID:

```bash
solana-keygen pubkey "$BACKUP/niara_exchange-keypair.json"              # must print $PROGRAM_ID
solana-keygen verify "$PROGRAM_ID" "$BACKUP/niara_exchange-keypair.json" # must print "Verification for public key: ...: Success"
```

## 3. Build and test

```bash
bash scripts/build.sh
cargo test --no-fail-fast -- --test-threads=1
bash scripts/check-program-id.sh
sha256sum target/deploy/niara_exchange.so   # note it: §6 compares the chain against this file
```

Deploy only a binary built from a committed tree (`git status` clean).

## 4. Fund the deployer (by hand, only when you decide to deploy)

Nothing in the repository airdrops. Approximate needs:

| For | SOL |
|---|---|
| Program data rent: `(size of .so + 45 + 128) × 6 960` lamports; 4.998 SOL for today's 717 920-byte binary | ~5.0 |
| Program account rent and deploy fees (about 720 write transactions) | < 0.01 |
| `scripts/setup.ts`: 0.65 to the role keys (0.5 of it to the demo funder) plus rent | ~0.7 |
| **First deploy + setup** | **~5.7; fund 6** |
| Each upgrade: a second ~5 SOL for the buffer, returned when the upgrade lands; the fees stay spent | ~5 free |

Measured on the local validator: the first deploy took 5.0026 SOL from the
deployer, an upgrade with the same binary 0.0036 SOL.

```bash
solana rent $(( $(stat -c %s target/deploy/niara_exchange.so) + 45 )) --url devnet   # exact rent, read-only
solana airdrop 5 "$(solana-keygen pubkey "$DEPLOYER")" --url devnet                  # or https://faucet.solana.com
solana balance "$(solana-keygen pubkey "$DEPLOYER")" --url devnet
```

The devnet faucet is rate-limited and caps each request, so 6 SOL takes more
than one request.

## 5. First deploy

```bash
solana program deploy target/deploy/niara_exchange.so \
  --program-id target/deploy/niara_exchange-keypair.json \
  --keypair "$DEPLOYER" \
  --upgrade-authority "$DEPLOYER" \
  --url devnet
```

- `--program-id` is the keypair **file** here: the first deploy must be signed
  by the program keypair. That is what puts the program at `$PROGRAM_ID`.
- `--keypair` pays; `--upgrade-authority` becomes the upgrade authority. Both
  are the dedicated deployer.
- It must end with `Program Id: B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v`.
- `solana program deploy` is used instead of `anchor deploy` on purpose:
  `anchor deploy` takes the cluster and wallet from `Anchor.toml` (localnet,
  `id.json`) unless overridden and also uploads the IDL; here every input is
  on the command line.

Every deploy starts by printing a 12-word seed phrase for a temporary buffer
account ("Recover the intermediate account's ephemeral keypair file..."). It
controls only that buffer. Keep the terminal open until `Program Id:` appears;
after that the phrase is worthless. If the deploy stops half-way (RPC errors,
rate limits), the SOL is in the buffer, not lost: the same message says how to
resume (`solana-keygen recover`, then `--buffer`). To see and reclaim buffers:

```bash
solana program show --buffers --buffer-authority "$(solana-keygen pubkey "$DEPLOYER")" --url devnet
solana program close --buffers --authority "$DEPLOYER" \
  --recipient "$(solana-keygen pubkey "$DEPLOYER")" --keypair "$DEPLOYER" --url devnet
```

If the public RPC keeps dropping the write transactions, a retry with
`--use-rpc` (not tried here) or a private devnet RPC URL is the usual way out.

## 6. Verify the deployment

```bash
solana program show "$PROGRAM_ID" --url devnet
```

Check in the output:

- `Program Id` is `$PROGRAM_ID`, which is also what
  `solana-keygen pubkey "$BACKUP/niara_exchange-keypair.json"` prints: the
  deployed program sits at the address of the backed-up keypair;
- `Owner` is `BPFLoaderUpgradeab1e11111111111111111111111`;
- `Authority` is the deployer address (`solana-keygen pubkey "$DEPLOYER"`),
  not the Register deployer and not `none`;
- `Data Length` is the size of `target/deploy/niara_exchange.so`.

Then compare the on-chain binary with the local one:

```bash
solana program dump "$PROGRAM_ID" /tmp/niara_exchange.onchain.so --url devnet
cmp --bytes="$(stat -c %s target/deploy/niara_exchange.so)" \
  /tmp/niara_exchange.onchain.so target/deploy/niara_exchange.so && echo "on-chain binary matches"
```

(The dump is zero-padded at the end if the program account was ever extended,
hence `--bytes`.) Record the deploy signature, the slot and the `.so` sha256
in the deploy notes.

## 7. Upgrade authority

- Whoever holds it can replace the program, and with it every rule, at once:
  an upgrade is **not** subject to the admin timelock (spec §3.6). In v1 it is
  a single key, the deployer. Say "timelocked admin" only together with
  "single-key upgrade authority".
- The same key is the only one allowed to call `initialize_config`, once.
- Show it: `solana program show "$PROGRAM_ID" --url devnet` (`Authority`).
- Move it (e.g. to a Squads multisig, spec §9.6). The new authority signs too,
  which protects against a typo:

  ```bash
  solana program set-upgrade-authority "$PROGRAM_ID" \
    --upgrade-authority "$DEPLOYER" \
    --new-upgrade-authority <NEW_AUTHORITY_KEYPAIR> \
    --keypair "$DEPLOYER" --url devnet
  ```

  `Config.admin` does not move with it: that is a separate, timelocked
  `propose_change(Admin)` / `execute_change`.
- Never pass `--final` (to `deploy` or `set-upgrade-authority`): it makes the
  program immutable for good. That is a post-audit production decision.

## 8. Setup (once per deployment)

```bash
npm ci --ignore-scripts          # once per clone; heavy, run it alone
node scripts/setup.ts --cluster devnet
```

`NIARA_DEPLOYER` overrides the deployer path (default `$DEPLOYER` above).
The script checks first that the program is deployed and that the deployer is
its upgrade authority, then:

1. creates the role keypairs in `~/.config/niara-exchange/devnet/` and funds
   them from the deployer (0.05 SOL each to the listing authority, the
   settlement operator and the mock mint authority; `--funder-sol`, default
   0.5, to the demo funder);
2. creates two Niara-controlled test currencies, Token-2022 mints with on-mint
   metadata, 6 decimals, no freeze authority: **"Niara Mock USDT (TEST)"**
   (`mockUSDT`) and **"Niara Mock BRL (TEST)"** (`mockBRL`);
3. `initialize_config`: `fee_mint` = MockUSDT, timelock **60 s**,
   `--fee-bps` (default 50, i.e. 0.5%, which is also the program's maximum) and
   `--cashback-bps` (default 2000, i.e. 20% of
   the fee);
4. `init_fee_vault`;
5. `list_currency` for MockUSDT and MockBRL.

It prints an explorer link for every transaction and writes the public
addresses to `~/.config/niara-exchange/devnet/deployment.json`. It is safe to
run again: steps whose accounts exist are skipped, and a role key is topped up
only when it holds less than half of its amount. `timelock_secs` is fixed at
`initialize_config`; fee, cashback, authorities and the fee mint can change
later through the timelock.

Then back up the role keys (§1).

The line `bigint: Failed to load bindings, pure JS will be used` at start is
harmless: a dependency's optional native module is not built
(`--ignore-scripts`).

## 9. Demo

```bash
node scripts/demo.ts --cluster devnet            # all three flows
node scripts/demo.ts --cluster devnet --flow a   # one of a, b, c
```

| Flow | What it shows |
|---|---|
| a | `Equity`, order-book mode: create mint → `submit_listing` → `approve_listing` → issuance batch (`request_issuance`, `approve_issuance`) → delegate → `settle` → `withdraw_cashback` |
| b | `FixedIncome`, P2P escrow: mint, listing and batch as in (a), then `open_deal` (the holder sells debentures for MockUSDT) → `accept_deal` |
| c | `Currency`, P2P escrow, MockBRL ↔ MockUSDT: `open_deal` (the proposer buys MockBRL and escrows MockUSDT) → `accept_deal` |

Every issuer, seller and buyer is a new keypair created by the run and saved
in `~/.config/niara-exchange/devnet/demo-runs/<time>/`. They are funded by the
demo funder and return their unused SOL at the end. Every transaction prints
its explorer link, and the script checks the resulting balances, fee and
cashback and stops at the first mismatch. A full run is about 40 transactions
and cost the funder 0.049 SOL on the local validator (rent of the accounts it
leaves behind, plus fees). Refill the funder by running setup again or with:

```bash
solana transfer <DEMO_FUNDER_ADDRESS> 0.5 --from "$DEPLOYER" --keypair "$DEPLOYER" --url devnet
```

`NIARA_RPC_URL` points both scripts at another devnet RPC (they still verify
that it is devnet). Keep such a URL out of the repository and out of shell
history if it carries an API key.

## 10. Upgrade

1. §3 (build, tests, program ID check) on a committed tree.
2. Have ~5 SOL free on the deployer for the buffer (it comes back).
3. Deploy with the program **address**; the program keypair is not needed:

   ```bash
   solana program deploy target/deploy/niara_exchange.so \
     --program-id "$PROGRAM_ID" \
     --keypair "$DEPLOYER" \
     --upgrade-authority "$DEPLOYER" \
     --url devnet
   ```

   A larger binary extends the program account automatically; the extra rent
   is paid by the deployer and stays there.
4. §6 again.

An upgrade replaces the code, not the accounts. A change to the layout of
`Config`, `Listing`, `Deal` or any other existing account needs a migration
(or a new program ID and a new setup); plan it before deploying. Error codes
only ever grow at the end (CLAUDE.md).

## 11. Rehearse locally

```bash
bash scripts/localnet-demo.sh
```

Starts a throwaway `solana-test-validator` on 127.0.0.1 with the built program
(upgrade authority = a temporary local deployer), runs setup twice and the
demo, checks that the scripts refuse a wrong cluster, a deployer that is not
the upgrade authority and a keys folder inside the repository, and stops the
validator. Keys and ledger live in a temporary folder that is deleted at the
end. The validator peaked at about 500 MB of memory here. Run it after any
change to the scripts or to the program's instructions, before touching
devnet.

## 12. Do not

- commit a keypair, a `.env` file or an RPC URL with an API key;
- deploy, upgrade or run the scripts on devnet without deciding to, in that
  moment (CLAUDE.md: no network deploy without an explicit request);
- use the Register deployer, or `solana config set`;
- run `anchor keys sync`, or deploy when `scripts/check-program-id.sh` fails;
- pass `--final`.
