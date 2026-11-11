//! `init_cashback_ledger`, plus the listing side of the pause principle.

mod common;

use anchor_lang::error::ErrorCode as AnchorError;
use common::*;
use niara_exchange::{error::ExchangeError, AssetClass, CashbackLedger};
use solana_signer::Signer;

#[test]
fn anyone_can_create_a_ledger_for_an_eligible_listing() {
    let mut env = Env::initialized();
    let (asset, _) = env.listed_equity(SPL_TOKEN);
    let usdt = env.fee_currency(TOKEN_2022);
    let stranger = env.funded_keypair();
    env.init_cashback_ledger(&stranger, asset, usdt).unwrap();
    let ledger: CashbackLedger = env.fetch(&cashback_pda(&asset, &usdt));
    assert_eq!(ledger.listing, listing_pda(&asset));
    assert_eq!(ledger.payment_mint, usdt);
    assert_eq!((ledger.accrued, ledger.withdrawn), (0, 0));
}

/// Eligibility is by class, not status: the ledger can be prepared while the
/// listing is pending, and P2P classes accrue cashback too (spec §1).
#[test]
fn ledger_for_pending_and_p2p_listings() {
    let mut env = Env::initialized();
    let usdt = env.fee_currency(SPL_TOKEN);
    let payer = env.admin.insecure_clone();
    for class in [AssetClass::Equity, AssetClass::FixedIncome, AssetClass::Credit] {
        let issuer = env.funded_keypair();
        let mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
        env.submit_listing(&issuer, mint, class, DOC_HASH).unwrap();
        env.init_cashback_ledger(&payer, mint, usdt).unwrap();
        assert!(env.exists(&cashback_pda(&mint, &usdt)));
    }
}

#[test]
fn cannot_be_created_twice() {
    let mut env = Env::initialized();
    let (asset, _) = env.listed_equity(SPL_TOKEN);
    let usdt = env.fee_currency(SPL_TOKEN);
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, asset, usdt).unwrap();
    assert!(env.init_cashback_ledger(&payer, asset, usdt).is_err());
}

/// Invariant 9: a Currency listing never gets a cashback ledger.
#[test]
fn rejects_currency_listing() {
    let mut env = Env::initialized();
    let usdt = env.fee_currency(SPL_TOKEN);
    let brl = env.listed_currency(TOKEN_2022);
    let payer = env.admin.insecure_clone();
    for listing in [usdt, brl] {
        let res = env.init_cashback_ledger(&payer, listing, usdt);
        assert_custom_err(res, ExchangeError::NotCashbackEligible);
        assert!(!env.exists(&cashback_pda(&listing, &usdt)));
    }
}

/// Cashback is always in USDT: the ledger is keyed by `Config.fee_mint`, so
/// no ledger can be created for another currency or an equity mint. The fee
/// mint's listing status does not matter (status gates trading only).
#[test]
fn ledger_is_keyed_by_the_fee_mint() {
    let mut env = Env::initialized();
    let (asset, _) = env.listed_equity(SPL_TOKEN);
    let (equity, _) = env.listed_equity(SPL_TOKEN);
    let brl = env.listed_currency(SPL_TOKEN);
    let payer = env.admin.insecure_clone();
    for other in [brl, equity] {
        let res = env.init_cashback_ledger(&payer, asset, other);
        assert_custom_err(res, AnchorError::ConstraintSeeds);
        assert!(!env.exists(&cashback_pda(&asset, &other)));
    }

    let usdt = env.fee_currency(SPL_TOKEN);
    let authority = env.listing_authority.insecure_clone();
    env.suspend(&authority, usdt).unwrap();
    env.init_cashback_ledger(&payer, asset, usdt).unwrap();
}

#[test]
fn rejects_unlisted_mints() {
    let mut env = Env::initialized();
    let (asset, _) = env.listed_equity(SPL_TOKEN);
    let unlisted = env.create_mint(SPL_TOKEN, None);
    let payer = env.admin.insecure_clone();
    assert!(env.init_cashback_ledger(&payer, asset, unlisted).is_err());
    let usdt = env.fee_currency(SPL_TOKEN);
    assert!(env.init_cashback_ledger(&payer, unlisted, usdt).is_err());
}

/// Pause principle (spec §3.3): listing actions and ledger setup stay
/// available while paused; only payouts and trading stop.
#[test]
fn listing_actions_and_ledger_setup_work_while_paused() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    env.pause(&admin).unwrap();

    let (asset, _) = env.listed_equity(SPL_TOKEN); // submit + approve
    let usdt = env.fee_currency(SPL_TOKEN); // list_currency + init_fee_vault
    env.listed_currency(SPL_TOKEN); // list_currency
    env.init_cashback_ledger(&admin, asset, usdt).unwrap();
    let authority = env.listing_authority.insecure_clone();
    env.suspend(&authority, asset).unwrap();
    env.approve(&authority, asset).unwrap();
    env.delist(&authority, asset).unwrap();
}
