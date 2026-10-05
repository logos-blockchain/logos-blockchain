use lb_binary_codec::canonical::codec_fixtures;

use crate::header::{ContentId, HeaderId};

codec_fixtures!(HeaderId, Self::from([0x11u8; 32]) => "1111111111111111111111111111111111111111111111111111111111111111");

codec_fixtures!(ContentId, Self::from([0x22u8; 32]) => "2222222222222222222222222222222222222222222222222222222222222222");
