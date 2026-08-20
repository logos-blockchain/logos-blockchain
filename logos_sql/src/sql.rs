//! Construction of replicated SQL transactions.

use rusqlite::types::{ToSql, ToSqlOutput, Value, ValueRef};

use crate::{
    error::Error,
    logos_sql::LogosSql,
    protocol::{IdempotencyKey, Statement, Transaction, TxId},
};

/// The beginning of a replicated SQL transaction.
///
/// Add its first statement with [`Self::query`]. Binding parameters and
/// executing the transaction become available after that statement exists.
#[must_use = "the transaction must be executed to apply its SQL"]
pub struct TransactionBuilder<'a> {
    logos_sql: &'a LogosSql,
}

impl<'a> TransactionBuilder<'a> {
    pub(crate) const fn new(logos_sql: &'a LogosSql) -> Self {
        Self { logos_sql }
    }

    /// Adds the first SQL query to this transaction.
    pub fn query(self, sql: impl Into<String>) -> QueryBuilder<'a> {
        QueryBuilder {
            logos_sql: self.logos_sql,
            transaction: TransactionDraft::new(sql),
        }
    }
}

/// A replicated SQL transaction with a current query.
///
/// Parameter values are converted immediately into owned `SQLite` values, so
/// borrowed application data does not need to outlive the call to
/// [`Self::bind`].
#[must_use = "the transaction must be executed to apply its SQL"]
pub struct QueryBuilder<'a> {
    logos_sql: &'a LogosSql,
    transaction: TransactionDraft,
}

struct TransactionDraft {
    statements: Vec<Statement>,
    current: PendingStatement,
    error: Option<Error>,
}

impl QueryBuilder<'_> {
    /// Adds the next SQL query to this transaction.
    pub fn query(mut self, sql: impl Into<String>) -> Self {
        self.transaction.query(sql);

        self
    }

    /// Binds one parameter to the current statement.
    ///
    /// Values use [`rusqlite::types::ToSql`], the same conversion interface as
    /// ordinary `SQLite` writes. Any conversion error is returned by
    /// [`Self::execute`].
    pub fn bind<T>(mut self, value: T) -> Self
    where
        T: ToSql,
    {
        self.transaction.bind(&value);

        self
    }

    /// Commits the transaction locally and submits it to `ZoneSDK`.
    ///
    /// A successful return means the SQL effects and recovery record are
    /// committed locally. Publication and finality remain asynchronous.
    ///
    /// # Errors
    ///
    /// Returns an error when a parameter cannot be represented by the `λSQL`
    /// protocol, validation or the local commit fails, the sequencer is not
    /// ready, or the runtime has halted.
    pub async fn execute(self, idempotency_key: IdempotencyKey) -> Result<TxId, Error> {
        let transaction = self.transaction.finish()?;

        self.logos_sql
            .commit_transaction(transaction, idempotency_key)
            .await
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

    use super::{Statement, Transaction, TransactionDraft, to_owned_value};

    #[test]
    fn assembles_statements_and_parameters_in_order() {
        let mut draft =
            TransactionDraft::new("INSERT INTO credentials (label, account) VALUES (?1, ?2)");

        draft.bind(&"email");
        draft.bind(&"andrus@example.org");
        draft.query("UPDATE credentials SET password = ?2 WHERE label = ?1");
        draft.bind(&"email");
        draft.bind(&42i64);

        let transaction = draft.finish().unwrap();
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
}
