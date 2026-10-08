use std::{
    collections::{BTreeMap, btree_map::Entry},
    num::NonZero,
};

use blockchain_test_support::runtime_info::{
    NodeRuntimeInfo, NodeRuntimeInfoProvider, NodeWalletKey, NodeWalletKeyRole,
};
use hex::ToHex as _;
use lb_binary_codec::bincode::SerializeOp as _;
use lb_libp2p::identity::{Keypair, ed25519};
use lb_node::{UserConfig, config::RunConfig};
use testing_framework_core::scenario::DynError;

use crate::LbcEnv;

impl NodeRuntimeInfoProvider for LbcEnv {
    fn runtime_info(config: &RunConfig) -> Result<NodeRuntimeInfo, DynError> {
        let key = Keypair::from(ed25519::Keypair::from(
            config.user.network.backend.swarm.node_key.clone(),
        ));

        Ok(NodeRuntimeInfo {
            peer_id: key.public().to_peer_id(),
            slots_per_epoch: NonZero::new(
                config
                    .deployment
                    .genesis_era_parameters()
                    .cryptarchia
                    .slots_per_epoch(),
            )
            .ok_or("deployment has zero slots per epoch")?,
            wallets: node_wallet_keys_from_config(&config.user)?,
        })
    }
}

/// Classifies the node-owned wallet keys in deterministic order.
///
/// Every public key is returned once even if more than one configured key id
/// points to it.
fn node_wallet_keys_from_config(config: &UserConfig) -> Result<Vec<NodeWalletKey>, DynError> {
    let cryptarchia_funding_pk = config.cryptarchia.leader.wallet.funding_pk;
    let sdp_funding_pk = config.sdp.wallet.funding_pk;
    let voucher_master_key_id = config.wallet.voucher_master_key_id.clone();
    let blend_zk_key_id = config.blend.core.zk.secret_key_kms_id.clone();
    let mut keys_by_public_key = BTreeMap::<String, NodeWalletKey>::new();

    for (key_id, public_key) in &config.wallet.known_keys {
        let wallet_pk = public_key.to_bytes()?.encode_hex::<String>();
        let role = if *public_key == cryptarchia_funding_pk || *public_key == sdp_funding_pk {
            NodeWalletKeyRole::Funding
        } else if key_id == &voucher_master_key_id {
            NodeWalletKeyRole::VoucherMaster
        } else if key_id == &blend_zk_key_id {
            NodeWalletKeyRole::BlendZk
        } else {
            NodeWalletKeyRole::General
        };

        match keys_by_public_key.entry(wallet_pk.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(NodeWalletKey { wallet_pk, role });
            }
            Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                if existing.role == role || role == NodeWalletKeyRole::General {
                    continue;
                }
                if existing.role == NodeWalletKeyRole::General {
                    existing.role = role;
                    continue;
                }
                return Err(format!(
                    "Node wallet public key '{}' has conflicting roles {:?} and {role:?}",
                    existing.wallet_pk, existing.role,
                )
                .into());
            }
        }
    }

    let mut node_wallet_keys = keys_by_public_key.into_values().collect::<Vec<_>>();
    for role in [
        NodeWalletKeyRole::Funding,
        NodeWalletKeyRole::VoucherMaster,
        NodeWalletKeyRole::BlendZk,
    ] {
        let count = node_wallet_keys
            .iter()
            .filter(|key| key.role == role)
            .count();
        if count != 1 {
            return Err(
                format!("Expected exactly one {role:?} node wallet key, found {count}").into(),
            );
        }
    }

    node_wallet_keys.sort_by(|left, right| {
        left.role
            .priority()
            .cmp(&right.role.priority())
            .then_with(|| left.wallet_pk.cmp(&right.wallet_pk))
    });
    Ok(node_wallet_keys)
}
