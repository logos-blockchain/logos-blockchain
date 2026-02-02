use std::{fs, net::Ipv4Addr, path::PathBuf, sync::Arc};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use lb_tests::nodes::validator::create_validator_config;
use lb_tracing_service::TracingSettings;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot::channel;

use crate::{
    config::Host,
    repo::{ConfigRepo, RepoResponse},
};

#[derive(Debug, Deserialize)]
pub struct CfgSyncConfig {
    pub port: u16,
    pub n_hosts: usize,
    pub timeout: u64,

    // Tracing params
    pub tracing_settings: TracingSettings,
}

impl CfgSyncConfig {
    pub fn load_from_file(file_path: &PathBuf) -> Result<Self, String> {
        let config_content = fs::read_to_string(file_path)
            .map_err(|err| format!("Failed to read config file: {err}"))?;
        serde_yaml::from_str(&config_content)
            .map_err(|err| format!("Failed to parse config file: {err}"))
    }

    #[must_use]
    pub fn to_tracing_settings(&self) -> TracingSettings {
        self.tracing_settings.clone()
    }
}

#[derive(Serialize, Deserialize)]
pub struct ClientIp {
    pub ip: Ipv4Addr,
    pub identifier: String,
}

#[derive(Serialize, Deserialize)]
pub struct CustomClientIp {
    pub ip: Ipv4Addr,
    pub identifier: String,
    pub network_port: u16,
    pub blend_port: u16,
    pub api_port: u16,
}

async fn default_node_config(
    State(config_repo): State<Arc<ConfigRepo>>,
    Json(payload): Json<ClientIp>,
) -> impl IntoResponse {
    let ClientIp { ip, identifier } = payload;

    let (reply_tx, reply_rx) = channel();
    config_repo.register(Host::default_node_from_ip(ip, identifier), reply_tx);

    (reply_rx.await).map_or_else(
        |_| (StatusCode::INTERNAL_SERVER_ERROR, "Error receiving config").into_response(),
        |config_response| match config_response {
            RepoResponse::Config(config) => {
                let config = create_validator_config(*config);
                (StatusCode::OK, Json(config)).into_response()
            }
            RepoResponse::Timeout => (StatusCode::REQUEST_TIMEOUT).into_response(),
        },
    )
}

async fn custom_node_config(
    State(config_repo): State<Arc<ConfigRepo>>,
    Json(payload): Json<CustomClientIp>,
) -> impl IntoResponse {
    let CustomClientIp {
        ip,
        identifier,
        network_port,
        blend_port,
        api_port,
    } = payload;

    let (reply_tx, reply_rx) = channel();
    config_repo.register(
        Host::custom_node_from_ip(ip, identifier, network_port, blend_port, api_port),
        reply_tx,
    );

    (reply_rx.await).map_or_else(
        |_| (StatusCode::INTERNAL_SERVER_ERROR, "Error receiving config").into_response(),
        |config_response| match config_response {
            RepoResponse::Config(config) => {
                let config = create_validator_config(*config);
                (StatusCode::OK, Json(config)).into_response()
            }
            RepoResponse::Timeout => (StatusCode::REQUEST_TIMEOUT).into_response(),
        },
    )
}

async fn get_node_config(
    State(repo): State<Arc<ConfigRepo>>,
    Json(p): Json<CustomClientIp>,
) -> impl IntoResponse {
    let host =
        Host::custom_node_from_ip(p.ip, p.identifier, p.network_port, p.blend_port, p.api_port);

    repo.append(host).map_or_else(
        || {
            (
                StatusCode::BAD_REQUEST,
                "Network not initialized. Initial nodes must sync first.",
            )
                .into_response()
        },
        |cfg| {
            let node_config = create_validator_config(cfg);
            let yaml = serde_yaml::to_string(&node_config).unwrap_or_default();

            (StatusCode::OK, [(CONTENT_TYPE, "text/yaml")], yaml).into_response()
        },
    )
}

pub fn cfgsync_app(config_repo: Arc<ConfigRepo>) -> Router {
    Router::new()
        .route("/init/default-node", post(default_node_config))
        .route("/init/custom-node", post(custom_node_config))
        .route("/config/custom-node", post(get_node_config))
        .with_state(config_repo)
}
