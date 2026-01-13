use core::time::Duration;
use std::{num::NonZeroU64, str::FromStr as _};

use key_management_system_service::keys::{UnsecuredEd25519Key, ZkKey};
use nomos_blend_service::core::settings::ZkSettings;
use nomos_libp2p::Multiaddr;
use nomos_node::config::blend::serde::{
    Config,
    core::{BackendConfig as CoreBackendConfig, Config as CoreConfig},
    edge::{BackendConfig as EdgeBackendConfig, Config as EdgeConfig},
};
use num_bigint::BigUint;

use crate::common::kms::key_id_for_preload_backend;

pub type GeneralBlendConfig = (Config, ZkKey);

#[must_use]
pub fn create_blend_configs(ids: &[[u8; 32]], ports: &[u16]) -> Vec<GeneralBlendConfig> {
    ids.iter()
        .zip(ports)
        .map(|(id, port)| {
            let private_key = UnsecuredEd25519Key::from_bytes(id);
            // We need unique ZK secret keys, so we just derive them deterministically from
            // the generated Ed25519 public keys, which are guaranteed to be unique because
            // they are in turned derived from node ID.
            let secret_zk_key =
                ZkKey::from(BigUint::from_bytes_le(private_key.public_key().as_bytes()));
            (
                Config {
                    non_ephemeral_signing_key: private_key,
                    recovery_path_prefix: "./recovery/blend".into(),
                    core: CoreConfig {
                        backend: CoreBackendConfig {
                            core_peering_degree: 1..=3,
                            edge_node_connection_timeout: Duration::from_secs(1),
                            listening_address: Multiaddr::from_str(&format!(
                                "/ip4/127.0.0.1/udp/{port}/quic-v1",
                            ))
                            .unwrap(),
                            max_dial_attempts_per_peer: NonZeroU64::try_from(3)
                                .expect("Max dial attempts per peer cannot be zero."),
                            max_edge_node_incoming_connections: 300,
                        },
                        zk: ZkSettings {
                            secret_key_kms_id: key_id_for_preload_backend(
                                &secret_zk_key.clone().into(),
                            ),
                        },
                    },
                    edge: EdgeConfig {
                        backend: EdgeBackendConfig {
                            max_dial_attempts_per_peer_per_message: 1.try_into().unwrap(),
                            replication_factor: 1.try_into().unwrap(),
                        },
                    },
                },
                secret_zk_key,
            )
        })
        .collect()
}
