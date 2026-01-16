//! PCP (Port Control Protocol) RFC 6887 implementation.
//!
//! Specification: <https://tools.ietf.org/html/rfc6887>
//!
//! ## Protocol Overview
//!
//! PCP enables clients to create explicit port mappings in NAT
//! devices.
//!
//! ## Message Flow
//!
//! ```text
//! Client                    PCP Server (NAT/Router)
//! ------                    -------------------
//! 1. ANNOUNCE(lifetime=0) ──────────────────────→ (Discover server, learn external IP)
//!    ←─────────────────── ANNOUNCE Response
//!    
//! 2. MAP(internal_port) ────────────────────────→ (Request port mapping)
//!    ←─────────────────── MAP Response(external_port, lifetime)
//!    
//! 3. MAP(lifetime=0) ───────────────────────────→ (Delete mapping)
//!    ←─────────────────── MAP Response(lifetime=0)
//! ```
//!
//! ## Wire Format
//!
//! All messages use network byte order. IPv4 addresses encoded as IPv4-mapped
//! IPv6.
//!
//! ```text
//! ANNOUNCE Request (24 bytes):
//! ┌─────────┬─────────┬─────────────┬─────────────┬──────────────────────┐
//! │ Version │ Opcode  │ Reserved    │ Lifetime    │ Client IPv6          │
//! │ (1)     │ (1)     │ (2)         │ (4)         │ (16)                 │
//! └─────────┴─────────┴─────────────┴─────────────┴──────────────────────┘
//!
//! MAP Request (60 bytes):
//! ┌─────────┬─────────┬─────────────┬─────────────┬──────────────────────┐
//! │ Version │ Opcode  │ Reserved    │ Lifetime    │ Client IPv6          │
//! │ (1)     │ (1)     │ (2)         │ (4)         │ (16)                 │
//! ├─────────┴─────────┴─────────────┴─────────────┴──────────────────────┤
//! │ Nonce (12) │ Proto │ Reserved │ Internal Port │ External Port        │
//! │            │ (1)   │ (3)      │ (2)           │ (2)                  │
//! ├────────────┴───────┴──────────┴───────────────┴──────────────────────┤
//! │ Suggested External IPv6 (16)                                         │
//! └───────────────────────────────────────────────────────────────────────┘
//!
//! ANNOUNCE Response (24 bytes):
//! ┌─────────┬─────────┬──────────┬─────────┬─────────────┬────────────────┐
//! │ Version │ Opcode  │ Result   │ Reserve │ Lifetime    │ Epoch Time     │
//! │ (1)     │ (1)     │ Code (1) │ d (1)   │ (4)         │ (4)            │
//! ├─────────┴─────────┴──────────┴─────────┴─────────────┴────────────────┤
//! │ Reserved (12)                                                        │
//! └──────────────────────────────────────────────────────────────────────┘
//!
//! MAP Response (60 bytes):
//! ┌─────────┬─────────┬──────────┬─────────┬─────────────┬────────────────┐
//! │ Version │ Opcode  │ Result   │ Reserve │ Lifetime    │ Epoch Time     │
//! │ (1)     │ (1)     │ Code (1) │ d (1)   │ (4)         │ (4)            │
//! ├─────────┴─────────┴──────────┴─────────┴─────────────┴────────────────┤
//! │ Reserved (12)                                                        │
//! ├──────────────────────────────────────────────────────────────────────┤
//! │ Nonce (12) │ Proto │ Reserved │ Internal Port │ Assigned Port        │
//! │            │ (1)   │ (3)      │ (2)           │ (2)                  │
//! ├────────────┴───────┴──────────┴───────────────┴──────────────────────┤
//! │ Assigned External IPv6 (16)                                         │
//! └───────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## RFC 6887 Implementation Status
//!
//! ### Implemented
//! - **Server Discovery**: ANNOUNCE opcode for PCP server detection
//! - **Inbound Mappings**: MAP opcode for port mapping creation/deletion
//! - **Mapping Renewal**: Extend mapping lifetimes before expiration
//!
//! ### Not Implemented
//! - **PEER Opcode**: Dynamic outbound mapping creation (rare use case)
//! - **IPv6 Support**: Native IPv6 client/server communication (IPv4 sufficient
//!   for current needs)
//! - **PCP Options**: `THIRD_PARTY`, `PREFER_FAILURE`, `FILTER` extensions
//!   (optional features)
//! - **Multiple Servers**: Multi-server redundancy and failover (single server
//!   adequate)
//! - **Version Negotiation**: Automatic fallback to older versions (PCP v2
//!   universally supported)
//!
//! ## Error Handling
//!
//! PCP defines standard result codes (0=success, 1-13=various errors).
//! Unknown codes are logged and treated as `InvalidResponse` errors.
//! Network errors trigger retry with increasing delays.

#![allow(
    dead_code,
    unused_imports,
    unused_variables,
    reason = "PCP will be merged in another PR"
)]

pub mod client;
pub mod connection;
pub mod mapping;
pub mod wire;
