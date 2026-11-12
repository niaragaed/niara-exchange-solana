//! `settle` (spec §3.4): order-book settlement through SPL delegates.

mod common;

use anchor_lang::{error::ErrorCode as AnchorError, prelude::Pubkey};
use anchor_spl::token::spl_token::state::AccountState;
use anchor_spl::token_2022::spl_token_2022::extension::ExtensionType;
use common::*;
use niara_exchange::{
    error::ExchangeError, events::Settled, AssetClass, ConfigChange, SettlementRecord,
    BPS_DENOMINATOR, MAX_FEE_BPS,
};
use solana_signer::Signer;

const ASSETS: u64 = 10;
const PAYMENT: u64 = 25_000_000; // 25.000000 in a 6-decimal currency

fn expected_fee(payment: u64, fee_bps: u16) -> u64 {
    (payment * fee_bps as u64).div_ceil(BPS_DENOMINATOR as u64)
}

fn expected_cashback(fee: u64) -> u64 {
    fee * CASHBACK_BPS as u64 / BPS_DENOMINATOR as u64
}

/// Balances of every account `settle` touches.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Snapshot {
    seller_asset: u64,
    buyer_asset: u64,
    buyer_payment: u64,
    seller_payment: u64,
    fee_vault: u64,
    cashback_accrued: u64,
}

fn snapshot(env: &Env, m: &Market) -> Snapshot {
    Snapshot {
        seller_asset: env.balance(&m.seller_asset),
        buyer_asset: env.balance(&m.buyer_asset),
        buyer_payment: env.balance(&m.buyer_payment),
        seller_payment: env.balance(&m.seller_payment),
        fee_vault: env.balance(&fee_vault_pda(&m.payment_mint)),
        cashback_accrued: env.cashback(m).accrued,
    }
}

/// Asserts a failed settle moved nothing and recorded nothing.
fn assert_untouched(env: &Env, m: &Market, before: Snapshot, id: [u8; 16]) {
    assert_eq!(snapshot(env, m), before, "a failed settle must not move funds");
    assert!(!env.exists(&settled_pda(&id)), "no settlement record");
}

fn assert_settles(asset_program: Pubkey, payment_program: Pubkey) {
    let mut env = Env::initialized();
    let m = env.market(asset_program, payment_program);
    let before = snapshot(&env, &m);

    let meta = env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();

    let fee = expected_fee(PAYMENT, FEE_BPS);
    let cashback = expected_cashback(fee);
    let after = snapshot(&env, &m);
    assert_eq!(after.seller_asset, before.seller_asset - ASSETS);
    assert_eq!(after.buyer_asset, before.buyer_asset + ASSETS);
    assert_eq!(after.buyer_payment, before.buyer_payment - PAYMENT);
    assert_eq!(after.seller_payment, before.seller_payment + PAYMENT - fee);
    assert_eq!(after.fee_vault, before.fee_vault + fee);
    assert_eq!(after.cashback_accrued, cashback);
    let ledger = env.fee_ledger(&m.payment_mint);
    assert_eq!((ledger.fees_collected, ledger.cashback_accrued), (fee, cashback));
    env.assert_vault_matches_ledger(&m.payment_mint);

    // Delegations are consumed, not reset.
    assert_eq!(env.token_account(&m.seller_asset).delegated_amount, 1_000 - ASSETS);
    assert_eq!(
        env.token_account(&m.buyer_payment).delegated_amount,
        1_000_000_000 - PAYMENT
    );

    let record: SettlementRecord = env.fetch(&settled_pda(&order_id(1)));
    let _ = record.bump;

    let events = events::<Settled>(&meta);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.order_id, order_id(1));
    assert_eq!(e.asset_mint, m.asset_mint);
    assert_eq!(e.payment_mint, m.payment_mint);
    assert_eq!(e.buyer, m.buyer.pubkey());
    assert_eq!(e.seller, m.seller.pubkey());
    assert_eq!(e.asset_amount, ASSETS);
    assert_eq!(e.payment_amount, PAYMENT);
    assert_eq!(e.fee, fee);
    assert_eq!(e.cashback, cashback);
}

