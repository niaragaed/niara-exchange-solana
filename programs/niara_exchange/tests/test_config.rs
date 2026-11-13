mod common;

use anchor_lang::{error::ErrorCode as AnchorError, prelude::Pubkey};
use common::*;
use niara_exchange::{
    error::ExchangeError,
    events::{ChangeExecuted, PauseSet},
    Config, ConfigChange, PendingChange, MAX_FEE_BPS, MAX_TIMELOCK_SECS, MIN_TIMELOCK_SECS,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

// ---- initialize_config -----------------------------------------------------

#[test]
fn initialize_config_stores_params() {
    let env = Env::initialized();
    let config: Config = env.fetch(&config_pda());
    assert_eq!(config.admin, env.admin.pubkey());
    assert_eq!(config.listing_authority, env.listing_authority.pubkey());
    assert_eq!(config.settlement_operator, env.operator.pubkey());
    assert_eq!(config.treasury, env.treasury.pubkey());
    assert_eq!(config.fee_bps, FEE_BPS);
    assert_eq!(config.cashback_bps, CASHBACK_BPS);
    assert_eq!(config.timelock_secs, TIMELOCK_SECS);
    assert!(!config.paused);
}

#[test]
fn initialize_config_requires_upgrade_authority() {
    let mut env = Env::deployed();
    let attacker = env.funded_keypair();
    let params = env.default_params();
    assert_custom_err(
        env.initialize_config(&attacker, params),
        ExchangeError::NotUpgradeAuthority,
    );
}

#[test]
fn initialize_config_rejects_immutable_program() {
    let mut env = Env::deployed();
    env.set_upgrade_authority(None);
    let admin = env.admin.insecure_clone();
    let params = env.default_params();
    assert_custom_err(
        env.initialize_config(&admin, params),
        ExchangeError::NotUpgradeAuthority,
    );
}

#[test]
fn initialize_config_validates_params() {
    let mut env = Env::deployed();
    let admin = env.admin.insecure_clone();

    let mut p = env.default_params();
    p.fee_bps = MAX_FEE_BPS + 1;
    assert_custom_err(env.initialize_config(&admin, p), ExchangeError::FeeTooHigh);

    let mut p = env.default_params();
    p.cashback_bps = 10_001;
    assert_custom_err(env.initialize_config(&admin, p), ExchangeError::CashbackTooHigh);

    let mut p = env.default_params();
    p.timelock_secs = MIN_TIMELOCK_SECS - 1;
    assert_custom_err(env.initialize_config(&admin, p), ExchangeError::InvalidTimelock);

    let mut p = env.default_params();
    p.timelock_secs = MAX_TIMELOCK_SECS + 1;
    assert_custom_err(env.initialize_config(&admin, p), ExchangeError::InvalidTimelock);

    let mut p = env.default_params();
    p.treasury = Pubkey::default();
    assert_custom_err(env.initialize_config(&admin, p), ExchangeError::InvalidAddress);

    let mut p = env.default_params();
    p.fee_mint = Pubkey::default();
    assert_custom_err(env.initialize_config(&admin, p), ExchangeError::InvalidAddress);

    // Boundary values are accepted.
    let mut p = env.default_params();
    p.fee_bps = MAX_FEE_BPS;
    p.cashback_bps = 10_000;
    env.initialize_config(&admin, p).unwrap();
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_mint, env.fee_mint);
}

/// Business rule: Niara charges at most 0.5%. The cap is 50 bps on both ways
/// of setting the fee, `initialize_config` and the timelock, and nothing
/// above it is ever stored.
#[test]
fn fee_cap_is_half_a_percent() {
    assert_eq!(MAX_FEE_BPS, 50);

    let mut env = Env::deployed();
    let admin = env.admin.insecure_clone();
    for too_high in [51, 100, 10_000, u16::MAX] {
        let mut p = env.default_params();
        p.fee_bps = too_high;
        assert_custom_err(env.initialize_config(&admin, p), ExchangeError::FeeTooHigh);
        assert!(!env.exists(&config_pda()), "no config created");
    }
    let mut p = env.default_params();
    p.fee_bps = 50;
    env.initialize_config(&admin, p).unwrap();
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_bps, 50);

    for too_high in [51, 100, 10_000, u16::MAX] {
        let change = ConfigChange::FeeBps(too_high);
        assert_custom_err(env.propose(&admin, change), ExchangeError::FeeTooHigh);
        assert!(!env.exists(&pending_pda(&change)), "no pending change created");
    }
    // Down and back up to the cap through the timelock.
    for fee in [0, 50] {
        let change = ConfigChange::FeeBps(fee);
        env.propose(&admin, change).unwrap();
        env.warp_secs(TIMELOCK_SECS);
        env.execute(&admin, &change).unwrap();
        assert_eq!(env.fetch::<Config>(&config_pda()).fee_bps, fee);
    }
}

