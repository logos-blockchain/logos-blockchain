use std::{
    ffi::OsStr,
    net::SocketAddr,
    path::{Path, PathBuf},
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
    mantle::{SignedMantleTx, Transaction as _, TxHash},
    sdp::Declaration,
};
use lb_http_api_common::paths::{
    BLOCKS_DETAIL, CRYPTARCHIA_HEADERS, CRYPTARCHIA_INFO, MANTLE_SDP_DECLARATIONS, NETWORK_INFO,
};
use lb_key_management_system_service::keys::secured_key::SecuredKey as _;
use lb_network_service::backends::libp2p::Libp2pInfo;
use lb_node::{
    HeaderId, UserConfig,
    config::{
        ApiConfig, CryptarchiaConfig, RunConfig, SdpConfig, StorageConfig, WalletConfig,
        api::serde::AxumBackendSettings,
        cryptarchia::serde::RequiredValues as CryptarchiaConfigRequiredValues,
        deployment::DeploymentSettings, sdp::serde::RequiredValues as SdpConfigRequiredValues,
        state::Config as StateConfig, tracing::serde as tracing,
        wallet::serde::RequiredValues as WalletConfigRequiredValues,
    },
};
use lb_testing_framework::release_reserved_port_block;
use lb_tx_service::MempoolMetrics;
use reqwest::Url;
use tempfile::NamedTempFile;
use tokio::time::error::Elapsed;

