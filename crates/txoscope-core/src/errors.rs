use std::fmt;

use crate::{
	AccountId, Availability, OnchainState, OperationId, OperationPhase, OutPoint, WalletScopeId,
};

/// A candidate excluded by ownership, capability or availability checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetCandidateRejection {
	pub outpoint: OutPoint,
	pub reason: DomainError,
}

/// Errors returned by domain operations and store backends.
///
/// Match variants to handle errors programmatically. Display messages are for
/// diagnostics and may change; downstream matches must allow future variants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DomainError {
	/// The caller supplied an invalid request; no state was changed.
	InvalidRequest {
		reason: String,
	},
	AccountNotFound(AccountId),
	ScopeNotFound(WalletScopeId),
	/// The account cannot provide the capabilities required by this operation.
	MissingAccountCapabilities {
		account_id: AccountId,
	},
	/// No registered scope can satisfy the operation's requirements.
	NoEligibleScope {
		account_id: AccountId,
	},
	MissingCapabilities {
		scope_id: WalletScopeId,
	},
	InsufficientFunds,
	/// Selected candidates carry fewer asset units than requested.
	InsufficientAsset {
		needed: u128,
		have: u128,
	},
	/// Selected candidates carry fewer sats than requested (channel value + fee budget).
	InsufficientValue {
		needed: u128,
		have: u128,
	},
	/// All supplied asset candidates were excluded before amount selection.
	NoEligibleAssetCandidates {
		rejections: Vec<AssetCandidateRejection>,
	},
	NoCandidateAvailable,
	InvalidScopeOwnership {
		scope_id: WalletScopeId,
		account_id: AccountId,
	},
	UtxoNotFound(OutPoint),
	UtxoUnavailable {
		outpoint: OutPoint,
		availability: Availability,
	},
	UtxoPolicyBlocked {
		outpoint: OutPoint,
		reason: String,
	},
	IntentNotFound(OperationId),
	IntentNotPending(OperationId),
	IntentNotCommitted(OperationId),
	IntentConflict {
		operation_id: OperationId,
	},
	OperationNotFound(OperationId),
	InvalidOperationTransition {
		operation_id: OperationId,
		from: OperationPhase,
		to: OperationPhase,
	},
	PlannedInputsMismatch {
		operation_id: OperationId,
	},
	ObservationConflict {
		outpoint: OutPoint,
		onchain_state: OnchainState,
	},
	CorruptedState(String),
	Storage(String),
}

impl fmt::Display for DomainError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidRequest { reason } => write!(f, "invalid request: {reason}"),
			Self::AccountNotFound(id) => write!(f, "account not found: {id}"),
			Self::ScopeNotFound(id) => write!(f, "scope not found: {id}"),
			Self::MissingAccountCapabilities { account_id } => {
				write!(f, "account {account_id} lacks required capabilities")
			},
			Self::NoEligibleScope { account_id } => {
				write!(f, "no eligible scope for account {account_id}")
			},
			Self::MissingCapabilities { scope_id } => {
				write!(f, "scope {scope_id} lacks required capabilities")
			},
			Self::InsufficientFunds => f.write_str("insufficient funds"),
			Self::InsufficientAsset { needed, have } => {
				write!(f, "insufficient asset amount: need {needed}, have {have}")
			},
			Self::InsufficientValue { needed, have } => {
				write!(f, "insufficient bitcoin value: need {needed} sats, have {have} sats")
			},
			Self::NoEligibleAssetCandidates { rejections } => {
				f.write_str("none of the supplied asset inputs can be used")?;
				for rejection in rejections {
					write!(f, "; {}: {}", rejection.outpoint, rejection.reason)?;
				}
				Ok(())
			},
			Self::NoCandidateAvailable => f.write_str("no candidate is available"),
			Self::InvalidScopeOwnership { scope_id, account_id } => {
				write!(f, "scope {scope_id} does not belong to account {account_id}")
			},
			Self::UtxoNotFound(outpoint) => write!(f, "UTXO not found: {outpoint}"),
			Self::UtxoUnavailable { outpoint, availability } => {
				write!(f, "UTXO {outpoint} is unavailable ({availability:?})")
			},
			Self::UtxoPolicyBlocked { outpoint, reason } => {
				write!(f, "UTXO {outpoint} is blocked by policy: {reason}")
			},
			Self::IntentNotFound(id) => write!(f, "intent not found for operation {id}"),
			Self::IntentNotPending(id) => write!(f, "intent for operation {id} is not pending"),
			Self::IntentNotCommitted(id) => write!(f, "intent for operation {id} is not committed"),
			Self::IntentConflict { operation_id } => {
				write!(
					f,
					"intent conflicts with the recorded selection for operation {operation_id}"
				)
			},
			Self::OperationNotFound(id) => write!(f, "operation not found: {id}"),
			Self::InvalidOperationTransition { operation_id, from, to } => {
				write!(f, "operation {operation_id} cannot advance from {from:?} to {to:?}")
			},
			Self::PlannedInputsMismatch { operation_id } => {
				write!(
					f,
					"actual inputs do not match the planned inputs for operation {operation_id}"
				)
			},
			Self::ObservationConflict { outpoint, onchain_state } => {
				write!(f, "observation conflicts with UTXO {outpoint} ({onchain_state:?})")
			},
			Self::CorruptedState(reason) => write!(f, "corrupted state: {reason}"),
			Self::Storage(reason) => write!(f, "storage error: {reason}"),
		}
	}
}

impl std::error::Error for DomainError {}

pub type DomainResult<T> = Result<T, DomainError>;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn errors_integrate_with_standard_error_handling() {
		fn fail() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
			Err(DomainError::InvalidRequest { reason: "duplicate input".into() })?
		}
		let error = fail().unwrap_err();
		assert_eq!(error.to_string(), "invalid request: duplicate input");
		assert!(error.source().is_none());
		assert_eq!(
			error.downcast_ref::<DomainError>(),
			Some(&DomainError::InvalidRequest { reason: "duplicate input".into() })
		);
	}
}
