//! Password-manager data and operations.
//!
//! `PasswordManager` owns `LogosSql` directly so the example shows where the
//! application ends and the replication library begins. Domain operations are
//! translated into parameterized SQL writes here.

use logos_sql::{Error as LogosSqlError, LogosSql, LogosSqlConfig, TransactionBuilder, TxId};

use crate::AppResult;

const SELECT_CREDENTIALS: &str = "
    SELECT label, account
    FROM credentials
    ORDER BY label
";
const SELECT_SCHEMA_EXISTS: &str = "
    SELECT COUNT(*) = 1
    FROM sqlite_schema
    WHERE type = 'table' AND name = 'credentials'
";

fn prepare_schema() -> TransactionBuilder {
    TransactionBuilder::new(
        "CREATE TABLE IF NOT EXISTS credentials (
            label TEXT PRIMARY KEY,
            account TEXT NOT NULL,
            password TEXT NOT NULL
        )",
    )
}

fn prepare_credential_insert(label: &str, account: &str, password: &str) -> TransactionBuilder {
    TransactionBuilder::new(
        "INSERT INTO credentials (label, account, password)
         VALUES (?1, ?2, ?3)",
    )
    .bind(label)
    .bind(account)
    .bind(password)
}

fn prepare_password_update(label: &str, password: &str) -> TransactionBuilder {
    TransactionBuilder::new(
        "UPDATE credentials
         SET password = ?2
         WHERE label = ?1",
    )
    .bind(label)
    .bind(password)
}

fn prepare_credential_delete(label: &str) -> TransactionBuilder {
    TransactionBuilder::new("DELETE FROM credentials WHERE label = ?1").bind(label)
}

/// One credential in the current local database view.
///
/// Passwords are deliberately plaintext in this first example slice. Do not
/// use this example with real credentials.
#[derive(Debug)]
pub struct Credential {
    pub label: String,
    pub account: String,
    pub password: String,
}

/// Non-secret credential information shown when listing the database.
#[derive(Debug)]
pub struct CredentialSummary {
    pub label: String,
    pub account: String,
}

/// Application-facing password-manager operations over one `λSQL` database.
pub struct PasswordManager {
    logos_sql: LogosSql,
}

impl PasswordManager {
    /// Starts `λSQL`, catches up with the channel, and prepares the database.
    pub async fn start(config: LogosSqlConfig) -> AppResult<Self> {
        let manager = Self {
            logos_sql: LogosSql::start(config).await?,
        };

        manager.initialize().await?;

        Ok(manager)
    }

    async fn initialize(&self) -> AppResult<()> {
        if self.schema_exists()? {
            return Ok(());
        }

        self.logos_sql.execute(prepare_schema()).await?;

        Ok(())
    }

    /// Existing participants install the schema through channel replay before
    /// startup completes. Only an empty database needs to publish the DDL.
    fn schema_exists(&self) -> AppResult<bool> {
        let connection = self.logos_sql.read_connection()?;

        Ok(connection.query_row(SELECT_SCHEMA_EXISTS, [], |row| row.get(0))?)
    }

    /// Adds one credential.
    ///
    /// Concurrent attempts to use the same label produce a primary-key
    /// conflict.
    pub async fn add(&self, label: String, account: String, password: String) -> AppResult<TxId> {
        // TODO(security): Encrypt the password before it enters `λSQL`. The
        // resulting ciphertext, salt, and nonce should be bound instead.
        Ok(self
            .logos_sql
            .execute(prepare_credential_insert(&label, &account, &password))
            .await?)
    }

    /// Replaces the password stored under one label.
    pub async fn update_password(&self, label: String, password: String) -> AppResult<TxId> {
        Ok(self
            .logos_sql
            .execute(prepare_password_update(&label, &password))
            .await?)
    }

    /// Removes one credential.
    pub async fn remove(&self, label: String) -> AppResult<TxId> {
        Ok(self
            .logos_sql
            .execute(prepare_credential_delete(&label))
            .await?)
    }

    /// Reads one credential from the local `SQLite` database.
    pub fn credential(&self, label: &str) -> AppResult<Option<Credential>> {
        let connection = self.logos_sql.read_connection()?;
        let mut statement = connection.prepare(
            "SELECT label, account, password
             FROM credentials
             WHERE label = ?1",
        )?;
        let mut rows = statement.query([label])?;

        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        Ok(Some(Credential {
            label: row.get("label")?,
            account: row.get("account")?,
            password: row.get("password")?,
        }))
    }

    /// Lists credential labels and accounts from the local database.
    pub fn credentials(&self) -> AppResult<Vec<CredentialSummary>> {
        let connection = self.logos_sql.read_connection()?;
        let mut statement = connection.prepare(SELECT_CREDENTIALS)?;

        let credentials = statement
            .query_map([], |row| {
                Ok(CredentialSummary {
                    label: row.get("label")?,
                    account: row.get("account")?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(credentials)
    }

    /// Gracefully stops the owned `λSQL` runtime.
    pub async fn shutdown(self) -> Result<(), LogosSqlError> {
        self.logos_sql.shutdown().await
    }
}
