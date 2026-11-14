//! Niara-approved issuance (spec §3.12): `submit_listing` hands the mint
//! authority to a program PDA; supply only grows through issuer-requested,
//! listing-authority-approved batches, and shrinks through `burn_with_reason`.
//! Invariant 11: `supply == Σ executed batches − Σ burns`.

mod common;

use anchor_lang::{error::ErrorCode as AnchorError, prelude::Pubkey};
use anchor_spl::token_2022::spl_token_2022::{self, instruction::AuthorityType};
use common::*;
use niara_exchange::{
    error::ExchangeError,
    events::{IssuanceExecuted, IssuanceRejected, IssuanceRequested, TokensBurned},
    AssetClass, BatchStatus, BurnReason, ListingStatus,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

/// `TokenError::OwnerMismatch`: the signer is not the mint authority.
const TOKEN_ERROR_OWNER_MISMATCH: u32 = 4;

struct Issuer {
    env: Env,
    issuer: Keypair,
    mint: Pubkey,
    authority: Keypair,
    holder: Keypair,
    /// Asset account owned by `holder`.
    holder_account: Pubkey,
}

/// A submitted (still `Pending`) listing of `class` on `token_program`.
fn submitted(token_program: Pubkey, class: AssetClass) -> Issuer {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let mint = env.create_mint(token_program, Some(issuer.pubkey()));
    env.submit_listing(&issuer, mint, class, DOC_HASH).unwrap();
    let authority = env.listing_authority.insecure_clone();
    let holder = env.funded_keypair();
    let holder_account = env.create_token_account(token_program, mint, holder.pubkey(), 0);
    Issuer {
        env,
        issuer,
        mint,
        authority,
        holder,
        holder_account,
    }
}

fn approved_as(token_program: Pubkey, class: AssetClass) -> Issuer {
    let mut s = submitted(token_program, class);
    let issuer = s.issuer.insecure_clone();
    s.env.approve_paid(&issuer, s.mint);
    s
}

fn approved(token_program: Pubkey) -> Issuer {
    approved_as(token_program, AssetClass::Equity)
}

impl Issuer {
    fn request(&mut self, amount: u64) -> litesvm::types::TransactionResult {
        let issuer = self.issuer.insecure_clone();
        let holder = self.holder.pubkey();
        self.env
            .request_issuance(&issuer, self.mint, amount, holder, ISSUANCE_DOC)
    }
    fn approve_batch(&mut self, batch_no: u64) -> litesvm::types::TransactionResult {
        let authority = self.authority.insecure_clone();
        let dest = self.holder_account;
        self.env.approve_issuance(&authority, self.mint, batch_no, dest)
    }
    fn reject_batch(&mut self, batch_no: u64) -> litesvm::types::TransactionResult {
        let authority = self.authority.insecure_clone();
        self.env.reject_issuance(&authority, self.mint, batch_no)
    }
    fn burn(&mut self, amount: u64, reason: BurnReason) -> litesvm::types::TransactionResult {
        let holder = self.holder.insecure_clone();
        let account = self.holder_account;
        self.env
            .burn_with_reason(&holder, self.mint, account, amount, reason, BURN_REF)
    }
    fn supply(&self) -> u64 {
        self.env.mint_supply(&self.mint)
    }
    /// A raw `MintTo` signed by `signer`, as if it were the mint authority.
    fn direct_mint_to(&mut self, signer: &Keypair) -> litesvm::types::TransactionResult {
        let program = self.env.owner_of(&self.mint);
        let ix = spl_token_2022::instruction::mint_to(
            &program,
            &self.mint,
            &self.holder_account,
            &signer.pubkey(),
            &[],
            1_000,
        )
        .unwrap();
        self.env.send(&[ix], &[signer])
    }
}

fn assert_token_err(res: litesvm::types::TransactionResult, code: u32) {
    let err = res.expect_err("token instruction should have failed");
    assert!(
        format!("{:?}", err.err).contains(&format!("Custom({code})")),
        "expected token error {code}, got {:?}",
        err.err
    );
}

// ---- submit_listing hands the mint authority to the program ------------------

#[test]
fn submit_listing_moves_mint_authority_to_the_program() {
    for token_program in [SPL_TOKEN, TOKEN_2022] {
        let s = submitted(token_program, AssetClass::Equity);
        assert_eq!(s.env.mint_authority(&s.mint), Some(mint_authority_pda(&s.mint)));
        let listing = s.env.listing(&s.mint);
        assert_eq!(listing.issuer, s.issuer.pubkey());
        assert_eq!((listing.next_batch_no, listing.issued, listing.burned), (1, 0, 0));
        assert_eq!(s.supply(), 0);
    }
}

#[test]
fn submit_listing_requires_zero_supply() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let mint = env.create_mint(SPL_TOKEN, Some(issuer.pubkey()));
    env.set_mint_supply(&mint, 1);
    let res = env.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH);
    assert_custom_err(res, ExchangeError::NonZeroSupply);
    assert!(!env.exists(&listing_pda(&mint)));
    assert_eq!(env.mint_authority(&mint), Some(issuer.pubkey()));
}

