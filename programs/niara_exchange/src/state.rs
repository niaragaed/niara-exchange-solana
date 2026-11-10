use anchor_lang::prelude::*;

use crate::{constants::*, error::ExchangeError};

/// Global protocol configuration. PDA: `["config"]`.
#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    pub listing_authority: Pubkey,
    pub settlement_operator: Pubkey,
    pub treasury: Pubkey,
    /// Protocol fee, in bps of the payment amount. Always `<= MAX_FEE_BPS`.
    pub fee_bps: u16,
    /// Issuer cashback, in bps **of the fee**. Always `<= BPS_DENOMINATOR`.
    pub cashback_bps: u16,
    /// Circuit breaker. Trading instructions revert while set.
    pub paused: bool,
    /// Delay between `propose_change` and `execute_change`. Fixed at init.
    pub timelock_secs: i64,
    pub bump: u8,
}

impl Config {
    pub fn require_not_paused(&self) -> Result<()> {
        require!(!self.paused, ExchangeError::Paused);
        Ok(())
    }
}

pub fn validate_fee_bps(fee_bps: u16) -> Result<()> {
    require!(fee_bps <= MAX_FEE_BPS, ExchangeError::FeeTooHigh);
    Ok(())
}

pub fn validate_cashback_bps(cashback_bps: u16) -> Result<()> {
    require!(
        cashback_bps <= BPS_DENOMINATOR,
        ExchangeError::CashbackTooHigh
    );
    Ok(())
}

pub fn validate_timelock(timelock_secs: i64) -> Result<()> {
    require!(
        (MIN_TIMELOCK_SECS..=MAX_TIMELOCK_SECS).contains(&timelock_secs),
        ExchangeError::InvalidTimelock
    );
    Ok(())
}

pub fn validate_address(address: &Pubkey) -> Result<()> {
    require!(*address != Pubkey::default(), ExchangeError::InvalidAddress);
    Ok(())
}

/// A timelocked change to `Config`. Each variant has its own pending slot
/// (`["pending", kind]`), so at most one proposal per kind is in flight.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum ConfigChange {
    FeeBps(u16),
    CashbackBps(u16),
    SettlementOperator(Pubkey),
    ListingAuthority(Pubkey),
    Treasury(Pubkey),
    Admin(Pubkey),
}

impl ConfigChange {
    pub fn kind(&self) -> u8 {
        match self {
            ConfigChange::FeeBps(_) => 0,
            ConfigChange::CashbackBps(_) => 1,
            ConfigChange::SettlementOperator(_) => 2,
            ConfigChange::ListingAuthority(_) => 3,
            ConfigChange::Treasury(_) => 4,
            ConfigChange::Admin(_) => 5,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            ConfigChange::FeeBps(v) => validate_fee_bps(*v),
            ConfigChange::CashbackBps(v) => validate_cashback_bps(*v),
            ConfigChange::SettlementOperator(k)
            | ConfigChange::ListingAuthority(k)
            | ConfigChange::Treasury(k)
            | ConfigChange::Admin(k) => validate_address(k),
        }
    }

    /// Applies the change. Re-validates, so a bound tightened by a program
    /// upgrade between propose and execute still holds.
    pub fn apply(&self, config: &mut Config) -> Result<()> {
        self.validate()?;
        match *self {
            ConfigChange::FeeBps(v) => config.fee_bps = v,
            ConfigChange::CashbackBps(v) => config.cashback_bps = v,
            ConfigChange::SettlementOperator(k) => config.settlement_operator = k,
            ConfigChange::ListingAuthority(k) => config.listing_authority = k,
            ConfigChange::Treasury(k) => config.treasury = k,
            ConfigChange::Admin(k) => config.admin = k,
        }
        Ok(())
    }
}

/// A proposed `ConfigChange` waiting for its timelock. PDA: `["pending", kind]`.
#[account]
#[derive(InitSpace)]
pub struct PendingChange {
    pub change: ConfigChange,
    pub execute_after: i64,
    pub bump: u8,
}

/// Asset class. Fixes the trading mode on-chain:
/// `Equity` -> order book (delegate); the others -> P2P escrow.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum AssetClass {
    Equity,
    FixedIncome,
    Credit,
    Currency,
}

