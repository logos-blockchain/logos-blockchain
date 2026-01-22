use std::{fs, net::Ipv4Addr, path::PathBuf, sync::Arc};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use lb_tests::nodes::validator::create_validator_config;
use lb_tracing_service::TracingSettings;
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

async fn validator_config(
    State(config_repo): State<Arc<ConfigRepo>>,
    Json(payload): Json<ClientIp>,
) -> impl IntoResponse {
    let ClientIp { ip, identifier } = payload;

    let (reply_tx, reply_rx) = channel();
    config_repo.register(Host::default_validator_from_ip(ip, identifier), reply_tx);

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

pub fn cfgsync_app(config_repo: Arc<ConfigRepo>) -> Router {
    Router::new()
        .route("/validator", post(validator_config))
        .with_state(config_repo)
}
