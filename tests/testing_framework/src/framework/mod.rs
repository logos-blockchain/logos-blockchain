mod app;
mod block_feed;
mod compose;
mod constants;
mod deployment_artifacts;
mod image;
mod k8s;
pub mod local;
mod snapshot;

use std::{
    env,
    num::{NonZeroU64, NonZeroUsize},
    time::Duration,
};

pub use app::LbcClusterApp;
use async_trait::async_trait;
pub use block_feed::{
    BlockFeed, BlockFeedCollector, BlockFeedCollectorRuntime, BlockFeedObservation,
    BlockFeedObserver, BlockFeedSnapshot, BlockFeedWaitError, BlockRecord, BoxedBlockFeedCollector,
    NodeHeadSnapshot, ObservedBlock, block_feed_sources, named_block_feed_sources,
};
use common_http_client::BasicAuthCredentials;
use lb_config::kms::key_id_for_preload_backend;
use lb_core::block::genesis::GenesisBlock;
use lb_node::config::RunConfig;
use reqwest::Url;
pub use snapshot::NodeStateSnapshotStore;
pub use testing_framework_app::{
    AppHost, AppHostDeployError, AppHostDeployer, AppHostEnv, AppRunContextExt,
};
use testing_framework_app::{AppHostScenarioBuilder, AppScenarioBuilderExt as _};
use testing_framework_core::{
    scenario::{
        Application, DynError, Expectation, ExternalNodeSource, NodeAccess, ReadinessProbe,
        Scenario, ScenarioBuildError, Workload,
    },
    topology::DeploymentProvider,
};
use testing_framework_runner_compose::ComposeProvisioner;
use testing_framework_runner_k8s::K8sClusterProvisioner;
use testing_framework_runner_local::ManualCluster;

use crate::{
    FailureDiagnosticsExpectation,
    node::{
        DeploymentPlan, NodeHttpClient,
        configs::{
            deployment::{DeploymentBuilder, TopologyConfig},
            postprocess,
            wallet::WalletConfig,
        },
    },
    workloads::{ClusterForkMonitor, ConsensusLiveness, inscription, transaction},
};

const DEFAULT_PAYLOAD_BYTES: usize = 128;

pub type ScenarioBuilderWith = ScenarioBuilder;

pub type LbcManualCluster = ManualCluster<LbcEnv>;
pub type LbcK8sManualCluster = testing_framework_runner_k8s::ManualCluster<LbcEnv>;

/// Scenario over the application host environment hosting the Logos cluster.
pub type LbcScenario = Scenario<AppHostEnv>;

#[derive(Clone)]
pub struct LbcEnv;

#[async_trait]
impl Application for LbcEnv {
    type Deployment = DeploymentPlan;

    type NodeClient = NodeHttpClient;

    type NodeConfig = RunConfig;

    fn external_node_client(source: &ExternalNodeSource) -> Result<Self::NodeClient, DynError> {
        let endpoint = Url::parse(source.endpoint())?;
        let basic_auth = external_basic_auth(&endpoint);

        Ok(NodeHttpClient::from_url_with_basic_auth(
            endpoint, basic_auth,
        ))
    }

    fn build_node_client(access: &NodeAccess) -> Result<Self::NodeClient, DynError> {
        let base_url = access.api_base_url()?;

        Ok(NodeHttpClient::from_url(base_url))
    }

    fn node_readiness_probe() -> ReadinessProbe {
        ReadinessProbe::Http {
            path: lb_http_api_common::paths::CRYPTARCHIA_INFO,
        }
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

/// Backend that provisions the Logos cluster application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LbcClusterBackend {
    #[default]
    Local,
    Compose,
    K8s,
}

/// Scenario builder for Logos blockchain clusters over the unified
/// app-composition model.
///
/// The cluster is registered as an application when the scenario is built;
/// the backend provisioner is selected with [`Self::with_backend`]. Deploy the
/// built scenario with [`AppHostDeployer`].
pub struct ScenarioBuilder {
    inner: AppHostScenarioBuilder,
    app: LbcClusterApp,
    backend: LbcClusterBackend,
}

impl ScenarioBuilder {
    #[must_use]
    pub fn new(deployment_provider: Box<dyn DeploymentProvider<DeploymentPlan>>) -> Self {
        Self {
            inner: AppHost::scenario(),
            app: LbcClusterApp::new(deployment_provider),
            backend: LbcClusterBackend::default(),
        }
    }

    #[must_use]
    pub fn deployment_with(f: impl FnOnce(DeploymentBuilder) -> DeploymentBuilder) -> Self {
        let topology = f(DeploymentBuilder::new(TopologyConfig::empty()));

        Self::new(Box::new(topology)).with_block_feed()
    }

    #[must_use]
    pub const fn with_backend(mut self, backend: LbcClusterBackend) -> Self {
        self.backend = backend;
        self
    }

    #[must_use]
    pub fn with_block_feed(mut self) -> Self {
        self.app = self.app.with_block_feed();
        self
    }

    #[must_use]
    pub fn with_wallet_config(mut self, wallet: WalletConfig) -> Self {
        self.app = self.app.with_wallet_config(wallet);
        self
    }

    #[must_use]
    pub fn with_external_node(mut self, node: ExternalNodeSource) -> Self {
        self.app = self.app.with_external_node(node);
        self
    }

    #[must_use]
    pub fn with_external_only(mut self) -> Self {
        self.app = self.app.with_external_only();
        self
    }

    #[must_use]
    pub fn with_run_duration(mut self, duration: Duration) -> Self {
        self.inner = self.inner.with_run_duration(duration);
        self
    }

    #[must_use]
    pub fn with_workload<W>(mut self, workload: W) -> Self
    where
        W: Workload<AppHostEnv> + 'static,
    {
        self.inner = self.inner.with_workload(workload);
        self
    }

    #[must_use]
    pub fn with_expectation<X>(mut self, expectation: X) -> Self
    where
        X: Expectation<AppHostEnv> + 'static,
    {
        self.inner = self.inner.with_expectation(expectation);
        self
    }

    pub fn build(self) -> Result<LbcScenario, ScenarioBuildError> {
        let Self {
            inner,
            app,
            backend,
        } = self;

        match backend {
            LbcClusterBackend::Local => inner.with_app(app),
            LbcClusterBackend::Compose => inner.with_app_using(app, ComposeProvisioner::default()),
            LbcClusterBackend::K8s => inner.with_app_using(app, K8sClusterProvisioner),
        }
        .build()
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

    let Some(genesis_block): Option<GenesisBlock> = deployment.config.genesis_block.clone() else {
        return;
    };

    let genesis_block = postprocess::apply_wallet_genesis_overrides(
        &mut node_configs,
        &genesis_block,
        deployment.config.blend_core_nodes,
        &wallet_accounts,
        key_id_for_preload_backend,
        deployment.config.test_context.as_deref(),
        deployment.config.sdp_funding_config,
        deployment.config.genesis_time(),
    );
    deployment.config.genesis_block = Some(genesis_block);

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
        self.with_expectation(FailureDiagnosticsExpectation::new(
            ConsensusLiveness::default(),
        ))
    }

    fn expect_cluster_fork_monitor(self) -> Self {
        self.with_expectation(FailureDiagnosticsExpectation::new(
            ClusterForkMonitor::default(),
        ))
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
