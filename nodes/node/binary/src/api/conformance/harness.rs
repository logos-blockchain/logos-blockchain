//! Runs the production HTTP backend against stubbed services.
//!
//! [`AxumBackend::serve`](crate::api::backend::AxumBackend) is started with
//! the node's real `RuntimeServiceId` and type parameters, so the router,
//! handlers and middleware are exactly what a node serves. Only the services
//! the handlers relay to are replaced: [`StubServices`] implements
//! [`Services`] by hand and answers each `request_relay` with a channel whose
//! receiving end is driven by a stub from [`super::stubs`].

use std::net::{Ipv4Addr, SocketAddr, TcpListener};

use async_trait::async_trait;
use lb_api_service::Backend as _;
use lb_core::mantle::transactions::genesis_tx::ChainId;
use lb_http_api_common::settings::AxumBackendSettings;
use overwatch::{
    DynError,
    overwatch::{Error, Overwatch, OverwatchRunner, Services, handle::OverwatchHandle},
    services::{
        lifecycle::LifecycleNotifier,
        relay::{AnyMessage, OutboundRelay},
        status::StatusWatcher,
    },
};
use tokio::runtime::Handle;

use super::stubs;
use crate::{ApiBackend, RuntimeServiceId};

pub const CHAIN_ID: &str = "conformance-test-chain";

pub struct StubServices {
    runtime: Handle,
}

/// Returns a relay whose messages are handed to `respond` on a background
/// task, standing in for the service's own inbound loop.
pub fn relay<Message>(
    runtime: &Handle,
    mut respond: impl FnMut(Message) + Send + 'static,
) -> AnyMessage
where
    Message: Send + 'static,
{
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    runtime.spawn(async move {
        while let Some(message) = receiver.recv().await {
            respond(message);
        }
    });
    Box::new(OutboundRelay::new(sender))
}

#[async_trait]
impl Services for StubServices {
    type Settings = ();
    type RuntimeServiceId = RuntimeServiceId;

    fn new(
        (): Self::Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, DynError> {
        Ok(Self {
            runtime: overwatch_handle.runtime().clone(),
        })
    }

    async fn start(&mut self, _: &RuntimeServiceId) -> Result<(), Error> {
        Ok(())
    }

    async fn start_sequence(&mut self, _: &[RuntimeServiceId]) -> Result<(), Error> {
        Ok(())
    }

    async fn start_all(&mut self) -> Result<(), Error> {
        Ok(())
    }

    async fn stop(&mut self, _: &RuntimeServiceId) -> Result<(), Error> {
        Ok(())
    }

    async fn stop_sequence(&mut self, _: &[RuntimeServiceId]) -> Result<(), Error> {
        Ok(())
    }

    async fn stop_all(&mut self) -> Result<(), Error> {
        Ok(())
    }

    async fn teardown(self) -> Result<(), Error> {
        Ok(())
    }

    fn ids(&self) -> Vec<RuntimeServiceId> {
        Vec::new()
    }

    fn request_relay(&mut self, service_id: &RuntimeServiceId) -> AnyMessage {
        stubs::relay_for(&self.runtime, *service_id)
            .unwrap_or_else(|| panic!("no conformance stub for service {service_id}"))
    }

    fn request_status_watcher(&self, service_id: &RuntimeServiceId) -> StatusWatcher {
        unimplemented!("handlers do not watch service status ({service_id})")
    }

    fn update_settings(&mut self, (): Self::Settings) {}

    fn get_service_lifecycle_notifier(&self, service_id: &RuntimeServiceId) -> &LifecycleNotifier {
        unimplemented!("handlers do not manage service lifecycles ({service_id})")
    }
}

/// A running backend and the Overwatch instance its handlers relay through.
pub struct Node {
    pub base_url: String,
    _overwatch: Overwatch<RuntimeServiceId>,
}

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("ephemeral port has an address")
        .port()
}

impl Node {
    pub async fn start() -> Self {
        let overwatch = OverwatchRunner::<StubServices>::run((), Some(Handle::current()))
            .expect("stub overwatch starts");
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
        let backend = ApiBackend::new(AxumBackendSettings {
            chain_id: ChainId::new_unchecked(CHAIN_ID.to_owned()),
            address,
            cors_origins: Vec::new(),
            timeout: std::time::Duration::from_secs(10),
            max_body_size: lb_http_api_common::settings::default_max_body_size(),
            max_concurrent_requests: 64,
        })
        .await
        .expect("backend settings are valid");
        let handle = overwatch.handle().clone();
        tokio::spawn(async move {
            backend.serve(handle).await.expect("backend serves");
        });

        let node = Self {
            base_url: format!("http://{address}"),
            _overwatch: overwatch,
        };
        node.wait_until_listening(address).await;
        node
    }

    async fn wait_until_listening(&self, address: SocketAddr) {
        for _ in 0..200 {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("backend did not start listening on {address}");
    }
}
