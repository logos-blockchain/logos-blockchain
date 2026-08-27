//! SQL transactions exchanged by `λSQL` instances.

use std::fmt::{self, Display, Formatter};

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use lb_codec::{BinaryCodec, BinaryDecode, BinaryEncode as _};
use lb_utils::bounded::{NonEmptyBoundedVec, UpperBoundedVec};
use lb_zone_sdk::node_types::Inscription;
use rand::RngCore as _;
use rusqlite::types::{ToSql, ToSqlOutput, Value};

use crate::error::Error;

mod codec;
mod fixtures;

pub const PAYLOAD_MARKER: [u8; 9] = *b"LOGOS_SQL";
const PAYLOAD_VERSION: u16 = 1;
const PAYLOAD_HEADER_LEN: usize = PAYLOAD_MARKER.len() + size_of::<u16>();
const MAX_PAYLOAD_BYTES: usize = Inscription::MAX;

/// Stable identity of one application write.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, BinaryCodec)]
pub struct TxId([u8; 32]);

impl Display for TxId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

impl TxId {
    fn generate() -> Self {
        let mut bytes = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);

        Self(bytes)
    }
}

impl From<[u8; 32]> for TxId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8; 32]> for TxId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<TxId> for [u8; 32] {
    fn from(tx_id: TxId) -> Self {
        tx_id.0
    }
}

/// Validated SQL text carried by one statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlText(String);

impl SqlText {
    fn new(sql: String) -> Result<Self, Error> {
        if sql.trim().is_empty() {
            return Err(Error::InvalidTransaction("statement SQL must not be empty"));
        }

        if sql.len() > MAX_PAYLOAD_BYTES {
            return Err(Error::InvalidTransaction("statement SQL is too large"));
        }

        Ok(Self(sql))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// One `SQLite` parameter with `λSQL`'s protocol validation.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlParameter(Value);

impl TryFrom<Value> for SqlParameter {
    type Error = Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match &value {
            Value::Real(value) if !value.is_finite() => {
                return Err(Error::InvalidTransaction("real parameters must be finite"));
            }
            Value::Text(value) if value.len() > MAX_PAYLOAD_BYTES => {
                return Err(Error::InvalidTransaction("text parameter is too large"));
            }
            Value::Blob(value) if value.len() > MAX_PAYLOAD_BYTES => {
                return Err(Error::InvalidTransaction("blob parameter is too large"));
            }
            _ => {}
        }

        Ok(Self(value))
    }
}

impl ToSql for SqlParameter {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

/// One parameterized SQL statement.
#[derive(Clone, Debug, PartialEq, BinaryCodec)]
pub struct Statement {
    sql: SqlText,
    params: UpperBoundedVec<SqlParameter, MAX_PAYLOAD_BYTES>,
}

impl Statement {
    /// Creates one non-empty statement within the protocol limits.
    pub fn new(sql: String, params: Vec<Value>) -> Result<Self, Error> {
        if params.len() > MAX_PAYLOAD_BYTES {
            return Err(Error::InvalidTransaction(
                "statement has too many parameters",
            ));
        }

        let sql = SqlText::new(sql)?;
        let params = params
            .into_iter()
            .map(SqlParameter::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let params = UpperBoundedVec::new_unchecked(params);

        Ok(Self { sql, params })
    }

    pub fn sql(&self) -> &str {
        self.sql.as_str()
    }

    pub fn params(&self) -> &[SqlParameter] {
        self.params.as_slice()
    }
}

/// Statements applied atomically at one channel position.
#[derive(Clone, Debug, PartialEq, BinaryCodec)]
pub struct Transaction {
    statements: NonEmptyBoundedVec<Statement, MAX_PAYLOAD_BYTES>,
}

impl Transaction {
    /// Creates a non-empty transaction within the protocol limits.
    pub fn new(statements: Vec<Statement>) -> Result<Self, Error> {
        if statements.is_empty() {
            return Err(Error::InvalidTransaction(
                "transaction must contain a statement",
            ));
        }

        if statements.len() > MAX_PAYLOAD_BYTES {
            return Err(Error::InvalidTransaction(
                "transaction contains too many statements",
            ));
        }

        Ok(Self {
            statements: NonEmptyBoundedVec::new_unchecked(statements),
        })
    }

    pub fn statements(&self) -> &[Statement] {
        self.statements.as_slice()
    }

    pub(crate) fn digest(&self) -> [u8; 32] {
        Blake2b::<U32>::digest(self.encode_to_vec()).into()
    }
}

/// Transaction payload carried by a `λSQL` channel inscription.
#[derive(Clone, Debug, PartialEq, BinaryCodec)]
pub struct ChannelInscription {
    pub tx_id: TxId,
    pub transaction: Transaction,
}

impl ChannelInscription {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let payload_len = payload_len(self.encoded_length())?;
        let mut payload = Vec::with_capacity(payload_len);

