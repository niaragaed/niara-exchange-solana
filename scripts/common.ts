// Shared helpers for scripts/setup.ts and scripts/demo.ts.
//
// TESTNET ONLY: no real assets, no real money. These scripts run against a
// local validator or Solana devnet and refuse any other cluster.
//
// Run with Node >= 22.18 (it executes TypeScript directly): `node scripts/setup.ts`.

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { Program } from "@anchor-lang/core";
import BN from "bn.js";
import {
  ExtensionType,
  LENGTH_SIZE,
  TOKEN_2022_PROGRAM_ID,
  TYPE_SIZE,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeMetadataPointerInstruction,
  createInitializeMintInstruction,
  getAccount,
  getAssociatedTokenAddressSync,
  getMintLen,
} from "@solana/spl-token";
import { createInitializeInstruction, pack } from "@solana/spl-token-metadata";
import type { TokenMetadata } from "@solana/spl-token-metadata";
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SYSVAR_CLOCK_PUBKEY,
  SystemProgram,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import type { TransactionInstruction } from "@solana/web3.js";

import type { NiaraExchange } from "../target/types/niara_exchange.ts";

export type ClusterName = "localnet" | "devnet";

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const IDL_PATH = join(REPO_ROOT, "target", "idl", "niara_exchange.json");

/** Every mint these scripts create is a Token-2022 mint with on-mint metadata. */
export const TOKEN_PROGRAM = TOKEN_2022_PROGRAM_ID;
export const BPF_LOADER_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");

const DEFAULT_RPC: Record<ClusterName, string> = {
  localnet: "http://127.0.0.1:8899",
  devnet: "https://api.devnet.solana.com",
};
const PUBLIC_GENESIS: Record<string, string> = {
  "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d": "mainnet-beta",
  "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY": "testnet",
  EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG: "devnet",
};

export interface Ctx {
  cluster: ClusterName;
  connection: Connection;
  program: Program<NiaraExchange>;
  /** Folder outside the repo that holds this cluster's keypairs. */
  keysDir: string;
  explorerQuery: string;
}

export function fail(message: string): never {
  console.error(`\nERROR: ${message}`);
  process.exit(1);
}

export function parseCluster(value: string | undefined): ClusterName {
  if (value === undefined || value === "localnet") return "localnet";
  if (value === "devnet") return "devnet";
  return fail(`unknown cluster "${value}": use localnet or devnet`);
}

/**
 * Connects and checks that the RPC really is the requested cluster, by genesis
 * hash. The Solana CLI config is never read: the cluster comes from --cluster
 * (default localnet) and, optionally, NIARA_RPC_URL (e.g. a private devnet RPC;
 * never commit such a URL).
 */
export async function connect(cluster: ClusterName): Promise<Ctx> {
  const custom = process.env.NIARA_RPC_URL;
  const url = custom ?? DEFAULT_RPC[cluster];
  if (cluster === "localnet" && !["127.0.0.1", "localhost", "[::1]"].includes(new URL(url).hostname)) {
    fail("--cluster localnet needs a local RPC URL (127.0.0.1)");
  }
  const connection = new Connection(url, "confirmed");
  let genesis: string;
  try {
    genesis = await connection.getGenesisHash();
  } catch {
    return fail(
      cluster === "localnet"
        ? "no local validator at 127.0.0.1:8899 (see scripts/localnet-demo.sh)"
        : "cannot reach the devnet RPC",
    );
  }
  const publicName = PUBLIC_GENESIS[genesis];
  if (cluster === "devnet" && publicName !== "devnet") {
    fail(`the RPC is not devnet (genesis ${genesis}${publicName ? `, ${publicName}` : ""})`);
  }
  if (cluster === "localnet" && publicName !== undefined) {
    fail(`the local RPC answers as ${publicName}; refusing to continue`);
  }

  if (!existsSync(IDL_PATH)) fail(`${IDL_PATH} not found: run 'bash scripts/build.sh' first`);
  const idl = JSON.parse(readFileSync(IDL_PATH, "utf8")) as NiaraExchange;
  const program = new Program<NiaraExchange>(idl, { connection });

  const keysDir = resolve(process.env.NIARA_KEYS_DIR ?? join(homedir(), ".config", "niara-exchange", cluster));
  const insideRepo = (path: string) => (path + sep).startsWith(realpathSync(REPO_ROOT) + sep);
  // Checked before creating it, and again through symlinks once it exists.
  if (insideRepo(keysDir)) fail(`the keys folder ${keysDir} is inside the repository; keys must stay outside it`);
  mkdirSync(keysDir, { recursive: true, mode: 0o700 });
  if (insideRepo(realpathSync(keysDir))) fail(`the keys folder ${keysDir} points inside the repository`);

  const explorerQuery =
    cluster === "devnet" ? "?cluster=devnet" : `?cluster=custom&customUrl=${encodeURIComponent(url)}`;
  console.log("Niara Exchange (TESTNET ONLY: test tokens, no real assets, no real money)");
  console.log(`cluster:  ${cluster}${custom && cluster === "devnet" ? " (RPC from NIARA_RPC_URL)" : ""}`);
  console.log(`program:  ${program.programId.toBase58()}`);
  console.log(`keys dir: ${keysDir}`);
  return { cluster, connection, program, keysDir, explorerQuery };
}