#[test]
fn settles_spl_token_asset_and_payment() {
    assert_settles(SPL_TOKEN, SPL_TOKEN);
}

#[test]
fn settles_token_2022_asset_and_payment() {
    assert_settles(TOKEN_2022, TOKEN_2022);
}

#[test]
fn settles_mixed_token_programs() {
    assert_settles(SPL_TOKEN, TOKEN_2022);
    assert_settles(TOKEN_2022, SPL_TOKEN);
}

/// Allowlisted metadata extensions do not get in the way of `transfer_checked`.
#[test]
fn settles_token_2022_metadata_mints() {
    let exts = [ExtensionType::MetadataPointer, ExtensionType::TokenMetadata];
    let mut env = Env::initialized_with_fee_mint(|env| env.create_mint_2022(None, &exts));
    let issuer = env.funded_keypair();
    let asset_mint = env.create_mint_2022(Some(issuer.pubkey()), &exts);
    env.submit_listing(&issuer, asset_mint, AssetClass::Equity, DOC_HASH)
        .unwrap();
    let authority = env.listing_authority.insecure_clone();
    env.approve(&authority, asset_mint).unwrap();
    let payment_mint = env.fee_currency(TOKEN_2022);
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, asset_mint, payment_mint)
        .unwrap();
    let m = env.trader_pair(asset_mint, payment_mint, issuer, 100, 1_000_000);
    env.settle(&m, order_id(1), 1, 1_000_000).unwrap();
    assert_eq!(env.balance(&m.buyer_asset), 1);
}

/// `payment_amount` is raw units of the payment mint; each leg uses its own
/// mint's decimals (asset 0, payment 6 in the fixture; here also 9 and 2).
#[test]
fn transfer_checked_uses_each_mints_own_decimals() {
    for (asset_decimals, payment_decimals) in [(9u8, 2u8), (2, 9)] {
        let mut env = Env::initialized();
        let m = env.market(SPL_TOKEN, TOKEN_2022);
        env.set_mint_decimals(&m.asset_mint, asset_decimals);
        env.set_mint_decimals(&m.payment_mint, payment_decimals);
        env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
        assert_eq!(env.balance(&m.buyer_asset), ASSETS);
        assert_eq!(
            env.balance(&m.seller_payment),
            PAYMENT - expected_fee(PAYMENT, FEE_BPS)
        );
    }
}

#[test]
fn fee_rounds_up_and_cashback_rounds_down() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    // 30 bps of 10_001 = 30.003 -> fee 31; 20% of 31 = 6.2 -> cashback 6.
    env.settle(&m, order_id(1), 1, 10_001).unwrap();
    assert_eq!(env.balance(&fee_vault_pda(&m.payment_mint)), 31);
    assert_eq!(env.balance(&m.seller_payment), 9_970);
    assert_eq!(env.cashback(&m).accrued, 6);
    // 30 bps of 2 -> fee 1, seller gets 1.
    env.settle(&m, order_id(2), 1, 2).unwrap();
    assert_eq!(env.balance(&fee_vault_pda(&m.payment_mint)), 32);
    assert_eq!(env.balance(&m.seller_payment), 9_971);
}

