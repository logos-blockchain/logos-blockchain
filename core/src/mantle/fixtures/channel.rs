use lb_codec::codec_fixtures;

use crate::mantle::channel::{SlotTimeframe, SlotTimeout};

codec_fixtures!(SlotTimeframe, Self::from(0x0403_0201u32) => "01020304");
codec_fixtures!(SlotTimeout, Self::from(0x0403_0201u32) => "01020304");