// ---- keypairs ---------------------------------------------------------------

export function loadKeypair(path: string): Keypair {
  if (!existsSync(path)) fail(`keypair not found: ${path}`);
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8")) as number[]));
}

/** Writes a new keypair file (mode 600); never overwrites an existing one. */
export function createKeypair(path: string): Keypair {
  const keypair = Keypair.generate();
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  writeFileSync(path, JSON.stringify(Array.from(keypair.secretKey)), { mode: 0o600, flag: "wx" });
  return keypair;
}

export function loadOrCreateKeypair(path: string): Keypair {
  return existsSync(path) ? loadKeypair(path) : createKeypair(path);
}

// ---- PDAs (spec §3.2) ---------------------------------------------------------

export function pdas(programId: PublicKey) {
  const find = (...seeds: (string | Uint8Array)[]) =>
    PublicKey.findProgramAddressSync(
      seeds.map((seed) => (typeof seed === "string" ? Buffer.from(seed) : seed)),
      programId,
    )[0];
  return {
    config: () => find("config"),
    vaultAuthority: () => find("vault"),
    programData: () => PublicKey.findProgramAddressSync([programId.toBuffer()], BPF_LOADER_UPGRADEABLE)[0],
    listing: (mint: PublicKey) => find("listing", mint.toBuffer()),
    mintAuthority: (mint: PublicKey) => find("mint_authority", mint.toBuffer()),
    batch: (listing: PublicKey, batchNo: BN) => find("batch", listing.toBuffer(), batchNo.toArrayLike(Buffer, "le", 8)),
    feeVault: (feeMint: PublicKey) => find("fee_vault", feeMint.toBuffer()),
    feeLedger: (feeMint: PublicKey) => find("fees", feeMint.toBuffer()),
    cashbackLedger: (listing: PublicKey, feeMint: PublicKey) => find("cashback", listing.toBuffer(), feeMint.toBuffer()),
    settlementRecord: (matchId: Uint8Array) => find("settled", matchId),
    deal: (proposer: PublicKey, dealId: Uint8Array) => find("deal", proposer.toBuffer(), dealId),
    dealVault: (deal: PublicKey) => find("deal_vault", deal.toBuffer()),
  };
}

// ---- transactions -------------------------------------------------------------

export function explorerTx(ctx: Ctx, signature: string): string {
  return `https://explorer.solana.com/tx/${signature}${ctx.explorerQuery}`;
}

export function explorerAddress(ctx: Ctx, address: PublicKey): string {
  return `https://explorer.solana.com/address/${address.toBase58()}${ctx.explorerQuery}`;
}

/** Sends one transaction, paid by `payer`, and prints its explorer link. */
export async function send(
  ctx: Ctx,
  label: string,
  instructions: TransactionInstruction[],
  payer: Keypair,
  ...cosigners: Keypair[]
): Promise<string> {
  const transaction = new Transaction().add(...instructions);
  transaction.feePayer = payer.publicKey;
  const signers = [payer, ...cosigners.filter((signer) => !signer.publicKey.equals(payer.publicKey))];
  try {
    const signature = await sendAndConfirmTransaction(ctx.connection, transaction, signers, {
      commitment: "confirmed",
    });
    console.log(`  ${label}\n      ${explorerTx(ctx, signature)}`);
    return signature;
  } catch (error) {
    const logs = (error as { logs?: string[] }).logs;
    if (logs?.length) console.error(logs.map((line) => `      ${line}`).join("\n"));
    throw new Error(`${label} failed: ${(error as Error).message}`, { cause: error });
  }
}

export const sol = (lamports: number): string => `${(lamports / LAMPORTS_PER_SOL).toFixed(4)} SOL`;

/**
 * Tops up to `lamports`, from `funder` and in one transaction, every target
 * that holds less than half of it. Never airdrops.
 */
