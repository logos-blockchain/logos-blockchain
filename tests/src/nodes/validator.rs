use std::{
    collections::HashSet,
    net::SocketAddr,
    process::{Child, Command, Stdio},
    str::FromStr as _,
    time::Duration,
};

use broadcast_service::BlockInfo;
use chain_service::CryptarchiaInfo;
use common_http_client::CommonHttpClient;
use futures::Stream;
use kzgrs_backend::common::share::{DaLightShare, DaShare, DaSharesCommitments};
use nomos_core::{
    block::Block,
    da::BlobId,
    mantle::{SignedMantleTx, Transaction as _, TxHash},
    sdp::{Declaration, SessionNumber},
};
use nomos_da_network_core::{
    protocols::sampling::SubnetsConfig,
    swarm::{BalancerStats, DAConnectionPolicySettings, MonitorStats},
};
use nomos_da_network_service::{
    MembershipResponse, NetworkConfig as DaNetworkConfig, api::http::ApiAdapterSettings,
    backends::libp2p::common::DaNetworkBackendSettings,
};
use nomos_da_sampling::{
    DaSamplingServiceSettings, backend::kzgrs::KzgrsSamplingBackendSettings,
    verifier::kzgrs::KzgrsDaVerifierSettings as SamplingVerifierSettings,
};
use nomos_da_verifier::{
    DaVerifierServiceSettings,
    backend::{kzgrs::KzgrsDaVerifierSettings, trigger::MempoolPublishTriggerConfig},
    storage::adapters::rocksdb::RocksAdapterSettings as VerifierStorageAdapterSettings,
};
use nomos_http_api_common::paths::{
    CRYPTARCHIA_HEADERS, CRYPTARCHIA_INFO, DA_BALANCER_STATS, DA_GET_MEMBERSHIP,
    DA_GET_SHARES_COMMITMENTS, DA_HISTORIC_SAMPLING, DA_MONITOR_STATS, MANTLE_SDP_DECLARATIONS,
    NETWORK_INFO, STORAGE_BLOCK,
};
use nomos_network::backends::libp2p::Libp2pInfo;
use nomos_node::{
    Config, HeaderId, RocksBackendSettings,
    api::{
        backend::AxumBackendSettings, handlers::GetCommitmentsRequest,
        testing::handlers::HistoricSamplingRequest,
    },
    config::mempool::serde::Config as MempoolConfig,
};
use nomos_sdp::SdpSettings;
use nomos_tracing::logging::local::FileConfig;
use nomos_tracing_service::LoggerLayer;
use nomos_utils::{math::NonNegativeF64, net::get_available_tcp_port};
use nomos_wallet::WalletServiceSettings;
use reqwest::Url;
use tempfile::NamedTempFile;
use tokio::time::error::Elapsed;
use tx_service::MempoolMetrics;

use super::{CLIENT, create_tempdir, get_exe_path, persist_tempdir};
use crate::{
    IS_DEBUG_TRACING, adjust_timeout,
    nodes::LOGS_PREFIX,
    topology::configs::{GeneralConfig, deployment::default_e2e_deployment_settings},
};

const BIN_PATH_DEBUG: &str = "../target/debug/nomos-node";
const BIN_PATH_RELEASE: &str = "../target/release/nomos-node";

pub enum Pool {
    Da,
    Mantle,
}

pub struct Validator {
    addr: SocketAddr,
    testing_http_addr: SocketAddr,
    tempdir: tempfile::TempDir,
    child: Child,
    config: Config,
    http_client: CommonHttpClient,
}

impl Drop for Validator {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Err(e) = persist_tempdir(&mut self.tempdir, "nomos-node")
        {
            println!("failed to persist tempdir: {e}");
        }

        if let Err(e) = self.child.kill() {
            println!("failed to kill the child process: {e}");
        }
    }
}