        payload.extend_from_slice(&PAYLOAD_MARKER);
        payload.extend_from_slice(&PAYLOAD_VERSION.to_le_bytes());
        self.encode_into(&mut payload);

        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(Error::InvalidPayload("payload exceeds the protocol limit"));
        }

        let (header, body) = payload
            .split_at_checked(PAYLOAD_HEADER_LEN)
            .ok_or(Error::InvalidPayload("header is missing"))?;

        if header[..PAYLOAD_MARKER.len()] != PAYLOAD_MARKER {
            return Err(Error::InvalidPayload("protocol marker does not match"));
        }

        let version_offset = PAYLOAD_MARKER.len();
        let version = u16::from_le_bytes([header[version_offset], header[version_offset + 1]]);

        if version != PAYLOAD_VERSION {
            return Err(Error::InvalidPayload("protocol version is not supported"));
        }

        <Self as BinaryDecode>::decode_all(body, &())
            .map_err(|_| Error::InvalidPayload("body cannot be decoded"))
    }
}

/// A local write after its identity and channel payload have been encoded.
pub struct EncodedWrite {
    pub tx_id: TxId,
    pub payload: Vec<u8>,
}

impl EncodedWrite {
    pub fn new(transaction: &Transaction) -> Result<Self, Error> {
        let tx_id = TxId::generate();

        let channel_inscription = ChannelInscription {
            tx_id,
            transaction: transaction.clone(),
        };

        let payload = channel_inscription.encode()?;

        Ok(Self { tx_id, payload })
    }
}

fn payload_len(body_len: usize) -> Result<usize, Error> {
    let payload_len = PAYLOAD_HEADER_LEN
        .checked_add(body_len)
        .ok_or(Error::InscriptionTooLarge)?;

    if payload_len > MAX_PAYLOAD_BYTES {
        return Err(Error::InscriptionTooLarge);
    }

    Ok(payload_len)
}

/// Returns whether an inscription belongs to `λSQL`.
#[must_use]
pub fn is_logos_sql_payload(payload: &[u8]) -> bool {
    payload.starts_with(&PAYLOAD_MARKER)
}

#[cfg(test)]
mod tests {
    use rusqlite::types::Value;

    use super::{
        ChannelInscription, EncodedWrite, MAX_PAYLOAD_BYTES, Statement, Transaction, TxId,
    };

    #[test]
    fn transaction_id_is_displayed_as_hex() {
        let tx_id = TxId::from([0xab; 32]);

        assert_eq!(tx_id.to_string(), "ab".repeat(32));
    }

    #[test]
    fn channel_inscription_round_trips() {
        let transaction = Transaction::new(vec![
            Statement::new(
                "INSERT INTO messages VALUES (?1)".to_owned(),
                vec![Value::Text("hello".to_owned())],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");

        let encoded = EncodedWrite::new(&transaction).expect("submission should encode");
        let decoded = ChannelInscription::decode(&encoded.payload)
            .expect("channel inscription should decode");

        assert_eq!(decoded.tx_id, encoded.tx_id);
        assert_eq!(decoded.transaction, transaction);
    }

    #[test]
    fn payload_rejects_trailing_bytes() {
        let transaction = Transaction::new(vec![
            Statement::new("SELECT 1".to_owned(), Vec::new()).expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let write = ChannelInscription {
            tx_id: TxId::from([3; 32]),
            transaction,
        };
        let mut payload = write.encode().expect("payload should encode");
        payload.push(0);

        assert!(ChannelInscription::decode(&payload).is_err());
    }

    #[test]
    fn payload_bytes_are_pinned() {
        let transaction = Transaction::new(vec![
            Statement::new("SELECT 1".to_owned(), Vec::new()).expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let write = ChannelInscription {
            tx_id: TxId::from([3; 32]),
            transaction,
        };

        let expected = hex::decode(concat!(
            "4c4f474f535f53514c0100",
            "0303030303030303030303030303030303030303030303030303030303030303",
            "010000000800000053454c454354203100000000"
        ))
        .expect("fixture should be valid hex");

        assert_eq!(write.encode().expect("payload should encode"), expected);
    }

    #[test]
    fn complete_payload_must_fit_one_inscription() {
        let transaction = Transaction::new(vec![
            Statement::new(
                "SELECT ?1".to_owned(),
                vec![Value::Blob(vec![0; MAX_PAYLOAD_BYTES])],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let result = EncodedWrite::new(&transaction);

        assert!(matches!(result, Err(crate::Error::InscriptionTooLarge)));
    }
}
