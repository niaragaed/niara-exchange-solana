mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use niara_exchange::{
    error::ExchangeError,
    events::{ListingApproved, ListingSubmitted},
    AssetClass, ConfigChange, Listing, ListingStatus,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

fn spl_token() -> Pubkey {
    anchor_spl::token::ID
}

fn token_2022() -> Pubkey {
    anchor_spl::token_2022::ID
}

/// Env with config initialized, plus an issuer that owns a fresh SPL Token mint.
fn with_issuer() -> (Env, Keypair, Pubkey) {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let mint = env.create_mint(spl_token(), Some(issuer.pubkey()));
    (env, issuer, mint)
}

// ---- submit_listing ------------------------------------------------------------

#[test]
fn submit_listing_for_each_issuer_class() {
    for class in [AssetClass::Equity, AssetClass::FixedIncome, AssetClass::Credit] {
        let (mut env, issuer, mint) = with_issuer();
        let meta = env.submit_listing(&issuer, mint, class, DOC_HASH).unwrap();

        let listing: Listing = env.fetch(&listing_pda(&mint));
        assert_eq!(listing.asset_mint, mint);
        assert_eq!(listing.issuer, issuer.pubkey());
        assert_eq!(listing.asset_class, class);
        assert_eq!(listing.status, ListingStatus::Pending);
        assert!(listing.cashback_eligible);
        assert_eq!(listing.doc_hash, DOC_HASH);
        assert_eq!(listing.approved_at, 0);

        let evs = events::<ListingSubmitted>(&meta);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].asset_class, class);
        assert_eq!(evs[0].issuer, issuer.pubkey());
    }
}

#[test]
fn submit_listing_accepts_token_2022_mint() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let mint = env.create_mint(token_2022(), Some(issuer.pubkey()));
    env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH)
        .unwrap();
    assert_eq!(env.fetch::<Listing>(&listing_pda(&mint)).asset_mint, mint);
}

#[test]
fn submit_listing_rejects_non_token_mint() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    // Valid mint layout, but owned by an unrelated program.
    let fake = env.create_mint(Pubkey::new_unique(), Some(issuer.pubkey()));
    assert!(env
        .submit_listing(&issuer, fake, AssetClass::Equity, DOC_HASH)
        .is_err());
}

#[test]
fn submit_listing_requires_mint_authority() {
    let (mut env, _issuer, mint) = with_issuer();
    let impostor = env.funded_keypair();
    assert_custom_err(
        env.submit_listing(&impostor, mint, AssetClass::Equity, DOC_HASH),
        ExchangeError::NotMintAuthority,
    );

    // A mint with no authority (fixed supply) cannot be submitted by anyone.
    let fixed = env.create_mint(spl_token(), None);
    assert_custom_err(
        env.submit_listing(&impostor, fixed, AssetClass::Equity, DOC_HASH),
        ExchangeError::NotMintAuthority,
    );
}

#[test]
fn submit_listing_rejects_currency_class() {
    let (mut env, issuer, mint) = with_issuer();
    assert_custom_err(
        env.submit_listing(&issuer, mint, AssetClass::Currency, DOC_HASH),
        ExchangeError::CurrencyNotSubmittable,
    );
}

#[test]
fn submit_listing_rejects_empty_doc_hash() {
    let (mut env, issuer, mint) = with_issuer();
    assert_custom_err(
        env.submit_listing(&issuer, mint, AssetClass::Equity, [0u8; 32]),
        ExchangeError::EmptyDocHash,
    );
}

#[test]
fn submit_listing_only_once_per_mint() {
    let (mut env, issuer, mint) = with_issuer();
    env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH)
        .unwrap();
    assert!(env
        .submit_listing(&issuer, mint, AssetClass::Credit, DOC_HASH)
        .is_err());
}

// ---- list_currency -------------------------------------------------------------

#[test]
fn list_currency_creates_approved_listing_without_cashback() {
    let mut env = Env::initialized();
    let authority = env.listing_authority.insecure_clone();
    // Stablecoin minted by a third party (e.g. a Circle-like issuer).
    let stablecoin = env.create_mint(spl_token(), Some(Pubkey::new_unique()));

    let now = env.now();
    let meta = env.list_currency(&authority, stablecoin).unwrap();

    let listing: Listing = env.fetch(&listing_pda(&stablecoin));
    assert_eq!(listing.asset_class, AssetClass::Currency);
    assert_eq!(listing.status, ListingStatus::Approved);
    assert!(!listing.cashback_eligible);
    assert_eq!(listing.issuer, Pubkey::default());
    assert_eq!(listing.approved_at, now);

    let evs = events::<ListingApproved>(&meta);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].asset_class, AssetClass::Currency);
    assert!(!evs[0].cashback_eligible);
}

#[test]
fn list_currency_accepts_token_2022_mint() {
    let mut env = Env::initialized();
    let authority = env.listing_authority.insecure_clone();
    let stablecoin = env.create_mint(token_2022(), None);
    env.list_currency(&authority, stablecoin).unwrap();
}