#[test]
fn issuer_alone_cannot_mint() {
    for token_program in [SPL_TOKEN, TOKEN_2022] {
        let mut s = approved(token_program);
        let issuer = s.issuer.insecure_clone();
        // Directly through the token program: no longer the mint authority.
        assert_token_err(s.direct_mint_to(&issuer), TOKEN_ERROR_OWNER_MISMATCH);
        // Nor can it take the authority back.
        let ix = spl_token_2022::instruction::set_authority(
            &token_program,
            &s.mint,
            Some(&issuer.pubkey()),
            AuthorityType::MintTokens,
            &issuer.pubkey(),
            &[],
        )
        .unwrap();
        assert_token_err(s.env.send(&[ix], &[&issuer]), TOKEN_ERROR_OWNER_MISMATCH);
        // Through the program, a request mints nothing; approving it is not
        // the issuer's call.
        s.request(1_000).unwrap();
        let dest = s.holder_account;
        let res = s.env.approve_issuance(&issuer, s.mint, 1, dest);
        assert_custom_err(res, ExchangeError::Unauthorized);
        assert_eq!(s.supply(), 0);
    }
}

#[test]
fn niara_alone_cannot_mint() {
    let mut s = approved(SPL_TOKEN);
    let authority = s.authority.insecure_clone();
    let admin = s.env.admin.insecure_clone();
    for niara in [&authority, &admin] {
        assert_token_err(s.direct_mint_to(niara), TOKEN_ERROR_OWNER_MISMATCH);
        let holder = s.holder.pubkey();
        let res = s
            .env
            .request_issuance(niara, s.mint, 1_000, holder, ISSUANCE_DOC);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    // No batch was requested, so there is nothing to approve.
    let res = s.approve_batch(1);
    assert_custom_err(res, AnchorError::AccountNotInitialized);
    assert_eq!(s.supply(), 0);
}

// ---- request_issuance ---------------------------------------------------------

#[test]
fn request_issuance_creates_sequential_batches() {
    let mut s = approved_as(SPL_TOKEN, AssetClass::FixedIncome);
    let now = s.env.now();
    let meta = s.request(500).unwrap();
    let e = &events::<IssuanceRequested>(&meta)[0];
    assert_eq!(
        (e.listing, e.asset_mint, e.batch_no, e.amount, e.recipient, e.doc_hash),
        (listing_pda(&s.mint), s.mint, 1, 500, s.holder.pubkey(), ISSUANCE_DOC)
    );
    s.request(700).unwrap();
    s.request(900).unwrap();
    assert_eq!(s.env.listing(&s.mint).next_batch_no, 4);

    for (batch_no, amount) in [(1u64, 500u64), (2, 700), (3, 900)] {
        let b = s.env.batch(&s.mint, batch_no);
        assert_eq!(b.listing, listing_pda(&s.mint));
        assert_eq!(b.batch_no, batch_no);
        assert_eq!(b.amount, amount);
        assert_eq!(b.recipient, s.holder.pubkey());
        assert_eq!(b.doc_hash, ISSUANCE_DOC);
        assert_eq!(b.status, BatchStatus::Requested);
        assert_eq!((b.requested_at, b.resolved_at), (now, 0));
    }
    // Requests mint nothing.
    assert_eq!(s.supply(), 0);
}

/// The batch PDA is derived from the listing's counter: a request for any
/// other number fails, so numbers cannot be skipped or reused.
#[test]
fn batch_numbers_cannot_be_skipped_or_reused() {
    let mut s = approved(SPL_TOKEN);
    let issuer = s.issuer.insecure_clone();
    let holder = s.holder.pubkey();
    for batch_no in [0u64, 2, 7] {
        let res = s
            .env
            .request_issuance_at(&issuer, s.mint, batch_no, 1, holder, ISSUANCE_DOC);
        assert_custom_err(res, AnchorError::ConstraintSeeds);
    }
    s.request(1).unwrap();
    let res = s
        .env
        .request_issuance_at(&issuer, s.mint, 1, 1, holder, ISSUANCE_DOC);
    assert!(res.is_err(), "batch 1 requested twice");
    assert_eq!(s.env.listing(&s.mint).next_batch_no, 2);
}

#[test]
fn only_the_listing_issuer_requests_issuance() {
    let mut s = submitted(SPL_TOKEN, AssetClass::Credit);
    // The issuer of another listing is a stranger to this one.
    let other_issuer = s.env.funded_keypair();
    let other_mint = s.env.create_mint(SPL_TOKEN, Some(other_issuer.pubkey()));
    s.env
        .submit_listing(&other_issuer, other_mint, AssetClass::Equity, DOC_HASH)
        .unwrap();
    let intruders = [
        s.env.funded_keypair(),
        s.authority.insecure_clone(),
        s.env.admin.insecure_clone(),
        s.holder.insecure_clone(),
        other_issuer,
    ];
    for intruder in intruders {
        let holder = s.holder.pubkey();
        let res = s
            .env
            .request_issuance(&intruder, s.mint, 1, holder, ISSUANCE_DOC);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_eq!(s.env.listing(&s.mint).next_batch_no, 1);
}

/// Spec §3.12: no batch is requested unless the listing is `Approved`, i.e.
/// the listing fee is paid. `Pending`, `Quoted`, `Suspended` and `Delisted`
/// refuse, and a refusal creates no batch.
#[test]
fn request_issuance_only_while_approved() {
    let mut s = submitted(SPL_TOKEN, AssetClass::Equity);
    let authority = s.authority.insecure_clone();
    let issuer = s.issuer.insecure_clone();
    assert_custom_err(s.request(1), ExchangeError::ListingNotIssuable); // Pending
    s.env.ensure_fee_currency();
    s.env.quote_default(s.mint).unwrap();
    assert_custom_err(s.request(1), ExchangeError::ListingNotIssuable); // Quoted
    // An expired quote is still `Quoted`: not issuable either.
    s.env.warp_secs(40 * 24 * 3600);
    assert_eq!(s.env.listing(&s.mint).status, ListingStatus::Quoted);
    assert_custom_err(s.request(1), ExchangeError::ListingNotIssuable);
    assert!(!s.env.exists(&batch_pda(&s.mint, 1)));
    assert_eq!(s.env.listing(&s.mint).next_batch_no, 1);
    s.env.quote_default(s.mint).unwrap();
    s.env.pay_listing_fee(&issuer, s.mint).unwrap();
    s.request(1).unwrap(); // Approved
    s.env.suspend(&authority, s.mint).unwrap();
    assert_custom_err(s.request(1), ExchangeError::ListingNotIssuable);
    s.env.approve(&authority, s.mint).unwrap();
    s.request(1).unwrap(); // reinstated
    s.env.delist(&authority, s.mint).unwrap();
    assert_custom_err(s.request(1), ExchangeError::ListingNotIssuable);
    // Only the two requests made while Approved exist.
    assert_eq!(s.env.listing(&s.mint).next_batch_no, 3);
    assert!(!s.env.exists(&batch_pda(&s.mint, 3)));
}

#[test]
fn request_issuance_validates_its_arguments() {
    let mut s = approved(SPL_TOKEN);
    let issuer = s.issuer.insecure_clone();
    let holder = s.holder.pubkey();
    let res = s.env.request_issuance(&issuer, s.mint, 0, holder, ISSUANCE_DOC);
    assert_custom_err(res, ExchangeError::ZeroAmount);
    let res = s.env.request_issuance(&issuer, s.mint, 1, holder, [0u8; 32]);
    assert_custom_err(res, ExchangeError::EmptyDocHash);
    let res = s
        .env
        .request_issuance(&issuer, s.mint, 1, Pubkey::default(), ISSUANCE_DOC);
    assert_custom_err(res, ExchangeError::InvalidAddress);
}

/// Currency listings have no issuer (`Pubkey::default()` cannot sign) and the
/// program does not hold their mint authority.
#[test]
fn currency_listings_have_no_issuance() {
    let mut env = Env::initialized();
    let currency = env.listed_currency(SPL_TOKEN);
    let anyone = env.funded_keypair();
    let res = env.request_issuance(&anyone, currency, 1, anyone.pubkey(), ISSUANCE_DOC);
    assert!(res.is_err());
    let account = env.create_token_account(SPL_TOKEN, currency, anyone.pubkey(), 10);
    env.set_mint_supply(&currency, 10);
    let res = env.burn_with_reason(&anyone, currency, account, 1, BurnReason::Redemption, BURN_REF);
    assert_custom_err(res, ExchangeError::CurrencyNotIssued);
}

// ---- approve_issuance / reject_issuance -----------------------------------------

#[test]
fn approved_batch_mints_to_the_recipient() {
    for token_program in [SPL_TOKEN, TOKEN_2022] {
        let mut s = approved(token_program);
        s.request(1_500).unwrap();
        s.env.warp_secs(60);
        let now = s.env.now();
        let meta = s.approve_batch(1).unwrap();

        let e = &events::<IssuanceExecuted>(&meta)[0];
        assert_eq!(
            (e.asset_mint, e.batch_no, e.amount, e.recipient, e.doc_hash, e.destination),
            (s.mint, 1, 1_500, s.holder.pubkey(), ISSUANCE_DOC, s.holder_account)
        );
        assert_eq!(s.env.balance(&s.holder_account), 1_500);
        assert_eq!(s.supply(), 1_500);
        let b = s.env.batch(&s.mint, 1);
        assert_eq!((b.status, b.resolved_at), (BatchStatus::Executed, now));
        assert_eq!(s.env.listing(&s.mint).issued, 1_500);
        s.env.assert_supply_matches_issuance(&s.mint);
    }
}

#[test]
fn a_batch_executes_at_most_once() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    s.approve_batch(1).unwrap();
    assert_custom_err(s.approve_batch(1), ExchangeError::BatchNotRequested);
    assert_custom_err(s.reject_batch(1), ExchangeError::BatchNotRequested);
    assert_eq!(s.supply(), 100);
}

/// Nothing executes before the listing fee is paid. A batch cannot even be
/// requested then (`request_issuance_only_while_approved`), so `approve_issuance`
/// is checked on a listing whose status is forced back with a batch open.
#[test]
fn approve_issuance_requires_an_approved_listing() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    for status in [ListingStatus::Pending, ListingStatus::Quoted] {
        s.env.force_listing_status(&s.mint, status);
        assert_custom_err(s.approve_batch(1), ExchangeError::ListingNotApproved);
        assert_eq!(s.supply(), 0);
        assert_eq!(s.env.batch(&s.mint, 1).status, BatchStatus::Requested);
    }
    // Without the forcing, no batch exists to approve before payment.
    let mut p = submitted(SPL_TOKEN, AssetClass::Equity);
    assert_custom_err(p.request(100), ExchangeError::ListingNotIssuable);
    assert_custom_err(p.approve_batch(1), AnchorError::AccountNotInitialized);
    p.env.ensure_fee_currency();
    p.env.quote_default(p.mint).unwrap();
    assert_custom_err(p.request(100), ExchangeError::ListingNotIssuable);
    assert_custom_err(p.approve_batch(1), AnchorError::AccountNotInitialized);
    assert_eq!(p.supply(), 0);
    // Once paid, the same flow works and charges no second fee.
    let issuer = p.issuer.insecure_clone();
    p.env.pay_listing_fee(&issuer, p.mint).unwrap();
    p.request(100).unwrap();
    p.approve_batch(1).unwrap();
    assert_eq!(p.supply(), 100);
    let ledger = p.env.fee_ledger(&p.env.fee_mint);
    assert_eq!(ledger.listing_fees_collected, LISTING_FEE);
}

