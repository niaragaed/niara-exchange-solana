//! Invariant fuzzing (spec §3.9): `proptest` drives random sequences of valid
//! and invalid instructions against the real `.so` in LiteSVM, and checks
//! invariants 1, 2, 3, 4, 5, 8, 9 and 10 after every step. (Escrow invariants
//! 6 and 7 arrive with the P2P escrow.)
//!
//! Each case builds a fresh world:
//! - listings: two `Equity` (SPL Token, Token-2022), one `FixedIncome`, one
//!   `Credit`, and two `Currency` (SPL Token, Token-2022);
//! - cashback ledgers for every cashback-eligible listing x currency;
//! - one funded, fully delegated buyer/seller pair for every asset x payment
//!   combination, including invalid ones (non-Equity asset, Currency as asset,
//!   Equity as payment).
//!
//! Actions are weighted towards valid settlements so that most steps move
//! funds; the counters checked at the end make sure the run is not vacuous.
//! `PROPTEST_CASES` overrides the number of cases (default 48).

mod common;

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

use anchor_lang::prelude::Pubkey;
use common::*;
use niara_exchange::{
    events::Settled, math::split_payment, AssetClass, CashbackLedger, Config, ConfigChange,
    Listing, ListingStatus, BPS_DENOMINATOR, MAX_FEE_BPS,
};
use proptest::{
    prelude::*,
    test_runner::{Config as ProptestConfig, TestCaseError, TestRunner},
};
use solana_keypair::Keypair;
use solana_signer::Signer;

// Listing indices.
const E0: usize = 0; // Equity, SPL Token
const E1: usize = 1; // Equity, Token-2022
const FI: usize = 2; // FixedIncome
const CR: usize = 3; // Credit
const USD: usize = 4; // Currency, SPL Token
const BRL: usize = 5; // Currency, Token-2022
const LISTINGS: usize = 6;
const CURRENCIES: [usize; 2] = [USD, BRL];

/// (asset, payment) of every trader pair. The first four are the valid
/// markets; the rest must never settle.
const PAIRS: [(usize, usize); 14] = [
    (E0, USD),
    (E0, BRL),
    (E1, USD),
    (E1, BRL),
    (FI, USD), // invariant 8
    (CR, BRL), // invariant 8
    (FI, BRL),
    (CR, USD),
    (USD, BRL), // invariants 8 and 9: Currency as asset
    (BRL, USD),
    (E0, E1), // invariant 10: Equity as payment
    (E1, E0),
    (FI, E0),
    (USD, E0),
];
const VALID_PAIRS: usize = 4;

const ASSET_BALANCE: u64 = 1_000_000_000;
const PAYMENT_BALANCE: u64 = 1_000_000_000_000_000;

static SETTLE_OK: AtomicU64 = AtomicU64::new(0);
static SETTLE_REJECTED: AtomicU64 = AtomicU64::new(0);
static WITHDRAW_OK: AtomicU64 = AtomicU64::new(0);
static WITHDRAW_REJECTED: AtomicU64 = AtomicU64::new(0);
static CONFIG_CHANGES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
enum StatusOp {
    Approve,
    Suspend,
    Delist,
}

#[derive(Clone, Debug)]
enum Action {
    /// `id` comes from a small space (128) so replays still happen.
    Settle {
        pair: usize,
        assets: u64,
        payment: u64,
        id: u8,
        intruder: bool,
    },
    Status { listing: usize, op: StatusOp },
    Pause,
    Unpause,
    /// `permille` of what is available; above 1000 overdraws.
    WithdrawCashback { asset: usize, currency: usize, permille: u16, intruder: bool },
    WithdrawFees { currency: usize, permille: u16, intruder: bool },
    ProposeFee(u16),
    ProposeCashback(u16),
    ExecuteFee,
    ExecuteCashback,
    Warp(i64),
    /// Re-approve (or shrink) a delegation through the real token program.
    Delegate { pair: usize, seller_side: bool, amount: u64 },
    InitLedger { listing: usize, currency: usize },
}

