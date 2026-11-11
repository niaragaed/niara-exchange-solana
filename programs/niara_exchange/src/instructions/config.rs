use anchor_lang::prelude::*;

use crate::{constants::*, error::ExchangeError, program::NiaraExchange, state::*};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeConfigParams {
    pub listing_authority: Pubkey,
    pub settlement_operator: Pubkey,
    pub treasury: Pubkey,
    /// The USDT mint (spec §3.4). `init_fee_vault` then creates its vault.
    pub fee_mint: Pubkey,
    pub fee_bps: u16,
    pub cashback_bps: u16,
    pub timelock_secs: i64,
}

/// Only the program's upgrade authority can create the config, so nobody can
/// front-run initialization after deployment.
#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        init,
        payer = admin,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, Config>,
    #[account(
        constraint = program.programdata_address()? == Some(program_data.key())
            @ ExchangeError::InvalidProgramData
    )]
    pub program: Program<'info, NiaraExchange>,
    #[account(
        constraint = program_data.upgrade_authority_address == Some(admin.key())
            @ ExchangeError::NotUpgradeAuthority
    )]
    pub program_data: Account<'info, ProgramData>,
    pub system_program: Program<'info, System>,
}

pub fn handle_initialize_config(
    ctx: Context<InitializeConfig>,
    params: InitializeConfigParams,
) -> Result<()> {
    validate_address(&params.listing_authority)?;
    validate_address(&params.settlement_operator)?;
    validate_address(&params.treasury)?;
    validate_address(&params.fee_mint)?;
    validate_fee_bps(params.fee_bps)?;
    validate_cashback_bps(params.cashback_bps)?;
    validate_timelock(params.timelock_secs)?;

    ctx.accounts.config.set_inner(Config {
        admin: ctx.accounts.admin.key(),
        listing_authority: params.listing_authority,
        settlement_operator: params.settlement_operator,
        treasury: params.treasury,
        fee_mint: params.fee_mint,
        fee_bps: params.fee_bps,
        cashback_bps: params.cashback_bps,
        paused: false,
        timelock_secs: params.timelock_secs,
        bump: ctx.bumps.config,
    });
    Ok(())
}
