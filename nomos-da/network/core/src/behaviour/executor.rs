use std::time::Duration;

use futures::StreamExt as _;
use libp2p::{PeerId, identity::Keypair, swarm::NetworkBehaviour};
use subnetworks_assignations::MembershipHandler;
use tokio::sync::{broadcast, mpsc::UnboundedSender};
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    addressbook::AddressBookHandler,
    maintenance::{
        balancer::{ConnectionBalancer, ConnectionBalancerBehaviour},
        monitor::{ConnectionMonitor, ConnectionMonitorBehaviour},
    },
    protocols::{
        dispersal::{
            executor::behaviour::DispersalExecutorBehaviour,
            validator::behaviour::DispersalValidatorBehaviour,
        },
        replication::behaviour::{ReplicationBehaviour, ReplicationConfig},
        sampling::{SamplingBehaviour, SubnetsConfig},
    },
};

/// Aggregated `NetworkBehaviour` composed of:
/// * Sampling
/// * Executor dispersal
/// * Validator dispersal
/// * Replication WARNING: Order of internal protocols matters as the first one
///   will be polled first until return a `Poll::Pending`.
/// 1) Sampling is the crucial one as we have to be responsive for consensus.
/// 2) Dispersal so we do not bottleneck executors.
/// 3) Replication is the least important (and probably the least used), it is
///    also dependant of dispersal.
#[derive(NetworkBehaviour)]
pub struct ExecutorBehaviour<Balancer, Monitor, Membership, HistoricMembership, Addressbook>
where
    Balancer: ConnectionBalancer,
    Monitor: ConnectionMonitor,
    Membership: MembershipHandler,
    HistoricMembership: MembershipHandler,
    Addressbook: AddressBookHandler,
{
    sampling: SamplingBehaviour<Membership, HistoricMembership, Addressbook>,
    executor_dispersal: DispersalExecutorBehaviour<Membership, Addressbook>,
    validator_dispersal: DispersalValidatorBehaviour<Membership>,
    replication: ReplicationBehaviour<Membership>,
    balancer: ConnectionBalancerBehaviour<Balancer, Addressbook>,
    monitor: ConnectionMonitorBehaviour<Monitor>,
}

impl<Balancer, Monitor, Membership, HistoricMembership, Addressbook>
    ExecutorBehaviour<Balancer, Monitor, Membership, HistoricMembership, Addressbook>
where
    Balancer: ConnectionBalancer,
    Monitor: ConnectionMonitor,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    <Membership as MembershipHandler>::NetworkId: Send,
    HistoricMembership: MembershipHandler + Clone + Send + Sync + 'static,
    <HistoricMembership as MembershipHandler>::NetworkId: Send,
    Addressbook: AddressBookHandler + Clone + Send + 'static,
{
    pub fn new(
        key: &Keypair,
        membership: Membership,
        addressbook: Addressbook,
        balancer: Balancer,
        monitor: Monitor,
        redial_cooldown: Duration,
        replication_config: ReplicationConfig,
        subnets_config: SubnetsConfig,
        refresh_sender: &broadcast::Sender<()>,
        balancer_stats_sender: UnboundedSender<<Balancer as ConnectionBalancer>::Stats>,
    ) -> Self {
        let peer_id = PeerId::from_public_key(&key.public());
        Self {
            sampling: SamplingBehaviour::new(
                peer_id,
                membership.clone(),
                addressbook.clone(),
                subnets_config,
                BroadcastStream::new(refresh_sender.subscribe()).filter_map(async |r| r.ok()),
            ),
            executor_dispersal: DispersalExecutorBehaviour::new(
                membership.clone(),
                addressbook.clone(),
            ),
            validator_dispersal: DispersalValidatorBehaviour::new(peer_id, membership.clone()),
            replication: ReplicationBehaviour::new(replication_config, peer_id, membership),
            balancer: ConnectionBalancerBehaviour::new(
                addressbook,
                balancer,
                Some(balancer_stats_sender),
                BroadcastStream::new(refresh_sender.subscribe()).filter_map(async |r| r.ok()),
            ),
            monitor: ConnectionMonitorBehaviour::new(monitor, redial_cooldown),
        }
    }

    pub const fn sampling_behaviour(
        &self,
    ) -> &SamplingBehaviour<Membership, HistoricMembership, Addressbook> {
        &self.sampling
    }

    pub const fn dispersal_executor_behaviour(
        &self,
    ) -> &DispersalExecutorBehaviour<Membership, Addressbook> {
        &self.executor_dispersal
    }

    pub const fn dispersal_validator_behaviour(&self) -> &DispersalValidatorBehaviour<Membership> {
        &self.validator_dispersal
    }

    pub const fn replication_behaviour(&self) -> &ReplicationBehaviour<Membership> {
        &self.replication
    }

    pub const fn sampling_behaviour_mut(
        &mut self,
    ) -> &mut SamplingBehaviour<Membership, HistoricMembership, Addressbook> {
        &mut self.sampling
    }

    pub const fn dispersal_executor_behaviour_mut(
        &mut self,
    ) -> &mut DispersalExecutorBehaviour<Membership, Addressbook> {
        &mut self.executor_dispersal
    }

    pub const fn dispersal_validator_behaviour_mut(
        &mut self,
    ) -> &mut DispersalValidatorBehaviour<Membership> {
        &mut self.validator_dispersal
    }

    pub const fn replication_behaviour_mut(&mut self) -> &mut ReplicationBehaviour<Membership> {
        &mut self.replication
    }

    pub const fn monitor_behaviour_mut(&mut self) -> &mut ConnectionMonitorBehaviour<Monitor> {
        &mut self.monitor
    }

    pub const fn monitor_behavior(&self) -> &ConnectionMonitorBehaviour<Monitor> {
        &self.monitor
    }

    pub const fn balancer_behaviour_mut(
        &mut self,
    ) -> &mut ConnectionBalancerBehaviour<Balancer, Addressbook> {
        &mut self.balancer
    }

    pub const fn balancer_behaviour(&self) -> &ConnectionBalancerBehaviour<Balancer, Addressbook> {
        &self.balancer
    }
}