#[test]
fn list_currency_requires_listing_authority() {
    let mut env = Env::initialized();
    let other = env.funded_keypair();
    let admin = env.admin.insecure_clone();
    let stablecoin = env.create_mint(spl_token(), None);
    assert_custom_err(
        env.list_currency(&other, stablecoin),
        ExchangeError::Unauthorized,
    );
    // The admin is not the listing authority either.
    assert_custom_err(
        env.list_currency(&admin, stablecoin),
        ExchangeError::Unauthorized,
    );
}

#[test]
fn list_currency_cannot_overwrite_existing_listing() {
    let (mut env, issuer, mint) = with_issuer();
    let authority = env.listing_authority.insecure_clone();
    env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH)
        .unwrap();
    assert!(env.list_currency(&authority, mint).is_err());
    assert_eq!(
        env.fetch::<Listing>(&listing_pda(&mint)).asset_class,
        AssetClass::Equity
    );
}

// ---- approve / suspend / delist ------------------------------------------------

#[test]
fn approve_listing_by_listing_authority() {
    let (mut env, issuer, mint) = with_issuer();
    let authority = env.listing_authority.insecure_clone();
    env.submit_listing(&issuer, mint, AssetClass::FixedIncome, DOC_HASH)
        .unwrap();

    env.warp_secs(100);
    let now = env.now();
    let meta = env.approve(&authority, mint).unwrap();

    let listing: Listing = env.fetch(&listing_pda(&mint));
    assert_eq!(listing.status, ListingStatus::Approved);
    assert_eq!(listing.approved_at, now);

    let evs = events::<ListingApproved>(&meta);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].listing, listing_pda(&mint));
    assert_eq!(evs[0].issuer, issuer.pubkey());
    assert_eq!(evs[0].doc_hash, DOC_HASH);
    assert_eq!(evs[0].approved_at, now);
}

#[test]
fn status_changes_require_listing_authority() {
    let (mut env, issuer, mint) = with_issuer();
    let admin = env.admin.insecure_clone();
    env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH)
        .unwrap();

    for signer in [&issuer, &admin] {
        assert_custom_err(env.approve(signer, mint), ExchangeError::Unauthorized);
        assert_custom_err(env.suspend(signer, mint), ExchangeError::Unauthorized);
        assert_custom_err(env.delist(signer, mint), ExchangeError::Unauthorized);
    }
}

#[test]
fn status_lifecycle() {
    let (mut env, issuer, mint) = with_issuer();
    let a = env.listing_authority.insecure_clone();
    env.submit_listing(&issuer, mint, AssetClass::Credit, DOC_HASH)
        .unwrap();
    let status = |env: &Env| env.fetch::<Listing>(&listing_pda(&mint)).status;

    // Pending cannot be suspended.
    assert_custom_err(env.suspend(&a, mint), ExchangeError::InvalidStatusTransition);

    env.approve(&a, mint).unwrap();
    assert_eq!(status(&env), ListingStatus::Approved);
    assert_custom_err(env.approve(&a, mint), ExchangeError::InvalidStatusTransition);

    env.suspend(&a, mint).unwrap();
    assert_eq!(status(&env), ListingStatus::Suspended);
    assert_custom_err(env.suspend(&a, mint), ExchangeError::InvalidStatusTransition);

    // Suspended -> Approved (reinstated).
    env.approve(&a, mint).unwrap();
    assert_eq!(status(&env), ListingStatus::Approved);

    env.delist(&a, mint).unwrap();
    assert_eq!(status(&env), ListingStatus::Delisted);

    // Delisted is terminal.
    assert_custom_err(env.approve(&a, mint), ExchangeError::InvalidStatusTransition);
    assert_custom_err(env.suspend(&a, mint), ExchangeError::InvalidStatusTransition);
    assert_custom_err(env.delist(&a, mint), ExchangeError::InvalidStatusTransition);
}

#[test]
fn pending_listing_can_be_rejected_by_delisting() {
    let (mut env, issuer, mint) = with_issuer();
    let a = env.listing_authority.insecure_clone();
    env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH)
        .unwrap();
    env.delist(&a, mint).unwrap();
    assert_eq!(
        env.fetch::<Listing>(&listing_pda(&mint)).status,
        ListingStatus::Delisted
    );
}

#[test]
fn currency_listing_can_be_suspended_and_delisted() {
    let mut env = Env::initialized();
    let a = env.listing_authority.insecure_clone();
    let stablecoin = env.create_mint(spl_token(), None);
    env.list_currency(&a, stablecoin).unwrap();
    env.suspend(&a, stablecoin).unwrap();
    env.approve(&a, stablecoin).unwrap();
    env.delist(&a, stablecoin).unwrap();
}

#[test]
fn rotated_listing_authority_takes_over() {
    let (mut env, issuer, mint) = with_issuer();
    let admin = env.admin.insecure_clone();
    let old = env.listing_authority.insecure_clone();
    let new = env.funded_keypair();
    env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH)
        .unwrap();

    let change = ConfigChange::ListingAuthority(new.pubkey());
    env.propose(&admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&admin, &change).unwrap();

    assert_custom_err(env.approve(&old, mint), ExchangeError::Unauthorized);
    env.approve(&new, mint).unwrap();
}