/// 30 bps of 1 rounds up to a fee of 1, which would leave the seller nothing
/// for the asset: `settle` refuses it.
#[test]
fn rejects_fill_that_leaves_the_seller_nothing() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let before = snapshot(&env, &m);
    let res = env.settle(&m, order_id(1), 1, 1);
    assert_custom_err(res, ExchangeError::ZeroSellerProceeds);
    assert_untouched(&env, &m, before, order_id(1));

    // At the maximum fee (50 bps) the same holds: the fee is ceil(0.5%) = 1.
    let admin = env.admin.insecure_clone();
    let change = ConfigChange::FeeBps(MAX_FEE_BPS);
    env.propose(&admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&admin, &change).unwrap();
    let res = env.settle(&m, order_id(2), 1, 1);
    assert_custom_err(res, ExchangeError::ZeroSellerProceeds);
    env.settle(&m, order_id(3), 1, 2).unwrap();
    assert_eq!(env.balance(&m.seller_payment), 1);

    // With a zero fee a 1-unit fill is fine: the seller receives it all.
    let change = ConfigChange::FeeBps(0);
    env.propose(&admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&admin, &change).unwrap();
    env.settle(&m, order_id(4), 1, 1).unwrap();
    assert_eq!(env.balance(&m.seller_payment), 2);
}

#[test]
fn zero_fee_after_timelocked_change() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let change = ConfigChange::FeeBps(0);
    env.propose(&admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&admin, &change).unwrap();

    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let meta = env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
    assert_eq!(env.balance(&m.seller_payment), PAYMENT);
    assert_eq!(env.balance(&fee_vault_pda(&m.payment_mint)), 0);
    let e = &events::<Settled>(&meta)[0];
    assert_eq!((e.fee, e.cashback), (0, 0));
}

/// Several matches (partial fills) draw on one aggregate delegation; the
/// operator cannot go past it.
#[test]
fn partial_fills_draw_on_one_delegation() {
    let mut env = Env::initialized();
    let (asset_mint, issuer) = env.listed_equity(SPL_TOKEN);
    let payment_mint = env.fee_currency(SPL_TOKEN);
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, asset_mint, payment_mint)
        .unwrap();
    let m = env.trader_pair(asset_mint, payment_mint, issuer, 10, 1_000_000);

    env.settle(&m, order_id(1), 4, 400_000).unwrap();
    env.settle(&m, order_id(2), 6, 600_000).unwrap();
    assert_eq!(env.balance(&m.buyer_asset), 10);
    // Exhausted: the token program clears a delegate whose amount reaches 0.
    assert_eq!(env.token_account(&m.seller_asset).delegate, None.into());
    let before = snapshot(&env, &m);
    let res = env.settle(&m, order_id(3), 1, 1);
    assert_custom_err(res, ExchangeError::DelegateNotSet);
    assert_untouched(&env, &m, before, order_id(3));
}

// ---- replay protection ------------------------------------------------------

#[test]
fn same_order_id_cannot_settle_twice() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    env.settle(&m, order_id(7), ASSETS, PAYMENT).unwrap();
    let before = snapshot(&env, &m);
    // Same id, still within the delegated amounts.
    let res = env.settle(&m, order_id(7), ASSETS, PAYMENT);
    assert!(res.is_err(), "replayed order_id settled again");
    assert_eq!(snapshot(&env, &m), before);
    // The id is global: another market cannot reuse it either.
    let other = env.market(SPL_TOKEN, SPL_TOKEN);
    assert!(env.settle(&other, order_id(7), ASSETS, PAYMENT).is_err());
    // A fresh id goes through.
    env.settle(&m, order_id(8), ASSETS, PAYMENT).unwrap();
}

/// An all-zero match id is an unset field, never a real UUID: `settle` refuses
/// it and records nothing for it.
#[test]
fn rejects_zero_match_id() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let zero = [0u8; 16];
    assert_eq!(order_id(0), zero);
    let before = snapshot(&env, &m);
    let res = env.settle(&m, zero, ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::ZeroMatchId);
    assert_untouched(&env, &m, before, zero);

    // The pause is checked first, as for every other rejection.
    let admin = env.admin.insecure_clone();
    env.pause(&admin).unwrap();
    assert_custom_err(env.settle(&m, zero, ASSETS, PAYMENT), ExchangeError::Paused);
    env.unpause(&admin).unwrap();
    assert_untouched(&env, &m, before, zero);

    // Only the all-zero id is refused: one set bit anywhere is a valid id.
    let mut low = zero;
    low[0] = 1;
    let mut high = zero;
    high[15] = 0x80;
    env.settle(&m, low, 1, 1_000).unwrap();
    env.settle(&m, high, 1, 1_000).unwrap();
}

