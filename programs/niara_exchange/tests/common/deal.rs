//! P2P escrow fixtures: bilateral deals between a proposer and a named
//! counterparty.

use super::*;
use niara_exchange::{Deal, DealSide, DealTerms, DEAL_SEED, DEAL_VAULT_SEED};

/// Default lifetime of a test deal.
pub const DEAL_TTL: i64 = 24 * 60 * 60;
/// What each party holds of each mint in a fresh `P2p`.
pub const P2P_ASSETS: u64 = 1_000_000;
pub const P2P_PAYMENT: u64 = 1_000_000_000_000;

pub fn deal_pda(proposer: &Pubkey, deal_id: &[u8; 16]) -> Pubkey {
    Pubkey::find_program_address(
        &[DEAL_SEED, proposer.as_ref(), deal_id.as_ref()],
        &program_id(),
    )
    .0
}

pub fn deal_vault_pda(deal: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[DEAL_VAULT_SEED, deal.as_ref()], &program_id()).0
}

/// Deterministic 16-byte deal id for tests.
pub fn deal_id(n: u128) -> [u8; 16] {
    n.to_le_bytes()
}

/// One P2P market and two parties that both hold both mints.
pub struct P2p {
    /// Asset issuer (a stranger for `Currency` assets, which have none).
    pub issuer: Keypair,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub asset_program: Pubkey,
    pub payment_program: Pubkey,
    pub proposer: Keypair,
    pub counterparty: Keypair,
    pub proposer_asset: Pubkey,
    pub proposer_payment: Pubkey,
    pub counterparty_asset: Pubkey,
    pub counterparty_payment: Pubkey,
}

impl P2p {
    pub fn deal(&self, id: [u8; 16]) -> Pubkey {
        deal_pda(&self.proposer.pubkey(), &id)
    }

    pub fn vault(&self, id: [u8; 16]) -> Pubkey {
        deal_vault_pda(&self.deal(id))
    }

    pub fn terms(&self, side: DealSide, asset_amount: u64, payment_amount: u64) -> DealTerms {
        DealTerms {
            counterparty: self.counterparty.pubkey(),
            proposer_side: side,
            asset_amount,
            payment_amount,
        }
    }

    /// The proposer's account of the mint it escrows on `side`.
    pub fn deposit_account(&self, side: DealSide) -> Pubkey {
        match side {
            DealSide::Sell => self.proposer_asset,
            DealSide::Buy => self.proposer_payment,
        }
    }
}

/// Accounts of one `open_deal` call; tests tweak fields for failure cases.
#[derive(Clone)]
pub struct OpenDealAccounts {
    pub proposer: Pubkey,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub asset_listing: Pubkey,
    pub payment_listing: Pubkey,
    pub deal: Pubkey,
    pub deal_vault: Pubkey,
    pub proposer_deposit: Pubkey,
    pub asset_program: Pubkey,
    pub payment_program: Pubkey,
}

#[derive(Clone)]
pub struct AcceptDealAccounts {
    pub counterparty: Pubkey,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub asset_listing: Pubkey,
    pub payment_listing: Pubkey,
    pub proposer: Pubkey,
    pub deal: Pubkey,
    pub deal_vault: Pubkey,
    pub counterparty_asset: Pubkey,
    pub counterparty_payment: Pubkey,
    pub proposer_proceeds: Pubkey,
    pub fee_ledger: Pubkey,
    pub fee_vault: Pubkey,
    pub cashback_ledger: Option<Pubkey>,
    pub asset_program: Pubkey,
    pub payment_program: Pubkey,
}

/// Accounts of `cancel_deal` and `expire_deal` (same layout).
#[derive(Clone)]
pub struct RefundAccounts {
    pub proposer: Pubkey,
    pub deal: Pubkey,
    pub deposit_mint: Pubkey,
    pub deal_vault: Pubkey,
    pub proposer_refund: Pubkey,
    pub token_program: Pubkey,
}

