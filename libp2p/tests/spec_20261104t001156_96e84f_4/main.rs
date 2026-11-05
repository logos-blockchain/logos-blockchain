//! Proof for finding `20261104T001156-96e84f/4` (P2P-NAT-SOLUTION, Phase 2/3).
//!
//! Spec, Phase 2: "A Public node must ensure that the port mapping is
//! periodically renewed according to the policy recommended by the port
//! mapping protocol in use." Phase 3: "loop periodically refreshes mapping:
//! Port Mapping Client ->> Router: Requests refresh".
//!
//! `AddressMapperBehaviour` (compiled unchanged from `src/`) is given a
//! mapper that always succeeds and the crate's default `NatMappingSettings`
//! (2 h lease, renewal at 0.8 of it = 96 min). It is then polled every 5 s for
//! the whole lease -- the cadence at which the swarm polls it in production,
//! because the AutoNAT v2 client's default probe tick (5 s) wakes the swarm
//! task. `ActiveState::poll` builds a brand-new renewal timer every time it is
//! polled before the timer fired, so the renewal is postponed forever and the
//! mapping expires at the router without ever being refreshed.
//!
//! Run: `cargo test --locked -p logos-blockchain-libp2p --all-features
//!       --test spec_20261104t001156_96e84f_4`

#[path = "../nat_mirror/mod.rs"]
mod nat_mirror;

pub mod behaviour {
    pub mod nat {
        pub use crate::nat_mirror::{address_mapper, gateway_monitor, inner, state_machine};
    }
}
pub mod config {
    pub use logos_blockchain_libp2p::{GatewaySettings, NatMappingSettings, TraversalSettings};
}
pub use logos_blockchain_libp2p::NatMappingSettings;

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
    time::Duration,
};

use behaviour::nat::address_mapper::{AddressMapperBehaviour, NatMapper, errors::AddressMapperError};
use libp2p::{Multiaddr, swarm::NetworkBehaviour as _};

static MAP_CALLS: AtomicUsize = AtomicUsize::new(0);

/// A gateway that grants every request.
struct AlwaysMaps;

#[async_trait::async_trait]
impl NatMapper for AlwaysMaps {
    async fn map_address(
        _address: &Multiaddr,
        _settings: NatMappingSettings,
    ) -> Result<Multiaddr, AddressMapperError> {
        MAP_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok("/ip4/203.0.113.7/udp/3000/quic-v1".parse().unwrap())
    }
}

async fn poll_once(mapper: &mut AddressMapperBehaviour<AlwaysMaps>) {
    futures::future::poll_fn(|cx| {
        drop(mapper.poll(cx));
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn active_mapping_is_renewed_before_its_lease_expires() {
    let settings = NatMappingSettings::default();
    let lease = settings.lease_duration;
    let mut mapper = AddressMapperBehaviour::<AlwaysMaps>::new(settings);
    mapper
        .try_map_address("/ip4/192.168.1.5/udp/3000/quic-v1".parse().unwrap())
        .unwrap();

    // Swarm-like cadence: poll every 5 s for the full lease.
    let poll_every = Duration::from_secs(5);
    let mut elapsed = Duration::ZERO;
    poll_once(&mut mapper).await;
    while elapsed < lease {
        tokio::time::advance(poll_every).await;
        elapsed += poll_every;
        poll_once(&mut mapper).await;
        poll_once(&mut mapper).await;
    }

    let calls = MAP_CALLS.load(Ordering::SeqCst);
    assert!(
        calls >= 2,
        "P2P-NAT-SOLUTION Phase 2/3 violated: the port mapping was requested once and never \
         refreshed during its whole {lease:?} lease ({calls} mapping request(s) in total) \
         while the behaviour was polled every {poll_every:?}; the renewal timer is re-armed \
         on every poll, so the mapping silently expires at the router."
    );
}
