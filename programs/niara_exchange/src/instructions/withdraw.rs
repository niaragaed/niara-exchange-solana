//! Pull-model payouts from the fee vault: issuer cashback, protocol fees and
//! the treasury's sweep of unaccounted excess.
//!
//! All three are blocked while the protocol is paused. Principle (spec §3.3): a pause
//! stops the protocol from paying out based on its own accounting, but never
//! stops a user from taking back their own escrowed deposit.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{
    constants::*,
    error::ExchangeError,
    events::{CashbackWithdrawn, ExcessSwept, ProtocolFeesWithdrawn},
    state::*,
};

#[event_cpi]
#[derive(Accounts)]
pub struct WithdrawCashback<'info> {
    pub issuer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// `Currency` listings have `issuer == Pubkey::default()`, which cannot sign.
    #[account(
        seeds = [LISTING_SEED, listing.asset_mint.as_ref()],
        bump = listing.bump,
        has_one = issuer @ ExchangeError::Unauthorized
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(mint::token_program = token_program)]
    pub payment_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [CASHBACK_SEED, listing.key().as_ref(), payment_mint.key().as_ref()],
        bump = cashback_ledger.bump
    )]
    pub cashback_ledger: Box<Account<'info, CashbackLedger>>,
    #[account(
        mut,
        seeds = [FEE_LEDGER_SEED, payment_mint.key().as_ref()],
        bump = fee_ledger.bump
    )]
    pub fee_ledger: Box<Account<'info, FeeLedger>>,
    /// CHECK: PDA signer of the fee vault; holds no data.
    #[account(seeds = [VAULT_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [FEE_VAULT_SEED, payment_mint.key().as_ref()],
        bump = fee_ledger.vault_bump,
        token::mint = payment_mint,
        token::authority = vault_authority,
        token::token_program = token_program
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Any account of the payment mint, chosen by the issuer.
    #[account(
        mut,
        token::mint = payment_mint,
        token::token_program = token_program,
        constraint = destination.key() != fee_vault.key() @ ExchangeError::InvalidDestination
    )]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_withdraw_cashback(ctx: Context<WithdrawCashback>, amount: u64) -> Result<()> {
    ctx.accounts.config.require_not_paused()?;
    // Per-listing containment (spec §9.5); accrual is not affected.
    require!(
        !ctx.accounts.listing.cashback_frozen,
        ExchangeError::CashbackFrozen
    );
    require!(amount > 0, ExchangeError::ZeroAmount);

    let ledger = &mut ctx.accounts.cashback_ledger;
    require!(
        amount <= ledger.available()?,
        ExchangeError::InsufficientCashback
    );
    ledger.withdrawn = ledger
        .withdrawn
        .checked_add(amount)
        .ok_or(ExchangeError::MathOverflow)?;
    let fee_ledger = &mut ctx.accounts.fee_ledger;
    fee_ledger.cashback_withdrawn = fee_ledger
        .cashback_withdrawn
        .checked_add(amount)
        .ok_or(ExchangeError::MathOverflow)?;

    pay_out(
        &ctx.accounts.token_program,
        &ctx.accounts.fee_vault,
        &ctx.accounts.payment_mint,
        &ctx.accounts.destination,
        &ctx.accounts.vault_authority,
        ctx.bumps.vault_authority,
        amount,
    )?;
    emit_cpi!(CashbackWithdrawn {
        listing: ctx.accounts.listing.key(),
        issuer: ctx.accounts.issuer.key(),
        payment_mint: ctx.accounts.payment_mint.key(),
        destination: ctx.accounts.destination.key(),
        amount,
    });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct WithdrawProtocolFees<'info> {
    pub treasury: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = treasury @ ExchangeError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub payment_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [FEE_LEDGER_SEED, payment_mint.key().as_ref()],
        bump = fee_ledger.bump
    )]
    pub fee_ledger: Box<Account<'info, FeeLedger>>,
    /// CHECK: PDA signer of the fee vault; holds no data.
    #[account(seeds = [VAULT_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [FEE_VAULT_SEED, payment_mint.key().as_ref()],
        bump = fee_ledger.vault_bump,
        token::mint = payment_mint,
        token::authority = vault_authority,
        token::token_program = token_program
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = payment_mint,
        token::token_program = token_program,
        constraint = destination.key() != fee_vault.key() @ ExchangeError::InvalidDestination
    )]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_withdraw_protocol_fees(
    ctx: Context<WithdrawProtocolFees>,
    amount: u64,
) -> Result<()> {
    ctx.accounts.config.require_not_paused()?;
    require!(amount > 0, ExchangeError::ZeroAmount);

    let fee_ledger = &mut ctx.accounts.fee_ledger;
    require!(
        amount <= fee_ledger.protocol_available()?,
        ExchangeError::InsufficientProtocolFees
    );
    fee_ledger.protocol_withdrawn = fee_ledger
        .protocol_withdrawn
        .checked_add(amount)
        .ok_or(ExchangeError::MathOverflow)?;

    pay_out(
        &ctx.accounts.token_program,
        &ctx.accounts.fee_vault,
        &ctx.accounts.payment_mint,
        &ctx.accounts.destination,
        &ctx.accounts.vault_authority,
        ctx.bumps.vault_authority,
        amount,
    )?;
    emit_cpi!(ProtocolFeesWithdrawn {
        treasury: ctx.accounts.treasury.key(),
        payment_mint: ctx.accounts.payment_mint.key(),
        destination: ctx.accounts.destination.key(),
        amount,
    });
    Ok(())
}

