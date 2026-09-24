//! Throughput measurements and the saved benchmark report.

use std::{fs, path::Path};

use lb_core::header::HeaderId;
use lb_testing_framework::NodeHttpClient;
use lb_zone_sdk::{node_types::ChannelId, sequencer::channel_inscriptions};
use serde::Serialize;

use crate::cucumber::error::{StepError, StepResult};

const SAMPLES_FILE: &str = "logos-sql-samples.json";
const REPORT_FILE: &str = "logos-sql-benchmark.json";

/// Sample times are measured from workload startup, including the initial
/// wait for finalization.
#[derive(Serialize)]
pub(super) struct SettlementSample {
    pub(super) seconds: f64,
    pub(super) finalized: u64,
}

/// Finalized SQL throughput for the sampling window, excluding warm-up and
/// drain.
#[derive(Serialize)]
pub(super) struct SettlementReport {
    payload_bytes: usize,
    pub(super) submitted: u64,
    measured_seconds: f64,
    measured_transactions: u64,
    finalized_transactions_per_second: f64,
    #[serde(skip)]
    samples: Vec<SettlementSample>,
}

#[derive(Serialize)]
struct BenchmarkReport<'a> {
    workload: &'static str,
    measurement: &'a SettlementReport,
    whole_workload_traffic: ChainTraffic,
}

/// Average batching efficiency and chain payload cost for the full workload.
#[derive(Serialize)]
pub(super) struct ChainTraffic {
    sql_transactions_per_inscription: f64,
    published_bytes_per_sql_transaction: f64,
}

impl SettlementReport {
    pub(super) fn from_samples(
        payload_bytes: usize,
        submitted: u64,
        samples: Vec<SettlementSample>,
    ) -> Result<Self, StepError> {
        let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
            return Err(StepError::StepFail {
                message: "no settlement samples were observed".to_owned(),
            });
        };

        let measured_seconds = last.seconds - first.seconds;
        let measured_transactions = last.finalized - first.finalized;

        if measured_transactions == 0 {
            return Err(StepError::StepFail {
                message: "no SQL transactions finalized during the measurement window".to_owned(),
            });
        }

        Ok(Self {
            payload_bytes,
            submitted,
            measured_seconds,
            measured_transactions,
            finalized_transactions_per_second: measured_transactions as f64 / measured_seconds,
            samples,
        })
    }

    /// Retain observations even if draining or result verification fails.
    pub(super) fn save_samples(&self, directory: &Path) -> StepResult {
        let observations = serde_json::json!({
            "submitted": self.submitted,
            "samples": self.samples,
        });

        write_json(directory, SAMPLES_FILE, &observations).map(drop)
    }

    /// Publish the report only after all submitted rows have been verified.
    pub(super) fn save(&self, directory: &Path, traffic: ChainTraffic) -> StepResult {
        let report = BenchmarkReport {
            workload: "one blob insert per SQL transaction; one writer; one read-only replica",
            measurement: self,
            whole_workload_traffic: traffic,
        };

        let json = write_json(directory, REPORT_FILE, &report)?;
        println!("Logos SQL benchmark:\n{json}");

        Ok(())
    }
}

impl ChainTraffic {
    /// Counts finalized channel payloads across warm-up, measurement and drain.
    /// The setup block excludes schema creation. Payload sizes exclude the
    /// enclosing chain transaction and proofs, so they are not gas estimates.
    pub(super) async fn read(
        client: &NodeHttpClient,
        channel_id: ChannelId,
        setup_lib: HeaderId,
        submitted: u64,
    ) -> Result<Self, StepError> {
        let mut current = client.consensus_info().await?.cryptarchia_info.lib;
        let mut inscription_count = 0u64;
        let mut payload_bytes = 0u64;

        while current != setup_lib {
            let block = client
                .block(&current)
                .await?
                .ok_or_else(|| StepError::StepFail {
                    message: format!("finalized block {current:?} is unavailable"),
                })?;

            let inscriptions = block
                .transactions
                .iter()
                .flat_map(|transaction| channel_inscriptions(transaction, channel_id));

            for inscription in inscriptions {
                inscription_count += 1;
                payload_bytes += inscription.payload.len() as u64;
            }

            current = block.header.parent_block;
        }

        if inscription_count == 0 {
            return Err(StepError::StepFail {
                message: "no workload inscriptions found".to_owned(),
            });
        }

        Ok(Self {
            sql_transactions_per_inscription: submitted as f64 / inscription_count as f64,
            published_bytes_per_sql_transaction: payload_bytes as f64 / submitted as f64,
        })
    }
}

/// Writes `value` as pretty JSON into `directory` and returns the text.
fn write_json(
    directory: &Path,
    file_name: &str,
    value: &impl Serialize,
) -> Result<String, StepError> {
    let json = serde_json::to_string_pretty(value).map_err(|error| StepError::StepFail {
        message: error.to_string(),
    })?;

    fs::create_dir_all(directory)?;
    fs::write(directory.join(file_name), &json)?;

    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::{SettlementReport, SettlementSample};

    #[test]
    fn throughput_counts_only_writes_finalized_during_measurement() {
        let samples = [(2.0, 20), (5.0, 20), (8.0, 50), (12.0, 50)]
            .into_iter()
            .map(|(seconds, finalized)| SettlementSample { seconds, finalized })
            .collect();

        let report = SettlementReport::from_samples(256, 80, samples)
            .expect("measurement should produce a report");

        assert_eq!(report.measured_transactions, 30);
        assert!((report.finalized_transactions_per_second - 3.0).abs() < f64::EPSILON);
        assert!((report.measured_seconds - 10.0).abs() < f64::EPSILON);
        assert_eq!(report.submitted, 80);
    }
}