// ---- authorization and pause ------------------------------------------------

#[test]
fn only_the_settlement_operator_can_settle() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let before = snapshot(&env, &m);
    for intruder in [env.funded_keypair(), env.admin.insecure_clone(), m.buyer.insecure_clone()] {
        let mut accounts = env.settle_accounts(&m);
        accounts.operator = intruder.pubkey();
        let res = env.settle_with(&intruder, &accounts, order_id(1), ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_untouched(&env, &m, before, order_id(1));
}

#[test]
fn settle_reverts_while_paused() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let admin = env.admin.insecure_clone();
    env.pause(&admin).unwrap();
    let before = snapshot(&env, &m);
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::Paused);
    assert_untouched(&env, &m, before, order_id(1));
    env.unpause(&admin).unwrap();
    env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
}

// ---- amounts and counterparties ---------------------------------------------

#[test]
fn rejects_zero_amounts() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let before = snapshot(&env, &m);
    for (a, p) in [(0, PAYMENT), (ASSETS, 0), (0, 0)] {
        let res = env.settle(&m, order_id(1), a, p);
        assert_custom_err(res, ExchangeError::ZeroAmount);
    }
    assert_untouched(&env, &m, before, order_id(1));
}

#[test]
fn rejects_buyer_equal_to_seller() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    // The seller "buys" from itself, with its own payment account delegated.
    let seller = m.seller.insecure_clone();
    let own_payment =
        env.create_token_account(SPL_TOKEN, m.payment_mint, seller.pubkey(), PAYMENT);
    env.approve_vault(&seller, SPL_TOKEN, own_payment, m.payment_mint, PAYMENT);
    let own_asset_in = env.create_token_account(SPL_TOKEN, m.asset_mint, seller.pubkey(), 0);
    let mut a = env.settle_accounts(&m);
    a.buyer = seller.pubkey();
    a.buyer_asset = own_asset_in;
    a.buyer_payment = own_payment;
    let operator = env.operator.insecure_clone();
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::SelfTrade);
}

#[test]
fn rejects_vault_authority_as_counterparty() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let vault = vault_authority();
    let operator = env.operator.insecure_clone();
    let mut a = env.settle_accounts(&m);
    a.buyer = vault;
    a.buyer_asset = env.create_token_account(SPL_TOKEN, m.asset_mint, vault, 0);

    // The fee vault itself as the buyer's payment account: Anchor refuses the
    // same mutable account twice before the handler runs.
    a.buyer_payment = fee_vault_pda(&m.payment_mint);
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::ConstraintDuplicateMutableAccount);

    // Any other vault-owned account, even "delegated" to the vault.
    a.buyer_payment = env.create_token_account(SPL_TOKEN, m.payment_mint, vault, PAYMENT);
    env.update_token_account(&a.buyer_payment, |t| {
        t.delegate = Some(vault).into();
        t.delegated_amount = PAYMENT;
    });
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::InvalidCounterparty);

    // And as the seller.
    let mut a = env.settle_accounts(&m);
    a.seller = vault;
    a.seller_asset = env.create_token_account(SPL_TOKEN, m.asset_mint, vault, ASSETS);
    a.seller_payment = env.create_token_account(SPL_TOKEN, m.payment_mint, vault, 0);
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::InvalidCounterparty);
}

// ---- listings: mode/class coupling and payment mint -------------------------

#[test]
fn rejects_asset_listing_that_is_not_approved() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let authority = env.listing_authority.insecure_clone();
    let before = snapshot(&env, &m);

    env.suspend(&authority, m.asset_mint).unwrap();
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::ListingNotApproved);

    env.delist(&authority, m.asset_mint).unwrap();
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::ListingNotApproved);
    assert_untouched(&env, &m, before, order_id(1));
}

