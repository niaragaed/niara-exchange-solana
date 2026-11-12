//! The fee mint (USDT, spec §3.4): `init_fee_vault`, the one `FeeLedger`
//! (which makes invariants 2 and 4 checkable on-chain) and the timelocked
//! fee-mint switch.

mod common;

use anchor_lang::{error::ErrorCode as AnchorError, prelude::Pubkey};
use anchor_spl::token_2022::spl_token_2022::extension::ExtensionType;
use common::*;
use niara_exchange::{
    error::ExchangeError, events::FeeVaultInitialized, AssetClass, Config, ConfigChange,
    DealSide, FeeLedger, BPS_DENOMINATOR,
};
use solana_signer::Signer;

fn fee_of(payment: u64) -> u64 {
    (payment * FEE_BPS as u64).div_ceil(BPS_DENOMINATOR as u64)
}

fn cashback_of(fee: u64) -> u64 {
    fee * CASHBACK_BPS as u64 / BPS_DENOMINATOR as u64
}

fn assert_empty_vault_and_ledger(env: &Env, mint: &Pubkey, token_program: Pubkey) {
    let ledger: FeeLedger = env.fee_ledger(mint);
    assert_eq!(ledger.payment_mint, *mint);
    assert_eq!(
        (
            ledger.fees_collected,
            ledger.cashback_accrued,
            ledger.protocol_withdrawn,
            ledger.cashback_withdrawn
        ),
        (0, 0, 0, 0)
    );
    let vault = env.token_account(&fee_vault_pda(mint));
    assert_eq!((vault.mint, vault.owner, vault.amount), (*mint, vault_authority(), 0));
    assert_eq!(env.owner_of(&fee_vault_pda(mint)), token_program);
}

// ---- init_fee_vault ------------------------------------------------------------

/// Permissionless: a stranger pays the rent. Nothing needs the fee mint to
/// be listed first.
#[test]
fn init_fee_vault_creates_an_empty_ledger_and_vault() {
    for token_program in [SPL_TOKEN, TOKEN_2022] {
        let mut env = Env::initialized();
        let usdt = env.fee_mint;
        env.create_mint_at(usdt, token_program, None);
        let stranger = env.funded_keypair();
        let meta = env.init_fee_vault(&stranger, usdt).unwrap();
        assert_empty_vault_and_ledger(&env, &usdt, token_program);
        let e = &events::<FeeVaultInitialized>(&meta)[0];
        assert_eq!(
            (e.fee_mint, e.fee_vault, e.fee_ledger),
            (usdt, fee_vault_pda(&usdt), fee_ledger_pda(&usdt))
        );
    }
}

#[test]
fn init_fee_vault_cannot_run_twice() {
    let mut env = Env::initialized();
    let usdt = env.fee_currency(SPL_TOKEN);
    let payer = env.funded_keypair();
    assert!(env.init_fee_vault(&payer, usdt).is_err());
}

/// Only the configured fee mint gets a vault: `list_currency` creates none,
/// and `init_fee_vault` refuses any other mint, listed or not.
#[test]
fn only_the_fee_mint_has_a_fee_vault() {
    let mut env = Env::initialized();
    env.fee_currency(SPL_TOKEN);
    let brl = env.listed_currency(SPL_TOKEN);
    let (equity, _) = env.listed_equity(SPL_TOKEN);
    let unlisted = env.create_mint(SPL_TOKEN, None);
    let payer = env.funded_keypair();
    for mint in [brl, equity, unlisted] {
        assert!(!env.exists(&fee_vault_pda(&mint)));
        let res = env.init_fee_vault(&payer, mint);
        assert_custom_err(res, ExchangeError::NotFeeMint);
        assert!(!env.exists(&fee_vault_pda(&mint)) && !env.exists(&fee_ledger_pda(&mint)));
    }
}

