//! SQLite [`StateStore`](txoscope_core::backend::StateStore) backend.
//!
//! Persists accounts, scopes, UTXOs, locks, operations, and intents with
//! transactional updates so a process can recover after restart.

mod codec;
mod sqlite;

pub use sqlite::SqliteStateStore;
