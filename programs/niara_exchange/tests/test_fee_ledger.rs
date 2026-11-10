//! Per-payment-mint fee accounting (`FeeLedger`), which makes invariants 2
//! and 4 checkable on-chain.

mod common;

use anchor_lang::error::ErrorCode as AnchorError;
use common::*;
use niara_exchange::{FeeLedger, BPS_DENOMINATOR};

fn fee_of(payment: u64) -> u64 {
    (payment * FEE_BPS as u64).div_ceil(BPS_DENOMINATOR as u64)
}

fn cashback_of(fee: u64) -> u64 {
    fee * CASHBACK_BPS as u64 / BPS_DENOMINATOR as u64
}

#[test]
fn list_currency_creates_an_empty_ledger_and_vault() {
    for token_program in [SPL_TOKEN, TOKEN_2022] {
        let mut env = Env::initialized();
        let mint = env.listed_currency(token_program);
        let ledger: FeeLedger = env.fee_ledger(&mint);
        assert_eq!(ledger.payment_mint, mint);
        assert_eq!(
            (
                ledger.fees_collected,
                ledger.cashback_accrued,
                ledger.protocol_withdrawn,
                ledger.cashback_withdrawn
            ),
            (0, 0, 0, 0)
        );
        let vault = env.token_account(&fee_vault_pda(&mint));
        assert_eq!(vault.mint, mint);
        assert_eq!(vault.owner, vault_authority());
        assert_eq!(vault.amount, 0);
        assert_eq!(env.owner_of(&fee_vault_pda(&mint)), token_program);
    }
}

#[test]
fn settle_records_gross_fee_and_cashback() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let mut fees = 0;
    let mut cashback = 0;
    // 2 is the smallest payment that leaves the seller something (fee 1).
    for (i, payment) in [2u64, 10_001, 25_000_000, 333_333].into_iter().enumerate() {
        env.settle(&m, order_id(i as u128), 1, payment).unwrap();
        fees += fee_of(payment);
        cashback += cashback_of(fee_of(payment));
        let ledger = env.fee_ledger(&m.payment_mint);
        assert_eq!(ledger.fees_collected, fees);
        assert_eq!(ledger.cashback_accrued, cashback);
        assert!(ledger.cashback_accrued <= ledger.fees_collected); // invariant 2
        assert_eq!(env.cashback(&m).accrued, cashback);
        env.assert_vault_matches_ledger(&m.payment_mint); // invariant 4
    }
}

/// One currency, two listings: the fee ledger aggregates both cashback
/// ledgers; a second currency has its own ledger and vault.
#[test]
fn ledgers_aggregate_per_payment_mint() {
    let mut env = Env::initialized();
    let usd = env.market(SPL_TOKEN, SPL_TOKEN);
    let (other_asset, other_issuer) = env.listed_equity(TOKEN_2022);
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, other_asset, usd.payment_mint)
        .unwrap();
    let usd2 = env.trader_pair(other_asset, usd.payment_mint, other_issuer, 100, 1_000_000_000);

    let brl_mint = env.listed_currency(TOKEN_2022);
    env.init_cashback_ledger(&payer, usd.asset_mint, brl_mint)
        .unwrap();
    let issuer = usd.issuer.insecure_clone();
    let brl = env.trader_pair(usd.asset_mint, brl_mint, issuer, 100, 1_000_000_000);

    env.settle(&usd, order_id(1), 1, 1_000_000).unwrap();
    env.settle(&usd2, order_id(2), 1, 2_000_000).unwrap();
    env.settle(&brl, order_id(3), 1, 5_000_000).unwrap();

    let usd_ledger = env.fee_ledger(&usd.payment_mint);
    assert_eq!(usd_ledger.fees_collected, fee_of(1_000_000) + fee_of(2_000_000));
    assert_eq!(
        usd_ledger.cashback_accrued,
        env.cashback(&usd).accrued + env.cashback(&usd2).accrued
    );
    let brl_ledger = env.fee_ledger(&brl_mint);
    assert_eq!(brl_ledger.fees_collected, fee_of(5_000_000));
    assert_eq!(brl_ledger.cashback_accrued, env.cashback(&brl).accrued);
    env.assert_vault_matches_ledger(&usd.payment_mint);
    env.assert_vault_matches_ledger(&brl_mint);
}

#[test]
fn settle_rejects_the_fee_ledger_of_another_currency() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let other = env.listed_currency(SPL_TOKEN);
    let mut a = env.settle_accounts(&m);
    a.fee_ledger = fee_ledger_pda(&other);
    let operator = env.operator.insecure_clone();
    let res = env.settle_with(&operator, &a, order_id(1), 1, 1_000_000);
    assert_custom_err(res, AnchorError::ConstraintSeeds);
}

/// Anyone can transfer into the fee vault. Donations are not accounted, so
/// between sweeps on-chain invariant 4 is `vault >= unwithdrawn`;
/// `sweep_excess` restores equality (tests in `test_withdraw.rs`).
#[test]
fn donations_to_the_vault_are_not_accounted() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    env.settle(&m, order_id(1), 1, 1_000_000).unwrap();
    let ledger_before = env.fee_ledger(&m.payment_mint);

    env.donate_to_fee_vault(m.payment_mint, 500);

    let ledger = env.fee_ledger(&m.payment_mint);
    assert_eq!(ledger.fees_collected, ledger_before.fees_collected);
    let unwithdrawn =
        ledger.fees_collected - ledger.protocol_withdrawn - ledger.cashback_withdrawn;
    assert_eq!(env.balance(&fee_vault_pda(&m.payment_mint)), unwithdrawn + 500);
}

/// Lamports sent to the fee vault address before `list_currency` (anyone can
/// do it) must not block the listing: the vault is still created, rent-exempt,
/// and owned by the right token program.
#[test]
fn prefunded_fee_vault_address_does_not_block_list_currency() {
    // The runtime only lets a system account exist rent-exempt, so the smallest
    // possible pre-fund is the 0-byte minimum (below a token account's rent);
    // 10_000_000 is above it.
    for (token_program, small) in [(SPL_TOKEN, true), (TOKEN_2022, true), (SPL_TOKEN, false)] {
        let mut env = Env::initialized();
        let prefund = if small {
            env.svm.minimum_balance_for_rent_exemption(0)
        } else {
            10_000_000
        };
        let mint = env.create_mint(token_program, None);
        env.svm.airdrop(&fee_vault_pda(&mint), prefund).unwrap();
        let authority = env.listing_authority.insecure_clone();
        env.list_currency(&authority, mint).unwrap();

        let vault = env.token_account(&fee_vault_pda(&mint));
        assert_eq!((vault.mint, vault.owner, vault.amount), (mint, vault_authority(), 0));
        assert_eq!(env.owner_of(&fee_vault_pda(&mint)), token_program);
        let account = env.svm.get_account(&fee_vault_pda(&mint)).unwrap();
        let rent = env.svm.minimum_balance_for_rent_exemption(account.data.len());
        assert!(account.lamports >= rent.max(prefund));
    }
}