#[test]
fn approved_batch_on_a_suspended_listing_reverts() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    let authority = s.authority.insecure_clone();
    s.env.suspend(&authority, s.mint).unwrap();
    assert_custom_err(s.approve_batch(1), ExchangeError::ListingNotApproved);
    assert_eq!(s.supply(), 0);
    assert_eq!(s.env.batch(&s.mint, 1).status, BatchStatus::Requested);
    // Reinstated: the same batch goes through.
    s.env.approve(&authority, s.mint).unwrap();
    s.approve_batch(1).unwrap();
    assert_eq!(s.supply(), 100);
}

#[test]
fn delisted_listing_never_issues_again() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    let authority = s.authority.insecure_clone();
    s.env.delist(&authority, s.mint).unwrap();
    assert_custom_err(s.approve_batch(1), ExchangeError::ListingNotApproved);
    // The pending request can still be closed out.
    s.reject_batch(1).unwrap();
    assert_eq!(s.supply(), 0);
}

#[test]
fn only_the_listing_authority_resolves_batches() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    let intruders = [
        s.env.funded_keypair(),
        s.env.admin.insecure_clone(),
        s.env.operator.insecure_clone(),
        s.env.treasury.insecure_clone(),
        s.issuer.insecure_clone(),
        s.holder.insecure_clone(),
    ];
    for intruder in intruders {
        let dest = s.holder_account;
        let res = s.env.approve_issuance(&intruder, s.mint, 1, dest);
        assert_custom_err(res, ExchangeError::Unauthorized);
        let res = s.env.reject_issuance(&intruder, s.mint, 1);
        assert_custom_err(res, ExchangeError::Unauthorized);
    }
    assert_eq!(s.env.batch(&s.mint, 1).status, BatchStatus::Requested);
}

