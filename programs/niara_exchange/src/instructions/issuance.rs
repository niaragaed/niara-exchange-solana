//! Niara-approved issuance (spec §3.12): the issuer requests numbered batches,
//! the listing authority approves or rejects them, and only the program mints,
//! through the `["mint_authority", asset_mint]` PDA that `submit_listing` made
//! the mint authority. Holders burn through `burn_with_reason`.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    burn_checked, mint_to_checked, BurnChecked, Mint, MintToChecked, TokenAccount,
    TokenInterface,
};

use crate::{
    constants::*,
    error::ExchangeError,
    events::{IssuanceExecuted, IssuanceRejected, IssuanceRequested, TokensBurned},
    state::*,
};

// ---- request_issuance -------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct RequestIssuance<'info> {
    /// Pays the rent of the batch record.
    #[account(mut)]
    pub issuer: Signer<'info>,
    /// `Currency` listings have `issuer == Pubkey::default()`, which cannot sign.
    #[account(
        mut,
        seeds = [LISTING_SEED, listing.asset_mint.as_ref()],
        bump = listing.bump,
        has_one = issuer @ ExchangeError::Unauthorized
    )]
    pub listing: Box<Account<'info, Listing>>,
    /// Batch numbers are sequential per listing, starting at 1.
    #[account(
        init,
        payer = issuer,
        space = 8 + IssuanceBatch::INIT_SPACE,
        seeds = [BATCH_SEED, listing.key().as_ref(), &listing.next_batch_no.to_le_bytes()],
        bump
    )]
    pub batch: Box<Account<'info, IssuanceBatch>>,
    pub system_program: Program<'info, System>,
}

pub fn handle_request_issuance(
    ctx: Context<RequestIssuance>,
    amount: u64,
    recipient: Pubkey,
    doc_hash: [u8; 32],
) -> Result<()> {
    let listing = &mut ctx.accounts.listing;
    // Only an `Approved` listing (listing fee paid, spec §3.13) takes new
    // batches: nothing is requested while it is `Pending`, `Quoted`,
    // `Suspended` or `Delisted`.
    require!(
        listing.status == ListingStatus::Approved,
        ExchangeError::ListingNotIssuable
    );
    require!(amount > 0, ExchangeError::ZeroAmount);
    require!(doc_hash != [0u8; 32], ExchangeError::EmptyDocHash);
    validate_address(&recipient)?;

    let batch_no = listing.next_batch_no;
    listing.next_batch_no = batch_no
        .checked_add(1)
        .ok_or(ExchangeError::MathOverflow)?;
    let now = Clock::get()?.unix_timestamp;
    let listing_key = listing.key();
    let asset_mint = listing.asset_mint;
    ctx.accounts.batch.set_inner(IssuanceBatch {
        listing: listing_key,
        batch_no,
        amount,
        recipient,
        doc_hash,
        status: BatchStatus::Requested,
        requested_at: now,
        resolved_at: 0,
        bump: ctx.bumps.batch,
    });
    emit_cpi!(IssuanceRequested {
        listing: listing_key,
        asset_mint,
        batch_no,
        amount,
        recipient,
        doc_hash,
    });
    Ok(())
}