fn action() -> impl Strategy<Value = Action> {
    let pair = prop_oneof![4 => 0..VALID_PAIRS, 1 => 0..PAIRS.len()];
    let payment = prop_oneof![
        8 => 1u64..10_000_000_000,
        2 => 1u64..20_000,
        1 => Just(0u64),
    ];
    let assets = prop_oneof![12 => 1u64..1_000, 1 => Just(0u64)];
    let settle = (pair, assets, payment, 0u8..128, prop::bool::weighted(0.05)).prop_map(
        |(pair, assets, payment, id, intruder)| Action::Settle {
            pair,
            assets,
            payment,
            id,
            intruder,
        },
    );
    let status_op = prop_oneof![
        3 => Just(StatusOp::Approve),
        2 => Just(StatusOp::Suspend),
        1 => Just(StatusOp::Delist),
    ];
    let status =
        (0..LISTINGS, status_op).prop_map(|(listing, op)| Action::Status { listing, op });
    let permille = prop_oneof![4 => 0u16..=1000, 1 => 1001u16..1100];
    let withdraw_cashback = (
        0..LISTINGS,
        prop::sample::select(CURRENCIES.to_vec()),
        permille.clone(),
        prop::bool::weighted(0.1),
    )
        .prop_map(|(asset, currency, permille, intruder)| Action::WithdrawCashback {
            asset,
            currency,
            permille,
            intruder,
        });
    let withdraw_fees = (
        prop::sample::select(CURRENCIES.to_vec()),
        permille,
        prop::bool::weighted(0.1),
    )
        .prop_map(|(currency, permille, intruder)| Action::WithdrawFees {
            currency,
            permille,
            intruder,
        });
    let bps = prop_oneof![3 => 0u16..=MAX_FEE_BPS, 1 => any::<u16>()];
    let cashback_bps = prop_oneof![3 => 0u16..=BPS_DENOMINATOR, 1 => any::<u16>()];
    prop_oneof![
        12 => settle,
        2 => status,
        1 => Just(Action::Pause),
        1 => Just(Action::Unpause),
        3 => withdraw_cashback,
        2 => withdraw_fees,
        1 => bps.prop_map(Action::ProposeFee),
        1 => cashback_bps.prop_map(Action::ProposeCashback),
        1 => Just(Action::ExecuteFee),
        1 => Just(Action::ExecuteCashback),
        1 => (0i64..2 * TIMELOCK_SECS).prop_map(Action::Warp),
        1 => (0..PAIRS.len(), any::<bool>(), 0u64..2_000)
            .prop_map(|(pair, seller_side, amount)| Action::Delegate { pair, seller_side, amount }),
        1 => (0..LISTINGS, prop::sample::select(vec![USD, BRL, E0]))
            .prop_map(|(listing, currency)| Action::InitLedger { listing, currency }),
    ]
}

struct World {
    env: Env,
    mints: Vec<Pubkey>,
    issuers: Vec<Keypair>,
    pairs: Vec<Market>,
    /// (listing, currency) pairs that have a cashback ledger.
    ledgers: HashSet<(usize, usize)>,
    /// Destination accounts for cashback, per (listing, currency).
    issuer_dests: HashMap<(usize, usize), Pubkey>,
    treasury_dests: HashMap<usize, Pubkey>,
    intruder: Keypair,
    /// Fees and cashback per currency, summed from `Settled` events.
    model: HashMap<usize, (u64, u64)>,
}

