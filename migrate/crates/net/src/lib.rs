//! Transport for BootShift Migrate: the [`Link`] trait the sender and receiver talk over, and the
//! in-memory [`FaultyLink`] that stands in for it in tests, able to drop, duplicate and cut frames
//! and reconnect — the faults a real TLS-over-TCP link suffers (plan.md task 2.3, §I3).
//!
//! The real TLS transport (rustls + SPAKE2 pairing + length framing) lands on this same `Link`
//! trait, so every test written against `FaultyLink` holds for it unchanged.

pub mod link;

pub use link::{Control, Endpoint, Fault, FaultyLink, Frame, Link, LinkError, Side};
