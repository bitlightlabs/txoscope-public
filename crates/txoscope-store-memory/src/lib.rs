//! In-memory [`StateStore`](txoscope_core::backend::StateStore) backend.
//!
//! Intended for tests, examples, simulation, and ephemeral usage.

mod memory;

pub use memory::InMemoryStateStore;