impl Validator {
    /// Check if the validator process is still running
    pub fn is_running(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => false,
        }
    }

    /// Wait for the validator process to exit, with a timeout
    /// Returns true if the process exited within the timeout, false otherwise
    pub async fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                if !self.is_running() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .is_ok()
    }

    pub async fn spawn(mut config: Config) -> Result<Self, Elapsed> {
        let dir = create_tempdir().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        let config_path = file.path().to_owned();

        if !*IS_DEBUG_TRACING {
            // setup logging so that we can intercept it later in testing
            config.tracing.logger = LoggerLayer::File(FileConfig {
                directory: dir.path().to_owned(),
                prefix: Some(LOGS_PREFIX.into()),
            });
        }

        config.storage.db_path = dir.path().join("db");
        dir.path().clone_into(
            &mut config
                .da_verifier
                .storage_adapter_settings
                .blob_storage_directory,
        );

        serde_yaml::to_writer(&mut file, &config).unwrap();
        let exe_path = get_exe_path(BIN_PATH_DEBUG, BIN_PATH_RELEASE);
        let child = Command::new(exe_path)
            .arg(&config_path)
            .current_dir(dir.path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let node = Self {
            addr: config.http.backend_settings.address,
            testing_http_addr: config.testing_http.backend_settings.address,
            child,
            tempdir: dir,
            config,
            http_client: CommonHttpClient::new_with_client(CLIENT.clone(), None),
        };

        tokio::time::timeout(adjust_timeout(Duration::from_secs(10)), async {
            node.wait_online().await;
        })
        .await?;

        Ok(node)
    }

    async fn get(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        CLIENT
            .get(format!("http://{}{}", self.addr, path))
            .send()
            .await
    }

    #[must_use]
    pub fn url(&self) -> Url {
        format!("http://{}", self.addr).parse().unwrap()
    }

    async fn wait_online(&self) {
        loop {
            let res = self.get(CRYPTARCHIA_INFO).await;
            if res.is_ok() && res.unwrap().status().is_success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn get_block(&self, id: HeaderId) -> Option<Block<SignedMantleTx>> {
        CLIENT
            .post(format!("http://{}{}", self.addr, STORAGE_BLOCK))
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&id).unwrap())
            .send()
            .await
            .unwrap()
            .json::<Option<Block<SignedMantleTx>>>()
            .await
            .unwrap()
    }

    pub async fn get_commitments(
        &self,
        blob_id: BlobId,
        session: SessionNumber,
    ) -> Option<DaSharesCommitments> {
        let request = GetCommitmentsRequest { blob_id, session };

        CLIENT
            .post(format!("http://{}{}", self.addr, DA_GET_SHARES_COMMITMENTS))
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&request).unwrap())
            .send()
            .await
            .unwrap()
            .json::<Option<DaSharesCommitments>>()
            .await
            .unwrap()
    }

    pub async fn get_mempoool_metrics(&self, pool: Pool) -> MempoolMetrics {
        let discr = match pool {
            Pool::Mantle => "mantle",
            Pool::Da => "da",
        };
        let addr = format!("/{discr}/metrics");
        let res = self
            .get(&addr)
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        MempoolMetrics {
            pending_items: res["pending_items"].as_u64().unwrap() as usize,
            last_item_timestamp: res["last_item_timestamp"].as_u64().unwrap(),
        }
    }

    pub async fn get_sdp_declarations(&self) -> Vec<Declaration> {
        CLIENT
            .get(format!(
                "http://{}{}",
                self.testing_http_addr, MANTLE_SDP_DECLARATIONS
            ))
            .send()
            .await
            .expect("Failed to fetch SDP declarations")
            .json::<Vec<Declaration>>()
            .await
            .expect("Failed to deserialize SDP declarations response")
    }

    pub async fn da_historic_sampling(
        &self,
        block_id: HeaderId,
        blob_ids: Vec<(BlobId, SessionNumber)>,
    ) -> Result<bool, reqwest::Error> {
        let request = HistoricSamplingRequest { block_id, blob_ids };

        let response = CLIENT
            .post(format!(
                "http://{}{}",
                self.testing_http_addr, DA_HISTORIC_SAMPLING
            ))
            .json(&request)
            .send()
            .await?;

        response.error_for_status_ref()?;

        // Parse the boolean response
        let success: bool = response.json().await?;
        Ok(success)
    }

    // not async so that we can use this in `Drop`
    #[must_use]
    pub fn get_logs_from_file(&self) -> String {
        println!(
            "fetching logs from dir {}...",
            self.tempdir.path().display()
        );
        // std::thread::sleep(std::time::Duration::from_secs(50));
        std::fs::read_dir(self.tempdir.path())
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path();
                (path.is_file() && path.to_str().unwrap().contains(LOGS_PREFIX)).then_some(path)
            })
            .map(|f| std::fs::read_to_string(f).unwrap())
            .collect::<String>()
    }

    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    pub async fn get_headers(
        &self,
        from: Option<HeaderId>,
        to: Option<HeaderId>,
        print: bool,
    ) -> Vec<HeaderId> {
        let mut req = CLIENT.get(format!("http://{}{}", self.addr, CRYPTARCHIA_HEADERS));

        if let Some(from) = from {
            req = req.query(&[("from", from)]);
        }

        if let Some(to) = to {
            req = req.query(&[("to", to)]);
        }

        let res = req.send().await;

        if print {
            println!("res: {res:?}");
        }

        res.unwrap().json::<Vec<HeaderId>>().await.unwrap()
    }

    pub async fn consensus_info(&self, print: bool) -> CryptarchiaInfo {
        let res = self.get(CRYPTARCHIA_INFO).await;
        if print {
            println!("{res:?}");
        }
        res.unwrap().json().await.unwrap()
    }

    pub async fn balancer_stats(&self) -> BalancerStats {
        self.get(DA_BALANCER_STATS)
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    pub async fn monitor_stats(&self) -> MonitorStats {
        self.get(DA_MONITOR_STATS)
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    pub async fn da_get_membership(
        &self,
        session_id: SessionNumber,
    ) -> Result<MembershipResponse, reqwest::Error> {
        let response = CLIENT
            .post(format!(
                "http://{}{}",
                self.testing_http_addr, DA_GET_MEMBERSHIP
            ))
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&session_id).unwrap())
            .send()
            .await?;

        response.error_for_status()?.json().await
    }

    pub async fn network_info(&self) -> Libp2pInfo {
        self.get(NETWORK_INFO).await.unwrap().json().await.unwrap()
    }

    pub async fn get_shares(
        &self,
        blob_id: BlobId,
        requested_shares: HashSet<[u8; 2]>,
        filter_shares: HashSet<[u8; 2]>,
        return_available: bool,
    ) -> Result<impl Stream<Item = DaLightShare>, common_http_client::Error> {
        self.http_client
            .get_shares::<DaShare>(
                Url::from_str(&format!("http://{}", self.addr))?,
                blob_id,
                requested_shares,
                filter_shares,
                return_available,
            )
            .await
    }

    pub async fn get_storage_commitments(
        &self,
        blob_id: BlobId,
    ) -> Result<Option<DaSharesCommitments>, common_http_client::Error> {
        self.http_client
            .get_storage_commitments::<DaShare>(
                Url::from_str(&format!("http://{}", self.addr))?,
                blob_id,
            )
            .await
    }

    pub async fn get_lib_stream(
        &self,
    ) -> Result<impl Stream<Item = BlockInfo>, common_http_client::Error> {
        self.http_client
            .get_lib_stream(Url::from_str(&format!("http://{}", self.addr))?)
            .await
    }

    /// Wait for a list of transactions to be included in blocks
    pub async fn wait_for_transactions_inclusion(
        &self,
        tx_hashes: Vec<TxHash>,
        timeout: Duration,
    ) -> Vec<Option<HeaderId>> {
        let mut results = vec![None; tx_hashes.len()];

        let mut tick = 0u8;
        let _ = tokio::time::timeout(timeout, async {
            loop {
                let headers = self.get_headers(None, None, tick == 0).await;

                for header_id in headers.iter().take(10) {
                    if let Some(block) = self.get_block(*header_id).await {
                        for tx in block.transactions() {
                            for (i, target_hash) in tx_hashes.iter().enumerate() {
                                if tx.hash() == *target_hash && results[i].is_none() {
                                    results[i] = Some(*header_id);
                                }
                            }
                        }
                    }
                }

                println!(
                    "waiting for transactions ... {} of {}",
                    results.iter().filter(|x| x.is_some()).count(),
                    tx_hashes.len()
                );
                if results.iter().all(Option::is_some) {
                    return;
                }

                tokio::time::sleep(Duration::from_millis(500)).await;
                tick = tick.wrapping_add(1);
            }
        })
        .await;

        results
    }
}

