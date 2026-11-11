//! P2P escrow (spec §3.5) for `FixedIncome`, `Credit` and `Currency` listings.
//!
//! v1 deals are bilateral: the proposer names the counterparty and escrows its
//! own side in a vault owned by the `Deal` PDA. The counterparty accepts by
//! signing and delivering its side directly; one instruction then swaps both
//! legs, charges the fee, accrues cashback and closes the vault. Both parties
//! sign their own transaction, so no operator is involved.
//!
//! Fees are always in USDT (`Config.fee_mint`, spec §3.4): one leg of every
//! deal is in the fee mint (the payment for `FixedIncome`/`Credit`, either one
//! for `Currency`), and whoever receives that leg pays the fee out of it.
//!
//! Pause principle (spec §3.3): a pause, or a listing that is no longer
//! `Approved`, blocks `open_deal` and `accept_deal` but never `cancel_deal` or
//! `expire_deal`: a proposer can always take back its own deposit.
//!
//! Anyone can transfer tokens into a deal vault. Closing a token account
//! requires a zero balance, so such tokens would otherwise block every exit and
//! strand the deposit. Every exit therefore releases the vault's whole balance:
//! the part beyond the deposit (`vault_excess`) goes with the deposit (to the
//! counterparty on accept, to the proposer on cancel or expiry).

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    close_account, transfer_checked, CloseAccount, Mint, TokenAccount, TokenInterface,
    TransferChecked,
};

use crate::{
    constants::*,
    error::ExchangeError,
    events::{DealCancelled, DealExecuted, DealExpired, DealOpened},
    math::{split_payment, PaymentSplit},
    mint_policy::{require_allowed_mint_extensions, require_no_freeze_authority},
    state::*,
    token_vault::{create_pda_token_account, PdaTokenAccount},
};

