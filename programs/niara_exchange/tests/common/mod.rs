//! Shared LiteSVM harness. Requires `anchor build` first (loads the `.so`).
#![allow(dead_code)]

use std::str::FromStr;

use anchor_lang::{
    prelude::{Clock, Pubkey},
    solana_program::{
        instruction::Instruction, program_option::COption, program_pack::Pack, system_program,
    },
    AccountDeserialize, AnchorDeserialize, Discriminator, InstructionData, ToAccountMetas,
};
use anchor_spl::{
    compat::spl_token_metadata_interface::state::TokenMetadata,
    token::spl_token::state::Mint,
    token_2022::spl_token_2022::{
        extension::{
            confidential_transfer::ConfidentialTransferMint,
            default_account_state::DefaultAccountState, metadata_pointer::MetadataPointer,
            mint_close_authority::MintCloseAuthority, non_transferable::NonTransferable,
            pausable::PausableConfig, permanent_delegate::PermanentDelegate,
            transfer_fee::TransferFeeConfig, transfer_hook::TransferHook,
            BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
        },
        state::Mint as Mint2022,
    },
};
use litesvm::{
    types::{TransactionMetadata, TransactionResult},
    LiteSVM,
};
use niara_exchange::{
    accounts, instruction, AssetClass, ConfigChange, InitializeConfigParams, CONFIG_SEED,
    LISTING_SEED, PENDING_SEED,
};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

pub const FEE_BPS: u16 = 30;
pub const CASHBACK_BPS: u16 = 2_000;
pub const TIMELOCK_SECS: i64 = 3_600;
pub const DOC_HASH: [u8; 32] = [7u8; 32];

const BPF_LOADER_UPGRADEABLE: &str = "BPFLoaderUpgradeab1e11111111111111111111111";

pub fn program_id() -> Pubkey {
    niara_exchange::id()
}

pub fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[CONFIG_SEED], &program_id()).0
}

pub fn pending_pda(change: &ConfigChange) -> Pubkey {
    Pubkey::find_program_address(&[PENDING_SEED, &[change.kind()]], &program_id()).0
}

pub fn listing_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[LISTING_SEED, mint.as_ref()], &program_id()).0
}

pub fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &program_id()).0
}

pub fn program_data_address() -> Pubkey {
    let loader = Pubkey::from_str(BPF_LOADER_UPGRADEABLE).unwrap();
    Pubkey::find_program_address(&[program_id().as_ref()], &loader).0
}

pub fn ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction::new_with_bytes(program_id(), &data.data(), accounts.to_account_metas(None))
}

pub struct Env {
    pub svm: LiteSVM,
    pub admin: Keypair,
    pub listing_authority: Keypair,
    pub operator: Pubkey,
    pub treasury: Pubkey,
}

impl Env {
    /// Program deployed, `admin` set as upgrade authority, config NOT initialized.
    pub fn deployed() -> Self {
        let mut svm = LiteSVM::new();
        let bytes = include_bytes!(concat!(
            env!("CARGO_TARGET_TMPDIR"),
            "/../deploy/niara_exchange.so"
        ));
        svm.add_program(program_id(), bytes).unwrap();

        let admin = Keypair::new();
        let listing_authority = Keypair::new();
        svm.airdrop(&admin.pubkey(), 10_000_000_000).unwrap();
        svm.airdrop(&listing_authority.pubkey(), 10_000_000_000).unwrap();

        let mut env = Env {
            svm,
            admin,
            listing_authority,
            operator: Pubkey::new_unique(),
            treasury: Pubkey::new_unique(),
        };
        let admin_key = env.admin.pubkey();
        env.set_upgrade_authority(Some(admin_key));
        env
    }

    /// Program deployed and config initialized with the default params.
    pub fn initialized() -> Self {
        let mut env = Self::deployed();
        let params = env.default_params();
        let admin = env.admin.insecure_clone();
        env.initialize_config(&admin, params).unwrap();
        env
    }

