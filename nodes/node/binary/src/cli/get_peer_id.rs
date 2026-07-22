use std::path::Path;

use color_eyre::eyre::Result;
use lb_libp2p::ed25519;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
use libp2p::PeerId;

use super::GetPeerIdArgs;
use crate::UserConfig;

/// Derives the libp2p `PeerId` from the node key in the user config at
/// `config_path`.
pub fn peer_id_from_config(config_path: &Path) -> Result<PeerId> {
    let user_config = deserialize_value_at_path::<UserConfig>(config_path, OnUnknownKeys::Warn)?;

    let node_key = user_config.network.backend.swarm.node_key;
    let keypair = libp2p::identity::Keypair::from(ed25519::Keypair::from(node_key));

    Ok(PeerId::from(keypair.public()))
}

pub fn run(args: &GetPeerIdArgs) -> Result<()> {
    let peer_id = peer_id_from_config(&args.config)?;

    println!("{peer_id}");

    Ok(())
}
