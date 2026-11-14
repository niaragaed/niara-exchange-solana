// On-chain refusals of niara_exchange (localnet or devnet), after scripts/setup.ts.
// TESTNET ONLY: test tokens, no real assets, no real money.
//
// Every case below is a transaction that the program must refuse. Each one is
// sent with skipPreflight, so it lands on the cluster as a FAILED transaction
// with its explorer link, and the script then reads it back and asserts the
// exact Anchor error (the custom error code of the failed instruction and the
// "Error Code: <Name>" log line). A case that succeeds, or fails with another
// error, stops the script.
//
//   1  approve_listing on a Quoted listing                 ListingFeeNotPaid
//   2  pay_listing_fee after the quote expired             QuoteExpired
//   3  request_issuance on a Quoted listing                ListingNotIssuable
//   4  settle on a listing that is not Approved            ListingNotApproved
//   5  settle with a zero match_id                         ZeroMatchId
//   6  pay_listing_fee while paused (then unpause)         Paused
//
// It creates one new Equity listing (issuer, mint, submit_listing,
// quote_listing) and leaves it Quoted: the listings of scripts/demo.ts are all
// Approved. Case 5 reuses the demo's flow-a listing when a demo run exists in
// the keys folder (its seller, buyer and accounts), and falls back to the new
// listing otherwise. Case 6 needs the admin (the deployer); unpause is not
// timelocked. The issuer is a fresh keypair saved under
// <keys dir>/refusal-runs/<time>/, funded by the demo funder, which gets the
// unused SOL back at the end. Nothing is ever airdropped.
//
// Usage: node scripts/demo-refusals.ts [--cluster localnet|devnet] [--skip-pause]
//   NIARA_KEYS_DIR, NIARA_RPC_URL: as in scripts/setup.ts
//   NIARA_DEPLOYER: deployer keypair (the admin), default
//                   ~/.config/solana/niara-exchange-deployer.json