impl Env {
    /// Submits an issuer listing of `class` and approves it by quote +
    /// payment of the listing fee; returns (mint, issuer).
    pub fn listed_issuer_asset(
        &mut self,
        token_program: Pubkey,
        class: AssetClass,
    ) -> (Pubkey, Keypair) {
        let issuer = self.funded_keypair();
        let mint = self.create_mint(token_program, Some(issuer.pubkey()));
        self.set_mint_decimals(&mint, ASSET_DECIMALS);
        self.submit_listing(&issuer, mint, class, DOC_HASH).unwrap();
        self.approve_paid(&issuer, mint);
        (mint, issuer)
    }

    /// An approved listing of `class` paid in the fee mint (USDT), the
    /// cashback ledger when the class is eligible, and two funded parties.
    /// A `Currency` asset is a fresh non-fee currency (BRL -> USDT).
    pub fn p2p(&mut self, class: AssetClass, asset_program: Pubkey, payment_program: Pubkey) -> P2p {
        // The fee currency first: approving an issuer listing pays a fee in it.
        let payment_mint = self.fee_currency(payment_program);
        let (asset_mint, issuer) = if class == AssetClass::Currency {
            (self.listed_currency(asset_program), self.funded_keypair())
        } else {
            self.listed_issuer_asset(asset_program, class)
        };
        if class.cashback_eligible() {
            let payer = self.admin.insecure_clone();
            self.init_cashback_ledger(&payer, asset_mint, payment_mint)
                .unwrap();
        }
        self.p2p_parties(asset_mint, payment_mint, issuer)
    }

    /// A `Currency` deal selling USDT (the fee mint, `asset_mint`) for a fresh
    /// non-fee currency (`payment_mint`): the fee is on the asset leg.
    pub fn p2p_selling_usdt(&mut self, asset_program: Pubkey, payment_program: Pubkey) -> P2p {
        let usdt = self.fee_currency(asset_program);
        let brl = self.listed_currency(payment_program);
        let nobody = self.funded_keypair();
        self.p2p_parties(usdt, brl, nobody)
    }

    /// Two new parties for existing listings, each holding `P2P_ASSETS` and
    /// `P2P_PAYMENT`.
    pub fn p2p_parties(&mut self, asset_mint: Pubkey, payment_mint: Pubkey, issuer: Keypair) -> P2p {
        let asset_program = self.owner_of(&asset_mint);
        let payment_program = self.owner_of(&payment_mint);
        let proposer = self.funded_keypair();
        let counterparty = self.funded_keypair();
        let (p, c) = (proposer.pubkey(), counterparty.pubkey());
        let proposer_asset = self.create_token_account(asset_program, asset_mint, p, P2P_ASSETS);
        let proposer_payment =
            self.create_token_account(payment_program, payment_mint, p, P2P_PAYMENT);
        let counterparty_asset =
            self.create_token_account(asset_program, asset_mint, c, P2P_ASSETS);
        let counterparty_payment =
            self.create_token_account(payment_program, payment_mint, c, P2P_PAYMENT);
        P2p {
            issuer,
            asset_mint,
            payment_mint,
            asset_program,
            payment_program,
            proposer,
            counterparty,
            proposer_asset,
            proposer_payment,
            counterparty_asset,
            counterparty_payment,
        }
    }

    pub fn fetch_deal(&self, address: &Pubkey) -> Deal {
        self.fetch(address)
    }

    pub fn lamports(&self, address: &Pubkey) -> u64 {
        self.svm.get_account(address).map_or(0, |a| a.lamports)
    }

    /// Sends `amount` of `mint` to `destination` through the real token
    /// program from a fresh holder (an unaccounted "donation").
    pub fn donate(&mut self, mint: Pubkey, destination: Pubkey, amount: u64) {
        let program = self.owner_of(&mint);
        let donor = self.funded_keypair();
        let source = self.create_token_account(program, mint, donor.pubkey(), amount);
        let ix = anchor_spl::token_2022::spl_token_2022::instruction::transfer_checked(
            &program,
            &source,
            &mint,
            &destination,
            &donor.pubkey(),
            &[],
            amount,
            self.mint_decimals(&mint),
        )
        .unwrap();
        self.send(&[ix], &[&donor]).unwrap();
    }