/// After a timelocked listing-authority rotation only the new key approves.
#[test]
fn rotated_listing_authority_takes_over_issuance() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    let admin = s.env.admin.insecure_clone();
    let new = s.env.funded_keypair();
    let change = niara_exchange::ConfigChange::ListingAuthority(new.pubkey());
    s.env.propose(&admin, change).unwrap();
    s.env.warp_secs(TIMELOCK_SECS);
    s.env.execute(&admin, &change).unwrap();
    assert_custom_err(s.approve_batch(1), ExchangeError::Unauthorized);
    let dest = s.holder_account;
    s.env.approve_issuance(&new, s.mint, 1, dest).unwrap();
}

#[test]
fn destination_must_belong_to_the_recipient() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    let authority = s.authority.insecure_clone();
    let stranger = s.env.create_token_account(SPL_TOKEN, s.mint, Pubkey::new_unique(), 0);
    let res = s.env.approve_issuance(&authority, s.mint, 1, stranger);
    assert_custom_err(res, ExchangeError::WrongRecipient);
    // Right owner, wrong mint.
    let other_mint = s.env.create_mint(SPL_TOKEN, None);
    let wrong_mint =
        s.env
            .create_token_account(SPL_TOKEN, other_mint, s.holder.pubkey(), 0);
    let res = s.env.approve_issuance(&authority, s.mint, 1, wrong_mint);
    assert_custom_err(res, AnchorError::ConstraintTokenMint);
    // Any account of the recipient works.
    let second = s.env.create_token_account(SPL_TOKEN, s.mint, s.holder.pubkey(), 0);
    s.env.approve_issuance(&authority, s.mint, 1, second).unwrap();
    assert_eq!(s.env.balance(&second), 100);
}

