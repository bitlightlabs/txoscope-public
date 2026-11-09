use std::collections::BTreeSet;

use crate::{
	lock::{LockPurpose, OperationLock},
	Account, AccountId, CapabilitySet, DomainError, DomainResult, ManagedUtxo, OperationId,
	OutPoint, WalletScope, WalletScopeId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputSelectionSpec {
	TargetValue { amount_sats: u64 },
	ExactOutpoints { outpoints: Vec<OutPoint> },
	FirstSelectable { candidates: Vec<OutPoint> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationPurpose {
	BtcSend,
	PlannedSelection,
	ManualReservation,
}

impl From<OperationPurpose> for LockPurpose {
	fn from(value: OperationPurpose) -> Self {
		match value {
			OperationPurpose::BtcSend => Self::BtcSend,
			OperationPurpose::PlannedSelection => Self::PlannedSelection,
			OperationPurpose::ManualReservation => Self::ManualReservation,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSelectionRequest {
	pub(crate) account_id: AccountId,
	pub(crate) spec: InputSelectionSpec,
	pub(crate) required_capabilities: CapabilitySet,
}

impl InputSelectionRequest {
	pub(crate) fn new(
		account_id: impl Into<AccountId>, spec: InputSelectionSpec,
		required_capabilities: CapabilitySet,
	) -> Self {
		Self { account_id: account_id.into(), spec, required_capabilities }
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectionLockPolicy {
	pub(crate) purpose: OperationPurpose,
	pub(crate) expires_at: Option<u64>,
}

impl SelectionLockPolicy {
	pub(crate) fn new(purpose: OperationPurpose, expires_at: Option<u64>) -> Self {
		Self { purpose, expires_at }
	}

	pub(crate) fn lock_purpose(&self) -> LockPurpose {
		self.purpose.into()
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(test, derive(strum::EnumIter))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schema", schema(rename_all = "snake_case"))]
pub enum OperationPhase {
	Selected,
	Constructing,
	Signed,
	Broadcast,
	AwaitingConfirmation,
	RollingBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationBinding {
	pub operation_id: OperationId,
	pub phase: OperationPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum IntentStatus {
	Pending,
	Committed,
	Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedOperation {
	pub operation_id: OperationId,
	pub phase: OperationPhase,
	pub updated_at: u64,
	pub expires_at: Option<u64>,
}

impl PersistedOperation {
	/// Checks the forward lifecycle at the current stored phase. Returns false
	/// for an idempotent call. Backends must check this inside their atomic write.
	pub fn validate_advance(&self, completed_phase: OperationPhase) -> DomainResult<bool> {
		if self.phase == completed_phase {
			return Ok(false);
		}
		let valid_transition = matches!(
			(self.phase, completed_phase),
			(
				OperationPhase::Selected,
				OperationPhase::Constructing
					| OperationPhase::Signed
					| OperationPhase::Broadcast
					| OperationPhase::AwaitingConfirmation
			) | (
				OperationPhase::Constructing,
				OperationPhase::Signed
					| OperationPhase::Broadcast
					| OperationPhase::AwaitingConfirmation
			) | (
				OperationPhase::Signed,
				OperationPhase::Broadcast | OperationPhase::AwaitingConfirmation
			) | (OperationPhase::Broadcast, OperationPhase::AwaitingConfirmation)
		);
		if !valid_transition {
			return Err(DomainError::InvalidOperationTransition {
				operation_id: self.operation_id.clone(),
				from: self.phase,
				to: completed_phase,
			});
		}
		Ok(true)
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationIntent {
	pub selection: OperationSelection,
	pub status: IntentStatus,
	pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationRequest {
	pub(crate) operation_id: OperationId,
	pub(crate) selection: InputSelectionRequest,
	pub(crate) locking: SelectionLockPolicy,
}

impl OperationRequest {
	pub(crate) fn new(
		operation_id: impl Into<OperationId>, selection: InputSelectionRequest,
		locking: SelectionLockPolicy,
	) -> Self {
		Self { operation_id: operation_id.into(), selection, locking }
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedInputs {
	pub outpoints: Vec<OutPoint>,
	pub change_scope_id: WalletScopeId,
}

impl OperationSelection {
	pub(crate) fn from_request(req: &OperationRequest, selected_inputs: SelectedInputs) -> Self {
		let lock_purpose = req.locking.lock_purpose();
		let effects = selected_inputs
			.outpoints
			.iter()
			.cloned()
			.map(|outpoint| SelectionEffect::LockUtxo { outpoint, purpose: lock_purpose })
			.collect();
		Self {
			operation_id: req.operation_id.clone(),
			selected_outpoints: selected_inputs.outpoints,
			change_scope_id: selected_inputs.change_scope_id,
			effects,
			expires_at: req.locking.expires_at,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSelection {
	pub operation_id: OperationId,
	pub selected_outpoints: Vec<OutPoint>,
	pub change_scope_id: WalletScopeId,
	pub effects: Vec<SelectionEffect>,
	pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionEffect {
	LockUtxo { outpoint: OutPoint, purpose: LockPurpose },
}

impl OperationSelection {
	/// Validates a committed retry against current ownership without applying effects.
	/// Backends must read the operation, inputs, and locks from one consistent snapshot.
	/// Forward phases remain idempotent while every selected input is still owned.
	pub fn validate_committed_reservation(
		&self, operation: Option<PersistedOperation>,
		get_utxo: impl Fn(&OutPoint) -> DomainResult<Option<ManagedUtxo>>,
		get_lock: impl Fn(&OutPoint) -> DomainResult<Option<OperationLock>>,
	) -> DomainResult<()> {
		let operation =
			operation.ok_or_else(|| DomainError::OperationNotFound(self.operation_id.clone()))?;
		if operation.phase == OperationPhase::RollingBack {
			return Err(DomainError::InvalidOperationTransition {
				operation_id: self.operation_id.clone(),
				from: operation.phase,
				to: OperationPhase::Selected,
			});
		}

		for outpoint in &self.selected_outpoints {
			let utxo =
				get_utxo(outpoint)?.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
			let lock = get_lock(outpoint)?;

			let binding_matches = utxo.operation_binding.as_ref().is_some_and(|binding| {
				binding.operation_id == self.operation_id && binding.phase == operation.phase
			});
			let lock_matches = lock.as_ref().is_some_and(|lock| {
				lock.operation_id == self.operation_id
					&& lock.account_id == utxo.account_id
					&& lock.scope_id == utxo.scope_id
					&& lock.expires_at == operation.expires_at
					&& self.effects.iter().any(|effect| {
						matches!(
							effect,
							SelectionEffect::LockUtxo { outpoint: locked, purpose }
								if locked == outpoint && *purpose == lock.purpose
						)
					})
			});

			if !binding_matches || !lock_matches {
				return Err(DomainError::PlannedInputsMismatch {
					operation_id: self.operation_id.clone(),
				});
			}

			if utxo.observation_conflict.is_some() {
				return Err(DomainError::ObservationConflict {
					outpoint: outpoint.clone(),
					onchain_state: utxo.onchain_state,
				});
			}
		}

		Ok(())
	}

	/// Validates selection structure and the current account/scope capabilities.
	/// Store backends must bind all readers to the same snapshot as their atomic write.
	pub fn validate_capabilities(
		&self, get_account: impl Fn(&AccountId) -> DomainResult<Option<Account>>,
		get_scope: impl Fn(&WalletScopeId) -> DomainResult<Option<WalletScope>>,
		get_utxo: impl Fn(&OutPoint) -> DomainResult<Option<ManagedUtxo>>,
	) -> DomainResult<()> {
		let validate_scope = |account_id: &AccountId,
		                      scope_id: &WalletScopeId,
		                      required: &CapabilitySet|
		 -> DomainResult<()> {
			let account = get_account(account_id)?
				.ok_or_else(|| DomainError::AccountNotFound(account_id.clone()))?;
			if !account.capabilities.contains_all(required) {
				return Err(DomainError::MissingAccountCapabilities {
					account_id: account_id.clone(),
				});
			}
			let scope =
				get_scope(scope_id)?.ok_or_else(|| DomainError::ScopeNotFound(scope_id.clone()))?;
			if scope.account_id != *account_id || !account.scopes.iter().any(|id| id == scope_id) {
				return Err(DomainError::ScopeNotFound(scope_id.clone()));
			}
			if !scope.capabilities.contains_all(required) {
				return Err(DomainError::MissingCapabilities { scope_id: scope_id.clone() });
			}
			Ok(())
		};

		let selected: BTreeSet<_> = self.selected_outpoints.iter().collect();
		let mut effects = BTreeSet::new();
		for SelectionEffect::LockUtxo { outpoint, .. } in &self.effects {
			if !effects.insert(outpoint) {
				return Err(DomainError::CorruptedState("duplicate selection effect".into()));
			}
		}
		if selected.is_empty()
			|| selected.len() != self.selected_outpoints.len()
			|| selected != effects
		{
			return Err(DomainError::CorruptedState(
				"selection inputs and lock effects must match and be non-empty and unique".into(),
			));
		}
		let mut change_required = CapabilitySet::default();
		for SelectionEffect::LockUtxo { outpoint, purpose } in &self.effects {
			let utxo =
				get_utxo(outpoint)?.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
			let required = purpose.required_capabilities();
			validate_scope(&utxo.account_id, &utxo.scope_id, &required)?;
			for capability in purpose.required_change_capabilities().iter() {
				change_required.insert(*capability);
			}
		}
		let change_scope = get_scope(&self.change_scope_id)?
			.ok_or_else(|| DomainError::ScopeNotFound(self.change_scope_id.clone()))?;
		validate_scope(&change_scope.account_id, &change_scope.id, &change_required)
	}
}
