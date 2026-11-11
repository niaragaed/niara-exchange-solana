//! Token-2022 extension allowlist (design spec §3.8), enforced by
//! `submit_listing`, `list_currency` and `init_fee_vault`.

mod common;

use anchor_lang::prelude::Pubkey;
use anchor_spl::token_2022::spl_token_2022::extension::ExtensionType;
use common::*;
use niara_exchange::{error::ExchangeError, AssetClass, Listing};
use solana_keypair::Keypair;
use solana_signer::Signer;

/// An env whose fee mint is a Token-2022 mint with `extensions` (mint
/// authority `issuer`, funded), built by `make`.
fn env_with_fee_mint(issuer: &Keypair, make: impl FnOnce(&mut Env, Pubkey) -> Pubkey) -> Env {
    let issuer_key = issuer.pubkey();
    let mut env = Env::initialized_with_fee_mint(|env| make(env, issuer_key));
    env.svm.airdrop(&issuer_key, 10_000_000_000).unwrap();
    env
}

/// Builds a Token-2022 mint with `extensions` and asserts that no path
/// accepts it. `log_hint` must appear in the program logs, which proves the
/// rejection came from the allowlist rather than from a malformed fixture.
fn assert_rejected(extensions: &[ExtensionType], log_hint: &str) {
    let issuer = Keypair::new();
    let mut env = env_with_fee_mint(&issuer, |env, issuer| {
        env.create_mint_2022(Some(issuer), extensions)
    });
    let authority = env.listing_authority.insecure_clone();
    let mint = env.fee_mint;
    assert_rejected_mint(&mut env, &issuer, &authority, mint, log_hint);
}

/// Every path must fail with `UnsupportedMintExtension`. `init_fee_vault`
/// runs the allowlist before it creates the fee vault, so mints the vault
/// creation itself would choke on (an unknown extension type Anchor cannot
/// size; `DefaultAccountState`, under which Token-2022 refuses
/// `InitializeAccount3`) are still rejected by the policy, with the policy's
/// error. `mint` is the configured fee mint, so `init_fee_vault` gets past
/// its own `NotFeeMint` check.
fn assert_rejected_mint(
    env: &mut Env,
    issuer: &Keypair,
    authority: &Keypair,
    mint: Pubkey,
    log_hint: &str,
) {
    let submit = env.submit_listing(issuer, mint, AssetClass::Equity, DOC_HASH);
    let currency = env.list_currency(authority, mint);
    let fee_vault = env.init_fee_vault(issuer, mint);
    for (path, res) in [
        ("submit_listing", submit),
        ("list_currency", currency),
        ("init_fee_vault", fee_vault),
    ] {
        let logs = assert_custom_err(res, ExchangeError::UnsupportedMintExtension);
        assert!(
            logs.iter().any(|l| l.contains(log_hint)),
            "{path}: expected log containing {log_hint:?}, got {logs:#?}"
        );
    }
    assert!(!env.exists(&listing_pda(&mint)), "no listing created");
    assert!(!env.exists(&fee_vault_pda(&mint)), "no fee vault created");
    assert!(!env.exists(&fee_ledger_pda(&mint)), "no fee ledger created");
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
    let issuer = Keypair::new();
    let mut env = env_with_fee_mint(&issuer, |env, issuer| {
        let mint = env.create_mint_2022(Some(issuer), &[ExtensionType::MetadataPointer]);
        env.corrupt_first_extension_type(&mint);
        mint
    });
    let authority = env.listing_authority.insecure_clone();
    let mint = env.fee_mint;
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

/// Each accepted extension set goes through every path (one mint for the
/// equity, since a mint can only be listed once; the currency is also the
/// fee mint, which gets its fee vault).
fn assert_accepted(extensions: &[ExtensionType]) {
    let issuer = Keypair::new();
    let mut env = env_with_fee_mint(&issuer, |env, _| env.create_mint_2022(None, extensions));
    let authority = env.listing_authority.insecure_clone();

    let equity = env.create_mint_2022(Some(issuer.pubkey()), extensions);
    env.submit_listing(&issuer, equity, AssetClass::Equity, DOC_HASH)
        .unwrap();
    assert_eq!(env.fetch::<Listing>(&listing_pda(&equity)).asset_mint, equity);

    let currency = env.fee_mint;
    env.list_currency(&authority, currency).unwrap();
    assert_eq!(
        env.fetch::<Listing>(&listing_pda(&currency)).asset_class,
        AssetClass::Currency
    );
    env.init_fee_vault(&issuer, currency).unwrap();
    assert_eq!(env.token_account(&fee_vault_pda(&currency)).mint, currency);
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
