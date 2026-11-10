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
    #[msg("Asset mints must not have a freeze authority")]
    FreezeAuthorityNotAllowed,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Buyer and seller must be different")]
    SelfTrade,
    #[msg("The vault authority cannot be a counterparty")]
    InvalidCounterparty,
    #[msg("Listing does not match the mint")]
    InvalidListing,
    #[msg("Listing is not approved")]
    ListingNotApproved,
    #[msg("Asset class is not traded in this mode")]
    WrongTradingMode,
    #[msg("Payment mint is not an approved Currency listing")]
    InvalidPaymentMint,
    #[msg("Token account has not delegated to the vault authority")]
    DelegateNotSet,
    #[msg("Delegated amount is lower than the settlement amount")]
    InsufficientDelegation,
    #[msg("Listing is not eligible for cashback")]
    NotCashbackEligible,
    #[msg("Amount exceeds the cashback available to withdraw")]
    InsufficientCashback,
    #[msg("Amount exceeds the protocol fees available to withdraw")]
    InsufficientProtocolFees,
    #[msg("Destination must not be the fee vault")]
    InvalidDestination,
    #[msg("Seller would receive nothing after the fee")]
    ZeroSellerProceeds,
    #[msg("Fee vault holds no excess over the unwithdrawn fees and cashback")]
    NothingToSweep,
}
