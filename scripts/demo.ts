// End-to-end demo of niara_exchange (localnet or devnet), after scripts/setup.ts.
// TESTNET ONLY: test tokens, no real assets, no real money.
//
// Three flows; every transaction prints its explorer link:
//   a  Equity, order-book mode: create mint -> submit_listing -> approve_listing
//      -> issuance batch -> delegate -> settle -> withdraw_cashback
//   b  FixedIncome, P2P escrow: (mint, listing and batch as in a) -> open_deal
//      (the holder sells debentures for MockUSDT) -> accept_deal
//   c  Currency, P2P escrow: MockBRL <-> MockUSDT, open_deal (the proposer buys
//      MockBRL and escrows MockUSDT) -> accept_deal
//
// Every party (issuers, sellers, buyers) is a fresh keypair created by this
// run and saved under <keys dir>/demo-runs/<time>/. They get their SOL from the
// demo funder that setup created and return what is left at the end. The
// listing authority, settlement operator and mock-token mint authority are the
// keypairs setup created. Nothing is ever airdropped.
//
// Usage: node scripts/demo.ts [--cluster localnet|devnet] [--flow a|b|c|all]
//   NIARA_KEYS_DIR, NIARA_RPC_URL: as in scripts/setup.ts

import { randomBytes } from "node:crypto";
import { join } from "node:path";
import { parseArgs } from "node:util";

import { createApproveCheckedInstruction, createMintToCheckedInstruction } from "@solana/spl-token";
import { LAMPORTS_PER_SOL, SystemProgram } from "@solana/web3.js";
import type { Keypair, PublicKey, TransactionInstruction } from "@solana/web3.js";

import {
  TOKEN_PROGRAM,
  ata,
  bn,
  clusterTime,
  connect,
  createAtaIx,
  createKeypair,
  createMintIxs,
  docHash,
  eventually,
  expectEqual,
  explorerAddress,
  fail,
  formatUnits,
  fund,
  loadKeypair,
  parseCluster,
  pdas,
  send,
  sol,
  tokenBalance,
} from "./common.ts";
import type { Ctx } from "./common.ts";

const MOCK_DECIMALS = 6;
const MOCK_UNIT = 10n ** BigInt(MOCK_DECIMALS);
/** Securities are whole units (shares, debentures). */
const ASSET_DECIMALS = 0;
const MIN_FUNDER_LAMPORTS = 0.2 * LAMPORTS_PER_SOL;

const { values: args } = parseArgs({
  options: { cluster: { type: "string" }, flow: { type: "string", default: "all" } },
});

interface Demo {
  ctx: Ctx;
  pda: ReturnType<typeof pdas>;
  usdt: PublicKey;
  brl: PublicKey;
  listingAuthority: Keypair;
  operator: Keypair;
  mockAuthority: Keypair;
  funder: Keypair;
  /** Creates a party of this run: a new keypair, saved in the run folder. */
  party: (name: string) => Keypair;
  parties: Keypair[];
}

type Step = (label: string, instructions: TransactionInstruction[], payer: Keypair, ...cosigners: Keypair[]) => Promise<string>;

/** Numbers the transactions of one flow: [a.1], [a.2], ... */
function stepper(demo: Demo, flow: string): Step {
  let count = 0;
  return (label, instructions, payer, ...cosigners) => {
    count += 1;
    return send(demo.ctx, `[${flow}.${count}] ${label}`, instructions, payer, ...cosigners);
  };
}

const usdt = (raw: bigint): string => `${formatUnits(raw, MOCK_DECIMALS)} mockUSDT`;

/** Same math as the program (spec §3.4): the fee rounds up, the cashback down. */
function feeSplit(usdtAmount: bigint, feeBps: number, cashbackBps: number, cashbackEligible: boolean) {
  const fee = (usdtAmount * BigInt(feeBps) + 9_999n) / 10_000n;
  const cashback = cashbackEligible ? (fee * BigInt(cashbackBps)) / 10_000n : 0n;
  return { fee, cashback };
}

