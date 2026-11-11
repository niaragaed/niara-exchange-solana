use anchor_lang::prelude::*;

use crate::{constants::*, error::ExchangeError, state::*};

/// Creates the `CashbackLedger` of one listing in the current fee mint
/// (cashback is always in USDT, spec §3.4). Permissionless: whoever pays the
/// rent (in practice the settlement worker, once per listing before its first
/// settlement or deal). It is created explicitly instead of with
/// `init_if_needed` inside `settle` (spec §3.8).
#[derive(Accounts)]
pub struct InitCashbackLedger<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        seeds = [LISTING_SEED, listing.asset_mint.as_ref()],
        bump = listing.bump,
        constraint = listing.cashback_eligible @ ExchangeError::NotCashbackEligible
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(
        init,
        payer = payer,
        space = 8 + CashbackLedger::INIT_SPACE,
        seeds = [CASHBACK_SEED, listing.key().as_ref(), config.fee_mint.as_ref()],
        bump
    )]
    pub cashback_ledger: Account<'info, CashbackLedger>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_cashback_ledger(ctx: Context<InitCashbackLedger>) -> Result<()> {
    ctx.accounts.cashback_ledger.set_inner(CashbackLedger {
        listing: ctx.accounts.listing.key(),
        payment_mint: ctx.accounts.config.fee_mint,
        accrued: 0,
        withdrawn: 0,
        bump: ctx.bumps.cashback_ledger,
    });
    Ok(())
}
