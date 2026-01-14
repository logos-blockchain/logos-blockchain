use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use demo_sequencer::{Transaction, TransferRequest, db::DbError};
use reqwest::{Method, header};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error};

use crate::sequencer::{Sequencer, SequencerError};

pub type AppState = Arc<Sequencer>;

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

fn friendly_error(err: &SequencerError) -> String {
    match err {
        SequencerError::Db(db_err) => match db_err.as_ref() {
            DbError::InsufficientBalance {
                account,
                balance,
                required,
            } => {
                format!("Insufficient balance: {account} has {balance} tokens but needs {required}")
            }
            DbError::SelfTransfer { account } => {
                format!("Cannot transfer to yourself ({account})")
            }
            _ => "Internal database error".to_owned(),
        },
        SequencerError::Timeout => "Transaction timed out waiting for confirmation".to_owned(),
        SequencerError::Serialization(_) => "Invalid transaction data".to_owned(),
        _ => "Internal server error".to_owned(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub account: String,
    pub balance: u64,
    pub confirmed_balance: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transactions: Option<Vec<Transaction>>,
}

#[derive(Debug, Deserialize)]
pub struct AccountQuery {
    #[serde(default)]
    pub tx: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountEntry {
    pub account: String,
    pub balance: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountsResponse {
    pub accounts: Vec<AccountEntry>,
}

/// POST /transfer
/// Request body: { "from": "alice", "to": "bob", "amount": 100 }
async fn transfer(
    State(sequencer): State<AppState>,
    Json(request): Json<TransferRequest>,
) -> impl IntoResponse {
    debug!(
        "API /transfer {} -> {} ({})",
        request.from, request.to, request.amount
    );

    match sequencer.process_transfer(request).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(e) => {
            error!("Transfer failed: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: friendly_error(&e),
                }),
            )
                .into_response()
        }
    }
}

async fn fetch_account_data(
    sequencer: &Sequencer,
    account: &str,
    include_tx: bool,
) -> Result<(u64, u64, Option<Vec<Transaction>>), String> {
    let balance = sequencer
        .get_balance(account)
        .await
        .map_err(|e| e.to_string())?;

    let confirmed_balance = sequencer
        .get_confirmed_balance(account)
        .await
        .map_err(|e| e.to_string())?;

    let transactions = if include_tx {
        Some(
            sequencer
                .get_account_transactions(account)
                .await
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };

    Ok((balance, confirmed_balance, transactions))
}

/// GET /accounts/{account}?tx=true
async fn get_balance(
    State(sequencer): State<AppState>,
    axum::extract::Path(account): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<AccountQuery>,
) -> impl IntoResponse {
    debug!("API /accounts/{}", account);

    match fetch_account_data(&sequencer, &account, query.tx).await {
        Ok((balance, confirmed_balance, transactions)) => (
            StatusCode::OK,
            Json(BalanceResponse {
                account,
                balance,
                confirmed_balance,
                transactions,
            }),
        )
            .into_response(),
        Err(e) => {
            error!("Get account failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: e }),
            )
                .into_response()
        }
    }
}

/// GET /accounts
/// Returns all accounts and their balances
async fn list_accounts(State(sequencer): State<AppState>) -> impl IntoResponse {
    debug!("API /accounts");

    match sequencer.list_accounts().await {
        Ok(accounts) => {
            let accounts = accounts
                .into_iter()
                .map(|(account, balance)| AccountEntry { account, balance })
                .collect();
            (StatusCode::OK, Json(AccountsResponse { accounts })).into_response()
        }
        Err(e) => {
            error!("List accounts failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// GET /health
/// Health check endpoint
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

pub fn create_router(sequencer: Arc<Sequencer>) -> axum::Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    axum::Router::new()
        .route("/transfer", post(transfer))
        .route("/accounts/:account", get(get_balance))
        .route("/accounts", get(list_accounts))
        .route("/health", get(health))
        .with_state(sequencer)
        .layer(cors)
}
