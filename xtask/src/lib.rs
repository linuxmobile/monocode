//! Shared logic for the build tasks.
//!
//! The binary is thin so the behaviour can be tested directly: the dev loop
//! manages child processes and cannot be asserted on from a command line.

pub mod dev;
pub mod package;
pub mod release;
pub mod version;
