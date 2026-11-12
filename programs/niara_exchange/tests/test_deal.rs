//! P2P escrow (spec §3.5): `open_deal`, `accept_deal`, `cancel_deal` and
//! `expire_deal`.

mod common;

use anchor_lang::{error::ErrorCode as AnchorError, prelude::Pubkey};
use anchor_spl::token_2022::spl_token_2022::extension::ExtensionType;
use common::*;
use niara_exchange::{
    error::ExchangeError,
    events::{DealCancelled, DealExecuted, DealExpired, DealOpened},
    AssetClass, CashbackLedger, ConfigChange, DealSide, DealStatus, BPS_DENOMINATOR,
    MAX_DEAL_DURATION_SECS, MAX_FEE_BPS,
};
use solana_signer::Signer;

const ASSETS: u64 = 10;
const PAYMENT: u64 = 25_000_000; // 25.000000 in a 6-decimal currency
const SIDES: [DealSide; 2] = [DealSide::Sell, DealSide::Buy];
const PROGRAMS: [(Pubkey, Pubkey); 4] = [
    (SPL_TOKEN, SPL_TOKEN),
    (TOKEN_2022, TOKEN_2022),
    (SPL_TOKEN, TOKEN_2022),
    (TOKEN_2022, SPL_TOKEN),
];

fn expected_fee(payment: u64, fee_bps: u16) -> u64 {
    (payment * fee_bps as u64).div_ceil(BPS_DENOMINATOR as u64)
}

fn expected_cashback(fee: u64, eligible: bool) -> u64 {
    if eligible {
        fee * CASHBACK_BPS as u64 / BPS_DENOMINATOR as u64
    } else {
        0
    }
}

/// Balances of every account a deal touches.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Snapshot {
    proposer_asset: u64,
    proposer_payment: u64,
    counterparty_asset: u64,
    counterparty_payment: u64,
    fee_vault: u64,
    deal_vault: u64,
    cashback: u64,
}

fn snapshot(env: &Env, p: &P2p, id: [u8; 16]) -> Snapshot {
    let vault = p.vault(id);
    let ledger = cashback_pda(&p.asset_mint, &env.fee_mint);
    Snapshot {
        proposer_asset: env.balance(&p.proposer_asset),
        proposer_payment: env.balance(&p.proposer_payment),
        counterparty_asset: env.balance(&p.counterparty_asset),
        counterparty_payment: env.balance(&p.counterparty_payment),
        fee_vault: env.balance(&fee_vault_pda(&env.fee_mint)),
        deal_vault: if env.exists(&vault) { env.balance(&vault) } else { 0 },
        cashback: if env.exists(&ledger) {
            env.fetch::<CashbackLedger>(&ledger).accrued
        } else {
            0
        },
    }
}

/// Applies `changes` through the timelock.
fn apply_changes(env: &mut Env, changes: &[ConfigChange]) {
    let admin = env.admin.insecure_clone();
    for change in changes {
        env.propose(&admin, *change).unwrap();
    }
    env.warp_secs(TIMELOCK_SECS);
    for change in changes {
        env.execute(&admin, change).unwrap();
    }
}

fn set_fee(env: &mut Env, fee_bps: u16) {
    apply_changes(env, &[ConfigChange::FeeBps(fee_bps)]);
}

/// Swaps a mint's data for that of a Token-2022 mint carrying `extension`
/// (impossible on a real cluster; checks the defense-in-depth re-run).
fn inject_extension(env: &mut Env, mint: &Pubkey, extension: ExtensionType) {
    let decimals = env.mint_decimals(mint);
    let donor = env.create_mint_2022(None, &[extension]);
    env.set_mint_decimals(&donor, decimals);
    let mut account = env.svm.get_account(&donor).unwrap();
    let original = env.svm.get_account(mint).unwrap();
    account.data[..82].copy_from_slice(&original.data[..82]);
    env.svm.set_account(*mint, account).unwrap();
}

/// A ready market with deal 1 open on `side`.
fn open_market(class: AssetClass, side: DealSide) -> (Env, P2p) {
    let mut env = Env::initialized();
    let p = env.p2p(class, SPL_TOKEN, SPL_TOKEN);
    env.open_deal(&p, deal_id(1), side, ASSETS, PAYMENT).unwrap();
    (env, p)
}

/// Asserts a failed call moved nothing and left deal `id` as it was.
fn assert_untouched(env: &Env, p: &P2p, id: [u8; 16], before: Snapshot, status: Option<DealStatus>) {
    assert_eq!(snapshot(env, p, id), before, "a failed call must not move funds");
    match status {
        Some(s) => assert_eq!(env.fetch_deal(&p.deal(id)).status, s),
        None => assert!(!env.exists(&p.deal(id)), "no deal account"),
    }
}

// ---- success paths ----------------------------------------------------------

/// The P2P markets: issuer assets and BRL paid in USDT (fee on the payment
/// leg), and USDT sold for BRL (fee on the asset leg).
#[derive(Clone, Copy, Debug)]
enum Kind {
    PaidInUsdt(AssetClass),
    SellingUsdt,
}

const KINDS: [Kind; 4] = [
    Kind::PaidInUsdt(AssetClass::FixedIncome),
    Kind::PaidInUsdt(AssetClass::Credit),
    Kind::PaidInUsdt(AssetClass::Currency),
    Kind::SellingUsdt,
];

impl Kind {
    fn build(self, env: &mut Env, asset_program: Pubkey, payment_program: Pubkey) -> P2p {
        match self {
            Kind::PaidInUsdt(class) => env.p2p(class, asset_program, payment_program),
            Kind::SellingUsdt => env.p2p_selling_usdt(asset_program, payment_program),
        }
    }

    fn class(self) -> AssetClass {
        match self {
            Kind::PaidInUsdt(class) => class,
            Kind::SellingUsdt => AssetClass::Currency,
        }
    }

    /// (asset_amount, payment_amount): 0.5 USDT for 2.7 BRL when USDT is sold.
    fn amounts(self) -> (u64, u64) {
        match self {
            Kind::PaidInUsdt(_) => (ASSETS, PAYMENT),
            Kind::SellingUsdt => (500_000, 2_700_000),
        }
    }
}

