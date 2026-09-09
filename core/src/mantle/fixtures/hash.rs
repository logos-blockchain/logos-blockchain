use lb_codec::codec_fixtures;

use crate::mantle::transactions::hash::{TxHash, TxHashPrefix};

codec_fixtures!(
    TxHash,
    Self([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000",
    Self([1u8; 32]) => "0101010101010101010101010101010101010101010101010101010101010101"
);

codec_fixtures!(
    TxHashPrefix,
    Self([0u8; _]) => "00000000000000000000000000000000",
    Self([1u8; _]) => "01010101010101010101010101010101"
);
