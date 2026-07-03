use lb_core_macros::nom_wire_fixtures;
use time::OffsetDateTime;

nom_wire_fixtures!(
    OffsetDateTime,
    OffsetDateTime::from_unix_timestamp(1_000).unwrap() => "e803000000000000"
);
