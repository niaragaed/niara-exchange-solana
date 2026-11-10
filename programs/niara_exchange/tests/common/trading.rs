//! Trading fixtures: token accounts, delegations and a ready-to-settle market.

use super::*;
use anchor_spl::token::spl_token::state::{Account as TokenAccountState, AccountState};
use niara_exchange::{
    CashbackLedger, FeeLedger, CASHBACK_SEED, FEE_LEDGER_SEED, FEE_VAULT_SEED, SETTLED_SEED,
    VAULT_SEED,
};

pub const SPL_TOKEN: Pubkey = anchor_spl::token::ID;
pub const TOKEN_2022: Pubkey = anchor_spl::token_2022::ID;
pub const ASSET_DECIMALS: u8 = 0;
pub const PAYMENT_DECIMALS: u8 = 6;

pub fn vault_authority() -> Pubkey {
    Pubkey::find_program_address(&[VAULT_SEED], &program_id()).0
}

pub fn fee_vault_pda(payment_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[FEE_VAULT_SEED, payment_mint.as_ref()], &program_id()).0
}

pub fn fee_ledger_pda(payment_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[FEE_LEDGER_SEED, payment_mint.as_ref()], &program_id()).0
}

pub fn cashback_pda(asset_mint: &Pubkey, payment_mint: &Pubkey) -> Pubkey {
    let listing = listing_pda(asset_mint);
    Pubkey::find_program_address(
        &[CASHBACK_SEED, listing.as_ref(), payment_mint.as_ref()],
        &program_id(),
    )
    .0
}

pub fn settled_pda(order_id: &[u8; 16]) -> Pubkey {
    Pubkey::find_program_address(&[SETTLED_SEED, order_id.as_ref()], &program_id()).0
}

/// Deterministic 16-byte match id for tests.
pub fn order_id(n: u128) -> [u8; 16] {
    n.to_le_bytes()
}

/// Everything `settle` needs, for one asset/payment pair and one buyer/seller.
pub struct Market {
    pub issuer: Keypair,
    pub seller: Keypair,
    pub buyer: Keypair,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub asset_program: Pubkey,
    pub payment_program: Pubkey,
    pub seller_asset: Pubkey,
    pub buyer_asset: Pubkey,
    pub buyer_payment: Pubkey,
    pub seller_payment: Pubkey,
}

impl Market {
    pub fn cashback_ledger(&self) -> Pubkey {
        cashback_pda(&self.asset_mint, &self.payment_mint)
    }
}

/// Accounts of one `settle` call; tests tweak fields to build failure cases.
#[derive(Clone)]
pub struct SettleAccounts {
    pub operator: Pubkey,
    pub asset_listing: Pubkey,
    pub payment_listing: Pubkey,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub seller: Pubkey,
    pub buyer: Pubkey,
    pub seller_asset: Pubkey,
    pub buyer_asset: Pubkey,
    pub buyer_payment: Pubkey,
    pub seller_payment: Pubkey,
    pub vault_authority: Pubkey,
    pub fee_vault: Pubkey,
    pub fee_ledger: Pubkey,
    pub cashback_ledger: Pubkey,
    pub asset_program: Pubkey,
    pub payment_program: Pubkey,
}

