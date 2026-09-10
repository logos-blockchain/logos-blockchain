//! Construction of replicated SQL transactions.

use rusqlite::types::{ToSql, ToSqlOutput, Value, ValueRef};

use crate::{
    error::Error,
    protocol::{Statement, Transaction, TxId},
};

/// A replicated SQL transaction with a current query.
///
/// Parameter values are converted immediately into owned `SQLite` values, so
/// borrowed application data does not need to outlive the call to
/// [`Self::bind`].
#[must_use = "the transaction must be passed to LogosSql::execute"]
pub struct TransactionBuilder {
    tx_id: TxId,
    transaction: TransactionDraft,
}

struct TransactionDraft {
    statements: Vec<Statement>,
    current: PendingStatement,
    error: Option<Error>,
}

impl TransactionBuilder {
    /// Starts a transaction with its first SQL query.
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            tx_id: TxId::generate(),
            transaction: TransactionDraft::new(sql),
        }
    }

    /// Returns the identity that will follow this write through publication
    /// and any later displacement.
    ///
    /// Applications can persist this value with their own operation before
    /// awaiting [`crate::LogosSql::execute`].
    #[must_use]
    pub const fn tx_id(&self) -> TxId {
        self.tx_id
    }

    /// Adds the next SQL query to this transaction.
    pub fn query(mut self, sql: impl Into<String>) -> Self {
        self.transaction.query(sql);

        self
    }

    /// Binds one parameter to the current statement.
    ///
    /// Values use [`rusqlite::types::ToSql`], the same conversion interface as
    /// ordinary `SQLite` writes. Any conversion error is returned by
    /// [`crate::LogosSql::execute`].
    pub fn bind<T>(mut self, value: T) -> Self
    where
        T: ToSql,
    {
        self.transaction.bind(&value);

        self
    }

    pub(crate) fn finish(self) -> Result<(TxId, Transaction), Error> {
        Ok((self.tx_id, self.transaction.finish()?))
    }
}

impl TransactionDraft {
    fn new(sql: impl Into<String>) -> Self {
        Self {
            statements: Vec::new(),
            current: PendingStatement::new(sql),
            error: None,
        }
    }

    fn query(&mut self, sql: impl Into<String>) {
        if self.error.is_some() {
            return;
        }

        let current = std::mem::replace(&mut self.current, PendingStatement::new(sql));

        match current.finish() {
            Ok(statement) => self.statements.push(statement),
            Err(error) => self.error = Some(error),
        }
    }

    fn bind(&mut self, value: &impl ToSql) {
        if self.error.is_some() {
            return;
        }

        match to_owned_value(value) {
            Ok(value) => self.current.params.push(value),
            Err(error) => self.error = Some(error),
        }
    }

    fn finish(self) -> Result<Transaction, Error> {
        if let Some(error) = self.error {
            return Err(error);
        }

        let mut statements = self.statements;
        statements.push(self.current.finish()?);

        Transaction::new(statements)
    }
}

struct PendingStatement {
    sql: String,
    params: Vec<Value>,
}

impl PendingStatement {
    fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: Vec::new(),
        }
    }

    fn finish(self) -> Result<Statement, Error> {
        Statement::new(self.sql, self.params)
    }
}

fn to_owned_value(value: &impl ToSql) -> Result<Value, Error> {
    match value.to_sql()? {
        ToSqlOutput::Borrowed(value) => owned_value_ref(value),
        ToSqlOutput::Owned(value) => Ok(value),
        _ => Err(Error::UnsupportedParameter),
    }
}

fn owned_value_ref(value: ValueRef<'_>) -> Result<Value, Error> {
    match value {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(value) => Ok(Value::Integer(value)),
        ValueRef::Real(value) => Ok(Value::Real(value)),
        ValueRef::Text(value) => String::from_utf8(value.to_vec())
            .map(Value::Text)
            .map_err(|_| Error::InvalidParameter("text parameter is not valid UTF-8")),
        ValueRef::Blob(value) => Ok(Value::Blob(value.to_vec())),
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::types::Value;

    use super::{Statement, Transaction, TransactionBuilder, to_owned_value};

    #[test]
    fn assembles_statements_and_parameters_in_order() {
        let transaction =
            TransactionBuilder::new("INSERT INTO credentials (label, account) VALUES (?1, ?2)")
                .bind("email")
                .bind("andrus@example.org")
                .query("UPDATE credentials SET password = ?2 WHERE label = ?1")
                .bind("email")
                .bind(42i64)
                .finish()
                .unwrap()
                .1;
        let expected = Transaction::new(vec![
            Statement::new(
                "INSERT INTO credentials (label, account) VALUES (?1, ?2)".to_owned(),
                vec![
                    Value::Text("email".to_owned()),
                    Value::Text("andrus@example.org".to_owned()),
                ],
            )
            .unwrap(),
            Statement::new(
                "UPDATE credentials SET password = ?2 WHERE label = ?1".to_owned(),
                vec![Value::Text("email".to_owned()), Value::Integer(42)],
            )
            .unwrap(),
        ])
        .unwrap();

        assert_eq!(transaction, expected);
    }

    #[test]
    fn converts_sql_parameters_to_owned_values() {
        let text = String::from("hello");

        assert_eq!(to_owned_value(&42i64).unwrap(), Value::Integer(42));
        assert_eq!(to_owned_value(&text.as_str()).unwrap(), Value::Text(text));
    }

    #[test]
    fn converts_none_to_sql_null() {
        assert_eq!(to_owned_value(&Option::<i64>::None).unwrap(), Value::Null);
    }

    #[test]
    fn transaction_identity_is_available_before_execution() {
        let transaction = TransactionBuilder::new("INSERT INTO items VALUES (?1)").bind(1i64);
        let tx_id = transaction.tx_id();

        let (finished_tx_id, _) = transaction.finish().expect("transaction should finish");

        assert_eq!(finished_tx_id, tx_id);
    }
}
