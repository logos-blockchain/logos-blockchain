use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use lb_node::config::{
    TracingConfig,
    deployment::{DeploymentSettings, WellKnownDeployment},
};
use lb_tests::topology::configs::GeneralConfig;
use time::OffsetDateTime;
use tokio::{sync::oneshot::Sender, time::timeout};

use crate::{
    Entropy, FaucetSettings, Host,
    config::{create_node_config_from_template, create_node_configs},
    load_entropy,
    server::CfgSyncConfig,
};

pub enum RepoResponse {
    Config(Box<(GeneralConfig, DeploymentSettings)>),
    Timeout,
}

pub struct ConfigRepo {
    waiting_hosts: Mutex<HashMap<Host, Sender<RepoResponse>>>,
    generated_user_configs: Mutex<HashMap<Host, GeneralConfig>>,
    deployment_settings: Mutex<Option<DeploymentSettings>>,
    pub deployment_settings_storage_path: PathBuf,
    n_hosts: usize,
    entropy: Entropy,
    faucet_settings: FaucetSettings,
    tracing_settings: TracingConfig,
    chain_start_time: OffsetDateTime,
    timeout_duration: Duration,
}

impl From<CfgSyncConfig> for Arc<ConfigRepo> {
    fn from(config: CfgSyncConfig) -> Self {
        let entropy = load_entropy(&config.entropy_file).expect("Failed to load entropy file");
        ConfigRepo::new(
            config.n_hosts,
            entropy,
            config.faucet_settings(),
            config
                .chain_start_time
                .unwrap_or_else(OffsetDateTime::now_utc),
            config.tracing_settings(),
            Duration::from_secs(config.timeout),
            config.deployment_settings_storage_path,
        )
    }
}

impl ConfigRepo {
    #[must_use]
    pub fn new(
        n_hosts: usize,
        entropy: Entropy,
        faucet_settings: FaucetSettings,
        chain_start_time: OffsetDateTime,
        tracing_settings: TracingConfig,
        timeout_duration: Duration,
        deployment_settings_storage_path: PathBuf,
    ) -> Arc<Self> {
        let repo = Arc::new(Self {
            waiting_hosts: Mutex::new(HashMap::new()),
            generated_user_configs: Mutex::new(HashMap::new()),
            deployment_settings: Mutex::new(None),
            deployment_settings_storage_path,
            n_hosts,
            entropy,
            faucet_settings,
            chain_start_time,
            tracing_settings,
            timeout_duration,
        });

        let repo_clone = Arc::clone(&repo);
        tokio::spawn(async move {
            repo_clone.run().await;
        });

        repo
    }

    /// Registers host into the initial node list.
    pub fn register(&self, host: Host, reply_tx: Sender<RepoResponse>) {
        let mut waiting_hosts = self.waiting_hosts.lock().unwrap();
        waiting_hosts.insert(host, reply_tx);
    }

    pub fn deployment_settings(&self) -> Option<DeploymentSettings> {
        self.deployment_settings.lock().unwrap().clone()
    }

    /// Generates a new node config for host based on the initial nodes config.
    pub fn append(&self, host: Host) -> Option<GeneralConfig> {
        let template = self
            .generated_user_configs
            .lock()
            .unwrap()
            .values()
            .next()
            .cloned();

        if let Some(template) = template {
            let new_config =
                create_node_config_from_template(&TracingConfig::default(), &host, &template);

            self.generated_user_configs
                .lock()
                .unwrap()
                .insert(host, new_config.clone());

            return Some(new_config);
        }

        None
    }

    fn persist_deployment_settings(&self, settings: &DeploymentSettings) -> Result<(), String> {
        let yaml = serde_yaml::to_string(settings)
            .map_err(|e| format!("Error: Failed to serialize deployment settings: {e}"))?;
        std::fs::write(&self.deployment_settings_storage_path, yaml)
            .map_err(|err| format!("Failed to write config to file: {err}"))?;
        Ok(())
    }

    async fn run(&self) {
        let timeout_duration = self.timeout_duration;

        if timeout(timeout_duration, self.wait_for_hosts()).await == Ok(()) {
            println!("All hosts have announced their IPs");

            let mut waiting_hosts = self.waiting_hosts.lock().unwrap();
            let hosts = waiting_hosts.keys().cloned().collect();

            let (configs, genesis_tx, faucet_pk) = create_node_configs(
                &self.entropy,
                &self.faucet_settings,
                &self.tracing_settings,
                hosts,
            );
            let devnet_settings = {
                let mut default_settings = DeploymentSettings::from(WellKnownDeployment::Devnet);
                default_settings.cryptarchia.genesis_state = genesis_tx;
                default_settings.cryptarchia.faucet_pk = faucet_pk;
                default_settings.time.chain_start_time = self.chain_start_time;
                default_settings
            };

            {
                let mut storage = self.generated_user_configs.lock().unwrap();
                (*storage).clone_from(&configs);
            };

            {
                let mut deployment_settings = self.deployment_settings.lock().unwrap();
                *deployment_settings = Some(devnet_settings.clone());
            };

            self.persist_deployment_settings(&devnet_settings)
                .expect("Settings should be persisted");

            for (host, sender) in waiting_hosts.drain() {
                let config = configs.get(&host).expect("host should have a config");
                drop(sender.send(RepoResponse::Config(Box::new((
                    config.to_owned(),
                    devnet_settings.clone(),
                )))));
            }
        } else {
            println!("Timeout: Not all hosts announced within the time limit");

            let mut waiting_hosts = self.waiting_hosts.lock().unwrap();
            for (_, sender) in waiting_hosts.drain() {
                drop(sender.send(RepoResponse::Timeout));
            }
        }
    }

    async fn wait_for_hosts(&self) {
        loop {
            if self.waiting_hosts.lock().unwrap().len() >= self.n_hosts {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
