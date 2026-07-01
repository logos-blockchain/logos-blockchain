use lb_core_macros::nom_wire_fixtures;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};

nom_wire_fixtures!(
    Ed25519PublicKey,
    Self::from_bytes(&[1u8; _]).unwrap() => "0101010101010101010101010101010101010101010101010101010101010101"
);
nom_wire_fixtures!(
    ZkPublicKey,
    Fr::from(1u64).into() => "0100000000000000000000000000000000000000000000000000000000000000"
);
