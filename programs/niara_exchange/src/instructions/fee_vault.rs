//! The fee vault and fee ledger of the fee mint (spec §3.4): every fee, and so
//! all cashback, is charged in `Config.fee_mint` (USDT), so exactly one vault
//! and one ledger collect it. Other currencies never get one.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenInterface};

use crate::{
    constants::*,
    error::ExchangeError,
    events::FeeVaultInitialized,
    mint_policy::require_allowed_mint_extensions,
    state::*,
    token_vault::{create_pda_token_account, PdaTokenAccount},
};

/// Permissionless (the caller pays the rent), like `init_cashback_ledger`: it
/// can only create the accounts of the configured fee mint, once. Called
/// after `initialize_config` and again after a timelocked fee-mint switch;
/// allowed while paused.
#[event_cpi]
#[derive(Accounts)]
pub struct InitFeeVault<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mint::token_program = token_program,
        constraint = fee_mint.key() == config.fee_mint @ ExchangeError::NotFeeMint
    )]
    pub fee_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: PDA signer of the fee vault; holds no data.
    #[account(seeds = [VAULT_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    /// CHECK: created and initialized by the handler *after* the mint
    /// allowlist check (an Anchor `init` would run first and fail on some
    /// rejected mints with an unrelated error). A PDA rather than an ATA:
    /// anyone can create an ATA, so creating one could be front-run to block
    /// this instruction.
    #[account(mut, seeds = [FEE_VAULT_SEED, fee_mint.key().as_ref()], bump)]
    pub fee_vault: UncheckedAccount<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + FeeLedger::INIT_SPACE,
        seeds = [FEE_LEDGER_SEED, fee_mint.key().as_ref()],
        bump
    )]
    pub fee_ledger: Box<Account<'info, FeeLedger>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_fee_vault(ctx: Context<InitFeeVault>) -> Result<()> {
    let accounts = &ctx.accounts;
    // Mint policy first, before anything is created for this mint.
    require_allowed_mint_extensions(&accounts.fee_mint.to_account_info())?;

    let fee_mint = accounts.fee_mint.key();
    let vault_bump = [ctx.bumps.fee_vault];
    let signer_seeds: &[&[&[u8]]] = &[&[FEE_VAULT_SEED, fee_mint.as_ref(), &vault_bump]];
    create_pda_token_account(
        PdaTokenAccount {
            payer: &accounts.payer.to_account_info(),
            account: &accounts.fee_vault.to_account_info(),
            mint: &accounts.fee_mint.to_account_info(),
            authority: &accounts.vault_authority.to_account_info(),
            token_program: accounts.token_program.key(),
            system_program: accounts.system_program.key(),
        },
        signer_seeds,
    )?;

    ctx.accounts.fee_ledger.set_inner(FeeLedger {
        payment_mint: fee_mint,
        fees_collected: 0,
        cashback_accrued: 0,
        protocol_withdrawn: 0,
        cashback_withdrawn: 0,
        bump: ctx.bumps.fee_ledger,
        vault_bump: ctx.bumps.fee_vault,
    });
    emit_cpi!(FeeVaultInitialized {
        fee_mint,
        fee_vault: ctx.accounts.fee_vault.key(),
        fee_ledger: ctx.accounts.fee_ledger.key(),
    });
    Ok(())
}