impl World {
    fn new() -> Self {
        let mut env = Env::initialized();
        let authority = env.listing_authority.insecure_clone();
        let mut mints = Vec::new();
        let mut issuers = Vec::new();

        for program in [SPL_TOKEN, TOKEN_2022] {
            let (mint, issuer) = env.listed_equity(program);
            mints.push(mint);
            issuers.push(issuer);
        }
        for class in [AssetClass::FixedIncome, AssetClass::Credit] {
            let issuer = env.funded_keypair();
            let mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
            env.set_mint_decimals(&mint, ASSET_DECIMALS);
            env.submit_listing(&issuer, mint, class, DOC_HASH).unwrap();
            env.approve(&authority, mint).unwrap();
            mints.push(mint);
            issuers.push(issuer);
        }
        for program in [SPL_TOKEN, TOKEN_2022] {
            mints.push(env.listed_currency(program));
            // Currency listings have no issuer; a stranger stands in.
            issuers.push(env.funded_keypair());
        }

        let payer = env.admin.insecure_clone();
        let mut ledgers = HashSet::new();
        let mut issuer_dests = HashMap::new();
        for listing in [E0, E1, FI, CR] {
            for currency in CURRENCIES {
                env.init_cashback_ledger(&payer, mints[listing], mints[currency])
                    .unwrap();
                ledgers.insert((listing, currency));
            }
        }
        for listing in 0..LISTINGS {
            for currency in CURRENCIES {
                let program = env.owner_of(&mints[currency]);
                let dest = env.create_token_account(
                    program,
                    mints[currency],
                    issuers[listing].pubkey(),
                    0,
                );
                issuer_dests.insert((listing, currency), dest);
            }
        }
        let mut treasury_dests = HashMap::new();
        let treasury = env.treasury.pubkey();
        for currency in CURRENCIES {
            let program = env.owner_of(&mints[currency]);
            let dest = env.create_token_account(program, mints[currency], treasury, 0);
            treasury_dests.insert(currency, dest);
        }

        let pairs = PAIRS
            .iter()
            .map(|&(asset, payment)| {
                let issuer = issuers[asset].insecure_clone();
                env.trader_pair(
                    mints[asset],
                    mints[payment],
                    issuer,
                    ASSET_BALANCE,
                    PAYMENT_BALANCE,
                )
            })
            .collect();

        let intruder = env.funded_keypair();
        World {
            env,
            mints,
            issuers,
            pairs,
            ledgers,
            issuer_dests,
            treasury_dests,
            intruder,
            model: CURRENCIES.iter().map(|&c| (c, (0, 0))).collect(),
        }
    }

    fn listing(&self, index: usize) -> Listing {
        self.env.fetch(&listing_pda(&self.mints[index]))
    }

    fn config(&self) -> Config {
        self.env.fetch(&config_pda())
    }

    fn balance_or_zero(&self, account: &Pubkey) -> u64 {
        if self.env.exists(account) {
            self.env.balance(account)
        } else {
            0
        }
    }

    fn apply(&mut self, action: Action) -> Result<(), TestCaseError> {
        match action {
            Action::Settle {
                pair,
                assets,
                payment,
                id,
                intruder,
            } => self.settle(pair, assets, payment, id, intruder)?,
            Action::Status { listing, op } => {
                let authority = self.env.listing_authority.insecure_clone();
                let mint = self.mints[listing];
                let _ = match op {
                    StatusOp::Approve => self.env.approve(&authority, mint),
                    StatusOp::Suspend => self.env.suspend(&authority, mint),
                    StatusOp::Delist => self.env.delist(&authority, mint),
                };
            }
            Action::Pause => {
                let admin = self.env.admin.insecure_clone();
                let _ = self.env.pause(&admin);
            }
            Action::Unpause => {
                let admin = self.env.admin.insecure_clone();
                let _ = self.env.unpause(&admin);
            }
            Action::WithdrawCashback {
                asset,
                currency,
                permille,
                intruder,
            } => self.withdraw_cashback(asset, currency, permille, intruder)?,
            Action::WithdrawFees {
                currency,
                permille,
                intruder,
            } => self.withdraw_fees(currency, permille, intruder)?,
            Action::ProposeFee(v) => self.propose(ConfigChange::FeeBps(v))?,
            Action::ProposeCashback(v) => self.propose(ConfigChange::CashbackBps(v))?,
            Action::ExecuteFee => self.execute(ConfigChange::FeeBps(0)),
            Action::ExecuteCashback => self.execute(ConfigChange::CashbackBps(0)),
            Action::Warp(secs) => self.env.warp_secs(secs),
            Action::Delegate {
                pair,
                seller_side,
                amount,
            } => {
                let m = &self.pairs[pair];
                let (owner, program, account, mint) = if seller_side {
                    (m.seller.insecure_clone(), m.asset_program, m.seller_asset, m.asset_mint)
                } else {
                    (m.buyer.insecure_clone(), m.payment_program, m.buyer_payment, m.payment_mint)
                };
                self.env.approve_vault(&owner, program, account, mint, amount);
            }
            Action::InitLedger { listing, currency } => {
                let payer = self.env.admin.insecure_clone();
                let existed = self.ledgers.contains(&(listing, currency));
                let res =
                    self.env
                        .init_cashback_ledger(&payer, self.mints[listing], self.mints[currency]);
                if res.is_ok() {
                    // Invariant 9: never for a Currency listing.
                    prop_assert!(
                        !CURRENCIES.contains(&listing),
                        "cashback ledger created for a Currency listing"
                    );
                    prop_assert!(CURRENCIES.contains(&currency), "ledger for a non-currency");
                    prop_assert!(!existed, "ledger initialized twice");
                    self.ledgers.insert((listing, currency));
                }
            }
        }
        Ok(())
    }

