use anchor_lang::prelude::*;

use crate::{constants::*, error::ExchangeError, state::*};

/// Creates the `CashbackLedger` for one (listing, payment mint) pair.
/// Permissionless: whoever pays the rent (in practice the settlement worker,
/// once per pair before its first settlement). It is created explicitly instead
/// of with `init_if_needed` inside `settle` (spec §3.8).
#[derive(Accounts)]
pub struct InitCashbackLedger<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        seeds = [LISTING_SEED, listing.asset_mint.as_ref()],
        bump = listing.bump,
        constraint = listing.cashback_eligible @ ExchangeError::NotCashbackEligible
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(
        seeds = [LISTING_SEED, payment_listing.asset_mint.as_ref()],
        bump = payment_listing.bump,
        constraint = payment_listing.is_approved_currency() @ ExchangeError::InvalidPaymentMint
    )]
    pub payment_listing: Box<Account<'info, Listing>>,
    #[account(
        init,
        payer = payer,
        space = 8 + CashbackLedger::INIT_SPACE,
        seeds = [CASHBACK_SEED, listing.key().as_ref(), payment_listing.asset_mint.as_ref()],
        bump
    )]
    pub cashback_ledger: Account<'info, CashbackLedger>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_cashback_ledger(ctx: Context<InitCashbackLedger>) -> Result<()> {
    ctx.accounts.cashback_ledger.set_inner(CashbackLedger {
        listing: ctx.accounts.listing.key(),
        payment_mint: ctx.accounts.payment_listing.asset_mint,
        accrued: 0,
        withdrawn: 0,
        bump: ctx.bumps.cashback_ledger,
    });
    Ok(())
}