/// A batch of one listing cannot be executed against another listing's mint.
#[test]
fn batch_is_bound_to_its_listing() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    // A second approved listing in the same environment.
    let other_issuer = s.env.funded_keypair();
    let other = s.env.create_mint(SPL_TOKEN, Some(other_issuer.pubkey()));
    s.env
        .submit_listing(&other_issuer, other, AssetClass::Equity, DOC_HASH)
        .unwrap();
    s.env.approve_paid(&other_issuer, other);
    let authority = s.authority.insecure_clone();
    let dest = s.env.create_token_account(SPL_TOKEN, other, s.holder.pubkey(), 0);
    // Batch 1 of the first listing, presented with the second mint.
    let ix = anchor_lang::solana_program::instruction::Instruction::new_with_bytes(
        program_id(),
        &anchor_lang::InstructionData::data(&niara_exchange::instruction::ApproveIssuance {}),
        anchor_lang::ToAccountMetas::to_account_metas(
            &niara_exchange::accounts::ApproveIssuance {
                listing_authority: authority.pubkey(),
                config: config_pda(),
                listing: listing_pda(&other),
                batch: batch_pda(&s.mint, 1),
                asset_mint: other,
                mint_authority: mint_authority_pda(&other),
                destination: dest,
                token_program: SPL_TOKEN,
                event_authority: event_authority(),
                program: program_id(),
            },
            None,
        ),
    );
    assert!(s.env.send(&[ix], &[&authority]).is_err());
    assert_eq!(s.env.mint_supply(&other), 0);
    assert_eq!(s.env.batch(&s.mint, 1).status, BatchStatus::Requested);
}

