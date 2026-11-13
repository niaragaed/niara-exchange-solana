//! Settlement math (spec §3.4): checked arithmetic with `u128` intermediates.

use anchor_lang::prelude::*;

use crate::{constants::BPS_DENOMINATOR, error::ExchangeError};

/// How a payment is split between the seller, the protocol and the issuer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentSplit {
    /// Protocol fee taken from the payment (includes the cashback share).
    pub fee: u64,
    /// Share of `fee` credited to the issuer. Always `<= fee`.
    pub cashback: u64,
    /// `payment_amount - fee`.
    pub seller_receives: u64,
}

/// ```text
/// fee             = ceil(payment_amount * fee_bps / 10_000)
/// cashback        = cashback_eligible ? floor(fee * cashback_bps / 10_000) : 0
/// seller_receives = payment_amount - fee
/// ```
/// The fee rounds up so it is never truncated to zero when `fee_bps > 0`; the
/// cashback rounds down so it never exceeds the fee.
pub fn split_payment(
    payment_amount: u64,
    fee_bps: u16,
    cashback_bps: u16,
    cashback_eligible: bool,
) -> Result<PaymentSplit> {
    let denominator = u128::from(BPS_DENOMINATOR);
    let fee = bps_ceil(payment_amount, fee_bps)?;

    let cashback = if cashback_eligible {
        let c = u128::from(fee)
            .checked_mul(u128::from(cashback_bps))
            .ok_or(ExchangeError::MathOverflow)?
            / denominator;
        u64::try_from(c).map_err(|_| error!(ExchangeError::MathOverflow))?
    } else {
        0
    };
    require!(cashback <= fee, ExchangeError::MathOverflow);

    let seller_receives = payment_amount
        .checked_sub(fee)
        .ok_or(ExchangeError::MathOverflow)?;
    Ok(PaymentSplit {
        fee,
        cashback,
        seller_receives,
    })
}

/// `ceil(amount * bps / 10_000)`, checked, with a `u128` intermediate.
fn bps_ceil(amount: u64, bps: u16) -> Result<u64> {
    let denominator = u128::from(BPS_DENOMINATOR);
    let value = u128::from(amount)
        .checked_mul(u128::from(bps))
        .and_then(|n| n.checked_add(denominator - 1))
        .ok_or(ExchangeError::MathOverflow)?
        / denominator;
    u64::try_from(value).map_err(|_| error!(ExchangeError::MathOverflow))
}

