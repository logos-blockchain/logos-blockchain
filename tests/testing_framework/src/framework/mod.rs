mod block_feed;
mod compose;
mod constants;
mod deployment_artifacts;
mod image;
mod k8s;
pub mod local;

use std::{
    env,
    num::{NonZeroU64, NonZeroUsize},
};

use async_trait::async_trait;
pub use block_feed::{
    BlockFeed, BlockFeedExtensionFactory, BlockFeedObservation, BlockFeedObserver,
    BlockFeedSnapshot, BlockFeedWaitError, BlockRecord, NodeHeadSnapshot, ObservedBlock,
    block_feed_source_provider, block_feed_sources, named_block_feed_sources,
};
use common_http_client::BasicAuthCredentials;
use lb_node::config::RunConfig;
use reqwest::Url;
use testing_framework_core::{
    scenario::{
        Application, DynError, ExternalNodeSource, NodeAccess,
        ScenarioBuilder as CoreScenarioBuilder,
    },
    topology::{DeploymentProvider, DeploymentSeed, DynTopologyError},
};
use testing_framework_runner_local::{ManualCluster, ProcessDeployer};

use crate::{
    node::{
        DeploymentPlan, NodeHttpClient,
        configs::{
            deployment::{DeploymentBuilder, TopologyConfig},
            key_id_for_preload_backend, postprocess,
            wallet::WalletConfig,
        },
    },
    workloads::{ClusterForkMonitor, ConsensusLiveness, inscription, transaction},
};

const DEFAULT_PAYLOAD_BYTES: usize = 128;

pub type ScenarioBuilder = CoreScenarioBuilder<LbcEnv>;
pub type ScenarioBuilderWith = ScenarioBuilder;

pub type LbcLocalDeployer = ProcessDeployer<LbcEnv>;
pub type LbcComposeDeployer = testing_framework_runner_compose::ComposeDeployer<LbcEnv>;
pub type LbcK8sDeployer = testing_framework_runner_k8s::K8sDeployer<LbcEnv>;

pub type LbcManualCluster = ManualCluster<LbcEnv>;
pub type LbcK8sManualCluster = testing_framework_runner_k8s::ManualCluster<LbcEnv>;

pub struct LbcEnv;

#[async_trait]
impl Application for LbcEnv {
    type Deployment = DeploymentPlan;

    type NodeClient = NodeHttpClient;

    type NodeConfig = RunConfig;

    fn external_node_client(source: &ExternalNodeSource) -> Result<Self::NodeClient, DynError> {
        let endpoint = Url::parse(source.endpoint())?;
        let basic_auth = external_basic_auth(&endpoint);

        Ok(NodeHttpClient::from_urls_with_basic_auth(
            endpoint, None, basic_auth,
        ))
    }

    fn build_node_client(access: &NodeAccess) -> Result<Self::NodeClient, DynError> {
        let base_url = access.api_base_url()?;
        let testing_url = access
            .testing_port()
            .map(|port| Url::parse(&format!("http://{}:{port}", access.host())))
            .transpose()?;

        Ok(NodeHttpClient::from_urls(base_url, testing_url))
    }

    fn node_readiness_path() -> &'static str {
        lb_http_api_common::paths::CRYPTARCHIA_INFO
    }
}

fn external_basic_auth(endpoint: &Url) -> Option<BasicAuthCredentials> {
    if !endpoint.username().is_empty() {
        return Some(BasicAuthCredentials::new(
            endpoint.username().to_owned(),
            Some(endpoint.password().unwrap_or_default().to_owned()),
        ));
    }

    let username = env::var("LOGOS_EXTERNAL_BASIC_AUTH_USER").ok()?;
    let password = env::var("LOGOS_EXTERNAL_BASIC_AUTH_PASS").ok()?;

    Some(BasicAuthCredentials::new(username, Some(password)))
}

pub trait CoreBuilderExt: Sized {
    #[must_use]
    fn deployment_with(f: impl FnOnce(DeploymentBuilder) -> DeploymentBuilder) -> Self;

