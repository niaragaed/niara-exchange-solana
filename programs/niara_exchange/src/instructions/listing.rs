use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::*,
    error::ExchangeError,
    events::{ListingApproved, ListingDelisted, ListingSubmitted, ListingSuspended},
    state::*,
};

/// Issuer-submitted listing for `Equity` / `FixedIncome` / `Credit`.
/// `InterfaceAccount<Mint>` accepts SPL Token and Token-2022 mints.
#[event_cpi]
#[derive(Accounts)]
pub struct SubmitListing<'info> {
    #[account(mut)]
    pub issuer: Signer<'info>,
    pub asset_mint: InterfaceAccount<'info, Mint>,
    #[account(
        init,
        payer = issuer,
        space = 8 + Listing::INIT_SPACE,
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump
    )]
    pub listing: Account<'info, Listing>,
    pub system_program: Program<'info, System>,
}

pub fn handle_submit_listing(
    ctx: Context<SubmitListing>,
    asset_class: AssetClass,
    doc_hash: [u8; 32],
) -> Result<()> {
    require!(
        asset_class != AssetClass::Currency,
        ExchangeError::CurrencyNotSubmittable
    );
    require!(doc_hash != [0u8; 32], ExchangeError::EmptyDocHash);

    let issuer = ctx.accounts.issuer.key();
    let mint_authority: Option<Pubkey> = ctx.accounts.asset_mint.mint_authority.into();
    require!(
        mint_authority == Some(issuer),
        ExchangeError::NotMintAuthority
    );

    let asset_mint = ctx.accounts.asset_mint.key();
    ctx.accounts.listing.set_inner(Listing {
        asset_mint,
        issuer,
        asset_class,
        status: ListingStatus::Pending,
        cashback_eligible: asset_class.cashback_eligible(),
        doc_hash,
        approved_at: 0,
        bump: ctx.bumps.listing,
    });
    emit_cpi!(ListingSubmitted {
        listing: ctx.accounts.listing.key(),
        asset_mint,
        issuer,
        asset_class,
        doc_hash,
    });
    Ok(())
}

/// Niara-curated currency (stablecoin) listing. Stablecoin issuers do not submit
/// listings, so the listing authority creates it directly as `Approved`, with no
/// issuer and no cashback.
#[event_cpi]
#[derive(Accounts)]
pub struct ListCurrency<'info> {
    #[account(mut)]
    pub listing_authority: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = listing_authority @ ExchangeError::Unauthorized
    )]
    pub config: Account<'info, Config>,
    pub asset_mint: InterfaceAccount<'info, Mint>,
    #[account(
        init,
        payer = listing_authority,
        space = 8 + Listing::INIT_SPACE,
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump
    )]
    pub listing: Account<'info, Listing>,
    pub system_program: Program<'info, System>,
}

pub fn handle_list_currency(ctx: Context<ListCurrency>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let asset_class = AssetClass::Currency;
    let listing = Listing {
        asset_mint: ctx.accounts.asset_mint.key(),
        issuer: Pubkey::default(),
        asset_class,
        status: ListingStatus::Approved,
        cashback_eligible: asset_class.cashback_eligible(),
        doc_hash: [0u8; 32],
        approved_at: now,
        bump: ctx.bumps.listing,
    };
    ctx.accounts.listing.set_inner(listing.clone());
    emit_cpi!(ListingApproved {
        listing: ctx.accounts.listing.key(),
        asset_mint: listing.asset_mint,
        issuer: listing.issuer,
        asset_class,
        cashback_eligible: listing.cashback_eligible,
        doc_hash: listing.doc_hash,
        approved_at: now,
    });
    Ok(())
}

/// Shared by `approve_listing`, `suspend_listing` and `delist`.
#[event_cpi]
#[derive(Accounts)]
pub struct UpdateListingStatus<'info> {
    pub listing_authority: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = listing_authority @ ExchangeError::Unauthorized
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [LISTING_SEED, listing.asset_mint.as_ref()],
        bump = listing.bump
    )]
    pub listing: Account<'info, Listing>,
}

fn transition(listing: &mut Listing, next: ListingStatus) -> Result<()> {
    require!(
        listing.status.can_transition_to(next),
        ExchangeError::InvalidStatusTransition
    );
    listing.status = next;
    Ok(())
}

pub fn handle_approve_listing(ctx: Context<UpdateListingStatus>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let listing = &mut ctx.accounts.listing;
    transition(listing, ListingStatus::Approved)?;
    listing.approved_at = now;
    let event = ListingApproved {
        listing: listing.key(),
        asset_mint: listing.asset_mint,
        issuer: listing.issuer,
        asset_class: listing.asset_class,
        cashback_eligible: listing.cashback_eligible,
        doc_hash: listing.doc_hash,
        approved_at: now,
    };
    emit_cpi!(event);
    Ok(())
}

pub fn handle_suspend_listing(ctx: Context<UpdateListingStatus>) -> Result<()> {
    let listing = &mut ctx.accounts.listing;
    transition(listing, ListingStatus::Suspended)?;
    let event = ListingSuspended {
        listing: listing.key(),
        asset_mint: listing.asset_mint,
    };
    emit_cpi!(event);
    Ok(())
}

pub fn handle_delist(ctx: Context<UpdateListingStatus>) -> Result<()> {
    let listing = &mut ctx.accounts.listing;
    transition(listing, ListingStatus::Delisted)?;
    let event = ListingDelisted {
        listing: listing.key(),
        asset_mint: listing.asset_mint,
    };
    emit_cpi!(event);
    Ok(())
}