fn assert_opens_and_executes(kind: Kind, asset_program: Pubkey, payment_program: Pubkey, side: DealSide) {
    let mut env = Env::initialized();
    let p = kind.build(&mut env, asset_program, payment_program);
    let class = kind.class();
    let (asset_amount, payment_amount) = kind.amounts();
    let id = deal_id(1);
    let start = snapshot(&env, &p, id);
    let expires_at = env.now() + DEAL_TTL;

    // open_deal: only the proposer's side moves, into the vault.
    let meta = env
        .open_deal(&p, id, side, asset_amount, payment_amount)
        .unwrap();
    let opened = snapshot(&env, &p, id);
    let (deposit, deposit_mint, deposit_program) = match side {
        DealSide::Sell => (asset_amount, p.asset_mint, asset_program),
        DealSide::Buy => (payment_amount, p.payment_mint, payment_program),
    };
    let mut expected = start;
    match side {
        DealSide::Sell => expected.proposer_asset -= asset_amount,
        DealSide::Buy => expected.proposer_payment -= payment_amount,
    }
    expected.deal_vault = deposit;
    assert_eq!(opened, expected);

    let deal_key = p.deal(id);
    let deal = env.fetch_deal(&deal_key);
    assert_eq!(deal.deal_id, id);
    assert_eq!(deal.proposer, p.proposer.pubkey());
    assert_eq!(deal.counterparty, p.counterparty.pubkey());
    assert_eq!((deal.asset_mint, deal.payment_mint), (p.asset_mint, p.payment_mint));
    assert_eq!((deal.asset_amount, deal.payment_amount), (asset_amount, payment_amount));
    assert_eq!(deal.proposer_side, side);
    assert_eq!((deal.fee_bps, deal.cashback_bps), (FEE_BPS, CASHBACK_BPS));
    assert_eq!(deal.expires_at, expires_at);
    assert_eq!(deal.status, DealStatus::Open);
    assert_eq!((deal.opened_at, deal.resolved_at), (env.now(), 0));

    // The vault is a token account of the deposited mint owned by the deal.
    let vault = env.token_account(&p.vault(id));
    assert_eq!((vault.mint, vault.owner), (deposit_mint, deal_key));
    assert_eq!(env.owner_of(&p.vault(id)), deposit_program);

    let e = &events::<DealOpened>(&meta)[0];
    assert_eq!((e.deal, e.deal_id, e.proposer), (deal_key, id, p.proposer.pubkey()));
    assert_eq!(e.counterparty, p.counterparty.pubkey());
    assert_eq!((e.asset_mint, e.payment_mint), (p.asset_mint, p.payment_mint));
    assert_eq!(
        (e.proposer_side, e.asset_amount, e.payment_amount),
        (side, asset_amount, payment_amount)
    );
    assert_eq!((e.fee_bps, e.cashback_bps), (FEE_BPS, CASHBACK_BPS));
    assert_eq!(e.expires_at, expires_at);

    // accept_deal: both legs, the fee and the cashback in one instruction.
    let vault_rent = env.lamports(&p.vault(id));
    let proposer_lamports = env.lamports(&p.proposer.pubkey());
    let meta = env.accept_deal(&p, id).unwrap();
    let after = snapshot(&env, &p, id);

    // The fee is on the USDT leg; whoever receives that leg pays it, and the
    // other leg moves in full (invariant 12).
    let usdt_is_asset = p.asset_mint == env.fee_mint;
    assert_eq!(usdt_is_asset, matches!(kind, Kind::SellingUsdt));
    let usdt_amount = if usdt_is_asset { asset_amount } else { payment_amount };
    let fee = expected_fee(usdt_amount, FEE_BPS);
    let cashback = expected_cashback(fee, class.cashback_eligible());
    let (asset_fee, payment_fee) = if usdt_is_asset { (fee, 0) } else { (0, fee) };
    let mut expected = opened;
    expected.deal_vault = 0;
    expected.fee_vault += fee;
    expected.cashback += cashback;
    match side {
        DealSide::Sell => {
            expected.counterparty_asset += asset_amount - asset_fee;
            expected.counterparty_payment -= payment_amount;
            expected.proposer_payment += payment_amount - payment_fee;
        }
        DealSide::Buy => {
            expected.counterparty_asset -= asset_amount;
            expected.counterparty_payment += payment_amount - payment_fee;
            expected.proposer_asset += asset_amount - asset_fee;
        }
    }
    assert_eq!(after, expected);
    // Invariant 1, on the USDT leg.
    assert!(fee > 0 && fee < usdt_amount);

    let ledger = env.fee_ledger(&env.fee_mint);
    assert_eq!((ledger.fees_collected, ledger.cashback_accrued), (fee, cashback));
    env.assert_vault_matches_ledger(&env.fee_mint);
    // Invariant 9: a Currency asset has no cashback ledger at all.
    assert_eq!(
        env.exists(&cashback_pda(&p.asset_mint, &env.fee_mint)),
        class.cashback_eligible()
    );
    // Invariant 12: the non-USDT currency never gets a fee vault or ledger.
    for mint in [p.asset_mint, p.payment_mint] {
        if mint != env.fee_mint {
            assert!(!env.exists(&fee_vault_pda(&mint)) && !env.exists(&fee_ledger_pda(&mint)));
        }
    }

    // Vault closed, rent back to the proposer (the counterparty paid the fee).
    assert!(!env.exists(&p.vault(id)));
    assert_eq!(env.lamports(&p.proposer.pubkey()), proposer_lamports + vault_rent);

    let deal = env.fetch_deal(&deal_key);
    assert_eq!(deal.status, DealStatus::Executed);
    assert_eq!(deal.resolved_at, env.now());

    let e = &events::<DealExecuted>(&meta)[0];
    assert_eq!((e.deal, e.deal_id, e.proposer), (deal_key, id, p.proposer.pubkey()));
    assert_eq!(e.counterparty, p.counterparty.pubkey());
    assert_eq!((e.asset_mint, e.payment_mint), (p.asset_mint, p.payment_mint));
    assert_eq!(
        (e.proposer_side, e.asset_amount, e.payment_amount),
        (side, asset_amount, payment_amount)
    );
    assert_eq!(e.fee_mint, env.fee_mint);
    assert_eq!((e.fee, e.cashback, e.vault_excess), (fee, cashback, 0));
}

#[test]
fn deals_execute_for_every_market_side_and_token_program() {
    for kind in KINDS {
        for side in SIDES {
            for (asset_program, payment_program) in PROGRAMS {
                assert_opens_and_executes(kind, asset_program, payment_program, side);
            }
        }
    }
}

/// BRL -> USDT: the fee is charged in USDT, on the payment leg, so the BRL
/// seller pays it; nothing accrues as cashback.
#[test]
fn currency_deal_for_usdt_charges_the_usdt_receiver() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::Currency, SPL_TOKEN, TOKEN_2022);
    env.open_deal(&p, deal_id(1), DealSide::Sell, 500_000, 100_000)
        .unwrap();
    let a = env.accept_deal_accounts(&p, deal_id(1));
    assert!(a.cashback_ledger.is_none(), "no ledger can exist for a Currency asset");
    let meta = env.accept_deal(&p, deal_id(1)).unwrap();
    let e = &events::<DealExecuted>(&meta)[0];
    let fee = expected_fee(100_000, FEE_BPS);
    assert_eq!((e.fee_mint, e.fee, e.cashback), (p.payment_mint, fee, 0));
    // The BRL buyer gets all the BRL; the BRL seller gets USDT minus the fee.
    assert_eq!(env.balance(&p.counterparty_asset), P2P_ASSETS + 500_000);
    assert_eq!(env.balance(&p.proposer_payment), P2P_PAYMENT + 100_000 - fee);
    assert_eq!(env.fee_ledger(&p.payment_mint).fees_collected, fee);
    // BRL has no fee vault at all.
    assert!(!env.exists(&fee_vault_pda(&p.asset_mint)));
}

