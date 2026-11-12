# CLAUDE.md — niara-exchange-solana

Solana program (`niara_exchange`, Anchor) for the Niara Exchange: curated
listing, Niara-approved issuance and atomic settlement of tokenized securities.
**Testnet only**: no real assets, no real money, no regulatory authorization.

The design spec is `EXCHANGE-MULTICHAIN-DESIGN.md`. Read it before changing the
program; when behavior changes, update the spec in the same change (or before
it, in its own commit, when asked to do the spec first).

This repo owns `EXCHANGE-MULTICHAIN-DESIGN.md`. The EVM repo only receives
copies; it proposes spec edits in its reports.

## Stack (pinned — do not bump without an explicit request)

| Component | Version | Where it is pinned |
|---|---|---|
| Anchor CLI + `anchor-lang` / `anchor-spl` | **1.2.2** | `Anchor.toml` `[toolchain] anchor_version`; `=1.2.2` in `programs/niara_exchange/Cargo.toml`; `Cargo.lock` |
| Solana crates | **3.x** (`solana-v3`, Anchor default) | `anchor-spl` features; `solana-v4` is a roadmap item |
| Solana CLI | **4.3.0** (Agave) | not in `[toolchain]` on purpose (it would switch the machine-wide release) |
| Rust | **1.99.1** | `rust-toolchain.toml` (MSRV in `Cargo.toml`: 1.89) |
| Tests | **LiteSVM 0.15** + **proptest 1** | `[dev-dependencies]`; `Cargo.lock` pins LiteSVM's dependency graph |
| Scripts (setup, demo) | **Node ≥ 22.18** (runs `.ts` directly), **`@anchor-lang/core` 1.2.2**, `@solana/web3.js` 1.98.4, `@solana/spl-token` 0.4.15 | exact versions in `package.json`; `package-lock.json` is committed |

`Cargo.lock` is committed and must keep resolving `anchor-lang 1.2.2`.

## Build and test

```bash
bash scripts/build.sh              # anchor build + fresh-artifact check (.so and IDL)
cargo test --no-fail-fast          # LiteSVM loads target/deploy/niara_exchange.so
PROPTEST_CASES=256 cargo test --test test_invariants   # longer fuzzing run
```

Always build with `scripts/build.sh` before `cargo test`: tests load the `.so`
from `target/deploy`, and the script fails instead of leaving a stale binary.

## Devnet preparation

Runbook: `docs/devnet-deploy.md` (keys, program ID check, deploy, upgrade
authority, setup, demo). Nothing is deployed without an explicit request.

```bash
bash scripts/check-program-id.sh   # declare_id!, Anchor.toml, keypair, backup and IDL agree
npm ci --ignore-scripts            # once per clone
npm run typecheck                  # needs a build: the scripts import target/types
bash scripts/localnet-demo.sh      # setup + demo + safety checks on a throwaway local validator
```

- `scripts/setup.ts` and `scripts/demo.ts` take the cluster from `--cluster`
  (default `localnet`) and verify it by genesis hash. They never read the
  Solana CLI config (which points at devnet with the Register deployer) and
  never airdrop.
- Keys live outside the repo, in `~/.config/niara-exchange/<cluster>/`; the
  scripts refuse a keys folder inside it.
- When an instruction's accounts or arguments change, update the scripts and
  run `scripts/localnet-demo.sh` before any devnet run.
- `npm ci`, the typecheck and the local validator are heavy processes too
  (see below).

## Machine limits

The dev machine has 8 GB RAM and WSL gets about 4.8 GB. Going over it gets the
whole WSL session killed by the OOM killer (exit 137).

- **One heavy process at a time**: never run `npm install`, `anchor build`
  (`scripts/build.sh`) and `cargo test` concurrently, and never in the
  background. Start one, wait for it to finish, then start the next.
- `cargo test` always with `--test-threads=1`
  (`cargo test ... -- --test-threads=1`).
- cargo `jobs = 2` (set in `~/.cargo/config.toml`); do not raise it.
- rust-analyzer is disabled on purpose; do not re-enable it.

## Rules

- **Never commit keypairs** or any secret: no `*-keypair.json`, `id.json`,
  operator keys, `.env` files, RPC URLs with API keys. `target/` (which holds
  the program keypair) stays ignored, and `.gitignore` blocks the usual key
  file names; still review `git status` before every commit.
- **No devnet (or any network) deploy** and no `git push` without an explicit
  request in the current conversation.
- **Never use `git checkout`, `git restore` or `git reset` on uncommitted
  work**: they discard it with no way back. Commit it or `git stash` it
  first.
- **Every instruction needs tests**: the success path, every authorization
  check, every `require!`/constraint failure, and its pause behavior. New
  accounting must also be covered by the invariant fuzzer
  (`tests/test_invariants.rs`) when it touches balances or supply.
- Keep the security checklist in spec §3.8: typed accounts, seeds + stored
  bumps, `has_one`, checked math, no `init_if_needed`, `emit_cpi!` events.
- Error codes are part of the client contract: append new `ExchangeError`
  variants at the end, never reorder or remove.
- **English** for code, comments, docs and commit messages.

## Commits

- Identity: `Niara <niaragaed@gmail.com>` (set in this repo's git config).
- **No AI attribution**: no `Co-Authored-By` trailer, no "Generated with"
  line, in commits or PR descriptions.
- One commit per requested step; conventional prefixes (`feat(program):`,
  `test:`, `docs:`, `build:`, `chore:`).
