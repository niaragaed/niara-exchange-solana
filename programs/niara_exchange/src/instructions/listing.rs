use anchor_lang::prelude::*;
use anchor_spl::{
    token_2022::spl_token_2022::instruction::AuthorityType,
    token_interface::{set_authority, Mint, SetAuthority, TokenInterface},
};

use crate::{
    constants::*,
    error::ExchangeError,
    events::{
        CashbackFreezeSet, ListingApproved, ListingDelisted, ListingSubmitted, ListingSuspended,
    },
    mint_policy::{require_allowed_mint_extensions, require_no_freeze_authority},
    state::*,
    token_vault::{create_pda_token_account, PdaTokenAccount},
};

/// Issuer-submitted listing for `Equity` / `FixedIncome` / `Credit`.
/// `InterfaceAccount<Mint>` accepts SPL Token and Token-2022 mints.
/// The issuer's mint authority moves to the program here (spec §3.12).
#[event_cpi]
#[derive(Accounts)]
pub struct SubmitListing<'info> {
    #[account(mut)]
    pub issuer: Signer<'info>,
    #[account(mut, mint::token_program = token_program)]
    pub asset_mint: InterfaceAccount<'info, Mint>,
    #[account(
        init,
        payer = issuer,
        space = 8 + Listing::INIT_SPACE,
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump
    )]
    pub listing: Account<'info, Listing>,
    /// CHECK: PDA that becomes the mint authority; holds no data.
    #[account(seeds = [MINT_AUTHORITY_SEED, asset_mint.key().as_ref()], bump)]
    pub mint_authority: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
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
    require_allowed_mint_extensions(&ctx.accounts.asset_mint.to_account_info())?;
    require_no_freeze_authority(&ctx.accounts.asset_mint)?;

    let issuer = ctx.accounts.issuer.key();
    let mint_authority: Option<Pubkey> = ctx.accounts.asset_mint.mint_authority.into();
    require!(
        mint_authority == Some(issuer),
        ExchangeError::NotMintAuthority
    );
    // Every unit in circulation must come from an approved batch (invariant 11).
    require!(
        ctx.accounts.asset_mint.supply == 0,
        ExchangeError::NonZeroSupply
    );

    // Hand the mint authority to the program, signed by the issuer while it
    // still holds it. No instruction ever moves it again.
    set_authority(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            SetAuthority {
                current_authority: ctx.accounts.issuer.to_account_info(),
                account_or_mint: ctx.accounts.asset_mint.to_account_info(),
            },
        ),
        AuthorityType::MintTokens,
        Some(ctx.accounts.mint_authority.key()),
    )?;

    let asset_mint = ctx.accounts.asset_mint.key();
    ctx.accounts.listing.set_inner(Listing {
        asset_mint,
        issuer,
        asset_class,
        status: ListingStatus::Pending,
        cashback_eligible: asset_class.cashback_eligible(),
        doc_hash,
        approved_at: 0,
        next_batch_no: 1,
        issued: 0,
        burned: 0,
        cashback_frozen: false,
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
/// issuer and no cashback. Unlike asset mints, a currency mint may have a freeze
/// authority (USDC does): an accepted, curated risk (spec §3.8).
/// Also creates the currency's fee vault and fee ledger, so `settle` never has
/// to.
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
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = listing_authority,
        space = 8 + Listing::INIT_SPACE,
        seeds = [LISTING_SEED, asset_mint.key().as_ref()],
        bump
    )]
    pub listing: Box<Account<'info, Listing>>,
    /// CHECK: PDA signer of the fee vaults; holds no data.
    #[account(seeds = [VAULT_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    /// CHECK: fee vault for this currency, created and initialized by the
    /// handler *after* the mint allowlist check (an Anchor `init` would run
    /// first and fail on some rejected mints with an unrelated error). A PDA
    /// rather than an ATA: anyone can create an ATA, so creating one could be
    /// front-run to block this listing.
    #[account(mut, seeds = [FEE_VAULT_SEED, asset_mint.key().as_ref()], bump)]
    pub fee_vault: UncheckedAccount<'info>,
    #[account(
        init,
        payer = listing_authority,
        space = 8 + FeeLedger::INIT_SPACE,
        seeds = [FEE_LEDGER_SEED, asset_mint.key().as_ref()],
        bump
    )]
    pub fee_ledger: Box<Account<'info, FeeLedger>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handle_list_currency(ctx: Context<ListCurrency>) -> Result<()> {
    // Mint policy first, before anything is created for this mint.
    require_allowed_mint_extensions(&ctx.accounts.asset_mint.to_account_info())?;
    create_fee_vault(&ctx.accounts, ctx.bumps.fee_vault)?;

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
        // Not issued through Niara: no batches, no issuance accounting.
        next_batch_no: 0,
        issued: 0,
        burned: 0,
        cashback_frozen: false,
        bump: ctx.bumps.listing,
    };
    ctx.accounts.listing.set_inner(listing.clone());
    ctx.accounts.fee_ledger.set_inner(FeeLedger {
        payment_mint: listing.asset_mint,
        fees_collected: 0,
        cashback_accrued: 0,
        protocol_withdrawn: 0,
        cashback_withdrawn: 0,
        bump: ctx.bumps.fee_ledger,
        vault_bump: ctx.bumps.fee_vault,
    });
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

