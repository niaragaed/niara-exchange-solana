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
    #[msg("Asset mint must have zero supply when the listing is submitted")]
    NonZeroSupply,
    #[msg("Issuance can only be requested while the listing is Pending or Approved")]
    ListingNotIssuable,
    #[msg("Issuance batch was already executed or rejected")]
    BatchNotRequested,
    #[msg("Destination is not owned by the batch recipient")]
    WrongRecipient,
    #[msg("reference must not be empty")]
    EmptyReference,
    #[msg("Currency listings are not issued through Niara")]
    CurrencyNotIssued,
    #[msg("Asset and payment mints must be different")]
    SameMint,
    #[msg("expires_at must be in the future and at most 30 days ahead")]
    InvalidExpiry,
    #[msg("Deal is not open")]
    DealNotOpen,
    #[msg("Deal has expired")]
    DealExpired,
    #[msg("Deal has not expired yet")]
    DealNotExpired,
    #[msg("Account does not match the deal")]
    DealAccountMismatch,
    #[msg("Token account has the wrong mint or token program")]
    InvalidTokenAccount,
    #[msg("Cashback ledger is required for a cashback-eligible listing")]
    MissingCashbackLedger,
    #[msg("Cashback withdrawals are frozen for this listing")]
    CashbackFrozen,
    #[msg("Cashback is already frozen for this listing")]
    CashbackAlreadyFrozen,
    #[msg("Cashback is not frozen for this listing")]
    CashbackNotFrozen,
    #[msg("The trade needs a leg in the fee mint (USDT)")]
    NotFeeMint,
    #[msg("match_id must not be all zeros")]
    ZeroMatchId,
    #[msg("deal_id must not be all zeros")]
    ZeroDealId,
    #[msg("An admin change takes effect only when the new admin signs accept_admin")]
    AdminChangeNeedsAcceptance,
    #[msg("Signer is not the admin proposed in the pending admin change")]
    NotPendingAdmin,
}
