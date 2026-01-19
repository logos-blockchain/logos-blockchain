use lb_tracing::{
    logging::loki::LokiConfig, metrics::otlp::OtlpMetricsConfig, tracing::otlp::OtlpTracingConfig,
};
use lb_tracing_service::{
    ConsoleLayer, FilterLayer, LoggerLayer, MetricsLayer, TracingLayer, TracingSettings,
};
use tracing::Level;

use crate::IS_DEBUG_TRACING;

#[derive(Clone, Default)]
pub struct GeneralTracingConfig {
    pub tracing_settings: TracingSettings,
}

impl GeneralTracingConfig {
    fn local_debug_tracing(id: usize) -> Self {
        let host_identifier = format!("node-{id}");
        Self {
            tracing_settings: TracingSettings {
                logger: LoggerLayer::Loki(LokiConfig {
                    endpoint: "http://localhost:3100".try_into().unwrap(),
                    host_identifier: host_identifier.clone(),
                }),
                tracing: TracingLayer::Otlp(OtlpTracingConfig {
                    endpoint: "http://localhost:4317".try_into().unwrap(),
                    sample_ratio: 0.5,
                    service_name: host_identifier.clone(),
                }),
                filter: FilterLayer::EnvFilter(lb_tracing::filter::envfilter::EnvFilterConfig {
                    // Allow events only from modules that matches the regex, if it matches - use
                    // provided tracing level. Libp2p related crates are very log intensive in debug
                    // mode.
                    filters: std::iter::once(&("logos-blockchain", "debug"))
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                }),
                metrics: MetricsLayer::Otlp(OtlpMetricsConfig {
                    endpoint: "http://127.0.0.1:9090/api/v1/otlp/v1/metrics"
                        .try_into()
                        .unwrap(),
                    host_identifier,
                }),
                console: ConsoleLayer::None,
                level: Level::DEBUG,
            },
        }
    }
}

#[must_use]
pub fn create_tracing_configs(ids: &[[u8; 32]]) -> Vec<GeneralTracingConfig> {
    if *IS_DEBUG_TRACING {
        create_debug_configs(ids)
    } else {
        create_default_configs(ids)
    }
}

fn create_debug_configs(ids: &[[u8; 32]]) -> Vec<GeneralTracingConfig> {
    ids.iter()
        .enumerate()
        .map(|(i, _)| GeneralTracingConfig::local_debug_tracing(i))
        .collect()
}

fn create_default_configs(ids: &[[u8; 32]]) -> Vec<GeneralTracingConfig> {
    ids.iter()
        .map(|_| GeneralTracingConfig::default())
        .collect()
}