import { randomBytes } from "node:crypto";
import { existsSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";

import { createMintToCheckedInstruction } from "@solana/spl-token";
import { Keypair, LAMPORTS_PER_SOL, SystemProgram, Transaction } from "@solana/web3.js";
import type { PublicKey, TransactionInstruction } from "@solana/web3.js";

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
  explorerTx,
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
import type { NiaraExchange } from "../target/types/niara_exchange.ts";

const MOCK_DECIMALS = 6;
const MOCK_UNIT = 10n ** BigInt(MOCK_DECIMALS);
const MIN_FUNDER_LAMPORTS = 0.1 * LAMPORTS_PER_SOL;
/** Same quote as the demo (spec §3.13): 1,000,000 mockUSDT valuation at 10 bps. */
const VALUATION = 1_000_000n * MOCK_UNIT;
const LISTING_FEE_BPS = 10;
const LISTING_FEE = (VALUATION * BigInt(LISTING_FEE_BPS) + 9_999n) / 10_000n;
/** The quote must outlive the first cases and then expire for case 2. */
const QUOTE_TTL_SECS = 75;

/** The Rust name, as the program logs it ("Error Code: ListingFeeNotPaid"); the IDL spells it in camelCase. */
type ErrorName = Capitalize<NiaraExchange["errors"][number]["name"]>;

const { values: args } = parseArgs({
  options: { cluster: { type: "string" }, "skip-pause": { type: "boolean", default: false } },
});

interface Recorded {
  label: string;
  expected: ErrorName;
  signature: string;
}
const recorded: Recorded[] = [];

/**
 * Sends a transaction that must fail, with skipPreflight so that it lands, and
 * asserts how it failed: the failed instruction's custom error code must be the
 * one of `expected` in the IDL, and the logs must name it.
 */
async function sendRefused(
  ctx: Ctx,
  label: string,
  expected: ErrorName,
  instructions: TransactionInstruction[],
  payer: Keypair,
  ...cosigners: Keypair[]
): Promise<string> {
  const entry = ctx.program.idl.errors?.find((error) => error.name.toLowerCase() === expected.toLowerCase());
  if (!entry) throw new Error(`the IDL has no error named ${expected}`);

  const { blockhash, lastValidBlockHeight } = await ctx.connection.getLatestBlockhash("confirmed");
  const transaction = new Transaction({ feePayer: payer.publicKey, blockhash, lastValidBlockHeight }).add(...instructions);
  transaction.sign(payer, ...cosigners.filter((signer) => !signer.publicKey.equals(payer.publicKey)));
  const signature = await ctx.connection.sendRawTransaction(transaction.serialize(), { skipPreflight: true });
  const confirmation = await ctx.connection.confirmTransaction({ signature, blockhash, lastValidBlockHeight }, "confirmed");
  console.log(`  ${label}\n      ${explorerTx(ctx, signature)}`);
  if (confirmation.value.err === null) throw new Error(`check failed: "${label}" succeeded, it must be refused`);

  const fetched = await eventually(
    () => ctx.connection.getTransaction(signature, { commitment: "confirmed", maxSupportedTransactionVersion: 0 }),
    (value) => value !== null,
  );
  if (!fetched?.meta) throw new Error(`check failed: cannot read back ${signature}`);
  const err = fetched.meta.err as { InstructionError?: [number, { Custom?: number } | string] } | string | null;
  const detail = typeof err === "object" && err !== null ? err.InstructionError?.[1] : undefined;
  const code = typeof detail === "object" ? detail.Custom : undefined;
  if (code !== entry.code) {
    throw new Error(
      `check failed: "${label}" failed with ${JSON.stringify(err)}, expected ${expected} (${entry.code}).\n` +
        (fetched.meta.logMessages ?? []).join("\n"),
    );
  }
  const logs = fetched.meta.logMessages ?? [];
  if (!logs.some((line) => line.includes(`Error Code: ${expected}.`))) {
    throw new Error(`check failed: the logs of "${label}" do not say "Error Code: ${expected}":\n${logs.join("\n")}`);
  }
  console.log(`  ok: refused with ${expected} (custom error ${entry.code})`);
  recorded.push({ label, expected, signature });
  return signature;
}

function latestDemoEquity(keysDir: string): { mint: PublicKey; seller: PublicKey; buyer: PublicKey } | undefined {
  const root = join(keysDir, "demo-runs");
  if (!existsSync(root)) return undefined;
  for (const run of readdirSync(root).sort().reverse()) {
    const files = ["tEQTY-mint", "equity-seller", "equity-buyer"].map((name) => join(root, run, `${name}.json`));
    if (files.every((file) => existsSync(file))) {
      const [mint, seller, buyer] = files.map((file) => loadKeypair(file).publicKey);
      if (mint && seller && buyer) return { mint, seller, buyer };
    }
  }
  return undefined;
}

async function main(): Promise<void> {
  const ctx = await connect(parseCluster(args.cluster));
  const { program, keysDir } = ctx;
  const pda = pdas(program.programId);
  const key = (name: string) => loadKeypair(join(keysDir, `${name}.json`));

  const config = await program.account.config.fetchNullable(pda.config());
  if (!config) fail("the program has no Config yet: run scripts/setup.ts first");
  if (config.paused) fail("the protocol is paused");
  const listingAuthority = key("listing-authority");
  const operator = key("settlement-operator");
  const mockAuthority = key("mock-mint-authority");
  const funder = key("demo-funder");
  const usdt = key("mock-usdt-mint").publicKey;
  if (
    !config.listingAuthority.equals(listingAuthority.publicKey) ||
    !config.settlementOperator.equals(operator.publicKey) ||
    !config.feeMint.equals(usdt)
  ) {
    fail("the keys folder does not match the on-chain Config (authorities or fee mint)");
  }
  let admin: Keypair | undefined;
  if (!args["skip-pause"]) {
    admin = loadKeypair(process.env.NIARA_DEPLOYER ?? join(homedir(), ".config", "solana", "niara-exchange-deployer.json"));
    if (!config.admin.equals(admin.publicKey)) {
      fail(`the deployer ${admin.publicKey.toBase58()} is not the admin of the Config; use NIARA_DEPLOYER or --skip-pause`);
    }
  }
  const funderBalance = await ctx.connection.getBalance(funder.publicKey);
  console.log(`funder:   ${funder.publicKey.toBase58()} (${sol(funderBalance)})`);
  if (funderBalance < MIN_FUNDER_LAMPORTS) fail(`the demo funder needs at least ${sol(MIN_FUNDER_LAMPORTS)}`);

  const runDir = join(keysDir, "refusal-runs", new Date().toISOString().replace(/[:.]/g, "-"));
  console.log(`run keys: ${runDir}`);
  const issuer = createKeypair(join(runDir, "issuer.json"));
  const mint = createKeypair(join(runDir, "tREF-mint.json"));
  const outsider = Keypair.generate(); // only an owner address (never signs, holds nothing), so it is not saved
  const listing = pda.listing(mint.publicKey);
  const cashbackLedger = pda.cashbackLedger(listing, usdt);

  try {
    console.log("\n=== Setup: one new Equity listing, left Quoted (the demo's listings are all Approved) ===");
    let count = 0;
    const step = (label: string, instructions: TransactionInstruction[], payer: Keypair, ...cosigners: Keypair[]) => {
      count += 1;
      return send(ctx, `[s.${count}] ${label}`, instructions, payer, ...cosigners);
    };
    await fund(ctx, funder, [{ name: "issuer", address: issuer.publicKey, lamports: 0.03 * LAMPORTS_PER_SOL }]);
    await step(
      `issuer creates the mint "Niara Refusal Test (TEST)" (tREF) ${mint.publicKey.toBase58()}`,
      await createMintIxs(ctx, {
        payer: issuer.publicKey,
        mint: mint.publicKey,
        authority: issuer.publicKey,
        decimals: 0,
        name: "Niara Refusal Test (TEST)",
        symbol: "tREF",
      }),
      issuer,
      mint,
    );
    await step(
      "submit_listing (equity): the mint authority moves to the program",
      [
        await program.methods
          .submitListing({ equity: {} }, docHash(`TEST refusal listing, ${mint.publicKey.toBase58()}`))
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
    const expiresAt = (await clusterTime(ctx)) + QUOTE_TTL_SECS;
    await step(
      `quote_listing: ${formatUnits(LISTING_FEE, MOCK_DECIMALS)} mockUSDT, payable for ${QUOTE_TTL_SECS} s`,
      [
        await program.methods
          .quoteListing(bn(VALUATION), LISTING_FEE_BPS, bn(expiresAt))
          .accountsPartial({ listingAuthority: listingAuthority.publicKey, config: pda.config(), listing })
          .instruction(),
      ],
      listingAuthority,
    );
    await step(
      "token accounts; Niara mints the quoted fee (test currency) to the issuer, so a refusal is never about funds",
      [
        createAtaIx(issuer.publicKey, usdt, issuer.publicKey),
        createAtaIx(issuer.publicKey, mint.publicKey, issuer.publicKey),
        createAtaIx(issuer.publicKey, mint.publicKey, outsider.publicKey),
        createAtaIx(issuer.publicKey, usdt, outsider.publicKey),
        createMintToCheckedInstruction(usdt, ata(usdt, issuer.publicKey), mockAuthority.publicKey, LISTING_FEE, MOCK_DECIMALS, [], TOKEN_PROGRAM),
      ],
      issuer,
      mockAuthority,
    );
    await step(
      "init_cashback_ledger (permissionless; settle needs it to exist)",
      [
        await program.methods
          .initCashbackLedger()
          .accountsPartial({
            payer: issuer.publicKey,
            config: pda.config(),
            listing,
            cashbackLedger,
            systemProgram: SystemProgram.programId,
          })
          .instruction(),
      ],
      issuer,
    );

    const payIx = () =>
      program.methods
        .payListingFee()
        .accountsPartial({
          issuer: issuer.publicKey,
          config: pda.config(),
          listing,
          feeMint: usdt,
          feeLedger: pda.feeLedger(usdt),
          feeVault: pda.feeVault(usdt),
          source: ata(usdt, issuer.publicKey),
          tokenProgram: TOKEN_PROGRAM,
        })
        .instruction();
    const settleIx = (target: { mint: PublicKey; seller: PublicKey; buyer: PublicKey }, matchId: Uint8Array) =>
      program.methods
        .settle(Array.from(matchId), bn(1), bn(MOCK_UNIT))
        .accountsPartial({
          settlementOperator: operator.publicKey,
          config: pda.config(),
          assetListing: pda.listing(target.mint),
          paymentListing: pda.listing(usdt),
          assetMint: target.mint,
          paymentMint: usdt,
          seller: target.seller,
          buyer: target.buyer,
          sellerAssetAccount: ata(target.mint, target.seller),
          buyerAssetAccount: ata(target.mint, target.buyer),
          buyerPaymentAccount: ata(usdt, target.buyer),
          sellerPaymentAccount: ata(usdt, target.seller),
          vaultAuthority: pda.vaultAuthority(),
          feeLedger: pda.feeLedger(usdt),
          feeVault: pda.feeVault(usdt),
          cashbackLedger: pda.cashbackLedger(pda.listing(target.mint), usdt),
          settlementRecord: pda.settlementRecord(matchId),
          assetTokenProgram: TOKEN_PROGRAM,
          paymentTokenProgram: TOKEN_PROGRAM,
          systemProgram: SystemProgram.programId,
        })
        .instruction();

    console.log("\n=== Refusals ===");
    let refusals = 0;
    const refuse = (label: string, expected: ErrorName, instructions: TransactionInstruction[], payer: Keypair, ...cosigners: Keypair[]) => {
      refusals += 1;
      return sendRefused(ctx, `[r.${refusals}] ${label}`, expected, instructions, payer, ...cosigners);
    };

    // Case 6 first: it needs the quote to be still valid, so that Paused is the
    // only thing wrong with the payment.
    if (admin) {
      const stillValid = expiresAt - (await clusterTime(ctx));
      if (stillValid < 15) fail("the quote is about to expire; run the script again");
      await step(
        "pause (admin)",
        [await program.methods.pause().accountsPartial({ admin: admin.publicKey, config: pda.config() }).instruction()],
        admin,
      );
      try {
        await refuse("pay_listing_fee while paused", "Paused", [await payIx()], issuer);
      } finally {
        // Unpause is not timelocked. Whatever happened above, do not leave the protocol paused.
        await step(
          "unpause (admin)",
          [await program.methods.unpause().accountsPartial({ admin: admin.publicKey, config: pda.config() }).instruction()],
          admin,
        );
      }
      const after = await eventually(() => program.account.config.fetch(pda.config()), (value) => !value.paused);
      if (after.paused) throw new Error("check failed: the protocol is still paused after unpause");
      console.log("  ok: the protocol is unpaused");
    } else {
      console.log("  skipped: pay_listing_fee while paused (--skip-pause)");
    }

    await refuse(
      "approve_listing on a Quoted listing: it only reinstates, it never approves an unpaid listing",
      "ListingFeeNotPaid",
      [
        await program.methods
          .approveListing()
          .accountsPartial({ listingAuthority: listingAuthority.publicKey, config: pda.config(), listing })
          .instruction(),
      ],
      listingAuthority,
    );

    const nextBatchNo = (await program.account.listing.fetch(listing)).nextBatchNo;
    await refuse(
      "request_issuance on a Quoted listing: only an Approved listing takes batches",
      "ListingNotIssuable",
      [
        await program.methods
          .requestIssuance(bn(10), issuer.publicKey, docHash("TEST refused batch"))
          .accountsPartial({
            issuer: issuer.publicKey,
            listing,
            batch: pda.batch(listing, nextBatchNo),
            systemProgram: SystemProgram.programId,
          })
          .instruction(),
      ],
      issuer,
    );

    const fresh = { mint: mint.publicKey, seller: issuer.publicKey, buyer: outsider.publicKey };
    await refuse(
      "settle on a listing that is Quoted, not Approved (valid accounts, non-zero match_id)",
      "ListingNotApproved",
      [await settleIx(fresh, randomBytes(16))],
      operator,
    );

    const demoEquity = latestDemoEquity(keysDir);
    const demoListing = demoEquity
      ? await program.account.listing.fetchNullable(pda.listing(demoEquity.mint))
      : null;
    const zeroTarget = demoEquity && demoListing && "approved" in demoListing.status ? demoEquity : fresh;
    console.log(
      zeroTarget === fresh
        ? "  (no usable demo listing in the keys folder: the zero match_id case uses the new listing)"
        : `  (the zero match_id case reuses the demo's Approved listing ${pda.listing(zeroTarget.mint).toBase58()})`,
    );
    await refuse(
      "settle with a zero match_id",
      "ZeroMatchId",
      [await settleIx(zeroTarget, new Uint8Array(16))],
      operator,
    );

    // Case 2: wait for the quote to expire on the cluster's clock.
    console.log(`\n  waiting for the quote to expire (cluster time ${expiresAt})...`);
    while ((await clusterTime(ctx)) <= expiresAt) await new Promise((done) => setTimeout(done, 2_000));
    await refuse("pay_listing_fee after the quote expired", "QuoteExpired", [await payIx()], issuer);

    console.log("\n=== Nothing moved ===");
    const finalListing = await program.account.listing.fetch(listing);
    if (!("quoted" in finalListing.status)) throw new Error("check failed: the new listing is not Quoted any more");
    console.log("  ok: the new listing is still Quoted (no refusal changed it)");
    await expectEqual("issuer's mockUSDT (the fee was never paid)", () => tokenBalance(ctx, ata(usdt, issuer.publicKey)), LISTING_FEE);
    await expectEqual(
      "listing_fee_paid on the listing",
      async () => BigInt(finalListing.listingFeePaid.toString()),
      0n,
    );
    console.log(`\nRefusals complete: ${recorded.length} refused with the exact error.`);
    for (const entry of recorded) console.log(`  ${entry.expected.padEnd(18)} ${explorerTx(ctx, entry.signature)}`);
  } finally {
    console.log("\n=== Clean-up ===");
    const left = await ctx.connection.getBalance(issuer.publicKey);
    if (left > 0) {
      // Best effort: the keypair stays in the run folder, so nothing is lost if this fails.
      await send(
        ctx,
        "return unused SOL to the demo funder",
        [SystemProgram.transfer({ fromPubkey: issuer.publicKey, toPubkey: funder.publicKey, lamports: left })],
        funder,
        issuer,
      ).catch((error: unknown) => console.error(`  warning: ${(error as Error).message}`));
    }
    const spent = funderBalance - (await ctx.connection.getBalance(funder.publicKey));
    console.log(`  this run cost the demo funder ${sol(spent)} (fees and rent)`);
  }
}

await main();
