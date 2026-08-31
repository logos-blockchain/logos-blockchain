use crate::{edge::backends::BlendBackend, service_components::Components as CommonComponents};

/// What an edge node needs on top of [`crate::service_components::Components`].
pub trait Components<RuntimeServiceId>: CommonComponents<RuntimeServiceId> {
    type Backend;
    type ProofsGenerator;
}

/// The deployment settings the edge backend is built from.
pub type EdgeBackendSettingsOf<Edge, RuntimeServiceId> =
    <<Edge as Components<RuntimeServiceId>>::Backend as BlendBackend<
        <Edge as CommonComponents<RuntimeServiceId>>::NodeId,
        RuntimeServiceId,
    >>::Settings;
