//! Niara Exchange: curated listing and atomic settlement for tokenized
//! securities. Testnet only; no real assets.
//!
//! Design: `EXCHANGE-MULTICHAIN-DESIGN.md`, section 3.

pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod mint_policy;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("B311xH3PcEx86gdRbuinrGrh1r9G2qzjDCji8uc6eD9v");

#[program]
pub mod niara_exchange {
    use super::*;

    // ---- Config ---------------------------------------------------------

    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        params: InitializeConfigParams,
    ) -> Result<()> {
        instructions::config::handle_initialize_config(ctx, params)
    }

    pub fn pause(ctx: Context<SetPause>) -> Result<()> {
        instructions::pause::handle_pause(ctx)
    }

    pub fn unpause(ctx: Context<SetPause>) -> Result<()> {
        instructions::pause::handle_unpause(ctx)
    }

    // ---- Timelocked admin changes --------------------------------------

    pub fn propose_change(ctx: Context<ProposeChange>, change: ConfigChange) -> Result<()> {
        instructions::timelock::handle_propose_change(ctx, change)
    }

    pub fn execute_change(ctx: Context<ResolveChange>) -> Result<()> {
        instructions::timelock::handle_execute_change(ctx)
    }

    pub fn cancel_change(ctx: Context<ResolveChange>) -> Result<()> {
        instructions::timelock::handle_cancel_change(ctx)
    }

    // ---- Listings -------------------------------------------------------

    pub fn submit_listing(
        ctx: Context<SubmitListing>,
        asset_class: AssetClass,
        doc_hash: [u8; 32],
    ) -> Result<()> {
        instructions::listing::handle_submit_listing(ctx, asset_class, doc_hash)
    }

    pub fn list_currency(ctx: Context<ListCurrency>) -> Result<()> {
        instructions::listing::handle_list_currency(ctx)
    }

    pub fn approve_listing(ctx: Context<UpdateListingStatus>) -> Result<()> {
        instructions::listing::handle_approve_listing(ctx)
    }

    pub fn suspend_listing(ctx: Context<UpdateListingStatus>) -> Result<()> {
        instructions::listing::handle_suspend_listing(ctx)
    }

    pub fn delist(ctx: Context<UpdateListingStatus>) -> Result<()> {
        instructions::listing::handle_delist(ctx)
    }
}
