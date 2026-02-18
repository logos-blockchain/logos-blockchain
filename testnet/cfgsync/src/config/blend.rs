use std::str::FromStr as _;

use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use lb_libp2p::Multiaddr;
use lb_node::config::blend::serde as blend;
use lb_tests::common::kms::key_id_for_preload_backend;
use num_bigint::BigUint;

pub type GeneralBlendConfig = (blend::Config, Ed25519Key, ZkKey);

#[must_use]
pub fn create_blend_configs(ids: &[[u8; 32]], ports: &[u16]) -> Vec<GeneralBlendConfig> {
    ids.iter()
        .zip(ports)
        .map(|(id, port)| {
            let private_key = Ed25519Key::from_bytes(id);
            // We need unique ZK secret keys, so we just derive them deterministically from
            // the generated Ed25519 public keys, which are guaranteed to be unique because
            // they are in turned derived from node ID.
            let secret_zk_key =
                ZkKey::from(BigUint::from_bytes_le(private_key.public_key().as_bytes()));
            (
                blend::Config {
                    non_ephemeral_signing_key_id: key_id_for_preload_backend(
                        &private_key.clone().into(),
                    ),
                    core: blend::core::Config {
                        backend: blend::core::BackendConfig {
                            listening_address: Multiaddr::from_str(&format!(
                                "/ip4/0.0.0.0/udp/{port}/quic-v1",
                            ))
                            .unwrap(),
                            ..Default::default()
                        },
                        zk: blend::core::ZkSettings {
                            secret_key_kms_id: key_id_for_preload_backend(
                                &secret_zk_key.clone().into(),
                            ),
                        },
                    },
                    edge: blend::edge::Config {
                        backend: blend::edge::BackendConfig::default(),
                    },
                },
                private_key,
                secret_zk_key,
            )
        })
        .collect()
}