#[test]
fn rejects_pending_asset_listing() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let asset_mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
    env.submit_listing(&issuer, asset_mint, AssetClass::Equity, DOC_HASH)
        .unwrap();
    let payment_mint = env.fee_currency(SPL_TOKEN);
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, asset_mint, payment_mint)
        .unwrap();
    let m = env.trader_pair(asset_mint, payment_mint, issuer, 100, PAYMENT);
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::ListingNotApproved);
}

/// Invariant 8: only `Equity` settles in order-book mode.
#[test]
fn rejects_fixed_income_and_credit_assets() {
    for class in [AssetClass::FixedIncome, AssetClass::Credit] {
        let mut env = Env::initialized();
        let issuer = env.funded_keypair();
        let asset_mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
        env.submit_listing(&issuer, asset_mint, class, DOC_HASH)
            .unwrap();
        let authority = env.listing_authority.insecure_clone();
        env.approve(&authority, asset_mint).unwrap();
        let payment_mint = env.fee_currency(SPL_TOKEN);
        let payer = env.admin.insecure_clone();
        env.init_cashback_ledger(&payer, asset_mint, payment_mint)
            .unwrap();
        let m = env.trader_pair(asset_mint, payment_mint, issuer, 100, PAYMENT);
        let before = snapshot(&env, &m);
        let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::WrongTradingMode);
        assert_untouched(&env, &m, before, order_id(1));
    }
}

/// Invariants 8 and 9: a currency cannot be the asset leg, and it never gets
/// a cashback ledger, so it can never accrue cashback.
#[test]
fn rejects_currency_as_asset() {
    let mut env = Env::initialized();
    let asset_currency = env.listed_currency(SPL_TOKEN);
    let payment_mint = env.fee_currency(SPL_TOKEN);
    let payer = env.admin.insecure_clone();
    let res = env.init_cashback_ledger(&payer, asset_currency, payment_mint);
    assert_custom_err(res, ExchangeError::NotCashbackEligible);

    let nobody = env.funded_keypair();
    let m = env.trader_pair(asset_currency, payment_mint, nobody, 100, PAYMENT);
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::AccountNotInitialized);
    assert!(!env.exists(&m.cashback_ledger()));
}

/// Invariant 10: the payment mint must be an `Approved` `Currency` listing.
#[test]
fn rejects_payment_mint_that_is_not_an_approved_currency() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let authority = env.listing_authority.insecure_clone();
    let before = snapshot(&env, &m);

    env.suspend(&authority, m.payment_mint).unwrap();
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::InvalidPaymentMint);

    env.delist(&authority, m.payment_mint).unwrap();
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::InvalidPaymentMint);
    assert_untouched(&env, &m, before, order_id(1));
}

/// Fees are always in USDT, so `settle` pays only in the fee mint: neither
/// another approved currency (BRL) nor an equity mint. The fee ledger, fee
/// vault and cashback ledger passed are the real (fee-mint) ones, so the
/// payment mint is the only thing wrong.
#[test]
fn rejects_payment_mint_that_is_not_the_fee_mint() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let brl = env.listed_currency(SPL_TOKEN);
    let (equity, _) = env.listed_equity(SPL_TOKEN);
    let operator = env.operator.insecure_clone();
    for payment_mint in [brl, equity] {
        let issuer = m.issuer.insecure_clone();
        let other = env.trader_pair(m.asset_mint, payment_mint, issuer, 100, PAYMENT);
        let mut a = env.settle_accounts(&other);
        a.fee_ledger = fee_ledger_pda(&m.payment_mint);
        a.fee_vault = fee_vault_pda(&m.payment_mint);
        a.cashback_ledger = m.cashback_ledger();
        let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::NotFeeMint);
        assert_eq!(env.balance(&other.buyer_payment), PAYMENT);
    }
    // Neither ever gets a fee vault or a fee ledger.
    for mint in [brl, equity] {
        assert!(!env.exists(&fee_vault_pda(&mint)));
        assert!(!env.exists(&fee_ledger_pda(&mint)));
    }
    env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
}