#[test]
fn initialize_config_only_once() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let params = env.default_params();
    assert!(env.initialize_config(&admin, params).is_err());
}

// ---- pause -------------------------------------------------------------------

#[test]
fn admin_can_pause_and_unpause() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();

    let meta = env.pause(&admin).unwrap();
    assert!(env.fetch::<Config>(&config_pda()).paused);
    let evs = events::<PauseSet>(&meta);
    assert_eq!(evs.len(), 1);
    assert!(evs[0].paused);

    assert_custom_err(env.pause(&admin), ExchangeError::AlreadyPaused);

    env.unpause(&admin).unwrap();
    assert!(!env.fetch::<Config>(&config_pda()).paused);
    assert_custom_err(env.unpause(&admin), ExchangeError::NotPaused);
}

#[test]
fn non_admin_cannot_pause() {
    let mut env = Env::initialized();
    let other = env.funded_keypair();
    assert_custom_err(env.pause(&other), ExchangeError::Unauthorized);
}

// ---- timelock ----------------------------------------------------------------

#[test]
fn change_executes_only_after_timelock() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let change = ConfigChange::FeeBps(50);

    let proposed_at = env.now();
    env.propose(&admin, change).unwrap();
    let pending: PendingChange = env.fetch(&pending_pda(&change));
    assert_eq!(pending.change, change);
    assert_eq!(pending.execute_after, proposed_at + TIMELOCK_SECS);

    assert_custom_err(env.execute(&admin, &change), ExchangeError::TimelockNotElapsed);

    env.warp_secs(TIMELOCK_SECS - 1);
    assert_custom_err(env.execute(&admin, &change), ExchangeError::TimelockNotElapsed);

    env.warp_secs(1);
    env.execute(&admin, &change).unwrap();
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_bps, 50);
    assert!(!env.exists(&pending_pda(&change)), "pending account closed");
}

#[test]
fn propose_validates_change() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    assert_custom_err(
        env.propose(&admin, ConfigChange::FeeBps(MAX_FEE_BPS + 1)),
        ExchangeError::FeeTooHigh,
    );
    assert_custom_err(
        env.propose(&admin, ConfigChange::CashbackBps(10_001)),
        ExchangeError::CashbackTooHigh,
    );
    assert_custom_err(
        env.propose(&admin, ConfigChange::Admin(Pubkey::default())),
        ExchangeError::InvalidAddress,
    );
    assert_custom_err(
        env.propose(&admin, ConfigChange::FeeMint(Pubkey::default())),
        ExchangeError::InvalidAddress,
    );
}

/// The fee mint has no setter outside the timelock: only the admin can
/// propose a switch, and it applies only after the delay.
#[test]
fn fee_mint_changes_only_through_the_timelock() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let other = env.funded_keypair();
    let old = env.fee_mint;
    let change = ConfigChange::FeeMint(Pubkey::new_unique());

    assert_custom_err(env.propose(&other, change), ExchangeError::Unauthorized);
    env.propose(&admin, change).unwrap();
    assert_custom_err(env.execute(&admin, &change), ExchangeError::TimelockNotElapsed);
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_mint, old);
    env.warp_secs(TIMELOCK_SECS);
    assert_custom_err(env.execute(&other, &change), ExchangeError::Unauthorized);
    env.execute(&admin, &change).unwrap();
    let ConfigChange::FeeMint(new) = change else { unreachable!() };
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_mint, new);
}

#[test]
fn only_admin_can_propose_execute_or_cancel() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let other = env.funded_keypair();
    let change = ConfigChange::Treasury(Pubkey::new_unique());

    assert_custom_err(env.propose(&other, change), ExchangeError::Unauthorized);

    env.propose(&admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    assert_custom_err(env.execute(&other, &change), ExchangeError::Unauthorized);
    assert_custom_err(env.cancel(&other, &change), ExchangeError::Unauthorized);
}

#[test]
fn one_pending_change_per_kind() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();

    env.propose(&admin, ConfigChange::FeeBps(40)).unwrap();
    // Same kind, different value: slot already taken.
    assert!(env.propose(&admin, ConfigChange::FeeBps(45)).is_err());
    // Different kind: independent slot.
    env.propose(&admin, ConfigChange::CashbackBps(1_000)).unwrap();
}

