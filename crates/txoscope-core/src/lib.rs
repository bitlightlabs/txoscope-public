//! Domain model and invariant-preserving primitives for txoscope.
//!
//! This crate defines accounts, wallet scopes, capabilities, managed UTXOs,
//! observations, reconciliation, selection, operations, intents, locks, and
//! store traits.
//!
//! Application code should prefer the `txoscope` facade crate.
//! Store backends and advanced integrations use the [`backend`] module.

mod account;
mod asset_selection;
mod capability;
mod errors;
mod ids;
mod lock;
mod observation;
mod operation;
mod orchestrator;
mod policy;
mod reconciliation;
mod scope;
mod selection_request;
mod selector;
mod state;
mod store;
mod utxo;
mod utxo_status;

pub use account::Account;
pub use asset_selection::{AssetCandidate, AssetSelectionConstraints, AssetSelectionPolicy};
pub use capability::{Capability, CapabilitySet};
pub use errors::{AssetCandidateRejection, DomainError, DomainResult};
pub use ids::{AccountId, OperationId, WalletScopeId};
pub use lock::LockPurpose;
pub use observation::{
	ObservationScope, ObservationSnapshot, ObservationSource, ObservedConfirmation,
	ObservedSpendStatus, ObservedUtxo,
};
pub use operation::{OperationBinding, OperationPhase};
pub use orchestrator::CleanupReport;
pub use scope::{DerivationScope, DescriptorRef, WalletScope};
pub use state::{Availability, OnchainState, UtxoRole};
pub use utxo::{ManagedUtxo, ObservationConflict, OutPoint};
pub use utxo_status::{UtxoLockStatus, UtxoStatus};

/// Low-level extension points for facade and store backend crates.
///
/// Application-facing callers should prefer the `txoscope` facade crate.
/// Backend crates use this module to implement persistence and internal state
/// transitions without exposing those APIs from the root prelude.
pub mod backend {
	pub use crate::lock::{LockPurpose, OperationLock};
	pub use crate::operation::{
		IntentStatus, OperationIntent, OperationSelection, PersistedOperation, SelectionEffect,
	};
	pub use crate::orchestrator::{Orchestrator, RecoveryReport};
	pub use crate::reconciliation::{ReconcilePlan, ReconciliationAction, ReconciliationPlanner};
	pub use crate::selection_request::{
		BtcSendRequest, ManualReservationExactRequest, ManualReservationFirstAvailableRequest,
		PlannedExactOutpointsRequest, PlannedSpecificOutpointRequest, SelectionRequest,
	};
	pub use crate::store::{StateBatch, StateRead, StateStore};
}
