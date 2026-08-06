use lb_codec::codec_fixtures;

use crate::{Epoch, Slot};

codec_fixtures!(Epoch, Self::new(1) => "01000000");
codec_fixtures!(Slot, Self::from(1u64) => "0100000000000000");
