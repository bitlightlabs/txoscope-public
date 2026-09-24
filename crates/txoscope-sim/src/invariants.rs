use txoscope_core::{
	backend::{Orchestrator, StateStore},
	Availability, DomainResult, OnchainState, OperationId, OperationPhase,
};
use txoscope_store_memory::InMemoryStateStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvariantViolationKind {
	ReservedWithoutLock,
	ReservedWithoutBinding,
	SelectableWithLock,
	SelectableWithBinding,
	SelectableWithConflict,
	SelectableWhileUnconfirmed,
	ReleasedOperationStillBound,
	LifecycleStateMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantViolation {
	pub step: usize,
	pub kind: InvariantViolationKind,
	pub detail: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct InvariantChecker;

impl InvariantChecker {
	pub fn check<S: StateStore>(
		&self, step: usize, orchestrator: &Orchestrator<S>, released_operations: &[OperationId],
	) -> DomainResult<Vec<InvariantViolation>> {
		let mut violations = Vec::new();

		for utxo in orchestrator.list_utxos()? {
			let outpoint = &utxo.outpoint;
			match utxo.availability {
				Availability::Reserved => {
					if utxo.operation_binding.is_none() {
						violations.push(InvariantViolation {
							step,
							kind: InvariantViolationKind::ReservedWithoutBinding,
							detail: outpoint.to_string(),
						});
					}
					if orchestrator.get_lock(outpoint)?.is_none() {
						violations.push(InvariantViolation {
							step,
							kind: InvariantViolationKind::ReservedWithoutLock,
							detail: outpoint.to_string(),
						});
					}
				},
				Availability::Selectable => {
					if orchestrator.get_lock(outpoint)?.is_some() {
						violations.push(InvariantViolation {
							step,
							kind: InvariantViolationKind::SelectableWithLock,
							detail: outpoint.to_string(),
						});
					}
					if utxo.operation_binding.is_some() {
						violations.push(InvariantViolation {
							step,
							kind: InvariantViolationKind::SelectableWithBinding,
							detail: outpoint.to_string(),
						});
					}
					if utxo.observation_conflict.is_some() {
						violations.push(InvariantViolation {
							step,
							kind: InvariantViolationKind::SelectableWithConflict,
							detail: outpoint.to_string(),
						});
					}
					if utxo.onchain_state != OnchainState::Confirmed {
						violations.push(InvariantViolation {
							step,
							kind: InvariantViolationKind::SelectableWhileUnconfirmed,
							detail: format!("{outpoint}:{:?}", utxo.onchain_state),
						});
					}
				},
				_ => {},
			}
		}

		for operation_id in released_operations {
			for utxo in orchestrator.list_utxos()? {
				if utxo
					.operation_binding
					.as_ref()
					.is_some_and(|binding| binding.operation_id == *operation_id)
				{
					violations.push(InvariantViolation {
						step,
						kind: InvariantViolationKind::ReleasedOperationStillBound,
						detail: format!("{operation_id}:{}", utxo.outpoint),
					});
				}
			}
			if let Some(operation) = orchestrator.get_operation(operation_id)? {
				if operation.phase != OperationPhase::RollingBack {
					violations.push(InvariantViolation {
						step,
						kind: InvariantViolationKind::ReleasedOperationStillBound,
						detail: format!("{operation_id}:phase={:?}", operation.phase),
					});
				}
			}
		}

		Ok(violations)
	}
}

#[deprecated(note = "use InvariantChecker::default().check(...)")]
pub fn check_invariants(
	step: usize, orchestrator: &Orchestrator<InMemoryStateStore>,
	released_operations: &[OperationId],
) -> DomainResult<Vec<InvariantViolation>> {
	InvariantChecker.check(step, orchestrator, released_operations)
}
