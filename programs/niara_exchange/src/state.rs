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
/// can transfer tokens into the vault, so on-chain it is strictly `>=`; such
/// donations are not accounted and cannot be withdrawn.
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

/// Replay guard for one settled match. PDA: `["settled", order_id]`.
/// Never closed: closing it would allow the same `order_id` to settle again.
#[account]
#[derive(InitSpace)]
pub struct SettlementRecord {
    pub bump: u8,
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
    fn timelock_bounds() {
        assert!(validate_timelock(MIN_TIMELOCK_SECS - 1).is_err());
        assert!(validate_timelock(MIN_TIMELOCK_SECS).is_ok());
        assert!(validate_timelock(MAX_TIMELOCK_SECS).is_ok());
        assert!(validate_timelock(MAX_TIMELOCK_SECS + 1).is_err());
        assert!(validate_timelock(-1).is_err());
    }
}
