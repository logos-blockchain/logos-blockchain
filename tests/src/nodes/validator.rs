use std::{
    collections::HashMap,
    net::SocketAddr,
    process::{Child, Command, Stdio},
    str::FromStr as _,
    time::Duration,
};

use futures::Stream;
use lb_chain_broadcast_service::BlockInfo;
use lb_chain_service::CryptarchiaInfo;
use lb_common_http_client::CommonHttpClient;
use lb_core::{
    block::Block,
    mantle::{SignedMantleTx, Transaction as _, TxHash, Value},
    sdp::Declaration,
};
use lb_http_api_common::{
    paths::{
        CRYPTARCHIA_HEADERS, CRYPTARCHIA_INFO, MANTLE_SDP_DECLARATIONS, NETWORK_INFO, STORAGE_BLOCK,
    },
    settings::AxumBackendSettings,
};
use lb_key_management_system_service::keys::secured_key::SecuredKey as _;
use lb_network_service::backends::libp2p::Libp2pInfo;
use lb_node::{
    HeaderId, RocksBackendSettings, UserConfig,
    config::{RunConfig, deployment::DeploymentSettings, mempool::serde::Config as MempoolConfig},
};
use lb_sdp_service::SdpSettings;
use lb_tracing::logging::local::FileConfig;
use lb_tracing_service::LoggerLayer;
use lb_tx_service::MempoolMetrics;
use lb_utils::net::get_available_tcp_port;
use lb_wallet_service::WalletServiceSettings;
use reqwest::Url;
use tempfile::NamedTempFile;
use tokio::time::error::Elapsed;

use super::{CLIENT, create_tempdir, get_exe_path, persist_tempdir};
use crate::{
    IS_DEBUG_TRACING, adjust_timeout, common::kms::key_id_for_preload_backend, nodes::LOGS_PREFIX,
    topology::configs::GeneralConfig,
};

const BIN_PATH_DEBUG: &str = "../target/debug/logos-blockchain-node";
const BIN_PATH_RELEASE: &str = "../target/release/logos-blockchain-node";

pub enum Pool {
    Mantle,
}

pub struct Validator {
    addr: SocketAddr,
    testing_http_addr: SocketAddr,
    tempdir: tempfile::TempDir,
    child: Child,
    config: RunConfig,
    http_client: CommonHttpClient,
}

impl Drop for Validator {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Err(e) = persist_tempdir(&mut self.tempdir, "logos-blockchain-node")
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

    pub async fn spawn(mut config: RunConfig) -> Result<Self, Elapsed> {
        let dir = create_tempdir().unwrap();
        let mut user_config_file = NamedTempFile::new().unwrap();
        let mut deployment_config_file = NamedTempFile::new().unwrap();

        if !*IS_DEBUG_TRACING {
            // setup logging so that we can intercept it later in testing
            config.user.tracing.logger = LoggerLayer::File(FileConfig {
                directory: dir.path().to_owned(),
                prefix: Some(LOGS_PREFIX.into()),
            });
        }

        config.user.storage.db_path = dir.path().join("db");

        serde_yaml::to_writer(&mut user_config_file, &config.user).unwrap();
        serde_yaml::to_writer(&mut deployment_config_file, &config.deployment).unwrap();
        let exe_path = get_exe_path(BIN_PATH_DEBUG, BIN_PATH_RELEASE);
        let child = Command::new(exe_path)
            .arg("--deployment")
            .arg(deployment_config_file.path().as_os_str())
            .arg(user_config_file.path().as_os_str())
            .current_dir(dir.path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let node = Self {
            addr: config.user.http.backend_settings.address,
            testing_http_addr: config.user.testing_http.backend_settings.address,
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

    pub async fn get_mempool_metrics(&self, pool: Pool) -> MempoolMetrics {
        let discr = match pool {
            Pool::Mantle => "mantle",
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

    // not async so that we can use this in `Drop`
    #[must_use]
    pub fn get_logs_from_file(&self) -> String {
        println!(
            "fetching logs from dir {}...",
            self.tempdir.path().display()
        );
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
    pub const fn config(&self) -> &RunConfig {
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

    pub async fn network_info(&self) -> Libp2pInfo {
        self.get(NETWORK_INFO).await.unwrap().json().await.unwrap()
    }

    pub async fn get_lib_stream(
        &self,
    ) -> Result<impl Stream<Item = BlockInfo>, lb_common_http_client::Error> {
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
pub fn create_validator_config(
    config: GeneralConfig,
    deployment_config: DeploymentSettings,
) -> RunConfig {
    let testing_http_address = format!("127.0.0.1:{}", get_available_tcp_port().unwrap())
        .parse()
        .unwrap();

    let user_config = UserConfig {
        network: config.network_config,
        blend: config.blend_config.0,
        time: config.time_config,
        cryptarchia: config.consensus_config.user_config().clone(),
        mempool: MempoolConfig {
            recovery_path: "./recovery/mempool.json".into(),
        },
        tracing: config.tracing_config.tracing_settings,
        http: lb_api_service::ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: config.api_config.address,
                max_concurrent_requests: 1000,
                ..Default::default()
            },
        },
        storage: RocksBackendSettings {
            db_path: "./db".into(),
            read_only: false,
            column_family: Some("blocks".into()),
        },
        sdp: SdpSettings {
            declaration: None,
            wallet_config: lb_sdp_service::wallet::SdpWalletConfig {
                max_tx_fee: Value::MAX,
                funding_pk: config.consensus_config.funding_sk.as_public_key(),
            },
        },
        wallet: WalletServiceSettings {
            known_keys: HashMap::from_iter([
                (
                    key_id_for_preload_backend(&config.consensus_config.known_key.clone().into()),
                    config.consensus_config.known_key.as_public_key(),
                ),
                (
                    key_id_for_preload_backend(&config.consensus_config.funding_sk.clone().into()),
                    config.consensus_config.funding_sk.as_public_key(),
                ),
            ]),
            voucher_master_key_id: key_id_for_preload_backend(
                &config.consensus_config.known_key.clone().into(),
            ),
            recovery_path: "./recovery/wallet.json".into(),
        },
        key_management: config.kms_config,
        testing_http: lb_api_service::ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: testing_http_address,
                max_concurrent_requests: 1000,
                ..Default::default()
            },
        },
    };

    RunConfig {
        deployment: deployment_config,
        user: user_config,
    }
}
