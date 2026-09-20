//! s1m as a library: the pieces later stages are built on.
//!
//! The binary in `src/main.rs` is still the scaffold CLI; the query pipeline
//! lands in later issues and calls into this crate rather than reimplementing
//! any of it.

pub mod jev;
pub mod parse;
pub mod scorer;
