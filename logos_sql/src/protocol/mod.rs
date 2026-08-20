//! SQL transactions exchanged by `λSQL` instances.

use std::fmt::{self, Display, Formatter};

use bincode::Options as _;
use blake2::{Blake2b, Digest as _, digest::consts::U32};
use lb_utils::bounded::{BoundedError, NonEmptyBoundedVec};
pub use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::error::Error;

const PAYLOAD_MARKER: [u8; 9] = *b"LOGOS_SQL";
const PAYLOAD_VERSION: u16 = 1;
const PAYLOAD_HEADER_LEN: usize = PAYLOAD_MARKER.len() + size_of::<u16>();
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 1_024;
const TX_ID_DOMAIN_SEPARATOR: &[u8] = b"logos sql transaction id";

/// Stable identity of one application write.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
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
    fn derive(
        writer_id: &[u8; 32],
        idempotency_key: &IdempotencyKey,
        transaction_digest: &[u8; 32],
    ) -> Self {
        let mut hasher = Blake2b::<U32>::new();

        hasher.update(TX_ID_DOMAIN_SEPARATOR);
        hasher.update(writer_id);
        hasher.update(idempotency_key.as_ref());
        hasher.update(transaction_digest);

        Self(hasher.finalize().into())
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

/// Application-supplied identity for safely retrying one write.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct IdempotencyKey(NonEmptyBoundedVec<u8, MAX_IDEMPOTENCY_KEY_BYTES>);

impl TryFrom<Vec<u8>> for IdempotencyKey {
    type Error = BoundedError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        NonEmptyBoundedVec::try_from(bytes).map(Self)
    }
}

impl AsRef<[u8]> for IdempotencyKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// One parameterized SQL statement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Statement {
    sql: String,
    #[serde(with = "serialized_values")]
    params: Vec<Value>,
}

impl Statement {
    /// Creates one non-empty statement.
    ///
    /// # Errors
    ///
    /// Returns an error if `sql` is empty.
    pub fn new(sql: String, params: Vec<Value>) -> Result<Self, Error> {
        if sql.trim().is_empty() {
            return Err(Error::InvalidTransaction("statement SQL must not be empty"));
        }

        Ok(Self { sql, params })
    }

    /// Returns the SQL text.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// Returns parameters in `SQLite` binding order.
    #[must_use]
    pub fn params(&self) -> &[Value] {
        &self.params
    }
}

mod serialized_values {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Value;

    /// Serializable representation of a `rusqlite` parameter value.
    #[derive(Deserialize, Serialize)]
    enum SqlValue {
        Null,
        Integer(i64),
        Real(f64),
        Text(String),
        Blob(Vec<u8>),
    }

    impl From<&Value> for SqlValue {
        fn from(value: &Value) -> Self {
            match value {
                Value::Null => Self::Null,
                Value::Integer(value) => Self::Integer(*value),
                Value::Real(value) => Self::Real(*value),
                Value::Text(value) => Self::Text(value.clone()),
                Value::Blob(value) => Self::Blob(value.clone()),
            }
        }
    }

    impl From<SqlValue> for Value {
        fn from(value: SqlValue) -> Self {
            match value {
                SqlValue::Null => Self::Null,
                SqlValue::Integer(value) => Self::Integer(value),
                SqlValue::Real(value) => Self::Real(value),
                SqlValue::Text(value) => Self::Text(value),
                SqlValue::Blob(value) => Self::Blob(value),
            }
        }
    }

    pub fn serialize<S>(values: &[Value], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .iter()
            .map(SqlValue::from)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<Value>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<SqlValue>::deserialize(deserializer)
            .map(|values| values.into_iter().map(Value::from).collect())
    }
}

/// Statements applied atomically at one channel position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transaction {
    statements: Vec<Statement>,
}

impl Transaction {
    /// Creates a non-empty transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if no statements are provided.
    pub fn new(statements: Vec<Statement>) -> Result<Self, Error> {
        if statements.is_empty() {
            return Err(Error::InvalidTransaction(
                "transaction must contain a statement",
            ));
        }

        Ok(Self { statements })
    }

