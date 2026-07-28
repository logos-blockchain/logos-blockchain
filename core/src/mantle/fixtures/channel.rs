use lb_wire::wire_fixtures;

use crate::mantle::channel::{SlotTimeframe, SlotTimeout};

wire_fixtures!(SlotTimeframe, Self::from(0x0403_0201u32) => "01020304");
wire_fixtures!(SlotTimeout, Self::from(0x0403_0201u32) => "01020304");
