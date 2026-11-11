//! Events are emitted with `emit_cpi!` so the Niara-Register indexer reads them
//! from inner instructions instead of (truncatable) program logs.

use anchor_lang::prelude::*;

use crate::state::{AssetClass, BurnReason, ConfigChange, DealSide};

#[event]
pub struct PauseSet {
    pub paused: bool,
    pub admin: Pubkey,
}

#[event]
pub struct ChangeProposed {
    pub change: ConfigChange,
    pub execute_after: i64,
}

#[event]
pub struct ChangeExecuted {
    pub change: ConfigChange,
}

#[event]
pub struct ChangeCancelled {
    pub change: ConfigChange,
}

#[event]
pub struct ListingSubmitted {
    pub listing: Pubkey,
    pub asset_mint: Pubkey,
    pub issuer: Pubkey,
    pub asset_class: AssetClass,
    pub doc_hash: [u8; 32],
}

#[event]
pub struct ListingApproved {
    pub listing: Pubkey,
    pub asset_mint: Pubkey,
    pub issuer: Pubkey,
    pub asset_class: AssetClass,
    pub cashback_eligible: bool,
    pub doc_hash: [u8; 32],
    pub approved_at: i64,
}

#[event]
pub struct ListingSuspended {
    pub listing: Pubkey,
    pub asset_mint: Pubkey,
}

#[event]
pub struct ListingDelisted {
    pub listing: Pubkey,
    pub asset_mint: Pubkey,
}

/// One order-book settlement. `order_id` is the off-chain match (fill) id.
/// `payment_amount == seller_receives + fee`; `cashback` is the part of `fee`
/// credited to the asset issuer.
#[event]
pub struct Settled {
    pub order_id: [u8; 16],
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub buyer: Pubkey,
    pub seller: Pubkey,
    pub asset_amount: u64,
    pub payment_amount: u64,
    pub fee: u64,
    pub cashback: u64,
}

/// Cashback withdrawals for one listing were frozen (`frozen == true`) or
/// unfrozen by the listing authority. Accrual is never affected.
#[event]
pub struct CashbackFreezeSet {
    pub listing: Pubkey,
    pub asset_mint: Pubkey,
    pub frozen: bool,
    pub listing_authority: Pubkey,
}

#[event]
pub struct CashbackWithdrawn {
    pub listing: Pubkey,
    pub issuer: Pubkey,
    pub payment_mint: Pubkey,
    pub destination: Pubkey,
    pub amount: u64,
}

#[event]
pub struct ProtocolFeesWithdrawn {
    pub treasury: Pubkey,
    pub payment_mint: Pubkey,
    pub destination: Pubkey,
    pub amount: u64,
}

#[event]
pub struct IssuanceRequested {
    pub listing: Pubkey,
    pub asset_mint: Pubkey,
    pub batch_no: u64,
    pub amount: u64,
    pub recipient: Pubkey,
    pub doc_hash: [u8; 32],
}

/// A batch was minted. `recipient` is the holder wallet; `destination` the
/// token account (owned by `recipient`) that received the tokens.
#[event]
pub struct IssuanceExecuted {
    pub asset_mint: Pubkey,
    pub batch_no: u64,
    pub amount: u64,
    pub recipient: Pubkey,
    pub doc_hash: [u8; 32],
    pub destination: Pubkey,
}

#[event]
pub struct IssuanceRejected {
    pub asset_mint: Pubkey,
    pub batch_no: u64,
    pub amount: u64,
    pub recipient: Pubkey,
}

#[event]
pub struct TokensBurned {
    pub asset_mint: Pubkey,
    pub holder: Pubkey,
    pub holder_account: Pubkey,
    pub amount: u64,
    pub reason: BurnReason,
    pub reference: [u8; 32],
}

/// The proposer escrowed its side of a P2P deal (`asset_amount` if selling,
/// `payment_amount` if buying). `fee_bps` and `cashback_bps` are the rates
/// fixed for this deal (the config's at open), charged on execution.
#[event]
pub struct DealOpened {
    pub deal: Pubkey,
    pub deal_id: [u8; 16],
    pub proposer: Pubkey,
    pub counterparty: Pubkey,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub proposer_side: DealSide,
    pub asset_amount: u64,
    pub payment_amount: u64,
    pub fee_bps: u16,
    pub cashback_bps: u16,
    pub expires_at: i64,
}

/// A P2P deal executed. Same accounting as `Settled`, on the USDT leg:
/// `fee` is in `fee_mint` (the payment mint, or the asset mint when the
/// currency sold is USDT) and was deducted from what the receiver of that leg
/// got; `cashback` is the part of `fee` credited to the asset issuer (0 for
/// `Currency` assets). `vault_excess` is what the escrow vault held beyond the
/// deposit (tokens sent to it outside `open_deal`); it went to the
/// counterparty with the deposit.
#[event]
pub struct DealExecuted {
    pub deal: Pubkey,
    pub deal_id: [u8; 16],
    pub proposer: Pubkey,
    pub counterparty: Pubkey,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub proposer_side: DealSide,
    pub asset_amount: u64,
    pub payment_amount: u64,
    pub fee_mint: Pubkey,
    pub fee: u64,
    pub cashback: u64,
    pub vault_excess: u64,
}

/// The proposer cancelled an open deal and got `deposit_amount` (plus any
/// `vault_excess`) of `deposit_mint` back.
#[event]
pub struct DealCancelled {
    pub deal: Pubkey,
    pub deal_id: [u8; 16],
    pub proposer: Pubkey,
    pub deposit_mint: Pubkey,
    pub deposit_amount: u64,
    pub vault_excess: u64,
}

/// An expired deal was refunded to the proposer (permissionless crank).
#[event]
pub struct DealExpired {
    pub deal: Pubkey,
    pub deal_id: [u8; 16],
    pub proposer: Pubkey,
    pub deposit_mint: Pubkey,
    pub deposit_amount: u64,
    pub vault_excess: u64,
}

/// Unaccounted tokens (sent to the fee vault outside `settle`) swept by the
/// treasury. `vault_balance_after` equals the unwithdrawn fees and cashback.
#[event]
pub struct ExcessSwept {
    pub treasury: Pubkey,
    pub payment_mint: Pubkey,
    pub destination: Pubkey,
    pub amount: u64,
    pub vault_balance_after: u64,
}

/// The fee vault and fee ledger of `fee_mint` exist; fees in it can be
/// collected from now on.
#[event]
pub struct FeeVaultInitialized {
    pub fee_mint: Pubkey,
    pub fee_vault: Pubkey,
    pub fee_ledger: Pubkey,
}