/// The mint policy runs before anything is created: a fee mint with a
/// rejected extension never gets a vault; allowlisted metadata is fine.
#[test]
fn init_fee_vault_runs_the_mint_policy() {
    let mut env =
        Env::initialized_with_fee_mint(|env| env.create_mint_2022(None, &[ExtensionType::PermanentDelegate]));
    let payer = env.funded_keypair();
    let usdt = env.fee_mint;
    let res = env.init_fee_vault(&payer, usdt);
    assert_custom_err(res, ExchangeError::UnsupportedMintExtension);
    assert!(!env.exists(&fee_vault_pda(&usdt)) && !env.exists(&fee_ledger_pda(&usdt)));

    let exts = [ExtensionType::MetadataPointer, ExtensionType::TokenMetadata];
    let mut env = Env::initialized_with_fee_mint(|env| env.create_mint_2022(None, &exts));
    let usdt = env.fee_mint;
    let payer = env.funded_keypair();
    env.init_fee_vault(&payer, usdt).unwrap();
    assert_empty_vault_and_ledger(&env, &usdt, TOKEN_2022);
}

/// Pause principle: account setup stays available while paused.
#[test]
fn init_fee_vault_works_while_paused() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    env.pause(&admin).unwrap();
    let usdt = env.fee_currency(SPL_TOKEN);
    assert_empty_vault_and_ledger(&env, &usdt, SPL_TOKEN);
}

/// Lamports sent to the fee vault address beforehand (anyone can do it) must
/// not block `init_fee_vault`: the vault is still created, rent-exempt, and
/// owned by the right token program.
#[test]
fn prefunded_fee_vault_address_does_not_block_init_fee_vault() {
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
        let usdt = env.fee_mint;
        env.create_mint_at(usdt, token_program, None);
        env.svm.airdrop(&fee_vault_pda(&usdt), prefund).unwrap();
        let payer = env.funded_keypair();
        env.init_fee_vault(&payer, usdt).unwrap();

        assert_empty_vault_and_ledger(&env, &usdt, token_program);
        let account = env.svm.get_account(&fee_vault_pda(&usdt)).unwrap();
        let rent = env.svm.minimum_balance_for_rent_exemption(account.data.len());
        assert!(account.lamports >= rent.max(prefund));
    }
}

// ---- the one fee ledger ------------------------------------------------------------

#[test]
fn settle_records_gross_fee_and_cashback() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN);
    let mut fees = 0;
    let mut cashback = 0;
    // 2 is the smallest payment that leaves the seller something (fee 1).
    for (i, payment) in [2u64, 10_001, 25_000_000, 333_333].into_iter().enumerate() {
        env.settle(&m, order_id(i as u128 + 1), 1, payment).unwrap();
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

/// Every listing and both trading modes collect into the same ledger, which
/// aggregates the cashback ledgers of all listings.
#[test]
fn one_ledger_for_every_listing_and_both_modes() {
    let mut env = Env::initialized();
    let a = env.market(SPL_TOKEN, SPL_TOKEN);
    let b = env.market(TOKEN_2022, SPL_TOKEN);
    let bond = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let brl = env.p2p(AssetClass::Currency, TOKEN_2022, SPL_TOKEN);
    assert_eq!((a.payment_mint, b.payment_mint, bond.payment_mint), (env.fee_mint, env.fee_mint, env.fee_mint));

    env.settle(&a, order_id(1), 1, 1_000_000).unwrap();
    env.settle(&b, order_id(2), 1, 2_000_000).unwrap();
    env.open_deal(&bond, deal_id(1), DealSide::Sell, 1, 5_000_000).unwrap();
    env.accept_deal(&bond, deal_id(1)).unwrap();
    env.open_deal(&brl, deal_id(1), DealSide::Buy, 1_000, 7_000_000).unwrap();
    env.accept_deal(&brl, deal_id(1)).unwrap();

    let ledger = env.fee_ledger(&env.fee_mint);
    let fees = [1_000_000, 2_000_000, 5_000_000, 7_000_000].map(fee_of);
    assert_eq!(ledger.fees_collected, fees.iter().sum::<u64>());
    let bond_cashback = env.fetch::<niara_exchange::CashbackLedger>(&cashback_pda(&bond.asset_mint, &env.fee_mint)).accrued;
    assert_eq!(
        ledger.cashback_accrued,
        env.cashback(&a).accrued + env.cashback(&b).accrued + bond_cashback
    );
    assert_eq!(bond_cashback, cashback_of(fees[2]));
    env.assert_vault_matches_ledger(&env.fee_mint);
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

// ---- fee-mint switch (timelocked) ---------------------------------------------------

fn switch_fee_mint(env: &mut Env, new_mint: Pubkey) {
    let admin = env.admin.insecure_clone();
    let change = ConfigChange::FeeMint(new_mint);
    env.propose(&admin, change).unwrap();
    assert_custom_err(env.execute(&admin, &change), ExchangeError::TimelockNotElapsed);
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&admin, &change).unwrap();
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_mint, new_mint);
    env.fee_mint = new_mint;
}