    /// LiteSVM deploys with no upgrade authority; patch the ProgramData header.
    /// Layout (bincode): u32 variant | u64 slot | u8 option tag | [u8; 32] authority.
    pub fn set_upgrade_authority(&mut self, authority: Option<Pubkey>) {
        let address = program_data_address();
        let mut account = self.svm.get_account(&address).unwrap();
        match authority {
            Some(key) => {
                account.data[12] = 1;
                account.data[13..45].copy_from_slice(key.as_ref());
            }
            None => {
                account.data[12] = 0;
                account.data[13..45].fill(0);
            }
        }
        self.svm.set_account(address, account).unwrap();
    }

    pub fn default_params(&self) -> InitializeConfigParams {
        InitializeConfigParams {
            listing_authority: self.listing_authority.pubkey(),
            settlement_operator: self.operator,
            treasury: self.treasury,
            fee_bps: FEE_BPS,
            cashback_bps: CASHBACK_BPS,
            timelock_secs: TIMELOCK_SECS,
        }
    }

    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> TransactionResult {
        let payer = signers[0].pubkey();
        let blockhash = self.svm.latest_blockhash();
        let msg = Message::new_with_blockhash(ixs, Some(&payer), &blockhash);
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), signers).unwrap();
        let res = self.svm.send_transaction(tx);
        // Fresh blockhash so identical follow-up transactions are not deduplicated.
        self.svm.expire_blockhash();
        res
    }

    pub fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    pub fn warp_secs(&mut self, secs: i64) {
        let mut clock = self.svm.get_sysvar::<Clock>();
        clock.unix_timestamp += secs;
        self.svm.set_sysvar(&clock);
    }

    pub fn funded_keypair(&mut self) -> Keypair {
        let kp = Keypair::new();
        self.svm.airdrop(&kp.pubkey(), 10_000_000_000).unwrap();
        kp
    }

    pub fn fetch<T: AccountDeserialize>(&self, address: &Pubkey) -> T {
        let account = self.svm.get_account(address).expect("account missing");
        T::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    pub fn exists(&self, address: &Pubkey) -> bool {
        self.svm
            .get_account(address)
            .is_some_and(|a| a.lamports > 0)
    }

    /// Writes an initialized, extension-free mint owned by `token_program`.
    pub fn create_mint(&mut self, token_program: Pubkey, authority: Option<Pubkey>) -> Pubkey {
        let mint = Pubkey::new_unique();
        let state = Mint {
            mint_authority: authority.map_or(COption::None, COption::Some),
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        let mut data = vec![0u8; Mint::LEN];
        Mint::pack(state, &mut data).unwrap();
        let lamports = self.svm.minimum_balance_for_rent_exemption(Mint::LEN);
        self.svm
            .set_account(
                mint,
                Account {
                    lamports,
                    data,
                    owner: token_program,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        mint
    }

    /// Writes an initialized Token-2022 mint carrying `extensions`, using the
    /// real Token-2022 account layout. `TokenMetadata` is variable-length and is
    /// written after the fixed-size extensions. Extension contents are default
    /// values: the listing policy only looks at extension types.
    pub fn create_mint_2022(
        &mut self,
        authority: Option<Pubkey>,
        extensions: &[ExtensionType],
    ) -> Pubkey {
        let mint = Pubkey::new_unique();
        let fixed: Vec<ExtensionType> = extensions
            .iter()
            .copied()
            .filter(|e| *e != ExtensionType::TokenMetadata)
            .collect();
        let metadata = extensions
            .contains(&ExtensionType::TokenMetadata)
            .then(|| TokenMetadata {
                mint,
                name: "Niara Test Asset".into(),
                symbol: "NTST".into(),
                uri: "https://example.com/ntst.json".into(),
                ..Default::default()
            });

        let mut len = if fixed.is_empty() && metadata.is_some() {
            BASE_ACCOUNT_AND_TYPE_LENGTH
        } else {
            ExtensionType::try_calculate_account_len::<Mint2022>(&fixed).unwrap()
        };
        if let Some(m) = &metadata {
            len += m.tlv_size_of().unwrap();
        }

        let mut data = vec![0u8; len];
        let mut state = StateWithExtensionsMut::<Mint2022>::unpack_uninitialized(&mut data).unwrap();
        for ext in &fixed {
            init_fixed_extension(&mut state, *ext);
        }
        if let Some(m) = &metadata {
            state.init_variable_len_extension(m, false).unwrap();
        }
        state.base = Mint2022 {
            mint_authority: authority.map_or(COption::None, COption::Some),
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        state.pack_base();
        if !extensions.is_empty() {
            state.init_account_type().unwrap();
        }

        let lamports = self.svm.minimum_balance_for_rent_exemption(len);
        self.svm
            .set_account(
                mint,
                Account {
                    lamports,
                    data,
                    owner: anchor_spl::token_2022::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        mint
    }

    /// Overwrites the type tag of the first TLV entry of a Token-2022 mint with a
    /// value no known extension uses.
    pub fn corrupt_first_extension_type(&mut self, mint: &Pubkey) {
        let mut account = self.svm.get_account(mint).unwrap();
        let at = BASE_ACCOUNT_AND_TYPE_LENGTH;
        account.data[at..at + 2].copy_from_slice(&UNKNOWN_EXTENSION_TYPE.to_le_bytes());
        self.svm.set_account(*mint, account).unwrap();
    }

    // ---- instruction wrappers ------------------------------------------

    pub fn initialize_config(
        &mut self,
        admin: &Keypair,
        params: InitializeConfigParams,
    ) -> TransactionResult {
        let ix = ix(
            accounts::InitializeConfig {
                admin: admin.pubkey(),
                config: config_pda(),
                program: program_id(),
                program_data: program_data_address(),
                system_program: system_program::ID,
            },
            instruction::InitializeConfig { params },
        );
        self.send(&[ix], &[admin])
    }

    pub fn pause(&mut self, admin: &Keypair) -> TransactionResult {
        let ix = ix(self.set_pause_accounts(admin), instruction::Pause {});
        self.send(&[ix], &[admin])
    }

    pub fn unpause(&mut self, admin: &Keypair) -> TransactionResult {
        let ix = ix(self.set_pause_accounts(admin), instruction::Unpause {});
        self.send(&[ix], &[admin])
    }

    fn set_pause_accounts(&self, admin: &Keypair) -> accounts::SetPause {
        accounts::SetPause {
            admin: admin.pubkey(),
            config: config_pda(),
            event_authority: event_authority(),
            program: program_id(),
        }
    }

    pub fn propose(&mut self, admin: &Keypair, change: ConfigChange) -> TransactionResult {
        let ix = ix(
            accounts::ProposeChange {
                admin: admin.pubkey(),
                config: config_pda(),
                pending_change: pending_pda(&change),
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::ProposeChange { change },
        );
        self.send(&[ix], &[admin])
    }

    fn resolve_accounts(&self, admin: &Keypair, change: &ConfigChange) -> accounts::ResolveChange {
        accounts::ResolveChange {
            admin: admin.pubkey(),
            config: config_pda(),
            pending_change: pending_pda(change),
            event_authority: event_authority(),
            program: program_id(),
        }
    }

    pub fn execute(&mut self, admin: &Keypair, change: &ConfigChange) -> TransactionResult {
        let ix = ix(self.resolve_accounts(admin, change), instruction::ExecuteChange {});
        self.send(&[ix], &[admin])
    }

    pub fn cancel(&mut self, admin: &Keypair, change: &ConfigChange) -> TransactionResult {
        let ix = ix(self.resolve_accounts(admin, change), instruction::CancelChange {});
        self.send(&[ix], &[admin])
    }

    pub fn submit_listing(
        &mut self,
        issuer: &Keypair,
        mint: Pubkey,
        asset_class: AssetClass,
        doc_hash: [u8; 32],
    ) -> TransactionResult {
        let ix = ix(
            accounts::SubmitListing {
                issuer: issuer.pubkey(),
                asset_mint: mint,
                listing: listing_pda(&mint),
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::SubmitListing {
                asset_class,
                doc_hash,
            },
        );
        self.send(&[ix], &[issuer])
    }

    pub fn list_currency(&mut self, authority: &Keypair, mint: Pubkey) -> TransactionResult {
        let ix = ix(
            accounts::ListCurrency {
                listing_authority: authority.pubkey(),
                config: config_pda(),
                asset_mint: mint,
                listing: listing_pda(&mint),
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::ListCurrency {},
        );
        self.send(&[ix], &[authority])
    }

    fn status_accounts(&self, authority: &Keypair, mint: &Pubkey) -> accounts::UpdateListingStatus {
        accounts::UpdateListingStatus {
            listing_authority: authority.pubkey(),
            config: config_pda(),
            listing: listing_pda(mint),
            event_authority: event_authority(),
            program: program_id(),
        }
    }

    pub fn approve(&mut self, authority: &Keypair, mint: Pubkey) -> TransactionResult {
        let ix = ix(self.status_accounts(authority, &mint), instruction::ApproveListing {});
        self.send(&[ix], &[authority])
    }

    pub fn suspend(&mut self, authority: &Keypair, mint: Pubkey) -> TransactionResult {
        let ix = ix(self.status_accounts(authority, &mint), instruction::SuspendListing {});
        self.send(&[ix], &[authority])
    }

    pub fn delist(&mut self, authority: &Keypair, mint: Pubkey) -> TransactionResult {
        let ix = ix(self.status_accounts(authority, &mint), instruction::Delist {});
        self.send(&[ix], &[authority])
    }
}

/// Asserts the transaction failed with the given custom program error code.
/// Returns the program logs for further checks.
pub fn assert_custom_err(res: TransactionResult, code: impl Into<u32>) -> Vec<String> {
    let code = code.into();
    let err = res.expect_err("transaction should have failed");
    let dbg = format!("{:?}", err.err);
    assert!(
        dbg.contains(&format!("Custom({code})")),
        "expected Custom({code}), got {dbg}\nlogs: {:#?}",
        err.meta.logs
    );
    err.meta.logs
}

pub const UNKNOWN_EXTENSION_TYPE: u16 = 0xFFF0;

/// Token-2022 layout: base state padded to the token-account length, then a
/// one-byte account type, then the TLV extension entries.
const BASE_ACCOUNT_AND_TYPE_LENGTH: usize = anchor_spl::token::spl_token::state::Account::LEN + 1;

fn init_fixed_extension(state: &mut StateWithExtensionsMut<Mint2022>, ext: ExtensionType) {
    match ext {
        ExtensionType::PermanentDelegate => {
            state.init_extension::<PermanentDelegate>(true).unwrap();
        }
        ExtensionType::TransferFeeConfig => {
            state.init_extension::<TransferFeeConfig>(true).unwrap();
        }
        ExtensionType::TransferHook => {
            state.init_extension::<TransferHook>(true).unwrap();
        }
        ExtensionType::DefaultAccountState => {
            state.init_extension::<DefaultAccountState>(true).unwrap();
        }
        ExtensionType::Pausable => {
            state.init_extension::<PausableConfig>(true).unwrap();
        }
        ExtensionType::NonTransferable => {
            state.init_extension::<NonTransferable>(true).unwrap();
        }
        ExtensionType::ConfidentialTransferMint => {
            state.init_extension::<ConfidentialTransferMint>(true).unwrap();
        }
        ExtensionType::MintCloseAuthority => {
            state.init_extension::<MintCloseAuthority>(true).unwrap();
        }
        ExtensionType::MetadataPointer => {
            state.init_extension::<MetadataPointer>(true).unwrap();
        }
        other => panic!("fixture does not support {other:?}"),
    }
}

/// Decodes `emit_cpi!` events of type `E` from a transaction's inner instructions.
pub fn events<E: Discriminator + AnchorDeserialize>(meta: &TransactionMetadata) -> Vec<E> {
    meta.inner_instructions
        .iter()
        .flatten()
        .filter_map(|inner| {
            let data = inner.instruction.data.as_slice();
            let data = data.strip_prefix(anchor_lang::event::EVENT_IX_TAG_LE)?;
            let data = data.strip_prefix(E::DISCRIMINATOR)?;
            E::try_from_slice(data).ok()
        })
        .collect()
}
