//! Niara Exchange: curated listing and atomic settlement for tokenized
//! securities. Testnet only; no real assets.
//!
//! Design: `EXCHANGE-MULTICHAIN-DESIGN.md`, section 3.

pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod math;
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

    // ---- Niara-approved issuance (spec §3.12) ----------------------------

    /// Issuer proposes a numbered batch; mints nothing.
    pub fn request_issuance(
        ctx: Context<RequestIssuance>,
        amount: u64,
        recipient: Pubkey,
        doc_hash: [u8; 32],
    ) -> Result<()> {
        instructions::issuance::handle_request_issuance(ctx, amount, recipient, doc_hash)
    }

    /// Listing authority executes a batch: the program mints via its PDA.
    pub fn approve_issuance(ctx: Context<ApproveIssuance>) -> Result<()> {
        instructions::issuance::handle_approve_issuance(ctx)
    }

    pub fn reject_issuance(ctx: Context<RejectIssuance>) -> Result<()> {
        instructions::issuance::handle_reject_issuance(ctx)
    }

    /// Holder burns its own tokens, with a reason and a document reference.
    pub fn burn_with_reason(
        ctx: Context<BurnWithReason>,
        amount: u64,
        reason: BurnReason,
        reference: [u8; 32],
    ) -> Result<()> {
        instructions::issuance::handle_burn_with_reason(ctx, amount, reason, reference)
    }

    // ---- Order-book settlement and cashback ------------------------------

    pub fn init_cashback_ledger(ctx: Context<InitCashbackLedger>) -> Result<()> {
        instructions::cashback::handle_init_cashback_ledger(ctx)
    }

    /// `order_id` is the off-chain match (fill) id; each settles at most once.
    pub fn settle(
        ctx: Context<Settle>,
        order_id: [u8; 16],
        asset_amount: u64,
        payment_amount: u64,
    ) -> Result<()> {
        instructions::settle::handle_settle(ctx, order_id, asset_amount, payment_amount)
    }

    // ---- Withdrawals (pull model; blocked while paused) ------------------

    pub fn withdraw_cashback(ctx: Context<WithdrawCashback>, amount: u64) -> Result<()> {
        instructions::withdraw::handle_withdraw_cashback(ctx, amount)
    }

    pub fn withdraw_protocol_fees(ctx: Context<WithdrawProtocolFees>, amount: u64) -> Result<()> {
        instructions::withdraw::handle_withdraw_protocol_fees(ctx, amount)
    }

    /// Treasury takes the fee vault's balance above what the ledger owes.
    pub fn sweep_excess(ctx: Context<SweepExcess>) -> Result<()> {
        instructions::withdraw::handle_sweep_excess(ctx)
    }
}
