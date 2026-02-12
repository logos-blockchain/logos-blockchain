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
use time::OffsetDateTime;
use tokio::sync::oneshot::channel;

use crate::{
    CfgsyncMode, FaucetSettings, Host, RegistrationInfo,
    repo::{ConfigRepo, RepoResponse},
};

#[derive(Debug, Deserialize)]
pub struct CfgSyncConfig {
    pub port: u16,
    pub n_hosts: usize,
    pub timeout: u64,
    pub chain_start_time: Option<OffsetDateTime>,
    pub deployment_settings_storage_path: PathBuf,

    pub mode: CfgsyncMode,

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

async fn handle_mode_error() -> (StatusCode, &'static str) {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "Setup is disabled: Server is in Read-Only mode.",
    )
}

async fn deployment_settings(State(repo): State<Arc<ConfigRepo>>) -> impl IntoResponse {
    let deployment_settings = repo.deployment_settings();
    let yaml = serde_yaml::to_string(&deployment_settings).unwrap_or_default();
    (StatusCode::OK, [(CONTENT_TYPE, "text/yaml")], yaml).into_response()
}

pub fn cfgsync_app(config_repo: Arc<ConfigRepo>, mode: CfgsyncMode) -> Router {
    let mut router = Router::new().route("/deployment-settings", get(deployment_settings));

    match mode {
        CfgsyncMode::Setup => {
            router = router.route("/init-with-node", post(init_node));
        }
        CfgsyncMode::Run => {
            router = router.route("/init-with-node", post(handle_mode_error));
        }
    }

    router.with_state(config_repo)
}
