//! Centralized well-known wire fixtures for every Mantle codec.
//!
//! Each [`nom_wire_fixtures!`](super::nom_wire_fixtures) below emits the type's
//! `WireExamples` impl — the fixture the codec traits require — plus a
//! `#[cfg(test)]` round-trip test. Gathering them in one module keeps the type
//! definitions clean (`#[derive(NomCodec)]` only) and gives a single auditable
//! list of every golden vector.

mod channel;
mod genesis;
mod ledger;
mod ops;
mod tx;
