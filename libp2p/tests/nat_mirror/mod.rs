//! Shared helper for the spec-audit proofs of task `20261104T001156-96e84f`
//! (P2P-NAT-SOLUTION, LIP 138).
//!
//! The NAT traversal internals of `logos-blockchain-libp2p`
//! (`behaviour::nat::{state_machine, address_mapper, gateway_monitor, inner}`)
//! are crate-private. This module compiles those exact source files, unchanged,
//! into an integration-test crate so that a proof can drive the real state
//! machine, address mapper and gateway monitor without touching `src/`.
//!
//! The including crate root must provide the paths the sources use:
//!
//! ```ignore
//! #[path = "../nat_mirror/mod.rs"]
//! mod nat_mirror;
//! pub mod behaviour {
//!     pub mod nat {
//!         pub use crate::nat_mirror::{address_mapper, gateway_monitor, inner, state_machine};
//!     }
//! }
//! pub mod config {
//!     pub use logos_blockchain_libp2p::{GatewaySettings, NatMappingSettings, TraversalSettings};
//! }
//! pub use logos_blockchain_libp2p::NatMappingSettings;
//! ```
#![allow(
    dead_code,
    unused_imports,
    unused_qualifications,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "Unmodified crate sources compiled into a test crate; not every item is used."
)]

#[path = "../../src/behaviour/nat/state_machine/mod.rs"]
pub mod state_machine;

#[path = "../../src/behaviour/nat/address_mapper/mod.rs"]
pub mod address_mapper;

#[path = "../../src/behaviour/nat/gateway_monitor.rs"]
pub mod gateway_monitor;

/// `inner.rs` pasted verbatim, plus accessors that need its private items.
pub mod inner {
    include!("../../src/behaviour/nat/inner.rs");

    impl<Rng, Mapper, Detector> InnerNatBehaviour<Rng, Mapper, Detector>
    where
        Rng: RngCore + 'static,
        Mapper: address_mapper::NatMapper + 'static,
        Detector: crate::behaviour::nat::gateway_monitor::GatewayDetector + 'static,
    {
        /// Builds the behaviour exactly as `NatBehaviour::new` does, but with
        /// an injected mapper and gateway detector.
        pub fn for_audit(rng: Rng, settings: &TraversalSettings) -> Self {
            let address_mapper_behaviour = AddressMapperBehaviour::<Mapper>::new(settings.mapping);
            let gateway_monitor = GatewayMonitor::<Detector>::new(settings.gateway_monitor);
            Self::create(rng, settings, address_mapper_behaviour, gateway_monitor)
        }

        /// Debug rendering of the current NAT state machine state.
        pub fn audit_state(&self) -> String {
            super::state_of(&self.state_machine)
        }
    }
}

/// Debug rendering of a [`state_machine::StateMachine`]'s current state,
/// without the command channel internals.
pub fn state_of(state_machine: &state_machine::StateMachine) -> String {
    let full = format!("{state_machine:?}");
    full.split(", command_tx").next().unwrap_or(&full).to_owned()
}
