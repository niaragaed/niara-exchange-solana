use anchor_lang::prelude::*;

#[constant]
pub const CONFIG_SEED: &[u8] = b"config";

#[constant]
pub const PENDING_SEED: &[u8] = b"pending";

#[constant]
pub const LISTING_SEED: &[u8] = b"listing";

/// Hard cap on the protocol fee: 0.5% of the fee-mint (USDT) leg. Business
/// rule: Niara charges at most 0.5%.
#[constant]
pub const MAX_FEE_BPS: u16 = 50;

#[constant]
pub const BPS_DENOMINATOR: u16 = 10_000;

/// Bounds for the admin timelock. The lower bound keeps the timelock meaningful;
/// the upper bound prevents bricking the config with an unreachable delay.
#[constant]
pub const MIN_TIMELOCK_SECS: i64 = 60;

#[constant]
pub const MAX_TIMELOCK_SECS: i64 = 30 * 24 * 60 * 60;

/// PDA signer for delegated transfers and the fee vaults. Holds no data.
#[constant]
pub const VAULT_SEED: &[u8] = b"vault";

/// Per payment mint: `["fee_vault", payment_mint]` token account.
#[constant]
pub const FEE_VAULT_SEED: &[u8] = b"fee_vault";

/// Per payment mint: `["fees", payment_mint]` fee ledger.
#[constant]
pub const FEE_LEDGER_SEED: &[u8] = b"fees";

/// Per (listing, payment mint): `["cashback", listing, payment_mint]`.
#[constant]
pub const CASHBACK_SEED: &[u8] = b"cashback";

/// Per settled match: `["settled", match_id]` replay guard.
#[constant]
pub const SETTLED_SEED: &[u8] = b"settled";

/// Per asset mint: `["mint_authority", asset_mint]`, the mint authority of
/// every issuer-listed asset after `submit_listing`. Holds no data.
#[constant]
pub const MINT_AUTHORITY_SEED: &[u8] = b"mint_authority";

/// Per issuance batch: `["batch", listing, batch_no (u64 LE)]`.
#[constant]
pub const BATCH_SEED: &[u8] = b"batch";

/// Per P2P deal: `["deal", proposer, deal_id]`. Never closed: its status is
/// the replay guard.
#[constant]
pub const DEAL_SEED: &[u8] = b"deal";

/// Per P2P deal: `["deal_vault", deal]` escrow token account whose authority is
/// the deal PDA. Closed when the deal is executed, cancelled or expired.
#[constant]
pub const DEAL_VAULT_SEED: &[u8] = b"deal_vault";

/// Longest a P2P deal may stay open: `expires_at <= now + 30 days`.
#[constant]
pub const MAX_DEAL_DURATION_SECS: i64 = 30 * 24 * 60 * 60;

/// Hard cap on the listing fee: 1% of the issuer's valuation (spec §3.13).
/// Business rule; the 0.1% floor is pricing policy and is not enforced.
#[constant]
pub const MAX_LISTING_FEE_BPS: u16 = 100;

/// Longest a listing-fee quote stays payable: `expires_at <= now + 30 days`.
#[constant]
pub const MAX_QUOTE_DURATION_SECS: i64 = 30 * 24 * 60 * 60;
