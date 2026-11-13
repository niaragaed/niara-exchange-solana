//! `withdraw_cashback` (listing issuer), `withdraw_protocol_fees` and
//! `sweep_excess` (treasury): pull-model payouts from the fee vault, blocked
//! while paused.

mod common;

use anchor_lang::{error::ErrorCode as AnchorError, prelude::Pubkey};
use common::*;
use niara_exchange::{
    error::ExchangeError,
    events::{CashbackWithdrawn, ExcessSwept, ProtocolFeesWithdrawn},
    ConfigChange,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

/// 30 bps of 1_000_000_000 = 3_000_000 fee; 20% = 600_000 cashback.
const PAYMENT: u64 = 1_000_000_000;
const FEE: u64 = 3_000_000;
const CASHBACK: u64 = 600_000;
/// Protocol share of one settlement's trading fee.
const PROTOCOL: u64 = FEE - CASHBACK;
/// The fee vault after `settled`: the market's listing fee (spec §3.13) plus
/// the settlement's fee.
const VAULT: u64 = LISTING_FEE + FEE;
/// What the treasury may withdraw after `settled`: the trading share plus the
/// listing fee, which is all protocol revenue.
const TREASURY: u64 = PROTOCOL + LISTING_FEE;

struct Setup {
    env: Env,
    m: Market,
    /// Payment-mint account owned by the issuer.
    issuer_dest: Pubkey,
    /// Payment-mint account owned by the treasury.
    treasury_dest: Pubkey,
}

fn settled(payment_program: Pubkey) -> Setup {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, payment_program);
    env.settle(&m, match_id(1), 1, PAYMENT).unwrap();
    let issuer_dest = env.create_token_account(payment_program, m.payment_mint, m.issuer.pubkey(), 0);
    let treasury = env.treasury.pubkey();
    let treasury_dest = env.create_token_account(payment_program, m.payment_mint, treasury, 0);
    Setup {
        env,
        m,
        issuer_dest,
        treasury_dest,
    }
}

impl Setup {
    fn issuer(&self) -> Keypair {
        self.m.issuer.insecure_clone()
    }
    fn treasury(&self) -> Keypair {
        self.env.treasury.insecure_clone()
    }
    fn withdraw_cashback(&mut self, amount: u64) -> litesvm::types::TransactionResult {
        let issuer = self.issuer();
        let dest = self.issuer_dest;
        self.env
            .withdraw_cashback(&issuer, self.m.asset_mint, self.m.payment_mint, dest, amount)
    }
    fn withdraw_fees(&mut self, amount: u64) -> litesvm::types::TransactionResult {
        let treasury = self.treasury();
        let dest = self.treasury_dest;
        self.env
            .withdraw_protocol_fees(&treasury, self.m.payment_mint, dest, amount)
    }
    fn vault(&self) -> u64 {
        self.env.balance(&fee_vault_pda(&self.m.payment_mint))
    }
}

// ---- withdraw_cashback --------------------------------------------------------

#[test]
fn issuer_withdraws_cashback_in_parts() {
    for payment_program in [SPL_TOKEN, TOKEN_2022] {
        let mut s = settled(payment_program);
        assert_eq!(s.env.cashback(&s.m).accrued, CASHBACK);

        let meta = s.withdraw_cashback(200_000).unwrap();
        let e = &events::<CashbackWithdrawn>(&meta)[0];
        assert_eq!(e.listing, listing_pda(&s.m.asset_mint));
        assert_eq!(e.issuer, s.m.issuer.pubkey());
        assert_eq!(e.payment_mint, s.m.payment_mint);
        assert_eq!(e.destination, s.issuer_dest);
        assert_eq!(e.amount, 200_000);

        s.withdraw_cashback(CASHBACK - 200_000).unwrap();
        assert_eq!(s.env.balance(&s.issuer_dest), CASHBACK);
        assert_eq!(s.vault(), VAULT - CASHBACK);
        let ledger = s.env.cashback(&s.m);
        assert_eq!((ledger.accrued, ledger.withdrawn), (CASHBACK, CASHBACK));
        assert_eq!(s.env.fee_ledger(&s.m.payment_mint).cashback_withdrawn, CASHBACK);
        s.env.assert_vault_matches_ledger(&s.m.payment_mint);
    }
}

