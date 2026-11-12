// One-time setup of a deployed niara_exchange program (localnet or devnet).
// TESTNET ONLY: every token created here is a Niara-controlled test token.
//
//   1. creates the role keypairs (listing authority, settlement operator,
//      treasury, mock-token mint authority, demo funder) in the keys folder,
//      outside the repository, and funds them from the deployer (no airdrop);
//   2. creates the MockUSDT and MockBRL mints (Token-2022, named as test tokens);
//   3. initialize_config (fee_mint = MockUSDT, timelock 60 s), signed by the
//      deployer, which must be the program's upgrade authority;
//   4. init_fee_vault;
//   5. list_currency for MockUSDT and MockBRL.
//
// Safe to run again: every step is skipped when its account already exists.
//
// Usage: node scripts/setup.ts [--cluster localnet|devnet] [--fee-bps 30]
//                              [--cashback-bps 2000] [--funder-sol 0.5]
//   NIARA_DEPLOYER   deployer keypair (default ~/.config/solana/niara-exchange-deployer.json)
//   NIARA_KEYS_DIR   keys folder (default ~/.config/niara-exchange/<cluster>)
//   NIARA_RPC_URL    RPC URL override (never commit one that carries an API key)

import { writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";

import { getMint } from "@solana/spl-token";
import { LAMPORTS_PER_SOL, PublicKey, SystemProgram } from "@solana/web3.js";
import type { Keypair } from "@solana/web3.js";

import {
  BPF_LOADER_UPGRADEABLE,
  TOKEN_PROGRAM,
  bn,
  connect,
  createMintIxs,
  eventually,
  explorerAddress,
  fail,
  fund,
  loadKeypair,
  loadOrCreateKeypair,
  parseCluster,
  pdas,
  send,
} from "./common.ts";
import type { Ctx } from "./common.ts";

/** Spec §3.3: delay between propose_change and execute_change (the program's minimum). */
const TIMELOCK_SECS = 60;
const MOCK_DECIMALS = 6;
const ROLE_LAMPORTS = 0.05 * LAMPORTS_PER_SOL;

const { values: args } = parseArgs({
  options: {
    cluster: { type: "string" },
    "fee-bps": { type: "string", default: "30" },
    "cashback-bps": { type: "string", default: "2000" },
    "funder-sol": { type: "string", default: "0.5" },
  },
});

function intArg(name: "fee-bps" | "cashback-bps", max: number): number {
  const value = Number(args[name]);
  if (!Number.isInteger(value) || value < 0 || value > max) fail(`--${name} must be an integer from 0 to ${max}`);
  return value;
}

/** The deployer must be the upgrade authority: only it can initialize_config (spec §3.6). */
async function requireUpgradeAuthority(ctx: Ctx, deployer: PublicKey): Promise<void> {
  const programId = ctx.program.programId;
  const program = await ctx.connection.getAccountInfo(programId);
  if (!program?.executable || !program.owner.equals(BPF_LOADER_UPGRADEABLE)) {
    fail(`program ${programId.toBase58()} is not deployed on this cluster (docs/devnet-deploy.md)`);
  }
  const programData = await ctx.connection.getAccountInfo(pdas(programId).programData());
  // ProgramData: u32 tag, u64 slot, Option<Pubkey> upgrade authority.
  const authority =
    programData && programData.data[12] === 1 ? new PublicKey(programData.data.subarray(13, 45)) : null;
  if (!authority?.equals(deployer)) {
    fail(
      `the program's upgrade authority is ${authority?.toBase58() ?? "none"}, not the deployer ` +
        `${deployer.toBase58()}; initialize_config would fail with NotUpgradeAuthority`,
    );
  }
}

async function ensureMockMint(ctx: Ctx, mint: Keypair, authority: Keypair, name: string, symbol: string): Promise<void> {
  if (await ctx.connection.getAccountInfo(mint.publicKey)) {
    const info = await getMint(ctx.connection, mint.publicKey, "confirmed", TOKEN_PROGRAM);
    if (!info.mintAuthority?.equals(authority.publicKey)) {
      fail(`${symbol} mint ${mint.publicKey.toBase58()} exists with another mint authority`);
    }
    console.log(`  ${symbol} mint already exists: ${mint.publicKey.toBase58()}`);
    return;
  }
  const instructions = await createMintIxs(ctx, {
    payer: authority.publicKey,
    mint: mint.publicKey,
    authority: authority.publicKey,
    decimals: MOCK_DECIMALS,
    name,
    symbol,
  });
  await send(ctx, `create ${symbol} mint "${name}" ${mint.publicKey.toBase58()}`, instructions, authority, mint);
}

async function ensureCurrencyListing(ctx: Ctx, listingAuthority: Keypair, mint: PublicKey, symbol: string): Promise<void> {
  const pda = pdas(ctx.program.programId);
  const listing = pda.listing(mint);
  if (await ctx.program.account.listing.fetchNullable(listing)) {
    console.log(`  ${symbol} is already listed: ${listing.toBase58()}`);
    return;
  }
  const instruction = await ctx.program.methods
    .listCurrency()
    .accountsPartial({
      listingAuthority: listingAuthority.publicKey,
      config: pda.config(),
      assetMint: mint,
      listing,
      tokenProgram: TOKEN_PROGRAM,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  await send(ctx, `list_currency ${symbol}`, [instruction], listingAuthority);
}

async function main(): Promise<void> {
  const feeBps = intArg("fee-bps", 100);
  const cashbackBps = intArg("cashback-bps", 10_000);
  const funderSol = Number(args["funder-sol"]);
  if (!(funderSol >= 0 && funderSol <= 5)) fail("--funder-sol must be between 0 and 5");

  const ctx = await connect(parseCluster(args.cluster));
  const { program, keysDir } = ctx;
  const pda = pdas(program.programId);

  const deployer = loadKeypair(
    process.env.NIARA_DEPLOYER ?? join(homedir(), ".config", "solana", "niara-exchange-deployer.json"),
  );
  console.log(`deployer: ${deployer.publicKey.toBase58()} (admin and upgrade authority)`);
  await requireUpgradeAuthority(ctx, deployer.publicKey);

  console.log("\n[1] role keypairs");
  const key = (name: string) => loadOrCreateKeypair(join(keysDir, `${name}.json`));
  const listingAuthority = key("listing-authority");
  const operator = key("settlement-operator");
  const treasury = key("treasury");
  const mockAuthority = key("mock-mint-authority");
  const funder = key("demo-funder");
  const mockUsdt = key("mock-usdt-mint");
  const mockBrl = key("mock-brl-mint");
  // The treasury signs nothing here or in the demo, so it gets no SOL.
  await fund(ctx, deployer, [
    { name: "listing authority", address: listingAuthority.publicKey, lamports: ROLE_LAMPORTS },
    { name: "settlement operator", address: operator.publicKey, lamports: ROLE_LAMPORTS },
    { name: "mock mint authority", address: mockAuthority.publicKey, lamports: ROLE_LAMPORTS },
    { name: "demo funder", address: funder.publicKey, lamports: Math.round(funderSol * LAMPORTS_PER_SOL) },
  ]);

  console.log("\n[2] mock currencies (Niara-controlled test tokens)");
  await ensureMockMint(ctx, mockUsdt, mockAuthority, "Niara Mock USDT (TEST)", "mockUSDT");
  await ensureMockMint(ctx, mockBrl, mockAuthority, "Niara Mock BRL (TEST)", "mockBRL");

  console.log("\n[3] initialize_config");
  const existing = await program.account.config.fetchNullable(pda.config());
  if (existing) {
    if (!existing.feeMint.equals(mockUsdt.publicKey)) {
      fail(`Config.fee_mint is ${existing.feeMint.toBase58()}, not this keys folder's MockUSDT`);
    }
    if (!existing.listingAuthority.equals(listingAuthority.publicKey)) {
      fail(`Config.listing_authority is ${existing.listingAuthority.toBase58()}, not this keys folder's key`);
    }
    console.log(
      `  config already exists: fee ${existing.feeBps} bps, cashback ${existing.cashbackBps} bps of the fee, ` +
        `timelock ${existing.timelockSecs.toString()} s, paused ${existing.paused}`,
    );
  } else {
    const instruction = await program.methods
      .initializeConfig({
        listingAuthority: listingAuthority.publicKey,
        settlementOperator: operator.publicKey,
        treasury: treasury.publicKey,
        feeMint: mockUsdt.publicKey,
        feeBps,
        cashbackBps,
        timelockSecs: bn(TIMELOCK_SECS),
      })
      .accountsPartial({
        admin: deployer.publicKey,
        config: pda.config(),
        program: program.programId,
        programData: pda.programData(),
        systemProgram: SystemProgram.programId,
      })
      .instruction();
    await send(
      ctx,
      `initialize_config (fee ${feeBps} bps, cashback ${cashbackBps} bps of the fee, timelock ${TIMELOCK_SECS} s)`,
      [instruction],
      deployer,
    );
  }

  console.log("\n[4] init_fee_vault");
  const feeVault = pda.feeVault(mockUsdt.publicKey);
  const feeLedger = pda.feeLedger(mockUsdt.publicKey);
  if (await program.account.feeLedger.fetchNullable(feeLedger)) {
    console.log(`  fee vault already exists: ${feeVault.toBase58()}`);
  } else {
    const instruction = await program.methods
      .initFeeVault()
      .accountsPartial({
        payer: deployer.publicKey,
        config: pda.config(),
        feeMint: mockUsdt.publicKey,
        vaultAuthority: pda.vaultAuthority(),
        feeVault,
        feeLedger,
        tokenProgram: TOKEN_PROGRAM,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
    await send(ctx, "init_fee_vault (MockUSDT)", [instruction], deployer);
  }

  console.log("\n[5] currency listings");
  await ensureCurrencyListing(ctx, listingAuthority, mockUsdt.publicKey, "mockUSDT");
  await ensureCurrencyListing(ctx, listingAuthority, mockBrl.publicKey, "mockBRL");

  const config = await eventually(
    () => program.account.config.fetchNullable(pda.config()),
    (value) => value !== null,
  );
  if (!config) fail("Config not found after initialize_config");
  const deployment = {
    note: "TESTNET ONLY. Public addresses, no secrets.",
    cluster: ctx.cluster,
    programId: program.programId.toBase58(),
    config: pda.config().toBase58(),
    vaultAuthority: pda.vaultAuthority().toBase58(),
    admin: config.admin.toBase58(),
    listingAuthority: config.listingAuthority.toBase58(),
    settlementOperator: config.settlementOperator.toBase58(),
    treasury: config.treasury.toBase58(),
    feeBps: config.feeBps,
    cashbackBps: config.cashbackBps,
    timelockSecs: config.timelockSecs.toNumber(),
    mockUsdtMint: mockUsdt.publicKey.toBase58(),
    mockBrlMint: mockBrl.publicKey.toBase58(),
    mockMintAuthority: mockAuthority.publicKey.toBase58(),
    feeVault: feeVault.toBase58(),
    feeLedger: feeLedger.toBase58(),
    mockUsdtListing: pda.listing(mockUsdt.publicKey).toBase58(),
    mockBrlListing: pda.listing(mockBrl.publicKey).toBase58(),
    demoFunder: funder.publicKey.toBase58(),
  };
  const deploymentPath = join(keysDir, "deployment.json");
  writeFileSync(deploymentPath, `${JSON.stringify(deployment, null, 2)}\n`);

  console.log("\nSetup complete.");
  console.log(`  program:   ${explorerAddress(ctx, program.programId)}`);
  console.log(`  config:    ${explorerAddress(ctx, pda.config())}`);
  console.log(`  MockUSDT:  ${explorerAddress(ctx, mockUsdt.publicKey)}`);
  console.log(`  MockBRL:   ${explorerAddress(ctx, mockBrl.publicKey)}`);
  console.log(`  fee vault: ${explorerAddress(ctx, feeVault)}`);
  console.log(`  addresses written to ${deploymentPath}`);
}

await main();