    #[must_use]
    fn with_block_feed(self) -> Self;

    #[must_use]
    fn with_wallet_config(self, wallet: WalletConfig) -> Self;
}

impl CoreBuilderExt for ScenarioBuilder {
    fn deployment_with(f: impl FnOnce(DeploymentBuilder) -> DeploymentBuilder) -> Self {
        let topology = f(DeploymentBuilder::new(TopologyConfig::empty()));

        Self::new(Box::new(topology)).with_block_feed()
    }

    fn with_block_feed(self) -> Self {
        testing_framework_core::scenario::CoreBuilderExt::with_runtime_extension_factory(
            self,
            Box::new(BlockFeedExtensionFactory),
        )
    }

    fn with_wallet_config(self, wallet: WalletConfig) -> Self {
        self.map_deployment_provider(|provider| {
            Box::new(WalletConfigProvider {
                inner: provider,
                wallet,
            })
        })
    }
}

struct WalletConfigProvider {
    inner: Box<dyn DeploymentProvider<DeploymentPlan>>,
    wallet: WalletConfig,
}

impl DeploymentProvider<DeploymentPlan> for WalletConfigProvider {
    fn build(&self, seed: Option<&DeploymentSeed>) -> Result<DeploymentPlan, DynTopologyError> {
        let mut deployment = self.inner.build(seed)?;
        apply_wallet_config_to_deployment(&mut deployment, &self.wallet);
        Ok(deployment)
    }
}

#[doc(hidden)]
pub fn apply_wallet_config_to_deployment(deployment: &mut DeploymentPlan, wallet: &WalletConfig) {
    deployment.config.wallet_config = wallet.clone();

    let wallet_accounts = wallet
        .accounts
        .iter()
        .map(|account| (account.secret_key.clone(), account.value))
        .collect::<Vec<_>>();

    let mut node_configs = deployment
        .plans
        .iter()
        .map(|plan| plan.general.clone())
        .collect::<Vec<_>>();

    let Some(base_genesis_tx) = deployment.config.genesis_tx.clone() else {
        return;
    };

    let genesis_tx = postprocess::apply_wallet_genesis_overrides(
        &mut node_configs,
        &base_genesis_tx,
        deployment.config.blend_core_nodes,
        &wallet_accounts,
        key_id_for_preload_backend,
        deployment.config.test_context.as_deref(),
    );
    deployment.config.genesis_tx = Some(genesis_tx);

    for (plan, node_config) in deployment.plans.iter_mut().zip(node_configs) {
        plan.general = node_config;
    }
}

pub trait ScenarioBuilderExt: Sized {
    #[must_use]
    fn transactions(self) -> TransactionFlowBuilder;

    #[must_use]
    fn transactions_with(
        self,
        f: impl FnOnce(TransactionFlowBuilder) -> TransactionFlowBuilder,
    ) -> ScenarioBuilderWith;

    #[must_use]
    fn inscriptions(self) -> InscriptionFlowBuilder;

    #[must_use]
    fn inscriptions_with(
        self,
        f: impl FnOnce(InscriptionFlowBuilder) -> InscriptionFlowBuilder,
    ) -> ScenarioBuilderWith;

    #[must_use]
    fn expect_consensus_liveness(self) -> Self;

    /// Adds a fail-fast fork monitor expectation.
    ///
    /// The scenario fails as soon as the monitor observes a LIB mismatch
    /// between nodes.
    #[must_use]
    fn expect_cluster_fork_monitor(self) -> Self;

    #[must_use]
    fn initialize_wallet(self, total_funds: u64, users: usize) -> Self;
}

impl ScenarioBuilderExt for ScenarioBuilderWith {
    fn transactions(self) -> TransactionFlowBuilder {
        TransactionFlowBuilder {
            builder: self,
            rate: NonZeroU64::MIN,
            users: None,
        }
    }

