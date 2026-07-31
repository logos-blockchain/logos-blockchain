use lb_codec::codec_fixtures;
use lb_key_management_system_keys::keys::ZkPublicKey;
use num_bigint::BigUint;

use crate::mantle::ledger::{Inputs, Note, NoteId, Outputs};

codec_fixtures!(NoteId, Self(BigUint::from(123u64).into()) => "7b00000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(Note, Self::new(1000, ZkPublicKey::from(BigUint::from(42u64))) => "e8030000000000002a00000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(Inputs, Self::new([NoteId(BigUint::from(123u64).into())]) => "017b00000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(Outputs, Self::new([Note::new(1000, ZkPublicKey::from(BigUint::from(42u64)))]) => "01e8030000000000002a00000000000000000000000000000000000000000000000000000000000000");
