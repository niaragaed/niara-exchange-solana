//! Token-2022 extension allowlist (design spec §3.8), enforced by both
//! `submit_listing` and `list_currency`.

mod common;

use anchor_spl::token_2022::spl_token_2022::extension::ExtensionType;
use common::*;
use niara_exchange::{error::ExchangeError, AssetClass, Listing};
use solana_signer::Signer;

/// Builds a Token-2022 mint with `extensions` and asserts that neither listing
/// path accepts it. `log_hint` must appear in the program logs, which proves the
/// rejection came from the allowlist rather than from a malformed fixture.
fn assert_rejected(extensions: &[ExtensionType], log_hint: &str) {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let authority = env.listing_authority.insecure_clone();
    let mint = env.create_mint_2022(Some(issuer.pubkey()), extensions);
    assert_rejected_mint(&mut env, &issuer, &authority, mint, log_hint);
}

fn assert_rejected_mint(
    env: &mut Env,
    issuer: &solana_keypair::Keypair,
    authority: &solana_keypair::Keypair,
    mint: anchor_lang::prelude::Pubkey,
    log_hint: &str,
) {
    for (path, res) in [
        (
            "submit_listing",
            env.submit_listing(issuer, mint, AssetClass::Equity, DOC_HASH),
        ),
        ("list_currency", env.list_currency(authority, mint)),
    ] {
        let logs = assert_custom_err(res, ExchangeError::UnsupportedMintExtension);
        assert!(
            logs.iter().any(|l| l.contains(log_hint)),
            "{path}: expected log containing {log_hint:?}, got {logs:#?}"
        );
    }
    assert!(!env.exists(&listing_pda(&mint)), "no listing created");
}

macro_rules! rejects {
    ($name:ident, $ext:ident) => {
        #[test]
        fn $name() {
            assert_rejected(
                &[ExtensionType::$ext],
                concat!("Unsupported mint extension: ", stringify!($ext)),
            );
        }
    };
}

rejects!(rejects_permanent_delegate, PermanentDelegate);
rejects!(rejects_transfer_fee_config, TransferFeeConfig);
rejects!(rejects_transfer_hook, TransferHook);
rejects!(rejects_default_account_state, DefaultAccountState);
rejects!(rejects_pausable, Pausable);
rejects!(rejects_non_transferable, NonTransferable);
rejects!(rejects_confidential_transfer, ConfidentialTransferMint);
rejects!(rejects_mint_close_authority, MintCloseAuthority);

#[test]
fn rejects_unknown_extension() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let authority = env.listing_authority.insecure_clone();
    let mint = env.create_mint_2022(Some(issuer.pubkey()), &[ExtensionType::MetadataPointer]);
    env.corrupt_first_extension_type(&mint);
    assert_rejected_mint(&mut env, &issuer, &authority, mint, "unknown extension type");
}

#[test]
fn rejects_disallowed_extension_next_to_allowed_ones() {
    assert_rejected(
        &[
            ExtensionType::MetadataPointer,
            ExtensionType::PermanentDelegate,
            ExtensionType::TokenMetadata,
        ],
        "Unsupported mint extension: PermanentDelegate",
    );
}

/// Each accepted extension set is listed through both paths (one mint each,
/// since a mint can only be listed once).
fn assert_accepted(extensions: &[ExtensionType]) {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let authority = env.listing_authority.insecure_clone();

    let equity = env.create_mint_2022(Some(issuer.pubkey()), extensions);
    env.submit_listing(&issuer, equity, AssetClass::Equity, DOC_HASH)
        .unwrap();
    assert_eq!(env.fetch::<Listing>(&listing_pda(&equity)).asset_mint, equity);

    let currency = env.create_mint_2022(None, extensions);
    env.list_currency(&authority, currency).unwrap();
    assert_eq!(
        env.fetch::<Listing>(&listing_pda(&currency)).asset_class,
        AssetClass::Currency
    );
}

#[test]
fn accepts_token_2022_without_extensions() {
    assert_accepted(&[]);
}

#[test]
fn accepts_metadata_pointer_only() {
    assert_accepted(&[ExtensionType::MetadataPointer]);
}

#[test]
fn accepts_metadata_pointer_and_token_metadata() {
    assert_accepted(&[ExtensionType::MetadataPointer, ExtensionType::TokenMetadata]);
}

#[test]
fn accepts_token_metadata_only() {
    assert_accepted(&[ExtensionType::TokenMetadata]);
}
