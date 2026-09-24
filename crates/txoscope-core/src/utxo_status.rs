//! Caller-facing, read-only diagnostics for a managed output.
use crate::lock::{LockPurpose, OperationLock};
use crate::{
	AccountId, Availability, DomainError, ManagedUtxo, OnchainState, OperationBinding, OperationId,
	OutPoint, WalletScopeId,
};

/// The persisted lock, distinct from the UTXO's operation binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtxoLockStatus {
	pub operation_id: OperationId,
	pub purpose: LockPurpose,
	/// Recorded UTC Unix deadline; passing it does not itself release the lock.
	pub expires_at: Option<u64>,
}

/// Recorded state and generic input selectability, without applying request capabilities.
///
/// An expired reservation remains unavailable until reservation cleanup releases it.
/// A released broadcast input may have no operation or lock and still be unspendable.
/// This query does not reserve an input or guarantee future selection success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtxoStatus {
	pub outpoint: OutPoint,
	pub account_id: AccountId,
	pub scope_id: WalletScopeId,
	pub value_sats: u64,
	pub onchain_state: OnchainState,
	pub availability: Availability,
	/// Persisted ownership and phase, including after selection and signing.
	pub operation: Option<OperationBinding>,
	pub lock: Option<UtxoLockStatus>,
	/// The primary reason the input cannot currently be locked for selection.
	pub unavailable_reason: Option<DomainError>,
}

impl UtxoStatus {
	pub fn is_selectable(&self) -> bool {
		self.unavailable_reason.is_none()
	}

	pub(crate) fn from_records(utxo: ManagedUtxo, lock: Option<OperationLock>) -> Self {
		let unavailable_reason = if utxo.can_be_locked(lock.is_some()) {
			None
		} else {
			let reason = if let Some(binding) = &utxo.operation_binding {
				format!(
					"input is owned by operation {} ({:?})",
					binding.operation_id, binding.phase
				)
			} else if lock.is_some() {
				"input has an active lock".into()
			} else if let Some(conflict) = &utxo.observation_conflict {
				format!("input has conflicting observations: {}", conflict.message)
			} else {
				format!(
					"input is not spendable (availability: {:?}, on-chain state: {:?})",
					utxo.availability, utxo.onchain_state
				)
			};
			Some(DomainError::UtxoPolicyBlocked { outpoint: utxo.outpoint.clone(), reason })
		};
		Self {
			outpoint: utxo.outpoint,
			account_id: utxo.account_id,
			scope_id: utxo.scope_id,
			value_sats: utxo.value_sats,
			onchain_state: utxo.onchain_state,
			availability: utxo.availability,
			operation: utxo.operation_binding,
			lock: lock.map(|lock| UtxoLockStatus {
				operation_id: lock.operation_id,
				purpose: lock.purpose,
				expires_at: lock.expires_at,
			}),
			unavailable_reason,
		}
	}
}
