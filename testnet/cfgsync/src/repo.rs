use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use lb_node::config::deployment::{DeploymentSettings, WellKnownDeployment};
use lb_tests::topology::configs::GeneralConfig;
use lb_tracing_service::TracingSettings;
use tokio::{sync::oneshot::Sender, time::timeout};

use crate::{
    Host,
    config::{create_node_config_from_template, create_node_configs},
    server::CfgSyncConfig,
};

pub enum RepoResponse {
    Config(Box<(GeneralConfig, DeploymentSettings)>),
    Timeout,
}

pub struct ConfigRepo {
    waiting_hosts: Mutex<HashMap<Host, Sender<RepoResponse>>>,
    generated_configs: Mutex<HashMap<Host, GeneralConfig>>,
    deployment_settings: Mutex<Option<DeploymentSettings>>,
    n_hosts: usize,
    tracing_settings: TracingSettings,
    timeout_duration: Duration,
}

impl From<CfgSyncConfig> for Arc<ConfigRepo> {
    fn from(config: CfgSyncConfig) -> Self {
        let tracing_settings = config.to_tracing_settings();

        ConfigRepo::new(
            config.n_hosts,
            tracing_settings,
            Duration::from_secs(config.timeout),
        )
    }
}

impl ConfigRepo {
    #[must_use]
    pub fn new(
        n_hosts: usize,
        tracing_settings: TracingSettings,
        timeout_duration: Duration,
    ) -> Arc<Self> {
        let repo = Arc::new(Self {
            waiting_hosts: Mutex::new(HashMap::new()),
            generated_configs: Mutex::new(HashMap::new()),
            deployment_settings: Mutex::new(None),
            n_hosts,
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
            .generated_configs
            .lock()
            .unwrap()
            .values()
            .next()
            .cloned();

        if let Some(template) = template {
            let new_config =
                create_node_config_from_template(&TracingSettings::default(), &host, &template);

            self.generated_configs
                .lock()
                .unwrap()
                .insert(host, new_config.clone());

            return Some(new_config);
        }

        None
    }

    async fn run(&self) {
        let timeout_duration = self.timeout_duration;

        if timeout(timeout_duration, self.wait_for_hosts()).await == Ok(()) {
            println!("All hosts have announced their IPs");

            let mut waiting_hosts = self.waiting_hosts.lock().unwrap();
            let hosts = waiting_hosts.keys().cloned().collect();

            let (configs, genesis_tx) = create_node_configs(&self.tracing_settings, hosts);
            let devnet_settings = {
                let mut default_settings = DeploymentSettings::from(WellKnownDeployment::Devnet);
                default_settings.cryptarchia.genesis_state = genesis_tx;
                default_settings
            };

            {
                let mut storage = self.generated_configs.lock().unwrap();
                (*storage).clone_from(&configs);
            };

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