// ---- open_deal --------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
#[instruction(deal_id: [u8; 16])]
pub struct OpenDeal<'info> {
    /// Pays the rent of the deal (never closed) and of its vault (refunded to
    /// the proposer when the vault closes).
    #[account(mut)]
    pub proposer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,

    #[account(mint::token_program = asset_token_program)]
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = payment_token_program)]
    pub payment_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump = asset_listing.bump,
        constraint = asset_listing.asset_mint == asset_mint.key() @ ExchangeError::InvalidListing
    )]
    pub asset_listing: Box<Account<'info, Listing>>,
    #[account(
        seeds = [LISTING_SEED, payment_mint.key().as_ref()],
        bump = payment_listing.bump,
        constraint = payment_listing.asset_mint == payment_mint.key() @ ExchangeError::InvalidListing
    )]
    pub payment_listing: Box<Account<'info, Listing>>,

    #[account(
        init,
        payer = proposer,
        space = 8 + Deal::INIT_SPACE,
        seeds = [DEAL_SEED, proposer.key().as_ref(), deal_id.as_ref()],
        bump
    )]
    pub deal: Box<Account<'info, Deal>>,
    /// CHECK: escrow vault, created by the handler as a token account of the
    /// deposited mint (which depends on `proposer_side`) owned by `deal`.
    #[account(mut, seeds = [DEAL_VAULT_SEED, deal.key().as_ref()], bump)]
    pub deal_vault: UncheckedAccount<'info>,
    /// The proposer's account of the mint it escrows: the asset when selling,
    /// the payment when buying (mint and token program checked by the handler).
    #[account(mut, token::authority = proposer)]
    pub proposer_deposit_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub asset_token_program: Interface<'info, TokenInterface>,
    pub payment_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handle_open_deal(
    ctx: Context<OpenDeal>,
    deal_id: [u8; 16],
    terms: DealTerms,
    expires_at: i64,
) -> Result<()> {
    let accounts = &ctx.accounts;
    accounts.config.require_not_paused()?;
    require!(
        terms.asset_amount > 0 && terms.payment_amount > 0,
        ExchangeError::ZeroAmount
    );
    let proposer = accounts.proposer.key();
    validate_address(&terms.counterparty)?;
    require_keys_neq!(terms.counterparty, proposer, ExchangeError::SelfTrade);

    let now = Clock::get()?.unix_timestamp;
    let latest = now
        .checked_add(MAX_DEAL_DURATION_SECS)
        .ok_or(ExchangeError::MathOverflow)?;
    require!(
        expires_at > now && expires_at <= latest,
        ExchangeError::InvalidExpiry
    );

    require_p2p_market(
        &accounts.asset_listing,
        &accounts.payment_listing,
        &accounts.asset_mint,
        &accounts.payment_mint,
    )?;
    let fee_leg = accounts.config.fee_leg(
        accounts.asset_listing.asset_class,
        &accounts.asset_mint.key(),
        &accounts.payment_mint.key(),
    )?;
    // Fail early on a deal that could not execute at the current fee
    // (`accept_deal` re-checks with the fee in force then).
    fee_split(
        fee_leg,
        terms.asset_amount,
        terms.payment_amount,
        &accounts.config,
        accounts.asset_listing.cashback_eligible,
    )?;

    let (deposit_mint, deposit_program, deposit_amount) = match terms.proposer_side {
        DealSide::Sell => (
            &accounts.asset_mint,
            accounts.asset_token_program.key(),
            terms.asset_amount,
        ),
        DealSide::Buy => (
            &accounts.payment_mint,
            accounts.payment_token_program.key(),
            terms.payment_amount,
        ),
    };
    require_token_account(
        &accounts.proposer_deposit_account,
        &deposit_mint.key(),
        &deposit_program,
    )?;

    let deal_key = accounts.deal.key();
    let vault_bump = [ctx.bumps.deal_vault];
    let vault_seeds: &[&[&[u8]]] = &[&[DEAL_VAULT_SEED, deal_key.as_ref(), &vault_bump]];
    create_pda_token_account(
        PdaTokenAccount {
            payer: &accounts.proposer.to_account_info(),
            account: &accounts.deal_vault.to_account_info(),
            mint: &deposit_mint.to_account_info(),
            authority: &accounts.deal.to_account_info(),
            token_program: deposit_program,
            system_program: accounts.system_program.key(),
        },
        vault_seeds,
    )?;
    transfer(
        deposit_program,
        accounts.proposer_deposit_account.to_account_info(),
        deposit_mint,
        accounts.deal_vault.to_account_info(),
        accounts.proposer.to_account_info(),
        deposit_amount,
        &[],
    )?;

    let deal = Deal {
        deal_id,
        proposer,
        counterparty: terms.counterparty,
        asset_mint: accounts.asset_mint.key(),
        payment_mint: accounts.payment_mint.key(),
        asset_amount: terms.asset_amount,
        payment_amount: terms.payment_amount,
        proposer_side: terms.proposer_side,
        expires_at,
        status: DealStatus::Open,
        opened_at: now,
        resolved_at: 0,
        bump: ctx.bumps.deal,
        vault_bump: ctx.bumps.deal_vault,
    };
    ctx.accounts.deal.set_inner(deal.clone());
    emit_cpi!(DealOpened {
        deal: deal_key,
        deal_id,
        proposer,
        counterparty: deal.counterparty,
        asset_mint: deal.asset_mint,
        payment_mint: deal.payment_mint,
        proposer_side: deal.proposer_side,
        asset_amount: deal.asset_amount,
        payment_amount: deal.payment_amount,
        expires_at,
    });
    Ok(())
}

