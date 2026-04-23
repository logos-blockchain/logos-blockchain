use std::collections::HashMap;

use lb_node::config::tracing::serde as tracing;

use crate::IS_DEBUG_TRACING;

const LOKI_ENDPOINT: &str = "http://localhost:3100";
const OTLP_TRACING_ENDPOINT: &str = "http://localhost:4317";
const OTLP_METRICS_ENDPOINT: &str = "http://127.0.0.1:9090/api/v1/otlp/v1/metrics";

const DEBUG_TRACING_SAMPLE_RATIO: f64 = 0.5;

#[derive(Clone, Default)]
pub struct GeneralTracingConfig {
    pub tracing_settings: tracing::Config,
}

impl GeneralTracingConfig {
    fn local_debug_tracing(id: usize) -> Self {
        let host_identifier = format!("node-{id}");
        Self {
            tracing_settings: tracing::Config {
                logger: tracing::logger::Layers {
                    otlp: Some(tracing::logger::OtlpConfig {
                        endpoint: LOKI_ENDPOINT.try_into().unwrap(),
                        service_name: host_identifier.clone(),
                        authorization_header: None,
                    }),
                    stdout: true,
                    file: None,
                    gelf: None,
                    loki: None,
                    stderr: false,
                },
                tracing: tracing::tracing::Layer::Otlp(tracing::tracing::OtlpConfig {
                    endpoint: OTLP_TRACING_ENDPOINT.try_into().unwrap(),
                    sample_ratio: DEBUG_TRACING_SAMPLE_RATIO,
                    service_name: host_identifier.clone(),
                    authorization_header: None,
                }),
                filter: tracing::filter::Layer::Env(tracing::filter::EnvConfig {
                    filters: HashMap::from([
                        ("logos_blockchain".to_owned(), tracing::Level::DEBUG),
                        ("libp2p".to_owned(), tracing::Level::DEBUG),
                    ]),
                }),
                metrics: tracing::metrics::Layer::Otlp(tracing::metrics::OtlpConfig {
                    endpoint: OTLP_METRICS_ENDPOINT.try_into().unwrap(),
                    host_identifier,
                    authorization_header: None,
                }),
                console: tracing::console::Layer::None,
                level: tracing::Level::DEBUG,
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
