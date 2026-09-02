//! Starting Logos SQL instances and submitting application writes.

use futures::future::join_all;
use lb_core::mantle::gas::GasCost;
use lb_zone_sdk::sequencer::FundingConfig;
use logos_sql::{LogosSql, LogosSqlConfig, TransactionBuilder};
use tracing::info;

use super::tables::{InstanceRow, WriteRow};
use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
    world::CucumberWorld,
};

pub(super) async fn start_instances(
    world: &mut CucumberWorld,
    rows: Vec<InstanceRow>,
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
        let node_name = world.zone.sequencer_node_name(&row.sequencer)?.to_owned();
        let funding_pk = world.funding_wallet(&node_name)?.public_key()?;
        let config = LogosSqlConfig {
            channel_id: world.zone.sequencer_channel_id(&row.sequencer)?,
            signing_key: world.zone.sequencer_signing_key(&row.sequencer)?.clone(),
            node_url: world.zone_node_url_for_sequencer(&row.sequencer)?,
            funding: FundingConfig {
                funding_pk,
                change_pk: None,
                max_tx_fee: GasCost::new(u64::MAX),
                priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
            },
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