    fn settle(
        &mut self,
        pair: usize,
        assets: u64,
        payment: u64,
        id: u8,
        intruder: bool,
    ) -> Result<(), TestCaseError> {
        let (asset_index, payment_index) = PAIRS[pair];
        let order = order_id(id as u128);
        let m = &self.pairs[pair];
        let accounts = self.env.settle_accounts(m);
        let watched = [
            m.seller_asset,
            m.buyer_asset,
            m.buyer_payment,
            m.seller_payment,
            fee_vault_pda(&m.payment_mint),
        ];
        let before: Vec<u64> = watched.iter().map(|a| self.balance_or_zero(a)).collect();
        let record_existed = self.env.exists(&settled_pda(&order));
        let config = self.config();
        let asset_listing = self.listing(asset_index);
        let payment_listing = self.listing(payment_index);

        let res = if intruder {
            let signer = self.intruder.insecure_clone();
            let mut a = accounts;
            a.operator = signer.pubkey();
            self.env.settle_with(&signer, &a, order, assets, payment)
        } else {
            let operator = self.env.operator.insecure_clone();
            self.env.settle_with(&operator, &accounts, order, assets, payment)
        };
        let after: Vec<u64> = watched.iter().map(|a| self.balance_or_zero(a)).collect();

        let meta = match res {
            Err(_) => {
                SETTLE_REJECTED.fetch_add(1, Relaxed);
                prop_assert_eq!(&before, &after, "failed settle moved funds");
                prop_assert_eq!(self.env.exists(&settled_pda(&order)), record_existed);
                return Ok(());
            }
            Ok(meta) => meta,
        };
        SETTLE_OK.fetch_add(1, Relaxed);

        prop_assert!(!intruder, "settled by a non-operator");
        prop_assert!(!config.paused, "settled while paused");
        prop_assert!(!record_existed, "order_id settled twice");
        prop_assert!(assets > 0 && payment > 0);
        // Invariant 3.
        prop_assert_eq!(asset_listing.status, ListingStatus::Approved);
        // Invariant 8 (and 9: a Currency is never the asset leg).
        prop_assert_eq!(asset_listing.asset_class, AssetClass::Equity);
        // Invariant 10.
        prop_assert!(payment_listing.is_approved_currency());

        let events = events::<Settled>(&meta);
        prop_assert_eq!(events.len(), 1);
        let e = &events[0];
        let expected = split_payment(
            payment,
            config.fee_bps,
            config.cashback_bps,
            asset_listing.cashback_eligible,
        )
        .unwrap();
        prop_assert_eq!((e.fee, e.cashback), (expected.fee, expected.cashback));
        prop_assert_eq!(e.payment_amount, payment);
        prop_assert_eq!(e.asset_amount, assets);

        let seller_receives = after[3] - before[3];
        // Invariant 1, on the actual balance changes.
        prop_assert_eq!(e.payment_amount, seller_receives + e.fee);
        prop_assert_eq!(before[0] - after[0], assets);
        prop_assert_eq!(after[1] - before[1], assets);
        prop_assert_eq!(before[2] - after[2], payment);
        prop_assert_eq!(after[4] - before[4], e.fee);

        let entry = self.model.get_mut(&payment_index).expect("payment is a currency");
        entry.0 += e.fee;
        entry.1 += e.cashback;
        Ok(())
    }

