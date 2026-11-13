//! On-chain listing fee (spec §3.13): `quote_listing` (listing authority),
//! `pay_listing_fee` (issuer), and `approve_listing` limited to reinstatement.

mod common;

use anchor_lang::{error::ErrorCode as AnchorError, prelude::Pubkey, AccountSerialize, Discriminator};
use common::*;
use litesvm::types::TransactionMetadata;
use niara_exchange::{
    error::ExchangeError,
    events::{ListingApproved, ListingFeePaid, ListingQuoted},
    AssetClass, Config, ConfigChange, Listing, ListingStatus, MAX_LISTING_FEE_BPS,
    MAX_QUOTE_DURATION_SECS,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

/// SPL Token `InsufficientFunds`.
const TOKEN_INSUFFICIENT_FUNDS: u32 = 1;

/// A submitted (`Pending`) listing of `class`, with the fee currency (USDT)
/// created under `fee_program` and its fee vault ready.
struct Pending {
    env: Env,
    issuer: Keypair,
    mint: Pubkey,
    authority: Keypair,
}

fn pending_with(fee_program: Pubkey, class: AssetClass) -> Pending {
    let mut env = Env::initialized();
    env.fee_currency(fee_program);
    let issuer = env.funded_keypair();
    let mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
    env.submit_listing(&issuer, mint, class, DOC_HASH).unwrap();
    let authority = env.listing_authority.insecure_clone();
    Pending {
        env,
        issuer,
        mint,
        authority,
    }
}

fn pending() -> Pending {
    pending_with(SPL_TOKEN, AssetClass::Equity)
}

impl Pending {
    fn quote(&mut self, valuation: u64, bps: u16, expires_at: i64) -> litesvm::types::TransactionResult {
        let authority = self.authority.insecure_clone();
        self.env.quote_listing(&authority, self.mint, valuation, bps, expires_at)
    }
    fn quote_default(&mut self) {
        self.env.quote_default(self.mint).unwrap();
    }
    fn pay(&mut self) -> litesvm::types::TransactionResult {
        let issuer = self.issuer.insecure_clone();
        self.env.pay_listing_fee(&issuer, self.mint)
    }
    fn listing(&self) -> Listing {
        self.env.listing(&self.mint)
    }
    fn status(&self) -> ListingStatus {
        self.listing().status
    }
    fn vault(&self) -> u64 {
        self.env.balance(&fee_vault_pda(&self.env.fee_mint))
    }
    fn listing_fees(&self) -> u64 {
        self.env.fee_ledger(&self.env.fee_mint).listing_fees_collected
    }
}

/// Discriminators of the `emit_cpi!` events of a transaction, in order.
fn event_order(meta: &TransactionMetadata) -> Vec<Vec<u8>> {
    meta.inner_instructions
        .iter()
        .flatten()
        .filter_map(|inner| {
            let data = inner.instruction.data.as_slice();
            data.strip_prefix(anchor_lang::event::EVENT_IX_TAG_LE)
                .map(|d| d[..8].to_vec())
        })
        .collect()
}

/// Rewrites a listing's status in place (to reach states no instruction
/// produces, for defense-in-depth checks).
fn force_status(env: &mut Env, mint: &Pubkey, status: ListingStatus) {
    let address = listing_pda(mint);
    let mut listing: Listing = env.fetch(&address);
    listing.status = status;
    let mut data = Vec::new();
    listing.try_serialize(&mut data).unwrap();
    let mut account = env.svm.get_account(&address).unwrap();
    account.data[..data.len()].copy_from_slice(&data);
    env.svm.set_account(address, account).unwrap();
}

// ---- success path ---------------------------------------------------------------

#[test]
fn quote_then_pay_approves_each_issuer_class() {
    for fee_program in [SPL_TOKEN, TOKEN_2022] {
        for class in [AssetClass::Equity, AssetClass::FixedIncome, AssetClass::Credit] {
            let mut s = pending_with(fee_program, class);
            let fee_mint = s.env.fee_mint;

            // Quote.
            let expires_at = s.env.now() + QUOTE_SECS;
            let meta = s.quote(VALUATION, LISTING_FEE_BPS, expires_at).unwrap();
            let l = s.listing();
            assert_eq!(l.status, ListingStatus::Quoted);
            assert_eq!(
                (l.valuation, l.listing_fee_bps, l.listing_fee, l.listing_fee_mint),
                (VALUATION, LISTING_FEE_BPS, LISTING_FEE, fee_mint)
            );
            assert_eq!((l.quote_expires_at, l.listing_fee_paid, l.approved_at), (expires_at, 0, 0));
            let e = &events::<ListingQuoted>(&meta)[0];
            assert_eq!(e.listing, listing_pda(&s.mint));
            assert_eq!((e.asset_mint, e.fee_mint), (s.mint, fee_mint));
            assert_eq!((e.valuation, e.listing_fee_bps, e.listing_fee), (VALUATION, LISTING_FEE_BPS, LISTING_FEE));
            assert_eq!(e.expires_at, expires_at);

            // Pay from an account holding a little more than the fee.
            s.env.warp_secs(10);
            let now = s.env.now();
            let source = s.env.fee_mint_account(s.issuer.pubkey(), LISTING_FEE + 5);
            let a = s.env.pay_accounts(s.issuer.pubkey(), s.mint, source);
            let issuer = s.issuer.insecure_clone();
            let meta = s.env.pay_listing_fee_with(&issuer, &a).unwrap();

            let l = s.listing();
            assert_eq!(l.status, ListingStatus::Approved);
            assert_eq!((l.approved_at, l.listing_fee_paid), (now, LISTING_FEE));
            assert_eq!(s.env.balance(&source), 5);
            assert_eq!(s.vault(), LISTING_FEE);
            // Protocol revenue only: no trading fee, no cashback.
            let ledger = s.env.fee_ledger(&fee_mint);
            assert_eq!(ledger.listing_fees_collected, LISTING_FEE);
            assert_eq!((ledger.fees_collected, ledger.cashback_accrued), (0, 0));
            s.env.assert_vault_matches_ledger(&fee_mint);

            let paid = &events::<ListingFeePaid>(&meta)[0];
            assert_eq!(paid.listing, listing_pda(&s.mint));
            assert_eq!((paid.asset_mint, paid.issuer, paid.fee_mint), (s.mint, s.issuer.pubkey(), fee_mint));
            assert_eq!((paid.valuation, paid.listing_fee_bps, paid.amount), (VALUATION, LISTING_FEE_BPS, LISTING_FEE));
            let approved = &events::<ListingApproved>(&meta)[0];
            assert_eq!((approved.asset_mint, approved.issuer), (s.mint, s.issuer.pubkey()));
            assert_eq!((approved.asset_class, approved.approved_at), (class, now));
            assert!(approved.cashback_eligible);
            assert_eq!(approved.doc_hash, DOC_HASH);
            // ListingFeePaid first, then ListingApproved, and nothing else.
            assert_eq!(
                event_order(&meta),
                vec![ListingFeePaid::DISCRIMINATOR.to_vec(), ListingApproved::DISCRIMINATOR.to_vec()]
            );
        }
    }
}

/// `listing_fee = ceil(valuation * bps / 10_000)`: never zero, rounded up.
#[test]
fn quoted_fee_rounds_up() {
    for (valuation, bps, fee) in [
        (10_001, 100, 101),
        (1, 1, 1),
        (1_000_000_000, 10, 1_000_000), // 0.1%, the pricing floor
        (u64::MAX, MAX_LISTING_FEE_BPS, u64::MAX / 100 + 1),
    ] {
        let mut s = pending();
        let expires_at = s.env.now() + QUOTE_SECS;
        s.quote(valuation, bps, expires_at).unwrap();
        assert_eq!(s.listing().listing_fee, fee, "{valuation} at {bps} bps");
    }
    // And that is what is charged.
    let mut s = pending();
    let expires_at = s.env.now() + QUOTE_SECS;
    s.quote(10_001, 100, expires_at).unwrap();
    s.pay().unwrap();
    assert_eq!((s.vault(), s.listing().listing_fee_paid), (101, 101));
}

// ---- quote_listing refusals ---------------------------------------------------------

/// The cap is 1% (100 bps); 0 bps is refused too, so a quote is never free.
/// The 0.1% floor is pricing policy, not enforced.
#[test]
fn quote_enforces_the_cap_and_a_non_zero_rate() {
    assert_eq!(MAX_LISTING_FEE_BPS, 100);
    let mut s = pending();
    let expires_at = s.env.now() + QUOTE_SECS;
    for bps in [0, MAX_LISTING_FEE_BPS + 1, u16::MAX] {
        assert_custom_err(s.quote(VALUATION, bps, expires_at), ExchangeError::InvalidListingFeeBps);
        assert_eq!(s.status(), ListingStatus::Pending);
    }
    for bps in [1, 9, MAX_LISTING_FEE_BPS] {
        s.quote(VALUATION, bps, expires_at).unwrap();
        assert_eq!(s.listing().listing_fee_bps, bps);
    }
}

#[test]
fn quote_rejects_zero_valuation() {
    let mut s = pending();
    let expires_at = s.env.now() + QUOTE_SECS;
    assert_custom_err(s.quote(0, LISTING_FEE_BPS, expires_at), ExchangeError::ZeroValuation);
    assert_eq!(s.status(), ListingStatus::Pending);
    // Also on a re-quote.
    s.quote_default();
    assert_custom_err(s.quote(0, LISTING_FEE_BPS, expires_at), ExchangeError::ZeroValuation);
    assert_eq!(s.listing().valuation, VALUATION);
}

/// `now < expires_at <= now + 30 days`.
#[test]
fn quote_expiry_boundary() {
    assert_eq!(MAX_QUOTE_DURATION_SECS, 30 * 24 * 60 * 60);
    let mut s = pending();
    let now = s.env.now();
    for expires_at in [now - 1, now, now + MAX_QUOTE_DURATION_SECS + 1, i64::MAX] {
        assert_custom_err(s.quote(VALUATION, LISTING_FEE_BPS, expires_at), ExchangeError::InvalidExpiry);
        assert_eq!(s.status(), ListingStatus::Pending);
    }
    s.quote(VALUATION, LISTING_FEE_BPS, now + 1).unwrap();
    s.quote(VALUATION, LISTING_FEE_BPS, now + MAX_QUOTE_DURATION_SECS).unwrap();
    assert_eq!(s.listing().quote_expires_at, now + MAX_QUOTE_DURATION_SECS);
}

#[test]
fn only_the_listing_authority_quotes() {
    let mut s = pending();
    let intruders = [
        s.issuer.insecure_clone(),
        s.env.admin.insecure_clone(),
        s.env.treasury.insecure_clone(),
        s.env.funded_keypair(),
    ];
    let expires_at = s.env.now() + QUOTE_SECS;
    for intruder in intruders {
        let res = s.env.quote_listing(&intruder, s.mint, VALUATION, LISTING_FEE_BPS, expires_at);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_eq!(s.status(), ListingStatus::Pending);
}

/// After a timelocked rotation only the new listing authority can quote.
#[test]
fn rotated_listing_authority_quotes() {
    let mut s = pending();
    let admin = s.env.admin.insecure_clone();
    let new = s.env.funded_keypair();
    let change = ConfigChange::ListingAuthority(new.pubkey());
    s.env.propose(&admin, change).unwrap();
    s.env.warp_secs(TIMELOCK_SECS);
    s.env.execute(&admin, &change).unwrap();
    let expires_at = s.env.now() + QUOTE_SECS;
    let old = s.authority.insecure_clone();
    let res = s.env.quote_listing(&old, s.mint, VALUATION, LISTING_FEE_BPS, expires_at);
    assert_custom_err(res, ExchangeError::Unauthorized);
    s.env.quote_listing(&new, s.mint, VALUATION, LISTING_FEE_BPS, expires_at).unwrap();
}

/// Only `Pending` and `Quoted` can be quoted: never after payment.
#[test]
fn quote_refuses_approved_suspended_and_delisted() {
    let mut s = pending();
    s.quote_default();
    s.pay().unwrap();
    let authority = s.authority.insecure_clone();
    let expires_at = s.env.now() + QUOTE_SECS;
    assert_custom_err(s.quote(VALUATION, 1, expires_at), ExchangeError::InvalidStatusTransition);
    s.env.suspend(&authority, s.mint).unwrap();
    assert_custom_err(s.quote(VALUATION, 1, expires_at), ExchangeError::InvalidStatusTransition);
    s.env.delist(&authority, s.mint).unwrap();
    assert_custom_err(s.quote(VALUATION, 1, expires_at), ExchangeError::InvalidStatusTransition);
    assert_eq!(s.listing().listing_fee_bps, LISTING_FEE_BPS);
}

/// Currencies pay no listing fee: the class is checked before the status,
/// in every status (`Pending` and `Quoted` are forced, since no instruction
/// puts a currency there).
#[test]
fn currency_listings_are_never_quoted() {
    let mut env = Env::initialized();
    let usdt = env.fee_currency(SPL_TOKEN);
    let brl = env.listed_currency(SPL_TOKEN);
    let authority = env.listing_authority.insecure_clone();
    let expires_at = env.now() + QUOTE_SECS;
    for mint in [usdt, brl] {
        let res = env.quote_listing(&authority, mint, VALUATION, LISTING_FEE_BPS, expires_at);
        assert_custom_err(res, ExchangeError::CurrencyHasNoListingFee);
    }
    env.suspend(&authority, brl).unwrap();
    let res = env.quote_listing(&authority, brl, VALUATION, LISTING_FEE_BPS, expires_at);
    assert_custom_err(res, ExchangeError::CurrencyHasNoListingFee);

    for status in [ListingStatus::Pending, ListingStatus::Quoted] {
        force_status(&mut env, &brl, status);
        let res = env.quote_listing(&authority, brl, VALUATION, LISTING_FEE_BPS, expires_at);
        assert_custom_err(res, ExchangeError::CurrencyHasNoListingFee);
    }
    // A currency has no issuer that could sign a payment either.
    let l = env.listing(&brl);
    assert_eq!((l.issuer, l.listing_fee, l.listing_fee_paid), (Pubkey::default(), 0, 0));
}

// ---- re-quoting -------------------------------------------------------------------

/// A re-quote replaces every field; the payment charges the latest quote.
#[test]
fn requote_replaces_the_quote_until_paid() {
    let mut s = pending();
    let first = s.env.now() + QUOTE_SECS;
    s.quote(VALUATION, LISTING_FEE_BPS, first).unwrap();
    let second = s.env.now() + 2 * QUOTE_SECS;
    let meta = s.quote(2_000_000_000, 10, second).unwrap();
    let l = s.listing();
    assert_eq!(l.status, ListingStatus::Quoted);
    assert_eq!((l.valuation, l.listing_fee_bps, l.listing_fee), (2_000_000_000, 10, 2_000_000));
    assert_eq!(l.quote_expires_at, second);
    assert_eq!(events::<ListingQuoted>(&meta)[0].listing_fee, 2_000_000);

    s.pay().unwrap();
    assert_eq!((s.vault(), s.listing_fees()), (2_000_000, 2_000_000));
    assert_eq!(s.listing().listing_fee_paid, 2_000_000);
}

/// An expired quote can be replaced and the new one paid.
#[test]
fn expired_quote_can_be_requoted() {
    let mut s = pending();
    s.quote_default();
    s.env.warp_secs(QUOTE_SECS);
    assert_custom_err(s.pay(), ExchangeError::QuoteExpired);
    s.quote_default();
    s.pay().unwrap();
    assert_eq!(s.status(), ListingStatus::Approved);
}

// ---- pay_listing_fee refusals ------------------------------------------------------

/// Payable while `now < quote_expires_at`, from `quote_expires_at` on expired.
#[test]
fn pay_expiry_boundary() {
    for (wait, ok) in [(99, true), (100, false), (101, false)] {
        let mut s = pending();
        let expires_at = s.env.now() + 100;
        s.quote(VALUATION, LISTING_FEE_BPS, expires_at).unwrap();
        s.env.warp_secs(wait);
        let res = s.pay();
        if ok {
            res.unwrap();
            assert_eq!(s.status(), ListingStatus::Approved);
        } else {
            assert_custom_err(res, ExchangeError::QuoteExpired);
            assert_eq!((s.status(), s.vault()), (ListingStatus::Quoted, 0));
        }
    }
}

/// An unquoted listing (no quote mint recorded yet) fails on the status,
/// not on the mint.
#[test]
fn pay_requires_a_quote() {
    let mut s = pending();
    assert_custom_err(s.pay(), ExchangeError::ListingNotQuoted);
    assert_eq!((s.status(), s.vault()), (ListingStatus::Pending, 0));
    // Rejected before any quote: still not payable.
    let authority = s.authority.insecure_clone();
    s.env.delist(&authority, s.mint).unwrap();
    assert_custom_err(s.pay(), ExchangeError::ListingNotQuoted);
}

/// The fee is charged once per listing: a second payment and a re-quote are
/// both refused, and the vault holds one fee.
#[test]
fn double_payment_is_refused() {
    let mut s = pending();
    s.quote_default();
    s.pay().unwrap();
    assert_custom_err(s.pay(), ExchangeError::ListingNotQuoted);
    let expires_at = s.env.now() + QUOTE_SECS;
    assert_custom_err(s.quote(VALUATION, LISTING_FEE_BPS, expires_at), ExchangeError::InvalidStatusTransition);
    assert_eq!((s.vault(), s.listing_fees()), (LISTING_FEE, LISTING_FEE));

    // Not after suspension and reinstatement either.
    let authority = s.authority.insecure_clone();
    s.env.suspend(&authority, s.mint).unwrap();
    assert_custom_err(s.pay(), ExchangeError::ListingNotQuoted);
    s.env.approve(&authority, s.mint).unwrap();
    assert_custom_err(s.pay(), ExchangeError::ListingNotQuoted);
    assert_eq!((s.vault(), s.listing_fees()), (LISTING_FEE, LISTING_FEE));
    assert_eq!(s.listing().listing_fee_paid, LISTING_FEE);
}

/// Only `listing.issuer` pays (and so approves) its listing.
#[test]
fn only_the_issuer_pays() {
    let mut s = pending();
    s.quote_default();
    let intruders = [
        s.authority.insecure_clone(),
        s.env.admin.insecure_clone(),
        s.env.treasury.insecure_clone(),
        s.env.funded_keypair(),
    ];
    for intruder in intruders {
        let source = s.env.fee_mint_account(intruder.pubkey(), LISTING_FEE);
        let a = s.env.pay_accounts(intruder.pubkey(), s.mint, source);
        assert_custom_err(s.env.pay_listing_fee_with(&intruder, &a), ExchangeError::Unauthorized);
    }
    assert_eq!((s.status(), s.vault()), (ListingStatus::Quoted, 0));
}

/// The issuer pays from its own account of the fee mint, into the fee
/// mint's vault.
#[test]
fn pay_checks_the_source_and_the_vault() {
    let mut s = pending();
    s.quote_default();
    let issuer = s.issuer.insecure_clone();

    // Someone else's account.
    let stranger = Pubkey::new_unique();
    let source = s.env.fee_mint_account(stranger, LISTING_FEE);
    let a = s.env.pay_accounts(issuer.pubkey(), s.mint, source);
    assert_custom_err(s.env.pay_listing_fee_with(&issuer, &a), AnchorError::ConstraintTokenOwner);

    // An account of another mint.
    let brl = s.env.listed_currency(SPL_TOKEN);
    let source = s.env.create_token_account(SPL_TOKEN, brl, issuer.pubkey(), LISTING_FEE);
    let a = s.env.pay_accounts(issuer.pubkey(), s.mint, source);
    assert_custom_err(s.env.pay_listing_fee_with(&issuer, &a), AnchorError::ConstraintTokenMint);

    // Another account of the fee mint in place of the vault.
    let source = s.env.fee_mint_account(issuer.pubkey(), LISTING_FEE);
    let mut a = s.env.pay_accounts(issuer.pubkey(), s.mint, source);
    a.fee_vault = s.env.fee_mint_account(Pubkey::new_unique(), 0);
    assert_custom_err(s.env.pay_listing_fee_with(&issuer, &a), AnchorError::ConstraintSeeds);

    // Not enough on the account: the token program refuses, nothing changes.
    let source = s.env.fee_mint_account(issuer.pubkey(), LISTING_FEE - 1);
    let a = s.env.pay_accounts(issuer.pubkey(), s.mint, source);
    assert_custom_err(s.env.pay_listing_fee_with(&issuer, &a), TOKEN_INSUFFICIENT_FUNDS);
    assert_eq!(s.env.balance(&source), LISTING_FEE - 1);
    assert_eq!((s.status(), s.vault(), s.listing_fees()), (ListingStatus::Quoted, 0, 0));
}

/// Only the configured fee mint (USDT) is accepted.
#[test]
fn pay_rejects_any_other_mint() {
    let mut s = pending();
    s.quote_default();
    let issuer = s.issuer.insecure_clone();
    let brl = s.env.listed_currency(SPL_TOKEN);
    let source = s.env.create_token_account(SPL_TOKEN, brl, issuer.pubkey(), LISTING_FEE);
    let mut a = s.env.pay_accounts(issuer.pubkey(), s.mint, source);
    a.fee_mint = brl;
    assert_custom_err(s.env.pay_listing_fee_with(&issuer, &a), ExchangeError::NotFeeMint);
    assert_eq!(s.status(), ListingStatus::Quoted);
}

/// A quote is in the units of the fee mint it was made in: after a
/// timelocked fee-mint switch it cannot be paid until it is re-quoted.
#[test]
fn fee_mint_switch_voids_an_open_quote() {
    let mut s = pending();
    let old_usdt = s.env.fee_mint;
    s.quote_default();

    let new_usdt = s.env.create_mint(TOKEN_2022, Some(Pubkey::new_unique()));
    s.env.set_mint_decimals(&new_usdt, PAYMENT_DECIMALS);
    let admin = s.env.admin.insecure_clone();
    let change = ConfigChange::FeeMint(new_usdt);
    s.env.propose(&admin, change).unwrap();
    s.env.warp_secs(TIMELOCK_SECS);
    s.env.execute(&admin, &change).unwrap();
    assert_eq!(s.env.fetch::<Config>(&config_pda()).fee_mint, new_usdt);
    s.env.fee_mint = new_usdt;
    s.env.fee_currency(TOKEN_2022);
    let issuer = s.issuer.insecure_clone();

    // In the new mint: not the quote's mint.
    let source = s.env.fee_mint_account(issuer.pubkey(), LISTING_FEE);
    let a = s.env.pay_accounts(issuer.pubkey(), s.mint, source);
    assert_custom_err(s.env.pay_listing_fee_with(&issuer, &a), ExchangeError::NotFeeMint);
    // In the old mint: no longer the fee mint.
    let old_source = s.env.create_token_account(SPL_TOKEN, old_usdt, issuer.pubkey(), LISTING_FEE);
    let old = PayAccounts {
        issuer: issuer.pubkey(),
        listing: listing_pda(&s.mint),
        fee_mint: old_usdt,
        fee_ledger: fee_ledger_pda(&old_usdt),
        fee_vault: fee_vault_pda(&old_usdt),
        source: old_source,
        token_program: SPL_TOKEN,
    };
    assert_custom_err(s.env.pay_listing_fee_with(&issuer, &old), ExchangeError::NotFeeMint);

    // Re-quoted in the new mint, it is paid there.
    s.quote_default();
    assert_eq!(s.listing().listing_fee_mint, new_usdt);
    s.env.pay_listing_fee_with(&issuer, &a).unwrap();
    assert_eq!(s.env.fee_ledger(&new_usdt).listing_fees_collected, LISTING_FEE);
    assert_eq!(s.env.fee_ledger(&old_usdt).listing_fees_collected, 0);
    s.env.assert_vault_matches_ledger(&new_usdt);
}

/// Pause principle (spec §3.3): quoting is a listing action and stays
/// available; paying is blocked.
#[test]
fn pause_blocks_payment_not_quotes() {
    let mut s = pending();
    let admin = s.env.admin.insecure_clone();
    s.env.pause(&admin).unwrap();
    s.quote_default();
    s.quote_default();
    assert_custom_err(s.pay(), ExchangeError::Paused);
    assert_eq!((s.status(), s.vault()), (ListingStatus::Quoted, 0));
    s.env.unpause(&admin).unwrap();
    s.pay().unwrap();
    assert_eq!(s.status(), ListingStatus::Approved);
}

// ---- approve_listing and rejection ---------------------------------------------------

/// The bypass: `approve_listing` cannot approve an issuer listing that has
/// not paid. It only reinstates, and reinstating charges nothing.
#[test]
fn approve_listing_cannot_bypass_the_fee() {
    for class in [AssetClass::Equity, AssetClass::FixedIncome, AssetClass::Credit] {
        let mut s = pending_with(SPL_TOKEN, class);
        let authority = s.authority.insecure_clone();
        assert_custom_err(s.env.approve(&authority, s.mint), ExchangeError::ListingFeeNotPaid);
        assert_eq!(s.status(), ListingStatus::Pending);
        s.quote_default();
        assert_custom_err(s.env.approve(&authority, s.mint), ExchangeError::ListingFeeNotPaid);
        assert_eq!(s.status(), ListingStatus::Quoted);
        assert_eq!(s.listing().approved_at, 0);

        s.pay().unwrap();
        s.env.suspend(&authority, s.mint).unwrap();
        s.env.warp_secs(50);
        let now = s.env.now();
        // Reinstatement: free, no token movement, no second fee.
        let meta = s.env.approve(&authority, s.mint).unwrap();
        assert_eq!(s.status(), ListingStatus::Approved);
        assert_eq!(s.listing().approved_at, now);
        assert_eq!(events::<ListingApproved>(&meta)[0].approved_at, now);
        assert!(events::<ListingFeePaid>(&meta).is_empty());
        assert_eq!((s.vault(), s.listing_fees()), (LISTING_FEE, LISTING_FEE));
        assert_eq!(s.listing().listing_fee_paid, LISTING_FEE);
    }
}

/// `delist` rejects a quoted listing; it can then be neither paid nor quoted.
#[test]
fn quoted_listing_can_be_rejected_by_delisting() {
    let mut s = pending();
    s.quote_default();
    let authority = s.authority.insecure_clone();
    s.env.delist(&authority, s.mint).unwrap();
    assert_eq!(s.status(), ListingStatus::Delisted);
    assert_custom_err(s.pay(), ExchangeError::ListingNotQuoted);
    let expires_at = s.env.now() + QUOTE_SECS;
    assert_custom_err(s.quote(VALUATION, 1, expires_at), ExchangeError::InvalidStatusTransition);
    assert_eq!((s.vault(), s.listing().listing_fee_paid), (0, 0));
}

/// A quoted listing is not tradable: only payment approves it.
#[test]
fn quoted_listing_does_not_trade() {
    let mut s = pending();
    s.quote_default();
    let usdt = s.env.fee_mint;
    let payer = s.env.admin.insecure_clone();
    s.env.init_cashback_ledger(&payer, s.mint, usdt).unwrap();
    let issuer = s.issuer.insecure_clone();
    let m = s.env.trader_pair(s.mint, usdt, issuer, 10, 1_000_000);
    let res = s.env.settle(&m, match_id(1), 1, 1_000_000);
    assert_custom_err(res, ExchangeError::ListingNotApproved);
    s.pay().unwrap();
    s.env.settle(&m, match_id(1), 1, 1_000_000).unwrap();
}

// ---- accounting -------------------------------------------------------------------

/// The listing fee is protocol revenue: the treasury withdraws it, it never
/// accrues to the issuer's cashback, and it is owed (never swept).
#[test]
fn listing_fee_is_protocol_revenue_without_cashback() {
    let mut env = Env::initialized();
    let m = env.market(SPL_TOKEN, SPL_TOKEN); // listing fee paid, no trade yet
    let usdt = m.payment_mint;
    assert_eq!(env.cashback(&m).accrued, 0);
    let ledger = env.fee_ledger(&usdt);
    assert_eq!((ledger.listing_fees_collected, ledger.cashback_accrued), (LISTING_FEE, 0));

    let treasury = env.treasury.insecure_clone();
    let to = env.create_token_account(SPL_TOKEN, usdt, treasury.pubkey(), 0);
    assert_custom_err(env.sweep_excess(&treasury, usdt, to), ExchangeError::NothingToSweep);
    let res = env.withdraw_protocol_fees(&treasury, usdt, to, LISTING_FEE + 1);
    assert_custom_err(res, ExchangeError::InsufficientProtocolFees);
    env.withdraw_protocol_fees(&treasury, usdt, to, LISTING_FEE).unwrap();
    assert_eq!(env.balance(&to), LISTING_FEE);
    assert_eq!(env.balance(&fee_vault_pda(&usdt)), 0);
    env.assert_vault_matches_ledger(&usdt);

    // A later trade's cashback is a share of the trading fee only.
    env.settle(&m, match_id(1), 1, 1_000_000_000).unwrap();
    let fee = 3_000_000; // 30 bps
    assert_eq!(env.cashback(&m).accrued, fee * u64::from(CASHBACK_BPS) / 10_000);
    let ledger = env.fee_ledger(&usdt);
    assert_eq!((ledger.fees_collected, ledger.listing_fees_collected), (fee, LISTING_FEE));
    env.assert_vault_matches_ledger(&usdt);
}

/// Each listing pays once; the ledger sums them (invariant 13).
#[test]
fn ledger_sums_every_listing_fee() {
    let mut env = Env::initialized();
    env.fee_currency(SPL_TOKEN);
    let mut paid = 0;
    for (n, bps) in [(1u64, 1u16), (2, 50), (3, MAX_LISTING_FEE_BPS)] {
        let issuer = env.funded_keypair();
        let mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
        env.submit_listing(&issuer, mint, AssetClass::Credit, DOC_HASH).unwrap();
        let authority = env.listing_authority.insecure_clone();
        let expires_at = env.now() + QUOTE_SECS;
        env.quote_listing(&authority, mint, n * 1_000_000_007, bps, expires_at).unwrap();
        env.pay_listing_fee(&issuer, mint).unwrap();
        let l = env.listing(&mint);
        let expected = (u128::from(l.valuation) * u128::from(bps)).div_ceil(10_000) as u64;
        assert_eq!((l.listing_fee, l.listing_fee_paid), (expected, expected));
        paid += expected;
    }
    let usdt = env.fee_mint;
    assert_eq!(env.fee_ledger(&usdt).listing_fees_collected, paid);
    env.assert_vault_matches_ledger(&usdt);
}
