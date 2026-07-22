use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
};
use lb_groth16::fr_from_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;
use reqwest::StatusCode;
use tokio::sync::mpsc;

pub struct FaucetServerState {
    queue: mpsc::Sender<ZkPublicKey>,
    cooldowns: Mutex<HashMap<ZkPublicKey, Instant>>,
    cooldown: Duration,
}

impl FaucetServerState {
    #[must_use]
    pub fn new(queue: mpsc::Sender<ZkPublicKey>, cooldown: Duration) -> Self {
        Self {
            queue,
            cooldowns: Mutex::new(HashMap::new()),
            cooldown,
        }
    }

    /// Registers a drip attempt for `key`. Returns the remaining cooldown if
    /// the key is still cooling down from a previous request.
    fn try_start_cooldown(&self, key: ZkPublicKey) -> Result<(), Duration> {
        let mut cooldowns = self
            .cooldowns
            .lock()
            .expect("cooldown mutex should not be poisoned");
        let now = Instant::now();
        cooldowns.retain(|_, expires_at| *expires_at > now);

        if let Some(expires_at) = cooldowns.get(&key) {
            return Err(expires_at.duration_since(now));
        }

        cooldowns.insert(key, now + self.cooldown);
        drop(cooldowns);
        Ok(())
    }

    fn clear_cooldown(&self, key: ZkPublicKey) {
        self.cooldowns
            .lock()
            .expect("cooldown mutex should not be poisoned")
            .remove(&key);
    }
}

async fn transfer_funds(
    State(state): State<Arc<FaucetServerState>>,
    Path(key_id): Path<String>,
) -> impl IntoResponse {
    let bytes = match hex::decode(&key_id) {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid hex: {e}")).into_response(),
    };

    let recipient_pk = match fr_from_bytes(&bytes) {
        Ok(fr) => ZkPublicKey::new(fr),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid key format: {e:?}"),
            )
                .into_response();
        }
    };

    if let Err(remaining) = state.try_start_cooldown(recipient_pk) {
        let retry_after_secs = remaining.as_secs().max(1);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({
                "status": "cooldown",
                "retry_after_secs": retry_after_secs,
            })
            .to_string(),
        )
            .into_response();
    }

    if state.queue.try_send(recipient_pk).is_ok() {
        (
            StatusCode::ACCEPTED,
            serde_json::json!({
                "status": "queued",
            })
            .to_string(),
        )
            .into_response()
    } else {
        state.clear_cooldown(recipient_pk);
        (
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({
                "status": "queue_full",
            })
            .to_string(),
        )
            .into_response()
    }
}

pub fn faucet_app(state: Arc<FaucetServerState>) -> Router {
    Router::new()
        .route("/faucet/:pk", post(transfer_funds))
        .with_state(state)
}