    // ---- open_deal -----------------------------------------------------------

    pub fn open_deal_accounts(&self, p: &P2p, id: [u8; 16], side: DealSide) -> OpenDealAccounts {
        let deal = p.deal(id);
        OpenDealAccounts {
            proposer: p.proposer.pubkey(),
            asset_mint: p.asset_mint,
            payment_mint: p.payment_mint,
            asset_listing: listing_pda(&p.asset_mint),
            payment_listing: listing_pda(&p.payment_mint),
            deal,
            deal_vault: deal_vault_pda(&deal),
            proposer_deposit: p.deposit_account(side),
            asset_program: p.asset_program,
            payment_program: p.payment_program,
        }
    }

    /// Opens a deal that expires `DEAL_TTL` from now.
    pub fn open_deal(
        &mut self,
        p: &P2p,
        id: [u8; 16],
        side: DealSide,
        asset_amount: u64,
        payment_amount: u64,
    ) -> TransactionResult {
        let expires_at = self.now() + DEAL_TTL;
        self.open_deal_at(p, id, side, asset_amount, payment_amount, expires_at)
    }

    pub fn open_deal_at(
        &mut self,
        p: &P2p,
        id: [u8; 16],
        side: DealSide,
        asset_amount: u64,
        payment_amount: u64,
        expires_at: i64,
    ) -> TransactionResult {
        let accounts = self.open_deal_accounts(p, id, side);
        let terms = p.terms(side, asset_amount, payment_amount);
        let proposer = p.proposer.insecure_clone();
        self.open_deal_with(&proposer, &accounts, id, terms, expires_at)
    }

