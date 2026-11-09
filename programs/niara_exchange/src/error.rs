use anchor_lang::prelude::*;

#[error_code]
pub enum ExchangeError {
    #[msg("Signer is not authorized for this action")]
    Unauthorized,
    #[msg("Program data account does not belong to this program")]
    InvalidProgramData,
    #[msg("Signer is not the program upgrade authority")]
    NotUpgradeAuthority,
    #[msg("fee_bps exceeds MAX_FEE_BPS")]
    FeeTooHigh,
    #[msg("cashback_bps exceeds 10_000")]
    CashbackTooHigh,
    #[msg("timelock_secs is outside the allowed range")]
    InvalidTimelock,
    #[msg("Address must not be the default (all-zero) pubkey")]
    InvalidAddress,
    #[msg("Protocol is already paused")]
    AlreadyPaused,
    #[msg("Protocol is not paused")]
    NotPaused,
    #[msg("Protocol is paused")]
    Paused,
    #[msg("Timelock has not elapsed yet")]
    TimelockNotElapsed,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Signer is not the mint authority of the asset")]
    NotMintAuthority,
    #[msg("Currency listings are created by the listing authority via list_currency")]
    CurrencyNotSubmittable,
    #[msg("doc_hash must not be empty")]
    EmptyDocHash,
    #[msg("Listing status transition is not allowed")]
    InvalidStatusTransition,
    #[msg("Mint has a Token-2022 extension outside the allowlist (MetadataPointer, TokenMetadata)")]
    UnsupportedMintExtension,
}
