//! Proves the premise of the listing-time extension allowlist (spec §3.8):
//! Token-2022 cannot add a disallowed extension to a mint once it is
//! initialized, so a mint that passed the allowlist at listing time keeps
//! passing it for the life of the listing.
//!
//! Unlike `test_mint_extensions.rs`, nothing here writes account data by hand:
//! every mint is created and modified through the real Token-2022 program that
//! LiteSVM bundles. Each disallowed extension also has a control case showing
//! the same instruction succeeds when sent *before* `InitializeMint2`, so the
//! failures below come from the mint being initialized, not from a malformed
//! instruction.

mod common;

use anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction};
use anchor_spl::{
    compat::spl_token_metadata_interface::instruction as metadata_ix,
    token_2022::spl_token_2022::{
        self,
        extension::{
            confidential_transfer, default_account_state, group_pointer, interest_bearing_mint,
            metadata_pointer, pausable, transfer_fee, transfer_hook, BaseStateWithExtensions,
            ExtensionType, StateWithExtensions,
        },
        instruction as t22,
        state::{AccountState, Mint},
    },
};
use common::*;
use niara_exchange::AssetClass;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// `TokenError::AlreadyInUse`: returned by every extension initializer when the
/// mint is already initialized.
const TOKEN_ERROR_ALREADY_IN_USE: u32 = 6;

/// A disallowed mint extension and a builder for its initialize instruction.
struct Case {
    extension: ExtensionType,
    build: fn(mint: &Pubkey, authority: &Pubkey) -> Instruction,
}

const ID: Pubkey = spl_token_2022::ID;

/// Every mint extension the allowlist rejects that has an initializer: the
/// eight from the spec's table plus two more that are not allowlisted.
fn disallowed_cases() -> Vec<Case> {
    vec![
        Case {
            extension: ExtensionType::PermanentDelegate,
            build: |m, a| t22::initialize_permanent_delegate(&ID, m, a).unwrap(),
        },
        Case {
            extension: ExtensionType::TransferFeeConfig,
            build: |m, a| {
                transfer_fee::instruction::initialize_transfer_fee_config(
                    &ID,
                    m,
                    Some(a),
                    Some(a),
                    100,
                    u64::MAX,
                )
                .unwrap()
            },
        },
        Case {
            extension: ExtensionType::TransferHook,
            build: |m, a| {
                transfer_hook::instruction::initialize(&ID, m, Some(*a), Some(Pubkey::new_unique()))
                    .unwrap()
            },
        },
        Case {
            extension: ExtensionType::DefaultAccountState,
            build: |m, _| {
                default_account_state::instruction::initialize_default_account_state(
                    &ID,
                    m,
                    &AccountState::Frozen,
                )
                .unwrap()
            },
        },
        Case {
            extension: ExtensionType::Pausable,
            build: |m, a| pausable::instruction::initialize(&ID, m, a).unwrap(),
        },
        Case {
            extension: ExtensionType::NonTransferable,
            build: |m, _| t22::initialize_non_transferable_mint(&ID, m).unwrap(),
        },
        Case {
            extension: ExtensionType::ConfidentialTransferMint,
            build: |m, a| {
                confidential_transfer::instruction::initialize_mint(&ID, m, Some(*a), true, None)
                    .unwrap()
            },
        },
        Case {
            extension: ExtensionType::MintCloseAuthority,
            build: |m, a| t22::initialize_mint_close_authority(&ID, m, Some(a)).unwrap(),
        },
        Case {
            extension: ExtensionType::InterestBearingConfig,
            build: |m, a| {
                interest_bearing_mint::instruction::initialize(&ID, m, Some(*a), 500).unwrap()
            },
        },
        Case {
            extension: ExtensionType::GroupPointer,
            build: |m, a| group_pointer::instruction::initialize(&ID, m, Some(*a), Some(*m)).unwrap(),
        },
    ]
}

