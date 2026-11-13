//! Listing-fee fixtures (spec §3.13): `quote_listing`, `pay_listing_fee`, and
//! `approve_paid`, the one call every trading fixture uses to take an issuer
//! listing from `Pending` to `Approved`.

use super::*;

/// Fixture quote: a 1_000 USDT valuation (6 decimals) at the 1% cap.
pub const VALUATION: u64 = 1_000_000_000;
pub const LISTING_FEE_BPS: u16 = 100;
/// `ceil(VALUATION * LISTING_FEE_BPS / 10_000)`.
pub const LISTING_FEE: u64 = 10_000_000;
/// How long the fixture quote stays payable.
pub const QUOTE_SECS: i64 = 7 * 24 * 60 * 60;

/// Accounts of one `pay_listing_fee` call; tests tweak fields to build
/// failure cases.
#[derive(Clone)]
pub struct PayAccounts {
    pub issuer: Pubkey,
    pub listing: Pubkey,
    pub fee_mint: Pubkey,
    pub fee_ledger: Pubkey,
    pub fee_vault: Pubkey,
    pub source: Pubkey,
    pub token_program: Pubkey,
}

impl Env {
    pub fn quote_listing(
        &mut self,
        authority: &Keypair,
        mint: Pubkey,
        valuation: u64,
        listing_fee_bps: u16,
        expires_at: i64,
    ) -> TransactionResult {
        let ix = ix(
            self.status_accounts(authority, &mint),
            instruction::QuoteListing {
                valuation,
                listing_fee_bps,
                expires_at,
            },
        );
        self.send(&[ix], &[authority])
    }

    /// The fixture quote (`VALUATION` at `LISTING_FEE_BPS`), signed by the
    /// listing authority, payable for `QUOTE_SECS`.
    pub fn quote_default(&mut self, mint: Pubkey) -> TransactionResult {
        let authority = self.listing_authority.insecure_clone();
        let expires_at = self.now() + QUOTE_SECS;
        self.quote_listing(&authority, mint, VALUATION, LISTING_FEE_BPS, expires_at)
    }

    /// `pay_listing_fee` accounts for the configured fee mint.
    pub fn pay_accounts(&self, issuer: Pubkey, mint: Pubkey, source: Pubkey) -> PayAccounts {
        let fee_mint = self.fee_mint;
        PayAccounts {
            issuer,
            listing: listing_pda(&mint),
            fee_mint,
            fee_ledger: fee_ledger_pda(&fee_mint),
            fee_vault: fee_vault_pda(&fee_mint),
            source,
            token_program: self.owner_of(&fee_mint),
        }
    }

    pub fn pay_listing_fee_with(&mut self, signer: &Keypair, a: &PayAccounts) -> TransactionResult {
        let ix = ix(
            accounts::PayListingFee {
                issuer: a.issuer,
                config: config_pda(),
                listing: a.listing,
                fee_mint: a.fee_mint,
                fee_ledger: a.fee_ledger,
                fee_vault: a.fee_vault,
                source: a.source,
                token_program: a.token_program,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::PayListingFee {},
        );
        self.send(&[ix], &[signer])
    }

    /// A fresh account of the configured fee mint owned by `owner`, holding
    /// `amount` (written directly, like every trading fixture balance).
    pub fn fee_mint_account(&mut self, owner: Pubkey, amount: u64) -> Pubkey {
        let fee_mint = self.fee_mint;
        let program = self.owner_of(&fee_mint);
        self.create_token_account(program, fee_mint, owner, amount)
    }

    /// The issuer pays the quoted fee from a fresh account holding exactly it.
    pub fn pay_listing_fee(&mut self, issuer: &Keypair, mint: Pubkey) -> TransactionResult {
        let fee = self.listing(&mint).listing_fee;
        let source = self.fee_mint_account(issuer.pubkey(), fee);
        let a = self.pay_accounts(issuer.pubkey(), mint, source);
        self.pay_listing_fee_with(issuer, &a)
    }

    /// The fee currency, ready to collect fees: the existing fee mint under
    /// its token program, or a new SPL Token one.
    pub fn ensure_fee_currency(&mut self) -> Pubkey {
        let program = if self.exists(&self.fee_mint) {
            self.owner_of(&self.fee_mint)
        } else {
            SPL_TOKEN
        };
        self.fee_currency(program)
    }

    /// `Pending` -> `Approved` the only way v1 allows: the fixture quote, then
    /// the issuer pays it. Creates the fee currency (SPL Token) first if no
    /// test did; tests that want it under Token-2022 create it before.
    pub fn approve_paid(&mut self, issuer: &Keypair, mint: Pubkey) {
        self.ensure_fee_currency();
        self.quote_default(mint).unwrap();
        self.pay_listing_fee(issuer, mint).unwrap();
    }
}
