use ark_ff::AdditiveGroup as _;
use lb_core_macros::nom_wire_fixtures;
use lb_groth16::Fr;
use time::OffsetDateTime;

use crate::mantle::transactions::genesis_tx::{ChainId, CryptarchiaParameter};

nom_wire_fixtures!(
    ChainId,
    "logos-chain-1".to_owned().try_into().unwrap() => "0d000000000000006c6f676f732d636861696e2d31"
);

nom_wire_fixtures!(
    CryptarchiaParameter,
    Self { chain_id: ChainId::try_from("logos-chain-1".to_owned()).unwrap(), genesis_time: OffsetDateTime::from_unix_timestamp(1000).unwrap(), epoch_nonce: Fr::ZERO } => "0d000000000000006c6f676f732d636861696e2d31e8030000000000000000000000000000000000000000000000000000000000000000000000000000"
);
