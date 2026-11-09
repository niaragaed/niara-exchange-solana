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
