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
