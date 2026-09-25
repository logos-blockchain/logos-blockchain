//! SQL transactions exchanged by `λSQL` instances.

use std::fmt::{self, Display, Formatter};

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode as _};
use lb_utils::bounded::{NonEmptyBoundedVec, UpperBoundedVec};
use lb_zone_sdk::node_types::Inscription;
use rand::RngCore as _;
use rusqlite::types::{ToSql, ToSqlOutput, Value};

use crate::error::Error;

mod codec;
mod compression;
mod fixtures;

// Every payload starts with this marker, version and body encoding.
pub const PAYLOAD_MARKER: [u8; 9] = *b"LOGOS_SQL";
const PAYLOAD_VERSION: u16 = 2;
const PAYLOAD_HEADER_LEN: usize = PAYLOAD_MARKER.len() + size_of::<u16>() + 1;

// Published bytes, including our header, must fit into one chain inscription.
const MAX_PAYLOAD_BYTES: usize = Inscription::MAX;

// Allow up to 64 MiB before compression. This caps decompression allocations;
// the compressed payload must still fit the chain's smaller inscription limit.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

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
    pub(crate) fn generate() -> Self {
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

        if sql.len() > MAX_BODY_BYTES {
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
            Value::Text(value) if value.len() > MAX_BODY_BYTES => {
                return Err(Error::InvalidTransaction("text parameter is too large"));
            }
            Value::Blob(value) if value.len() > MAX_BODY_BYTES => {
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

impl SqlParameter {
    pub fn into_value(self) -> Value {
        self.0
    }
}

/// One parameterized SQL statement.
#[derive(Clone, Debug, PartialEq, BinaryCodec)]
pub struct Statement {
    sql: SqlText,
    params: UpperBoundedVec<SqlParameter, MAX_BODY_BYTES>,
}

impl Statement {
    /// Creates one non-empty statement within the protocol limits.
    pub fn new(sql: String, params: Vec<Value>) -> Result<Self, Error> {
        if params.len() > MAX_BODY_BYTES {
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
    statements: NonEmptyBoundedVec<Statement, MAX_BODY_BYTES>,
}

impl Transaction {
    /// Creates a non-empty transaction within the protocol limits.
    pub fn new(statements: Vec<Statement>) -> Result<Self, Error> {
        if statements.is_empty() {
            return Err(Error::InvalidTransaction(
                "transaction must contain a statement",
            ));
        }

        if statements.len() > MAX_BODY_BYTES {
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
}

/// `SQLite` function whose result must be reproduced during channel replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturedFunction {
    Random,
    RandomBlob,
    Date,
    Time,
    DateTime,
    JulianDay,
    UnixEpoch,
    Strftime,
    TimeDiff,
    CurrentDate,
    CurrentTime,
    CurrentTimestamp,
}

/// One captured `SQLite` function call and the value returned locally.
#[derive(Clone, Debug, PartialEq, BinaryCodec)]
pub struct CapturedFunctionCall {
    pub function: CapturedFunction,
    pub result: SqlParameter,
}

impl CapturedFunctionCall {
    pub fn new(function: CapturedFunction, result: Value) -> Result<Self, Error> {
        Ok(Self {
            function,
            result: SqlParameter::try_from(result)?,
        })
    }
}

/// Function results captured while executing one replicated transaction.
#[derive(Clone, Debug, PartialEq, BinaryCodec)]
pub struct CapturedFunctionCalls {
    calls: UpperBoundedVec<CapturedFunctionCall, MAX_BODY_BYTES>,
}

impl CapturedFunctionCalls {
    pub fn new(calls: Vec<CapturedFunctionCall>) -> Result<Self, Error> {
        let calls = UpperBoundedVec::try_from(calls).map_err(|_| Error::InscriptionTooLarge)?;

        Ok(Self { calls })
    }

    pub const fn empty() -> Self {
        Self {
            calls: UpperBoundedVec::new_unchecked(Vec::new()),
        }
    }

    pub fn as_slice(&self) -> &[CapturedFunctionCall] {
        self.calls.as_slice()
    }
}

/// Transaction payload carried by a `λSQL` channel inscription.
#[derive(Clone, Debug, PartialEq, BinaryCodec)]
pub struct ChannelInscription {
    pub tx_id: TxId,
    pub transaction: Transaction,
    pub captured_function_calls: CapturedFunctionCalls,
}

impl ChannelInscription {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        if self.encoded_length() > MAX_BODY_BYTES {
            return Err(Error::InvalidTransaction(
                "transaction exceeds the uncompressed size limit",
            ));
        }

        let (encoding, body) = compression::encode(self.encode_to_vec())?;
        let mut payload = Vec::with_capacity(payload_len(body.len())?);

        payload.extend_from_slice(&PAYLOAD_MARKER);
        payload.extend_from_slice(&PAYLOAD_VERSION.to_le_bytes());
        payload.push(encoding);
        payload.extend_from_slice(&body);

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

        let body = compression::decode(header[PAYLOAD_HEADER_LEN - 1], body)?;

        <Self as BinaryDecode>::decode_all(&body, &())
            .map_err(|_| Error::InvalidPayload("body cannot be decoded"))
    }

    pub fn content_digest(&self) -> [u8; 32] {
        Blake2b::<U32>::digest(self.encode_to_vec()).into()
    }
}

/// A local write after its identity and channel payload have been encoded.
pub struct EncodedWrite {
    pub tx_id: TxId,
    pub content_digest: [u8; 32],
    pub payload: Vec<u8>,
}

impl EncodedWrite {
    pub fn new(
        tx_id: TxId,
        transaction: &Transaction,
        captured_function_calls: CapturedFunctionCalls,
    ) -> Result<Self, Error> {
        let channel_inscription = ChannelInscription {
            tx_id,
            transaction: transaction.clone(),
            captured_function_calls,
        };

        let content_digest = channel_inscription.content_digest();
        let payload = channel_inscription.encode()?;

        Ok(Self {
            tx_id,
            content_digest,
            payload,
        })
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
    use rand::{RngCore as _, SeedableRng as _, rngs::StdRng};
    use rusqlite::types::Value;

    use super::{
        CapturedFunctionCalls, ChannelInscription, EncodedWrite, MAX_BODY_BYTES, MAX_PAYLOAD_BYTES,
        Statement, Transaction, TxId,
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

        let encoded = EncodedWrite::new(
            TxId::generate(),
            &transaction,
            CapturedFunctionCalls::empty(),
        )
        .expect("payload should encode");
        let decoded = ChannelInscription::decode(&encoded.payload)
            .expect("channel inscription should decode");

        assert_eq!(decoded.tx_id, encoded.tx_id);
        assert_eq!(decoded.transaction, transaction);
        assert_eq!(decoded.content_digest(), encoded.content_digest);
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
            captured_function_calls: CapturedFunctionCalls::empty(),
        };
        let mut payload = write.encode().expect("payload should encode");
        payload.push(0);

        assert!(ChannelInscription::decode(&payload).is_err());
    }

    #[test]
    fn plain_and_compressed_payload_fixtures_decode() {
        let transaction = Transaction::new(vec![
            Statement::new("SELECT 1".to_owned(), Vec::new()).expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let write = ChannelInscription {
            tx_id: TxId::from([3; 32]),
            transaction,
            captured_function_calls: CapturedFunctionCalls::empty(),
        };

        let plain = hex::decode(concat!(
            "4c4f474f535f53514c020000",
            "0303030303030303030303030303030303030303030303030303030303030303",
            "010000000800000053454c45435420310000000000000000"
        ))
        .expect("fixture should be valid hex");

        let expected = hex::decode(concat!(
            "4c4f474f535f53514c02000138000000",
            "78018586c10d000004c44e7c8c610533889f1ffbcf422ca06993f2030190ad22c35b6df71c2abd027b"
        ))
        .expect("fixture should be valid hex");

        assert_eq!(
            ChannelInscription::decode(&expected).expect("compressed fixture should decode"),
            write
        );
        assert_eq!(
            ChannelInscription::decode(&plain).expect("plain fixture should decode"),
            write
        );

        let mut trailing_plain = plain;
        trailing_plain.push(0);

        assert!(ChannelInscription::decode(&trailing_plain).is_err());
    }

    #[test]
    fn content_digest_is_pinned() {
        let transaction = Transaction::new(vec![
            Statement::new("SELECT 1".to_owned(), Vec::new()).expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let write = ChannelInscription {
            tx_id: TxId::from([3; 32]),
            transaction,
            captured_function_calls: CapturedFunctionCalls::empty(),
        };

        assert_eq!(
            hex::encode(write.content_digest()),
            "b119823633ba6fbe90618b226b0d68eae1805876a86c1057237aca6205874b30"
        );
    }

    #[test]
    fn large_transaction_is_accepted_when_its_compressed_payload_fits() {
        let transaction = Transaction::new(vec![
            Statement::new(
                "SELECT ?1".to_owned(),
                vec![Value::Blob(vec![0; MAX_PAYLOAD_BYTES + 1])],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let encoded = EncodedWrite::new(
            TxId::generate(),
            &transaction,
            CapturedFunctionCalls::empty(),
        )
        .expect("compressed transaction should fit");

        assert!(encoded.payload.len() <= MAX_PAYLOAD_BYTES);
        let decoded = ChannelInscription::decode(&encoded.payload).expect("payload should decode");

        assert_eq!(decoded.transaction, transaction);
    }

    #[test]
    fn published_payload_must_still_fit_one_inscription() {
        let mut data = vec![0; MAX_PAYLOAD_BYTES + 1];
        StdRng::seed_from_u64(7).fill_bytes(&mut data);

        let transaction = Transaction::new(vec![
            Statement::new("SELECT ?1".to_owned(), vec![Value::Blob(data)])
                .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let write = ChannelInscription {
            tx_id: TxId::from([3; 32]),
            transaction,
            captured_function_calls: CapturedFunctionCalls::empty(),
        };

        assert!(matches!(
            write.encode(),
            Err(crate::Error::InscriptionTooLarge)
        ));
    }

    #[test]
    fn uncompressed_transaction_size_is_limited_even_if_it_would_compress() {
        let transaction = Transaction::new(vec![
            Statement::new(
                "SELECT ?1".to_owned(),
                vec![Value::Blob(vec![0; MAX_BODY_BYTES])],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");
        let write = ChannelInscription {
            tx_id: TxId::from([3; 32]),
            transaction,
            captured_function_calls: CapturedFunctionCalls::empty(),
        };

        assert!(matches!(
            write.encode(),
            Err(crate::Error::InvalidTransaction(_))
        ));
    }
}