/// After a switch: new trades pay in the new mint and collect into its own
/// vault; the old vault's fees and cashback stay withdrawable through the old
/// ledgers; open deals in the old mint can only be refunded.
#[test]
fn fee_mint_switch_keeps_old_balances_withdrawable() {
    let mut env = Env::initialized();
    let old = env.market(SPL_TOKEN, SPL_TOKEN);
    let old_usdt = old.payment_mint;
    env.settle(&old, order_id(1), 1, 10_000_000).unwrap();
    let bond = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    for n in [1, 2] {
        env.open_deal(&bond, deal_id(n), DealSide::Sell, 1, 1_000_000).unwrap();
    }
    let old_ledger = env.fee_ledger(&old_usdt);

    // A redeployed test USDT, under the other token program.
    let new_usdt = env.create_mint(TOKEN_2022, Some(Pubkey::new_unique()));
    env.set_mint_decimals(&new_usdt, PAYMENT_DECIMALS);
    switch_fee_mint(&mut env, new_usdt);

    // Nothing trades until the new fee vault exists, and the old mint never
    // trades again.
    let res = env.settle(&old, order_id(2), 1, 10_000_000);
    assert_custom_err(res, ExchangeError::NotFeeMint);
    assert_eq!(env.fee_currency(TOKEN_2022), new_usdt);
    assert_empty_vault_and_ledger(&env, &new_usdt, TOKEN_2022);
    let res = env.settle(&old, order_id(2), 1, 10_000_000);
    assert_custom_err(res, ExchangeError::NotFeeMint);

    // Cashback ledgers are keyed by the current fee mint.
    let payer = env.admin.insecure_clone();
    let res = env.init_cashback_ledger(&payer, old.asset_mint, old_usdt);
    assert_custom_err(res, AnchorError::ConstraintSeeds);
    env.init_cashback_ledger(&payer, old.asset_mint, new_usdt).unwrap();
    env.init_cashback_ledger(&payer, bond.asset_mint, new_usdt).unwrap();

    // Old-mint deals: accept fails, both refunds work.
    let res = env.accept_deal(&bond, deal_id(1));
    assert_custom_err(res, ExchangeError::NotFeeMint);
    env.cancel_deal(&bond, deal_id(1)).unwrap();
    env.warp_secs(DEAL_TTL);
    env.expire_deal(&bond, deal_id(2)).unwrap();
    assert_eq!(env.balance(&bond.proposer_asset), P2P_ASSETS);

    // New trades collect into the new vault only.
    let issuer = old.issuer.insecure_clone();
    let fresh = env.trader_pair(old.asset_mint, new_usdt, issuer, 10, 1_000_000_000);
    env.settle(&fresh, order_id(3), 1, 20_000_000).unwrap();
    assert_eq!(env.fee_ledger(&new_usdt).fees_collected, fee_of(20_000_000));
    assert_eq!(env.fee_ledger(&old_usdt).fees_collected, old_ledger.fees_collected);
    env.assert_vault_matches_ledger(&new_usdt);

    // The old vault still pays out what it owes.
    let issuer = old.issuer.insecure_clone();
    let cashback = env.fetch::<niara_exchange::CashbackLedger>(&cashback_pda(&old.asset_mint, &old_usdt)).accrued;
    assert!(cashback > 0);
    let to = env.create_token_account(SPL_TOKEN, old_usdt, issuer.pubkey(), 0);
    env.withdraw_cashback(&issuer, old.asset_mint, old_usdt, to, cashback).unwrap();
    let protocol = old_ledger.fees_collected - old_ledger.cashback_accrued;
    let treasury = env.treasury.insecure_clone();
    let to = env.create_token_account(SPL_TOKEN, old_usdt, treasury.pubkey(), 0);
    env.withdraw_protocol_fees(&treasury, old_usdt, to, protocol).unwrap();
    assert_eq!(env.balance(&fee_vault_pda(&old_usdt)), 0);
    env.assert_vault_matches_ledger(&old_usdt);
}
