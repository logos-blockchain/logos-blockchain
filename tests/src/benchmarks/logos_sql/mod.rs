//! Settlement throughput of a single SQL writer and a read-only replica.
//!
//! Cucumber owns the chain setup and creates the `benchmark_writes` table.
//! This workload keeps inserting one row per transaction while the replica
//! samples finalized state. Warm-up and draining the last writes are outside
//! the throughput measurement.
//!
//! TPS is the increase in finalized rows divided by elapsed sampling time.
//! Each row represents one SQL transaction, not one inscription or block.

use std::{path::Path, time::Duration};

use lb_core::header::HeaderId;
use lb_testing_framework::NodeHttpClient;
use lb_zone_sdk::node_types::ChannelId;
use logos_sql::{Error as SqlError, LogosSql, TransactionBuilder};
use rand::{RngCore as _, SeedableRng as _, rngs::StdRng};
use rusqlite::Connection;
use tokio::{
    sync::watch,
    time::{Instant, sleep, timeout},
};

mod progress;
mod report;

use progress::SqlProgressMonitor;
use report::{ChainTraffic, SettlementReport, SettlementSample};

use crate::cucumber::error::{StepError, StepResult};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
const QUEUE_RETRY_DELAY: Duration = Duration::from_millis(5);
// A failure deadline for warm-up and draining, not an assumed finality delay.
const SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(300);
const WORKLOAD_SEED: u64 = 42;

const COUNT_ROWS: &str = "SELECT COUNT(*) FROM benchmark_writes";
const SELECT_ROWS: &str = "SELECT id, payload FROM benchmark_writes ORDER BY id";

/// One SQL transaction inserts one row. Seeded random blobs avoid inflating
/// compression results with repeated dummy data.
pub struct SqlWorkload {
    pub payload_bytes: usize,
    pub measure_for: Duration,
}

/// Recreates the same row sequence for submission and verification.
struct WorkloadRows {
    random: StdRng,
    next_id: u64,
    payload_bytes: usize,
}

impl WorkloadRows {
    fn new(payload_bytes: usize) -> Self {
        Self {
            random: StdRng::seed_from_u64(WORKLOAD_SEED),
            next_id: 1,
            payload_bytes,
        }
    }

    fn next_row(&mut self) -> Result<WorkloadRow, StepError> {
        let id = i64::try_from(self.next_id).map_err(|error| StepError::InvalidArgument {
            message: error.to_string(),
        })?;
        let mut payload = vec![0; self.payload_bytes];
        self.random.fill_bytes(&mut payload);
        self.next_id += 1;

        Ok(WorkloadRow { id, payload })
    }
}

struct WorkloadRow {
    id: i64,
    payload: Vec<u8>,
}

impl WorkloadRow {
    fn transaction(&self) -> TransactionBuilder {
        TransactionBuilder::new("INSERT INTO benchmark_writes (id, payload) VALUES (?1, ?2)")
            .bind(self.id)
            .bind(self.payload.clone())
    }
}

enum Submission {
    Committed,
    Stopped,
}

/// Runs against instances already started and funded by the test harness.
pub struct LogosSqlBenchmark<'a> {
    pub writer: &'a LogosSql,
    pub replica: &'a LogosSql,
    pub node: &'a NodeHttpClient,
    pub channel_id: ChannelId,
    pub output_dir: &'a Path,
    pub workload: SqlWorkload,
}

