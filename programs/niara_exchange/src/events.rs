//! Events are emitted with `emit_cpi!` so the Niara-Register indexer reads them
//! from inner instructions instead of (truncatable) program logs.

use anchor_lang::prelude::*;

use crate::state::{AssetClass, ConfigChange};

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