/// USDT -> BRL: the currency sold is USDT, so the fee is on the asset leg and
/// the party buying USDT pays it; the BRL leg moves in full.
#[test]
fn currency_deal_selling_usdt_charges_the_usdt_buyer() {
    for side in SIDES {
        let mut env = Env::initialized();
        let p = env.p2p_selling_usdt(SPL_TOKEN, TOKEN_2022);
        let (usdt, brl) = (500_000, 2_700_000);
        env.open_deal(&p, deal_id(1), side, usdt, brl).unwrap();
        let meta = env.accept_deal(&p, deal_id(1)).unwrap();
        let fee = expected_fee(usdt, FEE_BPS);
        let e = &events::<DealExecuted>(&meta)[0];
        assert_eq!((e.fee_mint, e.fee, e.cashback), (p.asset_mint, fee, 0));
        let (usdt_buyer, brl_seller) = match side {
            // The proposer sells USDT: the counterparty buys it and pays BRL.
            DealSide::Sell => (p.counterparty_asset, p.proposer_payment),
            DealSide::Buy => (p.proposer_asset, p.counterparty_payment),
        };
        assert_eq!(env.balance(&usdt_buyer), P2P_ASSETS + usdt - fee);
        assert_eq!(env.balance(&brl_seller), P2P_PAYMENT + brl);
        assert_eq!(env.balance(&fee_vault_pda(&p.asset_mint)), fee);
        assert_eq!(env.fee_ledger(&p.asset_mint).fees_collected, fee);
        assert!(!env.exists(&fee_vault_pda(&p.payment_mint)));
    }
}

/// Fees are only ever charged in USDT: a deal with no USDT leg is rejected
/// (BRL <-> EUR, a bond paid in BRL), at open and at accept.
#[test]
fn deal_needs_a_usdt_leg() {
    let mut env = Env::initialized();
    env.fee_currency(SPL_TOKEN);
    let brl = env.listed_currency(SPL_TOKEN);
    let eur = env.listed_currency(SPL_TOKEN);
    let (bond, issuer) = env.listed_issuer_asset(SPL_TOKEN, AssetClass::FixedIncome);
    for (asset, payment) in [(brl, eur), (eur, brl), (bond, brl)] {
        let p = env.p2p_parties(asset, payment, issuer.insecure_clone());
        for side in SIDES {
            let before = snapshot(&env, &p, deal_id(1));
            let res = env.open_deal(&p, deal_id(1), side, ASSETS, PAYMENT);
            assert_custom_err(res, ExchangeError::NotFeeMint);
            assert_untouched(&env, &p, deal_id(1), before, None);
        }
    }
    assert!(!env.exists(&fee_vault_pda(&brl)) && !env.exists(&fee_vault_pda(&eur)));
}

/// Allowlisted metadata extensions on both mints do not get in the way.
#[test]
fn deal_with_token_2022_metadata_mints() {
    let exts = [ExtensionType::MetadataPointer, ExtensionType::TokenMetadata];
    let mut env = Env::initialized_with_fee_mint(|env| env.create_mint_2022(None, &exts));
    let issuer = env.funded_keypair();
    let asset_mint = env.create_mint_2022(Some(issuer.pubkey()), &exts);
    env.set_mint_decimals(&asset_mint, ASSET_DECIMALS);
    env.submit_listing(&issuer, asset_mint, AssetClass::Credit, DOC_HASH)
        .unwrap();
    let authority = env.listing_authority.insecure_clone();
    env.approve(&authority, asset_mint).unwrap();
    let payment_mint = env.fee_currency(TOKEN_2022);
    let payer = env.admin.insecure_clone();
    env.init_cashback_ledger(&payer, asset_mint, payment_mint)
        .unwrap();
    let p = env.p2p_parties(asset_mint, payment_mint, issuer);
    for (n, side) in SIDES.into_iter().enumerate() {
        let id = deal_id(n as u128 + 1);
        env.open_deal(&p, id, side, ASSETS, PAYMENT).unwrap();
        env.accept_deal(&p, id).unwrap();
    }
}

/// The fee is part of the agreed terms: `accept_deal` charges the fee and
/// cashback rates stored at `open_deal`, whatever the config says by then. A
/// deal opened after the change gets the new rates.
#[test]
fn fee_is_fixed_at_open() {
    for side in SIDES {
        let (mut env, p) = open_market(AssetClass::FixedIncome, side);
        // A raise to the maximum fee (50 bps).
        let (new_fee, new_cashback) = (MAX_FEE_BPS, 5_000);
        apply_changes(
            &mut env,
            &[ConfigChange::FeeBps(new_fee), ConfigChange::CashbackBps(new_cashback)],
        );
        let ledger = cashback_pda(&p.asset_mint, &env.fee_mint);

        // Deal 1, opened before the change: the old rates.
        let deal = env.fetch_deal(&p.deal(deal_id(1)));
        assert_eq!((deal.fee_bps, deal.cashback_bps), (FEE_BPS, CASHBACK_BPS));
        let before = snapshot(&env, &p, deal_id(1));
        let meta = env.accept_deal(&p, deal_id(1)).unwrap();
        let fee = expected_fee(PAYMENT, FEE_BPS);
        let cashback = expected_cashback(fee, true);
        let e = &events::<DealExecuted>(&meta)[0];
        assert_eq!((e.fee, e.cashback), (fee, cashback));
        let after = snapshot(&env, &p, deal_id(1));
        assert_eq!(after.fee_vault, before.fee_vault + fee);
        assert_eq!(after.cashback, before.cashback + cashback);
        let seller_payment = match side {
            DealSide::Sell => after.proposer_payment,
            DealSide::Buy => after.counterparty_payment,
        };
        assert_eq!(seller_payment, P2P_PAYMENT + PAYMENT - fee);

        // Deal 2, opened after the change: the new rates.
        let meta = env
            .open_deal(&p, deal_id(2), side, ASSETS, PAYMENT)
            .unwrap();
        let o = &events::<DealOpened>(&meta)[0];
        assert_eq!((o.fee_bps, o.cashback_bps), (new_fee, new_cashback));
        let deal = env.fetch_deal(&p.deal(deal_id(2)));
        assert_eq!((deal.fee_bps, deal.cashback_bps), (new_fee, new_cashback));
        let meta = env.accept_deal(&p, deal_id(2)).unwrap();
        let fee2 = expected_fee(PAYMENT, new_fee);
        let cashback2 = fee2 * new_cashback as u64 / BPS_DENOMINATOR as u64;
        let e = &events::<DealExecuted>(&meta)[0];
        assert_eq!((e.fee, e.cashback), (fee2, cashback2));
        let ledger_total = env.fetch::<CashbackLedger>(&ledger).accrued;
        assert_eq!(ledger_total, cashback + cashback2);
        let fees = env.fee_ledger(&env.fee_mint);
        assert_eq!(fees.fees_collected, fee + fee2);
        env.assert_vault_matches_ledger(&env.fee_mint);
    }
}

