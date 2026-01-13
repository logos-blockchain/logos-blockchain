pub mod benchmarks;
pub mod common;
pub mod nodes;
pub mod topology;

use std::{env, ops::Mul as _, sync::LazyLock, time::Duration};

use nomos_core::proofs::leader_proof::POL_PROOF_DEV_MODE;
use nomos_libp2p::{Multiaddr, PeerId, multiaddr};

static IS_SLOW_TEST_ENV: LazyLock<bool> =
    LazyLock::new(|| env::var("SLOW_TEST_ENV").is_ok_and(|s| s == "true"));

pub static GLOBAL_PARAMS_PATH: LazyLock<String> = LazyLock::new(|| {
    let relative_path = "./kzgrs/kzgrs_test_params";
    let current_dir = env::current_dir().expect("Failed to get current directory");
    current_dir
        .join(relative_path)
        .canonicalize()
        .expect("Failed to resolve absolute path")
        .to_string_lossy()
        .to_string()
});

/// Global flag indicating whether debug tracing configuration is enabled to
/// send traces to local grafana stack.
pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(|| {
    env::var("NOMOS_TESTS_TRACING").is_ok_and(|val| val.eq_ignore_ascii_case("true"))
});

/// In slow test environments like Codecov, use 2x timeout.
#[must_use]
pub fn adjust_timeout(d: Duration) -> Duration {
    if *IS_SLOW_TEST_ENV { d.mul(2) } else { d }
}

fn node_address_from_port(port: u16) -> Multiaddr {
    multiaddr(std::net::Ipv4Addr::LOCALHOST, port)
}

#[must_use]
pub fn secret_key_to_peer_id(node_key: nomos_libp2p::ed25519::SecretKey) -> PeerId {
    PeerId::from_public_key(
        &nomos_libp2p::ed25519::Keypair::from(node_key)
            .public()
            .into(),
    )
}

#[must_use]
pub fn secret_key_to_provider_id(
    node_key: nomos_libp2p::ed25519::SecretKey,
) -> nomos_core::sdp::ProviderId {
    nomos_core::sdp::ProviderId::try_from(
        nomos_libp2p::ed25519::Keypair::from(node_key)
            .public()
            .to_bytes(),
    )
    .unwrap()
}

pub fn verify_pol_proof_dev_mode() {
    assert!(
        env::var(POL_PROOF_DEV_MODE).is_ok(),
        "\nError! `POL_PROOF_DEV_MODE` runtime environment variable is required for these tests.\n"
    );
}