impl Env {
    /// Writes an initialized token account (base layout, valid for SPL Token
    /// and for a Token-2022 account without extensions).
    pub fn create_token_account(
        &mut self,
        token_program: Pubkey,
        mint: Pubkey,
        owner: Pubkey,
        amount: u64,
    ) -> Pubkey {
        let address = Pubkey::new_unique();
        let state = TokenAccountState {
            mint,
            owner,
            amount,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut data = vec![0u8; TokenAccountState::LEN];
        TokenAccountState::pack(state, &mut data).unwrap();
        let lamports = self
            .svm
            .minimum_balance_for_rent_exemption(TokenAccountState::LEN);
        self.svm
            .set_account(
                address,
                Account {
                    lamports,
                    data,
                    owner: token_program,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        address
    }

    pub fn token_account(&self, address: &Pubkey) -> TokenAccountState {
        let account = self.svm.get_account(address).expect("token account missing");
        TokenAccountState::unpack_from_slice(&account.data[..TokenAccountState::LEN]).unwrap()
    }

    pub fn balance(&self, address: &Pubkey) -> u64 {
        self.token_account(address).amount
    }

    pub fn update_token_account(
        &mut self,
        address: &Pubkey,
        f: impl FnOnce(&mut TokenAccountState),
    ) {
        let mut account = self.svm.get_account(address).unwrap();
        let mut state =
            TokenAccountState::unpack_from_slice(&account.data[..TokenAccountState::LEN]).unwrap();
        f(&mut state);
        TokenAccountState::pack_into_slice(&state, &mut account.data[..TokenAccountState::LEN]);
        self.svm.set_account(*address, account).unwrap();
    }

    /// Approves `amount` to the vault PDA through the real token program, as a
    /// wallet would when placing an order.
    pub fn approve_vault(
        &mut self,
        owner: &Keypair,
        token_program: Pubkey,
        account: Pubkey,
        mint: Pubkey,
        amount: u64,
    ) {
        let decimals = self.mint_decimals(&mint);
        let ix = anchor_spl::token_2022::spl_token_2022::instruction::approve_checked(
            &token_program,
            &account,
            &mint,
            &vault_authority(),
            &owner.pubkey(),
            &[],
            amount,
            decimals,
        )
        .unwrap();
        self.send(&[ix], &[owner]).unwrap();
    }

    /// Mint base layout: `decimals` at offset 44 (SPL Token and Token-2022).
    pub fn set_mint_decimals(&mut self, mint: &Pubkey, decimals: u8) {
        let mut account = self.svm.get_account(mint).unwrap();
        account.data[44] = decimals;
        self.svm.set_account(*mint, account).unwrap();
    }

    pub fn mint_decimals(&self, mint: &Pubkey) -> u8 {
        self.svm.get_account(mint).unwrap().data[44]
    }

    pub fn owner_of(&self, address: &Pubkey) -> Pubkey {
        self.svm.get_account(address).unwrap().owner
    }

    /// Lists `payment_mint` as an approved currency and returns it.
    pub fn listed_currency(&mut self, token_program: Pubkey) -> Pubkey {
        let mint = self.create_mint(token_program, Some(Pubkey::new_unique()));
        self.set_mint_decimals(&mint, PAYMENT_DECIMALS);
        let authority = self.listing_authority.insecure_clone();
        self.list_currency(&authority, mint).unwrap();
        mint
    }

    /// Submits and approves an `Equity` listing; returns (mint, issuer).
    pub fn listed_equity(&mut self, token_program: Pubkey) -> (Pubkey, Keypair) {
        let issuer = self.funded_keypair();
        let mint = self.create_mint(token_program, Some(issuer.pubkey()));
        self.set_mint_decimals(&mint, ASSET_DECIMALS);
        self.submit_listing(&issuer, mint, AssetClass::Equity, DOC_HASH)
            .unwrap();
        let authority = self.listing_authority.insecure_clone();
        self.approve(&authority, mint).unwrap();
        (mint, issuer)
    }

    /// A ready-to-settle market: approved equity + approved currency, cashback
    /// ledger, funded accounts, and both delegations granted to the vault.
    pub fn market(&mut self, asset_program: Pubkey, payment_program: Pubkey) -> Market {
        let (asset_mint, issuer) = self.listed_equity(asset_program);
        let payment_mint = self.listed_currency(payment_program);
        let payer = self.admin.insecure_clone();
        self.init_cashback_ledger(&payer, asset_mint, payment_mint)
            .unwrap();
        self.trader_pair(asset_mint, payment_mint, issuer, 1_000, 1_000_000_000)
    }

    /// New seller/buyer for existing listings. The seller holds and delegates
    /// `assets`; the buyer holds and delegates `payment`.
    pub fn trader_pair(
        &mut self,
        asset_mint: Pubkey,
        payment_mint: Pubkey,
        issuer: Keypair,
        assets: u64,
        payment: u64,
    ) -> Market {
        let asset_program = self.owner_of(&asset_mint);
        let payment_program = self.owner_of(&payment_mint);
        let seller = self.funded_keypair();
        let buyer = self.funded_keypair();
        let seller_asset =
            self.create_token_account(asset_program, asset_mint, seller.pubkey(), assets);
        let buyer_asset = self.create_token_account(asset_program, asset_mint, buyer.pubkey(), 0);
        let buyer_payment =
            self.create_token_account(payment_program, payment_mint, buyer.pubkey(), payment);
        let seller_payment =
            self.create_token_account(payment_program, payment_mint, seller.pubkey(), 0);
        self.approve_vault(&seller, asset_program, seller_asset, asset_mint, assets);
        self.approve_vault(&buyer, payment_program, buyer_payment, payment_mint, payment);
        Market {
            issuer,
            seller,
            buyer,
            asset_mint,
            payment_mint,
            asset_program,
            payment_program,
            seller_asset,
            buyer_asset,
            buyer_payment,
            seller_payment,
        }
    }

    pub fn settle_accounts(&self, m: &Market) -> SettleAccounts {
        SettleAccounts {
            operator: self.operator.pubkey(),
            asset_listing: listing_pda(&m.asset_mint),
            payment_listing: listing_pda(&m.payment_mint),
            asset_mint: m.asset_mint,
            payment_mint: m.payment_mint,
            seller: m.seller.pubkey(),
            buyer: m.buyer.pubkey(),
            seller_asset: m.seller_asset,
            buyer_asset: m.buyer_asset,
            buyer_payment: m.buyer_payment,
            seller_payment: m.seller_payment,
            vault_authority: vault_authority(),
            fee_vault: fee_vault_pda(&m.payment_mint),
            fee_ledger: fee_ledger_pda(&m.payment_mint),
            cashback_ledger: m.cashback_ledger(),
            asset_program: m.asset_program,
            payment_program: m.payment_program,
        }
    }

    pub fn settle(
        &mut self,
        m: &Market,
        id: [u8; 16],
        asset_amount: u64,
        payment_amount: u64,
    ) -> TransactionResult {
        let accounts = self.settle_accounts(m);
        let operator = self.operator.insecure_clone();
        self.settle_with(&operator, &accounts, id, asset_amount, payment_amount)
    }

    pub fn settle_with(
        &mut self,
        signer: &Keypair,
        a: &SettleAccounts,
        id: [u8; 16],
        asset_amount: u64,
        payment_amount: u64,
    ) -> TransactionResult {
        let ix = ix(
            accounts::Settle {
                settlement_operator: a.operator,
                config: config_pda(),
                asset_listing: a.asset_listing,
                payment_listing: a.payment_listing,
                asset_mint: a.asset_mint,
                payment_mint: a.payment_mint,
                seller: a.seller,
                buyer: a.buyer,
                seller_asset_account: a.seller_asset,
                buyer_asset_account: a.buyer_asset,
                buyer_payment_account: a.buyer_payment,
                seller_payment_account: a.seller_payment,
                vault_authority: a.vault_authority,
                fee_ledger: a.fee_ledger,
                fee_vault: a.fee_vault,
                cashback_ledger: a.cashback_ledger,
                settlement_record: settled_pda(&id),
                asset_token_program: a.asset_program,
                payment_token_program: a.payment_program,
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::Settle {
                order_id: id,
                asset_amount,
                payment_amount,
            },
        );
        self.send(&[ix], &[signer])
    }

    pub fn init_cashback_ledger(
        &mut self,
        payer: &Keypair,
        asset_mint: Pubkey,
        payment_mint: Pubkey,
    ) -> TransactionResult {
        let ix = ix(
            accounts::InitCashbackLedger {
                payer: payer.pubkey(),
                listing: listing_pda(&asset_mint),
                payment_listing: listing_pda(&payment_mint),
                cashback_ledger: cashback_pda(&asset_mint, &payment_mint),
                system_program: system_program::ID,
            },
            instruction::InitCashbackLedger {},
        );
        self.send(&[ix], &[payer])
    }

    pub fn fee_ledger(&self, payment_mint: &Pubkey) -> FeeLedger {
        self.fetch(&fee_ledger_pda(payment_mint))
    }

    /// Invariant 4 (no donations): fee vault balance == unwithdrawn fees.
    pub fn assert_vault_matches_ledger(&self, payment_mint: &Pubkey) {
        let ledger = self.fee_ledger(payment_mint);
        assert_eq!(
            self.balance(&fee_vault_pda(payment_mint)),
            ledger.fees_collected - ledger.protocol_withdrawn - ledger.cashback_withdrawn,
            "fee vault balance != unwithdrawn fees: {ledger:?}"
        );
    }

    pub fn cashback(&self, m: &Market) -> CashbackLedger {
        self.fetch(&m.cashback_ledger())
    }

    pub fn withdraw_cashback(
        &mut self,
        issuer: &Keypair,
        asset_mint: Pubkey,
        payment_mint: Pubkey,
        destination: Pubkey,
        amount: u64,
    ) -> TransactionResult {
        let ix = ix(
            accounts::WithdrawCashback {
                issuer: issuer.pubkey(),
                config: config_pda(),
                listing: listing_pda(&asset_mint),
                payment_mint,
                cashback_ledger: cashback_pda(&asset_mint, &payment_mint),
                fee_ledger: fee_ledger_pda(&payment_mint),
                vault_authority: vault_authority(),
                fee_vault: fee_vault_pda(&payment_mint),
                destination,
                token_program: self.owner_of(&payment_mint),
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::WithdrawCashback { amount },
        );
        self.send(&[ix], &[issuer])
    }

    pub fn withdraw_protocol_fees(
        &mut self,
        treasury: &Keypair,
        payment_mint: Pubkey,
        destination: Pubkey,
        amount: u64,
    ) -> TransactionResult {
        let ix = ix(
            accounts::WithdrawProtocolFees {
                treasury: treasury.pubkey(),
                config: config_pda(),
                payment_mint,
                fee_ledger: fee_ledger_pda(&payment_mint),
                vault_authority: vault_authority(),
                fee_vault: fee_vault_pda(&payment_mint),
                destination,
                token_program: self.owner_of(&payment_mint),
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::WithdrawProtocolFees { amount },
        );
        self.send(&[ix], &[treasury])
    }

    pub fn sweep_excess(
        &mut self,
        treasury: &Keypair,
        payment_mint: Pubkey,
        destination: Pubkey,
    ) -> TransactionResult {
        let ix = ix(
            accounts::SweepExcess {
                treasury: treasury.pubkey(),
                config: config_pda(),
                payment_mint,
                fee_ledger: fee_ledger_pda(&payment_mint),
                vault_authority: vault_authority(),
                fee_vault: fee_vault_pda(&payment_mint),
                destination,
                token_program: self.owner_of(&payment_mint),
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::SweepExcess {},
        );
        self.send(&[ix], &[treasury])
    }

    /// Sends `amount` of `payment_mint` straight to its fee vault through the
    /// real token program, as any holder can (an unaccounted "donation").
    pub fn donate_to_fee_vault(&mut self, payment_mint: Pubkey, amount: u64) {
        let program = self.owner_of(&payment_mint);
        let donor = self.funded_keypair();
        let source = self.create_token_account(program, payment_mint, donor.pubkey(), amount);
        let ix = anchor_spl::token_2022::spl_token_2022::instruction::transfer_checked(
            &program,
            &source,
            &payment_mint,
            &fee_vault_pda(&payment_mint),
            &donor.pubkey(),
            &[],
            amount,
            self.mint_decimals(&payment_mint),
        )
        .unwrap();
        self.send(&[ix], &[&donor]).unwrap();
    }
}