use super::{CLIENT, create_tempdir, get_exe_path, persist_tempdir};
use crate::{
    IS_DEBUG_TRACING, common::kms::key_id_for_preload_backend, get_reserved_available_tcp_port,
    nodes::LOGS_PREFIX, topology::configs::GeneralConfig,
};

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
        // Wait for the process to fully exit so that ports and other resources
        // are released before the next test iteration spawns new validators.
        // After SIGKILL, wait() returns almost immediately.
        drop(self.child.wait());
        release_reserved_port_block();
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

    /// Kill the validator process.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }

    /// Restart the validator process using the same config and state directory.
    /// This preserves persisted state (like SDP nonces fetched from ledger).
    pub async fn restart(&mut self) -> Result<(), Elapsed> {
        // Kill the current process
        drop(self.child.kill());
        self.wait_for_exit(Duration::from_secs(5)).await;

        // Re-write config files (they were temporary and may have been cleaned up)
        let mut user_config_file = NamedTempFile::new().unwrap();
        let mut deployment_config_file = NamedTempFile::new().unwrap();

        serde_yaml::to_writer(&mut user_config_file, &self.config.user).unwrap();
        serde_yaml::to_writer(&mut deployment_config_file, &self.config.deployment).unwrap();

        // Spawn new process with same config
        let exe_path = get_exe_path();
        self.child = Command::new(exe_path)
            .arg("--deployment")
            .arg(deployment_config_file.path().as_os_str())
            .arg(user_config_file.path().as_os_str())
            .current_dir(self.tempdir.path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();

        // Wait for the node to come online
        tokio::time::timeout(Duration::from_secs(10), async {
            self.wait_online().await;
        })
        .await?;

        Ok(())
    }

    /// Restarts with the same deployment and user configs, but attaches
    /// provided cli arguments.
    pub async fn restart_with_args<I, S>(&mut self, args: I) -> Result<(), Elapsed>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        drop(self.child.kill());
        self.wait_for_exit(Duration::from_secs(5)).await;

        // Re-write config files (they were temporary and may have been cleaned up)
        let (user_config_path, deployment_config_path) =
            Self::create_config_files(self.tempdir.path(), &self.config);

        // Spawn new process with same config
        let exe_path = get_exe_path();
        self.child = Command::new(exe_path)
            .arg("--deployment")
            .arg(deployment_config_path.as_os_str())
            .args(args)
            .arg(user_config_path.as_os_str())
            .current_dir(self.tempdir.path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();

        // Wait for the node to come online
        tokio::time::timeout(Duration::from_secs(10), async {
            self.wait_online().await;
        })
        .await?;

        Ok(())
    }

    fn create_config_files(dir: &Path, config: &RunConfig) -> (PathBuf, PathBuf) {
        let user_config_path = dir.join("user_config.yaml");
        let deployment_config_path = dir.join("deployment_config.yaml");
        let mut user_config_file = std::fs::File::create(&user_config_path).unwrap();
        let mut deployment_config_file = std::fs::File::create(&deployment_config_path).unwrap();
        serde_yaml::to_writer(&mut user_config_file, &config.user).unwrap();
        serde_yaml::to_writer(&mut deployment_config_file, &config.deployment).unwrap();
        println!("User config: '{}'", user_config_path.display());
        println!("Deployment config: '{}'", deployment_config_path.display());
        (user_config_path, deployment_config_path)
    }

    pub async fn spawn(mut config: RunConfig) -> Result<Self, Elapsed> {
        let dir = create_tempdir().unwrap();

        if !*IS_DEBUG_TRACING {
            // setup logging so that we can intercept it later in testing
            config.user.tracing.logger = tracing::logger::Layers {
                file: Some(tracing::logger::FileConfig {
                    directory: dir.path().to_owned(),
                    prefix: Some(LOGS_PREFIX.into()),
                }),
                loki: None,
                gelf: None,
                otlp: None,
                stdout: false,
                stderr: false,
            };
        }

        config.user.state.base_folder = dir.path().to_path_buf();
        "db".clone_into(&mut config.user.storage.backend.folder_name);

        // let user_config_path = dir.path().join("user_config.yaml");
        let (user_config_path, deployment_config_path) =
            Self::create_config_files(dir.path(), &config);

        let exe_path = get_exe_path();
        let child = Command::new(exe_path)
            .arg("--deployment")
            .arg(deployment_config_path.as_os_str())
            .arg(user_config_path.as_os_str())
            .current_dir(dir.path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let node = Self {
            addr: config.user.api.backend.listen_address,
            testing_http_addr: config.user.api.testing.listen_address,
            child,
            tempdir: dir,
            config,
            http_client: CommonHttpClient::new_with_client(CLIENT.clone(), None),
        };

        tokio::time::timeout(Duration::from_secs(10), async {
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

    pub async fn wait_for_height(&self, target_height: u64, duration: Duration) -> Option<()> {
        tokio::time::timeout(duration, async {
            loop {
                let info = self.consensus_info(false).await;
                println!("{info:?}");
                if info.height >= target_height {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .ok()
    }

    pub async fn get_block(&self, id: HeaderId) -> Option<Block<SignedMantleTx>> {
        let path = BLOCKS_DETAIL.replace(":id", &id.to_string());
        CLIENT
            .get(format!("http://{}{}", self.addr, path))
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
pub fn create_validator_user_config(config: GeneralConfig) -> UserConfig {
    let network_config = config.network_config;

    let blend_config = config.blend_config.0;

    let time_config = config.time_config;

    let cryptarchia_config = {
        let mut base_config =
            CryptarchiaConfig::with_required_values(CryptarchiaConfigRequiredValues {
                // We use the same funding key used for SDP.
                funding_pk: config.consensus_config.funding_pk,
            });
        base_config.service.bootstrap.prolonged_bootstrap_period =
            config.consensus_config.prolonged_bootstrap_period;
        base_config
    };

    let tracing_config = config.tracing_config.tracing_settings;

    let api_config = ApiConfig {
        backend: AxumBackendSettings {
            listen_address: config.api_config.address,
            max_concurrent_requests: 1000,
            ..Default::default()
        },
        testing: AxumBackendSettings {
            listen_address: format!("127.0.0.1:{}", get_reserved_available_tcp_port().unwrap())
                .parse()
                .unwrap(),
            max_concurrent_requests: 1000,
            ..Default::default()
        },
    };

    let storage_config = StorageConfig::default();

    let mut sdp_config = SdpConfig::with_required_values(SdpConfigRequiredValues {
        funding_pk: config.consensus_config.funding_sk.as_public_key(),
    });

    if let Some(declaration_id) = config.sdp_config.declaration_id {
        sdp_config.declaration_id = Some(declaration_id);
    }

    let wallet_config = {
        let mut base_config = WalletConfig::with_required_values(WalletConfigRequiredValues {
            voucher_master_key_id: key_id_for_preload_backend(
                &config.consensus_config.known_key.clone().into(),
            ),
        });
        base_config.known_keys = [
            (
                key_id_for_preload_backend(&config.consensus_config.known_key.clone().into()),
                config.consensus_config.known_key.as_public_key(),
            ),
            (
                key_id_for_preload_backend(&config.consensus_config.funding_sk.clone().into()),
                config.consensus_config.funding_sk.as_public_key(),
            ),
        ]
        .into_iter()
        .chain(config.consensus_config.other_keys.iter().map(|sk| {
            (
                key_id_for_preload_backend(&sk.clone().into()),
                sk.as_public_key(),
            )
        }))
        .collect();

        base_config
    };

    let kms_config = config.kms_config;

    let state_config = StateConfig::default();

    UserConfig {
        network: network_config,
        blend: blend_config,
        time: time_config,
        cryptarchia: cryptarchia_config,
        tracing: tracing_config,
        api: api_config,
        storage: storage_config,
        sdp: sdp_config,
        wallet: wallet_config,
        kms: kms_config,
        state: state_config,
    }
}

#[must_use]
pub fn create_validator_config(
    config: GeneralConfig,
    deployment_config: DeploymentSettings,
) -> RunConfig {
    RunConfig {
        deployment: deployment_config,
        user: create_validator_user_config(config),
    }
}
