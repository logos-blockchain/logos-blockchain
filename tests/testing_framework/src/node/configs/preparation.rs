use std::num::NonZeroU64;

use lb_config::preparation::PreparedNetwork;
use lb_libp2p::identity::{Keypair, ed25519};
use testing_framework_core::scenario::DynError;

use super::{SharedDeployment, deployment::TopologyConfig, deployment_settings_for_topology};

/// Prepared network material alongside the existing Logos topology settings.
///
/// Reuses the existing topology and protocol material rather than
/// defining a second configuration schema. Other implementations consume
/// `shared_inputs()`; the Logos renderer also uses the retained native options.
#[derive(Clone)]
pub struct PreparedDeployment {
    pub(super) config: TopologyConfig,
    // Empty topologies do not have genesis material yet.
    pub(super) network: Option<PreparedNetwork>,
}

impl PreparedDeployment {
    pub fn shared_inputs(&self) -> Result<SharedDeployment, DynError> {
        let network_keys = self
            .node_ids()
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let key = self.config().node_config_override(index).map_or_else(
                    || ed25519::SecretKey::try_from_bytes(*id).expect("32-byte node identity"),
                    |config| config.user.network.backend.swarm.node_key.clone(),
                );
                Keypair::from(ed25519::Keypair::from(key))
            })
            .collect();

        shared_inputs(&self.config, network_keys)
    }

    #[must_use]
    pub const fn config(&self) -> &TopologyConfig {
        &self.config
    }

    #[must_use]
    pub fn node_ids(&self) -> &[[u8; 32]] {
        self.network.as_ref().map_or(&[], PreparedNetwork::node_ids)
    }

    /// Gives adapters access to genesis and participant secrets, not native
    /// service settings. Empty topologies have no prepared network.
    #[must_use]
    pub const fn network(&self) -> Option<&PreparedNetwork> {
        self.network.as_ref()
    }
}

fn shared_inputs(
    config: &TopologyConfig,
    network_keys: Vec<Keypair>,
) -> Result<SharedDeployment, DynError> {
    let settings = config
        .genesis_block
        .as_ref()
        .map(|genesis| deployment_settings_for_topology(genesis, config));
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

    Ok(SharedDeployment::from_parts(
        deployment_yaml,
        network_keys,
        slots_per_epoch,
    ))
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{ops::OpRef, traits::MantleTx as _};
    use lb_key_management_system_service::keys::Key;
    use lb_libp2p::identity::ed25519;

    use crate::{
        DeploymentBuilder, TopologyConfig,
        configs::{
            build_node_run_config, build_plan,
            wallet::{WalletAccount, WalletConfig},
        },
    };

    #[test]
    fn prepared_network_survives_native_rendering() {
        let wallet = WalletAccount::deterministic(0, 500_000, false).unwrap();
        let prepared = DeploymentBuilder::new(TopologyConfig::with_node_numbers(2))
            .with_wallet_config(WalletConfig::new(vec![wallet.clone()]))
            .prepare()
            .unwrap();
        let shared = prepared.shared_inputs().unwrap();
        let network = prepared.network().unwrap();
        let genesis_id = network.genesis.header().id();
        let transfer = network.genesis.genesis_tx().transfer().operation();
        assert_eq!(
            transfer
                .outputs
                .iter()
                .find(|note| note.pk == wallet.public_key())
                .unwrap()
                .value,
            wallet.value
        );
        for key in &network.consensus.regular_note_keys {
            assert_eq!(
                transfer
                    .outputs
                    .iter()
                    .find(|note| note.pk == key.to_public_key())
                    .unwrap()
                    .value,
                2_500_000
            );
        }
        let declarations = network
            .genesis
            .genesis_tx()
            .op_refs()
            .into_iter()
            .filter_map(|op| match op {
                OpRef::SDPDeclare(declaration) => {
                    Some((declaration.id(), declaration.service_note_id))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(declarations.len(), 2);
        for (i, (_, note_id)) in declarations.iter().enumerate() {
            assert_eq!(*note_id, network.consensus.blend_notes[i].note_id);
        }

        let plan = build_plan(prepared).unwrap();
        assert_eq!(
            plan.config().genesis_block.as_ref().unwrap().header().id(),
            genesis_id
        );
        assert_eq!(shared.node_count(), 2);

        for (i, node) in plan.nodes().iter().enumerate() {
            let config = build_node_run_config(&plan, node, None).unwrap();
            let expected =
                ed25519::Keypair::from(config.user.network.backend.swarm.node_key.clone());
            assert_eq!(
                shared.network_key(i).unwrap().public(),
                expected.public().into()
            );
            assert_eq!(
                shared.slots_per_epoch().unwrap().get(),
                config
                    .deployment
                    .genesis_era_parameters()
                    .cryptarchia
                    .slots_per_epoch()
            );
            assert_eq!(
                shared.deployment_yaml().unwrap(),
                serde_yaml::to_string(&config.deployment).unwrap()
            );
            assert_eq!(
                node.general.sdp_config.declaration_id,
                Some(declarations[i].0)
            );
            let wallet_key = Key::Zk(wallet.secret_key.clone());
            let wallet_key_id = lb_config::kms::key_id_for_preload_backend(&wallet_key);
            assert!(
                node.general
                    .kms_config
                    .backend
                    .keys
                    .contains_key(&wallet_key_id)
            );
        }
    }
}
