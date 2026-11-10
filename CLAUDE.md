# CLAUDE.md — niara-exchange-solana

Solana program (`niara_exchange`, Anchor) for the Niara Exchange: curated
listing, Niara-approved issuance and atomic settlement of tokenized securities.
**Testnet only**: no real assets, no real money, no regulatory authorization.

The design spec is `EXCHANGE-MULTICHAIN-DESIGN.md`. Read it before changing the
program; when behavior changes, update the spec in the same change (or before
it, in its own commit, when asked to do the spec first).

## Stack (pinned — do not bump without an explicit request)

| Component | Version | Where it is pinned |
|---|---|---|
| Anchor CLI + `anchor-lang` / `anchor-spl` | **1.2.2** | `Anchor.toml` `[toolchain] anchor_version`; `=1.2.2` in `programs/niara_exchange/Cargo.toml`; `Cargo.lock` |
| Solana crates | **3.x** (`solana-v3`, Anchor default) | `anchor-spl` features; `solana-v4` is a roadmap item |
| Solana CLI | **4.3.0** (Agave) | not in `[toolchain]` on purpose (it would switch the machine-wide release) |
| Rust | **1.99.1** | `rust-toolchain.toml` (MSRV in `Cargo.toml`: 1.89) |
| Tests | **LiteSVM 0.15** + **proptest 1** | `[dev-dependencies]`; `Cargo.lock` pins LiteSVM's dependency graph |

`Cargo.lock` is committed and must keep resolving `anchor-lang 1.2.2`.

## Build and test

```bash
bash scripts/build.sh              # anchor build + fresh-artifact check (.so and IDL)
cargo test --no-fail-fast          # LiteSVM loads target/deploy/niara_exchange.so
PROPTEST_CASES=256 cargo test --test test_invariants   # longer fuzzing run
```

Always build with `scripts/build.sh` before `cargo test`: tests load the `.so`
from `target/deploy`, and the script fails instead of leaving a stale binary.

## Rules

- **Never commit keypairs** or any secret: no `*-keypair.json`, `id.json`,
  operator keys, `.env` files, RPC URLs with API keys. `target/` (which holds
  the program keypair) stays ignored, and `.gitignore` blocks the usual key
  file names; still review `git status` before every commit.
- **No devnet (or any network) deploy** and no `git push` without an explicit
  request in the current conversation.
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