#[test]
fn cancel_frees_the_slot() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let change = ConfigChange::FeeBps(40);

    env.propose(&admin, change).unwrap();
    env.cancel(&admin, &change).unwrap();
    assert!(!env.exists(&pending_pda(&change)));
    assert!(env.execute(&admin, &change).is_err(), "nothing left to execute");

    let replacement = ConfigChange::FeeBps(45);
    env.propose(&admin, replacement).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&admin, &replacement).unwrap();
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_bps, 45);
}

// ---- two-step admin transfer (propose_change + accept_admin) ----------------

/// The admin moves only when the new admin signs `accept_admin` after the
/// timelock. The new admin holds no SOL and only signs (a member pays the
/// fee), as a Squads vault does through a vault transaction.
#[test]
fn admin_transfer_needs_the_new_admins_signature() {
    let mut env = Env::initialized();
    let old_admin = env.admin.insecure_clone();
    let new_admin = Keypair::new(); // no lamports at all
    let member = env.funded_keypair(); // pays the transaction fee
    let change = ConfigChange::Admin(new_admin.pubkey());

    env.propose(&old_admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    // execute_change cannot install an admin that never signed.
    assert_custom_err(
        env.execute(&old_admin, &change),
        ExchangeError::AdminChangeNeedsAcceptance,
    );
    assert_eq!(env.fetch::<Config>(&config_pda()).admin, old_admin.pubkey());

    let pending = pending_pda(&change);
    let rent = env.svm.get_account(&pending).unwrap().lamports;
    let old_balance = env.svm.get_balance(&old_admin.pubkey()).unwrap();
    let accounts = env.accept_admin_accounts(new_admin.pubkey(), &change);
    let meta = env.accept_admin_with(&member, &new_admin, accounts).unwrap();

    assert_eq!(env.fetch::<Config>(&config_pda()).admin, new_admin.pubkey());
    assert!(!env.exists(&pending), "pending account closed");
    // The outgoing admin gets back the rent it paid at propose_change.
    assert_eq!(env.svm.get_balance(&old_admin.pubkey()).unwrap(), old_balance + rent);
    assert_eq!(env.svm.get_balance(&new_admin.pubkey()).unwrap_or(0), 0);
    let evs = events::<ChangeExecuted>(&meta);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].change, change);

    // Only the new admin acts as admin from now on.
    assert_custom_err(env.pause(&old_admin), ExchangeError::Unauthorized);
    env.svm.airdrop(&new_admin.pubkey(), 1_000_000_000).unwrap();
    env.pause(&new_admin).unwrap();
}

#[test]
fn only_the_proposed_admin_can_accept() {
    let mut env = Env::initialized();
    let old_admin = env.admin.insecure_clone();
    let new_admin = env.funded_keypair();
    let change = ConfigChange::Admin(new_admin.pubkey());
    env.propose(&old_admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);

    let stranger = env.funded_keypair();
    for signer in [&stranger, &old_admin] {
        let accounts = env.accept_admin_accounts(signer.pubkey(), &change);
        let res = env.accept_admin_with(signer, signer, accounts);
        assert_custom_err(res, ExchangeError::NotPendingAdmin);
    }
    assert_eq!(env.fetch::<Config>(&config_pda()).admin, old_admin.pubkey());
    assert!(env.exists(&pending_pda(&change)));
    env.accept_admin(&new_admin, &change).unwrap();
}

#[test]
fn accept_admin_waits_for_the_timelock() {
    let mut env = Env::initialized();
    let old_admin = env.admin.insecure_clone();
    let new_admin = env.funded_keypair();
    let change = ConfigChange::Admin(new_admin.pubkey());
    env.propose(&old_admin, change).unwrap();

    env.warp_secs(TIMELOCK_SECS - 1);
    assert_custom_err(
        env.accept_admin(&new_admin, &change),
        ExchangeError::TimelockNotElapsed,
    );
    env.warp_secs(1);
    env.accept_admin(&new_admin, &change).unwrap();
}

