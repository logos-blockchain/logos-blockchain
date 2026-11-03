use lb_binary_codec::canonical::codec_fixtures;
use num_bigint::BigUint;

use crate::mantle::notes::{NoteCommitment, NoteNullifier};

codec_fixtures!(NoteCommitment, Self(BigUint::from(123u64).into()) => "7b00000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(NoteNullifier,Self(BigUint::from(123u64).into()) => "7b00000000000000000000000000000000000000000000000000000000000000");
