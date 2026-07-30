use lb_codec::codec_fixtures;

use crate::Epoch;

codec_fixtures!(Epoch, Self::new(1) => "01000000");
