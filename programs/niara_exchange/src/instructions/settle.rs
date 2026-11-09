//! Order-book settlement (spec §3.4): an atomic swap through SPL delegates.
//!
//! The seller approved the vault PDA as delegate of its asset account, and the
//! buyer did the same for its payment account. The settlement operator moves
//! both legs and the fee in one instruction; any failed check or CPI reverts
//! everything.

use anchor_lang::{prelude::*, solana_program::program_option::COption};
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{
    constants::*,
    error::ExchangeError,
    events::Settled,
    math::split_payment,
    mint_policy::{require_allowed_mint_extensions, require_no_freeze_authority},
    state::*,
};

#[event_cpi]
#[derive(Accounts)]
#[instruction(order_id: [u8; 16])]
pub struct Settle<'info> {
    /// Pays the rent of the `SettlementRecord`.
    #[account(mut)]
    pub settlement_operator: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = settlement_operator @ ExchangeError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,

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

    #[account(mint::token_program = asset_token_program)]
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = payment_token_program)]
    pub payment_mint: Box<InterfaceAccount<'info, Mint>>,

    /// CHECK: the seller's wallet. Only used as the required owner of the two
    /// seller token accounts below; it does not sign.
    pub seller: UncheckedAccount<'info>,
    /// CHECK: the buyer's wallet. Only used as the required owner of the two
    /// buyer token accounts below; it does not sign.
    pub buyer: UncheckedAccount<'info>,

    #[account(
        mut,
        token::mint = asset_mint,
        token::authority = seller,
        token::token_program = asset_token_program
    )]
    pub seller_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = asset_mint,
        token::authority = buyer,
        token::token_program = asset_token_program
    )]
    pub buyer_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = payment_mint,
        token::authority = buyer,
        token::token_program = payment_token_program
    )]
    pub buyer_payment_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = payment_mint,
        token::authority = seller,
        token::token_program = payment_token_program
    )]
    pub seller_payment_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: PDA signer for delegated transfers; holds no data.
    #[account(seeds = [VAULT_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [FEE_VAULT_SEED, payment_mint.key().as_ref()],
        bump,
        token::mint = payment_mint,
        token::authority = vault_authority,
        token::token_program = payment_token_program
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [CASHBACK_SEED, asset_listing.key().as_ref(), payment_mint.key().as_ref()],
        bump = cashback_ledger.bump
    )]
    pub cashback_ledger: Box<Account<'info, CashbackLedger>>,

    /// Replay guard: `init` fails if this match was already settled.
    #[account(
        init,
        payer = settlement_operator,
        space = 8 + SettlementRecord::INIT_SPACE,
        seeds = [SETTLED_SEED, order_id.as_ref()],
        bump
    )]
    pub settlement_record: Box<Account<'info, SettlementRecord>>,

    pub asset_token_program: Interface<'info, TokenInterface>,
    pub payment_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

/// `order_id` is the off-chain **match** (fill) id, a UUID: one maker order can
/// be filled by several matches, each settled once.
/// `payment_amount` is in raw units of the payment mint; the program does no
/// price conversion.
pub fn handle_settle(
    ctx: Context<Settle>,
    order_id: [u8; 16],
    asset_amount: u64,
    payment_amount: u64,
) -> Result<()> {
    let accounts = &ctx.accounts;
    accounts.config.require_not_paused()?;
    require!(
        asset_amount > 0 && payment_amount > 0,
        ExchangeError::ZeroAmount
    );

    let buyer = accounts.buyer.key();
    let seller = accounts.seller.key();
    let vault = accounts.vault_authority.key();
    require_keys_neq!(buyer, seller, ExchangeError::SelfTrade);
    require!(
        buyer != vault && seller != vault,
        ExchangeError::InvalidCounterparty
    );

    // Mode/class coupling (invariants 3, 8 and 10).
    let asset_listing = &accounts.asset_listing;
    require!(
        asset_listing.status == ListingStatus::Approved,
        ExchangeError::ListingNotApproved
    );
    require!(
        asset_listing.asset_class.is_order_book(),
        ExchangeError::WrongTradingMode
    );
    require!(
        accounts.payment_listing.is_approved_currency(),
        ExchangeError::InvalidPaymentMint
    );

    // Mint policy, re-run as defense in depth (spec §3.8).
    require_allowed_mint_extensions(&accounts.asset_mint.to_account_info())?;
    require_allowed_mint_extensions(&accounts.payment_mint.to_account_info())?;
    require_no_freeze_authority(&accounts.asset_mint)?;

    require_delegation(&accounts.seller_asset_account, &vault, asset_amount)?;
    require_delegation(&accounts.buyer_payment_account, &vault, payment_amount)?;

    let split = split_payment(
        payment_amount,
        accounts.config.fee_bps,
        accounts.config.cashback_bps,
        asset_listing.cashback_eligible,
    )?;

    let bump = [ctx.bumps.vault_authority];
    let signer_seeds: &[&[&[u8]]] = &[&[VAULT_SEED, &bump]];
    let vault_info = accounts.vault_authority.to_account_info();

    // 1. Asset: seller -> buyer.
    transfer(
        &accounts.asset_token_program,
        &accounts.seller_asset_account,
        &accounts.asset_mint,
        accounts.buyer_asset_account.to_account_info(),
        vault_info.clone(),
        asset_amount,
        signer_seeds,
    )?;
    // 2. Payment: buyer -> seller.
    transfer(
        &accounts.payment_token_program,
        &accounts.buyer_payment_account,
        &accounts.payment_mint,
        accounts.seller_payment_account.to_account_info(),
        vault_info.clone(),
        split.seller_receives,
        signer_seeds,
    )?;
    // 3. Fee: buyer -> fee vault.
    transfer(
        &accounts.payment_token_program,
        &accounts.buyer_payment_account,
        &accounts.payment_mint,
        accounts.fee_vault.to_account_info(),
        vault_info,
        split.fee,
        signer_seeds,
    )?;

    // 4. Accounting.
    let ledger = &mut ctx.accounts.cashback_ledger;
    ledger.accrued = ledger
        .accrued
        .checked_add(split.cashback)
        .ok_or(ExchangeError::MathOverflow)?;
    ctx.accounts.settlement_record.bump = ctx.bumps.settlement_record;

    // 5. Event.
    emit_cpi!(Settled {
        order_id,
        asset_mint: ctx.accounts.asset_mint.key(),
        payment_mint: ctx.accounts.payment_mint.key(),
        buyer,
        seller,
        asset_amount,
        payment_amount,
        fee: split.fee,
        cashback: split.cashback,
    });
    Ok(())
}

fn require_delegation(
    account: &InterfaceAccount<TokenAccount>,
    vault: &Pubkey,
    amount: u64,
) -> Result<()> {
    require!(
        account.delegate == COption::Some(*vault),
        ExchangeError::DelegateNotSet
    );
    require!(
        account.delegated_amount >= amount,
        ExchangeError::InsufficientDelegation
    );
    Ok(())
}

/// `transfer_checked` signed by the vault PDA as delegate, with the mint's own
/// decimals. Zero amounts are skipped (e.g. `fee` when `fee_bps == 0`).
fn transfer<'info>(
    token_program: &Interface<'info, TokenInterface>,
    from: &InterfaceAccount<'info, TokenAccount>,
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
            token_program.key(),
            TransferChecked {
                from: from.to_account_info(),
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