/// Listing fee of a quote (spec §3.13):
/// `ceil(valuation * listing_fee_bps / 10_000)`. Rounds up, like the trading
/// fee, so a non-zero rate on a non-zero valuation is never free.
pub fn listing_fee(valuation: u64, listing_fee_bps: u16) -> Result<u64> {
    bps_ceil(valuation, listing_fee_bps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{MAX_FEE_BPS, MAX_LISTING_FEE_BPS};
    use proptest::prelude::*;

    #[test]
    fn listing_fee_examples() {
        // 1% of 1_000 USDT (6 decimals) = 10 USDT.
        assert_eq!(listing_fee(1_000_000_000, 100).unwrap(), 10_000_000);
        // 0.1% (the policy floor) of the same valuation.
        assert_eq!(listing_fee(1_000_000_000, 10).unwrap(), 1_000_000);
        // Rounds up: 1 bp of 1 unit is 0.0001 -> 1.
        assert_eq!(listing_fee(1, 1).unwrap(), 1);
        // 1% of 10_001 = 100.01 -> 101.
        assert_eq!(listing_fee(10_001, 100).unwrap(), 101);
        // The largest valuation at the cap does not overflow.
        assert_eq!(
            listing_fee(u64::MAX, MAX_LISTING_FEE_BPS).unwrap(),
            u64::MAX / 100 + 1
        );
    }

    fn split(amount: u64, fee_bps: u16, cashback_bps: u16) -> PaymentSplit {
        split_payment(amount, fee_bps, cashback_bps, true).unwrap()
    }

    #[test]
    fn examples() {
        // 30 bps of 1_000_000 = 3_000 exactly; 20% of it is cashback.
        assert_eq!(
            split(1_000_000, 30, 2_000),
            PaymentSplit { fee: 3_000, cashback: 600, seller_receives: 997_000 }
        );
        // Fee rounds up: 30 bps of 1 is 0.003 -> 1.
        assert_eq!(
            split(1, 30, 2_000),
            PaymentSplit { fee: 1, cashback: 0, seller_receives: 0 }
        );
        // 30 bps of 10_001 = 30.003 -> 31; cashback floor(31 * 0.2) = 6.
        assert_eq!(
            split(10_001, 30, 2_000),
            PaymentSplit { fee: 31, cashback: 6, seller_receives: 9_970 }
        );
        // Zero fee.
        assert_eq!(
            split(10_001, 0, 10_000),
            PaymentSplit { fee: 0, cashback: 0, seller_receives: 10_001 }
        );
        // Full cashback share, at the maximum fee (50 bps).
        assert_eq!(
            split(1_000_000, MAX_FEE_BPS, 10_000),
            PaymentSplit { fee: 5_000, cashback: 5_000, seller_receives: 995_000 }
        );
    }

    #[test]
    fn not_eligible_means_no_cashback() {
        let s = split_payment(1_000_000, MAX_FEE_BPS, 10_000, false).unwrap();
        assert_eq!(s.fee, 5_000);
        assert_eq!(s.cashback, 0);
    }

    #[test]
    fn max_amount_does_not_overflow() {
        let s = split(u64::MAX, MAX_FEE_BPS, 10_000);
        // ceil(0.5%): u64::MAX is not a multiple of 200, so it rounds up.
        assert_eq!(s.fee, u64::MAX / 200 + 1);
        assert_eq!(s.seller_receives + s.fee, u64::MAX);
    }

    proptest! {
        #[test]
        fn split_is_exact_and_bounded(
            amount in any::<u64>(),
            fee_bps in 0..=MAX_FEE_BPS,
            cashback_bps in 0..=BPS_DENOMINATOR,
            eligible in any::<bool>(),
        ) {
            let s = split_payment(amount, fee_bps, cashback_bps, eligible).unwrap();
            // Invariant 1.
            prop_assert_eq!(u128::from(s.seller_receives) + u128::from(s.fee), u128::from(amount));
            prop_assert!(s.cashback <= s.fee);
            // ceil: fee is the smallest value with fee * 10_000 >= amount * fee_bps.
            let exact = u128::from(amount) * u128::from(fee_bps);
            prop_assert!(u128::from(s.fee) * 10_000 >= exact);
            prop_assert!(s.fee == 0 || (u128::from(s.fee) - 1) * 10_000 < exact);
            // floor.
            if eligible {
                let c = u128::from(s.fee) * u128::from(cashback_bps);
                prop_assert!(u128::from(s.cashback) * 10_000 <= c);
                prop_assert!((u128::from(s.cashback) + 1) * 10_000 > c);
            } else {
                prop_assert_eq!(s.cashback, 0);
            }
            if fee_bps > 0 && amount > 0 {
                prop_assert!(s.fee > 0);
            }
        }

        #[test]
        fn listing_fee_is_ceil_and_bounded(
            valuation in 1..=u64::MAX,
            bps in 1..=MAX_LISTING_FEE_BPS,
        ) {
            let fee = listing_fee(valuation, bps).unwrap();
            let exact = u128::from(valuation) * u128::from(bps);
            // ceil: the smallest value with fee * 10_000 >= valuation * bps.
            prop_assert!(u128::from(fee) * 10_000 >= exact);
            prop_assert!((u128::from(fee) - 1) * 10_000 < exact);
            // Never free, never above 1% rounded up.
            prop_assert!(fee >= 1);
            prop_assert!(u128::from(fee) <= u128::from(valuation) / 100 + 1);
        }
    }
}
