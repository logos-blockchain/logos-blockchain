//! Well-known examples that pin the protocol's binary representation.

use lb_codec::codec_fixtures;
use rusqlite::types::Value;

use super::{ChannelInscription, SqlParameter, SqlText, Statement, Transaction, TxId};

codec_fixtures!(
    TxId,
    TxId::from([3; 32]) =>
        "0303030303030303030303030303030303030303030303030303030303030303"
);

codec_fixtures!(
    SqlText,
    SqlText::new("SELECT 1".to_owned()).expect("fixture should be valid") =>
        "0800000053454c4543542031"
);

codec_fixtures!(
    SqlParameter,
    SqlParameter::try_from(Value::Null).expect("fixture should be valid") => "00",
    SqlParameter::try_from(Value::Integer(42)).expect("fixture should be valid") =>
        "012a00000000000000",
    SqlParameter::try_from(Value::Real(1.5)).expect("fixture should be valid") =>
        "02000000000000f83f",
    SqlParameter::try_from(Value::Text("hi".to_owned())).expect("fixture should be valid") =>
        "03020000006869",
    SqlParameter::try_from(Value::Blob(vec![0, 255])).expect("fixture should be valid") =>
        "040200000000ff"
);

fn statement_fixture() -> Statement {
    Statement::new("SELECT 1".to_owned(), Vec::new()).expect("fixture should be valid")
}

fn statement_with_values_fixture() -> Statement {
    Statement::new(
        "VALUES".to_owned(),
        vec![
            Value::Null,
            Value::Integer(42),
            Value::Real(1.5),
            Value::Text("hi".to_owned()),
            Value::Blob(vec![0, 255]),
        ],
    )
    .expect("fixture should be valid")
}

codec_fixtures!(
    Statement,
    statement_fixture() => "0800000053454c454354203100000000",
    statement_with_values_fixture() => concat!(
        "0600000056414c55455305000000",
        "00",
        "012a00000000000000",
        "02000000000000f83f",
        "03020000006869",
        "040200000000ff"
    )
);

fn transaction_fixture() -> Transaction {
    Transaction::new(vec![statement_fixture()]).expect("fixture should be valid")
}

codec_fixtures!(
    Transaction,
    transaction_fixture() => "010000000800000053454c454354203100000000"
);

fn channel_inscription_fixture() -> ChannelInscription {
    ChannelInscription {
        tx_id: TxId::from([3; 32]),
        transaction: transaction_fixture(),
    }
}

codec_fixtures!(
    ChannelInscription,
    channel_inscription_fixture() => concat!(
        "0303030303030303030303030303030303030303030303030303030303030303",
        "010000000800000053454c454354203100000000"
    )
);
