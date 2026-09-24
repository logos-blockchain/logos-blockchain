//! Starting Logos SQL instances and submitting application writes.

use futures::future::join_all;
use lb_core::mantle::gas::GasCost;
use lb_zone_sdk::sequencer::FundingConfig;
use logos_sql::{LogosSql, LogosSqlConfig, PublicationConfig, TransactionBuilder, WriterConfig};
use tracing::info;

use super::tables::{InstanceRow, WriteRow};
use crate::{
    benchmarks::logos_sql::{LogosSqlBenchmark, SqlWorkload},
    cucumber::{
        error::{StepError, StepResult},
        steps::TARGET,
        world::CucumberWorld,
    },
};

#[derive(Clone, Copy)]
pub(super) enum InstanceMode {
    ReadOnly,
    Writer {
        priority_fee_percent: u64,
        publication: PublicationConfig,
    },
}

impl Default for InstanceMode {
    fn default() -> Self {
        Self::Writer {
            priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
            publication: PublicationConfig::default(),
        }
    }
}

impl InstanceMode {
    const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

pub(super) async fn start_instances(
    world: &mut CucumberWorld,
    rows: Vec<InstanceRow>,
    mode: InstanceMode,
) -> StepResult {
    let test_context =
        world
            .lifecycle
            .test_context
            .clone()
            .ok_or_else(|| StepError::LogicalError {
                message: "Cucumber test context is not initialized".to_owned(),
            })?;

    for row in rows {
        let writer = match mode {
            InstanceMode::ReadOnly => None,
            InstanceMode::Writer {
                priority_fee_percent,
                publication,
            } => {
                let node_name = world.zone.sequencer_node_name(&row.sequencer)?.to_owned();
                let funding_pk = world.funding_wallet(&node_name)?.public_key()?;

                Some(WriterConfig {
                    publication,
                    signing_key: world.zone.sequencer_signing_key(&row.sequencer)?.clone(),
                    funding: FundingConfig {
                        funding_pk,
                        change_pk: None,
                        max_tx_fee: GasCost::new(u64::MAX),
                        priority_fee_percent,
                    },
                })
            }
        };

        let config = LogosSqlConfig {
            channel_id: world.zone.sequencer_channel_id(&row.sequencer)?,
            node_url: world.zone_node_url_for_sequencer(&row.sequencer)?,
            writer,
            state_dir: world
                .lifecycle
                .scenario_base_dir
                .join("logos_sql")
                .join(&test_context)
                .join(&row.alias),
        };

        info!(
            target: TARGET,
            instance = %row.alias,
            sequencer = %row.sequencer,
            read_only = mode.is_read_only(),
            "Starting Logos SQL instance"
        );

        let instance = LogosSql::start(config).await?;
        world.logos_sql.insert(row.alias, instance)?;
    }

    Ok(())
}

pub(super) async fn stop_instance(world: &mut CucumberWorld, alias: &str) -> StepResult {
    info!(target: TARGET, instance = alias, "Stopping Logos SQL instance");

    world.logos_sql.stop(alias).await
}

pub(super) async fn execute_write(
    world: &mut CucumberWorld,
    instance_alias: &str,
    write_alias: String,
    sql: String,
) -> StepResult {
    let tx_id = world
        .logos_sql
        .instance(instance_alias)?
        .execute(TransactionBuilder::new(sql))
        .await?;

    info!(
        target: TARGET,
        instance = instance_alias,
        write = %write_alias,
        %tx_id,
        "Committed Logos SQL write locally"
    );

    world.logos_sql.remember_write(write_alias, tx_id)
}

pub(super) async fn execute_writes_concurrently(
    world: &mut CucumberWorld,
    rows: Vec<WriteRow>,
) -> StepResult {
    let executions = rows.into_iter().map(async |row| {
        let result = world
            .logos_sql
            .instance(&row.instance)?
            .execute(TransactionBuilder::new(row.sql))
            .await
            .map_err(StepError::from);

        Ok::<_, StepError>((row.instance, row.write, result?))
    });

    for result in join_all(executions).await {
        let (instance_alias, write_alias, tx_id) = result?;

        info!(
            target: TARGET,
            instance = %instance_alias,
            write = %write_alias,
            %tx_id,
            "Committed concurrent Logos SQL write locally"
        );

        world.logos_sql.remember_write(write_alias, tx_id)?;
    }

    Ok(())
}

/// Connects the benchmark to the instances and output directory of this
/// scenario.
pub(super) async fn benchmark(
    world: &CucumberWorld,
    writer_alias: &str,
    replica_alias: &str,
    sequencer_alias: &str,
    workload: SqlWorkload,
) -> StepResult {
    let client = world.zone_node_http_client_for_sequencer(sequencer_alias)?;
    let channel_id = world.zone.sequencer_channel_id(sequencer_alias)?;
    let benchmark = LogosSqlBenchmark {
        writer: world.logos_sql.instance(writer_alias)?,
        replica: world.logos_sql.instance(replica_alias)?,
        node: &client,
        channel_id,
        output_dir: &world.lifecycle.scenario_base_dir,
        workload,
    };

    benchmark.run().await
}
