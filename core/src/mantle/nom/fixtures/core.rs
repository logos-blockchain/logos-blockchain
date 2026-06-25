use lb_core_macros::nom_wire_fixtures;
use lb_cryptarchia_engine::Epoch;

nom_wire_fixtures!(Epoch, Self::new(1) => "01000000");