// ---- accept_deal ------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct AcceptDeal<'info> {
    pub counterparty: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,

    #[account(mint::token_program = asset_token_program)]
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = payment_token_program)]
    pub payment_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump = asset_listing.bump,
        constraint = asset_listing.asset_mint == asset_mint.key() @ ExchangeError::InvalidListing
    )]
    pub asset_listing: Box<Account<'info, Listing>>,
    #[account(
        seeds = [LISTING_SEED, payment_mint.key().as_ref()],
        bump = payment_listing.bump,
        constraint = payment_listing.asset_mint == payment_mint.key() @ ExchangeError::InvalidListing
    )]
    pub payment_listing: Box<Account<'info, Listing>>,

    /// CHECK: the deal's proposer (`has_one` on `deal`). Receives the vault's
    /// rent when it closes.
    #[account(mut)]
    pub proposer: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [DEAL_SEED, deal.proposer.as_ref(), deal.deal_id.as_ref()],
        bump = deal.bump,
        has_one = counterparty @ ExchangeError::Unauthorized,
        has_one = proposer @ ExchangeError::DealAccountMismatch,
        has_one = asset_mint @ ExchangeError::DealAccountMismatch,
        has_one = payment_mint @ ExchangeError::DealAccountMismatch
    )]
    pub deal: Box<Account<'info, Deal>>,
    #[account(
        mut,
        seeds = [DEAL_VAULT_SEED, deal.key().as_ref()],
        bump = deal.vault_bump,
        token::authority = deal,
        constraint = deal_vault.mint == deal.deposit_mint() @ ExchangeError::DealAccountMismatch
    )]
    pub deal_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = asset_mint,
        token::authority = counterparty,
        token::token_program = asset_token_program
    )]
    pub counterparty_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = payment_mint,
        token::authority = counterparty,
        token::token_program = payment_token_program
    )]
    pub counterparty_payment_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// The proposer's account of the mint it receives: the payment when it
    /// sold, the asset when it bought (mint and token program checked by the
    /// handler).
    #[account(mut, token::authority = proposer)]
    pub proposer_proceeds_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// Ledger and vault of the fee mint (USDT). The handler requires one leg
    /// of the deal to be that mint; the vault was created with it.
    #[account(
        mut,
        seeds = [FEE_LEDGER_SEED, config.fee_mint.as_ref()],
        bump = fee_ledger.bump
    )]
    pub fee_ledger: Box<Account<'info, FeeLedger>>,
    #[account(
        mut,
        seeds = [FEE_VAULT_SEED, config.fee_mint.as_ref()],
        bump = fee_ledger.vault_bump
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Required when the asset listing is cashback-eligible; `None` for
    /// `Currency` assets, which never accrue cashback. Cashback is in the fee
    /// mint, which is then the payment mint.
    #[account(
        mut,
        seeds = [CASHBACK_SEED, asset_listing.key().as_ref(), config.fee_mint.as_ref()],
        bump = cashback_ledger.bump
    )]
    pub cashback_ledger: Option<Account<'info, CashbackLedger>>,

    pub asset_token_program: Interface<'info, TokenInterface>,
    pub payment_token_program: Interface<'info, TokenInterface>,
}

