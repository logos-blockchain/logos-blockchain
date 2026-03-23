use std::{
    env,
    time::{Duration, SystemTime},
};

use lb_testing_framework::{
    CoreBuilderExt as _, LbcLocalDeployer, ScenarioBuilder, ScenarioBuilderExt as _,
};
use testing_framework_core::scenario::{Deployer as _, ExternalNodeSource};
use thiserror::Error;

const DEFAULT_CHANNELS: usize = 8;
const DEFAULT_PAYLOAD_BYTES: usize = 128;
const DEFAULT_RUN_DURATION_SECS: u64 = 60 * 60;

#[derive(Debug, Error)]
enum TestConfigError {
    #[error("missing external URLs; set LOGOS_EXTERNAL_NODE_URLS or pass --external-node-urls")]
    MissingExternalUrls,
    #[error("external node URL list resolved to empty")]
    EmptyExternalUrls,
    #[error("invalid --inscription-channels value '{raw}': {source}")]
    InvalidChannels {
        raw: String,
        source: std::num::ParseIntError,
    },
    #[error("inscription channel count must be > 0")]
    ZeroChannels,
    #[error("invalid --inscription-payload-bytes value '{raw}': {source}")]
    InvalidPayloadBytes {
        raw: String,
        source: std::num::ParseIntError,
    },
    #[error("inscription payload bytes must be > 0")]
    ZeroPayloadBytes,
    #[error("invalid --run-duration-secs value '{raw}': {source}")]
    InvalidRunDuration {
        raw: String,
        source: std::num::ParseIntError,
    },
    #[error("run duration must be > 0 seconds")]
    ZeroRunDuration,
}

fn external_nodes_from_env() -> Result<Vec<ExternalNodeSource>, TestConfigError> {
    let raw =
        env::var("LOGOS_EXTERNAL_NODE_URLS").map_err(|_| TestConfigError::MissingExternalUrls)?;
    let mut nodes = Vec::new();
    for (idx, url) in raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        nodes.push(ExternalNodeSource::new(
            format!("external-{idx}"),
            url.to_owned(),
        ));
    }

    if nodes.is_empty() {
        return Err(TestConfigError::EmptyExternalUrls);
    }

    Ok(nodes)
}

fn channels_from_env() -> Result<usize, TestConfigError> {
    let raw =
        env::var("LOGOS_INSCRIPTION_CHANNELS").unwrap_or_else(|_| DEFAULT_CHANNELS.to_string());
    let channels = raw
        .parse::<usize>()
        .map_err(|source| TestConfigError::InvalidChannels { raw, source })?;

    if channels == 0 {
        return Err(TestConfigError::ZeroChannels);
    }

    Ok(channels)
}

fn inscription_payload_bytes_from_env() -> Result<usize, TestConfigError> {
    let raw = env::var("LOGOS_INSCRIPTION_PAYLOAD_BYTES")
        .unwrap_or_else(|_| DEFAULT_PAYLOAD_BYTES.to_string());

    let payload_bytes = raw
        .parse::<usize>()
        .map_err(|source| TestConfigError::InvalidPayloadBytes { raw, source })?;

    if payload_bytes == 0 {
        return Err(TestConfigError::ZeroPayloadBytes);
    }

    Ok(payload_bytes)
}

fn run_duration_from_env() -> Result<Duration, TestConfigError> {
    let raw = env::var("LOGOS_WORKLOAD_DURATION_SECS")
        .unwrap_or_else(|_| DEFAULT_RUN_DURATION_SECS.to_string());
    let secs = raw
        .parse::<u64>()
        .map_err(|source| TestConfigError::InvalidRunDuration { raw, source })?;

    if secs == 0 {
        return Err(TestConfigError::ZeroRunDuration);
    }

    Ok(Duration::from_secs(secs))
}

fn unique_scenario_base_dir() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0u128, |duration| duration.as_nanos());

    env::temp_dir().join(format!("tf-external-urls-inscription-{nanos}"))
}

#[tokio::test]
#[ignore = "long-running scenario: external inscription workload; duration configurable via LOGOS_WORKLOAD_DURATION_SECS"]
async fn external_urls_inscription_workload() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let external_nodes = external_nodes_from_env()?;
    let inscription_channels = channels_from_env()?;
    let inscription_payload_bytes = inscription_payload_bytes_from_env()?;
    let run_duration = run_duration_from_env()?;

    let deployer = LbcLocalDeployer::new();

    // External-only sources: no managed nodes.
    let scenario_base_dir = unique_scenario_base_dir();
    let mut builder =
        ScenarioBuilder::deployment_with(|t| t.nodes(0).scenario_base_dir(scenario_base_dir));

    for node in external_nodes {
        builder = builder.with_external_node(node);
    }

    let mut scenario = builder
        .with_external_only_sources()
        .inscriptions_with(|inscriptions| {
            inscriptions
                .channels(inscription_channels)
                .inscription_payload_bytes(inscription_payload_bytes)
        })
        .with_run_duration(run_duration)
        .build()?;

    let runner = deployer.deploy(&scenario).await?;
    let _handle = runner.run(&mut scenario).await?;

    Ok(())
}