/** The mock-token mint authority (Niara) mints test currency to a party. */
function mintMockIx(demo: Demo, mint: PublicKey, owner: PublicKey, amount: bigint): TransactionInstruction {
  return createMintToCheckedInstruction(
    mint,
    ata(mint, owner),
    demo.mockAuthority.publicKey,
    amount,
    MOCK_DECIMALS,
    [],
    TOKEN_PROGRAM,
  );
}

/**
 * Native issuance (spec §3.12): the issuer creates the mint and submits the
 * listing (the mint authority moves to the program), Niara approves the
 * listing, the issuer requests batch #1 and Niara approves it, which mints to
 * the holder. Ends with the listing's cashback ledger. Returns the mint.
 */
async function issueAsset(
  demo: Demo,
  step: Step,
  params: {
    issuer: Keypair;
    holder: Keypair;
    assetClass: "equity" | "fixedIncome";
    name: string;
    symbol: string;
    amount: bigint;
  },
): Promise<PublicKey> {
  const { ctx, pda } = demo;
  const { program } = ctx;
  const { issuer, holder, name, symbol, amount } = params;
  const mint = demo.party(`${symbol}-mint`);
  const listing = pda.listing(mint.publicKey);

  await step(
    `issuer creates the mint "${name}" (${symbol}) ${mint.publicKey.toBase58()}`,
    await createMintIxs(ctx, {
      payer: issuer.publicKey,
      mint: mint.publicKey,
      authority: issuer.publicKey,
      decimals: ASSET_DECIMALS,
      name,
      symbol,
    }),
    issuer,
    mint,
  );

  const assetClass = params.assetClass === "equity" ? { equity: {} } : { fixedIncome: {} };
  await step(
    `submit_listing (${params.assetClass}): the mint authority moves to the program`,
    [
      await program.methods
        .submitListing(assetClass, docHash(`TEST offering document of ${name}, ${mint.publicKey.toBase58()}`))
        .accountsPartial({
          issuer: issuer.publicKey,
          assetMint: mint.publicKey,
          listing,
          mintAuthority: pda.mintAuthority(mint.publicKey),
          tokenProgram: TOKEN_PROGRAM,
          systemProgram: SystemProgram.programId,
        })
        .instruction(),
    ],
    issuer,
  );

  await step(
    "approve_listing (Niara listing authority)",
    [
      await program.methods
        .approveListing()
        .accountsPartial({ listingAuthority: demo.listingAuthority.publicKey, config: pda.config(), listing })
        .instruction(),
    ],
    demo.listingAuthority,
  );

  const batchNo = (await eventually(() => program.account.listing.fetch(listing), () => true)).nextBatchNo;
  const batch = pda.batch(listing, batchNo);
  await step(
    `request_issuance: batch #${batchNo.toString()}, ${amount} ${symbol} to the holder`,
    [
      await program.methods
        .requestIssuance(bn(amount), holder.publicKey, docHash(`TEST corporate act for batch ${batchNo.toString()} of ${name}`))
        .accountsPartial({ issuer: issuer.publicKey, listing, batch, systemProgram: SystemProgram.programId })
        .instruction(),
    ],
    issuer,
  );

  await step("holder creates its token account", [createAtaIx(holder.publicKey, mint.publicKey, holder.publicKey)], holder);
  await step(
    `approve_issuance (Niara listing authority): the program mints batch #${batchNo.toString()}`,
    [
      await program.methods
        .approveIssuance()
        .accountsPartial({
          listingAuthority: demo.listingAuthority.publicKey,
          config: pda.config(),
          listing,
          batch,
          assetMint: mint.publicKey,
          mintAuthority: pda.mintAuthority(mint.publicKey),
          destination: ata(mint.publicKey, holder.publicKey),
          tokenProgram: TOKEN_PROGRAM,
        })
        .instruction(),
    ],
    demo.listingAuthority,
  );
  await expectEqual(`holder's ${symbol} balance`, () => tokenBalance(ctx, ata(mint.publicKey, holder.publicKey)), amount);

  await step(
    "init_cashback_ledger (permissionless; the issuer pays the rent)",
    [
      await program.methods
        .initCashbackLedger()
        .accountsPartial({
          payer: issuer.publicKey,
          config: pda.config(),
          listing,
          cashbackLedger: pda.cashbackLedger(listing, demo.usdt),
          systemProgram: SystemProgram.programId,
        })
        .instruction(),
    ],
    issuer,
  );
  console.log(`      listing: ${explorerAddress(ctx, listing)}`);
  return mint.publicKey;
}