/// Creates `["fee_vault", mint]` as a token account of `mint` owned by the vault
/// authority, after the mint policy check (see `token_vault`).
fn create_fee_vault(accounts: &ListCurrency, vault_bump: u8) -> Result<()> {
    let mint_key = accounts.asset_mint.key();
    let bump = [vault_bump];
    let signer_seeds: &[&[&[u8]]] = &[&[FEE_VAULT_SEED, mint_key.as_ref(), &bump]];
    create_pda_token_account(
        PdaTokenAccount {
            payer: &accounts.listing_authority.to_account_info(),
            account: &accounts.fee_vault.to_account_info(),
            mint: &accounts.asset_mint.to_account_info(),
            authority: &accounts.vault_authority.to_account_info(),
            token_program: accounts.token_program.key(),
            system_program: accounts.system_program.key(),
        },
        signer_seeds,
    )
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

// ---- cashback freeze (spec §9.5) ---------------------------------------------

/// Stops `withdraw_cashback` for one listing; accrual continues. Listing
/// authority, no timelock (containment must act immediately), any listing
/// status, allowed while paused.
pub fn handle_freeze_cashback(ctx: Context<UpdateListingStatus>) -> Result<()> {
    set_cashback_frozen(&mut ctx.accounts.listing, true)?;
    let event = cashback_freeze_event(&ctx.accounts.listing, &ctx.accounts.listing_authority);
    emit_cpi!(event);
    Ok(())
}

pub fn handle_unfreeze_cashback(ctx: Context<UpdateListingStatus>) -> Result<()> {
    set_cashback_frozen(&mut ctx.accounts.listing, false)?;
    let event = cashback_freeze_event(&ctx.accounts.listing, &ctx.accounts.listing_authority);
    emit_cpi!(event);
    Ok(())
}

fn set_cashback_frozen(listing: &mut Listing, frozen: bool) -> Result<()> {
    // `Currency` listings never accrue cashback: nothing to freeze.
    require!(listing.cashback_eligible, ExchangeError::NotCashbackEligible);
    if frozen {
        require!(!listing.cashback_frozen, ExchangeError::CashbackAlreadyFrozen);
    } else {
        require!(listing.cashback_frozen, ExchangeError::CashbackNotFrozen);
    }
    listing.cashback_frozen = frozen;
    Ok(())
}

fn cashback_freeze_event(listing: &Account<Listing>, authority: &Signer) -> CashbackFreezeSet {
    CashbackFreezeSet {
        listing: listing.key(),
        asset_mint: listing.asset_mint,
        frozen: listing.cashback_frozen,
        listing_authority: authority.key(),
    }
}
