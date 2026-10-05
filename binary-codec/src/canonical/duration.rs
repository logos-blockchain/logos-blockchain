use core::time::Duration;

use super::{BinaryEncode, codec_fixtures};

// A duration: its whole seconds, then the nanoseconds past the last whole
// second, so two durations encode alike exactly when they are equal.
impl BinaryEncode for Duration {
    fn encoded_length(&self) -> usize {
        self.as_secs().encoded_length() + self.subsec_nanos().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.as_secs().encode_into(out);
        self.subsec_nanos().encode_into(out);
    }
}

codec_fixtures!(
    Duration,
    encode_only,
    Duration::from_secs(1) => "010000000000000000000000",
    Duration::new(1, 500_000_000) => "01000000000000000065cd1d"
);
