//! Per-listing cashback freeze (spec §9.5): `freeze_cashback` and
//! `unfreeze_cashback` by the listing authority, with no timelock.
//! `withdraw_cashback` reverts while the listing is frozen; cashback keeps
//! accruing.

mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use niara_exchange::{
    error::ExchangeError, events::CashbackFreezeSet, AssetClass, CashbackLedger, DealSide,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

const ASSETS: u64 = 10;
const PAYMENT: u64 = 25_000_000;

/// A market with cashback accrued by one settlement, and an empty account of
/// the payment mint owned by the issuer.
fn accrued() -> (Env, Market, Pubkey) {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
    let dest = env.create_token_account(SPL_TOKEN, m.payment_mint, m.issuer.pubkey(), 0);
    (env, m, dest)
}

fn available(env: &Env, m: &Market) -> u64 {
    env.cashback(m).available().unwrap()
}

fn withdraw(env: &mut Env, m: &Market, dest: Pubkey, amount: u64) -> litesvm::types::TransactionResult {
    let issuer = m.issuer.insecure_clone();
    env.withdraw_cashback(&issuer, m.asset_mint, m.payment_mint, dest, amount)
}

fn authority(env: &Env) -> Keypair {
    env.listing_authority.insecure_clone()
}

#[test]
fn listings_start_unfrozen() {
    let mut env = Env::initialized();
    let (equity, _) = env.listed_equity(SPL_TOKEN);
    let (bond, _) = env.listed_issuer_asset(TOKEN_2022, AssetClass::FixedIncome);
    let usd = env.listed_currency(SPL_TOKEN);
    for mint in [equity, bond, usd] {
        assert!(!env.listing(&mint).cashback_frozen);
    }
}

#[test]
fn freeze_blocks_withdrawal_until_unfrozen() {
    let (mut env, m, dest) = accrued();
    let authority = authority(&env);
    let amount = available(&env, &m);
    assert!(amount > 0);

    let meta = env.freeze_cashback(&authority, m.asset_mint).unwrap();
    assert!(env.listing(&m.asset_mint).cashback_frozen);
    let e = &events::<CashbackFreezeSet>(&meta)[0];
    assert_eq!(
        (e.listing, e.asset_mint, e.frozen, e.listing_authority),
        (listing_pda(&m.asset_mint), m.asset_mint, true, authority.pubkey())
    );

    for attempt in [amount, 1] {
        let res = withdraw(&mut env, &m, dest, attempt);
        assert_custom_err(res, ExchangeError::CashbackFrozen);
    }
    assert_eq!(env.balance(&dest), 0);
    assert_eq!(env.cashback(&m).withdrawn, 0);
    assert_eq!(env.fee_ledger(&m.payment_mint).cashback_withdrawn, 0);

    let meta = env.unfreeze_cashback(&authority, m.asset_mint).unwrap();
    assert!(!env.listing(&m.asset_mint).cashback_frozen);
    let e = &events::<CashbackFreezeSet>(&meta)[0];
    assert_eq!((e.frozen, e.listing_authority), (false, authority.pubkey()));

    withdraw(&mut env, &m, dest, amount).unwrap();
    assert_eq!(env.balance(&dest), amount);
    env.assert_vault_matches_ledger(&m.payment_mint);
}

/// Nothing is lost while frozen: `settle` and `accept_deal` keep accruing,
/// and everything is withdrawable after unfreezing.
#[test]
fn cashback_keeps_accruing_while_frozen() {
    let (mut env, m, dest) = accrued();
    let authority = authority(&env);
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    let before = env.cashback(&m).accrued;
    env.settle(&m, order_id(2), ASSETS, PAYMENT).unwrap();
    let accrued = env.cashback(&m).accrued;
    assert_eq!(accrued, 2 * before);

    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    env.freeze_cashback(&authority, p.asset_mint).unwrap();
    env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    env.accept_deal(&p, deal_id(1)).unwrap();
    let ledger: CashbackLedger = env.fetch(&cashback_pda(&p.asset_mint, &p.payment_mint));
    assert_eq!(ledger.accrued, before, "same fee and cashback as one settlement");

    env.unfreeze_cashback(&authority, m.asset_mint).unwrap();
    withdraw(&mut env, &m, dest, accrued).unwrap();
    assert_eq!(env.balance(&dest), accrued);
}

#[test]
fn freeze_is_per_listing() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let (other_mint, other_issuer) = env.listed_equity(SPL_TOKEN);
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, other_mint, m.payment_mint)
        .unwrap();
    let other = env.trader_pair(other_mint, m.payment_mint, other_issuer, 1_000, 1_000_000_000);
    env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
    env.settle(&other, order_id(2), ASSETS, PAYMENT).unwrap();
    let dest = env.create_token_account(SPL_TOKEN, m.payment_mint, m.issuer.pubkey(), 0);
    let other_dest =
        env.create_token_account(SPL_TOKEN, m.payment_mint, other.issuer.pubkey(), 0);

    let authority = authority(&env);
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    let res = withdraw(&mut env, &m, dest, 1);
    assert_custom_err(res, ExchangeError::CashbackFrozen);
    let amount = available(&env, &other);
    withdraw(&mut env, &other, other_dest, amount).unwrap();
    assert!(!env.listing(&other_mint).cashback_frozen);
}