#[test]
fn rejects_unlisted_mints() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let mut a = env.settle_accounts(&m);
    // A listing PDA for another mint does not match the asset mint's seeds.
    let (other, _) = env.listed_equity(SPL_TOKEN);
    a.asset_listing = listing_pda(&other);
    let operator = env.operator.insecure_clone();
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::ConstraintSeeds);
}

// ---- mint policy, re-run as defense in depth --------------------------------

/// Swaps a mint's data for that of a Token-2022 mint carrying `extension`.
/// Cannot happen on a real cluster (see test_mint_extension_lock.rs); this
/// checks that `settle` re-runs the allowlist anyway.
fn inject_extension(env: &mut Env, mint: &Pubkey, extension: ExtensionType, decimals: u8) {
    let donor = env.create_mint_2022(None, &[extension]);
    env.set_mint_decimals(&donor, decimals);
    let mut account = env.svm.get_account(&donor).unwrap();
    let original = env.svm.get_account(mint).unwrap();
    account.data[..82].copy_from_slice(&original.data[..82]);
    env.svm.set_account(*mint, account).unwrap();
}

#[test]
fn rechecks_extension_allowlist_on_both_mints() {
    for asset_side in [true, false] {
        let mut env = Env::initialized();
        let m = env.market(TOKEN_2022, TOKEN_2022);
        let (mint, decimals) = if asset_side {
            (m.asset_mint, ASSET_DECIMALS)
        } else {
            (m.payment_mint, PAYMENT_DECIMALS)
        };
        inject_extension(&mut env, &mint, ExtensionType::PermanentDelegate, decimals);
        let before = snapshot(&env, &m);
        let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::UnsupportedMintExtension);
        assert_untouched(&env, &m, before, order_id(1));
    }
}

#[test]
fn rechecks_asset_freeze_authority() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    env.set_freeze_authority(&m.asset_mint, Some(Pubkey::new_unique()));
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::FreezeAuthorityNotAllowed);
}

/// A payment mint with a freeze authority settles (accepted curated risk).
#[test]
fn payment_mint_with_freeze_authority_settles() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    env.set_freeze_authority(&m.payment_mint, Some(Pubkey::new_unique()));
    env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
}

// ---- delegations --------------------------------------------------------------

#[test]
fn rejects_missing_or_foreign_delegate() {
    for seller_side in [true, false] {
        for delegate in [None, Some(Pubkey::new_unique())] {
            let mut env = Env::initialized();
            let m = env.market(SPL_TOKEN, SPL_TOKEN);
            let account = if seller_side { m.seller_asset } else { m.buyer_payment };
            env.update_token_account(&account, |a| {
                a.delegate = delegate.into();
                a.delegated_amount = if delegate.is_some() { u64::MAX } else { 0 };
            });
            let before = snapshot(&env, &m);
            let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
            assert_custom_err(res, ExchangeError::DelegateNotSet);
            assert_untouched(&env, &m, before, order_id(1));
        }
    }
}

#[test]
fn rejects_insufficient_delegation() {
    for seller_side in [true, false] {
        let mut env = Env::initialized();
        let m = env.market(SPL_TOKEN, SPL_TOKEN);
        let (account, needed) = if seller_side {
            (m.seller_asset, ASSETS)
        } else {
            (m.buyer_payment, PAYMENT)
        };
        env.update_token_account(&account, |a| a.delegated_amount = needed - 1);
        let before = snapshot(&env, &m);
        let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::InsufficientDelegation);
        assert_untouched(&env, &m, before, order_id(1));
    }
}

/// The delegation passes but the balance does not: the token program fails a
/// later CPI and the whole instruction reverts, including the asset leg.
#[test]
fn reverts_atomically_when_a_later_leg_fails() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    env.update_token_account(&m.buyer_payment, |a| a.amount = PAYMENT - 1);
    let before = snapshot(&env, &m);
    assert!(env.settle(&m, order_id(1), ASSETS, PAYMENT).is_err());
    assert_untouched(&env, &m, before, order_id(1));
}

