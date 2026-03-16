use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

use libp2p::PeerId;

use super::types::{BanStatus, OffenseKind};
use crate::config::BanningConfig;

/// Record of a peer's ban status
#[derive(Clone, Debug)]
pub struct PeerRecord {
    ban_status: BanStatus,
}

impl PeerRecord {
    fn set_ban(&mut self, banned_until: SystemTime, offense: OffenseKind, context: Option<String>) {
        self.ban_status = BanStatus::Banned {
            banned_until,
            offense,
            context,
        };
    }

    fn clear_ban(&mut self) {
        self.ban_status = BanStatus::NotBanned;
    }
}

/// In-memory store for banned peers
/// Note: This is not persistent across restarts, but can easily be extended to
/// do so.
#[derive(Debug, Clone, Default)]
pub struct BanStore {
    peers: HashMap<PeerId, PeerRecord>,
    config: BanningConfig,
}

impl BanStore {
    /// Create a new `BanStore` with the given configuration
    pub fn new(config: &BanningConfig) -> Self {
        let mut store = Self {
            peers: HashMap::new(),
            config: config.clone(),
        };
        // Pre-populate blacklisted peers
        for peer in &config.blacklist {
            store.peers.insert(
                *peer,
                PeerRecord {
                    ban_status: BanStatus::Banned {
                        banned_until: SystemTime::now()
                            + config.ban_duration(OffenseKind::BlackListed),
                        offense: OffenseKind::BlackListed,
                        context: Some("Blacklisted by config".to_owned()),
                    },
                },
            );
        }
        store
    }

    /// Get the ban expiry check interval
    pub const fn get_ban_expiry_check_interval(&self) -> Duration {
        self.config.ban_expiry_check_interval
    }

    /// Get the ban state of a peer
    pub fn get_ban_state(&mut self, peer: &PeerId) -> BanStatus {
        // Refresh status now
        if let Some(record) = self.peers.get_mut(peer)
            && let BanStatus::Banned { banned_until, .. } = record.ban_status
            && SystemTime::now() >= banned_until
        {
            record.clear_ban();
        }
        // Return current status
        self.peers
            .get(peer)
            .map_or(BanStatus::NotBanned, |ban_state| {
                ban_state.ban_status.clone()
            })
    }

    /// Apply a ban to a peer for a given offense kind - the ban period is
    /// determined by the configuration
    pub fn ban_peer(
        &mut self,
        peer: PeerId,
        offense: OffenseKind,
        context: Option<String>,
    ) -> BanStatus {
        if self.config.whitelist.contains(&peer) {
            return BanStatus::NotBanned;
        }
        if self.config.blacklist.contains(&peer) {
            return self
                .peers
                .get(&peer)
                .expect("blacklisted peers are pre-populated")
                .ban_status
                .clone();
        }

        let entry = self.peers.entry(peer).or_insert_with(|| PeerRecord {
            ban_status: BanStatus::default(),
        });
        let banned_until = SystemTime::now() + self.config.ban_duration(offense);
        entry.set_ban(banned_until, offense, context);

        entry.ban_status.clone()
    }

    /// Unban a peer
    pub fn unban_peer(&mut self, peer: &PeerId) -> bool {
        if let Some(rec) = self.peers.get_mut(peer) {
            return if matches!(rec.ban_status, BanStatus::NotBanned) {
                false
            } else {
                rec.clear_ban();
                true
            };
        }
        false
    }

    /// Update the ban state of all peers, unbanning those whose ban period has
    /// expired, returning a list of just unbanned peers
    pub fn update_ban_state(&mut self) -> Vec<PeerId> {
        let now = SystemTime::now();
        let mut just_unbanned = Vec::new();

        for (peer, rec) in &mut self.peers {
            if let BanStatus::Banned { banned_until, .. } = rec.ban_status
                && now >= banned_until
            {
                rec.clear_ban();
                just_unbanned.push(*peer);
            }
        }
        just_unbanned
    }

    /// Get a list of all currently banned peers with the banned until time and
    /// offense kind
    pub fn get_active_bans(&mut self) -> Vec<(PeerId, BanStatus)> {
        self.update_ban_state();

        self.peers
            .iter()
            .map(|(p, st)| (*p, st.ban_status.clone()))
            .filter(|(_, st)| matches!(st, BanStatus::Banned { .. }))
            .collect()
    }
}
