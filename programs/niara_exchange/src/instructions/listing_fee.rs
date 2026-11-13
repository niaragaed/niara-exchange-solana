//! On-chain listing fee (spec §3.13): the listing authority quotes it, the
//! issuer pays it into the fee vault, and paying is what approves an issuer
//! listing. `Currency` listings never pay one.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{
    constants::*,
    error::ExchangeError,
    events::{ListingApproved, ListingFeePaid, ListingQuoted},
    instructions::listing::{transition, UpdateListingStatus},
    math::listing_fee,
    mint_policy::require_allowed_mint_extensions,
    state::*,
};

// ---- quote_listing ----------------------------------------------------------

/// Quotes (or re-quotes, until paid) the listing fee of a `Pending` or
/// `Quoted` issuer listing. Listing authority; allowed while paused. The fee
/// is in `Config.fee_mint` and the valuation becomes public.
pub fn handle_quote_listing(
    ctx: Context<UpdateListingStatus>,
    valuation: u64,
    listing_fee_bps: u16,
    expires_at: i64,
) -> Result<()> {
    let fee_mint = ctx.accounts.config.fee_mint;
    let listing = &mut ctx.accounts.listing;
    // Checked before the status, so every `Currency` listing gets this error
    // whatever its status (spec §3.13).
    require!(
        listing.asset_class != AssetClass::Currency,
        ExchangeError::CurrencyHasNoListingFee
    );
    transition(listing, ListingStatus::Quoted)?;
    require!(valuation > 0, ExchangeError::ZeroValuation);
    require!(
        (1..=MAX_LISTING_FEE_BPS).contains(&listing_fee_bps),
        ExchangeError::InvalidListingFeeBps
    );
    let now = Clock::get()?.unix_timestamp;
    let latest = now
        .checked_add(MAX_QUOTE_DURATION_SECS)
        .ok_or(ExchangeError::MathOverflow)?;
    require!(
        expires_at > now && expires_at <= latest,
        ExchangeError::InvalidExpiry
    );

    let fee = listing_fee(valuation, listing_fee_bps)?;
    listing.valuation = valuation;
    listing.listing_fee_bps = listing_fee_bps;
    listing.listing_fee = fee;
    listing.listing_fee_mint = fee_mint;
    listing.quote_expires_at = expires_at;

    let event = ListingQuoted {
        listing: listing.key(),
        asset_mint: listing.asset_mint,
        fee_mint,
        valuation,
        listing_fee_bps,
        listing_fee: fee,
        expires_at,
    };
    emit_cpi!(event);
    Ok(())
}

// ---- pay_listing_fee --------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct PayListingFee<'info> {
    /// Signs the transfer out of its own account of the fee mint.
    pub issuer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// `Currency` listings have `issuer == Pubkey::default()`, which cannot sign.
    #[account(
        mut,
        seeds = [LISTING_SEED, listing.asset_mint.as_ref()],
        bump = listing.bump,
        has_one = issuer @ ExchangeError::Unauthorized
    )]
    pub listing: Box<Account<'info, Listing>>,
    /// The fee mint in force. The handler also requires it to be the mint
    /// the quote was made in, after the status check (an unquoted listing
    /// has no quote mint, and must fail with `ListingNotQuoted`).
    #[account(
        mint::token_program = token_program,
        constraint = fee_mint.key() == config.fee_mint @ ExchangeError::NotFeeMint
    )]
    pub fee_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [FEE_LEDGER_SEED, fee_mint.key().as_ref()],
        bump = fee_ledger.bump
    )]
    pub fee_ledger: Box<Account<'info, FeeLedger>>,
    /// The fee vault is a PDA that only `init_fee_vault` creates, so the
    /// seeds and the mint identify it; no vault-authority account is needed
    /// to pay into it.
    #[account(
        mut,
        seeds = [FEE_VAULT_SEED, fee_mint.key().as_ref()],
        bump = fee_ledger.vault_bump,
        token::mint = fee_mint,
        token::token_program = token_program
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Any account of the fee mint owned by the issuer.
    #[account(
        mut,
        token::mint = fee_mint,
        token::authority = issuer,
        token::token_program = token_program
    )]
    pub source: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

/// Issuer pays the quoted fee; the listing becomes `Approved`. Blocked while
/// paused. Charged once per listing: afterwards the status is never `Quoted`
/// again.
pub fn handle_pay_listing_fee(ctx: Context<PayListingFee>) -> Result<()> {
    let accounts = &ctx.accounts;
    accounts.config.require_not_paused()?;
    require!(
        accounts.listing.status == ListingStatus::Quoted,
        ExchangeError::ListingNotQuoted
    );
    let now = Clock::get()?.unix_timestamp;
    require!(
        now < accounts.listing.quote_expires_at,
        ExchangeError::QuoteExpired
    );
    // The quote is in base units of the fee mint it was made in: after a
    // fee-mint switch it must be re-quoted (spec §3.13).
    require!(
        accounts.fee_mint.key() == accounts.listing.listing_fee_mint,
        ExchangeError::NotFeeMint
    );
    // Defense in depth: `init_fee_vault` already checked this mint, and the
    // rejected extensions cannot be added later (spec §3.8).
    require_allowed_mint_extensions(&accounts.fee_mint.to_account_info())?;

    let amount = accounts.listing.listing_fee;
    transfer_checked(
        CpiContext::new(
            accounts.token_program.key(),
            TransferChecked {
                from: accounts.source.to_account_info(),
                mint: accounts.fee_mint.to_account_info(),
                to: accounts.fee_vault.to_account_info(),
                authority: accounts.issuer.to_account_info(),
            },
        ),
        amount,
        accounts.fee_mint.decimals,
    )?;

    ctx.accounts.fee_ledger.record_listing_fee(amount)?;
    let fee_mint = ctx.accounts.fee_mint.key();
    let listing = &mut ctx.accounts.listing;
    transition(listing, ListingStatus::Approved)?;
    listing.listing_fee_paid = amount;
    listing.approved_at = now;

    let paid = ListingFeePaid {
        listing: listing.key(),
        asset_mint: listing.asset_mint,
        issuer: listing.issuer,
        fee_mint,
        valuation: listing.valuation,
        listing_fee_bps: listing.listing_fee_bps,
        amount,
    };
    let approved = ListingApproved {
        listing: listing.key(),
        asset_mint: listing.asset_mint,
        issuer: listing.issuer,
        asset_class: listing.asset_class,
        cashback_eligible: listing.cashback_eligible,
        doc_hash: listing.doc_hash,
        approved_at: now,
    };
    emit_cpi!(paid);
    emit_cpi!(approved);
    Ok(())
}