    /// Returns statements in execution order.
    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    fn digest(&self) -> Result<[u8; 32], Error> {
        let encoded = codec().serialize(self)?;

        Ok(Blake2b::<U32>::digest(encoded).into())
    }
}

/// Transaction payload carried by a `λSQL` channel inscription.
#[derive(Serialize, Deserialize)]
pub struct ChannelWrite {
    pub tx_id: TxId,
    pub transaction: Transaction,
}

impl ChannelWrite {
    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut payload = Vec::from(PAYLOAD_MARKER);
        payload.extend_from_slice(&PAYLOAD_VERSION.to_le_bytes());
        payload.extend(codec().serialize(self)?);

        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let (header, body) = payload
            .split_at_checked(PAYLOAD_HEADER_LEN)
            .ok_or(Error::InvalidPayload("header is missing"))?;

        if header[..PAYLOAD_MARKER.len()] != PAYLOAD_MARKER {
            return Err(Error::InvalidPayload("protocol marker does not match"));
        }

        let version_offset = PAYLOAD_MARKER.len();
        let version = u16::from_le_bytes([header[version_offset], header[version_offset + 1]]);

        if version != PAYLOAD_VERSION {
            return Err(Error::UnsupportedProtocolVersion(version));
        }

        codec()
            .deserialize(body)
            .map_err(|_| Error::InvalidPayload("body cannot be decoded"))
    }
}

/// A local write after its identity and channel payload have been encoded.
pub struct EncodedWrite {
    pub tx_id: TxId,
    pub transaction_digest: [u8; 32],
    pub payload: Vec<u8>,
}

impl EncodedWrite {
    pub fn new(
        transaction: &Transaction,
        idempotency_key: &IdempotencyKey,
        writer_id: &[u8; 32],
    ) -> Result<Self, Error> {
        let transaction_digest = transaction.digest()?;
        let tx_id = TxId::derive(writer_id, idempotency_key, &transaction_digest);

        let channel_write = ChannelWrite {
            tx_id,
            transaction: transaction.clone(),
        };

        let payload = channel_write.encode()?;

        Ok(Self {
            tx_id,
            transaction_digest,
            payload,
        })
    }
}

/// Returns whether an inscription belongs to `λSQL`.
#[must_use]
pub fn is_logos_sql_payload(payload: &[u8]) -> bool {
    payload.starts_with(&PAYLOAD_MARKER)
}

fn codec() -> impl bincode::Options {
    bincode::DefaultOptions::new()
        .with_little_endian()
        .with_fixint_encoding()
        .reject_trailing_bytes()
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedError;

    use super::{
        ChannelWrite, EncodedWrite, IdempotencyKey, MAX_IDEMPOTENCY_KEY_BYTES, Statement,
        Transaction, TxId, Value,
    };

    #[test]
    fn transaction_id_is_displayed_as_hex() {
        let tx_id = TxId::from([0xab; 32]);

        assert_eq!(tx_id.to_string(), "ab".repeat(32));
    }

    #[test]
    fn idempotency_key_must_not_be_empty() {
        let result = IdempotencyKey::try_from(Vec::new());

        assert_eq!(result, Err(BoundedError::EmptyInput));
    }

    #[test]
    fn idempotency_key_has_a_size_limit() {
        let result = IdempotencyKey::try_from(vec![0; MAX_IDEMPOTENCY_KEY_BYTES + 1]);

        assert_eq!(
            result,
            Err(BoundedError::TooManyItems {
                count: MAX_IDEMPOTENCY_KEY_BYTES + 1,
                max: MAX_IDEMPOTENCY_KEY_BYTES,
            })
        );
    }

    #[test]
    fn channel_write_round_trips() {
        let transaction = Transaction::new(vec![
            Statement::new(
                "INSERT INTO messages VALUES (?1)".to_owned(),
                vec![Value::Text("hello".to_owned())],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid");

        let key = IdempotencyKey::try_from(b"request-1".to_vec()).expect("key should be valid");
        let encoded =
            EncodedWrite::new(&transaction, &key, &[3; 32]).expect("submission should encode");
        let decoded = ChannelWrite::decode(&encoded.payload).expect("channel write should decode");

        assert_eq!(decoded.tx_id, encoded.tx_id);
        assert_eq!(decoded.transaction, transaction);
    }
}
