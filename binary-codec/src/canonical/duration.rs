use core::time::Duration;

use super::{BinaryEncode, codec_fixtures};

// A duration: its whole seconds,
impl BinaryEncode for Duration {
    fn encoded_length(&self) -> usize {
        self.as_secs().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.as_secs().encode_into(out);
    }
}

codec_fixtures!(Duration, encode_only, Duration::from_secs(1) => "0100000000000000");
