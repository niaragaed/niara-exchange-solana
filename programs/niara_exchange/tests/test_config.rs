mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use niara_exchange::{
    error::ExchangeError, events::PauseSet, Config, ConfigChange, PendingChange,
    MAX_FEE_BPS, MAX_TIMELOCK_SECS, MIN_TIMELOCK_SECS,
};
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

    // Boundary values are accepted.
    let mut p = env.default_params();
    p.fee_bps = MAX_FEE_BPS;
    p.cashback_bps = 10_000;
    env.initialize_config(&admin, p).unwrap();
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
    assert!(env.propose(&admin, ConfigChange::FeeBps(60)).is_err());
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

    let replacement = ConfigChange::FeeBps(60);
    env.propose(&admin, replacement).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&admin, &replacement).unwrap();
    assert_eq!(env.fetch::<Config>(&config_pda()).fee_bps, 60);
}

#[test]
fn admin_handover_via_timelock() {
    let mut env = Env::initialized();
    let old_admin = env.admin.insecure_clone();
    let new_admin = env.funded_keypair();
    let change = ConfigChange::Admin(new_admin.pubkey());

    env.propose(&old_admin, change).unwrap();
    env.warp_secs(TIMELOCK_SECS);
    env.execute(&old_admin, &change).unwrap();

    assert_eq!(env.fetch::<Config>(&config_pda()).admin, new_admin.pubkey());
    assert_custom_err(env.pause(&old_admin), ExchangeError::Unauthorized);
    env.pause(&new_admin).unwrap();
}

#[test]
fn every_change_kind_applies() {
    let mut env = Env::initialized();
    let admin = env.admin.insecure_clone();
    let operator = Pubkey::new_unique();
    let authority = Pubkey::new_unique();
    let treasury = Pubkey::new_unique();
    let changes = [
        ConfigChange::FeeBps(MAX_FEE_BPS),
        ConfigChange::CashbackBps(5_000),
        ConfigChange::SettlementOperator(operator),
        ConfigChange::ListingAuthority(authority),
        ConfigChange::Treasury(treasury),
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
}