/// A fee cut after open does not lower the fee either.
#[test]
fn fee_cut_after_open_does_not_apply() {
    let (mut env, p) = open_market(AssetClass::Credit, DealSide::Buy);
    set_fee(&mut env, 0);
    let meta = env.accept_deal(&p, deal_id(1)).unwrap();
    assert_eq!(events::<DealExecuted>(&meta)[0].fee, expected_fee(PAYMENT, FEE_BPS));
}

#[test]
fn zero_fee_skips_the_fee_leg() {
    let mut env = Env::initialized();
    set_fee(&mut env, 0);
    let p = env.p2p(AssetClass::Credit, SPL_TOKEN, SPL_TOKEN);
    env.open_deal(&p, deal_id(1), DealSide::Buy, ASSETS, PAYMENT)
        .unwrap();
    let meta = env.accept_deal(&p, deal_id(1)).unwrap();
    let e = &events::<DealExecuted>(&meta)[0];
    assert_eq!((e.fee, e.cashback), (0, 0));
    assert_eq!(env.balance(&p.counterparty_payment), P2P_PAYMENT + PAYMENT);
    assert_eq!(env.balance(&fee_vault_pda(&p.payment_mint)), 0);
}

#[test]
fn cancel_refunds_the_proposer() {
    for side in SIDES {
        let (mut env, p) = open_market(AssetClass::FixedIncome, side);
        let id = deal_id(1);
        let vault_rent = env.lamports(&p.vault(id));
        let lamports = env.lamports(&p.proposer.pubkey());
        let meta = env.cancel_deal(&p, id).unwrap();
        // Back where it started; the counterparty and the fee vault untouched.
        let s = snapshot(&env, &p, id);
        assert_eq!((s.proposer_asset, s.proposer_payment), (P2P_ASSETS, P2P_PAYMENT));
        assert_eq!((s.counterparty_asset, s.counterparty_payment), (P2P_ASSETS, P2P_PAYMENT));
        assert_eq!((s.fee_vault, s.deal_vault, s.cashback), (0, 0, 0));
        assert!(!env.exists(&p.vault(id)));
        // Rent back, minus the fee of the proposer's own transaction.
        assert_eq!(env.lamports(&p.proposer.pubkey()), lamports + vault_rent - 5_000);
        let deal = env.fetch_deal(&p.deal(id));
        assert_eq!((deal.status, deal.resolved_at), (DealStatus::Cancelled, env.now()));
        let e = &events::<DealCancelled>(&meta)[0];
        let (mint, amount) = match side {
            DealSide::Sell => (p.asset_mint, ASSETS),
            DealSide::Buy => (p.payment_mint, PAYMENT),
        };
        assert_eq!((e.deal, e.deal_id, e.proposer), (p.deal(id), id, p.proposer.pubkey()));
        assert_eq!((e.deposit_mint, e.deposit_amount, e.vault_excess), (mint, amount, 0));
    }
}

#[test]
fn anyone_can_expire_after_expires_at() {
    for side in SIDES {
        let (mut env, p) = open_market(AssetClass::Credit, side);
        let id = deal_id(1);
        env.warp_secs(DEAL_TTL);
        let vault_rent = env.lamports(&p.vault(id));
        let lamports = env.lamports(&p.proposer.pubkey());
        let meta = env.expire_deal(&p, id).unwrap();
        let s = snapshot(&env, &p, id);
        assert_eq!((s.proposer_asset, s.proposer_payment), (P2P_ASSETS, P2P_PAYMENT));
        assert_eq!((s.counterparty_asset, s.counterparty_payment), (P2P_ASSETS, P2P_PAYMENT));
        assert!(!env.exists(&p.vault(id)));
        // The stranger paid the transaction; the proposer gets the full rent.
        assert_eq!(env.lamports(&p.proposer.pubkey()), lamports + vault_rent);
        let deal = env.fetch_deal(&p.deal(id));
        assert_eq!((deal.status, deal.resolved_at), (DealStatus::Expired, env.now()));
        let e = &events::<DealExpired>(&meta)[0];
        let amount = if side == DealSide::Sell { ASSETS } else { PAYMENT };
        assert_eq!((e.deal, e.proposer, e.deposit_amount), (p.deal(id), p.proposer.pubkey(), amount));
        assert_eq!(e.vault_excess, 0);
    }
}

// ---- expiry (invariant 7) -----------------------------------------------------

/// Live while `now < expires_at`; expirable from `expires_at` on. No gap, no
/// overlap.
#[test]
fn expiry_boundary_is_exact() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let expires_at = env.now() + 100;
    for n in 1..=3 {
        env.open_deal_at(&p, deal_id(n), DealSide::Sell, ASSETS, PAYMENT, expires_at)
            .unwrap();
    }
    env.warp_secs(99);
    let res = env.expire_deal(&p, deal_id(1));
    assert_custom_err(res, ExchangeError::DealNotExpired);
    env.accept_deal(&p, deal_id(1)).unwrap();

    env.warp_secs(1); // now == expires_at
    let before = snapshot(&env, &p, deal_id(2));
    let res = env.accept_deal(&p, deal_id(2));
    assert_custom_err(res, ExchangeError::DealExpired);
    assert_untouched(&env, &p, deal_id(2), before, Some(DealStatus::Open));
    env.expire_deal(&p, deal_id(2)).unwrap();

    env.warp_secs(1_000_000);
    let res = env.accept_deal(&p, deal_id(3));
    assert_custom_err(res, ExchangeError::DealExpired);
    env.expire_deal(&p, deal_id(3)).unwrap();
}

#[test]
fn proposer_can_cancel_an_expired_deal() {
    let (mut env, p) = open_market(AssetClass::FixedIncome, DealSide::Buy);
    env.warp_secs(DEAL_TTL + 1);
    env.cancel_deal(&p, deal_id(1)).unwrap();
    assert_eq!(env.fetch_deal(&p.deal(deal_id(1))).status, DealStatus::Cancelled);
}

#[test]
fn rejects_expiry_outside_the_window() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    env.warp_secs(1_000);
    let now = env.now();
    let before = snapshot(&env, &p, deal_id(1));
    for expires_at in [now - 1, now, now + MAX_DEAL_DURATION_SECS + 1, i64::MAX] {
        let res = env.open_deal_at(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT, expires_at);
        assert_custom_err(res, ExchangeError::InvalidExpiry);
    }
    assert_untouched(&env, &p, deal_id(1), before, None);
    env.open_deal_at(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT, now + 1)
        .unwrap();
    env.open_deal_at(&p, deal_id(2), DealSide::Sell, ASSETS, PAYMENT, now + MAX_DEAL_DURATION_SECS)
        .unwrap();
}

// ---- replay (the status, and the closed vault) ---------------------------------