/// Treasury pulls the fee vault's unaccounted excess: tokens that reached the
/// vault outside `settle` (anyone can transfer into it). Owed funds
/// (unwithdrawn protocol fees + unwithdrawn cashback) never move here, so after
/// a sweep the vault holds exactly what the `FeeLedger` says (invariant 4).
#[event_cpi]
#[derive(Accounts)]
pub struct SweepExcess<'info> {
    pub treasury: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = treasury @ ExchangeError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub payment_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        seeds = [FEE_LEDGER_SEED, payment_mint.key().as_ref()],
        bump = fee_ledger.bump
    )]
    pub fee_ledger: Box<Account<'info, FeeLedger>>,
    /// CHECK: PDA signer of the fee vault; holds no data.
    #[account(seeds = [VAULT_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [FEE_VAULT_SEED, payment_mint.key().as_ref()],
        bump = fee_ledger.vault_bump,
        token::mint = payment_mint,
        token::authority = vault_authority,
        token::token_program = token_program
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = payment_mint,
        token::token_program = token_program,
        constraint = destination.key() != fee_vault.key() @ ExchangeError::InvalidDestination
    )]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_sweep_excess(ctx: Context<SweepExcess>) -> Result<()> {
    ctx.accounts.config.require_not_paused()?;
    let owed = ctx.accounts.fee_ledger.unwithdrawn()?;
    let balance = ctx.accounts.fee_vault.amount;
    let excess = balance.saturating_sub(owed);
    require!(excess > 0, ExchangeError::NothingToSweep);

    pay_out(
        &ctx.accounts.token_program,
        &ctx.accounts.fee_vault,
        &ctx.accounts.payment_mint,
        &ctx.accounts.destination,
        &ctx.accounts.vault_authority,
        ctx.bumps.vault_authority,
        excess,
    )?;
    emit_cpi!(ExcessSwept {
        treasury: ctx.accounts.treasury.key(),
        payment_mint: ctx.accounts.payment_mint.key(),
        destination: ctx.accounts.destination.key(),
        amount: excess,
        vault_balance_after: owed,
    });
    Ok(())
}

fn pay_out<'info>(
    token_program: &Interface<'info, TokenInterface>,
    fee_vault: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    destination: &InterfaceAccount<'info, TokenAccount>,
    vault_authority: &UncheckedAccount<'info>,
    vault_bump: u8,
    amount: u64,
) -> Result<()> {
    let bump = [vault_bump];
    let signer_seeds: &[&[&[u8]]] = &[&[VAULT_SEED, &bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            token_program.key(),
            TransferChecked {
                from: fee_vault.to_account_info(),
                mint: mint.to_account_info(),
                to: destination.to_account_info(),
                authority: vault_authority.to_account_info(),
            },
            signer_seeds,
        ),
        amount,
        mint.decimals,
    )
}
