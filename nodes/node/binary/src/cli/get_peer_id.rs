use color_eyre::eyre::Result;
use lb_libp2p::ed25519;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
use libp2p::PeerId;

use super::GetPeerIdArgs;
use crate::UserConfig;

pub fn run(args: &GetPeerIdArgs) -> Result<()> {
    let user_config = deserialize_value_at_path::<UserConfig>(&args.config, OnUnknownKeys::Warn)?;

    let node_key = user_config.network.backend.swarm.node_key;
    let keypair = libp2p::identity::Keypair::from(ed25519::Keypair::from(node_key));
    let peer_id = PeerId::from(keypair.public());

    println!("{peer_id}");

    Ok(())
}