/// What `SystemProgram::CreateAccount` would produce: a zeroed, rent-exempt
/// account owned by Token-2022, sized for `extensions`.
fn allocate_mint(env: &mut Env, extensions: &[ExtensionType]) -> Pubkey {
    let len = ExtensionType::try_calculate_account_len::<Mint>(extensions).unwrap();
    let mint = Pubkey::new_unique();
    let lamports = env.svm.minimum_balance_for_rent_exemption(len);
    env.svm
        .set_account(
            mint,
            Account {
                lamports,
                data: vec![0; len],
                owner: ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    mint
}

fn initialize_mint(mint: &Pubkey, authority: &Pubkey) -> Instruction {
    // A freeze authority is required by `DefaultAccountState(Frozen)` in the
    // control case; set it everywhere so the cases differ only in the extension.
    t22::initialize_mint2(&ID, mint, authority, Some(authority), 6).unwrap()
}

fn extensions_of(env: &Env, mint: &Pubkey) -> Vec<ExtensionType> {
    let account = env.svm.get_account(mint).unwrap();
    assert_eq!(account.owner, ID);
    StateWithExtensions::<Mint>::unpack(&account.data)
        .expect("initialized Token-2022 mint")
        .get_extension_types()
        .unwrap()
}

/// Creates an initialized Token-2022 mint with no extensions.
fn plain_mint(env: &mut Env, authority: &Keypair) -> Pubkey {
    let mint = allocate_mint(env, &[]);
    env.send(&[initialize_mint(&mint, &authority.pubkey())], &[authority])
        .unwrap();
    assert!(extensions_of(env, &mint).is_empty());
    mint
}

/// Asserts no disallowed extension can be initialized on `mint`, that its
/// extension set is unchanged afterwards, and that the program still lists it.
fn assert_locked(env: &mut Env, mint: Pubkey, issuer: &Keypair) {
    let before = extensions_of(env, &mint);
    for case in disallowed_cases() {
        let ix = (case.build)(&mint, &issuer.pubkey());
        let res = env.send(&[ix], &[issuer]);
        let err = res.expect_err(&format!(
            "{:?} was initialized on an already-initialized mint",
            case.extension
        ));
        assert!(
            format!("{:?}", err.err).contains(&format!("Custom({TOKEN_ERROR_ALREADY_IN_USE})")),
            "{:?}: expected TokenError::AlreadyInUse, got {:?}\nlogs: {:#?}",
            case.extension,
            err.err,
            err.meta.logs
        );
    }
    assert_eq!(extensions_of(env, &mint), before, "extension set changed");

    env.submit_listing(issuer, mint, AssetClass::Equity, DOC_HASH)
        .expect("mint still passes the allowlist");
}

#[test]
fn control_disallowed_extensions_initialize_before_initialize_mint() {
    let mut env = Env::initialized();
    let authority = env.funded_keypair();
    for case in disallowed_cases() {
        let mint = allocate_mint(&mut env, &[case.extension]);
        let ixs = [
            (case.build)(&mint, &authority.pubkey()),
            initialize_mint(&mint, &authority.pubkey()),
        ];
        env.send(&ixs, &[&authority])
            .unwrap_or_else(|e| panic!("{:?} control failed: {e:?}", case.extension));
        assert_eq!(extensions_of(&env, &mint), vec![case.extension]);
    }
}

#[test]
fn disallowed_extension_cannot_be_added_to_initialized_mint() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let mint = plain_mint(&mut env, &issuer);
    assert_locked(&mut env, mint, &issuer);
}

/// Spare room does not help: `InitializeMint2` refuses an account whose length
/// does not match its initialized extensions, so a mint cannot be created with
/// a slot reserved for an extension to be filled in later.
#[test]
fn mint_with_room_for_disallowed_extension_cannot_be_initialized_without_it() {
    let mut env = Env::initialized();
    let authority = env.funded_keypair();
    for case in disallowed_cases() {
        let mint = allocate_mint(&mut env, &[case.extension]);
        let res = env.send(&[initialize_mint(&mint, &authority.pubkey())], &[&authority]);
        assert!(res.is_err(), "{:?}: InitializeMint2 accepted spare room", case.extension);
        let data = env.svm.get_account(&mint).unwrap().data;
        assert!(data.iter().all(|b| *b == 0), "account must stay uninitialized");
    }
}

/// `Reallocate` only applies to token accounts; it cannot grow a mint to make
/// room for a new extension.
#[test]
fn reallocate_cannot_grow_a_mint() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let mint = plain_mint(&mut env, &issuer);
    let ix = t22::reallocate(
        &ID,
        &mint,
        &issuer.pubkey(),
        &issuer.pubkey(),
        &[],
        &[ExtensionType::PermanentDelegate],
    )
    .unwrap();
    assert!(env.send(&[ix], &[&issuer]).is_err());
    assert!(extensions_of(&env, &mint).is_empty());
}

/// The one way a mint does grow after initialization is `TokenMetadata`
/// (variable-length, written through the metadata interface), which is
/// allowlisted. After it, disallowed extensions are still locked out.
#[test]
fn metadata_mint_stays_locked_after_post_init_metadata_write() {
    let mut env = Env::initialized();
    let issuer = env.funded_keypair();
    let authority = issuer.pubkey();

    let mint = allocate_mint(&mut env, &[ExtensionType::MetadataPointer]);
    env.send(
        &[
            metadata_pointer::instruction::initialize(&ID, &mint, Some(authority), Some(mint))
                .unwrap(),
            initialize_mint(&mint, &authority),
        ],
        &[&issuer],
    )
    .unwrap();

    // Token-2022 reallocs the mint for the metadata but does not pay for it.
    env.svm.airdrop(&mint, 10_000_000).unwrap();
    env.send(
        &[metadata_ix::initialize(
            &ID,
            &mint,
            &authority,
            &mint,
            &authority,
            "Niara Test Asset".into(),
            "NTST".into(),
            "https://example.com/ntst.json".into(),
        )],
        &[&issuer],
    )
    .unwrap();
    assert_eq!(
        extensions_of(&env, &mint),
        vec![ExtensionType::MetadataPointer, ExtensionType::TokenMetadata]
    );

    assert_locked(&mut env, mint, &issuer);
}
