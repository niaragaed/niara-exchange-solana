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
    let fee = u128::from(payment_amount)
        .checked_mul(u128::from(fee_bps))
        .and_then(|n| n.checked_add(denominator - 1))
        .ok_or(ExchangeError::MathOverflow)?
        / denominator;
    let fee = u64::try_from(fee).map_err(|_| error!(ExchangeError::MathOverflow))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MAX_FEE_BPS;
    use proptest::prelude::*;

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
        // Full cashback share.
        assert_eq!(split(1_000_000, 100, 10_000).cashback, 10_000);
    }

    #[test]
    fn not_eligible_means_no_cashback() {
        let s = split_payment(1_000_000, 100, 10_000, false).unwrap();
        assert_eq!(s.fee, 10_000);
        assert_eq!(s.cashback, 0);
    }

    #[test]
    fn max_amount_does_not_overflow() {
        let s = split(u64::MAX, MAX_FEE_BPS, 10_000);
        assert_eq!(s.fee, u64::MAX / 100 + 1);
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
    }
}