    fn withdraw_cashback(
        &mut self,
        asset: usize,
        currency: usize,
        permille: u16,
        intruder: bool,
    ) -> Result<(), TestCaseError> {
        let ledger_address = cashback_pda(&self.mints[asset], &self.mints[currency]);
        let available = if self.env.exists(&ledger_address) {
            self.env.fetch::<CashbackLedger>(&ledger_address).available().unwrap()
        } else {
            0
        };
        let amount = scaled(available, permille);
        let paused = self.config().paused;
        let signer = if intruder {
            self.intruder.insecure_clone()
        } else {
            self.issuers[asset].insecure_clone()
        };
        let dest = self.issuer_dests[&(asset, currency)];
        let before = self.env.balance(&dest);
        let res = self.env.withdraw_cashback(
            &signer,
            self.mints[asset],
            self.mints[currency],
            dest,
            amount,
        );
        if res.is_err() {
            WITHDRAW_REJECTED.fetch_add(1, Relaxed);
            prop_assert_eq!(self.env.balance(&dest), before);
            return Ok(());
        }
        WITHDRAW_OK.fetch_add(1, Relaxed);
        prop_assert!(!paused, "cashback withdrawn while paused");
        prop_assert!(!intruder, "cashback withdrawn by a non-issuer");
        prop_assert!(!CURRENCIES.contains(&asset), "cashback from a Currency listing");
        prop_assert!(amount > 0 && amount <= available);
        prop_assert_eq!(self.env.balance(&dest) - before, amount);
        Ok(())
    }

    fn withdraw_fees(
        &mut self,
        currency: usize,
        permille: u16,
        intruder: bool,
    ) -> Result<(), TestCaseError> {
        let mint = self.mints[currency];
        let available = self.env.fee_ledger(&mint).protocol_available().unwrap();
        let amount = scaled(available, permille);
        let paused = self.config().paused;
        let signer = if intruder {
            self.intruder.insecure_clone()
        } else {
            self.env.treasury.insecure_clone()
        };
        let dest = self.treasury_dests[&currency];
        let before = self.env.balance(&dest);
        let res = self.env.withdraw_protocol_fees(&signer, mint, dest, amount);
        if res.is_err() {
            WITHDRAW_REJECTED.fetch_add(1, Relaxed);
            prop_assert_eq!(self.env.balance(&dest), before);
            return Ok(());
        }
        WITHDRAW_OK.fetch_add(1, Relaxed);
        prop_assert!(!paused, "protocol fees withdrawn while paused");
        prop_assert!(!intruder, "protocol fees withdrawn by a non-treasury");
        prop_assert!(amount > 0 && amount <= available);
        prop_assert_eq!(self.env.balance(&dest) - before, amount);
        Ok(())
    }

    fn propose(&mut self, change: ConfigChange) -> Result<(), TestCaseError> {
        let admin = self.env.admin.insecure_clone();
        let res = self.env.propose(&admin, change);
        if res.is_ok() {
            // Out-of-range values are refused at proposal time.
            prop_assert!(change.validate().is_ok(), "accepted invalid {change:?}");
        }
        Ok(())
    }

    /// The pending slot is per kind; the value in `kind` is ignored.
    fn execute(&mut self, kind: ConfigChange) {
        let admin = self.env.admin.insecure_clone();
        if self.env.execute(&admin, &kind).is_ok() {
            CONFIG_CHANGES.fetch_add(1, Relaxed);
        }
    }