#[test]
fn reverts_when_an_account_is_frozen() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    env.update_token_account(&m.seller_payment, |a| a.state = AccountState::Frozen);
    let before = snapshot(&env, &m);
    assert!(env.settle(&m, order_id(1), ASSETS, PAYMENT).is_err());
    assert_untouched(&env, &m, before, order_id(1));
}

// ---- token account validation -------------------------------------------------

#[test]
fn rejects_destination_owned_by_someone_else() {
    for seller_payment in [true, false] {
        let mut env = Env::initialized();
        let m = env.market(SPL_TOKEN, SPL_TOKEN);
        let thief = Pubkey::new_unique();
        let mut a = env.settle_accounts(&m);
        if seller_payment {
            a.seller_payment = env.create_token_account(SPL_TOKEN, m.payment_mint, thief, 0);
        } else {
            a.buyer_asset = env.create_token_account(SPL_TOKEN, m.asset_mint, thief, 0);
        }
        let operator = env.operator.insecure_clone();
        let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
        assert_custom_err(res, AnchorError::ConstraintTokenOwner);
    }
}

#[test]
fn rejects_token_account_of_the_wrong_mint() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let mut a = env.settle_accounts(&m);
    // Payment-mint account passed as the buyer's asset account.
    a.buyer_asset = env.create_token_account(SPL_TOKEN, m.payment_mint, m.buyer.pubkey(), 0);
    let operator = env.operator.insecure_clone();
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::ConstraintTokenMint);
}

#[test]
fn rejects_wrong_token_program() {
    let mut env = Env::initialized();
    let m = env.market(TOKEN_2022, SPL_TOKEN);
    let mut a = env.settle_accounts(&m);
    a.asset_program = SPL_TOKEN;
    let operator = env.operator.insecure_clone();
    assert!(env
        .settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT)
        .is_err());
}

#[test]
fn rejects_foreign_fee_vault() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let mut a = env.settle_accounts(&m);
    a.fee_vault = env.create_token_account(SPL_TOKEN, m.payment_mint, vault_authority(), 0);
    let operator = env.operator.insecure_clone();
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::ConstraintSeeds);
}

#[test]
fn rejects_cashback_ledger_of_another_listing() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let other = env.market(SPL_TOKEN, SPL_TOKEN);
    let mut a = env.settle_accounts(&m);
    a.cashback_ledger = other.cashback_ledger();
    let operator = env.operator.insecure_clone();
    let res = env.settle_with(&operator, &a, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::ConstraintSeeds);
}

#[test]
fn requires_an_initialized_cashback_ledger() {
    let mut env = Env::initialized();
    let (asset_mint, issuer) = env.listed_equity(SPL_TOKEN);
    let payment_mint = env.fee_currency(SPL_TOKEN);
    let m = env.trader_pair(asset_mint, payment_mint, issuer, 100, PAYMENT);
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::AccountNotInitialized);
}

/// The fee mint is listed but `init_fee_vault` has not run yet: there is no
/// fee vault or ledger to collect into, so nothing settles.
#[test]
fn requires_the_fee_vault() {
    let mut env = Env::initialized();
    let (asset_mint, issuer) = env.listed_equity(SPL_TOKEN);
    let usdt = env.fee_mint;
    env.create_mint_at(usdt, SPL_TOKEN, Some(Pubkey::new_unique()));
    let authority = env.listing_authority.insecure_clone();
    env.list_currency(&authority, usdt).unwrap();
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, asset_mint, usdt).unwrap();
    let m = env.trader_pair(asset_mint, usdt, issuer, 100, PAYMENT);
    let res = env.settle(&m, order_id(1), ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::AccountNotInitialized);

    env.init_fee_vault(&payer, usdt).unwrap();
    env.settle(&m, order_id(1), ASSETS, PAYMENT).unwrap();
}
