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
//! Random donations into the fee vaults and `sweep_excess` calls exercise the
//! on-chain form of invariant 4 (`vault >= unwithdrawn`, `==` after a sweep).
//! Issuance requests, approvals/rejections (incl. on suspended or delisted
//! listings and while paused) and burns check invariant 11 on the four
//! issuer listings. Their trader fixtures hold balances written directly
//! (outside supply); issuance goes to a separate holder per listing, and only
//! that holder burns, so `supply == issued - burned` stays exact.
//! `PROPTEST_CASES` overrides the number of cases (default 48).

mod common;

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

use anchor_lang::prelude::Pubkey;
use common::*;
use niara_exchange::{
    events::{IssuanceExecuted, Settled, TokensBurned},
    math::split_payment,
    AssetClass, BurnReason, CashbackLedger, Config, ConfigChange, Listing, ListingStatus,
    BPS_DENOMINATOR, MAX_FEE_BPS,
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
static SWEEP_OK: AtomicU64 = AtomicU64::new(0);
static SWEEP_REJECTED: AtomicU64 = AtomicU64::new(0);
static ISSUE_OK: AtomicU64 = AtomicU64::new(0);
static BURN_OK: AtomicU64 = AtomicU64::new(0);

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
    /// Unaccounted transfer straight into a fee vault.
    Donate { currency: usize, amount: u64 },
    Sweep { currency: usize, intruder: bool },
    /// Issuance on one of `ISSUER_LISTINGS` (index into `self.mints`).
    RequestIssuance { listing: usize, amount: u64, intruder: bool },
    /// Approve or reject the oldest open batch of the listing.
    ResolveIssuance { listing: usize, approve: bool, intruder: bool },
    /// The listing's holder burns `permille` of its balance (above 1000 overdraws).
    Burn { listing: usize, permille: u16, redemption: bool },
}