#[must_use]
#[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
pub fn create_validator_config(config: GeneralConfig) -> Config {
    let testing_http_address = format!("127.0.0.1:{}", get_available_tcp_port().unwrap())
        .parse()
        .unwrap();
    let custom_deployment_config = default_e2e_deployment_settings();

    let da_policy_settings = config.da_config.policy_settings;
    Config {
        network: config.network_config,
        blend: config.blend_config.0,
        deployment: custom_deployment_config,
        time: config.time_config,
        cryptarchia: config.consensus_config.user_config().clone(),
        mempool: MempoolConfig {
            recovery_path: "./recovery/mempool.json".into(),
        },

        da_network: DaNetworkConfig {
            backend: DaNetworkBackendSettings {
                node_key: config.da_config.node_key,
                listening_address: config.da_config.listening_address,
                policy_settings: DAConnectionPolicySettings {
                    min_dispersal_peers: 0,
                    min_replication_peers: da_policy_settings.min_replication_peers,
                    max_dispersal_failures: da_policy_settings.max_dispersal_failures,
                    max_sampling_failures: da_policy_settings.max_sampling_failures,
                    max_replication_failures: da_policy_settings.max_replication_failures,
                    malicious_threshold: da_policy_settings.malicious_threshold,
                },
                monitor_settings: config.da_config.monitor_settings,
                balancer_interval: config.da_config.balancer_interval,
                redial_cooldown: config.da_config.redial_cooldown,
                replication_settings: config.da_config.replication_settings,
                subnets_settings: SubnetsConfig {
                    num_of_subnets: config.da_config.num_samples as usize,
                    shares_retry_limit: config.da_config.retry_shares_limit,
                    commitments_retry_limit: config.da_config.retry_commitments_limit,
                },
            },
            membership: config.da_config.membership.clone(),
            api_adapter_settings: ApiAdapterSettings {
                api_port: config.api_config.address.port(),
                is_secure: false,
            },
            subnet_refresh_interval: config.da_config.subnets_refresh_interval,
            subnet_threshold: config.da_config.num_subnets as usize,
            min_session_members: config.da_config.num_subnets as usize,
        },
        da_verifier: DaVerifierServiceSettings {
            share_verifier_settings: KzgrsDaVerifierSettings {
                global_params_path: config.da_config.global_params_path.clone(),
                domain_size: config.da_config.num_subnets as usize,
            },
            tx_verifier_settings: (),
            network_adapter_settings: (),
            storage_adapter_settings: VerifierStorageAdapterSettings {
                blob_storage_directory: "./".into(),
            },
            mempool_trigger_settings: MempoolPublishTriggerConfig {
                publish_threshold: NonNegativeF64::try_from(0.8).unwrap(),
                share_duration: Duration::from_secs(5),
                prune_duration: Duration::from_secs(30),
                prune_interval: Duration::from_secs(5),
            },
        },
        tracing: config.tracing_config.tracing_settings,
        http: nomos_api::ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: config.api_config.address,
                rate_limit_per_second: 10000,
                rate_limit_burst: 10000,
                max_concurrent_requests: 1000,
                ..Default::default()
            },
        },
        da_sampling: DaSamplingServiceSettings {
            sampling_settings: KzgrsSamplingBackendSettings {
                num_samples: config.da_config.num_samples,
                num_subnets: config.da_config.num_subnets,
                old_blobs_check_interval: config.da_config.old_blobs_check_interval,
                blobs_validity_duration: config.da_config.blobs_validity_duration,
            },
            share_verifier_settings: SamplingVerifierSettings {
                global_params_path: config.da_config.global_params_path,
                domain_size: config.da_config.num_subnets as usize,
            },
            commitments_wait_duration: Duration::from_secs(1),
            sdp_blob_trigger_sampling_delay: adjust_timeout(Duration::from_secs(5)),
        },
        storage: RocksBackendSettings {
            db_path: "./db".into(),
            read_only: false,
            column_family: Some("blocks".into()),
        },
        sdp: SdpSettings { declaration: None },
        wallet: WalletServiceSettings {
            known_keys: HashSet::from_iter([config.consensus_config.user_config().leader.pk]),
        },
        key_management: config.kms_config,
        testing_http: nomos_api::ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: testing_http_address,
                rate_limit_per_second: 10000,
                rate_limit_burst: 10000,
                max_concurrent_requests: 1000,
                ..Default::default()
            },
        },
    }
}
