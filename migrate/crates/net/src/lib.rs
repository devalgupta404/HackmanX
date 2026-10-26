//! Transport for BootShift Migrate: length-prefixed framing over a TLS 1.3 byte stream, SPAKE2
//! pairing from a 6-word code, reconnect and back-pressure. The `FaultyLink` test transport lives
//! here too. Built out in plan.md Phase 1/3 (§I3).
//!
//! Skeleton: types land as the protocol (`migrate-core::proto`) and pairing are implemented.
