use core::{convert::Infallible, net::SocketAddr};

use axum::{
    Router,
    extract::State,
    response::{Sse, sse::Event},
    routing::get,
    serve,
};
use demo_sequencer::BlockData;
use futures::{Stream, StreamExt as _};
use tokio::{net::TcpListener, sync::broadcast::Receiver};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;

pub struct Server {
    block_receiver_channel: Receiver<BlockData>,
    cancellation_token: CancellationToken,
}

impl Server {
    pub const fn new(
        block_receiver_channel: Receiver<BlockData>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            block_receiver_channel,
            cancellation_token,
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
        (
            Router::new()
                .route("/block_stream", get(handle_block_stream))
                .with_state(AppState {
                    block_receiver_channel: self.block_receiver_channel,
                }),
            self.cancellation_token,
        )
    }
}

struct AppState {
    block_receiver_channel: Receiver<BlockData>,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            block_receiver_channel: self.block_receiver_channel.resubscribe(),
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
