//! Mint extension allowlist (design spec §3.8).
//!
//! Accepted: SPL Token mints, and Token-2022 mints whose only extensions are
//! `MetadataPointer` and/or `TokenMetadata`. Everything else is rejected,
//! including extension types this program does not know about:
//! - `PermanentDelegate` can drain escrow vaults;
//! - `TransferFeeConfig` breaks `payment_amount == seller_receives + fee`;
//! - `TransferHook` runs third-party code inside settlement;
//! - `DefaultAccountState`, `Pausable`, `NonTransferable` can strand a settlement;
//! - `ConfidentialTransferMint` hides balances the program must check;
//! - `MintCloseAuthority` lets the mint disappear under a live listing.

use anchor_lang::prelude::*;
use anchor_spl::token_2022::spl_token_2022::{
    extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions},
    state::Mint as MintState,
};

use crate::error::ExchangeError;

pub const ALLOWED_MINT_EXTENSIONS: [ExtensionType; 2] =
    [ExtensionType::MetadataPointer, ExtensionType::TokenMetadata];

pub fn is_allowed_extension(extension: ExtensionType) -> bool {
    ALLOWED_MINT_EXTENSIONS.contains(&extension)
}

/// Fails with `UnsupportedMintExtension` unless every extension on `mint` is
/// allowlisted. The caller must already have checked that `mint` is a mint owned
/// by SPL Token or Token-2022 (e.g. via `InterfaceAccount<Mint>`).
pub fn require_allowed_mint_extensions(mint: &AccountInfo) -> Result<()> {
    if *mint.owner == anchor_spl::token::ID {
        // Classic SPL Token mints cannot carry extensions.
        return Ok(());
    }
    require_keys_eq!(
        *mint.owner,
        anchor_spl::token_2022::ID,
        ExchangeError::UnsupportedMintExtension
    );

    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<MintState>::unpack(&data)
        .map_err(|_| error!(ExchangeError::UnsupportedMintExtension))?;
    // Unknown extension types fail to parse here and are rejected as well.
    let extensions = state.get_extension_types().map_err(|_| {
        msg!("Mint has an unknown extension type");
        error!(ExchangeError::UnsupportedMintExtension)
    })?;
    for extension in extensions {
        if !is_allowed_extension(extension) {
            msg!("Unsupported mint extension: {:?}", extension);
            return err!(ExchangeError::UnsupportedMintExtension);
        }
    }
    Ok(())
}
