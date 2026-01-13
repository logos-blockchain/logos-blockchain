use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use nomos_tracing_service::TracingSettings;
use tests::topology::configs::{GeneralConfig, da::DaParams};
use tokio::{sync::oneshot::Sender, time::timeout};

use crate::{
    config::{Host, create_node_configs},
    server::CfgSyncConfig,
};

pub enum RepoResponse {
    Config(Box<GeneralConfig>),
    Timeout,
}

pub struct ConfigRepo {
    waiting_hosts: Mutex<HashMap<Host, Sender<RepoResponse>>>,
    n_hosts: usize,
    da_params: DaParams,
    tracing_settings: TracingSettings,
    timeout_duration: Duration,
}

impl From<CfgSyncConfig> for Arc<ConfigRepo> {
    fn from(config: CfgSyncConfig) -> Self {
        let da_params = config.to_da_params();
        let tracing_settings = config.to_tracing_settings();

        ConfigRepo::new(
            config.n_hosts,
            da_params,
            tracing_settings,
            Duration::from_secs(config.timeout),
        )
    }
}

impl ConfigRepo {
    #[must_use]
    pub fn new(
        n_hosts: usize,
        da_params: DaParams,
        tracing_settings: TracingSettings,
        timeout_duration: Duration,
    ) -> Arc<Self> {
        let repo = Arc::new(Self {
            waiting_hosts: Mutex::new(HashMap::new()),
            n_hosts,
            da_params,
            tracing_settings,
            timeout_duration,
        });

        let repo_clone = Arc::clone(&repo);
        tokio::spawn(async move {
            repo_clone.run().await;
        });

        repo
    }

    pub fn register(&self, host: Host, reply_tx: Sender<RepoResponse>) {
        let mut waiting_hosts = self.waiting_hosts.lock().unwrap();
        waiting_hosts.insert(host, reply_tx);
    }

    async fn run(&self) {
        let timeout_duration = self.timeout_duration;

        if timeout(timeout_duration, self.wait_for_hosts()).await == Ok(()) {
            println!("All hosts have announced their IPs");

            let mut waiting_hosts = self.waiting_hosts.lock().unwrap();
            let hosts = waiting_hosts.keys().cloned().collect();

            let configs = create_node_configs(&self.da_params, &self.tracing_settings, hosts);

            for (host, sender) in waiting_hosts.drain() {
                let config = configs.get(&host).expect("host should have a config");
                drop(sender.send(RepoResponse::Config(Box::new(config.to_owned()))));
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
