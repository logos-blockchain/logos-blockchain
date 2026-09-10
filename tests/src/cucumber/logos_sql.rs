use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use logos_sql::{LogosSql, TxId};

use crate::cucumber::error::{StepError, StepResult};

/// Logos SQL runtimes and application writes owned by one Cucumber scenario.
#[derive(Default)]
pub struct LogosSqlState {
    instances: HashMap<String, LogosSql>,
    writes: HashMap<String, TxId>,
}

impl LogosSqlState {
    pub fn insert(&mut self, alias: String, instance: LogosSql) -> StepResult {
        if self.instances.contains_key(&alias) {
            return Err(StepError::LogicalError {
                message: format!("Logos SQL instance '{alias}' is already running"),
            });
        }

        self.instances.insert(alias, instance);

        Ok(())
    }

    pub fn instance(&self, alias: &str) -> Result<&LogosSql, StepError> {
        self.instances
            .get(alias)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Logos SQL instance '{alias}' is not running"),
            })
    }

    pub async fn stop(&mut self, alias: &str) -> StepResult {
        let instance = self
            .instances
            .remove(alias)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Logos SQL instance '{alias}' is not running"),
            })?;

        instance
            .shutdown()
            .await
            .map_err(|error| StepError::StepFail {
                message: format!("failed to stop Logos SQL instance '{alias}': {error}"),
            })
    }

    pub fn remember_write(&mut self, alias: String, tx_id: TxId) -> StepResult {
        if self.writes.contains_key(&alias) {
            return Err(StepError::LogicalError {
                message: format!("Logos SQL write '{alias}' is already recorded"),
            });
        }

        self.writes.insert(alias, tx_id);

        Ok(())
    }

    pub fn write(&self, alias: &str) -> Result<TxId, StepError> {
        self.writes
            .get(alias)
            .copied()
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Logos SQL write '{alias}' is not recorded"),
            })
    }

    pub async fn displaced_writes(&self) -> Result<HashSet<TxId>, StepError> {
        let mut displaced = HashSet::new();

        for instance in self.instances.values() {
            displaced.extend(instance.displaced_writes().await?);
        }

        Ok(displaced)
    }

    pub async fn shutdown_all(&mut self) -> StepResult {
        let instances = std::mem::take(&mut self.instances);

        for (alias, instance) in instances {
            instance
                .shutdown()
                .await
                .map_err(|error| StepError::StepFail {
                    message: format!("failed to stop Logos SQL instance '{alias}': {error}"),
                })?;
        }

        Ok(())
    }

    pub fn clear(&mut self) {
        self.instances.clear();
        self.writes.clear();
    }
}

impl fmt::Debug for LogosSqlState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut instances = self.instances.keys().collect::<Vec<_>>();
        instances.sort_unstable();

        let mut writes = self.writes.keys().collect::<Vec<_>>();
        writes.sort_unstable();

        formatter
            .debug_struct("LogosSqlState")
            .field("instances", &instances)
            .field("writes", &writes)
            .finish()
    }
}
