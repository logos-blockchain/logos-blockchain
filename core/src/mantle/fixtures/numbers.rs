use lb_core_macros::nom_wire_fixtures;
use lb_groth16::Fr;

nom_wire_fixtures!(u8, 0x07u8 => "07", 0u8 => "00");
nom_wire_fixtures!(u16, 1u16 => "0100", 0x0201u16 => "0102");
nom_wire_fixtures!(u32, 1u32 => "01000000", 0x0403_0201u32 => "01020304");
nom_wire_fixtures!(u64, 1u64 => "0100000000000000", 0x0807_0605_0403_0201u64 => "0102030405060708");
nom_wire_fixtures!(
    Fr,
    Self::from(1u64) => "0100000000000000000000000000000000000000000000000000000000000000"
);