// ---- flow a: Equity, order book (delegate + settle) -----------------------------

async function flowEquity(demo: Demo): Promise<void> {
  console.log("\n=== Flow a: Equity, order-book mode (delegate -> settle -> withdraw_cashback) ===");
  const { ctx, pda } = demo;
  const { program } = ctx;
  const step = stepper(demo, "a");
  const issuer = demo.party("equity-issuer");
  const seller = demo.party("equity-seller");
  const buyer = demo.party("equity-buyer");
  await fund(ctx, demo.funder, [
    { name: "issuer", address: issuer.publicKey, lamports: 0.02 * LAMPORTS_PER_SOL },
    { name: "seller", address: seller.publicKey, lamports: 0.01 * LAMPORTS_PER_SOL },
    { name: "buyer", address: buyer.publicKey, lamports: 0.01 * LAMPORTS_PER_SOL },
  ]);

  const shares = 100n;
  const payment = 2_500n * MOCK_UNIT;
  const mint = await issueAsset(demo, step, {
    issuer,
    holder: seller,
    assetClass: "equity",
    name: "Niara Demo Equity (TEST)",
    symbol: "tEQTY",
    amount: 1_000n,
  });
  const listing = pda.listing(mint);
  const cashbackLedger = pda.cashbackLedger(listing, demo.usdt);

  await step(
    `buyer and seller create their token accounts; Niara mints ${usdt(payment)} (test currency) to the buyer`,
    [
      createAtaIx(buyer.publicKey, mint, buyer.publicKey),
      createAtaIx(buyer.publicKey, demo.usdt, buyer.publicKey),
      createAtaIx(seller.publicKey, demo.usdt, seller.publicKey),
      mintMockIx(demo, demo.usdt, buyer.publicKey, payment),
    ],
    buyer,
    seller,
    demo.mockAuthority,
  );

  // The order goes live: each side approves the exact amount to the vault PDA.
  // Tokens stay in the wallets until settle (spec §3.5).
  const vaultAuthority = pda.vaultAuthority();
  await step(
    `seller delegates ${shares} tEQTY to the program's vault PDA`,
    [createApproveCheckedInstruction(ata(mint, seller.publicKey), mint, vaultAuthority, seller.publicKey, shares, ASSET_DECIMALS, [], TOKEN_PROGRAM)],
    seller,
  );
  await step(
    `buyer delegates ${usdt(payment)} to the program's vault PDA`,
    [createApproveCheckedInstruction(ata(demo.usdt, buyer.publicKey), demo.usdt, vaultAuthority, buyer.publicKey, payment, MOCK_DECIMALS, [], TOKEN_PROGRAM)],
    buyer,
  );

  const config = await program.account.config.fetch(pda.config());
  const { fee, cashback } = feeSplit(payment, config.feeBps, config.cashbackBps, true);
  const matchId = randomBytes(16);
  await step(
    `settle (operator): match ${matchId.toString("hex")}, ${shares} tEQTY for ${usdt(payment)}`,
    [
      await program.methods
        .settle(Array.from(matchId), bn(shares), bn(payment))
        .accountsPartial({
          settlementOperator: demo.operator.publicKey,
          config: pda.config(),
          assetListing: listing,
          paymentListing: pda.listing(demo.usdt),
          assetMint: mint,
          paymentMint: demo.usdt,
          seller: seller.publicKey,
          buyer: buyer.publicKey,
          sellerAssetAccount: ata(mint, seller.publicKey),
          buyerAssetAccount: ata(mint, buyer.publicKey),
          buyerPaymentAccount: ata(demo.usdt, buyer.publicKey),
          sellerPaymentAccount: ata(demo.usdt, seller.publicKey),
          vaultAuthority,
          feeLedger: pda.feeLedger(demo.usdt),
          feeVault: pda.feeVault(demo.usdt),
          cashbackLedger,
          settlementRecord: pda.settlementRecord(matchId),
          assetTokenProgram: TOKEN_PROGRAM,
          paymentTokenProgram: TOKEN_PROGRAM,
          systemProgram: SystemProgram.programId,
        })
        .instruction(),
    ],
    demo.operator,
  );
  await expectEqual("buyer's tEQTY", () => tokenBalance(ctx, ata(mint, buyer.publicKey)), shares);
  await expectEqual("seller's mockUSDT (payment - fee)", () => tokenBalance(ctx, ata(demo.usdt, seller.publicKey)), payment - fee);
  await expectEqual(
    "cashback accrued to the issuer",
    async () => BigInt((await program.account.cashbackLedger.fetch(cashbackLedger)).accrued.toString()),
    cashback,
  );
  console.log(`      fee ${usdt(fee)} (${config.feeBps} bps), of which cashback ${usdt(cashback)}`);

  if (cashback === 0n) {
    console.log("  no cashback accrued at the current rates; skipping withdraw_cashback");
    return;
  }
  await step(
    `withdraw_cashback (issuer): ${usdt(cashback)}`,
    [
      createAtaIx(issuer.publicKey, demo.usdt, issuer.publicKey),
      await program.methods
        .withdrawCashback(bn(cashback))
        .accountsPartial({
          issuer: issuer.publicKey,
          config: pda.config(),
          listing,
          paymentMint: demo.usdt,
          cashbackLedger,
          feeLedger: pda.feeLedger(demo.usdt),
          vaultAuthority,
          feeVault: pda.feeVault(demo.usdt),
          destination: ata(demo.usdt, issuer.publicKey),
          tokenProgram: TOKEN_PROGRAM,
        })
        .instruction(),
    ],
    issuer,
  );
  await expectEqual("issuer's mockUSDT (cashback)", () => tokenBalance(ctx, ata(demo.usdt, issuer.publicKey)), cashback);
}

