//! Invariant fuzzing (spec §3.9): `proptest` drives random sequences of valid
//! and invalid instructions against the real `.so` in LiteSVM, and checks
//! invariants 1 to 12 after every step.
//!
//! Each case builds a fresh world:
//! - listings: two `Equity` (SPL Token, Token-2022), one `FixedIncome`, one
//!   `Credit`, and three `Currency`: USD, the fee mint (`Config.fee_mint`,
//!   SPL Token or Token-2022, drawn per case) with the only fee vault, BRL
//!   (Token-2022) and EUR (SPL Token);
//! - cashback ledgers for every cashback-eligible listing, in USD;
//! - one funded, fully delegated buyer/seller pair for every asset x payment
//!   combination, including invalid ones (non-Equity asset, Currency as asset,
//!   Equity as payment, a payment currency that is not the fee mint).
//!
//! Invariant 12: fees only ever accrue in USD. No fee vault or ledger exists
//! for any other mint, every successful settlement pays in USD, and every
//! executed deal has a USD leg whose receiver paid the fee.
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
//!
//! P2P escrow (invariants 6 and 7): random `open_deal` on valid and invalid
//! pairs (Equity asset, non-Currency payment, same mint, no USD leg such as
//! BRL <-> EUR), with expiries in
//! and out of the 30-day window, then `accept_deal`, `cancel_deal` and
//! `expire_deal` on tracked deals (incl. wrong signers, a crank redirecting
//! the refund, replays of resolved deals) and donations into deal vaults.
//! A zero match id never settles and a zero deal id never opens.
//! The outcome of every escrow call is predicted exactly; the balances of
//! every deal party are modelled, so a deposit that is stranded or paid twice
//! fails the run, and each vault must hold exactly its deposit while the deal
//! is open and be closed afterwards.
//! `PROPTEST_CASES` overrides the number of cases (default 48).

mod common;

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

