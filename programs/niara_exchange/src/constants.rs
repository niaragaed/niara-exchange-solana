use anchor_lang::prelude::*;

#[constant]
pub const CONFIG_SEED: &[u8] = b"config";

#[constant]
pub const PENDING_SEED: &[u8] = b"pending";

#[constant]
pub const LISTING_SEED: &[u8] = b"listing";

/// Hard cap on the protocol fee: 1% of the payment amount.
#[constant]
pub const MAX_FEE_BPS: u16 = 100;

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

/// Per settled match: `["settled", order_id]` replay guard.
#[constant]
pub const SETTLED_SEED: &[u8] = b"settled";

/// Per asset mint: `["mint_authority", asset_mint]`, the mint authority of
/// every issuer-listed asset after `submit_listing`. Holds no data.
#[constant]
pub const MINT_AUTHORITY_SEED: &[u8] = b"mint_authority";

/// Per issuance batch: `["batch", listing, batch_no (u64 LE)]`.
#[constant]
pub const BATCH_SEED: &[u8] = b"batch";