// ---- approve_issuance -------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct ApproveIssuance<'info> {
    pub listing_authority: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = listing_authority @ ExchangeError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump = listing.bump
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(
        mut,
        seeds = [BATCH_SEED, listing.key().as_ref(), &batch.batch_no.to_le_bytes()],
        bump = batch.bump,
        has_one = listing @ ExchangeError::InvalidListing
    )]
    pub batch: Box<Account<'info, IssuanceBatch>>,
    #[account(mut, mint::token_program = token_program)]
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: PDA mint authority of `asset_mint`; holds no data.
    #[account(seeds = [MINT_AUTHORITY_SEED, asset_mint.key().as_ref()], bump)]
    pub mint_authority: UncheckedAccount<'info>,
    /// Any account of the asset mint owned by the batch's recipient.
    #[account(
        mut,
        token::mint = asset_mint,
        token::token_program = token_program,
        constraint = destination.owner == batch.recipient @ ExchangeError::WrongRecipient
    )]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_approve_issuance(ctx: Context<ApproveIssuance>) -> Result<()> {
    ctx.accounts.config.require_not_paused()?;
    require!(
        ctx.accounts.batch.status == BatchStatus::Requested,
        ExchangeError::BatchNotRequested
    );
    require!(
        ctx.accounts.listing.status == ListingStatus::Approved,
        ExchangeError::ListingNotApproved
    );

    let amount = ctx.accounts.batch.amount;
    let mint_key = ctx.accounts.asset_mint.key();
    let bump = [ctx.bumps.mint_authority];
    let signer_seeds: &[&[&[u8]]] = &[&[MINT_AUTHORITY_SEED, mint_key.as_ref(), &bump]];
    mint_to_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            MintToChecked {
                mint: ctx.accounts.asset_mint.to_account_info(),
                to: ctx.accounts.destination.to_account_info(),
                authority: ctx.accounts.mint_authority.to_account_info(),
            },
            signer_seeds,
        ),
        amount,
        ctx.accounts.asset_mint.decimals,
    )?;

    let listing = &mut ctx.accounts.listing;
    listing.issued = listing
        .issued
        .checked_add(amount)
        .ok_or(ExchangeError::MathOverflow)?;
    let batch = &mut ctx.accounts.batch;
    batch.status = BatchStatus::Executed;
    batch.resolved_at = Clock::get()?.unix_timestamp;

    let event = IssuanceExecuted {
        asset_mint: mint_key,
        batch_no: batch.batch_no,
        amount,
        recipient: batch.recipient,
        doc_hash: batch.doc_hash,
        destination: ctx.accounts.destination.key(),
    };
    emit_cpi!(event);
    Ok(())
}

// ---- reject_issuance --------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct RejectIssuance<'info> {
    pub listing_authority: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = listing_authority @ ExchangeError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [LISTING_SEED, listing.asset_mint.as_ref()], bump = listing.bump)]
    pub listing: Box<Account<'info, Listing>>,
    #[account(
        mut,
        seeds = [BATCH_SEED, listing.key().as_ref(), &batch.batch_no.to_le_bytes()],
        bump = batch.bump,
        has_one = listing @ ExchangeError::InvalidListing
    )]
    pub batch: Box<Account<'info, IssuanceBatch>>,
}

pub fn handle_reject_issuance(ctx: Context<RejectIssuance>) -> Result<()> {
    let batch = &mut ctx.accounts.batch;
    require!(
        batch.status == BatchStatus::Requested,
        ExchangeError::BatchNotRequested
    );
    batch.status = BatchStatus::Rejected;
    batch.resolved_at = Clock::get()?.unix_timestamp;
    let event = IssuanceRejected {
        asset_mint: ctx.accounts.listing.asset_mint,
        batch_no: batch.batch_no,
        amount: batch.amount,
        recipient: batch.recipient,
    };
    emit_cpi!(event);
    Ok(())
}

// ---- burn_with_reason -------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct BurnWithReason<'info> {
    pub holder: Signer<'info>,
    #[account(
        mut,
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump = listing.bump,
        constraint = listing.asset_class != AssetClass::Currency @ ExchangeError::CurrencyNotIssued
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(mut, mint::token_program = token_program)]
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = asset_mint,
        token::authority = holder,
        token::token_program = token_program
    )]
    pub holder_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_burn_with_reason(
    ctx: Context<BurnWithReason>,
    amount: u64,
    reason: BurnReason,
    reference: [u8; 32],
) -> Result<()> {
    require!(amount > 0, ExchangeError::ZeroAmount);
    require!(reference != [0u8; 32], ExchangeError::EmptyReference);

    burn_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            BurnChecked {
                mint: ctx.accounts.asset_mint.to_account_info(),
                from: ctx.accounts.holder_account.to_account_info(),
                authority: ctx.accounts.holder.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.asset_mint.decimals,
    )?;
    let listing = &mut ctx.accounts.listing;
    listing.burned = listing
        .burned
        .checked_add(amount)
        .ok_or(ExchangeError::MathOverflow)?;

    emit_cpi!(TokensBurned {
        asset_mint: ctx.accounts.asset_mint.key(),
        holder: ctx.accounts.holder.key(),
        holder_account: ctx.accounts.holder_account.key(),
        amount,
        reason,
        reference,
    });
    Ok(())
}