use anchor_lang::prelude::Pubkey;
use common::*;
use niara_exchange::{
    events::{DealExecuted, DealOpened, IssuanceExecuted, Settled, TokensBurned},
    math::{split_payment, PaymentSplit},
    AssetClass, BurnReason, CashbackLedger, Config, ConfigChange, DealSide, DealStatus, FeeLeg,
    Listing, ListingStatus, BPS_DENOMINATOR, MAX_DEAL_DURATION_SECS, MAX_FEE_BPS,
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
const USD: usize = 4; // Currency, the fee mint (USDT), SPL Token or Token-2022
const BRL: usize = 5; // Currency, Token-2022
const EUR: usize = 6; // Currency, SPL Token
const LISTINGS: usize = 7;
const CURRENCIES: [usize; 3] = [USD, BRL, EUR];

/// (asset, payment) of every trader pair. The first two are the valid
/// markets; the rest must never settle.
const PAIRS: [(usize, usize); 14] = [
    (E0, USD),
    (E1, USD),
    (E0, BRL), // invariant 12: payment not in the fee mint
    (E1, EUR),
    (FI, USD), // invariant 8
    (CR, BRL), // invariant 8
    (FI, BRL),
    (CR, USD),
    (BRL, USD), // invariants 8 and 9: Currency as asset
    (USD, BRL),
    (E0, E1), // invariant 10: Equity as payment
    (E1, E0),
    (FI, E0),
    (USD, E0),
];
const VALID_PAIRS: usize = 2;

/// (asset, payment) of every P2P pair. The first six are valid markets.
const DEAL_PAIRS: [(usize, usize); 15] = [
    (FI, USD),
    (CR, USD),
    (BRL, USD), // Currency deal: BRL sold for USD, fee on the payment leg
    (USD, BRL), // USD sold for BRL: fee on the asset leg
    (EUR, USD),
    (USD, EUR),
    (FI, BRL), // invariant 12: no USD leg
    (CR, EUR),
    (BRL, EUR),
    (EUR, BRL),
    (E0, USD), // invariant 8: Equity never trades P2P
    (E1, BRL),
    (FI, E0), // invariant 10: Equity as payment
    (CR, FI), // invariant 10: non-Currency payment
    (USD, USD), // same mint
];
const VALID_DEAL_PAIRS: usize = 6;

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
static DEAL_OPEN_OK: AtomicU64 = AtomicU64::new(0);
static DEAL_OPEN_REJECTED: AtomicU64 = AtomicU64::new(0);
static DEAL_ACCEPT_OK: AtomicU64 = AtomicU64::new(0);
static DEAL_ACCEPT_REJECTED: AtomicU64 = AtomicU64::new(0);
static DEAL_CANCEL_OK: AtomicU64 = AtomicU64::new(0);
static DEAL_EXPIRE_OK: AtomicU64 = AtomicU64::new(0);
static DEAL_REFUND_REJECTED: AtomicU64 = AtomicU64::new(0);
static FREEZE_CHANGES: AtomicU64 = AtomicU64::new(0);
static FROZEN_WITHDRAW_REJECTED: AtomicU64 = AtomicU64::new(0);
static ZERO_ID_REJECTED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
enum StatusOp {
    Approve,
    Suspend,
    Delist,
}

#[derive(Clone, Debug)]
enum Action {
    /// `id` comes from a small space (128) so replays still happen; 0 is the
    /// all-zero match id, which never settles.
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
    /// `prefer_frozen`: target a frozen listing with cashback, if there is one.
    WithdrawCashback {
        asset: usize,
        currency: usize,
        permille: u16,
        intruder: bool,
        prefer_frozen: bool,
    },
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
    /// `id` comes from a small space (32) so reopened ids still happen; 0 is
    /// the all-zero deal id, which never opens.
    OpenDeal { pair: usize, sell: bool, assets: u64, payment: u64, ttl: i64, id: u8 },
    /// `deal` picks a tracked deal (mostly an open one).
    AcceptDeal { deal: usize, intruder: bool },
    CancelDeal { deal: usize, intruder: bool },
    /// `thief`: the crank names its own account as the refund destination.
    ExpireDeal { deal: usize, thief: bool },
    /// Unaccounted transfer straight into an open deal's vault.
    DonateToDeal { deal: usize, amount: u64 },
    /// `freeze_cashback` / `unfreeze_cashback` on any listing.
    CashbackFreeze { listing: usize, freeze: bool, intruder: bool },
}

/// The model of one deal opened during the run.
#[derive(Clone, Debug)]
struct TrackedDeal {
    pair: usize,
    id: [u8; 16],
    side: DealSide,
    assets: u64,
    payment: u64,
    /// `(fee_bps, cashback_bps)` of the config at open: the deal's fee.
    rates: (u16, u16),
    expires_at: i64,
    status: DealStatus,
    /// Tokens donated into the vault while open.
    excess: u64,
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
    let settle_id = prop_oneof![15 => 1u8..128, 1 => Just(0u8)];
    let settle = (pair, assets, payment, settle_id, prop::bool::weighted(0.05)).prop_map(
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
    // Mostly the fee mint; the others have no fee vault, so must always fail.
    let currency = || prop_oneof![6 => Just(USD), 1 => Just(BRL), 1 => Just(EUR)];
    let withdraw_cashback = (
        0..LISTINGS,
        currency(),
        permille.clone(),
        prop::bool::weighted(0.1),
        prop::bool::weighted(0.5),
    )
        .prop_map(
            |(asset, currency, permille, intruder, prefer_frozen)| Action::WithdrawCashback {
                asset,
                currency,
                permille,
                intruder,
                prefer_frozen,
            },
        );
    let withdraw_fees = (
        currency(),
        permille,
        prop::bool::weighted(0.1),
    )
        .prop_map(|(currency, permille, intruder)| Action::WithdrawFees {
            currency,
            permille,
            intruder,
        });
    let deal_pair = prop_oneof![4 => 0..VALID_DEAL_PAIRS, 1 => 0..DEAL_PAIRS.len()];
    let ttl = prop_oneof![
        8 => 1i64..2 * TIMELOCK_SECS,
        1 => -100i64..=0,
        1 => Just(MAX_DEAL_DURATION_SECS),
        1 => Just(MAX_DEAL_DURATION_SECS + 1),
    ];
    let deal_payment = prop_oneof![
        8 => 1u64..10_000_000_000,
        2 => 1u64..20_000,
        1 => Just(0u64),
    ];
    let open_deal = (
        deal_pair,
        any::<bool>(),
        prop_oneof![12 => 1u64..1_000, 1 => Just(0u64)],
        deal_payment,
        ttl,
        prop_oneof![15 => 1u8..32, 1 => Just(0u8)],
    )
        .prop_map(|(pair, sell, assets, payment, ttl, id)| Action::OpenDeal {
            pair,
            sell,
            assets,
            payment,
            ttl,
            id,
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
        2 => (0i64..2 * TIMELOCK_SECS).prop_map(Action::Warp),
        1 => (0..PAIRS.len(), any::<bool>(), 0u64..2_000)
            .prop_map(|(pair, seller_side, amount)| Action::Delegate { pair, seller_side, amount }),
        1 => (0..LISTINGS, prop::sample::select(vec![USD, USD, BRL, E0]))
            .prop_map(|(listing, currency)| Action::InitLedger { listing, currency }),
        // Only the fee mint has a fee vault to donate into.
        2 => (1u64..1_000_000).prop_map(|amount| Action::Donate { currency: USD, amount }),
        2 => (currency(), prop::bool::weighted(0.1))
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
        6 => open_deal,
        5 => (any::<usize>(), prop::bool::weighted(0.1))
            .prop_map(|(deal, intruder)| Action::AcceptDeal { deal, intruder }),
        2 => (any::<usize>(), prop::bool::weighted(0.1))
            .prop_map(|(deal, intruder)| Action::CancelDeal { deal, intruder }),
        2 => (any::<usize>(), prop::bool::weighted(0.1))
            .prop_map(|(deal, thief)| Action::ExpireDeal { deal, thief }),
        1 => (any::<usize>(), 1u64..1_000)
            .prop_map(|(deal, amount)| Action::DonateToDeal { deal, amount }),
        // Mostly freezes, mostly on the listings that accrue the most
        // cashback; USD checks that a Currency listing cannot be frozen.
        3 => (
            prop::sample::select(vec![E0, E1, E0, E1, FI, CR, USD]),
            prop::bool::weighted(0.7),
            prop::bool::weighted(0.1),
        )
            .prop_map(|(listing, freeze, intruder)| Action::CashbackFreeze {
                listing,
                freeze,
                intruder,
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
    /// One proposer/counterparty pair per `DEAL_PAIRS` entry.
    deal_parties: Vec<P2p>,
    /// The intruder's (asset, payment) accounts per deal pair.
    intruder_accounts: Vec<(Pubkey, Pubkey)>,
    deals: Vec<TrackedDeal>,
    /// Modelled balance of every deal party's token accounts (invariant 6).
    party_balances: HashMap<Pubkey, u64>,
}

impl World {
    /// `fee_program`: token program of the fee mint (USD).
    fn new(fee_program: Pubkey) -> Self {
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
        // USD is the fee mint (listed, with its fee vault); BRL and EUR are
        // plain currencies. Currency listings have no issuer; a stranger
        // stands in.
        mints.push(env.fee_currency(fee_program));
        issuers.push(env.funded_keypair());
        for program in [TOKEN_2022, SPL_TOKEN] {
            mints.push(env.listed_currency(program));
            issuers.push(env.funded_keypair());
        }

        let payer = env.admin.insecure_clone();
        let mut ledgers = HashSet::new();
        let mut issuer_dests = HashMap::new();
        for listing in [E0, E1, FI, CR] {
            env.init_cashback_ledger(&payer, mints[listing], mints[USD])
                .unwrap();
            ledgers.insert((listing, USD));
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
        let mut deal_parties = Vec::new();
        let mut intruder_accounts = Vec::new();
        let mut party_balances = HashMap::new();
        for &(asset, payment) in &DEAL_PAIRS {
            let p = env.p2p_parties(mints[asset], mints[payment], issuers[asset].insecure_clone());
            for (account, amount) in [
                (p.proposer_asset, P2P_ASSETS),
                (p.proposer_payment, P2P_PAYMENT),
                (p.counterparty_asset, P2P_ASSETS),
                (p.counterparty_payment, P2P_PAYMENT),
            ] {
                party_balances.insert(account, amount);
            }
            let intruder_asset =
                env.create_token_account(p.asset_program, p.asset_mint, intruder.pubkey(), P2P_ASSETS);
            let intruder_payment = env.create_token_account(
                p.payment_program,
                p.payment_mint,
                intruder.pubkey(),
                P2P_PAYMENT,
            );
            intruder_accounts.push((intruder_asset, intruder_payment));
            deal_parties.push(p);
        }
        World {
            deal_parties,
            intruder_accounts,
            deals: Vec::new(),
            party_balances,
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
                prefer_frozen,
            } => {
                let (asset, currency) = if prefer_frozen {
                    self.frozen_with_cashback().unwrap_or((asset, currency))
                } else {
                    (asset, currency)
                };
                self.withdraw_cashback(asset, currency, permille, intruder)?
            }
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
                    // Invariant 12: cashback is only ever in the fee mint.
                    prop_assert_eq!(currency, USD, "cashback ledger for a non-fee mint");
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
            Action::OpenDeal {
                pair,
                sell,
                assets,
                payment,
                ttl,
                id,
            } => self.open_deal(pair, sell, assets, payment, ttl, id)?,
            Action::AcceptDeal { deal, intruder } => self.accept_deal(deal, intruder)?,
            Action::CancelDeal { deal, intruder } => self.cancel_deal(deal, intruder)?,
            Action::ExpireDeal { deal, thief } => self.expire_deal(deal, thief)?,
            Action::DonateToDeal { deal, amount } => self.donate_to_deal(deal, amount),
            Action::CashbackFreeze {
                listing,
                freeze,
                intruder,
            } => self.cashback_freeze(listing, freeze, intruder)?,
        }
        Ok(())
    }

    /// A (listing, currency) whose listing is frozen and has cashback available.
    fn frozen_with_cashback(&self) -> Option<(usize, usize)> {
        let mut ledgers: Vec<(usize, usize)> = self.ledgers.iter().copied().collect();
        ledgers.sort();
        ledgers.into_iter().find(|&(listing, currency)| {
            self.listing(listing).cashback_frozen
                && self
                    .env
                    .fetch::<CashbackLedger>(&cashback_pda(&self.mints[listing], &self.mints[currency]))
                    .available()
                    .unwrap()
                    > 0
        })
    }

    /// Listing authority only, no timelock, any listing status, never paused;
    /// a `Currency` listing has nothing to freeze.
    fn cashback_freeze(
        &mut self,
        listing: usize,
        freeze: bool,
        intruder: bool,
    ) -> Result<(), TestCaseError> {
        let mint = self.mints[listing];
        let before = self.env.listing(&mint);
        let signer = if intruder {
            self.intruder.insecure_clone()
        } else {
            self.env.listing_authority.insecure_clone()
        };
        let res = if freeze {
            self.env.freeze_cashback(&signer, mint)
        } else {
            self.env.unfreeze_cashback(&signer, mint)
        };
        let expected_ok =
            !intruder && before.cashback_eligible && before.cashback_frozen != freeze;
        prop_assert_eq!(res.is_ok(), expected_ok, "cashback freeze {} -> {}", listing, freeze);
        let now_frozen = self.env.listing(&mint).cashback_frozen;
        if expected_ok {
            FREEZE_CHANGES.fetch_add(1, Relaxed);
            prop_assert_eq!(now_frozen, freeze);
        } else {
            prop_assert_eq!(now_frozen, before.cashback_frozen);
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
        prop_assert_eq!(currency, USD, "swept a non-fee mint");
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
                if order == [0u8; 16] {
                    ZERO_ID_REJECTED.fetch_add(1, Relaxed);
                }
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
        prop_assert!(order != [0u8; 16], "settled a zero match id");
        prop_assert!(assets > 0 && payment > 0);
        // Invariant 3.
        prop_assert_eq!(asset_listing.status, ListingStatus::Approved);
        // Invariant 8 (and 9: a Currency is never the asset leg).
        prop_assert_eq!(asset_listing.asset_class, AssetClass::Equity);
        // Invariant 10.
        prop_assert!(payment_listing.is_approved_currency());
        // Invariant 12: paid in the fee mint, so the seller pays the fee.
        prop_assert_eq!(payment_index, USD, "settled in a non-fee mint");

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
        let frozen = self.listing(asset).cashback_frozen;
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
            if frozen && !paused && !intruder && amount > 0 && amount <= available {
                FROZEN_WITHDRAW_REJECTED.fetch_add(1, Relaxed);
            }
            prop_assert_eq!(self.env.balance(&dest), before);
            return Ok(());
        }
        WITHDRAW_OK.fetch_add(1, Relaxed);
        prop_assert!(!paused, "cashback withdrawn while paused");
        prop_assert!(!frozen, "cashback withdrawn from a frozen listing");
        prop_assert!(!intruder, "cashback withdrawn by a non-issuer");
        prop_assert!(!CURRENCIES.contains(&asset), "cashback from a Currency listing");
        prop_assert_eq!(currency, USD, "cashback withdrawn in a non-fee mint");
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
        let available = if self.env.exists(&fee_ledger_pda(&mint)) {
            self.env.fee_ledger(&mint).protocol_available().unwrap()
        } else {
            0
        };
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
        prop_assert_eq!(currency, USD, "protocol fees withdrawn in a non-fee mint");
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

    // ---- P2P escrow (invariants 6 and 7) -------------------------------------

    /// Index of a tracked deal: mostly an open one, sometimes any (replays).
    fn pick_deal(&self, pick: usize) -> Option<usize> {
        let open: Vec<usize> = (0..self.deals.len())
            .filter(|&i| self.deals[i].status == DealStatus::Open)
            .collect();
        if !open.is_empty() && pick % 4 != 0 {
            Some(open[pick % open.len()])
        } else if !self.deals.is_empty() {
            Some(pick % self.deals.len())
        } else {
            None
        }
    }

    fn credit(&mut self, account: Pubkey, amount: u64) {
        *self.party_balances.get_mut(&account).unwrap() += amount;
    }

    fn debit(&mut self, account: Pubkey, amount: u64) {
        *self.party_balances.get_mut(&account).unwrap() -= amount;
    }

    #[allow(clippy::too_many_arguments)]
    fn open_deal(
        &mut self,
        pair: usize,
        sell: bool,
        assets: u64,
        payment: u64,
        ttl: i64,
        id: u8,
    ) -> Result<(), TestCaseError> {
        let (asset_index, payment_index) = DEAL_PAIRS[pair];
        let side = if sell { DealSide::Sell } else { DealSide::Buy };
        let id = deal_id(id as u128);
        let deal_key = self.deal_parties[pair].deal(id);
        let existed = self.env.exists(&deal_key);
        let config = self.config();
        let asset_listing = self.listing(asset_index);
        let payment_listing = self.listing(payment_index);
        let now = self.env.now();
        let expires_at = now.saturating_add(ttl);
        let rates = (config.fee_bps, config.cashback_bps);
        let fee = usd_fee(
            DEAL_PAIRS[pair],
            assets,
            payment,
            rates,
            asset_listing.cashback_eligible,
        );
        let expected_ok = !config.paused
            && assets > 0
            && payment > 0
            && ttl > 0
            && ttl <= MAX_DEAL_DURATION_SECS
            && asset_index != payment_index
            && asset_listing.status == ListingStatus::Approved
            && asset_listing.asset_class.is_p2p()
            && payment_listing.is_approved_currency()
            // Invariant 12: a USD leg whose receiver still gets something.
            && fee.is_some_and(|(_, split)| split.seller_receives > 0)
            // A zero deal id never opens.
            && id != [0u8; 16]
            && !existed;

        let p = &self.deal_parties[pair];
        let res = self
            .env
            .open_deal_at(p, id, side, assets, payment, expires_at);
        prop_assert_eq!(
            res.is_ok(),
            expected_ok,
            "open_deal pair {} {:?} {}/{} ttl {}: {:?}",
            pair,
            side,
            assets,
            payment,
            ttl,
            res.as_ref().err().map(|e| &e.err)
        );
        let meta = match res {
            Err(_) => {
                DEAL_OPEN_REJECTED.fetch_add(1, Relaxed);
                if id == [0u8; 16] {
                    ZERO_ID_REJECTED.fetch_add(1, Relaxed);
                }
                prop_assert_eq!(self.env.exists(&deal_key), existed);
                return Ok(());
            }
            Ok(meta) => meta,
        };
        DEAL_OPEN_OK.fetch_add(1, Relaxed);
        // Invariants 3, 8 and 10 at open.
        prop_assert_eq!(asset_listing.status, ListingStatus::Approved);
        prop_assert!(asset_listing.asset_class.is_p2p(), "Equity opened as a deal");
        prop_assert!(payment_listing.is_approved_currency());
        let e = &events::<DealOpened>(&meta)[0];
        prop_assert_eq!((e.asset_amount, e.payment_amount, e.expires_at), (assets, payment, expires_at));
        // The fee is part of the agreed terms: the config's rates at open.
        prop_assert_eq!((e.fee_bps, e.cashback_bps), rates);
        let deal = self.env.fetch_deal(&deal_key);
        prop_assert_eq!((deal.fee_bps, deal.cashback_bps), rates);

        let deposit = if sell { assets } else { payment };
        let account = self.deal_parties[pair].deposit_account(side);
        self.debit(account, deposit);
        self.deals.push(TrackedDeal {
            pair,
            id,
            side,
            assets,
            payment,
            rates,
            expires_at,
            status: DealStatus::Open,
            excess: 0,
        });
        Ok(())
    }

    fn accept_deal(&mut self, pick: usize, intruder: bool) -> Result<(), TestCaseError> {
        match self.pick_deal(pick) {
            Some(index) => self.accept_deal_at(index, intruder),
            None => Ok(()),
        }
    }

    fn accept_deal_at(&mut self, index: usize, intruder: bool) -> Result<(), TestCaseError> {
        let d = self.deals[index].clone();
        let (asset_index, payment_index) = DEAL_PAIRS[d.pair];
        let config = self.config();
        let asset_listing = self.listing(asset_index);
        let payment_listing = self.listing(payment_index);
        let now = self.env.now();
        // Only deals with a USD leg ever open. The fee is the one stored at
        // open, whatever fee changes executed since.
        let (leg, split) = usd_fee(
            DEAL_PAIRS[d.pair],
            d.assets,
            d.payment,
            d.rates,
            asset_listing.cashback_eligible,
        )
        .expect("an open deal has a USD leg");
        // Checked at open with the same rates, so it holds for good.
        prop_assert!(split.seller_receives > 0, "deal {:?} cannot pay its fee", d);
        let expected_ok = !intruder
            && d.status == DealStatus::Open
            && !config.paused
            && now < d.expires_at
            && asset_listing.status == ListingStatus::Approved
            && payment_listing.is_approved_currency();

        let p = &self.deal_parties[d.pair];
        let res = if intruder {
            let signer = self.intruder.insecure_clone();
            let (asset_account, payment_account) = self.intruder_accounts[d.pair];
            let mut a = self.env.accept_deal_accounts(p, d.id);
            a.counterparty = signer.pubkey();
            a.counterparty_asset = asset_account;
            a.counterparty_payment = payment_account;
            self.env.accept_deal_with(&signer, &a)
        } else {
            self.env.accept_deal(p, d.id)
        };
        // Invariant 7: never after expires_at.
        if now >= d.expires_at {
            prop_assert!(res.is_err(), "deal accepted at {now} >= expires_at {}", d.expires_at);
        }
        prop_assert_eq!(
            res.is_ok(),
            expected_ok,
            "accept_deal {:?}: {:?}",
            d,
            res.as_ref().err().map(|e| &e.err)
        );
        let meta = match res {
            Err(_) => {
                DEAL_ACCEPT_REJECTED.fetch_add(1, Relaxed);
                return Ok(());
            }
            Ok(meta) => meta,
        };
        DEAL_ACCEPT_OK.fetch_add(1, Relaxed);
        // Invariants 3, 8, 9 and 10 at execution.
        prop_assert_eq!(asset_listing.status, ListingStatus::Approved);
        prop_assert!(asset_listing.asset_class.is_p2p());
        prop_assert!(payment_listing.is_approved_currency());
        let e = &events::<DealExecuted>(&meta)[0];
        prop_assert_eq!((e.fee, e.cashback), (split.fee, split.cashback));
        prop_assert_eq!(e.fee_mint, self.mints[USD]);
        prop_assert_eq!(e.vault_excess, d.excess);
        if asset_listing.asset_class == AssetClass::Currency {
            prop_assert_eq!(e.cashback, 0, "cashback on a Currency deal");
        }
        // Invariant 1, on the USD leg.
        let usd_amount = if leg == FeeLeg::Asset { d.assets } else { d.payment };
        prop_assert_eq!(usd_amount, split.seller_receives + split.fee);

        // Invariants 6 and 12: the deposit (and any excess) went to the
        // counterparty; the receiver of the USD leg got it minus the fee, the
        // other leg moved in full.
        let (asset_fee, payment_fee) = match leg {
            FeeLeg::Asset => (split.fee, 0),
            FeeLeg::Payment => (0, split.fee),
        };
        let p = &self.deal_parties[d.pair];
        let (pa, pp, ca, cp) = (
            p.proposer_asset,
            p.proposer_payment,
            p.counterparty_asset,
            p.counterparty_payment,
        );
        match d.side {
            DealSide::Sell => {
                self.credit(ca, d.assets - asset_fee + d.excess);
                self.debit(cp, d.payment);
                self.credit(pp, d.payment - payment_fee);
            }
            DealSide::Buy => {
                self.debit(ca, d.assets);
                self.credit(pa, d.assets - asset_fee);
                self.credit(cp, d.payment - payment_fee + d.excess);
            }
        }
        let entry = self.model.get_mut(&USD).unwrap();
        entry.0 += split.fee;
        entry.1 += split.cashback;
        self.deals[index].status = DealStatus::Executed;
        Ok(())
    }

    fn cancel_deal(&mut self, pick: usize, intruder: bool) -> Result<(), TestCaseError> {
        match self.pick_deal(pick) {
            Some(index) => self.cancel_deal_at(index, intruder),
            None => Ok(()),
        }
    }

    fn cancel_deal_at(&mut self, index: usize, intruder: bool) -> Result<(), TestCaseError> {
        let d = self.deals[index].clone();
        let p = &self.deal_parties[d.pair];
        let res = if intruder {
            let signer = self.intruder.insecure_clone();
            let (asset_account, payment_account) = self.intruder_accounts[d.pair];
            let mut a = self.env.refund_accounts(p, d.id);
            a.proposer = signer.pubkey();
            a.proposer_refund = match d.side {
                DealSide::Sell => asset_account,
                DealSide::Buy => payment_account,
            };
            self.env.cancel_deal_with(&signer, &a)
        } else {
            self.env.cancel_deal(p, d.id)
        };
        // Never blocked by a pause or by the listings' status.
        let expected_ok = !intruder && d.status == DealStatus::Open;
        prop_assert_eq!(
            res.is_ok(),
            expected_ok,
            "cancel_deal {:?}: {:?}",
            d,
            res.as_ref().err().map(|e| &e.err)
        );
        if res.is_err() {
            DEAL_REFUND_REJECTED.fetch_add(1, Relaxed);
            return Ok(());
        }
        DEAL_CANCEL_OK.fetch_add(1, Relaxed);
        self.refunded(index, DealStatus::Cancelled);
        Ok(())
    }

    fn expire_deal(&mut self, pick: usize, thief: bool) -> Result<(), TestCaseError> {
        match self.pick_deal(pick) {
            Some(index) => self.expire_deal_with_thief(index, thief),
            None => Ok(()),
        }
    }

    fn expire_deal_at(&mut self, index: usize) -> Result<(), TestCaseError> {
        self.expire_deal_with_thief(index, false)
    }

    fn expire_deal_with_thief(&mut self, index: usize, thief: bool) -> Result<(), TestCaseError> {
        let d = self.deals[index].clone();
        let now = self.env.now();
        let p = &self.deal_parties[d.pair];
        let res = if thief {
            // Permissionless crank trying to redirect the refund.
            let signer = self.intruder.insecure_clone();
            let (asset_account, payment_account) = self.intruder_accounts[d.pair];
            let mut a = self.env.refund_accounts(p, d.id);
            a.proposer_refund = match d.side {
                DealSide::Sell => asset_account,
                DealSide::Buy => payment_account,
            };
            self.env.expire_deal_with(&signer, &a)
        } else {
            self.env.expire_deal(p, d.id)
        };
        // Invariant 7: never before expires_at.
        if now < d.expires_at {
            prop_assert!(res.is_err(), "deal expired at {now} < expires_at {}", d.expires_at);
        }
        let expected_ok = !thief && d.status == DealStatus::Open && now >= d.expires_at;
        prop_assert_eq!(
            res.is_ok(),
            expected_ok,
            "expire_deal {:?} at {}: {:?}",
            d,
            now,
            res.as_ref().err().map(|e| &e.err)
        );
        if res.is_err() {
            DEAL_REFUND_REJECTED.fetch_add(1, Relaxed);
            return Ok(());
        }
        DEAL_EXPIRE_OK.fetch_add(1, Relaxed);
        self.refunded(index, DealStatus::Expired);
        Ok(())
    }

    /// Invariant 6: the deposit (and any excess) went back to the proposer.
    fn refunded(&mut self, index: usize, status: DealStatus) {
        let d = self.deals[index].clone();
        let deposit = match d.side {
            DealSide::Sell => d.assets,
            DealSide::Buy => d.payment,
        };
        let account = self.deal_parties[d.pair].deposit_account(d.side);
        self.credit(account, deposit + d.excess);
        self.deals[index].status = status;
    }

    fn donate_to_deal(&mut self, pick: usize, amount: u64) {
        if let Some(index) = self.pick_deal(pick) {
            self.donate_to_deal_at(index, amount);
        }
    }

    fn donate_to_deal_at(&mut self, index: usize, amount: u64) {
        let d = &self.deals[index];
        if d.status != DealStatus::Open {
            return; // the vault is closed
        }
        let p = &self.deal_parties[d.pair];
        let mint = match d.side {
            DealSide::Sell => p.asset_mint,
            DealSide::Buy => p.payment_mint,
        };
        let vault = p.vault(d.id);
        self.env.donate(mint, vault, amount);
        self.deals[index].excess += amount;
    }

    /// Invariant 6 after every step: an open deal's vault holds exactly the
    /// deposit (plus donations); a resolved deal has no vault; every party
    /// account holds what the model says (so nothing was stranded or paid
    /// twice).
    fn check_deals(&self) -> Result<(), TestCaseError> {
        for d in &self.deals {
            let p = &self.deal_parties[d.pair];
            let deal = self.env.fetch_deal(&p.deal(d.id));
            prop_assert_eq!(deal.status, d.status, "deal status");
            let vault = p.vault(d.id);
            if d.status == DealStatus::Open {
                let deposit = match d.side {
                    DealSide::Sell => d.assets,
                    DealSide::Buy => d.payment,
                };
                prop_assert_eq!(self.env.balance(&vault), deposit + d.excess, "vault balance");
            } else {
                prop_assert!(!self.env.exists(&vault), "vault of a resolved deal still open");
            }
        }
        for (account, expected) in &self.party_balances {
            prop_assert_eq!(self.env.balance(account), *expected, "party balance");
        }
        Ok(())
    }

    fn check_invariants(&self) -> Result<(), TestCaseError> {
        // Invariant 5.
        let config = self.config();
        prop_assert!(config.fee_bps <= MAX_FEE_BPS, "fee_bps {}", config.fee_bps);
        prop_assert!(config.cashback_bps <= BPS_DENOMINATOR);

        // Invariant 12: one fee mint, and no fee account for any other mint.
        prop_assert_eq!(config.fee_mint, self.mints[USD]);
        for (index, mint) in self.mints.iter().enumerate() {
            if index != USD {
                prop_assert!(!self.env.exists(&fee_ledger_pda(mint)), "fee ledger for {index}");
                prop_assert!(!self.env.exists(&fee_vault_pda(mint)), "fee vault for {index}");
            }
        }
        for currency in [BRL, EUR] {
            prop_assert_eq!(self.model[&currency], (0, 0), "fees modelled in a non-fee mint");
            prop_assert_eq!(self.excess[&currency], 0);
        }

        {
            let currency = USD;
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

        // Invariant 6.
        self.check_deals()?;

        // Invariant 9: no Currency listing ever has a cashback ledger.
        for listing in CURRENCIES {
            for currency in [USD, BRL, EUR, E0] {
                prop_assert!(!self
                    .env
                    .exists(&cashback_pda(&self.mints[listing], &self.mints[currency])));
            }
        }
        Ok(())
    }
}

/// The USD (fee-mint) leg of a deal and the fee on it (spec §3.4): the payment
/// when it is USD, else the asset when it is USD; `None` when the deal has no
/// USD leg (it can never open).
fn usd_fee(
    (asset_index, payment_index): (usize, usize),
    assets: u64,
    payment: u64,
    (fee_bps, cashback_bps): (u16, u16),
    eligible: bool,
) -> Option<(FeeLeg, PaymentSplit)> {
    let (leg, amount) = if payment_index == USD {
        (FeeLeg::Payment, payment)
    } else if asset_index == USD {
        (FeeLeg::Asset, assets)
    } else {
        return None;
    };
    Some((leg, split_payment(amount, fee_bps, cashback_bps, eligible).unwrap()))
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
    let strategy = (any::<bool>(), prop::collection::vec(action(), 1..80));
    let result = runner.run(&strategy, |(fee_mint_2022, actions)| {
        let mut world = World::new(if fee_mint_2022 { TOKEN_2022 } else { SPL_TOKEN });
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
    let (o_ok, o_rejected) = (DEAL_OPEN_OK.load(Relaxed), DEAL_OPEN_REJECTED.load(Relaxed));
    let (a_ok, a_rejected) = (DEAL_ACCEPT_OK.load(Relaxed), DEAL_ACCEPT_REJECTED.load(Relaxed));
    let (cancelled, expired) = (DEAL_CANCEL_OK.load(Relaxed), DEAL_EXPIRE_OK.load(Relaxed));
    let r_rejected = DEAL_REFUND_REJECTED.load(Relaxed);
    let (freezes, frozen_rejected) =
        (FREEZE_CHANGES.load(Relaxed), FROZEN_WITHDRAW_REJECTED.load(Relaxed));
    let zero_ids = ZERO_ID_REJECTED.load(Relaxed);
    println!(
        "settle ok {ok} / rejected {rejected}; withdraw ok {w_ok} / rejected {w_rejected}; \
         sweep ok {s_ok} / rejected {s_rejected}; batches executed {issued}; \
         burns {burned}; config changes executed {changes}; \
         deals opened {o_ok} / rejected {o_rejected}; accepted {a_ok} / rejected {a_rejected}; \
         cancelled {cancelled}; expired {expired}; refunds rejected {r_rejected}; \
         cashback freeze changes {freezes}; withdrawals stopped by a freeze {frozen_rejected}; \
         zero ids rejected {zero_ids}"
    );
    // Not vacuous: both outcomes of every guarded path were exercised.
    assert!(ok > 0 && rejected > 0, "settle outcomes not exercised");
    assert!(w_ok > 0 && w_rejected > 0, "withdraw outcomes not exercised");
    assert!(s_ok > 0 && s_rejected > 0, "sweep outcomes not exercised");
    assert!(issued > 0 && burned > 0, "issuance paths not exercised");
    assert!(o_ok > 0 && o_rejected > 0, "open_deal outcomes not exercised");
    assert!(a_ok > 0 && a_rejected > 0, "accept_deal outcomes not exercised");
    assert!(
        cancelled > 0 && expired > 0 && r_rejected > 0,
        "refund outcomes not exercised"
    );
    assert!(freezes > 0 && frozen_rejected > 0, "cashback freeze not exercised");
    assert!(zero_ids > 0, "zero match and deal ids not exercised");
}

/// Deterministic sweep of invariants 6 and 7: on every valid P2P pair and both
/// sides, one deal is accepted, one cancelled and one expired, with donations
/// in between; the invalid pairs never open.
#[test]
fn every_deposit_ends_in_exactly_one_place() {
    let mut world = World::new(TOKEN_2022);
    // Ids start at 1: a zero deal id never opens.
    let mut id = 1u8;
    for pair in 0..DEAL_PAIRS.len() {
        for sell in [true, false] {
            for _ in 0..3 {
                world.open_deal(pair, sell, 5, 1_000_000, 1_000, id).unwrap();
                id += 1;
            }
        }
    }
    world.check_invariants().unwrap();
    assert_eq!(world.deals.len(), VALID_DEAL_PAIRS * 6, "only valid pairs open");
    for i in 0..world.deals.len() {
        world.donate_to_deal_at(i, 3);
    }
    // Before expiry: accept every third deal, cancel the next; expiry fails.
    let open: Vec<usize> = (0..world.deals.len()).collect();
    for &i in &open {
        world.expire_deal_at(i).unwrap();
        match i % 3 {
            0 => world.accept_deal_at(i, false).unwrap(),
            1 => world.cancel_deal_at(i, false).unwrap(),
            _ => {}
        }
        world.check_invariants().unwrap();
    }
    // After expiry: accept fails, expire refunds the rest.
    world.env.warp_secs(1_000);
    for &i in &open {
        world.accept_deal_at(i, false).unwrap();
        world.expire_deal_at(i).unwrap();
        world.check_invariants().unwrap();
    }
    let statuses: Vec<DealStatus> = world.deals.iter().map(|d| d.status).collect();
    for (i, status) in statuses.iter().enumerate() {
        let expected = match i % 3 {
            0 => DealStatus::Executed,
            1 => DealStatus::Cancelled,
            _ => DealStatus::Expired,
        };
        assert_eq!(*status, expected, "deal {i}");
    }
}

/// Deterministic sweep of invariants 8, 9, 10 and 12: every invalid market
/// (non-Equity asset, Currency as asset, Equity as payment, a payment
/// currency that is not the fee mint) is attempted with healthy delegations
/// and listings. None may settle.
#[test]
fn invalid_markets_never_settle() {
    let mut world = World::new(SPL_TOKEN);
    for (pair, _) in PAIRS.iter().enumerate().skip(VALID_PAIRS) {
        for id in 0..3u8 {
            world
                .settle(pair, 1, 1_000_000, 100 + pair as u8 * 3 + id, false)
                .unwrap();
        }
    }
    world.check_invariants().unwrap();
    assert_eq!(world.env.fee_ledger(&world.mints[USD]).fees_collected, 0);
}