#[test]
fn only_the_listing_authority_can_freeze_or_unfreeze() {
    let (mut env, m, _) = accrued();
    let intruders = [
        m.issuer.insecure_clone(),
        env.admin.insecure_clone(),
        env.treasury.insecure_clone(),
        env.operator.insecure_clone(),
        env.funded_keypair(),
    ];
    for intruder in &intruders {
        let res = env.freeze_cashback(intruder, m.asset_mint);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert!(!env.listing(&m.asset_mint).cashback_frozen);
    let authority = authority(&env);
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    for intruder in &intruders {
        let res = env.unfreeze_cashback(intruder, m.asset_mint);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert!(env.listing(&m.asset_mint).cashback_frozen);
}

#[test]
fn freeze_and_unfreeze_must_change_the_flag() {
    let (mut env, m, _) = accrued();
    let authority = authority(&env);
    let res = env.unfreeze_cashback(&authority, m.asset_mint);
    assert_custom_err(res, ExchangeError::CashbackNotFrozen);
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    let res = env.freeze_cashback(&authority, m.asset_mint);
    assert_custom_err(res, ExchangeError::CashbackAlreadyFrozen);
    env.unfreeze_cashback(&authority, m.asset_mint).unwrap();
    let res = env.unfreeze_cashback(&authority, m.asset_mint);
    assert_custom_err(res, ExchangeError::CashbackNotFrozen);
}

/// `Currency` listings never accrue cashback, so there is nothing to freeze.
#[test]
fn currency_listings_cannot_be_frozen() {
    let mut env = Env::initialized();
    let usd = env.listed_currency(SPL_TOKEN);
    let authority = authority(&env);
    let res = env.freeze_cashback(&authority, usd);
    assert_custom_err(res, ExchangeError::NotCashbackEligible);
    assert!(!env.listing(&usd).cashback_frozen);
}

/// Containment must work in any state: while paused, and on a listing that is
/// pending, suspended or delisted (cashback stays withdrawable after delisting,
/// §3.3, so it must stay freezable too).
#[test]
fn freeze_works_while_paused_and_on_any_listing_status() {
    let (mut env, m, dest) = accrued();
    let authority = authority(&env);
    let admin = env.admin.insecure_clone();

    env.pause(&admin).unwrap();
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    env.unfreeze_cashback(&authority, m.asset_mint).unwrap();
    env.unpause(&admin).unwrap();

    let issuer = env.funded_keypair();
    let pending = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
    env.submit_listing(&issuer, pending, AssetClass::Credit, DOC_HASH)
        .unwrap();
    env.freeze_cashback(&authority, pending).unwrap();

    env.suspend(&authority, m.asset_mint).unwrap();
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    env.delist(&authority, m.asset_mint).unwrap();
    let res = withdraw(&mut env, &m, dest, 1);
    assert_custom_err(res, ExchangeError::CashbackFrozen);
    env.unfreeze_cashback(&authority, m.asset_mint).unwrap();
    let amount = available(&env, &m);
    withdraw(&mut env, &m, dest, amount).unwrap();
}

/// Pause and freeze are separate switches: lifting one leaves the other.
#[test]
fn pause_and_freeze_are_independent() {
    let (mut env, m, dest) = accrued();
    let authority = authority(&env);
    let admin = env.admin.insecure_clone();
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    env.pause(&admin).unwrap();
    let res = withdraw(&mut env, &m, dest, 1);
    assert_custom_err(res, ExchangeError::Paused);
    env.unpause(&admin).unwrap();
    let res = withdraw(&mut env, &m, dest, 1);
    assert_custom_err(res, ExchangeError::CashbackFrozen);
    env.pause(&admin).unwrap();
    env.unfreeze_cashback(&authority, m.asset_mint).unwrap();
    let res = withdraw(&mut env, &m, dest, 1);
    assert_custom_err(res, ExchangeError::Paused);
    env.unpause(&admin).unwrap();
    withdraw(&mut env, &m, dest, 1).unwrap();
}

/// The treasury's protocol share is not affected by an issuer's freeze.
#[test]
fn freeze_does_not_block_protocol_fees() {
    let (mut env, m, _) = accrued();
    let authority = authority(&env);
    env.freeze_cashback(&authority, m.asset_mint).unwrap();
    let treasury = env.treasury.insecure_clone();
    let dest = env.create_token_account(SPL_TOKEN, m.payment_mint, treasury.pubkey(), 0);
    let amount = env.fee_ledger(&m.payment_mint).protocol_available().unwrap();
    env.withdraw_protocol_fees(&treasury, m.payment_mint, dest, amount)
        .unwrap();
    env.assert_vault_matches_ledger(&m.payment_mint);
}