export async function fund(
  ctx: Ctx,
  funder: Keypair,
  targets: { name: string; address: PublicKey; lamports: number }[],
): Promise<void> {
  const balances = await ctx.connection.getMultipleAccountsInfo(targets.map((target) => target.address));
  const transfers = targets
    .map((target, index) => ({ ...target, balance: balances[index]?.lamports ?? 0 }))
    .filter((target) => target.balance < target.lamports / 2)
    .map((target) => ({ ...target, missing: target.lamports - target.balance }));
  if (transfers.length === 0) return;
  const total = transfers.reduce((sum, transfer) => sum + transfer.missing, 0);
  const available = await ctx.connection.getBalance(funder.publicKey);
  if (available < total + 10_000_000) {
    fail(
      `${funder.publicKey.toBase58()} has ${sol(available)} but needs ${sol(total + 10_000_000)}. ` +
        "Fund it by hand; these scripts never airdrop.",
    );
  }
  await send(
    ctx,
    `fund ${transfers.map((transfer) => `${transfer.name} (${sol(transfer.missing)})`).join(", ")}`,
    transfers.map((transfer) =>
      SystemProgram.transfer({ fromPubkey: funder.publicKey, toPubkey: transfer.address, lamports: transfer.missing }),
    ),
    funder,
  );
}

// ---- tokens -------------------------------------------------------------------

/**
 * Instructions that create a Token-2022 mint with on-mint metadata
 * (MetadataPointer + TokenMetadata, the two extensions the program accepts,
 * spec §3.8) and no freeze authority. `payer` and `authority` must sign.
 */
export async function createMintIxs(
  ctx: Ctx,
  params: { payer: PublicKey; mint: PublicKey; authority: PublicKey; decimals: number; name: string; symbol: string },
): Promise<TransactionInstruction[]> {
  const { payer, mint, authority, decimals, name, symbol } = params;
  const metadata: TokenMetadata = { updateAuthority: authority, mint, name, symbol, uri: "", additionalMetadata: [] };
  const mintLen = getMintLen([ExtensionType.MetadataPointer]);
  const metadataLen = TYPE_SIZE + LENGTH_SIZE + pack(metadata).length;
  const lamports = await ctx.connection.getMinimumBalanceForRentExemption(mintLen + metadataLen);
  return [
    SystemProgram.createAccount({
      fromPubkey: payer,
      newAccountPubkey: mint,
      space: mintLen,
      lamports,
      programId: TOKEN_PROGRAM,
    }),
    createInitializeMetadataPointerInstruction(mint, authority, mint, TOKEN_PROGRAM),
    createInitializeMintInstruction(mint, decimals, authority, null, TOKEN_PROGRAM),
    createInitializeInstruction({
      programId: TOKEN_PROGRAM,
      metadata: mint,
      updateAuthority: authority,
      mint,
      mintAuthority: authority,
      name,
      symbol,
      uri: "",
    }),
  ];
}

export function ata(mint: PublicKey, owner: PublicKey): PublicKey {
  return getAssociatedTokenAddressSync(mint, owner, false, TOKEN_PROGRAM);
}

export function createAtaIx(payer: PublicKey, mint: PublicKey, owner: PublicKey): TransactionInstruction {
  return createAssociatedTokenAccountIdempotentInstruction(payer, ata(mint, owner), owner, mint, TOKEN_PROGRAM);
}

export async function tokenBalance(ctx: Ctx, account: PublicKey): Promise<bigint> {
  return (await getAccount(ctx.connection, account, "confirmed", TOKEN_PROGRAM)).amount;
}

// ---- reads after a write --------------------------------------------------------

/**
 * Repeats `read` until `accept` likes the result (about 10 s at most), then
 * returns the last result either way. A public RPC is load-balanced, so a read
 * sent right after a confirmation can reach a node that has not seen it yet.
 */
export async function eventually<T>(read: () => Promise<T>, accept: (value: T) => boolean): Promise<T> {
  for (let attempt = 1; ; attempt += 1) {
    const value = await read().catch((error: unknown) => {
      if (attempt >= 20) throw error;
      return undefined;
    });
    if (value !== undefined && (accept(value) || attempt >= 20)) return value;
    await new Promise((done) => setTimeout(done, 500));
  }
}

// ---- small utilities ------------------------------------------------------------

export const bn = (value: bigint | number): BN => new BN(value.toString());

/** 32-byte document hash, as the program's `[u8; 32]`. */
export function docHash(text: string): number[] {
  return Array.from(createHash("sha256").update(text).digest());
}

/** The cluster's clock (`Clock.unix_timestamp`), which deal expiry is checked against. */
export async function clusterTime(ctx: Ctx): Promise<number> {
  const clock = await ctx.connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY);
  if (!clock) fail("cannot read the Clock sysvar");
  return Number(clock.data.readBigInt64LE(32));
}

export function formatUnits(raw: bigint, decimals: number): string {
  if (decimals === 0) return raw.toString();
  const text = raw.toString().padStart(decimals + 1, "0");
  return `${text.slice(0, -decimals)}.${text.slice(-decimals)}`;
}

/** Fails the script unless `read` returns `expected` (see `eventually`). */
export async function expectEqual(what: string, read: () => Promise<bigint>, expected: bigint): Promise<void> {
  const actual = await eventually(read, (value) => value === expected);
  if (actual !== expected) throw new Error(`check failed: ${what} is ${actual}, expected ${expected}`);
  console.log(`  ok: ${what} = ${actual}`);
}