// ---- P2P escrow: open_deal -> accept_deal (flows b and c) -----------------------

/**
 * One bilateral deal (spec §3.5). The proposer escrows its side in the deal
 * vault and names the counterparty; the counterparty accepts and the program
 * swaps both legs atomically. MockUSDT is the payment mint in both flows, so
 * the seller of the asset receives it and pays the fee.
 */
async function runDeal(
  demo: Demo,
  step: Step,
  params: {
    proposer: Keypair;
    counterparty: Keypair;
    proposerSide: "sell" | "buy";
    assetMint: PublicKey;
    assetAmount: bigint;
    /** The asset amount as shown to a person, e.g. "10 tDEB". */
    assetLabel: string;
    assetSymbol: string;
    paymentAmount: bigint;
    cashbackEligible: boolean;
  },
): Promise<void> {
  const { ctx, pda } = demo;
  const { program } = ctx;
  const { proposer, counterparty, proposerSide, assetMint, assetAmount, paymentAmount } = params;
  const selling = proposerSide === "sell";
  const assetListing = pda.listing(assetMint);
  const dealId = randomBytes(16);
  const deal = pda.deal(proposer.publicKey, dealId);
  const dealVault = pda.dealVault(deal);
  const feeLedger = pda.feeLedger(demo.usdt);
  const feesBefore = await program.account.feeLedger.fetch(feeLedger);
  const market = {
    config: pda.config(),
    assetMint,
    paymentMint: demo.usdt,
    assetListing,
    paymentListing: pda.listing(demo.usdt),
    deal,
    dealVault,
    assetTokenProgram: TOKEN_PROGRAM,
    paymentTokenProgram: TOKEN_PROGRAM,
  };

  const expiresAt = (await clusterTime(ctx)) + 3_600;
  await step(
    `open_deal ${dealId.toString("hex")}: the proposer ${selling ? "sells" : "buys"} ${params.assetLabel} for ` +
      `${usdt(paymentAmount)} and escrows ${selling ? params.assetLabel : usdt(paymentAmount)} (expires in 1 h)`,
    [
      await program.methods
        .openDeal(
          Array.from(dealId),
          {
            counterparty: counterparty.publicKey,
            proposerSide: selling ? { sell: {} } : { buy: {} },
            assetAmount: bn(assetAmount),
            paymentAmount: bn(paymentAmount),
          },
          bn(expiresAt),
        )
        .accountsPartial({
          ...market,
          proposer: proposer.publicKey,
          proposerDepositAccount: ata(selling ? assetMint : demo.usdt, proposer.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .instruction(),
    ],
    proposer,
  );
  const opened = await eventually(() => program.account.deal.fetch(deal), () => true);
  await expectEqual("deal vault (the escrowed deposit)", () => tokenBalance(ctx, dealVault), selling ? assetAmount : paymentAmount);

  // The rates were fixed at open_deal: the fee is part of the agreed terms.
  const { fee, cashback } = feeSplit(paymentAmount, opened.feeBps, opened.cashbackBps, params.cashbackEligible);
  await step(
    "accept_deal (counterparty): both legs swap atomically, the fee goes to the fee vault",
    [
      await program.methods
        .acceptDeal()
        .accountsPartial({
          ...market,
          counterparty: counterparty.publicKey,
          proposer: proposer.publicKey,
          counterpartyAssetAccount: ata(assetMint, counterparty.publicKey),
          counterpartyPaymentAccount: ata(demo.usdt, counterparty.publicKey),
          proposerProceedsAccount: ata(selling ? demo.usdt : assetMint, proposer.publicKey),
          feeLedger,
          feeVault: pda.feeVault(demo.usdt),
          // Currency assets never accrue cashback: no ledger (optional account).
          cashbackLedger: params.cashbackEligible ? pda.cashbackLedger(assetListing, demo.usdt) : null,
        })
        .instruction(),
    ],
    counterparty,
  );

  const [seller, buyer] = selling ? [proposer, counterparty] : [counterparty, proposer];
  await expectEqual(`buyer's ${params.assetSymbol}`, () => tokenBalance(ctx, ata(assetMint, buyer.publicKey)), assetAmount);
  await expectEqual("seller's mockUSDT (payment - fee)", () => tokenBalance(ctx, ata(demo.usdt, seller.publicKey)), paymentAmount - fee);
  const ledgerDelta = (field: "feesCollected" | "cashbackAccrued") => async () =>
    BigInt((await program.account.feeLedger.fetch(feeLedger))[field].sub(feesBefore[field]).toString());
  await expectEqual("fee recorded in the fee ledger", ledgerDelta("feesCollected"), fee);
  await expectEqual("cashback recorded in the fee ledger", ledgerDelta("cashbackAccrued"), cashback);
  const vault = await eventually(async () => ({ info: await ctx.connection.getAccountInfo(dealVault) }), (value) => value.info === null);
  if (vault.info !== null) throw new Error("check failed: the deal vault is still open");
  const executed = await eventually(() => program.account.deal.fetch(deal), (value) => "executed" in value.status);
  if (!("executed" in executed.status)) throw new Error("check failed: the deal is not Executed");
  console.log("  ok: deal Executed, vault closed");
  console.log(`      fee ${usdt(fee)} (${opened.feeBps} bps), of which cashback ${usdt(cashback)}`);
  console.log(`      deal: ${explorerAddress(ctx, deal)}`);
}

async function flowFixedIncome(demo: Demo): Promise<void> {
  console.log("\n=== Flow b: FixedIncome, P2P escrow (open_deal -> accept_deal) ===");
  const { ctx } = demo;
  const step = stepper(demo, "b");
  const issuer = demo.party("bond-issuer");
  const holder = demo.party("bond-seller");
  const buyer = demo.party("bond-buyer");
  await fund(ctx, demo.funder, [
    { name: "issuer", address: issuer.publicKey, lamports: 0.02 * LAMPORTS_PER_SOL },
    { name: "holder (proposer)", address: holder.publicKey, lamports: 0.015 * LAMPORTS_PER_SOL },
    { name: "buyer (counterparty)", address: buyer.publicKey, lamports: 0.01 * LAMPORTS_PER_SOL },
  ]);

  const units = 10n;
  const payment = 10_000n * MOCK_UNIT;
  const mint = await issueAsset(demo, step, {
    issuer,
    holder,
    assetClass: "fixedIncome",
    name: "Niara Demo Debenture (TEST)",
    symbol: "tDEB",
    amount: 50n,
  });
  await step(
    `both parties create their token accounts; Niara mints ${usdt(payment)} (test currency) to the buyer`,
    [
      createAtaIx(holder.publicKey, demo.usdt, holder.publicKey),
      createAtaIx(buyer.publicKey, mint, buyer.publicKey),
      createAtaIx(buyer.publicKey, demo.usdt, buyer.publicKey),
      mintMockIx(demo, demo.usdt, buyer.publicKey, payment),
    ],
    buyer,
    holder,
    demo.mockAuthority,
  );
  await runDeal(demo, step, {
    proposer: holder,
    counterparty: buyer,
    proposerSide: "sell",
    assetMint: mint,
    assetAmount: units,
    assetLabel: `${units} tDEB`,
    assetSymbol: "tDEB",
    paymentAmount: payment,
    cashbackEligible: true,
  });
}

async function flowCurrency(demo: Demo): Promise<void> {
  console.log("\n=== Flow c: Currency, P2P escrow (MockBRL <-> MockUSDT) ===");
  const { ctx } = demo;
  const step = stepper(demo, "c");
  const usdtHolder = demo.party("fx-brl-buyer");
  const brlHolder = demo.party("fx-brl-seller");
  await fund(ctx, demo.funder, [
    { name: "mockBRL buyer (proposer)", address: usdtHolder.publicKey, lamports: 0.015 * LAMPORTS_PER_SOL },
    { name: "mockBRL seller (counterparty)", address: brlHolder.publicKey, lamports: 0.01 * LAMPORTS_PER_SOL },
  ]);

  const brl = 5_000n * MOCK_UNIT;
  const payment = 1_000n * MOCK_UNIT;
  await step(
    `both parties create their token accounts; Niara mints ${usdt(payment)} to the proposer and ` +
      `${formatUnits(brl, MOCK_DECIMALS)} mockBRL to the counterparty (test currencies)`,
    [
      createAtaIx(usdtHolder.publicKey, demo.usdt, usdtHolder.publicKey),
      createAtaIx(usdtHolder.publicKey, demo.brl, usdtHolder.publicKey),
      createAtaIx(brlHolder.publicKey, demo.usdt, brlHolder.publicKey),
      createAtaIx(brlHolder.publicKey, demo.brl, brlHolder.publicKey),
      mintMockIx(demo, demo.usdt, usdtHolder.publicKey, payment),
      mintMockIx(demo, demo.brl, brlHolder.publicKey, brl),
    ],
    usdtHolder,
    brlHolder,
    demo.mockAuthority,
  );
  await runDeal(demo, step, {
    proposer: usdtHolder,
    counterparty: brlHolder,
    proposerSide: "buy",
    assetMint: demo.brl,
    assetAmount: brl,
    assetLabel: `${formatUnits(brl, MOCK_DECIMALS)} mockBRL`,
    assetSymbol: "mockBRL",
    paymentAmount: payment,
    cashbackEligible: false,
  });
}

// ---- run ------------------------------------------------------------------------

/** Returns the parties' unused SOL to the demo funder (the funder pays the fees). */
async function returnSol(demo: Demo): Promise<void> {
  const { ctx, funder } = demo;
  const balances = await Promise.all(demo.parties.map((party) => ctx.connection.getBalance(party.publicKey)));
  const funded = demo.parties
    .map((party, index) => ({ party, lamports: balances[index] ?? 0 }))
    .filter((entry) => entry.lamports > 0);
  for (let start = 0; start < funded.length; start += 5) {
    const chunk = funded.slice(start, start + 5);
    // Best effort: the keypairs stay in the run folder, so nothing is lost if this fails.
    await send(
      ctx,
      `return unused SOL to the demo funder (${chunk.length} parties)`,
      chunk.map(({ party, lamports }) =>
        SystemProgram.transfer({ fromPubkey: party.publicKey, toPubkey: funder.publicKey, lamports }),
      ),
      funder,
      ...chunk.map(({ party }) => party),
    ).catch((error: unknown) => console.error(`  warning: ${(error as Error).message}`));
  }
}

async function main(): Promise<void> {
  const flows = { a: flowEquity, b: flowFixedIncome, c: flowCurrency };
  const flow = args.flow ?? "all";
  if (flow !== "all" && !(flow in flows)) fail("--flow must be a, b, c or all");
  const selected = flow === "all" ? Object.values(flows) : [flows[flow as keyof typeof flows]];

  const ctx = await connect(parseCluster(args.cluster));
  const { program, keysDir } = ctx;
  const pda = pdas(program.programId);
  const key = (name: string) => loadKeypair(join(keysDir, `${name}.json`));

  const config = await program.account.config.fetchNullable(pda.config());
  if (!config) fail("the program has no Config yet: run scripts/setup.ts first");
  if (config.paused) fail("the protocol is paused");
  const listingAuthority = key("listing-authority");
  const operator = key("settlement-operator");
  const usdtMint = key("mock-usdt-mint").publicKey;
  if (
    !config.listingAuthority.equals(listingAuthority.publicKey) ||
    !config.settlementOperator.equals(operator.publicKey) ||
    !config.feeMint.equals(usdtMint)
  ) {
    fail("the keys folder does not match the on-chain Config (authorities or fee mint)");
  }

  const runDir = join(keysDir, "demo-runs", new Date().toISOString().replace(/[:.]/g, "-"));
  const parties: Keypair[] = [];
  const demo: Demo = {
    ctx,
    pda,
    usdt: usdtMint,
    brl: key("mock-brl-mint").publicKey,
    listingAuthority,
    operator,
    mockAuthority: key("mock-mint-authority"),
    funder: key("demo-funder"),
    party: (name) => {
      const keypair = createKeypair(join(runDir, `${name}.json`));
      if (!name.endsWith("-mint")) parties.push(keypair);
      return keypair;
    },
    parties,
  };
  const funderBalance = await ctx.connection.getBalance(demo.funder.publicKey);
  console.log(`funder:   ${demo.funder.publicKey.toBase58()} (${sol(funderBalance)})`);
  console.log(`run keys: ${runDir}`);
  if (funderBalance < MIN_FUNDER_LAMPORTS) {
    fail(`the demo funder needs at least ${sol(MIN_FUNDER_LAMPORTS)}: run scripts/setup.ts again or send it SOL by hand`);
  }

  try {
    for (const run of selected) await run(demo);
    console.log("\nDemo complete: every check passed.");
  } finally {
    console.log("\n=== Clean-up ===");
    await returnSol(demo);
    const spent = funderBalance - (await ctx.connection.getBalance(demo.funder.publicKey));
    console.log(`  this run cost the demo funder ${sol(spent)} (fees and rent)`);
  }
}

await main();