#[test]
fn cashback_cannot_exceed_what_is_available() {
    let mut s = settled(SPL_TOKEN);
    let res = s.withdraw_cashback(CASHBACK + 1);
    assert_custom_err(res, ExchangeError::InsufficientCashback);
    s.withdraw_cashback(CASHBACK).unwrap();
    let res = s.withdraw_cashback(1);
    assert_custom_err(res, ExchangeError::InsufficientCashback);
    s.env.assert_vault_matches_ledger(&s.m.payment_mint);
}

#[test]
fn only_the_listing_issuer_withdraws_cashback() {
    let mut s = settled(SPL_TOKEN);
    let intruders = [
        s.env.funded_keypair(),
        s.env.admin.insecure_clone(),
        s.env.treasury.insecure_clone(),
        s.env.operator.insecure_clone(),
        s.m.seller.insecure_clone(),
    ];
    for intruder in intruders {
        let dest = s.issuer_dest;
        let res =
            s.env
                .withdraw_cashback(&intruder, s.m.asset_mint, s.m.payment_mint, dest, 1);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_eq!(s.env.cashback(&s.m).withdrawn, 0);
}

/// An issuer of one listing cannot pull another listing's cashback.
#[test]
fn issuer_cannot_withdraw_another_listings_cashback() {
    let mut s = settled(SPL_TOKEN);
    let (other_asset, other_issuer) = s.env.listed_equity(SPL_TOKEN);
    let dest = s.env.create_token_account(SPL_TOKEN, s.m.payment_mint, other_issuer.pubkey(), 0);
    // Other issuer signs, names the first listing.
    let res = s
        .env
        .withdraw_cashback(&other_issuer, s.m.asset_mint, s.m.payment_mint, dest, 1);
    assert_custom_err(res, ExchangeError::Unauthorized);
    // Other issuer's own listing has no ledger for this currency yet.
    let res = s
        .env
        .withdraw_cashback(&other_issuer, other_asset, s.m.payment_mint, dest, 1);
    assert_custom_err(res, AnchorError::AccountNotInitialized);
}

/// The issuer may send cashback to any account of the payment mint.
#[test]
fn issuer_chooses_the_destination() {
    let mut s = settled(SPL_TOKEN);
    let elsewhere = s
        .env
        .create_token_account(SPL_TOKEN, s.m.payment_mint, Pubkey::new_unique(), 0);
    let issuer = s.issuer();
    s.env
        .withdraw_cashback(&issuer, s.m.asset_mint, s.m.payment_mint, elsewhere, CASHBACK)
        .unwrap();
    assert_eq!(s.env.balance(&elsewhere), CASHBACK);
}

// ---- withdraw_protocol_fees -------------------------------------------------

/// The protocol share is the trading fee minus cashback, plus every listing
/// fee (no cashback share).
#[test]
fn treasury_withdraws_only_the_protocol_share() {
    for payment_program in [SPL_TOKEN, TOKEN_2022] {
        let mut s = settled(payment_program);
        assert_eq!(s.env.fee_ledger(&s.m.payment_mint).listing_fees_collected, LISTING_FEE);
        let res = s.withdraw_fees(TREASURY + 1);
        assert_custom_err(res, ExchangeError::InsufficientProtocolFees);

        let meta = s.withdraw_fees(TREASURY).unwrap();
        let e = &events::<ProtocolFeesWithdrawn>(&meta)[0];
        assert_eq!(e.treasury, s.env.treasury.pubkey());
        assert_eq!(e.payment_mint, s.m.payment_mint);
        assert_eq!(e.destination, s.treasury_dest);
        assert_eq!(e.amount, TREASURY);
        assert_eq!(s.env.balance(&s.treasury_dest), TREASURY);
        assert_eq!(s.env.fee_ledger(&s.m.payment_mint).protocol_withdrawn, TREASURY);
        s.env.assert_vault_matches_ledger(&s.m.payment_mint);

        // The cashback share stays for the issuer.
        assert_eq!(s.vault(), CASHBACK);
        let res = s.withdraw_fees(1);
        assert_custom_err(res, ExchangeError::InsufficientProtocolFees);
        s.withdraw_cashback(CASHBACK).unwrap();
        assert_eq!(s.vault(), 0);
        s.env.assert_vault_matches_ledger(&s.m.payment_mint);
    }
}

/// Withdrawals interleaved with new settlements keep the vault == ledger.
#[test]
fn withdrawals_interleave_with_settlements() {
    let mut s = settled(SPL_TOKEN);
    s.withdraw_fees(1_000_000).unwrap();
    s.withdraw_cashback(100_000).unwrap();
    let m = s.env.trader_pair(
        s.m.asset_mint,
        s.m.payment_mint,
        s.issuer(),
        10,
        PAYMENT,
    );
    s.env.settle(&m, match_id(2), 1, PAYMENT).unwrap();
    s.env.assert_vault_matches_ledger(&s.m.payment_mint);
    s.withdraw_fees(TREASURY + PROTOCOL - 1_000_000).unwrap();
    s.withdraw_cashback(2 * CASHBACK - 100_000).unwrap();
    assert_eq!(s.vault(), 0);
    s.env.assert_vault_matches_ledger(&s.m.payment_mint);
}

#[test]
fn only_the_treasury_withdraws_protocol_fees() {
    let mut s = settled(SPL_TOKEN);
    let intruders = [
        s.env.funded_keypair(),
        s.env.admin.insecure_clone(),
        s.env.operator.insecure_clone(),
        s.issuer(),
    ];
    for intruder in intruders {
        let dest = s.treasury_dest;
        let res = s.env.withdraw_protocol_fees(&intruder, s.m.payment_mint, dest, 1);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_eq!(s.env.fee_ledger(&s.m.payment_mint).protocol_withdrawn, 0);
}

/// After a timelocked treasury change, only the new treasury can withdraw.
#[test]
fn rotated_treasury_takes_over() {
    let mut s = settled(SPL_TOKEN);
    let new_treasury = s.env.funded_keypair();
    let admin = s.env.admin.insecure_clone();
    let change = ConfigChange::Treasury(new_treasury.pubkey());
    s.env.propose(&admin, change).unwrap();
    s.env.warp_secs(TIMELOCK_SECS);
    s.env.execute(&admin, &change).unwrap();

    let res = s.withdraw_fees(1);
    assert_custom_err(res, ExchangeError::Unauthorized);
    let dest = s.treasury_dest;
    s.env
        .withdraw_protocol_fees(&new_treasury, s.m.payment_mint, dest, PROTOCOL)
        .unwrap();
}

// ---- sweep_excess -------------------------------------------------------------

impl Setup {
    fn sweep(&mut self) -> litesvm::types::TransactionResult {
        let treasury = self.treasury();
        let dest = self.treasury_dest;
        self.env.sweep_excess(&treasury, self.m.payment_mint, dest)
    }
}

#[test]
fn treasury_sweeps_only_the_excess() {
    for payment_program in [SPL_TOKEN, TOKEN_2022] {
        let mut s = settled(payment_program);
        s.env.donate_to_fee_vault(s.m.payment_mint, 777);
        assert_eq!(s.vault(), VAULT + 777);
        let ledger_before = s.env.fee_ledger(&s.m.payment_mint);

        let meta = s.sweep().unwrap();
        let e = &events::<ExcessSwept>(&meta)[0];
        assert_eq!(e.treasury, s.env.treasury.pubkey());
        assert_eq!(e.payment_mint, s.m.payment_mint);
        assert_eq!(e.destination, s.treasury_dest);
        assert_eq!(e.amount, 777);
        assert_eq!(e.vault_balance_after, VAULT);
        assert_eq!(s.env.balance(&s.treasury_dest), 777);

        // Invariant 4 is strict again; the ledger is untouched.
        assert_eq!(s.vault(), VAULT);
        s.env.assert_vault_matches_ledger(&s.m.payment_mint);
        let ledger = s.env.fee_ledger(&s.m.payment_mint);
        assert_eq!(
            (ledger.fees_collected, ledger.protocol_withdrawn, ledger.cashback_withdrawn),
            (
                ledger_before.fees_collected,
                ledger_before.protocol_withdrawn,
                ledger_before.cashback_withdrawn
            )
        );
        assert_custom_err(s.sweep(), ExchangeError::NothingToSweep);

        // Everything owed is still there for its owners.
        s.withdraw_cashback(CASHBACK).unwrap();
        s.withdraw_fees(TREASURY).unwrap();
        assert_eq!(s.vault(), 0);
    }
}

#[test]
fn sweep_without_excess_is_rejected() {
    let mut s = settled(SPL_TOKEN);
    assert_custom_err(s.sweep(), ExchangeError::NothingToSweep);
    s.withdraw_fees(PROTOCOL).unwrap();
    s.withdraw_cashback(CASHBACK / 2).unwrap();
    assert_custom_err(s.sweep(), ExchangeError::NothingToSweep);
    assert_eq!(s.env.balance(&s.treasury_dest), PROTOCOL);

    // Another currency has no fee vault or ledger at all: fees are only in
    // the fee mint.
    let currency = s.env.listed_currency(SPL_TOKEN);
    let treasury = s.treasury();
    let dest = s.env.create_token_account(SPL_TOKEN, currency, treasury.pubkey(), 0);
    let res = s.env.sweep_excess(&treasury, currency, dest);
    assert_custom_err(res, AnchorError::AccountNotInitialized);
}

/// Donations interleaved with settlements and partial withdrawals: the sweep
/// takes exactly the donations, never owed fees or cashback.
#[test]
fn sweep_interleaves_with_settlements_and_withdrawals() {
    let mut s = settled(SPL_TOKEN);
    s.withdraw_fees(1_000_000).unwrap();
    s.env.donate_to_fee_vault(s.m.payment_mint, 40);
    s.withdraw_cashback(100_000).unwrap();
    let m = s.env.trader_pair(s.m.asset_mint, s.m.payment_mint, s.issuer(), 10, PAYMENT);
    s.env.settle(&m, match_id(2), 1, PAYMENT).unwrap();
    s.env.donate_to_fee_vault(s.m.payment_mint, 2);

    let meta = s.sweep().unwrap();
    assert_eq!(events::<ExcessSwept>(&meta)[0].amount, 42);
    s.env.assert_vault_matches_ledger(&s.m.payment_mint);
    s.withdraw_fees(TREASURY + PROTOCOL - 1_000_000).unwrap();
    s.withdraw_cashback(2 * CASHBACK - 100_000).unwrap();
    assert_eq!(s.vault(), 0);
    assert_eq!(s.env.balance(&s.treasury_dest), TREASURY + PROTOCOL + 42);
}

#[test]
fn only_the_treasury_sweeps() {
    let mut s = settled(SPL_TOKEN);
    s.env.donate_to_fee_vault(s.m.payment_mint, 500);
    let intruders = [
        s.env.funded_keypair(),
        s.env.admin.insecure_clone(),
        s.env.operator.insecure_clone(),
        s.env.listing_authority.insecure_clone(),
        s.issuer(),
    ];
    for intruder in intruders {
        let dest = s.treasury_dest;
        let res = s.env.sweep_excess(&intruder, s.m.payment_mint, dest);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_eq!(s.vault(), VAULT + 500);
}

#[test]
fn sweep_is_blocked_while_paused() {
    let mut s = settled(SPL_TOKEN);
    s.env.donate_to_fee_vault(s.m.payment_mint, 500);
    let admin = s.env.admin.insecure_clone();
    s.env.pause(&admin).unwrap();
    assert_custom_err(s.sweep(), ExchangeError::Paused);
    assert_eq!(s.vault(), VAULT + 500);
    s.env.unpause(&admin).unwrap();
    s.sweep().unwrap();
    assert_eq!(s.vault(), VAULT);
}

#[test]
fn sweep_destination_must_hold_the_payment_mint() {
    let mut s = settled(SPL_TOKEN);
    s.env.donate_to_fee_vault(s.m.payment_mint, 500);
    let treasury = s.treasury();
    let wrong = s.env.create_token_account(SPL_TOKEN, s.m.asset_mint, treasury.pubkey(), 0);
    let res = s.env.sweep_excess(&treasury, s.m.payment_mint, wrong);
    assert_custom_err(res, AnchorError::ConstraintTokenMint);
    assert_eq!(s.vault(), VAULT + 500);
}

// ---- shared failure paths -----------------------------------------------------

/// Pause principle (spec §3.3): a pause stops payouts based on the protocol's
/// own accounting.
#[test]
fn both_withdrawals_are_blocked_while_paused() {
    let mut s = settled(SPL_TOKEN);
    let admin = s.env.admin.insecure_clone();
    s.env.pause(&admin).unwrap();
    assert_custom_err(s.withdraw_cashback(1), ExchangeError::Paused);
    assert_custom_err(s.withdraw_fees(1), ExchangeError::Paused);
    assert_eq!(s.vault(), VAULT);
    s.env.unpause(&admin).unwrap();
    s.withdraw_cashback(1).unwrap();
    s.withdraw_fees(1).unwrap();
}

#[test]
fn zero_amount_withdrawals_are_rejected() {
    let mut s = settled(SPL_TOKEN);
    assert_custom_err(s.withdraw_cashback(0), ExchangeError::ZeroAmount);
    assert_custom_err(s.withdraw_fees(0), ExchangeError::ZeroAmount);
}

/// Paying the vault into itself would mark funds withdrawn while they stay in
/// the vault. Anchor's duplicate-mutable-account check catches it first;
/// `InvalidDestination` is the program's own guard behind it.
#[test]
fn destination_cannot_be_the_fee_vault() {
    let mut s = settled(SPL_TOKEN);
    let vault = fee_vault_pda(&s.m.payment_mint);
    let issuer = s.issuer();
    let treasury = s.treasury();
    s.env.donate_to_fee_vault(s.m.payment_mint, 500);
    for res in [
        s.env
            .withdraw_cashback(&issuer, s.m.asset_mint, s.m.payment_mint, vault, 1),
        s.env.withdraw_protocol_fees(&treasury, s.m.payment_mint, vault, 1),
        s.env.sweep_excess(&treasury, s.m.payment_mint, vault),
    ] {
        let err = res.expect_err("vault paid into itself");
        let dbg = format!("{:?}", err.err);
        let duplicate = u32::from(AnchorError::ConstraintDuplicateMutableAccount);
        let invalid = u32::from(ExchangeError::InvalidDestination);
        assert!(
            dbg.contains(&format!("Custom({duplicate})"))
                || dbg.contains(&format!("Custom({invalid})")),
            "unexpected error {dbg}"
        );
    }
    let ledger = s.env.fee_ledger(&s.m.payment_mint);
    assert_eq!((ledger.protocol_withdrawn, ledger.cashback_withdrawn), (0, 0));
}

#[test]
fn destination_must_hold_the_payment_mint() {
    let mut s = settled(SPL_TOKEN);
    let wrong = s
        .env
        .create_token_account(SPL_TOKEN, s.m.asset_mint, s.m.issuer.pubkey(), 0);
    let issuer = s.issuer();
    let res = s
        .env
        .withdraw_cashback(&issuer, s.m.asset_mint, s.m.payment_mint, wrong, 1);
    assert_custom_err(res, AnchorError::ConstraintTokenMint);
    let treasury = s.treasury();
    let res = s.env.withdraw_protocol_fees(&treasury, s.m.payment_mint, wrong, 1);
    assert_custom_err(res, AnchorError::ConstraintTokenMint);
}

/// Accrued amounts stay withdrawable after the asset is delisted or the
/// currency is suspended: listing status gates trading, not payouts.
#[test]
fn withdrawals_survive_listing_status_changes() {
    let mut s = settled(SPL_TOKEN);
    let authority = s.env.listing_authority.insecure_clone();
    s.env.delist(&authority, s.m.asset_mint).unwrap();
    s.env.suspend(&authority, s.m.payment_mint).unwrap();
    s.withdraw_cashback(CASHBACK).unwrap();
    s.withdraw_fees(TREASURY).unwrap();
    assert_eq!(s.vault(), 0);
}

/// Currency listings have no issuer and no cashback ledger.
#[test]
fn currency_listings_have_no_cashback_to_withdraw() {
    let mut s = settled(SPL_TOKEN);
    let anyone = s.env.funded_keypair();
    let dest = s.issuer_dest;
    let currency = s.env.listed_currency(SPL_TOKEN);
    let res = s
        .env
        .withdraw_cashback(&anyone, currency, s.m.payment_mint, dest, 1);
    assert!(res.is_err());
}
