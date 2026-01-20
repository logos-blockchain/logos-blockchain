use libp2p::StreamProtocol;

pub const REPLICATION_PROTOCOL: StreamProtocol =
    StreamProtocol::new("/logos-blockchain/da/1.0.0/replication");
pub const DISPERSAL_PROTOCOL: StreamProtocol =
    StreamProtocol::new("/logos-blockchain/da/1.0.0/dispersal");
pub const SAMPLING_PROTOCOL: StreamProtocol =
    StreamProtocol::new("/logos-blockchain/da/1.0.0/sampling");