/// Once a deal leaves `Open` nothing can touch it again. The vault is closed
/// at that point, so the account checks fail before the status check.
#[test]
fn resolved_deals_cannot_be_replayed() {
    for first in ["accept", "cancel", "expire"] {
        let (mut env, p) = open_market(AssetClass::FixedIncome, DealSide::Sell);
        let id = deal_id(1);
        if first == "expire" {
            env.warp_secs(DEAL_TTL);
        }
        match first {
            "accept" => env.accept_deal(&p, id).unwrap(),
            "cancel" => env.cancel_deal(&p, id).unwrap(),
            _ => env.expire_deal(&p, id).unwrap(),
        };
        let status = env.fetch_deal(&p.deal(id)).status;
        let before = snapshot(&env, &p, id);
        let accept = env.accept_deal(&p, id);
        assert_custom_err(accept, AnchorError::AccountNotInitialized);
        let cancel = env.cancel_deal(&p, id);
        assert_custom_err(cancel, AnchorError::AccountNotInitialized);
        env.warp_secs(DEAL_TTL);
        let expire = env.expire_deal(&p, id);
        assert_custom_err(expire, AnchorError::AccountNotInitialized);
        assert_untouched(&env, &p, id, before, Some(status));
        // The deal id cannot be reopened by the same proposer.
        let res = env.open_deal(&p, id, DealSide::Sell, ASSETS, PAYMENT);
        assert!(res.is_err(), "deal id reopened after {first}");
        assert_eq!(env.fetch_deal(&p.deal(id)).status, status);
    }
}

/// Deal ids are scoped to the proposer.
#[test]
fn same_deal_id_for_two_proposers() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::Credit, SPL_TOKEN, SPL_TOKEN);
    let issuer = p.issuer.insecure_clone();
    let q = env.p2p_parties(p.asset_mint, p.payment_mint, issuer);
    env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    env.open_deal(&q, deal_id(1), DealSide::Buy, ASSETS, PAYMENT)
        .unwrap();
    env.accept_deal(&q, deal_id(1)).unwrap();
    env.accept_deal(&p, deal_id(1)).unwrap();
}

// ---- pause and listing status (spec §3.3 pause principle) ----------------------

#[test]
fn pause_blocks_open_and_accept_but_never_refunds() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    for n in 1..=2 {
        env.open_deal(&p, deal_id(n), DealSide::Sell, ASSETS, PAYMENT)
            .unwrap();
    }
    let short = env.now() + 10;
    env.open_deal_at(&p, deal_id(3), DealSide::Sell, ASSETS, PAYMENT, short)
        .unwrap();
    let admin = env.admin.insecure_clone();
    env.pause(&admin).unwrap();

    let before = snapshot(&env, &p, deal_id(4));
    let res = env.open_deal(&p, deal_id(4), DealSide::Sell, ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::Paused);
    assert_untouched(&env, &p, deal_id(4), before, None);

    let before = snapshot(&env, &p, deal_id(1));
    let res = env.accept_deal(&p, deal_id(1));
    assert_custom_err(res, ExchangeError::Paused);
    assert_untouched(&env, &p, deal_id(1), before, Some(DealStatus::Open));

    // Taking back one's own deposit is never paused.
    env.cancel_deal(&p, deal_id(2)).unwrap();
    env.warp_secs(10);
    env.expire_deal(&p, deal_id(3)).unwrap();

    env.unpause(&admin).unwrap();
    env.accept_deal(&p, deal_id(1)).unwrap();
    env.open_deal(&p, deal_id(4), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    env.accept_deal(&p, deal_id(4)).unwrap();
}

#[test]
fn suspended_or_delisted_asset_blocks_open_and_accept_but_never_refunds() {
    for delist in [false, true] {
        let mut env = Env::initialized();
        let p = env.p2p(AssetClass::Credit, SPL_TOKEN, SPL_TOKEN);
        for n in 1..=2 {
            env.open_deal(&p, deal_id(n), DealSide::Buy, ASSETS, PAYMENT)
                .unwrap();
        }
        let short = env.now() + 10;
        env.open_deal_at(&p, deal_id(3), DealSide::Buy, ASSETS, PAYMENT, short)
            .unwrap();
        let authority = env.listing_authority.insecure_clone();
        if delist {
            env.delist(&authority, p.asset_mint).unwrap();
        } else {
            env.suspend(&authority, p.asset_mint).unwrap();
        }
        let res = env.open_deal(&p, deal_id(4), DealSide::Buy, ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::ListingNotApproved);
        let before = snapshot(&env, &p, deal_id(1));
        let res = env.accept_deal(&p, deal_id(1));
        assert_custom_err(res, ExchangeError::ListingNotApproved);
        assert_untouched(&env, &p, deal_id(1), before, Some(DealStatus::Open));
        env.cancel_deal(&p, deal_id(2)).unwrap();
        env.warp_secs(10);
        env.expire_deal(&p, deal_id(3)).unwrap();
        if delist {
            // Delisting is terminal: the deal can only be refunded.
            env.cancel_deal(&p, deal_id(1)).unwrap();
        } else {
            // Reinstated, the open deal can execute again.
            env.approve(&authority, p.asset_mint).unwrap();
            env.accept_deal(&p, deal_id(1)).unwrap();
        }
    }
}

#[test]
fn suspended_payment_currency_blocks_open_and_accept_but_never_refunds() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::Currency, SPL_TOKEN, SPL_TOKEN);
    for n in 1..=3 {
        env.open_deal(&p, deal_id(n), DealSide::Buy, ASSETS, PAYMENT)
            .unwrap();
    }
    let authority = env.listing_authority.insecure_clone();
    env.suspend(&authority, p.payment_mint).unwrap();
    let res = env.open_deal(&p, deal_id(4), DealSide::Buy, ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::InvalidPaymentMint);
    let res = env.accept_deal(&p, deal_id(1));
    assert_custom_err(res, ExchangeError::InvalidPaymentMint);
    env.cancel_deal(&p, deal_id(2)).unwrap();
    env.warp_secs(DEAL_TTL);
    env.expire_deal(&p, deal_id(3)).unwrap();
}

#[test]
fn pending_asset_listing_cannot_open() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let asset_mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
    env.submit_listing(&issuer, asset_mint, AssetClass::FixedIncome, DOC_HASH)
        .unwrap();
    let payment_mint = env.fee_currency(SPL_TOKEN);
    let p = env.p2p_parties(asset_mint, payment_mint, issuer);
    let res = env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::ListingNotApproved);
}

// ---- market rules (invariants 8 and 10) ----------------------------------------

#[test]
fn equity_is_never_traded_p2p() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::Equity, SPL_TOKEN, SPL_TOKEN);
    for side in SIDES {
        let before = snapshot(&env, &p, deal_id(1));
        let res = env.open_deal(&p, deal_id(1), side, ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::WrongTradingMode);
        assert_untouched(&env, &p, deal_id(1), before, None);
    }
}

