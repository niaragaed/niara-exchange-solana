# Devnet deploy runbook: `niara_exchange`

**Testnet only.** Solana devnet, test tokens, no real assets, no real money, no
regulatory authorization. The scripts refuse any cluster other than a local
validator or devnet (they check the genesis hash).

Status on 2026-11-14: **the program is deployed and upgraded on devnet, setup
and the demo have run** (record below). The 2026-11-13 deploy was followed on
2026-11-14 by the upgrade with the on-chain listing fee, the setup (fee 50 bps,
cashback 2000 bps) and the demo of the three flows. Before the deploy,
everything was rehearsed against a local validator: the setup and demo scripts
(`bash scripts/localnet-demo.sh`, §11), and once by hand the CLI commands of
§2 and §4 to §10, with `--url http://127.0.0.1:8899` in place of
`--url devnet`.

Program ID: `B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v`

## Devnet deployment record

All links are Solana Explorer with `cluster=devnet`.

### Program deploy (2026-11-13)

| Item | Value |
|---|---|
| Program ID | [`B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v`](https://explorer.solana.com/address/B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v?cluster=devnet) |
| ProgramData | [`8fVRp3gwLVrxE4peeyWQj3iBZw6Wccs3H16iCsEJPVWN`](https://explorer.solana.com/address/8fVRp3gwLVrxE4peeyWQj3iBZw6Wccs3H16iCsEJPVWN?cluster=devnet) |
| Upgrade authority (dedicated deployer, not the Register's) | [`J8iiiosBVqFYy7WMryvV9eweaYM4Xat5SYZGmDynJARz`](https://explorer.solana.com/address/J8iiiosBVqFYy7WMryvV9eweaYM4Xat5SYZGmDynJARz?cluster=devnet) |
| Deploy transaction | [`L28KvPeSZa9a2tW14Q4qTH1MGbZKuGeRTwzF95WHxnYLFxe8tmQugEkQT3B5uDQEtH16TRW7anWLzAoDBvrURWt`](https://explorer.solana.com/tx/L28KvPeSZa9a2tW14Q4qTH1MGbZKuGeRTwzF95WHxnYLFxe8tmQugEkQT3B5uDQEtH16TRW7anWLzAoDBvrURWt?cluster=devnet) |
| Slot, time | 521888257, 2026-11-13 13:43:45 -03:00 |
| Source | commit `c3e238d` (program sources as of `a834178`, two-step admin transfer) |
| Binary | 730 680 bytes, sha256 `8bcf23a65173f066021691350fcca124f39e9d61fda532600effa1ec36e36fa7` |
| Checks (§6) | `program show`: owner BPFLoaderUpgradeable, authority = deployer, data length 730 680; `program dump` matches the local `.so` byte for byte; no buffer left |
| Cost | 3.7172 SOL from the deployer (12 → 8.2828): 3.7127 program data rent, the rest program account rent and fees |

### Program upgrade: on-chain listing fee (2026-11-14)

Upgrade of the program deployed on 2026-11-13, with the listing fee of spec
§3.13 and the Approved-only issuance batches. It landed before the first
setup, so no account needed a migration.

| Item | Value |
|---|---|
| Program ID | [`B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v`](https://explorer.solana.com/address/B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v?cluster=devnet) |
| Upgrade transaction | [`3h4AN3VFyP6nSrxjzpsLVoXYijWCiLPGAeaJghL4DHLgjPio3grdg8JonWKE87tkQzyUirMRWSCcvWEcuVRRiHXS`](https://explorer.solana.com/tx/3h4AN3VFyP6nSrxjzpsLVoXYijWCiLPGAeaJghL4DHLgjPio3grdg8JonWKE87tkQzyUirMRWSCcvWEcuVRRiHXS?cluster=devnet) |
| Slot, time | 522155572, 2026-11-14 07:30:02 -03:00 |
| Upgrade authority | [`J8iiiosBVqFYy7WMryvV9eweaYM4Xat5SYZGmDynJARz`](https://explorer.solana.com/address/J8iiiosBVqFYy7WMryvV9eweaYM4Xat5SYZGmDynJARz?cluster=devnet) (the deployer, unchanged) |
| Source | commit `e5b4363` (program sources unchanged since `60e7c7d`), built with `scripts/build.sh`; working tree clean |
| Binary | 759 704 bytes, sha256 `a2423edf39b497afac616b87f223c568ef2048b10783600413f244577ad892b2` (the 759 728-byte figure in §10 was measured on an earlier tree) |
| Extension | automatic: the CLI extended the program account from 730 680 to 759 704 bytes; program data balance 3.71273324 → 3.86017516 SOL |
| Checks (§6) | `program show`: owner BPFLoaderUpgradeable, authority = deployer, data length 759 704; `program dump` matches the local `.so` byte for byte (sha256 of the dumped first 759 704 bytes is the one above); no buffer left (`program show --buffers` lists none; the buffer `9kMYfnH4cbn1QiBAynYhCxhmRofbAwTYmC4WTjrx3c1n` was consumed by the upgrade) |
| Cost | 0.15121 SOL from the deployer (8.28279864 → 8.13158672): 0.14744 extension rent, the rest fees. The buffer rent (~3.86 SOL) came back |

### Setup (2026-11-14)

`node scripts/setup.ts --cluster devnet --fee-bps 50 --cashback-bps 2000 --funder-sol 0.2`
(the demo funder got 0.2 SOL instead of the default 0.5: a full demo run costs
about 0.04 SOL). `initialize_config` fixed the fee at **50 bps** (0.5%, the
program's maximum), the cashback at **2000 bps of the fee** and the timelock at
60 s. Role keys are in `~/.config/niara-exchange/devnet/` (outside the
repository, mode 600) and are all different from each other and from the
admin.

| Role | Public key |
|---|---|
| Config (PDA) | [`HRbQRCDQDRe4BxfRJfPaywyqeWks9SjoH9jpMsRJDAFE`](https://explorer.solana.com/address/HRbQRCDQDRe4BxfRJfPaywyqeWks9SjoH9jpMsRJDAFE?cluster=devnet) |
| Admin = upgrade authority (deployer) | [`J8iiiosBVqFYy7WMryvV9eweaYM4Xat5SYZGmDynJARz`](https://explorer.solana.com/address/J8iiiosBVqFYy7WMryvV9eweaYM4Xat5SYZGmDynJARz?cluster=devnet) |
| Listing authority | [`C7axhNVVXvKHZvvAg5ZFsgQ1uuHwE2p2YLxYA49eo9MH`](https://explorer.solana.com/address/C7axhNVVXvKHZvvAg5ZFsgQ1uuHwE2p2YLxYA49eo9MH?cluster=devnet) |
| Settlement operator | [`PNQkiCd6YLUEFhahK3QGmiLtYjX9JRnigWPJ4AR9ziL`](https://explorer.solana.com/address/PNQkiCd6YLUEFhahK3QGmiLtYjX9JRnigWPJ4AR9ziL?cluster=devnet) |
| Treasury (signs nothing, holds no SOL) | [`EDPd25GPmw85m7vYs9KtuTEveaykW5xAhnNk27vVS1Rw`](https://explorer.solana.com/address/EDPd25GPmw85m7vYs9KtuTEveaykW5xAhnNk27vVS1Rw?cluster=devnet) |
| Mock mint authority | [`Aa8UEMMRwYqoo16hFRS4jQecHjbyTiEawggq4Fz7rtEH`](https://explorer.solana.com/address/Aa8UEMMRwYqoo16hFRS4jQecHjbyTiEawggq4Fz7rtEH?cluster=devnet) |
| Demo funder | [`4jHkxt1nNQpCHipNq75nkanKScRKb9JaoPDwAFGUsbxn`](https://explorer.solana.com/address/4jHkxt1nNQpCHipNq75nkanKScRKb9JaoPDwAFGUsbxn?cluster=devnet) |

| Test token (no value, Token-2022, 6 decimals, no freeze authority) | Mint | Listing (`Currency`) |
|---|---|---|
| "Niara Mock USDT (TEST)" (`mockUSDT`, the fee mint) | [`8iWQ7VxYrkUv471vEoZ664eLwDzWinPiCJpRvcTUA24y`](https://explorer.solana.com/address/8iWQ7VxYrkUv471vEoZ664eLwDzWinPiCJpRvcTUA24y?cluster=devnet) | [`3akRnDyay46DHopQzEJrcAybki99LKyjVM2YhyhisYNy`](https://explorer.solana.com/address/3akRnDyay46DHopQzEJrcAybki99LKyjVM2YhyhisYNy?cluster=devnet) |
| "Niara Mock BRL (TEST)" (`mockBRL`) | [`5EVnygYXvWZtuRPpafUnEBZBgG2SUczupvt5RtCxafjk`](https://explorer.solana.com/address/5EVnygYXvWZtuRPpafUnEBZBgG2SUczupvt5RtCxafjk?cluster=devnet) | [`Epu1MfYHsdPL6RuAMHRwtNAmJbgGWVa9HsUko7Trvg6d`](https://explorer.solana.com/address/Epu1MfYHsdPL6RuAMHRwtNAmJbgGWVa9HsUko7Trvg6d?cluster=devnet) |

Fee vault [`2A7HdMHydgPvjxrZgU2XEHPCKxhJkkp7eA1JfhWajqCE`](https://explorer.solana.com/address/2A7HdMHydgPvjxrZgU2XEHPCKxhJkkp7eA1JfhWajqCE?cluster=devnet), fee ledger
[`FVpx3pynbAiTDmAeCEco7ug4XRf4Pcb1jKeQj5LU4Qf`](https://explorer.solana.com/address/FVpx3pynbAiTDmAeCEco7ug4XRf4Pcb1jKeQj5LU4Qf?cluster=devnet), vault authority (PDA)
[`84kn8YgJXxTVSRkBuNXLtxmeHDUiBMSSVzfgqzGmuu8U`](https://explorer.solana.com/address/84kn8YgJXxTVSRkBuNXLtxmeHDUiBMSSVzfgqzGmuu8U?cluster=devnet). The public addresses are
also in `~/.config/niara-exchange/devnet/deployment.json`.

| Step | Transaction |
|---|---|
| fund listing authority (0.0500 SOL), settlement operator (0.0500 SOL), mock mint authority (0.0500 SOL), demo funder (0.2000 SOL) | [`2hqCYvCqsnV3NkVqgxPYCpDoX7jnmEbRW59NXDnzsM8iHJhNU99THbGbcV7rUpQ4J9ahQswTdqLxHF5D6dFWSMUe`](https://explorer.solana.com/tx/2hqCYvCqsnV3NkVqgxPYCpDoX7jnmEbRW59NXDnzsM8iHJhNU99THbGbcV7rUpQ4J9ahQswTdqLxHF5D6dFWSMUe?cluster=devnet) |
| create mockUSDT mint "Niara Mock USDT (TEST)" 8iWQ7VxYrkUv471vEoZ664eLwDzWinPiCJpRvcTUA24y | [`wdKNyxbFPUEfmtgMrXJPjDyZ3icYDXMSkVzdKv4JPqmCwD2PZyZBLh1Con96UpcBEKCD9z9SWMBQMdPRxQRoCCr`](https://explorer.solana.com/tx/wdKNyxbFPUEfmtgMrXJPjDyZ3icYDXMSkVzdKv4JPqmCwD2PZyZBLh1Con96UpcBEKCD9z9SWMBQMdPRxQRoCCr?cluster=devnet) |
| create mockBRL mint "Niara Mock BRL (TEST)" 5EVnygYXvWZtuRPpafUnEBZBgG2SUczupvt5RtCxafjk | [`4RXn7bmKKvgj6PbahSAkVkcBER1kWRseNub4CJTHCcGLuKqKKbTGihpmVcQshyTPU6qAR8FvyyKqjAd6EJHakrf7`](https://explorer.solana.com/tx/4RXn7bmKKvgj6PbahSAkVkcBER1kWRseNub4CJTHCcGLuKqKKbTGihpmVcQshyTPU6qAR8FvyyKqjAd6EJHakrf7?cluster=devnet) |
| initialize_config (fee 50 bps, cashback 2000 bps of the fee, timelock 60 s) | [`3eihLHNd3G5vVaMP3g3XCYRoviaeXkGhz6Gj8efU8Kf7TzGeoDuwHKEewFhAp6U962hn8bfggJfarwJf7SWDodtX`](https://explorer.solana.com/tx/3eihLHNd3G5vVaMP3g3XCYRoviaeXkGhz6Gj8efU8Kf7TzGeoDuwHKEewFhAp6U962hn8bfggJfarwJf7SWDodtX?cluster=devnet) |
| init_fee_vault (MockUSDT) | [`4heHYgf8eGdGs6tFxgiEotoQ9emU6BapYn7TsCFXb8eF5YsD41A8143wibcEU72JLi1GuSGnDUHH2DouWSdrXmNW`](https://explorer.solana.com/tx/4heHYgf8eGdGs6tFxgiEotoQ9emU6BapYn7TsCFXb8eF5YsD41A8143wibcEU72JLi1GuSGnDUHH2DouWSdrXmNW?cluster=devnet) |
| list_currency mockUSDT | [`uDD7MDe7oukfCFedthMGYQPYz4UANBoGPRQ73ugQCfNu9NXXkYw9jiiQxZXzTZV27YvUXGn2dKGxgp9PJkRvjrS`](https://explorer.solana.com/tx/uDD7MDe7oukfCFedthMGYQPYz4UANBoGPRQ73ugQCfNu9NXXkYw9jiiQxZXzTZV27YvUXGn2dKGxgp9PJkRvjrS?cluster=devnet) |
| list_currency mockBRL | [`2SWEbLwiCeuuXRFDkFLtBsuoh14mGqf3sP1ApYp87vchZcP4CUsfMbEEVGgQkm3yZfffGNJCuPN6qSPSkrjaNwqx`](https://explorer.solana.com/tx/2SWEbLwiCeuuXRFDkFLtBsuoh14mGqf3sP1ApYp87vchZcP4CUsfMbEEVGgQkm3yZfffGNJCuPN6qSPSkrjaNwqx?cluster=devnet) |

### Demo (2026-11-14)

`node scripts/demo.ts --cluster devnet`: the three flows, 34 transactions
(including 2 of clean-up), every check passed:
"Demo complete: every check passed". Flows a and b each quote
(`quote_listing`) and pay (`pay_listing_fee`) a listing fee of 1,000 MockUSDT
(valuation 1,000,000 MockUSDT, 10 bps) before issuance; flow c trades two
`Currency` listings, which have no listing fee. Fees seen: flow a 12.5 MockUSDT with 2.5
cashback (withdrawn), flow b 50 with 10, flow c 5 with 0. After the run the fee
vault held 2,065 MockUSDT (2 × 1,000 listing fees + 67.5 trading fees − 2.5
cashback withdrawn). The issuer, seller and buyer keypairs of the run are in
`~/.config/niara-exchange/devnet/demo-runs/2026-11-14T10-31-02-302Z/` (not in
the repository). The run cost the demo funder 0.0379 SOL.

**Flow a: Equity, order-book mode (delegate -> settle -> withdraw_cashback)**

| Step | Transaction |
|---|---|
| fund issuer (0.0200 SOL), seller (0.0100 SOL), buyer (0.0100 SOL) | [`53sDwuVXaqCMheYkHtiKfmG7e6owxNEXRZ3koaj3cHeodt14Rhj9Vn8ADsDyE4m4QqwU8VtH3i3eF8v3MvU5ri6M`](https://explorer.solana.com/tx/53sDwuVXaqCMheYkHtiKfmG7e6owxNEXRZ3koaj3cHeodt14Rhj9Vn8ADsDyE4m4QqwU8VtH3i3eF8v3MvU5ri6M?cluster=devnet) |
| [a.1] issuer creates the mint "Niara Demo Equity (TEST)" (tEQTY) 8LjQkS1rTJGpzz9wbyAmqJk7oZgDsZS6vhPBv2BhzsTo | [`4GvgHtSY22kRk4W4nG2nM1yh4T9z9ep2KfAe6c8zuGCe1AxMV7yBMXPQnMW7cPCA1PCerETCBSErpQDQN1XoUN4M`](https://explorer.solana.com/tx/4GvgHtSY22kRk4W4nG2nM1yh4T9z9ep2KfAe6c8zuGCe1AxMV7yBMXPQnMW7cPCA1PCerETCBSErpQDQN1XoUN4M?cluster=devnet) |
| [a.2] submit_listing (equity): the mint authority moves to the program | [`5TasCoE8QXhT4Hj3uYg4tWfFbsxTD52pTGUvgf5b5hMZEyaBcMgAj96FW1Fv9pqXM35FLB78fBYgccbX4ZvTUpqT`](https://explorer.solana.com/tx/5TasCoE8QXhT4Hj3uYg4tWfFbsxTD52pTGUvgf5b5hMZEyaBcMgAj96FW1Fv9pqXM35FLB78fBYgccbX4ZvTUpqT?cluster=devnet) |
| [a.3] quote_listing (Niara listing authority): valuation 1000000.000000 mockUSDT at 10 bps = 1000.000000 mockUSDT, payable for 60 min | [`3YgFmfeCbZ9g6WY2qzCF35C9NHsNz4KxZmn362uxy2AnFkUJFfrVVHHHkJmiYUWJbGExB1imPpcsWwXocMVxNa4`](https://explorer.solana.com/tx/3YgFmfeCbZ9g6WY2qzCF35C9NHsNz4KxZmn362uxy2AnFkUJFfrVVHHHkJmiYUWJbGExB1imPpcsWwXocMVxNa4?cluster=devnet) |
| [a.4] Niara mints 1000.000000 mockUSDT (test currency) to the issuer for the listing fee | [`1tMuibLvNyvGib8sosJedVRkFLNf1MATniGSZZBPzbxkp1L4PGMPVJHTHCs7NqtwEetgvwVgHZfgMfHsisAZooK`](https://explorer.solana.com/tx/1tMuibLvNyvGib8sosJedVRkFLNf1MATniGSZZBPzbxkp1L4PGMPVJHTHCs7NqtwEetgvwVgHZfgMfHsisAZooK?cluster=devnet) |
| [a.5] pay_listing_fee (issuer): 1000.000000 mockUSDT into the fee vault; the listing is approved | [`Zn6tdHHFch4KEEDFALVr2Gigxoyuy83UP7d4JnAwPu2LTm7PBnQrmzt2hECZwEHCnzHznB7aZk9hoarRcPxB6u6`](https://explorer.solana.com/tx/Zn6tdHHFch4KEEDFALVr2Gigxoyuy83UP7d4JnAwPu2LTm7PBnQrmzt2hECZwEHCnzHznB7aZk9hoarRcPxB6u6?cluster=devnet) |
| [a.6] request_issuance: batch #1, 1000 tEQTY to the holder | [`NBkZPDz8xfb9FWzmR6UyEUdXLcmeQfWPd9dL7LLWJjKrgHFiMN5EhqkB5Py8Wm29YQ77KQzubeyLbZpCEBMKCe3`](https://explorer.solana.com/tx/NBkZPDz8xfb9FWzmR6UyEUdXLcmeQfWPd9dL7LLWJjKrgHFiMN5EhqkB5Py8Wm29YQ77KQzubeyLbZpCEBMKCe3?cluster=devnet) |
| [a.7] holder creates its token account | [`1BQkD3oxdVayiHW9zJfdp7nzQwUfARpoGqRBQj3LFkCFB5uFL87eJCicLduscD45pnyZSmLTJqKjW4cpXjSxy8d`](https://explorer.solana.com/tx/1BQkD3oxdVayiHW9zJfdp7nzQwUfARpoGqRBQj3LFkCFB5uFL87eJCicLduscD45pnyZSmLTJqKjW4cpXjSxy8d?cluster=devnet) |
| [a.8] approve_issuance (Niara listing authority): the program mints batch #1 | [`3ktf1bxnF1aWCA97sNEn2JqYB2kqkDdxg9DruEYr35MQf8oasZJY8Z1h76fYPiBbL6AdoMPW9mY1wH2jaNKF2Ke`](https://explorer.solana.com/tx/3ktf1bxnF1aWCA97sNEn2JqYB2kqkDdxg9DruEYr35MQf8oasZJY8Z1h76fYPiBbL6AdoMPW9mY1wH2jaNKF2Ke?cluster=devnet) |
| [a.9] init_cashback_ledger (permissionless; the issuer pays the rent) | [`ivtBcbL5z5MJR2ppjDh1Sf7uBsR14sXet3kRpyHP918L7UawQye7wHorv3NfctEheMgf2QhsAqr3PRjWNA55dHY`](https://explorer.solana.com/tx/ivtBcbL5z5MJR2ppjDh1Sf7uBsR14sXet3kRpyHP918L7UawQye7wHorv3NfctEheMgf2QhsAqr3PRjWNA55dHY?cluster=devnet) |
| [a.10] buyer and seller create their token accounts; Niara mints 2500.000000 mockUSDT (test currency) to the buyer | [`4SGYCLVvrznceGfT8tj3TxvKvTo4MUYp9AhcNWUSNnSfYjQFzBZPE2C3XsyiaXiiQUFS7og9b7UqxTbaaZT2dHs3`](https://explorer.solana.com/tx/4SGYCLVvrznceGfT8tj3TxvKvTo4MUYp9AhcNWUSNnSfYjQFzBZPE2C3XsyiaXiiQUFS7og9b7UqxTbaaZT2dHs3?cluster=devnet) |
| [a.11] seller delegates 100 tEQTY to the program's vault PDA | [`4ByZa1gLWcnyVzeM3ek8KE1qN7xjDcTLQ9rZeAWDgJDKSL9wcgD8g4WKcrt8SM8xhMqkXEKvDTv4Lq55Yc6LcffL`](https://explorer.solana.com/tx/4ByZa1gLWcnyVzeM3ek8KE1qN7xjDcTLQ9rZeAWDgJDKSL9wcgD8g4WKcrt8SM8xhMqkXEKvDTv4Lq55Yc6LcffL?cluster=devnet) |
| [a.12] buyer delegates 2500.000000 mockUSDT to the program's vault PDA | [`5m2zer8tRnUGm4pTAUNA8rFchxYasULbzuk8ThsY5i8nGq7SGWhLk6ubuBhyJYWZBHBKm9JtnjrcrsKVoRa6U852`](https://explorer.solana.com/tx/5m2zer8tRnUGm4pTAUNA8rFchxYasULbzuk8ThsY5i8nGq7SGWhLk6ubuBhyJYWZBHBKm9JtnjrcrsKVoRa6U852?cluster=devnet) |
| [a.13] settle (operator): match 182faf459eae3c76181fe9a33a268180, 100 tEQTY for 2500.000000 mockUSDT | [`2VWjFyoEk3hWCHvAj6uYq87BnVehWdFKGKfRmH3LDasteWv9WPwhAU7D7ePjNbMpxAzrpMxBozjdc789hsRQNsfq`](https://explorer.solana.com/tx/2VWjFyoEk3hWCHvAj6uYq87BnVehWdFKGKfRmH3LDasteWv9WPwhAU7D7ePjNbMpxAzrpMxBozjdc789hsRQNsfq?cluster=devnet) |
| [a.14] withdraw_cashback (issuer): 2.500000 mockUSDT | [`4kuMLYtCdMjguAdTZxj3XAaAk2hh3fW8VKQvt5VYNRgkfhcpYJ3zk2q532YGYUxvJGLzvbScHNn1MihLKV2pNLiw`](https://explorer.solana.com/tx/4kuMLYtCdMjguAdTZxj3XAaAk2hh3fW8VKQvt5VYNRgkfhcpYJ3zk2q532YGYUxvJGLzvbScHNn1MihLKV2pNLiw?cluster=devnet) |

**Flow b: FixedIncome, P2P escrow (open_deal -> accept_deal)**

| Step | Transaction |
|---|---|
| fund issuer (0.0200 SOL), holder (proposer) (0.0150 SOL), buyer (counterparty) (0.0100 SOL) | [`57xcywmvS8bzpr9cNqT6FxSsvAapFuzx9KdJEXGCvgWRnNycVnrsLJXX4deTrb7sCVkc6pJ1LvceeZqTRTkDwDHa`](https://explorer.solana.com/tx/57xcywmvS8bzpr9cNqT6FxSsvAapFuzx9KdJEXGCvgWRnNycVnrsLJXX4deTrb7sCVkc6pJ1LvceeZqTRTkDwDHa?cluster=devnet) |
| [b.1] issuer creates the mint "Niara Demo Debenture (TEST)" (tDEB) 4EBRQ9NRwpriv1YP6Y8a5WnByMxj2WjGveEnHZUk7Jnf | [`3oQSgt5hQMvSUvRAEJAc4wHPTNSgGDcCVLczUfQJLgobTZBaLKwHquuuc8HJoGwnrFU88rfF8PsxxjiHMexW9fXs`](https://explorer.solana.com/tx/3oQSgt5hQMvSUvRAEJAc4wHPTNSgGDcCVLczUfQJLgobTZBaLKwHquuuc8HJoGwnrFU88rfF8PsxxjiHMexW9fXs?cluster=devnet) |
| [b.2] submit_listing (fixedIncome): the mint authority moves to the program | [`2RLZrr5U3uVrnoCTm4gYKy4ididF36jGHGZnpiA5rtgwYvXTEf7UU1gC5av7GzMrhKFGdc3nGJfcWj3A3f5cVRPZ`](https://explorer.solana.com/tx/2RLZrr5U3uVrnoCTm4gYKy4ididF36jGHGZnpiA5rtgwYvXTEf7UU1gC5av7GzMrhKFGdc3nGJfcWj3A3f5cVRPZ?cluster=devnet) |
| [b.3] quote_listing (Niara listing authority): valuation 1000000.000000 mockUSDT at 10 bps = 1000.000000 mockUSDT, payable for 60 min | [`2pWyx8FQ27Ub6DvEKCyrLYT34ybKgikNjnWSkSX52RmjCNp28FsfNapme4fMhW5cVtVWLertY65j38FtjJt792Ex`](https://explorer.solana.com/tx/2pWyx8FQ27Ub6DvEKCyrLYT34ybKgikNjnWSkSX52RmjCNp28FsfNapme4fMhW5cVtVWLertY65j38FtjJt792Ex?cluster=devnet) |
| [b.4] Niara mints 1000.000000 mockUSDT (test currency) to the issuer for the listing fee | [`5ik3qnfuNXx5oaZCqNiMpxN4mTPVmyJ5zH66EhhAzGBiVtqoU38ZCmr32mLUzNStaniao4KPSs5nCGo3GtXC5T1J`](https://explorer.solana.com/tx/5ik3qnfuNXx5oaZCqNiMpxN4mTPVmyJ5zH66EhhAzGBiVtqoU38ZCmr32mLUzNStaniao4KPSs5nCGo3GtXC5T1J?cluster=devnet) |
| [b.5] pay_listing_fee (issuer): 1000.000000 mockUSDT into the fee vault; the listing is approved | [`WfeSb24ysoJD1or6DJChAmaXVG5worwSrzBDb2aDGrdTgSYZ6cH8NiUaVkFmZA9Sh171roDBYKrGuVx5Ugwpk61`](https://explorer.solana.com/tx/WfeSb24ysoJD1or6DJChAmaXVG5worwSrzBDb2aDGrdTgSYZ6cH8NiUaVkFmZA9Sh171roDBYKrGuVx5Ugwpk61?cluster=devnet) |
| [b.6] request_issuance: batch #1, 50 tDEB to the holder | [`58Jw4eNy3aVdZU8ssSPrP7w8KiR9qDxmfLmzcwGvuYufE848N4yytgyirzRcabzoYRP7zp15npSMe2KaLEr7mJY1`](https://explorer.solana.com/tx/58Jw4eNy3aVdZU8ssSPrP7w8KiR9qDxmfLmzcwGvuYufE848N4yytgyirzRcabzoYRP7zp15npSMe2KaLEr7mJY1?cluster=devnet) |
| [b.7] holder creates its token account | [`5NdevvdvcFpVwVm3CfZqJzC5WStTLfHD14iS3rA16TaRvRcL1FE3qTC79yZy4M9SZXCPHJrqUPhL5o5AvvnEjkMx`](https://explorer.solana.com/tx/5NdevvdvcFpVwVm3CfZqJzC5WStTLfHD14iS3rA16TaRvRcL1FE3qTC79yZy4M9SZXCPHJrqUPhL5o5AvvnEjkMx?cluster=devnet) |
| [b.8] approve_issuance (Niara listing authority): the program mints batch #1 | [`qNbgU8nhYyMntXhjAvSEurMWmAbqxPPpdPePTRDv3znjifT27LzsGLJV1umfY62YVm4vSX6YYUWmYkD27sPrG3o`](https://explorer.solana.com/tx/qNbgU8nhYyMntXhjAvSEurMWmAbqxPPpdPePTRDv3znjifT27LzsGLJV1umfY62YVm4vSX6YYUWmYkD27sPrG3o?cluster=devnet) |
| [b.9] init_cashback_ledger (permissionless; the issuer pays the rent) | [`41TrV58C1qATg6NgevHFqarsjzndyta79F9VvoXeH11KJdZY3rA23rz66KvyaqdGJegp3gt3kMuaYWbi6NgLxrrh`](https://explorer.solana.com/tx/41TrV58C1qATg6NgevHFqarsjzndyta79F9VvoXeH11KJdZY3rA23rz66KvyaqdGJegp3gt3kMuaYWbi6NgLxrrh?cluster=devnet) |
| [b.10] both parties create their token accounts; Niara mints 10000.000000 mockUSDT (test currency) to the buyer | [`tCk4zATgLV541b7M2doYtkCt6HLh4ygQ1vM1EEnN6ACbro6F3V2BkSgcUNmoiaCM8NCkQ32fKvv7jfgFsbvNL5a`](https://explorer.solana.com/tx/tCk4zATgLV541b7M2doYtkCt6HLh4ygQ1vM1EEnN6ACbro6F3V2BkSgcUNmoiaCM8NCkQ32fKvv7jfgFsbvNL5a?cluster=devnet) |
| [b.11] open_deal 525b4e8acbe25340c250fb7855e72ec4: the proposer sells 10 tDEB for 10000.000000 mockUSDT and escrows 10 tDEB (expires in 1 h) | [`2SiDGU7QpgcPd9RYX4zgQ22aViNWdcseucM7L68fb4J61vUM24KgmWU5ksehcmpsCtaSb9GqMKW9GEVmdrQbKxSP`](https://explorer.solana.com/tx/2SiDGU7QpgcPd9RYX4zgQ22aViNWdcseucM7L68fb4J61vUM24KgmWU5ksehcmpsCtaSb9GqMKW9GEVmdrQbKxSP?cluster=devnet) |
| [b.12] accept_deal (counterparty): both legs swap atomically, the fee goes to the fee vault | [`2ipwbWCRr3thK3vn4wqgPhXo5GMg8gEYEmkrvpgFp8pEXki8gHQ9fmHCnG7dqex5SJpKe9JRRLApvxwbsLr8W8S3`](https://explorer.solana.com/tx/2ipwbWCRr3thK3vn4wqgPhXo5GMg8gEYEmkrvpgFp8pEXki8gHQ9fmHCnG7dqex5SJpKe9JRRLApvxwbsLr8W8S3?cluster=devnet) |

**Flow c: Currency, P2P escrow (MockBRL <-> MockUSDT)**

| Step | Transaction |
|---|---|
| fund mockBRL buyer (proposer) (0.0150 SOL), mockBRL seller (counterparty) (0.0100 SOL) | [`3J63KMEnZKwQ49BEpsA8B5nyNiz3FaBz6L6jMPnXykUBtcpDj4NscefrrV2Xm19PMUzfLZuNqy8FWCaSp2H7SpLQ`](https://explorer.solana.com/tx/3J63KMEnZKwQ49BEpsA8B5nyNiz3FaBz6L6jMPnXykUBtcpDj4NscefrrV2Xm19PMUzfLZuNqy8FWCaSp2H7SpLQ?cluster=devnet) |
| [c.1] both parties create their token accounts; Niara mints 1000.000000 mockUSDT to the proposer and 5000.000000 mockBRL to the counterparty (test currencies) | [`1dBG8QJDDV61LfezZ5G5m6gyLZeZnS5yS1MS1G5Nwd1wPLF3RMbQTPJ5q3B7dMMXpFkupjjKb95uKqeBjpM34Yg`](https://explorer.solana.com/tx/1dBG8QJDDV61LfezZ5G5m6gyLZeZnS5yS1MS1G5Nwd1wPLF3RMbQTPJ5q3B7dMMXpFkupjjKb95uKqeBjpM34Yg?cluster=devnet) |
| [c.2] open_deal d8421272290578e2fc90047e39168f82: the proposer buys 5000.000000 mockBRL for 1000.000000 mockUSDT and escrows 1000.000000 mockUSDT (expires in 1 h) | [`5Jd6bQo1XQsPys4p5Hha13rFJFaef15iRkZKceygWpVME7ACE27nYU75qZvKQC5RSEwxMw9TZzwhnxpwa6ES822Z`](https://explorer.solana.com/tx/5Jd6bQo1XQsPys4p5Hha13rFJFaef15iRkZKceygWpVME7ACE27nYU75qZvKQC5RSEwxMw9TZzwhnxpwa6ES822Z?cluster=devnet) |
| [c.3] accept_deal (counterparty): both legs swap atomically, the fee goes to the fee vault | [`63hDDYxa9kTZpAfcdVTgCogrJGzsdBLHTLK7XbaXoMrhDiCN7fRNvv2kJJKrmm41Mw6ji8A6UY9EmHnNvAApJ1cX`](https://explorer.solana.com/tx/63hDDYxa9kTZpAfcdVTgCogrJGzsdBLHTLK7XbaXoMrhDiCN7fRNvv2kJJKrmm41Mw6ji8A6UY9EmHnNvAApJ1cX?cluster=devnet) |

**Clean-up**

| Step | Transaction |
|---|---|
| return unused SOL to the demo funder (5 parties) | [`626nnwqfgC6oPoa93JMbLbexVfgm6P9CkeW5jjxQ2Gf8oyeuiYWRkHRFkHabf1f33oobnaXFEWBWbZeEJQ2hcJbH`](https://explorer.solana.com/tx/626nnwqfgC6oPoa93JMbLbexVfgm6P9CkeW5jjxQ2Gf8oyeuiYWRkHRFkHabf1f33oobnaXFEWBWbZeEJQ2hcJbH?cluster=devnet) |
| return unused SOL to the demo funder (3 parties) | [`qy9Z5i4XM1bSQNkvzAXtiLGzevfb1cge2oqT1wudvzatGrjE6gUHGo1Emzy9VU89KuNZHxfyQ3pfJhXu2UxEmnK`](https://explorer.solana.com/tx/qy9Z5i4XM1bSQNkvzAXtiLGzevfb1cge2oqT1wudvzatGrjE6gUHGo1Emzy9VU89KuNZHxfyQ3pfJhXu2UxEmnK?cluster=devnet) |

### Refusal checks (2026-11-14)

`demo.ts` checks the success paths and the balances; the on-chain refusals
(every `require!`, authorization and pause case) are covered by the LiteSVM
tests, not by the devnet demo. The refusals run against devnet are the script
safety checks of `scripts/localnet-demo.sh`, each refused before any
transaction was sent:

| Command | Refused with |
|---|---|
| `setup.ts --cluster devnet` with a deployer that is not the upgrade authority | `the program's upgrade authority is J8ii…JARz, not the deployer …; initialize_config would fail with NotUpgradeAuthority` |
| `setup.ts --cluster devnet` with a keys folder inside the repository | `the keys folder … is inside the repository; keys must stay outside it` |
| `demo.ts --cluster devnet` with a keys folder inside the repository | the same message |
| `setup.ts --cluster localnet` with the devnet RPC URL | `--cluster localnet needs a local RPC URL (127.0.0.1)` |

Not repeated on devnet: the "the RPC is not devnet" refusal (it needs a
non-devnet endpoint; `localnet-demo.sh` covers it).

### Balances (SOL)

| Account | After the upgrade, before setup | After setup, before the demo | After the demo |
|---|---|---|---|
| Deployer / admin | 8.13158672 | 7.77744168 | 7.77744168 |
| Listing authority | 0 (not created) | 0.0465864 | 0.0465664 |
| Settlement operator | 0 (not created) | 0.05 | 0.04929904 |
| Treasury | 0 (not created) | 0 | 0 |
| Mock mint authority | 0 (not created) | 0.045154 | 0.045154 |
| Demo funder | 0 (not created) | 0.2 | 0.16209172 |

Before the upgrade the deployer held 8.28279864 SOL: upgrade and setup cost it
0.50535696 SOL in total (0.15121 upgrade, 0.35414504 setup, of which 0.35 was
sent to the role keys).

### IDL on-chain (2026-11-14)

The IDL is published as the program's canonical metadata (Program Metadata
program, seed `idl`), signed by the upgrade authority (the deployer).

| Item | Value |
|---|---|
| IDL account | [`B2iLwVDLyh9DiHpkFT3FZTPDqLuwXSCkZaTHAHSx9BbA`](https://explorer.solana.com/address/B2iLwVDLyh9DiHpkFT3FZTPDqLuwXSCkZaTHAHSx9BbA?cluster=devnet), owner `ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S`, 6 177 bytes |
| Content | `target/idl/niara_exchange.json` **without its `docs` fields** (41 778 bytes, 6 081 compressed, sha256 `0e3c7a987f1943fcd71bd202c54df50a4ef0a73206c8c0773178366711029462`); every other field is identical. `anchor idl fetch` returns it and it decompresses to exactly that file |
| Cost | 0.03226 SOL from the deployer (7.77744168 → 7.74517706): 0.03203 rent, refundable with `anchor idl close`, the rest fees (including those of the failed attempts below) |
| CLI | `~/.avm/bin/anchor-1.2.2 idl init --filepath <idl> --provider.cluster devnet --provider.wallet "$DEPLOYER" "$PROGRAM_ID"` |
| Explorer check | **Not done from here**: Explorer decodes in the browser and this session has no browser. Open a demo transaction, for example the `pay_listing_fee` of flow a, and look for the instruction name: the result is not recorded yet |

What happened, so it is not repeated blindly:

- The full IDL (13 031 bytes compressed, about 13 parallel writes) failed four
  times: `idl init` once and `idl upgrade` three times (the last one with
  `--priority-fee 200000`). Each time the CLI printed only "The provided
  transaction plan failed to execute", no transaction had failed on chain, and
  the account or buffer it left behind was incomplete (7 381 and 3 554 trailing
  zero bytes). The public devnet RPC also answered HTTP 429 to my own queries
  in that window, so rate limiting or dropped writes is the likely cause; it
  was not proven.
- Each failed run left a 0.0673 SOL account (the metadata account, then one
  buffer per `upgrade` run). `idl close` removes the metadata account; a buffer
  is consumed with `idl write-buffer --buffer <BUFFER> --close-buffer` (it
  works only while no metadata account exists), then `idl close`. All four were
  recovered; nothing was left on chain (checked with `getProgramAccounts` on
  the metadata program, filtered by size). The four failed runs cost about
  0.0002 SOL in fees.
- With the docs stripped (about 6 KB compressed) the CLI **still printed
  "Failed to initialize IDL"**, but this time the data had been written
  completely: the account holds the whole IDL, fetched back and compared. So a
  CLI error does not mean nothing was written: check with `anchor idl fetch`
  and `getProgramAccounts` before retrying.
- The IDL must be re-uploaded after every upgrade that changes it
  (`idl upgrade`, which needs the buffer rent free for the run). The published
  copy has no `docs`; the full IDL stays in `target/idl/`.

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
| Program data rent: `solana rent <size of .so + 45>`. Devnet charged 3.7127 SOL for the 730 680-byte binary (2026-11-13); the local validator charges more (4.998 SOL for 717 920 bytes) | ~3.7 |
| Program account rent and deploy fees (about 720 write transactions) | < 0.01 |
| `scripts/setup.ts`: 0.65 to the role keys (0.5 of it to the demo funder) plus rent | ~0.7 |
| **First deploy + setup** | **~4.4 on devnet; fund 5** |
| Each upgrade: a second program-data rent for the buffer (~3.7 SOL on devnet), returned when the upgrade lands; the fees stay spent | ~3.7 free |

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

  A Squads v4 vault is a PDA and cannot sign a CLI transaction. To hand the
  upgrade authority to one, replace `--new-upgrade-authority
  <NEW_AUTHORITY_KEYPAIR>` with `--new-upgrade-authority <VAULT_ADDRESS>
  --skip-new-upgrade-authority-signer-check`. That drops the typo protection,
  so check the vault address in the Squads app first.

  `Config.admin` does not move with it. Moving the admin is a separate
  two-step transfer:
  1. the current admin signs `propose_change(Admin(<NEW_ADMIN>))`;
  2. after the 60 s timelock, the new admin signs `accept_admin`. With a
     Squads vault as the new admin, that is a vault transaction approved by
     its members.

  `execute_change` refuses an admin change, so the admin can only move to a
  key that proves it can sign. Until step 2, the current admin can
  `cancel_change`. The admin alone rotates the listing authority, the
  settlement operator and the treasury, through `propose_change` and
  `execute_change`. No script automates any of this, and the Squads path has
  not been rehearsed yet.
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
| a | `Equity`, order-book mode: create mint → `submit_listing` → `quote_listing` (listing authority) → Niara mints the fee in MockUSDT to the issuer → `pay_listing_fee` (issuer; approves the listing) → issuance batch (`request_issuance`, `approve_issuance`) → delegate → `settle` → `withdraw_cashback` |
| b | `FixedIncome`, P2P escrow: mint, listing, listing fee and batch as in (a), then `open_deal` (the holder sells debentures for MockUSDT) → `accept_deal` |

Each demo listing is quoted at a 1,000,000 MockUSDT test valuation and 0.1%
(10 bps), so the issuer pays 1,000 MockUSDT into the fee vault. The
valuation is public on-chain (spec §3.13).
| c | `Currency`, P2P escrow, MockBRL ↔ MockUSDT: `open_deal` (the proposer buys MockBRL and escrows MockUSDT) → `accept_deal` |

Every issuer, seller and buyer is a new keypair created by the run and saved
in `~/.config/niara-exchange/devnet/demo-runs/<time>/`. They are funded by the
demo funder and return their unused SOL at the end. Every transaction prints
its explorer link, and the script checks the resulting balances, fee and
cashback and stops at the first mismatch. A full run is about 46 transactions
and cost the funder 0.052 SOL on the local validator (rent of the accounts it
leaves behind, plus fees; measured 2026-11-13 with the listing fee). Refill the funder by running setup again or with:

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
(or a new program ID and a new setup); plan it before deploying.

**Done 2026-11-14: on-chain listing fee (spec §3.13).** The upgrade landed
before the first setup, so no migration was needed; the record, with the
measured cost (0.15121 SOL: 0.14744 extension rent plus fees; the ~3.86 SOL
buffer came back), is at the top. The estimate made before it was 0.14756 SOL
for the extension and about 4.02 SOL free at the peak. Error codes only ever
grow at the end (CLAUDE.md).

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
