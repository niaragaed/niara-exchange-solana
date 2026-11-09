use anchor_lang::prelude::*;

use crate::{constants::*, error::ExchangeError, events::PauseSet, state::Config};

/// Circuit breaker. Not timelocked on purpose: it must act immediately.
#[event_cpi]
#[derive(Accounts)]
pub struct SetPause<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ ExchangeError::Unauthorized
    )]
    pub config: Account<'info, Config>,
}

pub fn handle_pause(ctx: Context<SetPause>) -> Result<()> {
    require!(!ctx.accounts.config.paused, ExchangeError::AlreadyPaused);
    ctx.accounts.config.paused = true;
    emit_cpi!(PauseSet {
        paused: true,
        admin: ctx.accounts.admin.key(),
    });
    Ok(())
}

pub fn handle_unpause(ctx: Context<SetPause>) -> Result<()> {
    require!(ctx.accounts.config.paused, ExchangeError::NotPaused);
    ctx.accounts.config.paused = false;
    emit_cpi!(PauseSet {
        paused: false,
        admin: ctx.accounts.admin.key(),
    });
    Ok(())
}