/// Only an `Approved` `Currency` listing is a payment mint.
#[test]
fn payment_must_be_an_approved_currency() {
    let mut env = Env::initialized();
    let (equity, _) = env.listed_equity(SPL_TOKEN);
    let (bond, issuer) = env.listed_issuer_asset(SPL_TOKEN, AssetClass::FixedIncome);
    let (credit, _) = env.listed_issuer_asset(SPL_TOKEN, AssetClass::Credit);
    for payment in [equity, credit] {
        let p = env.p2p_parties(bond, payment, issuer.insecure_clone());
        let res = env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::InvalidPaymentMint);
    }
    // A mint with no listing at all.
    let unlisted = env.create_mint(SPL_TOKEN, None);
    let p = env.p2p_parties(bond, unlisted, issuer);
    let res = env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT);
    assert_custom_err(res, AnchorError::AccountNotInitialized);
}

#[test]
fn currency_cannot_be_traded_against_itself() {
    let mut env = Env::initialized();
    let usd = env.listed_currency(SPL_TOKEN);
    let issuer = env.funded_keypair();
    let p = env.p2p_parties(usd, usd, issuer);
    let res = env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::SameMint);
}

#[test]
fn listing_must_match_its_mint() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let (other, _) = env.listed_issuer_asset(SPL_TOKEN, AssetClass::FixedIncome);
    let mut a = env.open_deal_accounts(&p, deal_id(1), DealSide::Sell);
    a.asset_listing = listing_pda(&other);
    let proposer = p.proposer.insecure_clone();
    let terms = p.terms(DealSide::Sell, ASSETS, PAYMENT);
    let expires_at = env.now() + DEAL_TTL;
    let res = env.open_deal_with(&proposer, &a, deal_id(1), terms, expires_at);
    assert_custom_err(res, AnchorError::ConstraintSeeds);
}

// ---- mint policy, re-run as defense in depth -------------------------------------

#[test]
fn rechecks_extension_allowlist_on_both_mints() {
    for asset_side in [true, false] {
        // At open.
        let mut env = Env::initialized();
        let p = env.p2p(AssetClass::FixedIncome, TOKEN_2022, TOKEN_2022);
        let mint = if asset_side { p.asset_mint } else { p.payment_mint };
        inject_extension(&mut env, &mint, ExtensionType::PermanentDelegate);
        let res = env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::UnsupportedMintExtension);

        // At accept; the proposer can still take its deposit back.
        let (mut env, p) = {
            let mut env = Env::initialized();
            let p = env.p2p(AssetClass::FixedIncome, TOKEN_2022, TOKEN_2022);
            for n in 1..=2 {
                env.open_deal(&p, deal_id(n), DealSide::Buy, ASSETS, PAYMENT)
                    .unwrap();
            }
            (env, p)
        };
        let mint = if asset_side { p.asset_mint } else { p.payment_mint };
        inject_extension(&mut env, &mint, ExtensionType::PermanentDelegate);
        let res = env.accept_deal(&p, deal_id(1));
        assert_custom_err(res, ExchangeError::UnsupportedMintExtension);
        env.cancel_deal(&p, deal_id(1)).unwrap();
        env.warp_secs(DEAL_TTL);
        env.expire_deal(&p, deal_id(2)).unwrap();
    }
}

#[test]
fn rechecks_issuer_asset_freeze_authority() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::Credit, SPL_TOKEN, SPL_TOKEN);
    env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    env.set_freeze_authority(&p.asset_mint, Some(Pubkey::new_unique()));
    let res = env.open_deal(&p, deal_id(2), DealSide::Sell, ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::FreezeAuthorityNotAllowed);
    let res = env.accept_deal(&p, deal_id(1));
    assert_custom_err(res, ExchangeError::FreezeAuthorityNotAllowed);
    env.cancel_deal(&p, deal_id(1)).unwrap();
}

/// Currencies may have a freeze authority (USDC does), on either leg.
#[test]
fn currency_with_freeze_authority_trades() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::Currency, SPL_TOKEN, SPL_TOKEN);
    env.set_freeze_authority(&p.asset_mint, Some(Pubkey::new_unique()));
    env.set_freeze_authority(&p.payment_mint, Some(Pubkey::new_unique()));
    env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    env.accept_deal(&p, deal_id(1)).unwrap();
}

// ---- terms -----------------------------------------------------------------------

#[test]
fn rejects_zero_amounts() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let before = snapshot(&env, &p, deal_id(1));
    for side in SIDES {
        for (a, pay) in [(0, PAYMENT), (ASSETS, 0), (0, 0)] {
            let res = env.open_deal(&p, deal_id(1), side, a, pay);
            assert_custom_err(res, ExchangeError::ZeroAmount);
        }
    }
    assert_untouched(&env, &p, deal_id(1), before, None);
}

/// An all-zero deal id is an unset field, never a real negotiation UUID:
/// `open_deal` refuses it and creates neither the deal nor its vault.
#[test]
fn rejects_zero_deal_id() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let zero = [0u8; 16];
    assert_eq!(deal_id(0), zero);
    let before = snapshot(&env, &p, zero);
    for side in SIDES {
        let res = env.open_deal(&p, zero, side, ASSETS, PAYMENT);
        assert_custom_err(res, ExchangeError::ZeroDealId);
    }
    assert_untouched(&env, &p, zero, before, None);
    assert!(!env.exists(&p.vault(zero)), "no deal vault");

    // The pause is checked first, as for every other rejection.
    let admin = env.admin.insecure_clone();
    env.pause(&admin).unwrap();
    let res = env.open_deal(&p, zero, DealSide::Sell, ASSETS, PAYMENT);
    assert_custom_err(res, ExchangeError::Paused);
    env.unpause(&admin).unwrap();
    assert_untouched(&env, &p, zero, before, None);

    // Only the all-zero id is refused: one set bit anywhere is a valid id.
    let mut low = zero;
    low[0] = 1;
    let mut high = zero;
    high[15] = 0x80;
    for (id, side) in [(low, DealSide::Sell), (high, DealSide::Buy)] {
        env.open_deal(&p, id, side, ASSETS, PAYMENT).unwrap();
        env.accept_deal(&p, id).unwrap();
    }
}

#[test]
fn counterparty_must_be_named_and_different() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let a = env.open_deal_accounts(&p, deal_id(1), DealSide::Sell);
    let proposer = p.proposer.insecure_clone();
    let expires_at = env.now() + DEAL_TTL;
    let mut terms = p.terms(DealSide::Sell, ASSETS, PAYMENT);
    terms.counterparty = proposer.pubkey();
    let res = env.open_deal_with(&proposer, &a, deal_id(1), terms, expires_at);
    assert_custom_err(res, ExchangeError::SelfTrade);
    terms.counterparty = Pubkey::default();
    let res = env.open_deal_with(&proposer, &a, deal_id(1), terms, expires_at);
    assert_custom_err(res, ExchangeError::InvalidAddress);
}

/// 30 bps of 1 rounds up to a fee of 1: the seller would get nothing, so
/// `open_deal` rejects it. The check is final: a deal opened at fee 0 keeps
/// that fee, so a later fee raise cannot make it fail at accept.
#[test]
fn rejects_deal_that_leaves_the_seller_nothing() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let before = snapshot(&env, &p, deal_id(1));
    let res = env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, 1);
    assert_custom_err(res, ExchangeError::ZeroSellerProceeds);
    assert_untouched(&env, &p, deal_id(1), before, None);

    set_fee(&mut env, 0);
    env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, 1)
        .unwrap();
    set_fee(&mut env, FEE_BPS);
    let meta = env.accept_deal(&p, deal_id(1)).unwrap();
    assert_eq!(events::<DealExecuted>(&meta)[0].fee, 0);
    assert_eq!(env.balance(&p.proposer_payment), P2P_PAYMENT + 1);
}

