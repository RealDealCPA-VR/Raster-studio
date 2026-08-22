//! Photoshop `.psd` reading and writing.
//!
//! An original implementation written from the published format
//! documentation. Nothing here is derived from another project's source.
//!
//! # Untrusted input
//!
//! A `.psd` arrives from someone else, and every length, count and offset in
//! it is attacker-controlled. Parsing therefore validates before it allocates,
//! uses checked arithmetic, and returns typed errors — a malformed file must
//! never panic, hang, or exhaust memory.