    pub fn open_deal_with(
        &mut self,
        signer: &Keypair,
        a: &OpenDealAccounts,
        id: [u8; 16],
        terms: DealTerms,
        expires_at: i64,
    ) -> TransactionResult {
        let ix = ix(
            accounts::OpenDeal {
                proposer: a.proposer,
                config: config_pda(),
                asset_mint: a.asset_mint,
                payment_mint: a.payment_mint,
                asset_listing: a.asset_listing,
                payment_listing: a.payment_listing,
                deal: a.deal,
                deal_vault: a.deal_vault,
                proposer_deposit_account: a.proposer_deposit,
                asset_token_program: a.asset_program,
                payment_token_program: a.payment_program,
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::OpenDeal {
                deal_id: id,
                terms,
                expires_at,
            },
        );
        self.send(&[ix], &[signer])
    }

    // ---- accept_deal ---------------------------------------------------------

    /// Accounts for accepting deal `id` of `p` as its counterparty, with the
    /// fee ledger and vault of the env's fee mint. The cashback ledger is
    /// passed when it exists.
    pub fn accept_deal_accounts(&self, p: &P2p, id: [u8; 16]) -> AcceptDealAccounts {
        let deal = p.deal(id);
        let side = self.fetch_deal(&deal).proposer_side;
        let ledger = cashback_pda(&p.asset_mint, &self.fee_mint);
        AcceptDealAccounts {
            counterparty: p.counterparty.pubkey(),
            asset_mint: p.asset_mint,
            payment_mint: p.payment_mint,
            asset_listing: listing_pda(&p.asset_mint),
            payment_listing: listing_pda(&p.payment_mint),
            proposer: p.proposer.pubkey(),
            deal,
            deal_vault: deal_vault_pda(&deal),
            counterparty_asset: p.counterparty_asset,
            counterparty_payment: p.counterparty_payment,
            proposer_proceeds: match side {
                DealSide::Sell => p.proposer_payment,
                DealSide::Buy => p.proposer_asset,
            },
            fee_ledger: fee_ledger_pda(&self.fee_mint),
            fee_vault: fee_vault_pda(&self.fee_mint),
            cashback_ledger: self.exists(&ledger).then_some(ledger),
            asset_program: p.asset_program,
            payment_program: p.payment_program,
        }
    }

    pub fn accept_deal(&mut self, p: &P2p, id: [u8; 16]) -> TransactionResult {
        let accounts = self.accept_deal_accounts(p, id);
        let counterparty = p.counterparty.insecure_clone();
        self.accept_deal_with(&counterparty, &accounts)
    }

    pub fn accept_deal_with(&mut self, signer: &Keypair, a: &AcceptDealAccounts) -> TransactionResult {
        let ix = ix(
            accounts::AcceptDeal {
                counterparty: a.counterparty,
                config: config_pda(),
                asset_mint: a.asset_mint,
                payment_mint: a.payment_mint,
                asset_listing: a.asset_listing,
                payment_listing: a.payment_listing,
                proposer: a.proposer,
                deal: a.deal,
                deal_vault: a.deal_vault,
                counterparty_asset_account: a.counterparty_asset,
                counterparty_payment_account: a.counterparty_payment,
                proposer_proceeds_account: a.proposer_proceeds,
                fee_ledger: a.fee_ledger,
                fee_vault: a.fee_vault,
                cashback_ledger: a.cashback_ledger,
                asset_token_program: a.asset_program,
                payment_token_program: a.payment_program,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::AcceptDeal {},
        );
        self.send(&[ix], &[signer])
    }

    // ---- cancel_deal / expire_deal ---------------------------------------------

    /// Refund to the proposer's own account of the deposited mint.
    pub fn refund_accounts(&self, p: &P2p, id: [u8; 16]) -> RefundAccounts {
        let deal = p.deal(id);
        let side = self.fetch_deal(&deal).proposer_side;
        let (deposit_mint, token_program) = match side {
            DealSide::Sell => (p.asset_mint, p.asset_program),
            DealSide::Buy => (p.payment_mint, p.payment_program),
        };
        RefundAccounts {
            proposer: p.proposer.pubkey(),
            deal,
            deposit_mint,
            deal_vault: deal_vault_pda(&deal),
            proposer_refund: p.deposit_account(side),
            token_program,
        }
    }

    pub fn cancel_deal(&mut self, p: &P2p, id: [u8; 16]) -> TransactionResult {
        let accounts = self.refund_accounts(p, id);
        let proposer = p.proposer.insecure_clone();
        self.cancel_deal_with(&proposer, &accounts)
    }

    pub fn cancel_deal_with(&mut self, signer: &Keypair, a: &RefundAccounts) -> TransactionResult {
        let ix = ix(
            accounts::CancelDeal {
                proposer: a.proposer,
                deal: a.deal,
                deposit_mint: a.deposit_mint,
                deal_vault: a.deal_vault,
                proposer_refund_account: a.proposer_refund,
                token_program: a.token_program,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::CancelDeal {},
        );
        self.send(&[ix], &[signer])
    }

    /// Cranked by a fresh stranger, who only pays the transaction fee.
    pub fn expire_deal(&mut self, p: &P2p, id: [u8; 16]) -> TransactionResult {
        let accounts = self.refund_accounts(p, id);
        let cranker = self.funded_keypair();
        self.expire_deal_with(&cranker, &accounts)
    }

    pub fn expire_deal_with(&mut self, payer: &Keypair, a: &RefundAccounts) -> TransactionResult {
        let ix = ix(
            accounts::ExpireDeal {
                proposer: a.proposer,
                deal: a.deal,
                deposit_mint: a.deposit_mint,
                deal_vault: a.deal_vault,
                proposer_refund_account: a.proposer_refund,
                token_program: a.token_program,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::ExpireDeal {},
        );
        self.send(&[ix], &[payer])
    }
}