#[test]
fn deposit_account_must_hold_the_escrowed_mint() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let proposer = p.proposer.insecure_clone();
    let expires_at = env.now() + DEAL_TTL;
    for side in SIDES {
        let terms = p.terms(side, ASSETS, PAYMENT);
        // The proposer's account of the other mint.
        let mut a = env.open_deal_accounts(&p, deal_id(1), side);
        a.proposer_deposit = p.deposit_account(match side {
            DealSide::Sell => DealSide::Buy,
            DealSide::Buy => DealSide::Sell,
        });
        let res = env.open_deal_with(&proposer, &a, deal_id(1), terms, expires_at);
        assert_custom_err(res, ExchangeError::InvalidTokenAccount);
        // Someone else's account of the right mint.
        a.proposer_deposit = match side {
            DealSide::Sell => p.counterparty_asset,
            DealSide::Buy => p.counterparty_payment,
        };
        let res = env.open_deal_with(&proposer, &a, deal_id(1), terms, expires_at);
        assert_custom_err(res, AnchorError::ConstraintTokenOwner);
    }
}

#[test]
fn proposer_without_funds_cannot_open() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    let res = env.open_deal(&p, deal_id(1), DealSide::Sell, P2P_ASSETS + 1, PAYMENT);
    assert!(res.is_err());
    assert!(!env.exists(&p.deal(deal_id(1))));
    assert!(!env.exists(&p.vault(deal_id(1))));
}

/// Lamports sent to the vault address before `open_deal` do not block it.
#[test]
fn prefunded_vault_address_does_not_block_open() {
    let mut env = Env::initialized();
    let p = env.p2p(AssetClass::FixedIncome, SPL_TOKEN, SPL_TOKEN);
    // Rent-exempt for an empty account, so the runtime accepts the transfer.
    env.svm.airdrop(&p.vault(deal_id(1)), 1_000_000).unwrap();
    env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    assert_eq!(env.balance(&p.vault(deal_id(1))), ASSETS);
    env.accept_deal(&p, deal_id(1)).unwrap();
}

// ---- accept_deal authorization and accounts ---------------------------------------

