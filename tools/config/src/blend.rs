use std::str::FromStr as _;

use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use lb_libp2p::Multiaddr;
use lb_node::config::blend::serde as blend;
use num_bigint::BigUint;

use crate::kms::key_id_for_preload_backend;

pub type GeneralBlendConfig = (blend::Config, Ed25519Key, ZkKey);

const DEFAULT_BLEND_LISTENING_HOST: &str = "127.0.0.1";

#[must_use]
pub fn create_blend_configs(ids: &[[u8; 32]], ports: &[u16]) -> Vec<GeneralBlendConfig> {
    create_blend_configs_with_listening_host(ids, DEFAULT_BLEND_LISTENING_HOST, ports)
}

#[must_use]
pub fn create_blend_configs_with_listening_host(
    ids: &[[u8; 32]],
    host: &str,
    ports: &[u16],
) -> Vec<GeneralBlendConfig> {
    ids.iter()
        .zip(ports)
        .map(|(id, port)| {
            let private_key = Ed25519Key::from_bytes(id);
            let secret_zk_key =
                ZkKey::from(BigUint::from_bytes_le(private_key.public_key().as_bytes()));
            let mut base_config = blend::Config::with_required_values(blend::RequiredValues {
                non_ephemeral_signing_key_id: key_id_for_preload_backend(
                    &private_key.clone().into(),
                ),
                secret_key_kms_id: key_id_for_preload_backend(&secret_zk_key.clone().into()),
            });
            base_config.core.backend.listening_address =
                Multiaddr::from_str(&format!("/ip4/{host}/udp/{port}/quic-v1")).unwrap();
            (base_config, private_key, secret_zk_key)
        })
        .collect()
}
