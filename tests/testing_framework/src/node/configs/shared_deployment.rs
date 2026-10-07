use std::num::NonZeroU64;

use lb_libp2p::identity::{Keypair, ed25519};
use testing_framework_core::scenario::DynError;

use super::deployment_settings_for_topology;
use crate::node::DeploymentPlan;

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

    /// Extracts shared inputs while leaving the typed Logos plan available to
    /// its adapter. Node overrides supply the effective network identities.
    pub fn from_plan(plan: &DeploymentPlan) -> Result<Self, DynError> {
        let settings = plan
            .config()
            .genesis_block
            .as_ref()
            .map(|genesis| deployment_settings_for_topology(genesis, plan.config()));
        let deployment_yaml = settings.as_ref().map(serde_yaml::to_string).transpose()?;
        let slots_per_epoch = settings
            .as_ref()
            .map(|settings| {
                NonZeroU64::new(
                    settings
                        .genesis_era_parameters()
                        .cryptarchia
                        .slots_per_epoch(),
                )
                .ok_or("shared deployment has zero slots per epoch")
            })
            .transpose()?;

        let network_keys = plan
            .nodes()
            .iter()
            .map(|node| {
                let key = plan.config().node_config_override(node.index()).map_or(
                    &node.general.network_config.backend.swarm.node_key,
                    |config| &config.user.network.backend.swarm.node_key,
                );
                Keypair::from(ed25519::Keypair::from(key.clone()))
            })
            .collect();

        Ok(Self {
            deployment_yaml,
            network_keys,
            slots_per_epoch,
        })
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

#[cfg(test)]
mod tests {
    use lb_libp2p::identity::ed25519;

    use super::SharedDeployment;
    use crate::{DeploymentBuilder, TopologyConfig, configs::build_node_run_config};

    #[test]
    fn generated_shared_inputs_match_native_configuration() {
        let plan =
            DeploymentBuilder::new(TopologyConfig::with_node_numbers(1).with_blend_core_nodes(0))
                .build()
                .unwrap();
        let config = build_node_run_config(&plan, &plan.nodes()[0], None).unwrap();
        let expected = ed25519::Keypair::from(config.user.network.backend.swarm.node_key.clone());

        let shared = SharedDeployment::from_plan(&plan).unwrap();
        assert_eq!(shared.node_count(), 1);
        assert_eq!(
            shared.slots_per_epoch().unwrap().get(),
            config
                .deployment
                .genesis_era_parameters()
                .cryptarchia
                .slots_per_epoch(),
        );
        assert_eq!(
            shared.network_key(0).unwrap().public(),
            expected.public().into()
        );
        assert_eq!(
            shared.deployment_yaml().unwrap(),
            serde_yaml::to_string(&config.deployment).unwrap(),
        );
    }
}