#[test]
fn only_the_named_counterparty_can_accept() {
    let (mut env, p) = open_market(AssetClass::FixedIncome, DealSide::Sell);
    let id = deal_id(1);
    let before = snapshot(&env, &p, id);
    let stranger = env.funded_keypair();
    for intruder in [stranger, p.proposer.insecure_clone(), env.admin.insecure_clone()] {
        let mut a = env.accept_deal_accounts(&p, id);
        a.counterparty = intruder.pubkey();
        a.counterparty_asset =
            env.create_token_account(SPL_TOKEN, p.asset_mint, intruder.pubkey(), 0);
        a.counterparty_payment =
            env.create_token_account(SPL_TOKEN, p.payment_mint, intruder.pubkey(), P2P_PAYMENT);
        let res = env.accept_deal_with(&intruder, &a);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_untouched(&env, &p, id, before, Some(DealStatus::Open));
    env.accept_deal(&p, id).unwrap();
}

#[test]
fn accept_accounts_must_match_the_deal() {
    let (mut env, p) = open_market(AssetClass::FixedIncome, DealSide::Sell);
    let id = deal_id(1);
    let counterparty = p.counterparty.insecure_clone();
    let before = snapshot(&env, &p, id);

    // Another proposer (with its own payment account).
    let fake = env.funded_keypair();
    let mut a = env.accept_deal_accounts(&p, id);
    a.proposer = fake.pubkey();
    a.proposer_proceeds = env.create_token_account(SPL_TOKEN, p.payment_mint, fake.pubkey(), 0);
    let res = env.accept_deal_with(&counterparty, &a);
    assert_custom_err(res, ExchangeError::DealAccountMismatch);

    // Another asset mint (with its own listing).
    let (other, _) = env.listed_issuer_asset(SPL_TOKEN, AssetClass::FixedIncome);
    let mut a = env.accept_deal_accounts(&p, id);
    a.asset_mint = other;
    a.asset_listing = listing_pda(&other);
    a.counterparty_asset = env.create_token_account(SPL_TOKEN, other, counterparty.pubkey(), 0);
    let res = env.accept_deal_with(&counterparty, &a);
    assert_custom_err(res, ExchangeError::DealAccountMismatch);

    // Another payment currency.
    let other = env.listed_currency(SPL_TOKEN);
    let mut a = env.accept_deal_accounts(&p, id);
    a.payment_mint = other;
    a.payment_listing = listing_pda(&other);
    a.counterparty_payment =
        env.create_token_account(SPL_TOKEN, other, counterparty.pubkey(), P2P_PAYMENT);
    a.proposer_proceeds = env.create_token_account(SPL_TOKEN, other, p.proposer.pubkey(), 0);
    let res = env.accept_deal_with(&counterparty, &a);
    assert_custom_err(res, ExchangeError::DealAccountMismatch);

    // Proceeds to the proposer's account of the wrong mint.
    let mut a = env.accept_deal_accounts(&p, id);
    a.proposer_proceeds = p.proposer_asset;
    let res = env.accept_deal_with(&counterparty, &a);
    assert_custom_err(res, ExchangeError::InvalidTokenAccount);

    // Proceeds to an account the proposer does not own.
    let mut a = env.accept_deal_accounts(&p, id);
    a.proposer_proceeds = env.create_token_account(SPL_TOKEN, p.payment_mint, counterparty.pubkey(), 0);
    let res = env.accept_deal_with(&counterparty, &a);
    assert_custom_err(res, AnchorError::ConstraintTokenOwner);

    // A fee vault that is not the fee mint's PDA, even owned by the vault
    // authority and of the right mint.
    let mut a = env.accept_deal_accounts(&p, id);
    a.fee_vault = env.create_token_account(SPL_TOKEN, p.payment_mint, vault_authority(), 0);
    let res = env.accept_deal_with(&counterparty, &a);
    assert_custom_err(res, AnchorError::ConstraintSeeds);

    // Another deal's vault.
    env.open_deal(&p, deal_id(2), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    let mut a = env.accept_deal_accounts(&p, id);
    a.deal_vault = p.vault(deal_id(2));
    let res = env.accept_deal_with(&counterparty, &a);
    assert_custom_err(res, AnchorError::ConstraintSeeds);

    let mut expected = before;
    expected.proposer_asset -= ASSETS; // deal 2's deposit
    assert_untouched(&env, &p, id, expected, Some(DealStatus::Open));
    env.accept_deal(&p, id).unwrap();
}

#[test]
fn eligible_listing_needs_its_cashback_ledger() {
    let mut env = Env::initialized();
    let (asset_mint, issuer) = env.listed_issuer_asset(SPL_TOKEN, AssetClass::FixedIncome);
    let payment_mint = env.fee_currency(SPL_TOKEN);
    let p = env.p2p_parties(asset_mint, payment_mint, issuer);
    env.open_deal(&p, deal_id(1), DealSide::Sell, ASSETS, PAYMENT)
        .unwrap();
    let a = env.accept_deal_accounts(&p, deal_id(1));
    assert!(a.cashback_ledger.is_none());
    let res = env.accept_deal(&p, deal_id(1));
    assert_custom_err(res, ExchangeError::MissingCashbackLedger);
    // Permissionless: the counterparty creates it and accepts.
    let counterparty = p.counterparty.insecure_clone();
    env.init_cashback_ledger(&counterparty, asset_mint, payment_mint)
        .unwrap();
    env.accept_deal(&p, deal_id(1)).unwrap();
    let ledger: CashbackLedger = env.fetch(&cashback_pda(&asset_mint, &payment_mint));
    assert_eq!(ledger.accrued, expected_cashback(expected_fee(PAYMENT, FEE_BPS), true));
}

/// A failed counterparty leg reverts the whole instruction.
#[test]
fn underfunded_counterparty_moves_nothing() {
    for side in SIDES {
        let (mut env, p) = open_market(AssetClass::Credit, side);
        let account = match side {
            DealSide::Sell => p.counterparty_payment,
            DealSide::Buy => p.counterparty_asset,
        };
        env.update_token_account(&account, |s| s.amount = 1);
        let before = snapshot(&env, &p, deal_id(1));
        assert!(env.accept_deal(&p, deal_id(1)).is_err());
        assert_untouched(&env, &p, deal_id(1), before, Some(DealStatus::Open));
        env.update_token_account(&account, |s| s.amount = P2P_PAYMENT);
        env.accept_deal(&p, deal_id(1)).unwrap();
    }
}

// ---- cancel_deal / expire_deal authorization and accounts --------------------------

#[test]
fn only_the_proposer_can_cancel() {
    let (mut env, p) = open_market(AssetClass::FixedIncome, DealSide::Sell);
    let id = deal_id(1);
    let before = snapshot(&env, &p, id);
    let stranger = env.funded_keypair();
    for intruder in [stranger, p.counterparty.insecure_clone(), env.admin.insecure_clone()] {
        let mut a = env.refund_accounts(&p, id);
        a.proposer = intruder.pubkey();
        a.proposer_refund = env.create_token_account(SPL_TOKEN, p.asset_mint, intruder.pubkey(), 0);
        let res = env.cancel_deal_with(&intruder, &a);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_untouched(&env, &p, id, before, Some(DealStatus::Open));
}

/// The crank is permissionless, but the refund only ever reaches the proposer.
#[test]
fn expire_refunds_only_the_proposer() {
    let (mut env, p) = open_market(AssetClass::FixedIncome, DealSide::Sell);
    let id = deal_id(1);
    env.warp_secs(DEAL_TTL);
    let before = snapshot(&env, &p, id);
    let thief = env.funded_keypair();
    let thief_account = env.create_token_account(SPL_TOKEN, p.asset_mint, thief.pubkey(), 0);

    let mut a = env.refund_accounts(&p, id);
    a.proposer_refund = thief_account;
    let res = env.expire_deal_with(&thief, &a);
    assert_custom_err(res, AnchorError::ConstraintTokenOwner);

    let mut a = env.refund_accounts(&p, id);
    a.proposer = thief.pubkey();
    a.proposer_refund = thief_account;
    let res = env.expire_deal_with(&thief, &a);
    assert_custom_err(res, ExchangeError::DealAccountMismatch);

    assert_untouched(&env, &p, id, before, Some(DealStatus::Open));
    assert_eq!(env.balance(&thief_account), 0);
    env.expire_deal(&p, id).unwrap();
}

#[test]
fn refund_accounts_must_match_the_deposit() {
    let (mut env, p) = open_market(AssetClass::FixedIncome, DealSide::Sell);
    let id = deal_id(1);
    let proposer = p.proposer.insecure_clone();
    // The other mint of the deal, with the proposer's account of it.
    let mut a = env.refund_accounts(&p, id);
    a.deposit_mint = p.payment_mint;
    a.proposer_refund = p.proposer_payment;
    let res = env.cancel_deal_with(&proposer, &a);
    assert_custom_err(res, ExchangeError::DealAccountMismatch);
    // Right mint, refund account of another mint.
    let mut a = env.refund_accounts(&p, id);
    a.proposer_refund = p.proposer_payment;
    let res = env.cancel_deal_with(&proposer, &a);
    assert_custom_err(res, AnchorError::ConstraintTokenMint);
    env.cancel_deal(&p, id).unwrap();
}

// ---- tokens sent to a vault (invariant 6) -------------------------------------------

/// Anyone can transfer into a deal vault. The excess must not block closing
/// the vault (which would strand the deposit): it leaves with the deposit.
#[test]
fn vault_donations_never_strand_the_deposit() {
    for exit in ["accept", "cancel", "expire"] {
        for side in SIDES {
            let (mut env, p) = open_market(AssetClass::FixedIncome, side);
            let id = deal_id(1);
            let deposit_mint = match side {
                DealSide::Sell => p.asset_mint,
                DealSide::Buy => p.payment_mint,
            };
            env.donate(deposit_mint, p.vault(id), 7);
            let before = snapshot(&env, &p, id);
            let excess = match exit {
                "accept" => {
                    let meta = env.accept_deal(&p, id).unwrap();
                    events::<DealExecuted>(&meta)[0].vault_excess
                }
                "cancel" => {
                    let meta = env.cancel_deal(&p, id).unwrap();
                    events::<DealCancelled>(&meta)[0].vault_excess
                }
                _ => {
                    env.warp_secs(DEAL_TTL);
                    let meta = env.expire_deal(&p, id).unwrap();
                    events::<DealExpired>(&meta)[0].vault_excess
                }
            };
            assert_eq!(excess, 7);
            assert!(!env.exists(&p.vault(id)));
            let after = snapshot(&env, &p, id);
            let fee = expected_fee(PAYMENT, FEE_BPS);
            match (exit, side) {
                ("accept", DealSide::Sell) => {
                    assert_eq!(after.counterparty_asset, before.counterparty_asset + ASSETS + 7)
                }
                ("accept", DealSide::Buy) => assert_eq!(
                    after.counterparty_payment,
                    before.counterparty_payment + PAYMENT - fee + 7
                ),
                (_, DealSide::Sell) => assert_eq!(after.proposer_asset, P2P_ASSETS + 7),
                (_, DealSide::Buy) => assert_eq!(after.proposer_payment, P2P_PAYMENT + 7),
            }
            // The fee is unaffected by the excess.
            if exit == "accept" {
                assert_eq!(after.fee_vault, fee);
                env.assert_vault_matches_ledger(&p.payment_mint);
            }
        }
    }
}
