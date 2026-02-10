mod block_feed;
pub mod local;

use std::num::{NonZeroU64, NonZeroUsize};

use async_trait::async_trait;
pub use block_feed::BlockRecord;
use lb_node::config::RunConfig;
use testing_framework_core::{
    scenario::{Application, DynError, FeedRuntime, ScenarioBuilder as CoreScenarioBuilder},
    topology::{DeploymentProvider, DeploymentSeed, DynTopologyError},
};
use testing_framework_runner_local::{ManualCluster, ProcessDeployer};

use crate::{
    framework::block_feed::{BlockFeedRuntime, prepare_block_feed},
    node::{
        DeploymentPlan, NodeHttpClient,
        configs::{
            deployment::{DeploymentBuilder, TopologyConfig},
            key_id_for_preload_backend, postprocess,
            wallet::WalletConfig,
        },
    },
    workloads::{ConsensusLiveness, transaction},
};

pub type ScenarioBuilder = CoreScenarioBuilder<LbcEnv>;
pub type ScenarioBuilderWith = ScenarioBuilder;

pub type LbcLocalDeployer = ProcessDeployer<LbcEnv>;

pub type LbcManualCluster = ManualCluster<LbcEnv>;

pub struct LbcEnv;

#[async_trait]
impl Application for LbcEnv {
    type Deployment = DeploymentPlan;

    type NodeClient = NodeHttpClient;

    type NodeConfig = RunConfig;

    type FeedRuntime = BlockFeedRuntime;

    async fn prepare_feed(
        client: Self::NodeClient,
    ) -> Result<(<Self::FeedRuntime as FeedRuntime>::Feed, Self::FeedRuntime), DynError> {
        prepare_block_feed(client).await
    }
}

pub trait CoreBuilderExt: Sized {
    #[must_use]
    fn deployment_with(f: impl FnOnce(DeploymentBuilder) -> DeploymentBuilder) -> Self;

    #[must_use]
    fn with_wallet_config(self, wallet: WalletConfig) -> Self;
}

impl CoreBuilderExt for ScenarioBuilder {
    fn deployment_with(f: impl FnOnce(DeploymentBuilder) -> DeploymentBuilder) -> Self {
        let topology = f(DeploymentBuilder::new(TopologyConfig::empty()));
        Self::new(Box::new(topology))
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
        &wallet_accounts,
        key_id_for_preload_backend,
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
    fn expect_consensus_liveness(self) -> Self;

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

    fn expect_consensus_liveness(self) -> Self {
        self.with_expectation(ConsensusLiveness::default())
    }

    fn initialize_wallet(self, total_funds: u64, users: usize) -> Self {
        let Some(user_count) = nonzero_users(users) else {
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
        if let Some(value) = nonzero_users(users) {
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

const fn nonzero_users(users: usize) -> Option<NonZeroUsize> {
    NonZeroUsize::new(users)
}
