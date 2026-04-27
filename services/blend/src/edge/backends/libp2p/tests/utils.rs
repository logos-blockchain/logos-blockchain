use core::num::{NonZeroU64, NonZeroUsize};

use lb_blend::scheduling::membership::Membership;
use lb_utils::blake_rng::BlakeRng;
use libp2p::{PeerId, identity::Keypair};
use rand::SeedableRng as _;
use tokio::sync::mpsc;

use crate::{
    edge::backends::libp2p::{BlendSwarm, swarm::Command},
    test_utils::PROTOCOL_NAME,
};

pub struct TestSwarm {
    pub swarm: BlendSwarm<BlakeRng>,
    pub command_sender: mpsc::Sender<Command>,
}

pub struct SwarmBuilder {
    membership: Membership<PeerId>,
    max_dial_attempts: Option<NonZeroU64>,
    replication_factor: Option<NonZeroUsize>,
}

impl SwarmBuilder {
    pub fn new(membership: Membership<PeerId>) -> Self {
        Self {
            membership,
            max_dial_attempts: None,
            replication_factor: None,
        }
    }

    pub fn with_max_dial_attempts(mut self, max_dial_attempts: u64) -> Self {
        self.max_dial_attempts = Some(max_dial_attempts.try_into().unwrap());
        self
    }

    pub fn with_replication_factor(mut self, replication_factor: usize) -> Self {
        self.replication_factor = Some(replication_factor.try_into().unwrap());
        self
    }

    pub fn build(self) -> TestSwarm {
        let (command_sender, command_receiver) = mpsc::channel(100);

        let swarm = BlendSwarm::new_test(
            &Keypair::generate_ed25519(),
            self.membership,
            command_receiver,
            self.max_dial_attempts
                .unwrap_or_else(|| 3u64.try_into().unwrap()),
            BlakeRng::from_entropy(),
            PROTOCOL_NAME,
            self.replication_factor
                .unwrap_or_else(|| 1usize.try_into().unwrap()),
        );

        TestSwarm {
            swarm,
            command_sender,
        }
    }
}