    fn check_invariants(&self) -> Result<(), TestCaseError> {
        // Invariant 5.
        let config = self.config();
        prop_assert!(config.fee_bps <= MAX_FEE_BPS, "fee_bps {}", config.fee_bps);
        prop_assert!(config.cashback_bps <= BPS_DENOMINATOR);

        for currency in CURRENCIES {
            let mint = self.mints[currency];
            let ledger = self.env.fee_ledger(&mint);
            let (fees, cashback) = self.model[&currency];
            prop_assert_eq!(ledger.fees_collected, fees, "fees vs Settled events");
            prop_assert_eq!(ledger.cashback_accrued, cashback, "cashback vs Settled events");

            // Invariant 2.
            prop_assert!(ledger.cashback_accrued <= ledger.fees_collected);
            let mut accrued = 0u64;
            let mut withdrawn = 0u64;
            for &(listing, c) in &self.ledgers {
                if c != currency {
                    continue;
                }
                let l: CashbackLedger =
                    self.env.fetch(&cashback_pda(&self.mints[listing], &mint));
                prop_assert!(l.withdrawn <= l.accrued);
                accrued += l.accrued;
                withdrawn += l.withdrawn;
            }
            prop_assert_eq!(accrued, ledger.cashback_accrued);
            prop_assert_eq!(withdrawn, ledger.cashback_withdrawn);
            prop_assert!(
                ledger.protocol_withdrawn <= ledger.fees_collected - ledger.cashback_accrued
            );

            // Invariant 4 (no donations in this harness).
            prop_assert_eq!(
                self.env.balance(&fee_vault_pda(&mint)),
                ledger.unwithdrawn().unwrap(),
                "fee vault != unwithdrawn fees"
            );
        }

        // Invariant 9: no Currency listing ever has a cashback ledger.
        for listing in CURRENCIES {
            for currency in [USD, BRL, E0] {
                prop_assert!(!self
                    .env
                    .exists(&cashback_pda(&self.mints[listing], &self.mints[currency])));
            }
        }
        Ok(())
    }
}

/// `permille` of `available`; above 1000 asks for more than is available.
fn scaled(available: u64, permille: u16) -> u64 {
    if permille > 1000 {
        available + u64::from(permille - 1000)
    } else {
        (u128::from(available) * u128::from(permille) / 1000) as u64
    }
}

#[test]
fn invariants_hold_under_random_instruction_sequences() {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(48);
    let mut runner = TestRunner::new(ProptestConfig {
        cases,
        failure_persistence: None,
        ..ProptestConfig::default()
    });
    let result = runner.run(&prop::collection::vec(action(), 1..60), |actions| {
        let mut world = World::new();
        world.check_invariants()?;
        for action in actions {
            world.apply(action)?;
            world.check_invariants()?;
        }
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }

    let (ok, rejected) = (SETTLE_OK.load(Relaxed), SETTLE_REJECTED.load(Relaxed));
    let (w_ok, w_rejected) = (WITHDRAW_OK.load(Relaxed), WITHDRAW_REJECTED.load(Relaxed));
    let changes = CONFIG_CHANGES.load(Relaxed);
    println!(
        "settle ok {ok} / rejected {rejected}; withdraw ok {w_ok} / rejected {w_rejected}; \
         config changes executed {changes}"
    );
    // Not vacuous: both outcomes of every guarded path were exercised.
    assert!(ok > 0 && rejected > 0, "settle outcomes not exercised");
    assert!(w_ok > 0 && w_rejected > 0, "withdraw outcomes not exercised");
}

/// Deterministic sweep of invariants 8, 9 and 10: every invalid market (non-
/// Equity asset, Currency as asset, Equity as payment) is attempted with
/// healthy delegations and listings. None may settle.
#[test]
fn invalid_markets_never_settle() {
    let mut world = World::new();
    for (pair, _) in PAIRS.iter().enumerate().skip(VALID_PAIRS) {
        for id in 0..3u8 {
            world
                .settle(pair, 1, 1_000_000, 100 + pair as u8 * 3 + id, false)
                .unwrap();
        }
    }
    world.check_invariants().unwrap();
    for currency in CURRENCIES {
        assert_eq!(world.env.fee_ledger(&world.mints[currency]).fees_collected, 0);
    }
}
