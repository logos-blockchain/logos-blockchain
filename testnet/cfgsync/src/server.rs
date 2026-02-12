use std::{fs, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use lb_node::config::TracingConfig;
use lb_tests::nodes::create_validator_config;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use tokio::sync::oneshot::channel;

use crate::{
    FaucetSettings, Host, RegistrationInfo,
    repo::{ConfigRepo, RepoResponse},
};

#[derive(Debug, Deserialize)]
pub struct CfgSyncConfig {
    pub port: u16,
    pub n_hosts: usize,
    pub timeout: u64,

    pub faucet_settings: FaucetSettings,
    // Tracing params
    pub tracing_settings: TracingConfig,
}

impl CfgSyncConfig {
    pub fn load_from_file(file_path: &PathBuf) -> Result<Self, String> {
        let config_content = fs::read_to_string(file_path)
            .map_err(|err| format!("Failed to read config file: {err}"))?;
        serde_yaml::from_str(&config_content)
            .map_err(|err| format!("Failed to parse config file: {err}"))
    }

    #[must_use]
    pub fn tracing_settings(&self) -> TracingConfig {
        self.tracing_settings.clone()
    }

    #[must_use]
    pub fn faucet_settings(&self) -> FaucetSettings {
        self.faucet_settings.clone()
    }
}

async fn init_node(
    State(config_repo): State<Arc<ConfigRepo>>,
    Json(info): Json<RegistrationInfo>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = channel();
    config_repo.register(Host::from(info), reply_tx);

    (reply_rx.await).map_or_else(
        |_| (StatusCode::INTERNAL_SERVER_ERROR, "Error receiving config").into_response(),
        |config_response| match config_response {
            RepoResponse::Config(response) => {
                let (config, deployment_settings) = *response;
                let config = create_validator_config(config, deployment_settings);
                (StatusCode::OK, Json(config)).into_response()
            }
            RepoResponse::Timeout => (StatusCode::REQUEST_TIMEOUT).into_response(),
        },
    )
}

async fn generate_config(
    State(repo): State<Arc<ConfigRepo>>,
    Json(info): Json<RegistrationInfo>,
) -> impl IntoResponse {
    let host = Host::from(info);

    repo.append(host).map_or_else(
        || {
            (
                StatusCode::BAD_REQUEST,
                "Network not initialized. Initial nodes must sync first.",
            )
                .into_response()
        },
        |cfg| {
            let node_config = create_validator_config(cfg, repo.deployment_settings().unwrap());
            let yaml = serde_yaml::to_string(&node_config).unwrap_or_default();

            (StatusCode::OK, [(CONTENT_TYPE, "text/yaml")], yaml).into_response()
        },
    )
}

async fn deployment_settings(State(repo): State<Arc<ConfigRepo>>) -> impl IntoResponse {
    let deployment_settings = repo.deployment_settings();
    let yaml = serde_yaml::to_string(&deployment_settings).unwrap_or_default();
    (StatusCode::OK, [(CONTENT_TYPE, "text/yaml")], yaml).into_response()
}

pub fn cfgsync_app(config_repo: Arc<ConfigRepo>) -> Router {
    Router::new()
        .route("/init-with-node", post(init_node))
        .route("/generate-config", post(generate_config))
        .route("/deployment-settings", get(deployment_settings))
        .with_state(config_repo)
}
