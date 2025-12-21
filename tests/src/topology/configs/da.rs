use std::{
    collections::HashSet, env, path::PathBuf, str::FromStr as _, sync::LazyLock, time::Duration,
};

use key_management_system_service::keys::{Ed25519Key, ZkKey};
use nomos_da_network_core::swarm::{
    DAConnectionMonitorSettings, DAConnectionPolicySettings, ReplicationConfig,
};
use nomos_libp2p::{Multiaddr, PeerId, ed25519};
use nomos_node::NomosDaMembership;
use num_bigint::BigUint;
use subnetworks_assignations::MembershipHandler as _;

use crate::secret_key_to_peer_id;

pub static GLOBAL_PARAMS_PATH: LazyLock<String> = LazyLock::new(|| {
    let relative_path = "./kzgrs/kzgrs_test_params";
    let current_dir = env::current_dir().expect("Failed to get current directory");
    current_dir
        .join(relative_path)
        .canonicalize()
        .expect("Failed to resolve absolute path")
        .to_string_lossy()
        .to_string()
});

#[derive(Clone)]
pub struct DaParams {
    pub subnetwork_size: usize,
    pub dispersal_factor: usize,
    pub num_samples: u16,
    pub num_subnets: u16,
    pub old_blobs_check_interval: Duration,
    pub blobs_validity_duration: Duration,
    pub global_params_path: String,
    pub policy_settings: DAConnectionPolicySettings,
    pub monitor_settings: DAConnectionMonitorSettings,
    pub balancer_interval: Duration,
    pub redial_cooldown: Duration,
    pub replication_settings: ReplicationConfig,
    pub subnets_refresh_interval: Duration,
    pub retry_shares_limit: usize,
    pub retry_commitments_limit: usize,
}

impl Default for DaParams {
    fn default() -> Self {
        Self {
            subnetwork_size: 2,
            dispersal_factor: 1,
            num_samples: 1,
            num_subnets: 2,
            old_blobs_check_interval: Duration::from_secs(5),
            blobs_validity_duration: Duration::from_secs(60),
            global_params_path: GLOBAL_PARAMS_PATH.to_string(),
            policy_settings: DAConnectionPolicySettings {
                min_dispersal_peers: 1,
                min_replication_peers: 1,
                max_dispersal_failures: 0,
                max_sampling_failures: 0,
                max_replication_failures: 0,
                malicious_threshold: 0,
            },
            monitor_settings: DAConnectionMonitorSettings {
                failure_time_window: Duration::from_secs(5),
                ..Default::default()
            },
            balancer_interval: Duration::from_secs(1),
            redial_cooldown: Duration::ZERO,
            replication_settings: ReplicationConfig {
                seen_message_cache_size: 1000,
                seen_message_ttl: Duration::from_secs(3600),
            },
            subnets_refresh_interval: Duration::from_secs(30),
            retry_shares_limit: 1,
            retry_commitments_limit: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeneralDaConfig {
    pub node_key: ed25519::SecretKey,
    pub signer: Ed25519Key,
    pub peer_id: PeerId,
    pub membership: NomosDaMembership,
    pub listening_address: Multiaddr,
    pub blob_storage_directory: PathBuf,
    pub global_params_path: String,
    pub verifier_sk: String,
    pub verifier_index: HashSet<u16>,
    pub num_samples: u16,
    pub num_subnets: u16,
    pub old_blobs_check_interval: Duration,
    pub blobs_validity_duration: Duration,
    pub policy_settings: DAConnectionPolicySettings,
    pub monitor_settings: DAConnectionMonitorSettings,
    pub balancer_interval: Duration,
    pub redial_cooldown: Duration,
    pub replication_settings: ReplicationConfig,
    pub subnets_refresh_interval: Duration,
    pub retry_shares_limit: usize,
    pub retry_commitments_limit: usize,
    pub secret_zk_key: ZkKey,
}

#[must_use]
pub fn create_da_configs(
    ids: &[[u8; 32]],
    da_params: &DaParams,
    ports: &[u16],
) -> Vec<GeneralDaConfig> {
    let mut node_keys = vec![];
    let mut peer_ids = vec![];
    let mut listening_addresses = vec![];

    for (i, id) in ids.iter().enumerate() {
        let mut node_key_bytes = *id;
        let node_key = ed25519::SecretKey::try_from_bytes(&mut node_key_bytes)
            .expect("Failed to generate secret key from bytes");
        node_keys.push(node_key.clone());

        let peer_id = secret_key_to_peer_id(node_key);
        peer_ids.push(peer_id);

        let listening_address =
            Multiaddr::from_str(&format!("/ip4/127.0.0.1/udp/{}/quic-v1", ports[i],))
                .expect("Failed to create multiaddr");
        listening_addresses.push(listening_address);
    }

    let membership =
        NomosDaMembership::new(0, da_params.subnetwork_size, da_params.dispersal_factor);

    ids.iter()
        .zip(node_keys)
        .enumerate()
        .map(|(i, (id, node_key))| {
            let blob_storage_directory = PathBuf::from(format!("/tmp/blob_storage_{i}"));
            let verifier_sk = blst::min_sig::SecretKey::key_gen(id, &[]).unwrap();
            let verifier_sk_bytes = verifier_sk.to_bytes();
            let peer_id = peer_ids[i];
            let signer = Ed25519Key::from_bytes(id);
            let subnetwork_ids = membership.membership(&peer_id);

            // We need unique ZK secret keys, so we just derive them deterministically from
            // the generated Ed25519 public keys, which are guaranteed to be unique because
            // they are in turned derived from node ID.
            let secret_zk_key = ZkKey::from(BigUint::from_bytes_le(signer.public_key().as_bytes()));

            GeneralDaConfig {
                node_key,
                signer,
                peer_id,
                secret_zk_key,
                membership: membership.clone(),
                listening_address: listening_addresses[i].clone(),
                blob_storage_directory,
                global_params_path: da_params.global_params_path.clone(),
                verifier_sk: hex::encode(verifier_sk_bytes),
                verifier_index: subnetwork_ids,
                num_samples: da_params.num_samples,
                num_subnets: da_params.num_subnets,
                old_blobs_check_interval: da_params.old_blobs_check_interval,
                blobs_validity_duration: da_params.blobs_validity_duration,
                policy_settings: da_params.policy_settings.clone(),
                monitor_settings: da_params.monitor_settings.clone(),
                balancer_interval: da_params.balancer_interval,
                redial_cooldown: da_params.redial_cooldown,
                replication_settings: da_params.replication_settings,
                subnets_refresh_interval: da_params.subnets_refresh_interval,
                retry_shares_limit: da_params.retry_shares_limit,
                retry_commitments_limit: da_params.retry_commitments_limit,
            }
        })
        .collect()
}
