//! Correlates chain inclusion, chain finality and SQL replay during a
//! benchmark. These diagnostics explain where progress stops; TPS is measured
//! separately from the replica's finalized row counts.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write as _,
    path::Path,
    time::Duration,
};

use lb_core::header::HeaderId;
use lb_testing_framework::NodeHttpClient;
use lb_zone_sdk::{node_types::ChannelId, sequencer::channel_inscriptions};
use logos_sql::LogosSql;
use rusqlite::Connection;
use serde::Serialize;
use tokio::time::{Instant, sleep, timeout};

use super::row_count;
use crate::cucumber::error::{StepError, StepResult};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Writes chain and database observations to JSONL, including on failed runs.
pub(super) struct SqlProgressMonitor<'a> {
    client: &'a NodeHttpClient,
    channel_id: ChannelId,
    history: HashMap<HeaderId, HistoryCount>,
    live: Connection,
    finalized: Connection,
    output: File,
}

#[derive(Clone, Copy, Default, Serialize)]
struct HistoryCount {
    blocks: u64,
    inscriptions: u64,
}

#[derive(Serialize)]
struct ProgressSample {
    seconds: f64,
    collection_seconds: f64,
    tip_height: u64,
    tip: HeaderId,
    lib: HeaderId,
    lib_slot: u64,
    included_since_setup: HistoryCount,
    finalized_since_setup: HistoryCount,
    replica_live_rows: u64,
    replica_finalized_rows: u64,
}

impl<'a> SqlProgressMonitor<'a> {
    pub(super) fn new(
        client: &'a NodeHttpClient,
        channel_id: ChannelId,
        setup_lib: HeaderId,
        replica: &LogosSql,
        directory: &Path,
    ) -> Result<Self, StepError> {
        fs::create_dir_all(directory)?;

        Ok(Self {
            client,
            channel_id,
            history: HashMap::from([(setup_lib, HistoryCount::default())]),
            live: replica.read_connection()?,
            finalized: replica.finalized_read_connection()?,
            output: File::create(directory.join("logos-sql-progress.jsonl"))?,
        })
    }

    /// Persist each sample immediately so failed runs still leave diagnostics.
    /// The caller stops this loop when verification finishes. A collection
    /// failure fails the benchmark rather than leaving an unexplained gap.
    pub(super) async fn record_progress(mut self) -> StepResult {
        let started = Instant::now();

        loop {
            let sample = timeout(SAMPLE_TIMEOUT, self.sample(started))
                .await
                .map_err(|_| StepError::Timeout {
                    message: "benchmark progress sampling stalled".to_owned(),
                })??;

            serde_json::to_writer(&mut self.output, &sample).map_err(|error| {
                StepError::StepFail {
                    message: error.to_string(),
                }
            })?;
            writeln!(self.output)?;

            sleep(SAMPLE_INTERVAL).await;
        }
    }

    async fn sample(&mut self, started: Instant) -> Result<ProgressSample, StepError> {
        let collecting = Instant::now();
        let info = self.client.consensus_info().await?.cryptarchia_info;
        let included = self.history_at(info.tip).await?;
        let finalized = self.history_at(info.lib).await?;

        Ok(ProgressSample {
            seconds: started.elapsed().as_secs_f64(),
            collection_seconds: collecting.elapsed().as_secs_f64(),
            tip_height: info.height,
            tip: info.tip,
            lib: info.lib,
            lib_slot: info.lib_slot.into(),
            included_since_setup: included,
            finalized_since_setup: finalized,
            replica_live_rows: row_count(&self.live)?,
            replica_finalized_rows: row_count(&self.finalized)?,
        })
    }

    /// Cache counts by block ID, so polling fetches only unfamiliar blocks.
    /// A fork uses its own parent's count rather than adding orphaned traffic.
    async fn history_at(&mut self, mut block_id: HeaderId) -> Result<HistoryCount, StepError> {
        let mut missing = Vec::new();

        while !self.history.contains_key(&block_id) {
            let block = self
                .client
                .block(&block_id)
                .await?
                .ok_or_else(|| StepError::StepFail {
                    message: format!("benchmark progress block {block_id:?} is unavailable"),
                })?;
            let inscriptions = block
                .transactions
                .iter()
                .map(|tx| channel_inscriptions(tx, self.channel_id).len() as u64)
                .sum::<u64>();

            missing.push((block_id, inscriptions));
            block_id = block.header.parent_block;
        }

        let mut count = self.history[&block_id];

        for (id, inscriptions) in missing.into_iter().rev() {
            count.blocks += 1;
            count.inscriptions += inscriptions;
            self.history.insert(id, count);
        }

        Ok(count)
    }
}