/// Listings with an issuer, so with Niara-approved issuance.
const ISSUER_LISTINGS: [usize; 4] = [E0, E1, FI, CR];

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
        1 => (prop::sample::select(CURRENCIES.to_vec()), 1u64..1_000_000)
            .prop_map(|(currency, amount)| Action::Donate { currency, amount }),
        1 => (prop::sample::select(CURRENCIES.to_vec()), prop::bool::weighted(0.1))
            .prop_map(|(currency, intruder)| Action::Sweep { currency, intruder }),
        3 => (
            prop::sample::select(ISSUER_LISTINGS.to_vec()),
            prop_oneof![8 => 1u64..1_000_000, 1 => Just(0u64)],
            prop::bool::weighted(0.1),
        )
            .prop_map(|(listing, amount, intruder)| Action::RequestIssuance {
                listing,
                amount,
                intruder,
            }),
        3 => (
            prop::sample::select(ISSUER_LISTINGS.to_vec()),
            prop::bool::weighted(0.8),
            prop::bool::weighted(0.1),
        )
            .prop_map(|(listing, approve, intruder)| Action::ResolveIssuance {
                listing,
                approve,
                intruder,
            }),
        2 => (
            prop::sample::select(ISSUER_LISTINGS.to_vec()),
            prop_oneof![4 => 0u16..=1000, 1 => 1001u16..1100],
            any::<bool>(),
        )
            .prop_map(|(listing, permille, redemption)| Action::Burn {
                listing,
                permille,
                redemption,
            }),
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
    /// Donations per currency since its last successful sweep.
    excess: HashMap<usize, u64>,
    /// Per issuer listing: the holder that receives issuance and burns, and
    /// its asset account.
    holders: HashMap<usize, (Keypair, Pubkey)>,
    /// Per issuer listing: open (requested, unresolved) batch numbers, oldest first.
    open_batches: HashMap<usize, Vec<u64>>,
    /// Per issuer listing: (Σ `IssuanceExecuted`, Σ `TokensBurned`).
    issuance: HashMap<usize, (u64, u64)>,
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

        let mut holders = HashMap::new();
        for listing in ISSUER_LISTINGS {
            let holder = env.funded_keypair();
            let program = env.owner_of(&mints[listing]);
            let account = env.create_token_account(program, mints[listing], holder.pubkey(), 0);
            holders.insert(listing, (holder, account));
        }

        let intruder = env.funded_keypair();
        World {
            holders,
            open_batches: ISSUER_LISTINGS.iter().map(|&l| (l, Vec::new())).collect(),
            issuance: ISSUER_LISTINGS.iter().map(|&l| (l, (0, 0))).collect(),
            env,
            mints,
            issuers,
            pairs,
            ledgers,
            issuer_dests,
            treasury_dests,
            intruder,
            model: CURRENCIES.iter().map(|&c| (c, (0, 0))).collect(),
            excess: CURRENCIES.iter().map(|&c| (c, 0)).collect(),
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
            Action::Donate { currency, amount } => {
                self.env.donate_to_fee_vault(self.mints[currency], amount);
                *self.excess.get_mut(&currency).unwrap() += amount;
            }
            Action::Sweep { currency, intruder } => self.sweep(currency, intruder)?,
            Action::RequestIssuance {
                listing,
                amount,
                intruder,
            } => self.request_issuance(listing, amount, intruder)?,
            Action::ResolveIssuance {
                listing,
                approve,
                intruder,
            } => self.resolve_issuance(listing, approve, intruder)?,
            Action::Burn {
                listing,
                permille,
                redemption,
            } => self.burn(listing, permille, redemption)?,
        }
        Ok(())
    }

    fn request_issuance(
        &mut self,
        listing: usize,
        amount: u64,
        intruder: bool,
    ) -> Result<(), TestCaseError> {
        let mint = self.mints[listing];
        let before = self.env.listing(&mint);
        let signer = if intruder {
            self.intruder.insecure_clone()
        } else {
            self.issuers[listing].insecure_clone()
        };
        let recipient = self.holders[&listing].0.pubkey();
        let res = self
            .env
            .request_issuance(&signer, mint, amount, recipient, ISSUANCE_DOC);
        let issuable = matches!(before.status, ListingStatus::Pending | ListingStatus::Approved);
        if res.is_err() {
            prop_assert!(
                intruder || !issuable || amount == 0,
                "valid issuance request failed"
            );
            prop_assert_eq!(self.env.listing(&mint).next_batch_no, before.next_batch_no);
            return Ok(());
        }
        prop_assert!(!intruder, "issuance requested by a non-issuer");
        prop_assert!(issuable, "issuance requested on a {:?} listing", before.status);
        prop_assert!(amount > 0);
        prop_assert_eq!(self.env.listing(&mint).next_batch_no, before.next_batch_no + 1);
        self.open_batches
            .get_mut(&listing)
            .unwrap()
            .push(before.next_batch_no);
        Ok(())
    }

    fn resolve_issuance(
        &mut self,
        listing: usize,
        approve: bool,
        intruder: bool,
    ) -> Result<(), TestCaseError> {
        // Prefer the chosen listing; otherwise any listing with an open batch.
        let listing = std::iter::once(listing)
            .chain(ISSUER_LISTINGS)
            .find(|l| !self.open_batches[l].is_empty())
            .unwrap_or(listing);
        let Some(&batch_no) = self.open_batches[&listing].first() else {
            return Ok(());
        };
        let mint = self.mints[listing];
        let status = self.env.listing(&mint).status;
        let paused = self.config().paused;
        let supply = self.env.mint_supply(&mint);
        let signer = if intruder {
            self.intruder.insecure_clone()
        } else {
            self.env.listing_authority.insecure_clone()
        };
        let res = if approve {
            let dest = self.holders[&listing].1;
            self.env.approve_issuance(&signer, mint, batch_no, dest)
        } else {
            self.env.reject_issuance(&signer, mint, batch_no)
        };
        let meta = match res {
            Err(_) => {
                prop_assert_eq!(self.env.mint_supply(&mint), supply, "failed resolve minted");
                let expected_to_fail =
                    intruder || (approve && (paused || status != ListingStatus::Approved));
                prop_assert!(expected_to_fail, "valid resolution of batch {batch_no} failed");
                return Ok(());
            }
            Ok(meta) => meta,
        };
        prop_assert!(!intruder, "batch resolved by a non-listing-authority");
        self.open_batches.get_mut(&listing).unwrap().remove(0);
        if approve {
            ISSUE_OK.fetch_add(1, Relaxed);
            prop_assert!(!paused, "issued while paused");
            prop_assert_eq!(status, ListingStatus::Approved, "issued on a non-Approved listing");
            let e = &events::<IssuanceExecuted>(&meta)[0];
            prop_assert_eq!(e.batch_no, batch_no);
            prop_assert_eq!(self.env.mint_supply(&mint) - supply, e.amount);
            self.issuance.get_mut(&listing).unwrap().0 += e.amount;
        } else {
            prop_assert_eq!(self.env.mint_supply(&mint), supply);
        }
        Ok(())
    }

    fn burn(&mut self, listing: usize, permille: u16, redemption: bool) -> Result<(), TestCaseError> {
        // Prefer the chosen listing; otherwise any holder with a balance.
        let listing = std::iter::once(listing)
            .chain(ISSUER_LISTINGS)
            .find(|l| self.env.balance(&self.holders[l].1) > 0)
            .unwrap_or(listing);
        let mint = self.mints[listing];
        let (holder, account) = {
            let (h, a) = &self.holders[&listing];
            (h.insecure_clone(), *a)
        };
        let balance = self.env.balance(&account);
        let amount = scaled(balance, permille);
        let reason = if redemption {
            BurnReason::Redemption
        } else {
            BurnReason::Cancellation
        };
        let res = self
            .env
            .burn_with_reason(&holder, mint, account, amount, reason, BURN_REF);
        let meta = match res {
            Err(_) => {
                prop_assert_eq!(self.env.balance(&account), balance);
                prop_assert!(amount == 0 || amount > balance, "valid burn of {amount} failed");
                return Ok(());
            }
            Ok(meta) => meta,
        };
        BURN_OK.fetch_add(1, Relaxed);
        prop_assert!(amount > 0 && amount <= balance);
        let e = &events::<TokensBurned>(&meta)[0];
        prop_assert_eq!((e.amount, e.reason), (amount, reason));
        prop_assert_eq!(balance - self.env.balance(&account), amount);
        self.issuance.get_mut(&listing).unwrap().1 += amount;
        Ok(())
    }

    fn sweep(&mut self, currency: usize, intruder: bool) -> Result<(), TestCaseError> {
        let mint = self.mints[currency];
        let paused = self.config().paused;
        let signer = if intruder {
            self.intruder.insecure_clone()
        } else {
            self.env.treasury.insecure_clone()
        };
        let dest = self.treasury_dests[&currency];
        let before = self.env.balance(&dest);
        let excess = self.excess[&currency];
        let res = self.env.sweep_excess(&signer, mint, dest);
        if res.is_err() {
            SWEEP_REJECTED.fetch_add(1, Relaxed);
            prop_assert_eq!(self.env.balance(&dest), before);
            // A valid sweep only fails when there is nothing to sweep.
            prop_assert!(paused || intruder || excess == 0, "valid sweep of {excess} failed");
            return Ok(());
        }
        SWEEP_OK.fetch_add(1, Relaxed);
        prop_assert!(!paused, "swept while paused");
        prop_assert!(!intruder, "swept by a non-treasury");
        // Exactly the donations, never owed fees or cashback.
        prop_assert_eq!(self.env.balance(&dest) - before, excess);
        self.excess.insert(currency, 0);
        // Invariant 4 is strict right after a sweep.
        let ledger = self.env.fee_ledger(&mint);
        prop_assert_eq!(
            self.env.balance(&fee_vault_pda(&mint)),
            ledger.unwithdrawn().unwrap()
        );
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
        prop_assert!(seller_receives > 0, "seller received nothing");
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

            // Invariant 4: the vault holds what is owed plus the donations
            // not swept yet (so `>=` on-chain, `==` after every sweep).
            prop_assert_eq!(
                self.env.balance(&fee_vault_pda(&mint)),
                ledger.unwithdrawn().unwrap() + self.excess[&currency],
                "fee vault != unwithdrawn fees + unswept donations"
            );
        }

        // Invariant 11: supply == Σ executed batches − Σ burns, on-chain
        // counters and events agreeing.
        for listing in ISSUER_LISTINGS {
            let mint = self.mints[listing];
            let l = self.env.listing(&mint);
            let (issued, burned) = self.issuance[&listing];
            prop_assert_eq!((l.issued, l.burned), (issued, burned), "listing vs events");
            prop_assert_eq!(self.env.mint_supply(&mint), issued - burned, "supply");
            prop_assert_eq!(self.env.mint_authority(&mint), Some(mint_authority_pda(&mint)));
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
    let (s_ok, s_rejected) = (SWEEP_OK.load(Relaxed), SWEEP_REJECTED.load(Relaxed));
    let (issued, burned) = (ISSUE_OK.load(Relaxed), BURN_OK.load(Relaxed));
    println!(
        "settle ok {ok} / rejected {rejected}; withdraw ok {w_ok} / rejected {w_rejected}; \
         sweep ok {s_ok} / rejected {s_rejected}; batches executed {issued}; \
         burns {burned}; config changes executed {changes}"
    );
    // Not vacuous: both outcomes of every guarded path were exercised.
    assert!(ok > 0 && rejected > 0, "settle outcomes not exercised");
    assert!(w_ok > 0 && w_rejected > 0, "withdraw outcomes not exercised");
    assert!(s_ok > 0 && s_rejected > 0, "sweep outcomes not exercised");
    assert!(issued > 0 && burned > 0, "issuance paths not exercised");
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
