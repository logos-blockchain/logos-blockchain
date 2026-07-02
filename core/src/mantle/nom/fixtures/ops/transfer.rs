use lb_core_macros::nom_wire_fixtures;
use lb_key_management_system_keys::keys::ZkPublicKey;
use num_bigint::BigUint;

use crate::mantle::{
    Note, NoteId,
    ledger::{Inputs, Outputs},
    ops::transfer::TransferOp,
};

nom_wire_fixtures!(TransferOp, {
    Self {
        inputs: Inputs::new([NoteId(BigUint::from(123u64).into())]),
        outputs: Outputs::new([Note::new(1000, ZkPublicKey::from(BigUint::from(42u64)))]),
    }
} => "017b0000000000000000000000000000000000000000000000000000000000000001e8030000000000002a00000000000000000000000000000000000000000000000000000000000000");