    fn transactions_with(
        self,
        f: impl FnOnce(TransactionFlowBuilder) -> TransactionFlowBuilder,
    ) -> ScenarioBuilderWith {
        f(self.transactions()).apply()
    }

    fn inscriptions(self) -> InscriptionFlowBuilder {
        InscriptionFlowBuilder {
            builder: self,
            channels: NonZeroUsize::MIN,
            inscription_payload_bytes: NonZeroUsize::new(DEFAULT_PAYLOAD_BYTES)
                .expect("constant is non-zero"),
        }
    }

    fn inscriptions_with(
        self,
        f: impl FnOnce(InscriptionFlowBuilder) -> InscriptionFlowBuilder,
    ) -> ScenarioBuilderWith {
        f(self.inscriptions()).apply()
    }

    fn expect_consensus_liveness(self) -> Self {
        self.with_expectation(ConsensusLiveness::default())
    }

    fn expect_cluster_fork_monitor(self) -> Self {
        self.with_expectation(ClusterForkMonitor::<LbcEnv>::default())
    }

    fn initialize_wallet(self, total_funds: u64, users: usize) -> Self {
        let Some(user_count) = nonzero_usize(users) else {
            tracing::warn!(
                users,
                "wallet user count must be non-zero; ignoring initialize_wallet"
            );
            return self;
        };

        match WalletConfig::uniform(total_funds, user_count) {
            Ok(wallet) => self.with_wallet_config(wallet),
            Err(error) => {
                tracing::warn!(
                    users,
                    total_funds,
                    error = %error,
                    "invalid initialize_wallet input; ignoring initialize_wallet"
                );
                self
            }
        }
    }
}

pub struct TransactionFlowBuilder {
    builder: ScenarioBuilderWith,
    rate: NonZeroU64,
    users: Option<NonZeroUsize>,
}

impl TransactionFlowBuilder {
    pub fn rate(mut self, rate: u64) -> Self {
        if let Some(rate) = NonZeroU64::new(rate) {
            self.rate = rate;
        } else {
            tracing::warn!(
                rate,
                "transaction rate must be non-zero; keeping previous rate"
            );
        }

        self
    }

    pub fn users(mut self, users: usize) -> Self {
        if let Some(value) = nonzero_usize(users) {
            self.users = Some(value);
        } else {
            tracing::warn!(
                users,
                "transaction user count must be non-zero; keeping previous setting"
            );
        }

        self
    }

    pub fn apply(self) -> ScenarioBuilderWith {
        let workload = transaction::Workload::new(self.rate).with_user_limit(self.users);
        self.builder.with_workload(workload)
    }
}

pub struct InscriptionFlowBuilder {
    builder: ScenarioBuilderWith,
    channels: NonZeroUsize,
    inscription_payload_bytes: NonZeroUsize,
}

impl InscriptionFlowBuilder {
    pub fn channels(mut self, channels: usize) -> Self {
        if let Some(value) = nonzero_usize(channels) {
            self.channels = value;
        } else {
            tracing::warn!(
                channels,
                "inscription channel count must be non-zero; keeping previous setting"
            );
        }

        self
    }

    pub fn inscription_payload_bytes(mut self, payload_bytes: usize) -> Self {
        if let Some(value) = nonzero_usize(payload_bytes) {
            self.inscription_payload_bytes = value;
        } else {
            tracing::warn!(
                payload_bytes,
                "inscription payload bytes must be non-zero; keeping previous setting"
            );
        }

        self
    }

    pub fn payload_bytes(self, payload_bytes: usize) -> Self {
        self.inscription_payload_bytes(payload_bytes)
    }

    pub fn apply(self) -> ScenarioBuilderWith {
        let workload = inscription::Workload::default()
            .with_channel_count(self.channels)
            .with_payload_bytes(self.inscription_payload_bytes);
        self.builder.with_workload(workload)
    }
}

const fn nonzero_usize(value: usize) -> Option<NonZeroUsize> {
    NonZeroUsize::new(value)
}