impl LogosSqlBenchmark<'_> {
    /// The schema must already be finalized on both instances. The table must
    /// be empty and this benchmark must be the channel's only writer.
    pub async fn run(&self) -> StepResult {
        self.check_preconditions()?;

        let setup_lib = self.node.consensus_info().await?.cryptarchia_info.lib;
        let monitor = SqlProgressMonitor::new(
            self.node,
            self.channel_id,
            setup_lib,
            self.replica,
            self.output_dir,
        )?;

        // Diagnostics run alongside the workload, including the final drain.
        // They help explain stalls but do not supply the TPS measurement.
        tokio::select! {
            result = self.measure_and_verify(setup_lib) => result,
            result = monitor.record_progress() => result,
        }
    }

    async fn measure_and_verify(&self, setup_lib: HeaderId) -> StepResult {
        let report = self.submit_and_measure().await?;
        report.save_samples(self.output_dir)?;

        self.drain_and_verify(report.submitted).await?;

        let traffic =
            ChainTraffic::read(self.node, self.channel_id, setup_lib, report.submitted).await?;

        report.save(self.output_dir, traffic)
    }

    /// Submit continuously through warm-up and measurement, then stop the
    /// writer. The remaining writes are drained separately from this timing.
    async fn submit_and_measure(&self) -> Result<SettlementReport, StepError> {
        let finalized = self.replica.finalized_read_connection()?;
        let (stop_tx, stop_rx) = watch::channel(false);
        let observe = async move {
            let samples = measure_settlement(finalized, self.workload.measure_for).await?;
            stop_tx.send_replace(true);

            Ok::<_, StepError>(samples)
        };

        let (submitted, samples) = timeout(SETTLEMENT_TIMEOUT + self.workload.measure_for, async {
            tokio::try_join!(self.submit_writes(stop_rx), observe)
        })
        .await
        .map_err(|_| StepError::Timeout {
            message: "SQL settlement benchmark stalled during warm-up or measurement".to_owned(),
        })??;

        SettlementReport::from_samples(self.workload.payload_bytes, submitted, samples)
    }

    /// Drain and check every submitted row before accepting the measurements.
    async fn drain_and_verify(&self, submitted: u64) -> StepResult {
        let finalized = self.replica.finalized_read_connection()?;
        let finalized = wait_for_finalized(finalized, submitted).await?;

        verify_rows(&finalized, submitted, self.workload.payload_bytes)
    }

    fn check_preconditions(&self) -> StepResult {
        if self.workload.measure_for.is_zero() || self.workload.payload_bytes == 0 {
            return Err(StepError::InvalidArgument {
                message: "measurement duration and payload size must be positive".to_owned(),
            });
        }

        let finalized = self.replica.finalized_read_connection()?;

        if row_count(&finalized)? != 0 {
            return Err(StepError::InvalidArgument {
                message: "benchmark table must start empty".to_owned(),
            });
        }

        Ok(())
    }

    /// Returns the number of rows committed by the writer before `stop`.
    async fn submit_writes(&self, stop: watch::Receiver<bool>) -> Result<u64, StepError> {
        let mut rows = WorkloadRows::new(self.workload.payload_bytes);
        let mut submitted = 0u64;

        while !*stop.borrow() {
            let row = rows.next_row()?;

            match self.submit_row(&row, &stop).await? {
                Submission::Committed => submitted += 1,
                Submission::Stopped => break,
            }
        }

        Ok(submitted)
    }

    /// A full queue has not committed the transaction, so the same input is
    /// retried until `stop`. Any other error invalidates the run rather than
    /// disappearing from the reported throughput.
    async fn submit_row(
        &self,
        row: &WorkloadRow,
        stop: &watch::Receiver<bool>,
    ) -> Result<Submission, StepError> {
        loop {
            match self.writer.execute(row.transaction()).await {
                Ok(_) => return Ok(Submission::Committed),
                Err(SqlError::PublishPending) if *stop.borrow() => return Ok(Submission::Stopped),
                Err(SqlError::PublishPending) => sleep(QUEUE_RETRY_DELAY).await,
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// Samples the finalized row count every [`SAMPLE_INTERVAL`] for `duration`,
/// starting the window at the first observed finalized write rather than at
/// submission. That first count is the baseline, so the initial batch is not
/// counted toward measured throughput.
///
/// The connection is owned because [`Connection`] is not `Sync`, and borrowing
/// it across an await would make the benchmark future non-`Send`.
async fn measure_settlement(
    connection: Connection,
    duration: Duration,
) -> Result<Vec<SettlementSample>, StepError> {
    let started = Instant::now();

    let first = loop {
        let sample = sample_finalized(&connection, started)?;

        if sample.finalized > 0 {
            break sample;
        }

        sleep(SAMPLE_INTERVAL).await;
    };

    let deadline = Instant::now() + duration;
    let mut samples = vec![first];

    loop {
        sleep(SAMPLE_INTERVAL).await;
        samples.push(sample_finalized(&connection, started)?);

        if Instant::now() >= deadline {
            return Ok(samples);
        }
    }
}

fn sample_finalized(
    connection: &Connection,
    started: Instant,
) -> Result<SettlementSample, StepError> {
    let finalized = row_count(connection)?;

    Ok(SettlementSample {
        seconds: started.elapsed().as_secs_f64(),
        finalized,
    })
}

/// Returns the connection once the replica finalized `expected` rows, so the
/// caller can verify them. See [`measure_settlement`] for why it is owned.
async fn wait_for_finalized(
    connection: Connection,
    expected: u64,
) -> Result<Connection, StepError> {
    timeout(SETTLEMENT_TIMEOUT, async move {
        while row_count(&connection)? != expected {
            sleep(SAMPLE_INTERVAL).await;
        }

        Ok(connection)
    })
    .await
    .map_err(|_| StepError::Timeout {
        message: format!("replica did not finalize all {expected} submitted SQL transactions"),
    })?
}

fn row_count(connection: &Connection) -> Result<u64, StepError> {
    let count = connection.query_row(COUNT_ROWS, [], |row| row.get(0))?;

    Ok(count)
}

fn verify_rows(connection: &Connection, expected: u64, payload_bytes: usize) -> StepResult {
    let mut statement = connection.prepare(SELECT_ROWS)?;
    let mut rows = statement.query([])?;

    let mut expected_rows = WorkloadRows::new(payload_bytes);

    for _ in 0..expected {
        let expected_row = expected_rows.next_row()?;
        let id = expected_row.id;

        let row = rows.next()?.ok_or_else(|| StepError::StepFail {
            message: format!("finalized row {id} is missing"),
        })?;

        if row.get::<_, i64>(0)? != id || row.get::<_, Vec<u8>>(1)? != expected_row.payload {
            return Err(StepError::StepFail {
                message: format!("finalized row {id} differs from the submitted write"),
            });
        }
    }

    if rows.next()?.is_some() {
        return Err(StepError::StepFail {
            message: "replica contains unexpected rows".to_owned(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rand::{RngCore as _, SeedableRng as _, rngs::StdRng};
    use rusqlite::{Connection, params};

    use super::{WORKLOAD_SEED, verify_rows};

    #[test]
    fn verification_checks_contents_not_just_row_count() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE benchmark_writes (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)",
            )
            .unwrap();
        let mut random = StdRng::seed_from_u64(WORKLOAD_SEED);

        for id in 1..=3 {
            let mut payload = vec![0; 32];
            random.fill_bytes(&mut payload);
            connection
                .execute(
                    "INSERT INTO benchmark_writes VALUES (?1, ?2)",
                    params![id, payload],
                )
                .unwrap();
        }

        verify_rows(&connection, 3, 32).unwrap();

        connection
            .execute(
                "UPDATE benchmark_writes SET payload = zeroblob(32) WHERE id = 2",
                [],
            )
            .unwrap();

        assert!(verify_rows(&connection, 3, 32).is_err());
    }
}
