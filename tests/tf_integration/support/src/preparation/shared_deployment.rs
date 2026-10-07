use std::num::NonZeroU64;

use lb_libp2p::identity::Keypair;
use testing_framework_core::scenario::DynError;

/// Network inputs prepared once and supplied to each implementation's adapter.
///
/// Adapters preserve the chosen genesis, chain parameters and identities while
/// rendering their own configuration. Native node settings and launch details
/// stay with the adapter; they are not part of this shared data.
///
/// Deployment YAML currently uses the Logos representation, rather than an
/// independent protocol schema. Keeping it as data lets adapters consume saved
/// inputs without deserializing them through the current checkout's node types.
/// An adapter must reject settings it cannot represent faithfully.
#[derive(Clone)]
pub struct SharedDeployment {
    // Empty generated deployments have no genesis or deployment settings yet.
    deployment_yaml: Option<String>,
    network_keys: Vec<Keypair>,
    slots_per_epoch: Option<NonZeroU64>,
}

impl SharedDeployment {
    #[must_use]
    pub const fn from_parts(
        deployment_yaml: Option<String>,
        network_keys: Vec<Keypair>,
        slots_per_epoch: Option<NonZeroU64>,
    ) -> Self {
        Self {
            deployment_yaml,
            network_keys,
            slots_per_epoch,
        }
    }

    /// Combines deployment YAML with network identities without interpreting
    /// the node configuration schema. Each adapter validates compatibility.
    #[must_use]
    pub const fn from_yaml(
        deployment_yaml: String,
        network_keys: Vec<Keypair>,
        slots_per_epoch: NonZeroU64,
    ) -> Self {
        Self {
            deployment_yaml: Some(deployment_yaml),
            network_keys,
            slots_per_epoch: Some(slots_per_epoch),
        }
    }

    #[must_use]
    pub const fn node_count(&self) -> usize {
        self.network_keys.len()
    }

    pub fn slots_per_epoch(&self) -> Result<NonZeroU64, DynError> {
        self.slots_per_epoch
            .ok_or_else(|| "shared deployment has no epoch settings".into())
    }

    pub fn network_key(&self, index: usize) -> Result<&Keypair, DynError> {
        self.network_keys
            .get(index)
            .ok_or_else(|| format!("node index {index} exceeds shared deployment capacity").into())
    }

    pub fn deployment_yaml(&self) -> Result<&str, DynError> {
        self.deployment_yaml
            .as_deref()
            .ok_or_else(|| "shared deployment has no genesis settings".into())
    }
}
