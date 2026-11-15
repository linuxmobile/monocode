//! A small wrapper around Tokio's mutex.
//!
//! The host store is synchronous: `rusqlite` calls block, and the host is the
//! only writer. Holding an async mutex across those short calls serialises them
//! without handing them to a worker thread, and the calls are dominated by
//! SQLite's own file I/O rather than CPU, so they cannot starve the runtime the
//! way a long computation would.

pub use tokio::sync::Mutex;