#[test]
fn reject_issuance_closes_the_request() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    s.env.warp_secs(30);
    let now = s.env.now();
    let meta = s.reject_batch(1).unwrap();
    let e = &events::<IssuanceRejected>(&meta)[0];
    assert_eq!(
        (e.asset_mint, e.batch_no, e.amount, e.recipient),
        (s.mint, 1, 100, s.holder.pubkey())
    );
    let b = s.env.batch(&s.mint, 1);
    assert_eq!((b.status, b.resolved_at), (BatchStatus::Rejected, now));
    assert_custom_err(s.approve_batch(1), ExchangeError::BatchNotRequested);
    assert_custom_err(s.reject_batch(1), ExchangeError::BatchNotRequested);
    assert_eq!(s.supply(), 0);
    // Numbering continues after a rejection.
    s.request(5).unwrap();
    s.approve_batch(2).unwrap();
    assert_eq!(s.supply(), 5);
}

/// Pause stops minting (approve) but not requests, rejections or burns.
#[test]
fn pause_blocks_execution_only() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    s.approve_batch(1).unwrap();
    let admin = s.env.admin.insecure_clone();
    s.env.pause(&admin).unwrap();
    s.request(50).unwrap();
    assert_custom_err(s.approve_batch(2), ExchangeError::Paused);
    s.request(60).unwrap();
    s.reject_batch(3).unwrap();
    s.burn(10, BurnReason::Redemption).unwrap();
    s.env.unpause(&admin).unwrap();
    s.approve_batch(2).unwrap();
    assert_eq!(s.supply(), 140);
    s.env.assert_supply_matches_issuance(&s.mint);
}

// ---- burn_with_reason ---------------------------------------------------------

#[test]
fn holder_burns_with_a_reason() {
    for token_program in [SPL_TOKEN, TOKEN_2022] {
        let mut s = approved(token_program);
        s.request(1_000).unwrap();
        s.approve_batch(1).unwrap();

        let meta = s.burn(300, BurnReason::Redemption).unwrap();
        let e = &events::<TokensBurned>(&meta)[0];
        assert_eq!(
            (e.asset_mint, e.holder, e.holder_account, e.amount, e.reason, e.reference),
            (s.mint, s.holder.pubkey(), s.holder_account, 300, BurnReason::Redemption, BURN_REF)
        );
        let meta = s.burn(200, BurnReason::Cancellation).unwrap();
        assert_eq!(events::<TokensBurned>(&meta)[0].reason, BurnReason::Cancellation);

        assert_eq!(s.env.balance(&s.holder_account), 500);
        assert_eq!(s.supply(), 500);
        let listing = s.env.listing(&s.mint);
        assert_eq!((listing.issued, listing.burned), (1_000, 500));
        s.env.assert_supply_matches_issuance(&s.mint);
    }
}

#[test]
fn burn_validates_amount_and_reference() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    s.approve_batch(1).unwrap();
    assert_custom_err(s.burn(0, BurnReason::Redemption), ExchangeError::ZeroAmount);
    let holder = s.holder.insecure_clone();
    let account = s.holder_account;
    let res = s
        .env
        .burn_with_reason(&holder, s.mint, account, 1, BurnReason::Redemption, [0u8; 32]);
    assert_custom_err(res, ExchangeError::EmptyReference);
    // More than the holder has: the token program refuses, nothing recorded.
    assert!(s.burn(101, BurnReason::Redemption).is_err());
    assert_eq!(s.env.listing(&s.mint).burned, 0);
    assert_eq!(s.supply(), 100);
}