impl AssetClass {
    /// `Currency` listings never accrue cashback.
    pub fn cashback_eligible(&self) -> bool {
        !matches!(self, AssetClass::Currency)
    }

    /// `settle` (order book) accepts only `Equity`.
    pub fn is_order_book(&self) -> bool {
        matches!(self, AssetClass::Equity)
    }

    /// `open_deal` / `accept_deal` (P2P escrow) accept everything but `Equity`.
    pub fn is_p2p(&self) -> bool {
        !self.is_order_book()
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum ListingStatus {
    Pending,
    Approved,
    Suspended,
    Delisted,
}

impl ListingStatus {
    /// Allowed transitions:
    /// - `Pending | Suspended -> Approved`
    /// - `Approved -> Suspended`
    /// - `Pending | Approved | Suspended -> Delisted` (terminal)
    pub fn can_transition_to(self, next: ListingStatus) -> bool {
        use ListingStatus::*;
        matches!(
            (self, next),
            (Pending, Approved)
                | (Suspended, Approved)
                | (Approved, Suspended)
                | (Pending, Delisted)
                | (Approved, Delisted)
                | (Suspended, Delisted)
        )
    }
}

/// Listing of an asset mint. PDA: `["listing", asset_mint]`.
#[account]
#[derive(InitSpace)]
pub struct Listing {
    pub asset_mint: Pubkey,
    /// Mint authority at submission time. `Pubkey::default()` for `Currency`
    /// listings, which have no issuer on Niara.
    pub issuer: Pubkey,
    pub asset_class: AssetClass,
    pub status: ListingStatus,
    pub cashback_eligible: bool,
    /// Hash of the offering document (registered in Niara-Register).
    pub doc_hash: [u8; 32],
    /// Unix timestamp of the latest approval; 0 until approved.
    pub approved_at: i64,
    /// Number of the next issuance batch (batches are numbered from 1).
    /// 0 for `Currency` listings, which have no issuance.
    pub next_batch_no: u64,
    /// Sum of executed issuance batches.
    pub issued: u64,
    /// Sum of burns through `burn_with_reason`. Invariant 11:
    /// `supply == issued - burned` (on-chain `<=`: holders can also burn
    /// directly through the token program).
    pub burned: u64,
    pub bump: u8,
}

impl Listing {
    /// The only listings accepted as a payment mint (invariant 10).
    pub fn is_approved_currency(&self) -> bool {
        self.asset_class == AssetClass::Currency && self.status == ListingStatus::Approved
    }
}

/// Issuer cashback owed for one listing, in one payment mint.
/// PDA: `["cashback", listing, payment_mint]`. Created by `init_cashback_ledger`.
#[account]
#[derive(InitSpace, Debug)]
pub struct CashbackLedger {
    pub listing: Pubkey,
    pub payment_mint: Pubkey,
    pub accrued: u64,
    pub withdrawn: u64,
    pub bump: u8,
}

impl CashbackLedger {
    pub fn available(&self) -> Result<u64> {
        self.accrued
            .checked_sub(self.withdrawn)
            .ok_or_else(|| error!(ExchangeError::MathOverflow))
    }
}

/// Fee accounting for one payment mint. PDA: `["fees", payment_mint]`.
/// Created by `list_currency` together with the fee vault
/// `["fee_vault", payment_mint]`.
///
/// `fees_collected` is the gross fee (protocol share + cashback). Derived:
/// - protocol share accrued = `fees_collected - cashback_accrued`
/// - unwithdrawn = `fees_collected - protocol_withdrawn - cashback_withdrawn`
///
/// Invariant 4: the fee vault balance equals the unwithdrawn amount. Anyone
/// can transfer tokens into the vault, so between sweeps it is `>=`; such
/// donations are not accounted, cannot be withdrawn as fees or cashback, and
/// `sweep_excess` (treasury) moves them out, restoring equality.
#[account]
#[derive(InitSpace, Debug)]
pub struct FeeLedger {
    pub payment_mint: Pubkey,
    pub fees_collected: u64,
    /// Sum of `CashbackLedger::accrued` over all listings, in this mint.
    pub cashback_accrued: u64,
    pub protocol_withdrawn: u64,
    /// Sum of `CashbackLedger::withdrawn` over all listings, in this mint.
    pub cashback_withdrawn: u64,
    pub bump: u8,
    pub vault_bump: u8,
}

impl FeeLedger {
    /// Records one settlement's fee, of which `cashback` goes to the issuer.
    pub fn record_fee(&mut self, fee: u64, cashback: u64) -> Result<()> {
        require!(cashback <= fee, ExchangeError::MathOverflow);
        self.fees_collected = self
            .fees_collected
            .checked_add(fee)
            .ok_or(ExchangeError::MathOverflow)?;
        self.cashback_accrued = self
            .cashback_accrued
            .checked_add(cashback)
            .ok_or(ExchangeError::MathOverflow)?;
        Ok(())
    }

    /// Protocol share not yet withdrawn by the treasury.
    pub fn protocol_available(&self) -> Result<u64> {
        self.fees_collected
            .checked_sub(self.cashback_accrued)
            .and_then(|accrued| accrued.checked_sub(self.protocol_withdrawn))
            .ok_or_else(|| error!(ExchangeError::MathOverflow))
    }

    /// What the fee vault must hold (invariant 4).
    pub fn unwithdrawn(&self) -> Result<u64> {
        self.fees_collected
            .checked_sub(self.protocol_withdrawn)
            .and_then(|v| v.checked_sub(self.cashback_withdrawn))
            .ok_or_else(|| error!(ExchangeError::MathOverflow))
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum BatchStatus {
    Requested,
    Executed,
    Rejected,
}

/// One requested increase of supply (spec §3.12). PDA:
/// `["batch", listing, batch_no (u64 LE)]`. Never closed: it is the on-chain
/// issuance record.
#[account]
#[derive(InitSpace, Debug)]
pub struct IssuanceBatch {
    pub listing: Pubkey,
    pub batch_no: u64,
    pub amount: u64,
    /// Wallet that receives the tokens: the holder of record.
    pub recipient: Pubkey,
    /// Hash of the document authorizing the batch, registered in Niara-Register.
    pub doc_hash: [u8; 32],
    pub status: BatchStatus,
    pub requested_at: i64,
    /// When approved or rejected; 0 while `Requested`.
    pub resolved_at: i64,
    pub bump: u8,
}

/// Why a holder burned tokens (`burn_with_reason`).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum BurnReason {
    Redemption,
    Cancellation,
}

/// Replay guard for one settled match. PDA: `["settled", order_id]`.
/// Never closed: closing it would allow the same `order_id` to settle again.
#[account]
#[derive(InitSpace)]
pub struct SettlementRecord {
    pub bump: u8,
}

/// The proposer's side of a P2P deal. It decides what the proposer escrows:
/// the asset when selling, the payment when buying.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum DealSide {
    Sell,
    Buy,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum DealStatus {
    Open,
    Executed,
    Cancelled,
    Expired,
}

/// Terms of a P2P deal as agreed off-chain. The mints come from the accounts
/// of `open_deal`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct DealTerms {
    /// The only wallet that can accept (v1 has no open offers).
    pub counterparty: Pubkey,
    pub proposer_side: DealSide,
    /// Raw units of the asset mint.
    pub asset_amount: u64,
    /// Raw units of the payment mint; the fee is taken out of it.
    pub payment_amount: u64,
}

/// A bilateral P2P deal (spec §3.5). PDA: `["deal", proposer, deal_id]`.
/// Never closed: once it leaves `Open` it can be neither accepted nor
/// refunded again (the status is the replay guard).
#[account]
#[derive(InitSpace, Debug)]
pub struct Deal {
    /// Off-chain negotiation id (UUID).
    pub deal_id: [u8; 16],
    pub proposer: Pubkey,
    pub counterparty: Pubkey,
    pub asset_mint: Pubkey,
    pub payment_mint: Pubkey,
    pub asset_amount: u64,
    pub payment_amount: u64,
    pub proposer_side: DealSide,
    /// The deal can be accepted while `now < expires_at`, and expired once
    /// `now >= expires_at`.
    pub expires_at: i64,
    pub status: DealStatus,
    pub opened_at: i64,
    /// When executed, cancelled or expired; 0 while `Open`.
    pub resolved_at: i64,
    pub bump: u8,
    /// Bump of the escrow vault `["deal_vault", deal]`.
    pub vault_bump: u8,
}

impl Deal {
    /// The mint the proposer escrowed.
    pub fn deposit_mint(&self) -> Pubkey {
        match self.proposer_side {
            DealSide::Sell => self.asset_mint,
            DealSide::Buy => self.payment_mint,
        }
    }

    /// How much of `deposit_mint` the proposer escrowed.
    pub fn deposit_amount(&self) -> u64 {
        match self.proposer_side {
            DealSide::Sell => self.asset_amount,
            DealSide::Buy => self.payment_amount,
        }
    }

    /// The mint the proposer receives on execution (the counterparty's side).
    pub fn proceeds_mint(&self) -> Pubkey {
        match self.proposer_side {
            DealSide::Sell => self.payment_mint,
            DealSide::Buy => self.asset_mint,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ListingStatus::*;

    #[test]
    fn status_transitions() {
        let all = [Pending, Approved, Suspended, Delisted];
        let allowed = [
            (Pending, Approved),
            (Suspended, Approved),
            (Approved, Suspended),
            (Pending, Delisted),
            (Approved, Delisted),
            (Suspended, Delisted),
        ];
        for from in all {
            for to in all {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn asset_class_modes() {
        assert!(AssetClass::Equity.is_order_book());
        assert!(!AssetClass::Equity.is_p2p());
        for c in [AssetClass::FixedIncome, AssetClass::Credit, AssetClass::Currency] {
            assert!(c.is_p2p());
            assert!(!c.is_order_book());
        }
        assert!(AssetClass::Equity.cashback_eligible());
        assert!(AssetClass::FixedIncome.cashback_eligible());
        assert!(AssetClass::Credit.cashback_eligible());
        assert!(!AssetClass::Currency.cashback_eligible());
    }

    #[test]
    fn change_kinds_are_unique() {
        let k = Pubkey::new_unique();
        let kinds: Vec<u8> = [
            ConfigChange::FeeBps(0),
            ConfigChange::CashbackBps(0),
            ConfigChange::SettlementOperator(k),
            ConfigChange::ListingAuthority(k),
            ConfigChange::Treasury(k),
            ConfigChange::Admin(k),
        ]
        .iter()
        .map(ConfigChange::kind)
        .collect();
        let mut dedup = kinds.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(kinds.len(), dedup.len());
    }

    #[test]
    fn change_validation() {
        assert!(ConfigChange::FeeBps(MAX_FEE_BPS).validate().is_ok());
        assert!(ConfigChange::FeeBps(MAX_FEE_BPS + 1).validate().is_err());
        assert!(ConfigChange::CashbackBps(BPS_DENOMINATOR).validate().is_ok());
        assert!(ConfigChange::CashbackBps(BPS_DENOMINATOR + 1).validate().is_err());
        assert!(ConfigChange::Admin(Pubkey::default()).validate().is_err());
        assert!(ConfigChange::Treasury(Pubkey::new_unique()).validate().is_ok());
    }

    #[test]
    fn deal_sides() {
        let asset = Pubkey::new_unique();
        let payment = Pubkey::new_unique();
        let mut deal = Deal {
            deal_id: [1; 16],
            proposer: Pubkey::new_unique(),
            counterparty: Pubkey::new_unique(),
            asset_mint: asset,
            payment_mint: payment,
            asset_amount: 3,
            payment_amount: 500,
            proposer_side: DealSide::Sell,
            expires_at: 0,
            status: DealStatus::Open,
            opened_at: 0,
            resolved_at: 0,
            bump: 0,
            vault_bump: 0,
        };
        assert_eq!((deal.deposit_mint(), deal.deposit_amount()), (asset, 3));
        assert_eq!(deal.proceeds_mint(), payment);
        deal.proposer_side = DealSide::Buy;
        assert_eq!((deal.deposit_mint(), deal.deposit_amount()), (payment, 500));
        assert_eq!(deal.proceeds_mint(), asset);
    }

    #[test]
    fn timelock_bounds() {
        assert!(validate_timelock(MIN_TIMELOCK_SECS - 1).is_err());
        assert!(validate_timelock(MIN_TIMELOCK_SECS).is_ok());
        assert!(validate_timelock(MAX_TIMELOCK_SECS).is_ok());
        assert!(validate_timelock(MAX_TIMELOCK_SECS + 1).is_err());
        assert!(validate_timelock(-1).is_err());
    }
}