pub fn handle_accept_deal(ctx: Context<AcceptDeal>) -> Result<()> {
    let accounts = &ctx.accounts;
    accounts.config.require_not_paused()?;
    let deal = &accounts.deal;
    require!(deal.status == DealStatus::Open, ExchangeError::DealNotOpen);
    let now = Clock::get()?.unix_timestamp;
    // Invariant 7: live while `now < expires_at`, expirable from then on.
    require!(now < deal.expires_at, ExchangeError::DealExpired);

    let asset_listing = &accounts.asset_listing;
    require_p2p_market(
        asset_listing,
        &accounts.payment_listing,
        &accounts.asset_mint,
        &accounts.payment_mint,
    )?;
    require!(
        !asset_listing.cashback_eligible || accounts.cashback_ledger.is_some(),
        ExchangeError::MissingCashbackLedger
    );

    // Re-checked against the fee mint in force: after a fee-mint switch, a
    // deal in the old one can only be refunded (spec §3.4).
    let fee_leg = accounts.config.fee_leg(
        asset_listing.asset_class,
        &accounts.asset_mint.key(),
        &accounts.payment_mint.key(),
    )?;
    let split = fee_split(
        fee_leg,
        deal.asset_amount,
        deal.payment_amount,
        &accounts.config,
        asset_listing.cashback_eligible,
    )?;

    let asset_leg = Leg {
        mint: &accounts.asset_mint,
        token_program: accounts.asset_token_program.key(),
        counterparty_account: accounts.counterparty_asset_account.to_account_info(),
        amount: deal.asset_amount,
        fee: if fee_leg == FeeLeg::Asset { split.fee } else { 0 },
    };
    let payment_leg = Leg {
        mint: &accounts.payment_mint,
        token_program: accounts.payment_token_program.key(),
        counterparty_account: accounts.counterparty_payment_account.to_account_info(),
        amount: deal.payment_amount,
        fee: if fee_leg == FeeLeg::Payment { split.fee } else { 0 },
    };
    // The proposer escrowed one leg; the counterparty delivers the other.
    let (escrowed, delivered) = match deal.proposer_side {
        DealSide::Sell => (asset_leg, payment_leg),
        DealSide::Buy => (payment_leg, asset_leg),
    };
    require_token_account(
        &accounts.proposer_proceeds_account,
        &delivered.mint.key(),
        &delivered.token_program,
    )?;

    let vault_excess = accounts
        .deal_vault
        .amount
        .checked_sub(deal.deposit_amount())
        .ok_or(ExchangeError::MathOverflow)?;

    let proposer = deal.proposer;
    let deal_id = deal.deal_id;
    let deal_bump = [deal.bump];
    let deal_seeds: &[&[&[u8]]] = &[&[DEAL_SEED, proposer.as_ref(), deal_id.as_ref(), &deal_bump]];
    let deal_info = accounts.deal.to_account_info();
    let counterparty_info = accounts.counterparty.to_account_info();
    let vault_info = accounts.deal_vault.to_account_info();
    let fee_vault_info = accounts.fee_vault.to_account_info();

    // 1. Escrowed leg: vault -> counterparty (minus the fee if it is the USDT
    //    leg, plus any excess), then the fee: vault -> fee vault.
    let to_counterparty = escrowed
        .net()?
        .checked_add(vault_excess)
        .ok_or(ExchangeError::MathOverflow)?;
    transfer(
        escrowed.token_program,
        vault_info.clone(),
        escrowed.mint,
        escrowed.counterparty_account.clone(),
        deal_info.clone(),
        to_counterparty,
        deal_seeds,
    )?;
    transfer(
        escrowed.token_program,
        vault_info.clone(),
        escrowed.mint,
        fee_vault_info.clone(),
        deal_info.clone(),
        escrowed.fee,
        deal_seeds,
    )?;
    // 2. Delivered leg: counterparty -> proposer (minus the fee if it is the
    //    USDT leg), then the fee: counterparty -> fee vault.
    transfer(
        delivered.token_program,
        delivered.counterparty_account.clone(),
        delivered.mint,
        accounts.proposer_proceeds_account.to_account_info(),
        counterparty_info.clone(),
        delivered.net()?,
        &[],
    )?;
    transfer(
        delivered.token_program,
        delivered.counterparty_account.clone(),
        delivered.mint,
        fee_vault_info,
        counterparty_info,
        delivered.fee,
        &[],
    )?;
    // 3. Vault empty: close it, rent back to the proposer.
    close_vault(
        escrowed.token_program,
        vault_info,
        accounts.proposer.to_account_info(),
        deal_info,
        deal_seeds,
    )?;
    let fee_mint = accounts.config.fee_mint;

    // 4. Accounting, as in `settle`.
    ctx.accounts
        .fee_ledger
        .record_fee(split.fee, split.cashback)?;
    if let Some(ledger) = ctx.accounts.cashback_ledger.as_mut() {
        ledger.accrued = ledger
            .accrued
            .checked_add(split.cashback)
            .ok_or(ExchangeError::MathOverflow)?;
    }
    let deal = &mut ctx.accounts.deal;
    deal.status = DealStatus::Executed;
    deal.resolved_at = now;

    let event = DealExecuted {
        deal: deal.key(),
        deal_id,
        proposer,
        counterparty: deal.counterparty,
        asset_mint: deal.asset_mint,
        payment_mint: deal.payment_mint,
        proposer_side: deal.proposer_side,
        asset_amount: deal.asset_amount,
        payment_amount: deal.payment_amount,
        fee_mint,
        fee: split.fee,
        cashback: split.cashback,
        vault_excess,
    };
    emit_cpi!(event);
    Ok(())
}

// ---- cancel_deal / expire_deal ------------------------------------------------

/// Proposer only, while `Open`. Reads neither `Config` nor the listings: a
/// pause or a suspended/delisted listing never blocks it.
#[event_cpi]
#[derive(Accounts)]
pub struct CancelDeal<'info> {
    /// Receives the vault's rent.
    #[account(mut)]
    pub proposer: Signer<'info>,
    #[account(
        mut,
        seeds = [DEAL_SEED, deal.proposer.as_ref(), deal.deal_id.as_ref()],
        bump = deal.bump,
        has_one = proposer @ ExchangeError::Unauthorized
    )]
    pub deal: Box<Account<'info, Deal>>,
    #[account(
        mint::token_program = token_program,
        constraint = deposit_mint.key() == deal.deposit_mint() @ ExchangeError::DealAccountMismatch
    )]
    pub deposit_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [DEAL_VAULT_SEED, deal.key().as_ref()],
        bump = deal.vault_bump,
        token::mint = deposit_mint,
        token::authority = deal,
        token::token_program = token_program
    )]
    pub deal_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = deposit_mint,
        token::authority = proposer,
        token::token_program = token_program
    )]
    pub proposer_refund_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_cancel_deal(ctx: Context<CancelDeal>) -> Result<()> {
    let accounts = &ctx.accounts;
    require!(
        accounts.deal.status == DealStatus::Open,
        ExchangeError::DealNotOpen
    );
    let refund = refund_proposer(
        &accounts.deal,
        &accounts.deal_vault,
        &accounts.deposit_mint,
        &accounts.proposer_refund_account,
        accounts.proposer.to_account_info(),
        accounts.token_program.key(),
    )?;

    let now = Clock::get()?.unix_timestamp;
    let deal = &mut ctx.accounts.deal;
    deal.status = DealStatus::Cancelled;
    deal.resolved_at = now;
    let event = DealCancelled {
        deal: deal.key(),
        deal_id: deal.deal_id,
        proposer: deal.proposer,
        deposit_mint: refund.deposit_mint,
        deposit_amount: refund.deposit_amount,
        vault_excess: refund.vault_excess,
    };
    emit_cpi!(event);
    Ok(())
}