/// `accept_admin` only takes the pending admin change, and only with the
/// current admin as the outgoing admin.
#[test]
fn accept_admin_accounts_are_checked() {
    let mut env = Env::initialized();
    let old_admin = env.admin.insecure_clone();
    let new_admin = env.funded_keypair();

    // No pending admin change.
    let change = ConfigChange::Admin(new_admin.pubkey());
    assert_custom_err(
        env.accept_admin(&new_admin, &change),
        AnchorError::AccountNotInitialized,
    );

    // Another kind's pending change, whatever the signer.
    let fee = ConfigChange::FeeBps(40);
    env.propose(&old_admin, fee).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    let accounts = env.accept_admin_accounts(new_admin.pubkey(), &fee);
    let res = env.accept_admin_with(&new_admin, &new_admin, accounts);
    assert_custom_err(res, ExchangeError::NotPendingAdmin);
    assert!(env.exists(&pending_pda(&fee)), "the fee change is untouched");

    // An outgoing admin that is not Config.admin (it would get the rent).
    env.propose(&old_admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    let mut accounts = env.accept_admin_accounts(new_admin.pubkey(), &change);
    accounts.admin = new_admin.pubkey();
    let res = env.accept_admin_with(&new_admin, &new_admin, accounts);
    assert_custom_err(res, ExchangeError::Unauthorized);
    assert_eq!(env.fetch::<Config>(&config_pda()).admin, old_admin.pubkey());
    env.accept_admin(&new_admin, &change).unwrap();
}

/// The current admin can cancel a transfer until the new admin accepts it.
#[test]
fn cancelled_admin_transfer_cannot_be_accepted() {
    let mut env = Env::initialized();
    let old_admin = env.admin.insecure_clone();
    let new_admin = env.funded_keypair();
    let change = ConfigChange::Admin(new_admin.pubkey());
    env.propose(&old_admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    let stranger = env.funded_keypair();
    assert_custom_err(env.cancel(&stranger, &change), ExchangeError::Unauthorized);
    env.cancel(&old_admin, &change).unwrap();
    assert_custom_err(
        env.accept_admin(&new_admin, &change),
        AnchorError::AccountNotInitialized,
    );
    assert_eq!(env.fetch::<Config>(&config_pda()).admin, old_admin.pubkey());
}

/// Admin and timelock actions are never paused (spec §3.3).
#[test]
fn admin_transfer_works_while_paused() {
    let mut env = Env::initialized();
    let old_admin = env.admin.insecure_clone();
    let new_admin = env.funded_keypair();
    env.pause(&old_admin).unwrap();
    let change = ConfigChange::Admin(new_admin.pubkey());
    env.propose(&old_admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.accept_admin(&new_admin, &change).unwrap();
    assert!(env.fetch::<Config>(&config_pda()).paused);
    // The new admin resumes.
    env.unpause(&new_admin).unwrap();
}

/// The operational roles rotate through the timelock, by the admin alone
/// (they do not sign), so a multisig admin can replace a leaked key.
#[test]
fn admin_rotates_operator_listing_authority_and_treasury() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let (operator, authority, treasury) =
        (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());
    let changes = [
        ConfigChange::SettlementOperator(operator),
        ConfigChange::ListingAuthority(authority),
        ConfigChange::Treasury(treasury),
    ];
    let stranger = env.funded_keypair();
    for c in changes {
        assert_custom_err(env.propose(&stranger, c), ExchangeError::Unauthorized);
        env.propose(&admin, c).unwrap();
    }
    env.warp_secs(TIMELOCK_SECS);
    for c in changes {
        env.execute(&admin, &c).unwrap();
    }
    let config: Config = env.fetch(&config_pda());
    assert_eq!(
        (config.settlement_operator, config.listing_authority, config.treasury),
        (operator, authority, treasury)
    );
}

#[test]
fn every_change_kind_applies() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let operator = Pubkey::new_unique();
    let authority = Pubkey::new_unique();
    let treasury = Pubkey::new_unique();
    let fee_mint = Pubkey::new_unique();
    let changes = [
        ConfigChange::FeeBps(MAX_FEE_BPS),
        ConfigChange::CashbackBps(5_000),
        ConfigChange::SettlementOperator(operator),
        ConfigChange::ListingAuthority(authority),
        ConfigChange::Treasury(treasury),
        ConfigChange::FeeMint(fee_mint),
    ];
    for c in changes {
        env.propose(&admin, c).unwrap();
    }
    env.warp_secs(TIMELOCK_SECS);
    for c in changes {
        env.execute(&admin, &c).unwrap();
    }
    let config: Config = env.fetch(&config_pda());
    assert_eq!(config.fee_bps, MAX_FEE_BPS);
    assert_eq!(config.cashback_bps, 5_000);
    assert_eq!(config.settlement_operator, operator);
    assert_eq!(config.listing_authority, authority);
    assert_eq!(config.treasury, treasury);
    assert_eq!(config.fee_mint, fee_mint);
}