#[test]
fn only_the_holder_burns_its_tokens() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    s.approve_batch(1).unwrap();
    let account = s.holder_account;
    for intruder in [
        s.issuer.insecure_clone(),
        s.authority.insecure_clone(),
        s.env.admin.insecure_clone(),
        s.env.funded_keypair(),
    ] {
        let res =
            s.env
                .burn_with_reason(&intruder, s.mint, account, 1, BurnReason::Cancellation, BURN_REF);
        assert_custom_err(res, AnchorError::ConstraintTokenOwner);
    }
    assert_eq!(s.supply(), 100);
}

/// Listing status gates trading and issuance, not redemption.
#[test]
fn burns_survive_delisting() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    s.approve_batch(1).unwrap();
    let authority = s.authority.insecure_clone();
    s.env.delist(&authority, s.mint).unwrap();
    assert_eq!(s.env.listing(&s.mint).status, ListingStatus::Delisted);
    s.burn(100, BurnReason::Redemption).unwrap();
    assert_eq!(s.supply(), 0);
}

// ---- invariant 11 ---------------------------------------------------------------

/// Issuance, rejections and burns across two holders keep
/// `supply == Σ executed − Σ burned`, equal to the sum of the events.
#[test]
fn supply_tracks_executed_batches_minus_burns() {
    let mut s = approved(TOKEN_2022);
    let second = s.env.funded_keypair();
    let second_account = s.env.create_token_account(TOKEN_2022, s.mint, second.pubkey(), 0);
    let issuer = s.issuer.insecure_clone();
    let authority = s.authority.insecure_clone();
    let (mut executed, mut burned) = (0u64, 0u64);

    for (i, amount) in [400u64, 250, 999, 1, 70].into_iter().enumerate() {
        let batch_no = i as u64 + 1;
        let (recipient, dest) = if i % 2 == 0 {
            (s.holder.pubkey(), s.holder_account)
        } else {
            (second.pubkey(), second_account)
        };
        s.env
            .request_issuance(&issuer, s.mint, amount, recipient, ISSUANCE_DOC)
            .unwrap();
        if i == 3 {
            s.env.reject_issuance(&authority, s.mint, batch_no).unwrap();
        } else {
            let meta = s.env.approve_issuance(&authority, s.mint, batch_no, dest).unwrap();
            executed += events::<IssuanceExecuted>(&meta)[0].amount;
        }
        s.env.assert_supply_matches_issuance(&s.mint);
    }
    let meta = s.burn(123, BurnReason::Redemption).unwrap();
    burned += events::<TokensBurned>(&meta)[0].amount;
    let meta = s
        .env
        .burn_with_reason(&second, s.mint, second_account, 50, BurnReason::Cancellation, BURN_REF)
        .unwrap();
    burned += events::<TokensBurned>(&meta)[0].amount;

    assert_eq!(s.supply(), executed - burned);
    let listing = s.env.listing(&s.mint);
    assert_eq!((listing.issued, listing.burned), (executed, burned));
    s.env.assert_supply_matches_issuance(&s.mint);
}

/// A holder can still burn directly through the token program; the program
/// cannot block or see it. On-chain invariant 11 is therefore
/// `supply <= issued − burned`, and the gap is an unexplained burn the
/// Register flags (a supply drop with no `TokensBurned`).
#[test]
fn direct_burns_are_unrecorded() {
    let mut s = approved(SPL_TOKEN);
    s.request(100).unwrap();
    s.approve_batch(1).unwrap();
    let holder = s.holder.insecure_clone();
    let ix = spl_token_2022::instruction::burn(
        &SPL_TOKEN,
        &s.holder_account,
        &s.mint,
        &holder.pubkey(),
        &[],
        30,
    )
    .unwrap();
    s.env.send(&[ix], &[&holder]).unwrap();
    let listing = s.env.listing(&s.mint);
    assert_eq!((listing.issued, listing.burned), (100, 0));
    assert_eq!(s.supply(), 70);
    assert!(s.supply() < listing.issued - listing.burned);
}
