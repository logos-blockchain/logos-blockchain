use ark_ff::AdditiveGroup as _;
use lb_binary_codec::canonical::codec_fixtures;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::ZkPublicKey;

use crate::mantle::transactions::genesis_tx::{
    ChainId, GenesisAllocation, GenesisParameters, GenesisTime,
};

codec_fixtures!(
    ChainId,
    "logos-chain-1".to_owned().try_into().unwrap() => "0d6c6f676f732d636861696e2d31"
);

codec_fixtures!(
    GenesisTime,
    Self::new(1000) => "e8030000",
    Self::new(u32::MAX) => "ffffffff"
);

codec_fixtures!(
    GenesisParameters,
    Self { chain_id: ChainId::try_from("logos-chain-1".to_owned()).unwrap(), start_time: GenesisTime::new(1000), epoch_nonce: Fr::ZERO } => "0d6c6f676f732d636861696e2d31e80300000000000000000000000000000000000000000000000000000000000000000000"
);

codec_fixtures!(
    GenesisAllocation,
    Self { value: 1000, public_key: ZkPublicKey::new(Fr::from(7u64)) }
        => "e8030000000000000700000000000000000000000000000000000000000000000000000000000000"
);
