//! Freeze authority policy (spec §3.8): asset mints must have none; currency
//! mints may (accepted, curated risk).

mod common;

use anchor_lang::prelude::Pubkey;
use anchor_spl::token_2022::spl_token_2022::extension::ExtensionType;
use common::*;
use niara_exchange::{error::ExchangeError, AssetClass, Listing, ListingStatus};
use solana_signer::Signer;

const TOKEN_PROGRAMS: [Pubkey; 2] = [anchor_spl::token::ID, anchor_spl::token_2022::ID];

#[test]
fn submit_listing_rejects_asset_mint_with_freeze_authority() {
    for token_program in TOKEN_PROGRAMS {
        for asset_class in [AssetClass::Equity, AssetClass::FixedIncome, AssetClass::Credit] {
            let mut env = Env::initialized();
            let issuer = env.funded_keypair();
            let mint = env.create_mint(token_program, Some(issuer.pubkey()));
            env.set_freeze_authority(&mint, Some(issuer.pubkey()));
            let res = env.submit_listing(&issuer, mint, asset_class, DOC_HASH);
            assert_custom_err(res, ExchangeError::FreezeAuthorityNotAllowed);
            assert!(!env.exists(&listing_pda(&mint)));
        }
    }
}

/// A freeze authority held by someone other than the issuer is rejected too.
#[test]
fn submit_listing_rejects_third_party_freeze_authority() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let mint = env.create_mint_2022(
        Some(issuer.pubkey()),
        &[ExtensionType::MetadataPointer, ExtensionType::TokenMetadata],
    );
    env.set_freeze_authority(&mint, Some(Pubkey::new_unique()));
    let res = env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH);
    assert_custom_err(res, ExchangeError::FreezeAuthorityNotAllowed);
}

#[test]
fn list_currency_accepts_mint_with_freeze_authority() {
    for token_program in TOKEN_PROGRAMS {
        let mut env = Env::initialized();
        let authority = env.listing_authority.insecure_clone();
        let mint = env.create_mint(token_program, Some(Pubkey::new_unique()));
        env.set_freeze_authority(&mint, Some(Pubkey::new_unique()));
        env.list_currency(&authority, mint).unwrap();
        let listing: Listing = env.fetch(&listing_pda(&mint));
        assert_eq!(listing.status, ListingStatus::Approved);
        assert_eq!(listing.asset_class, AssetClass::Currency);
    }
}

/// The listing-time check is enough only if a freeze authority cannot be added
/// later. Checked against the real token programs.
#[test]
fn freeze_authority_cannot_be_added_after_initialization() {
    use anchor_spl::token::spl_token::instruction as spl;
    use anchor_spl::token_2022::spl_token_2022::instruction as t22;

    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let new_freeze = Pubkey::new_unique();

    let legacy = env.create_mint(anchor_spl::token::ID, Some(issuer.pubkey()));
    let ix = spl::set_authority(
        &anchor_spl::token::ID,
        &legacy,
        Some(&new_freeze),
        spl::AuthorityType::FreezeAccount,
        &issuer.pubkey(),
        &[],
    )
    .unwrap();
    assert!(env.send(&[ix], &[&issuer]).is_err(), "SPL Token added a freeze authority");

    let mint_2022 = env.create_mint_2022(Some(issuer.pubkey()), &[]);
    let ix = t22::set_authority(
        &anchor_spl::token_2022::ID,
        &mint_2022,
        Some(&new_freeze),
        t22::AuthorityType::FreezeAccount,
        &issuer.pubkey(),
        &[],
    )
    .unwrap();
    assert!(env.send(&[ix], &[&issuer]).is_err(), "Token-2022 added a freeze authority");

    // Both mints remain listable.
    env.submit_listing(&issuer, legacy, AssetClass::Equity, DOC_HASH)
        .unwrap();
    env.submit_listing(&issuer, mint_2022, AssetClass::Equity, DOC_HASH)
        .unwrap();
}
