use core::{convert::Infallible, net::SocketAddr};

use axum::{
    Json, Router,
    extract::State,
    response::{Sse, sse::Event},
    routing::get,
    serve,
};
use futures::{Stream, StreamExt as _};
use reqwest::{Method, header};
use tokio::{net::TcpListener, sync::broadcast::Receiver};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};

use crate::{block::ValidatedL2Info, db::BlockStore};

pub struct Server {
    block_receiver_channel: Receiver<ValidatedL2Info>,
    cancellation_token: CancellationToken,
    blocks_db: BlockStore,
}

impl Server {
    pub const fn new(
        block_receiver_channel: Receiver<ValidatedL2Info>,
        cancellation_token: CancellationToken,
        blocks_db: BlockStore,
    ) -> Self {
        Self {
            block_receiver_channel,
            cancellation_token,
            blocks_db,
        }
    }

    pub fn start(self, address: SocketAddr) {
        let (router, cancellation_token) = self.into_router_and_cancellation_token();
        tokio::spawn(async move {
            serve(TcpListener::bind(address).await.unwrap(), router)
                .with_graceful_shutdown(async move {
                    cancellation_token.cancelled().await;
                })
                .await
                .unwrap();
        });
    }

    fn into_router_and_cancellation_token(self) -> (Router, CancellationToken) {
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

        (
            Router::new()
                .route("/block_stream", get(handle_block_stream))
                .route("/blocks", get(handle_get_blocks))
                .with_state(AppState {
                    block_receiver_channel: self.block_receiver_channel,
                    blocks_db: self.blocks_db,
                })
                .layer(cors),
            self.cancellation_token,
        )
    }
}

struct AppState {
    block_receiver_channel: Receiver<ValidatedL2Info>,
    blocks_db: BlockStore,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            block_receiver_channel: self.block_receiver_channel.resubscribe(),
            blocks_db: self.blocks_db.clone(),
        }
    }
}

async fn handle_block_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.block_receiver_channel)
        .map(|block_data_result| block_data_result.unwrap())
        .map(|block_data| serde_json::to_string(&block_data).unwrap())
        .map(|json_serialized_block_data| Ok(Event::default().data(json_serialized_block_data)));

    Sse::new(stream)
}

async fn handle_get_blocks(
    State(state): State<AppState>,
) -> Result<Json<Vec<ValidatedL2Info>>, Infallible> {
    let blocks = state.blocks_db.get_all_blocks().await.unwrap();
    Ok(Json(blocks))
}
