use anchor_lang::{prelude::*, solana_program::program_pack::Pack, system_program};
use anchor_spl::{
    token_2022::spl_token_2022::{
        extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions},
        state::{Account as TokenAccountState, Mint as MintState},
    },
    token_interface::{initialize_account3, InitializeAccount3, Mint, TokenInterface},
};

use crate::{
    constants::*,
    error::ExchangeError,
    events::{ListingApproved, ListingDelisted, ListingSubmitted, ListingSuspended},
    mint_policy::{require_allowed_mint_extensions, require_no_freeze_authority},
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
    require_allowed_mint_extensions(&ctx.accounts.asset_mint.to_account_info())?;
    require_no_freeze_authority(&ctx.accounts.asset_mint)?;

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
/// authority: what an Anchor `token::` `init` does, but callable after the mint
/// policy check. Like Anchor's `init`, it tolerates lamports sent to the
/// address beforehand (anyone can do that): it then tops up, allocates and
/// assigns instead of `create_account`, which would fail on a funded address.
fn create_fee_vault(accounts: &ListCurrency, vault_bump: u8) -> Result<()> {
    let mint_key = accounts.asset_mint.key();
    let bump = [vault_bump];
    let signer_seeds: &[&[&[u8]]] = &[&[FEE_VAULT_SEED, mint_key.as_ref(), &bump]];
    let mint = accounts.asset_mint.to_account_info();
    let vault = accounts.fee_vault.to_account_info();
    let payer = accounts.listing_authority.to_account_info();
    let token_program = accounts.token_program.key();
    let system = accounts.system_program.key();

    let space = token_account_space(&mint)?;
    let rent = Rent::get()?.minimum_balance(space);
    let current = vault.lamports();
    if current == 0 {
        system_program::create_account(
            CpiContext::new_with_signer(
                system,
                system_program::CreateAccount {
                    from: payer,
                    to: vault.clone(),
                },
                signer_seeds,
            ),
            rent,
            space as u64,
            &token_program,
        )?;
    } else {
        let top_up = rent.saturating_sub(current);
        if top_up > 0 {
            system_program::transfer(
                CpiContext::new(
                    system,
                    system_program::Transfer {
                        from: payer,
                        to: vault.clone(),
                    },
                ),
                top_up,
            )?;
        }
        system_program::allocate(
            CpiContext::new_with_signer(
                system,
                system_program::Allocate {
                    account_to_allocate: vault.clone(),
                },
                signer_seeds,
            ),
            space as u64,
        )?;
        system_program::assign(
            CpiContext::new_with_signer(
                system,
                system_program::Assign {
                    account_to_assign: vault.clone(),
                },
                signer_seeds,
            ),
            &token_program,
        )?;
    }
    initialize_account3(CpiContext::new(
        token_program,
        InitializeAccount3 {
            account: vault,
            mint,
            authority: accounts.vault_authority.to_account_info(),
        },
    ))
}

/// Size of a token account for `mint`, including the account extensions its
/// mint extensions require (same computation as Anchor's `init`). Only called
/// on mints that passed the allowlist, so every extension type is known.
fn token_account_space(mint: &AccountInfo) -> Result<usize> {
    if *mint.owner != anchor_spl::token_2022::ID {
        return Ok(TokenAccountState::LEN);
    }
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<MintState>::unpack(&data)?;
    let mint_extensions = state.get_extension_types()?;
    let required = ExtensionType::get_required_init_account_extensions(&mint_extensions);
    Ok(ExtensionType::try_calculate_account_len::<TokenAccountState>(&required)?)
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