/// Permissionless crank once `now >= expires_at`. Like `cancel_deal`, never
/// blocked by a pause or by the listings' status. The refund can only go to
/// an account owned by the proposer.
#[event_cpi]
#[derive(Accounts)]
pub struct ExpireDeal<'info> {
    /// CHECK: the deal's proposer (`has_one` on `deal`). Receives the vault's
    /// rent; does not sign.
    #[account(mut)]
    pub proposer: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [DEAL_SEED, deal.proposer.as_ref(), deal.deal_id.as_ref()],
        bump = deal.bump,
        has_one = proposer @ ExchangeError::DealAccountMismatch
    )]
    pub deal: Box<Account<'info, Deal>>,
    #[account(
        mint::token_program = token_program,
        constraint = deposit_mint.key() == deal.deposit_mint() @ ExchangeError::DealAccountMismatch
    )]
    pub deposit_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [DEAL_VAULT_SEED, deal.key().as_ref()],
        bump = deal.vault_bump,
        token::mint = deposit_mint,
        token::authority = deal,
        token::token_program = token_program
    )]
    pub deal_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = deposit_mint,
        token::authority = proposer,
        token::token_program = token_program
    )]
    pub proposer_refund_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_expire_deal(ctx: Context<ExpireDeal>) -> Result<()> {
    let accounts = &ctx.accounts;
    require!(
        accounts.deal.status == DealStatus::Open,
        ExchangeError::DealNotOpen
    );
    let now = Clock::get()?.unix_timestamp;
    // Invariant 7.
    require!(
        now >= accounts.deal.expires_at,
        ExchangeError::DealNotExpired
    );
    let refund = refund_proposer(
        &accounts.deal,
        &accounts.deal_vault,
        &accounts.deposit_mint,
        &accounts.proposer_refund_account,
        accounts.proposer.to_account_info(),
        accounts.token_program.key(),
    )?;

    let deal = &mut ctx.accounts.deal;
    deal.status = DealStatus::Expired;
    deal.resolved_at = now;
    let event = DealExpired {
        deal: deal.key(),
        deal_id: deal.deal_id,
        proposer: deal.proposer,
        deposit_mint: refund.deposit_mint,
        deposit_amount: refund.deposit_amount,
        vault_excess: refund.vault_excess,
    };
    emit_cpi!(event);
    Ok(())
}

// ---- helpers ----------------------------------------------------------------

/// Market checks shared by `open_deal` and `accept_deal` (invariants 3, 8 and
/// 10), plus the mint policy re-run on both mints (spec §3.8).
fn require_p2p_market(
    asset_listing: &Listing,
    payment_listing: &Listing,
    asset_mint: &InterfaceAccount<Mint>,
    payment_mint: &InterfaceAccount<Mint>,
) -> Result<()> {
    // A Currency deal (e.g. BRL <-> USDT) needs two different currencies.
    require_keys_neq!(asset_mint.key(), payment_mint.key(), ExchangeError::SameMint);
    require!(
        asset_listing.status == ListingStatus::Approved,
        ExchangeError::ListingNotApproved
    );
    require!(
        asset_listing.asset_class.is_p2p(),
        ExchangeError::WrongTradingMode
    );
    require!(
        payment_listing.is_approved_currency(),
        ExchangeError::InvalidPaymentMint
    );
    require_allowed_mint_extensions(&asset_mint.to_account_info())?;
    require_allowed_mint_extensions(&payment_mint.to_account_info())?;
    // Issuer-listed assets never have a freeze authority. A Currency sold as
    // the asset leg may, like any listed currency (curated risk, spec §3.8).
    if asset_listing.asset_class != AssetClass::Currency {
        require_no_freeze_authority(asset_mint)?;
    }
    Ok(())
}

