pub mod benchmarks;
pub mod common;
pub mod cucumber;
pub mod nodes;
pub mod topology;

use std::{env, sync::LazyLock};

use lb_libp2p::{Multiaddr, PeerId, multiaddr};

/// Global flag indicating whether debug tracing configuration is enabled to
/// send traces to local grafana stack.
pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(|| {
    env::var("LOGOS_BLOCKCHAIN_TESTS_TRACING").is_ok_and(|val| val.eq_ignore_ascii_case("true"))
});

fn node_address_from_port(port: u16) -> Multiaddr {
    multiaddr(std::net::Ipv4Addr::LOCALHOST, port)
}

#[must_use]
pub fn secret_key_to_peer_id(node_key: lb_libp2p::ed25519::SecretKey) -> PeerId {
    PeerId::from_public_key(&lb_libp2p::ed25519::Keypair::from(node_key).public().into())
}

#[must_use]
pub fn secret_key_to_provider_id(
    node_key: lb_libp2p::ed25519::SecretKey,
) -> lb_core::sdp::ProviderId {
    lb_core::sdp::ProviderId::try_from(
        lb_libp2p::ed25519::Keypair::from(node_key)
            .public()
            .to_bytes(),
    )
    .unwrap()
}
