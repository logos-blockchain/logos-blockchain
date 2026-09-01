use super::{
    GenesisTime, OffsetDateTime, StepError,
    genesis::{resolve_step_genesis_time, validate_genesis_time_change},
};

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