/// The fee on the USDT leg (spec §3.4): `fee = ceil(usdt_amount × fee_bps)`,
/// and the receiver of that leg must still get something
/// (`ZeroSellerProceeds`). Cashback only for eligible listings, whose USDT
/// leg is always the payment.
fn fee_split(
    fee_leg: FeeLeg,
    asset_amount: u64,
    payment_amount: u64,
    config: &Config,
    cashback_eligible: bool,
) -> Result<PaymentSplit> {
    let usdt_amount = match fee_leg {
        FeeLeg::Asset => asset_amount,
        FeeLeg::Payment => payment_amount,
    };
    let split = split_payment(
        usdt_amount,
        config.fee_bps,
        config.cashback_bps,
        cashback_eligible,
    )?;
    require!(split.seller_receives > 0, ExchangeError::ZeroSellerProceeds);
    Ok(split)
}

/// One leg of a deal at acceptance: `amount` of `mint` changes hands, of
/// which `fee` (non-zero only on the USDT leg) goes to the fee vault instead
/// of the receiver.
struct Leg<'a, 'info> {
    mint: &'a InterfaceAccount<'info, Mint>,
    token_program: Pubkey,
    counterparty_account: AccountInfo<'info>,
    amount: u64,
    fee: u64,
}

impl Leg<'_, '_> {
    /// What the receiver of this leg gets.
    fn net(&self) -> Result<u64> {
        self.amount
            .checked_sub(self.fee)
            .ok_or_else(|| error!(ExchangeError::MathOverflow))
    }
}

/// For token accounts whose mint depends on the deal side.
fn require_token_account(
    account: &InterfaceAccount<TokenAccount>,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Result<()> {
    require!(
        account.mint == *mint && account.to_account_info().owner == token_program,
        ExchangeError::InvalidTokenAccount
    );
    Ok(())
}

struct Refund {
    deposit_mint: Pubkey,
    deposit_amount: u64,
    vault_excess: u64,
}

/// Returns the vault's whole balance (deposit + excess) to the proposer and
/// closes the vault, rent to the proposer.
fn refund_proposer<'info>(
    deal: &Account<'info, Deal>,
    deal_vault: &InterfaceAccount<'info, TokenAccount>,
    deposit_mint: &InterfaceAccount<'info, Mint>,
    refund_account: &InterfaceAccount<'info, TokenAccount>,
    proposer: AccountInfo<'info>,
    token_program: Pubkey,
) -> Result<Refund> {
    let deposit_amount = deal.deposit_amount();
    let balance = deal_vault.amount;
    let vault_excess = balance
        .checked_sub(deposit_amount)
        .ok_or(ExchangeError::MathOverflow)?;
    let bump = [deal.bump];
    let seeds: &[&[&[u8]]] = &[&[
        DEAL_SEED,
        deal.proposer.as_ref(),
        deal.deal_id.as_ref(),
        &bump,
    ]];
    transfer(
        token_program,
        deal_vault.to_account_info(),
        deposit_mint,
        refund_account.to_account_info(),
        deal.to_account_info(),
        balance,
        seeds,
    )?;
    close_vault(
        token_program,
        deal_vault.to_account_info(),
        proposer,
        deal.to_account_info(),
        seeds,
    )?;
    Ok(Refund {
        deposit_mint: deposit_mint.key(),
        deposit_amount,
        vault_excess,
    })
}

/// `transfer_checked` with the mint's own decimals. `signer_seeds` is empty
/// when the authority is a transaction signer. Zero amounts are skipped (e.g.
/// `fee` when `fee_bps == 0`).
fn transfer<'info>(
    token_program: Pubkey,
    from: AccountInfo<'info>,
    mint: &InterfaceAccount<'info, Mint>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    transfer_checked(
        CpiContext::new_with_signer(
            token_program,
            TransferChecked {
                from,
                mint: mint.to_account_info(),
                to,
                authority,
            },
            signer_seeds,
        ),
        amount,
        mint.decimals,
    )
}

fn close_vault<'info>(
    token_program: Pubkey,
    vault: AccountInfo<'info>,
    destination: AccountInfo<'info>,
    deal: AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    close_account(CpiContext::new_with_signer(
        token_program,
        CloseAccount {
            account: vault,
            destination,
            authority: deal,
        },
        signer_seeds,
    ))
}
