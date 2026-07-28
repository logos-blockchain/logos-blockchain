use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_wire::wire_fixtures;
use num_bigint::BigUint;

use crate::mantle::ledger::{Inputs, Note, NoteId, Outputs};

wire_fixtures!(NoteId, Self(BigUint::from(123u64).into()) => "7b00000000000000000000000000000000000000000000000000000000000000");
wire_fixtures!(Note, Self::new(1000, ZkPublicKey::from(BigUint::from(42u64))) => "e8030000000000002a00000000000000000000000000000000000000000000000000000000000000");
wire_fixtures!(Inputs, Self::new([NoteId(BigUint::from(123u64).into())]) => "017b00000000000000000000000000000000000000000000000000000000000000");
wire_fixtures!(Outputs, Self::new([Note::new(1000, ZkPublicKey::from(BigUint::from(42u64)))]) => "01e8030000000000002a00000000000000000000000000000000000000000000000000000000000000");
