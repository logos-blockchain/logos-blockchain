use super::{
    GenesisTime, Key, OffsetDateTime, StepError, WalletAccount,
    genesis::{resolve_step_genesis_time, validate_genesis_time_change},
    key_id_for_preload_backend,
    lifecycle::node_wallet_key_overrides,
};

#[test]
fn node_wallet_key_overrides_put_the_account_in_the_node_kms_and_wallet() {
    let account = WalletAccount::deterministic(1, 0, true).expect("account");
    let key: Key = account.secret_key.clone().into();
    let key_id = key_id_for_preload_backend(&key);

    let overrides = node_wallet_key_overrides("step", &account).expect("overrides");

    let value_at = |path: String| {
        overrides
            .iter()
            .find(|config_override| config_override.path == path)
            .map(|config_override| config_override.value.clone())
    };
    assert_eq!(
        value_at(format!("kms.backend.keys.{key_id}")),
        Some(serde_yaml::to_value(&key).expect("key"))
    );
    assert_eq!(
        value_at(format!("wallet.known_keys.{key_id}")),
        Some(serde_yaml::to_value(account.public_key()).expect("public key"))
    );
}

#[test]
fn step_relative_genesis_time_uses_now_plus_offset() {
    let now = OffsetDateTime::from_unix_timestamp(1_000).expect("valid timestamp");

    let genesis_time = resolve_step_genesis_time("the chain starts 60 seconds from now", now, 60)
        .expect("offset should produce a valid genesis time");

    assert_eq!(genesis_time, GenesisTime::new(1_060));
}

#[test]
fn overflowing_step_relative_genesis_time_is_invalid_argument() {
    let now = time::Date::MAX.midnight().assume_utc();

    let error = resolve_step_genesis_time("the chain starts 1 seconds from now", now, 1)
        .expect_err("overflowing offset should fail");

    assert!(matches!(error, StepError::InvalidArgument { .. }));
}

#[test]
fn genesis_time_change_after_nodes_started_is_rejected() {
    let error =
        validate_genesis_time_change(Some(GenesisTime::new(1_000)), true, GenesisTime::new(1_001))
            .expect_err("genesis time should not change after nodes start");

    assert!(
        matches!(error, StepError::LogicalError { message } if message == "cannot change genesis time after nodes have started")
    );
}
