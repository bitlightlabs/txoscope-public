//! Simulation, invariant, and metrics helpers for txoscope.

pub mod invariants;
pub mod metrics;
pub use invariants::{InvariantChecker, InvariantViolation, InvariantViolationKind};
pub use metrics::{MetricsDelta, SimMetrics};
