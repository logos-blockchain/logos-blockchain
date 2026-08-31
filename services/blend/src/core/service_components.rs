use lb_utils::blake_rng::BlakeRng;

use crate::{core::backends::BlendBackend, service_components::Components as CommonComponents};

/// What a core node needs on top of [`Components`].
///
/// A core node is the only one that listens, decapsulates, holds core quota and
/// releases on a schedule, so the backend, the verifier, the core `PoQ`
/// generator and the rng are all its own. `NodeId` and `Dispatcher` come from
/// the supertrait: the service owns those and hands them to whichever mode is
/// running.
///
/// Unbounded for the same reason as [`Components`] — see there.
pub trait Components<RuntimeServiceId>: CommonComponents<RuntimeServiceId> {
    /// Drives release-round jitter and message shuffling. Core-only: no other
    /// mode schedules anything.
    type Rng;
    /// Checks the proofs on an incoming message. Core-only: no other mode
    /// receives one.
    type ProofsVerifier;
    /// Mints this node's core-quota proofs of quota.
    type CorePoQGenerator;
    /// The libp2p behaviour a core node listens and blends with.
    type Backend;
    /// Supplies a core node's proofs across all three quota branches.
    type ProofsGenerator;
}

/// The deployment settings the core backend is built from.
pub type BackendSettingsOf<Core, RuntimeServiceId> =
    <<Core as Components<RuntimeServiceId>>::Backend as BlendBackend<
        <Core as CommonComponents<RuntimeServiceId>>::NodeId,
        BlakeRng,
        <Core as Components<RuntimeServiceId>>::ProofsVerifier,
        RuntimeServiceId,
    >>::Settings;
