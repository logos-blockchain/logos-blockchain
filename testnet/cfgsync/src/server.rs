use std::{fs, path::PathBuf, sync::Arc};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use lb_node::config::deployment::WellKnownDeployment;
use lb_tests::nodes::validator::create_validator_config;
use lb_tracing_service::TracingSettings;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use tokio::sync::oneshot::channel;

use crate::{
    Host, RegistrationInfo,
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

async fn init_node(
    State(config_repo): State<Arc<ConfigRepo>>,
    Json(info): Json<RegistrationInfo>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = channel();
    config_repo.register(Host::from(info), reply_tx);

    (reply_rx.await).map_or_else(
        |_| (StatusCode::INTERNAL_SERVER_ERROR, "Error receiving config").into_response(),
        |config_response| match config_response {
            RepoResponse::Config(config) => {
                let config = create_validator_config(*config, WellKnownDeployment::Devnet.into());
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
            let node_config = create_validator_config(cfg, WellKnownDeployment::Devnet.into());
            let yaml = serde_yaml::to_string(&node_config).unwrap_or_default();

            (StatusCode::OK, [(CONTENT_TYPE, "text/yaml")], yaml).into_response()
        },
    )
}

pub fn cfgsync_app(config_repo: Arc<ConfigRepo>) -> Router {
    Router::new()
        .route("/init-with-node", post(init_node))
        .route("/generate-config", post(generate_config))
        .with_state(config_repo)
}
