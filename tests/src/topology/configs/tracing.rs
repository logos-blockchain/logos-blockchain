use lb_node::config::tracing::serde as tracing;

use crate::IS_DEBUG_TRACING;

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
                        endpoint: "http://localhost:3100".try_into().unwrap(),
                        service_name: host_identifier.clone(),
                    }),
                    stdout: true,
                    file: None,
                    gelf: None,
                    loki: None,
                    stderr: false,
                },
                tracing: tracing::tracing::Layer::Otlp(tracing::tracing::OtlpConfig {
                    endpoint: "http://localhost:4317".try_into().unwrap(),
                    sample_ratio: 0.5,
                    service_name: host_identifier.clone(),
                }),
                filter: tracing::filter::Layer::Env(tracing::filter::EnvConfig {
                    // Allow events only from modules that matches the regex, if it matches -
                    // use provided tracing level. Libp2p related crates
                    // are very log intensive in debug mode.
                    filters: std::iter::once(&("logos-blockchain", "debug"))
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                }),
                metrics: tracing::metrics::Layer::Otlp(tracing::metrics::OtlpConfig {
                    endpoint: "http://127.0.0.1:9090/api/v1/otlp/v1/metrics"
                        .try_into()
                        .unwrap(),
                    host_identifier,
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
