use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::ExchangeError,
    events::{ChangeCancelled, ChangeExecuted, ChangeProposed},
    state::{Config, ConfigChange, PendingChange},
};

#[event_cpi]
#[derive(Accounts)]
#[instruction(change: ConfigChange)]
pub struct ProposeChange<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ ExchangeError::Unauthorized
    )]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = admin,
        space = 8 + PendingChange::INIT_SPACE,
        seeds = [PENDING_SEED, &[change.kind()]],
        bump
    )]
    pub pending_change: Account<'info, PendingChange>,
    pub system_program: Program<'info, System>,
}

pub fn handle_propose_change(ctx: Context<ProposeChange>, change: ConfigChange) -> Result<()> {
    change.validate()?;
    let now = Clock::get()?.unix_timestamp;
    let execute_after = now
        .checked_add(ctx.accounts.config.timelock_secs)
        .ok_or(ExchangeError::MathOverflow)?;

    ctx.accounts.pending_change.set_inner(PendingChange {
        change,
        execute_after,
        bump: ctx.bumps.pending_change,
    });
    emit_cpi!(ChangeProposed {
        change,
        execute_after,
    });
    Ok(())
}

/// Shared by `execute_change` and `cancel_change`. The pending account is closed
/// to the current admin either way. An `Admin` change is cancelled here but
/// executed only by `accept_admin`.
#[event_cpi]
#[derive(Accounts)]
pub struct ResolveChange<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ ExchangeError::Unauthorized
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        close = admin,
        seeds = [PENDING_SEED, &[pending_change.change.kind()]],
        bump = pending_change.bump
    )]
    pub pending_change: Account<'info, PendingChange>,
}

pub fn handle_execute_change(ctx: Context<ResolveChange>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let pending = &ctx.accounts.pending_change;
    require!(
        now >= pending.execute_after,
        ExchangeError::TimelockNotElapsed
    );
    let change = pending.change;
    // The new admin must prove it controls the key (spec §3.3): a typo, or an
    // address nobody can sign for, would otherwise lose the admin for good.
    require!(
        !matches!(change, ConfigChange::Admin(_)),
        ExchangeError::AdminChangeNeedsAcceptance
    );
    change.apply(&mut ctx.accounts.config)?;
    emit_cpi!(ChangeExecuted { change });
    Ok(())
}

pub fn handle_cancel_change(ctx: Context<ResolveChange>) -> Result<()> {
    let change = ctx.accounts.pending_change.change;
    emit_cpi!(ChangeCancelled { change });
    Ok(())
}

/// Second step of an admin transfer (spec §3.3). The current admin proposes
/// `ConfigChange::Admin(new_admin)` with `propose_change`; once the timelock
/// has elapsed, the proposed admin signs this to take over. The current admin
/// can still `cancel_change` until then.
#[event_cpi]
#[derive(Accounts)]
pub struct AcceptAdmin<'info> {
    /// The proposed admin. It only signs: it pays nothing and is not written,
    /// so a multisig vault (e.g. a Squads v4 vault PDA, through a vault
    /// transaction) can accept.
    pub new_admin: Signer<'info>,
    /// CHECK: the outgoing admin, `config.admin` (checked by `has_one`). It
    /// paid the pending change's rent at `propose_change` and gets it back.
    #[account(mut)]
    pub admin: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ ExchangeError::Unauthorized
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        close = admin,
        seeds = [PENDING_SEED, &[pending_change.change.kind()]],
        bump = pending_change.bump
    )]
    pub pending_change: Account<'info, PendingChange>,
}

pub fn handle_accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
    let pending = &ctx.accounts.pending_change;
    let change = pending.change;
    let ConfigChange::Admin(proposed) = change else {
        return err!(ExchangeError::NotPendingAdmin);
    };
    require_keys_eq!(
        ctx.accounts.new_admin.key(),
        proposed,
        ExchangeError::NotPendingAdmin
    );
    let now = Clock::get()?.unix_timestamp;
    require!(
        now >= pending.execute_after,
        ExchangeError::TimelockNotElapsed
    );
    change.apply(&mut ctx.accounts.config)?;
    emit_cpi!(ChangeExecuted { change });
    Ok(())
}
