use std::collections::BTreeSet;

use crate::{
	account::Account,
	errors::{DomainError, DomainResult},
	lock::OperationLock,
	operation::{
		InputSelectionSpec, IntentStatus, OperationIntent, OperationPhase, OperationSelection,
		PersistedOperation, SelectionEffect,
	},
	reconciliation::{ReconcilePlan, ReconciliationPlanner},
	scope::WalletScope,
	selection_request::SelectionRequest,
	selector::{SelectionContext, SelectorRegistry},
	state::Availability,
	store::{StateBatch, StateRead, StateStore},
	utxo::ManagedUtxo,
	AccountId, OperationId, OutPoint, WalletScopeId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
	pub replayed_operations: Vec<OperationId>,
	pub failed_operations: Vec<OperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
	pub released_operations: Vec<OperationId>,
	pub released_outpoints: Vec<OutPoint>,
}

#[derive(Debug, Clone)]
pub struct Orchestrator<S> {
	store: S,
	owner: String,
}

impl<S: StateStore> Orchestrator<S> {
	/// Wraps `store` with default owner label `"txoscope"`.
	pub fn with_store(store: S) -> Self {
		Self { store, owner: "txoscope".to_string() }
	}

	pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
		self.owner = owner.into();
		self
	}

	pub fn set_owner(&mut self, owner: impl Into<String>) {
		self.owner = owner.into();
	}

	pub fn add_account(&mut self, account: Account) -> DomainResult<()> {
		self.store.put_account(account)
	}

	pub fn get_account(&self, account_id: &AccountId) -> DomainResult<Option<Account>> {
		self.store.get_account(account_id)
	}

	pub fn add_scope(&mut self, scope: WalletScope) -> DomainResult<()> {
		self.store.put_scope(scope)
	}

	pub fn get_scope(&self, scope_id: &WalletScopeId) -> DomainResult<Option<WalletScope>> {
		self.store.get_scope(scope_id)
	}

	pub fn add_utxo(&mut self, utxo: ManagedUtxo) -> DomainResult<()> {
		self.store.put_utxo(utxo)
	}

	pub fn get_utxo(&self, outpoint: &OutPoint) -> DomainResult<Option<ManagedUtxo>> {
		self.store.get_utxo(outpoint)
	}

	pub fn list_utxos(&self) -> DomainResult<Vec<ManagedUtxo>> {
		self.store.list_utxos()
	}

	pub fn get_lock(&self, outpoint: &OutPoint) -> DomainResult<Option<OperationLock>> {
		self.store.get_lock(outpoint)
	}

	pub fn is_outpoint_selectable(&self, outpoint: &OutPoint) -> DomainResult<bool> {
		Ok(self.utxo_status(outpoint)?.is_some_and(|status| status.is_selectable()))
	}

	/// Read the managed output and its lock without mutation or expiry cleanup.
	/// Backend read consistency follows the store's `StateRead` implementation.
	pub fn utxo_status(&self, outpoint: &OutPoint) -> DomainResult<Option<crate::UtxoStatus>> {
		let Some(utxo) = self.store.get_utxo(outpoint)? else {
			return Ok(None);
		};
		let lock = self.store.get_lock(outpoint)?;
		Ok(Some(crate::UtxoStatus::from_records(utxo, lock)))
	}

	pub fn get_operation(
		&self, operation_id: &OperationId,
	) -> DomainResult<Option<PersistedOperation>> {
		self.store.get_operation(operation_id)
	}

	pub fn get_intent(&self, operation_id: &OperationId) -> DomainResult<Option<OperationIntent>> {
		self.store.get_intent(operation_id)
	}

	pub fn list_operations(&self) -> DomainResult<Vec<PersistedOperation>> {
		self.store.list_operations()
	}

	pub fn pending_intents(&self) -> DomainResult<Vec<OperationIntent>> {
		self.store.list_pending_intents()
	}

	pub fn current_tick(&self) -> DomainResult<u64> {
		self.store.current_tick()
	}

	pub fn advance_tick(&mut self, delta: u64) -> DomainResult<()> {
		self.store.advance_tick(delta)
	}

	pub fn reconcile_snapshot(
		&mut self, snapshot: crate::ObservationSnapshot,
	) -> DomainResult<ReconcilePlan> {
		let plan = ReconciliationPlanner::new(&self.store as &dyn StateRead).plan(&snapshot)?;
		self.store.apply_reconciliation(plan.clone())?;
		Ok(plan)
	}

	pub fn prepare_operation<R>(&mut self, req: R) -> DomainResult<OperationSelection>
	where
		R: Into<SelectionRequest>,
	{
		let selection = self.prepare_pending_operation(req)?;
		self.commit_recorded_intent(&selection.operation_id)?;
		Ok(selection)
	}

	pub fn prepare_pending_operation<R>(&mut self, req: R) -> DomainResult<OperationSelection>
	where
		R: Into<SelectionRequest>,
	{
		let selection = self.propose_operation(req)?;
		self.persist_selection_intent(selection.clone())?;
		Ok(selection)
	}

	pub fn propose_operation<R>(&self, req: R) -> DomainResult<OperationSelection>
	where
		R: Into<SelectionRequest>,
	{
		let req = req.into().into_operation_request();
		if let InputSelectionSpec::ExactOutpoints { outpoints } = &req.selection.spec {
			if outpoints.is_empty() {
				return Err(DomainError::InvalidRequest {
					reason: "exact selection requires at least one outpoint".into(),
				});
			}
			let mut unique = BTreeSet::new();
			for outpoint in outpoints {
				if !unique.insert(outpoint) {
					return Err(DomainError::InvalidRequest {
						reason: format!("exact selection includes duplicate outpoint: {outpoint}"),
					});
				}
			}
		}
		let account_id = req.selection.account_id.clone();
		let account = self
			.store
			.get_account(&account_id)?
			.ok_or_else(|| DomainError::AccountNotFound(account_id.clone()))?;

		if !account.capabilities.contains_all(&req.selection.required_capabilities) {
			return Err(DomainError::MissingAccountCapabilities { account_id: account_id.clone() });
		}

		let mut eligible_scope_ids: Vec<WalletScopeId> = Vec::new();
		for scope_id in &account.scopes {
			if self.store.get_scope(scope_id)?.is_some_and(|scope| {
				scope.account_id == account_id
					&& scope.capabilities.contains_all(&req.selection.required_capabilities)
			}) {
				eligible_scope_ids.push(scope_id.clone());
			}
		}
		if eligible_scope_ids.is_empty()
			&& !matches!(&req.selection.spec, InputSelectionSpec::ExactOutpoints { outpoints } if !outpoints.is_empty())
		{
			return Err(DomainError::NoEligibleScope { account_id: account_id.clone() });
		}

		let ctx = SelectionContext {
			state: &self.store as &dyn StateRead,
			account_id: &account_id,
			account_scope_ids: &account.scopes,
			eligible_scope_ids: &eligible_scope_ids,
		};
		let selected_inputs =
			SelectorRegistry.selector_for(&req.selection.spec).select(&ctx, &req.selection)?;
		Ok(OperationSelection::from_request(&req, selected_inputs))
	}

	pub fn apply_selection(&mut self, selection: &OperationSelection) -> DomainResult<()> {
		self.persist_selection_intent(selection.clone())?;
		self.commit_recorded_intent(&selection.operation_id)
	}

	pub fn persist_selection_intent(
		&mut self, selection: OperationSelection,
	) -> DomainResult<OperationIntent> {
		self.store.persist_intent(selection)
	}

	pub fn commit_recorded_intent(&mut self, operation_id: &OperationId) -> DomainResult<()> {
		let intent = self
			.store
			.get_intent(operation_id)?
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;

		match intent.status {
			IntentStatus::Committed => return Ok(()),
			IntentStatus::Failed => {
				return Err(DomainError::IntentNotPending(operation_id.clone()))
			},
			IntentStatus::Pending => {},
		}

		match self.commit_selection(&intent.selection) {
			Ok(()) => {
				self.store.mark_intent_committed(operation_id)?;
				Ok(())
			},
			Err(err @ DomainError::Storage(_)) => Err(err),
			Err(err) => {
				if !self.is_pending_lock_conflict(operation_id, &err)? {
					self.store.mark_intent_failed(operation_id)?;
				}
				Err(err)
			},
		}
	}

	pub fn fail_recorded_intent(&mut self, operation_id: &OperationId) -> DomainResult<()> {
		let intent = self
			.store
			.get_intent(operation_id)?
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		match intent.status {
			IntentStatus::Committed => Err(DomainError::IntentNotPending(operation_id.clone())),
			IntentStatus::Failed => Ok(()),
			IntentStatus::Pending => self.store.mark_intent_failed(operation_id),
		}
	}

	fn is_pending_lock_conflict(
		&self, operation_id: &OperationId, error: &DomainError,
	) -> DomainResult<bool> {
		let outpoint = match error {
			DomainError::UtxoUnavailable { outpoint, .. }
			| DomainError::UtxoPolicyBlocked { outpoint, .. } => outpoint,
			_ => return Ok(false),
		};
		let Some(lock) = self.store.get_lock(outpoint)? else {
			return Ok(false);
		};
		if lock.operation_id == *operation_id {
			return Ok(false);
		}
		Ok(self
			.store
			.get_intent(&lock.operation_id)?
			.is_some_and(|intent| intent.status == IntentStatus::Pending))
	}

	pub fn recover_pending_intents(&mut self) -> DomainResult<RecoveryReport> {
		let mut pending = self.store.list_pending_intents()?;
		let mut replayed_operations = Vec::new();
		let mut failed_operations = Vec::new();
		while !pending.is_empty() {
			let mut deferred = Vec::new();
			let mut progressed = false;
			for intent in pending {
				match self.commit_selection(&intent.selection) {
					Ok(()) => {
						self.store.mark_intent_committed(&intent.selection.operation_id)?;
						replayed_operations.push(intent.selection.operation_id.clone());
						progressed = true;
					},
					Err(err @ DomainError::Storage(_)) => return Err(err),
					Err(err) => {
						if self.is_pending_lock_conflict(&intent.selection.operation_id, &err)? {
							deferred.push(intent);
						} else {
							self.store.mark_intent_failed(&intent.selection.operation_id)?;
							failed_operations.push(intent.selection.operation_id.clone());
							progressed = true;
						}
					},
				}
			}
			// Retry contention only after some holder was committed or rejected.
			// A pass with no progress leaves blocked intents pending for a later attempt.
			if !progressed {
				break;
			}
			pending = deferred;
		}
		Ok(RecoveryReport { replayed_operations, failed_operations })
	}

	pub fn cleanup_stale_reservations(&mut self, max_age: u64) -> DomainResult<CleanupReport> {
		let now = self.store.current_tick()?;
		let stale_operations: Vec<OperationId> = self
			.store
			.list_operations()?
			.into_iter()
			.filter(|operation| operation.phase == OperationPhase::Selected)
			.filter(|operation| now.saturating_sub(operation.updated_at) >= max_age)
			.map(|operation| operation.operation_id)
			.collect();

		let mut released_operations = Vec::new();
		let mut released_outpoints = Vec::new();
		for operation_id in stale_operations {
			if let Some(mut released) = self.release_selected_operation(&operation_id)? {
				released_operations.push(operation_id);
				released_outpoints.append(&mut released);
			}
		}

		Ok(CleanupReport { released_operations, released_outpoints })
	}

	/// Atomically releases a reservation only if it is still Selected. Automatic
	/// expiry must use this path so it cannot cancel a concurrently advanced operation.
	pub fn release_selected_operation(
		&mut self, operation_id: &OperationId,
	) -> DomainResult<Option<Vec<OutPoint>>> {
		self.store.release_selected_operation(operation_id)
	}

	pub fn release_operation(&mut self, operation_id: &OperationId) -> DomainResult<Vec<OutPoint>> {
		let operation = self.current_operation(operation_id)?;
		if operation.phase == OperationPhase::RollingBack {
			return Ok(Vec::new());
		}
		if !matches!(
			operation.phase,
			OperationPhase::Selected
				| OperationPhase::Constructing
				| OperationPhase::Signed
				| OperationPhase::Broadcast
				| OperationPhase::AwaitingConfirmation
				| OperationPhase::AwaitingL2Finality
		) {
			return Err(DomainError::InvalidOperationTransition {
				operation_id: operation_id.clone(),
				from: operation.phase,
				to: OperationPhase::RollingBack,
			});
		}
		self.store.release_operation(operation_id, OperationPhase::RollingBack)
	}

	/// Advances a committed operation while retaining its input ownership.
	/// Actual inputs are verified when leaving Selected. Broadcast and later
	/// phases start on-chain spending; only observed spends or explicit release
	/// remove bindings and locks. Returns the outpoints whose bindings advanced.
	pub fn complete_operation(
		&mut self, operation_id: &OperationId, completed_phase: OperationPhase,
		actual_outpoints: impl IntoIterator<Item = OutPoint>,
	) -> DomainResult<Vec<OutPoint>> {
		let operation = self.current_operation(operation_id)?;
		let intent = self
			.store
			.get_intent(operation_id)?
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		if intent.status != IntentStatus::Committed {
			return Err(DomainError::IntentNotCommitted(operation_id.clone()));
		}
		if !operation.validate_advance(completed_phase)? {
			return Ok(Vec::new());
		}
		if operation.phase == OperationPhase::Selected {
			self.verify_planned_inputs(operation_id, actual_outpoints)?;
		}
		self.store.complete_operation(operation_id, completed_phase)
	}

	pub fn verify_planned_inputs(
		&self, operation_id: &OperationId, actual_outpoints: impl IntoIterator<Item = OutPoint>,
	) -> DomainResult<()> {
		let planned = self
			.store
			.get_intent(operation_id)?
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		let planned: BTreeSet<OutPoint> =
			planned.selection.selected_outpoints.into_iter().collect();
		let actual: BTreeSet<OutPoint> = actual_outpoints.into_iter().collect();
		if planned == actual {
			Ok(())
		} else {
			Err(DomainError::PlannedInputsMismatch { operation_id: operation_id.clone() })
		}
	}

	fn commit_selection(&mut self, selection: &OperationSelection) -> DomainResult<()> {
		if let Some(operation) = self.store.get_operation(&selection.operation_id)? {
			if operation.phase != OperationPhase::Selected {
				return Err(DomainError::InvalidOperationTransition {
					operation_id: selection.operation_id.clone(),
					from: operation.phase,
					to: OperationPhase::Selected,
				});
			}
		}
		self.validate_selection(selection)?;
		self.store.apply_batch(StateBatch {
			selection: selection.clone(),
			phase: OperationPhase::Selected,
			owner: self.owner.clone(),
		})
	}

	fn validate_selection(&self, selection: &OperationSelection) -> DomainResult<()> {
		selection.validate_capabilities(
			|id| self.store.get_account(id),
			|id| self.store.get_scope(id),
			|id| self.store.get_utxo(id),
		)?;
		for effect in &selection.effects {
			match effect {
				SelectionEffect::LockUtxo { outpoint, .. } => {
					let utxo = self
						.store
						.get_utxo(outpoint)?
						.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
					let lock = self.store.get_lock(outpoint)?;
					if utxo.is_policy_spendable() && lock.is_none() {
						continue;
					}

					let idempotent = utxo.operation_binding.as_ref().is_some_and(|binding| {
						binding.operation_id == selection.operation_id
							&& binding.phase == OperationPhase::Selected
					}) && lock
						.as_ref()
						.is_some_and(|lock| lock.operation_id == selection.operation_id);
					if !idempotent {
						if utxo.availability != Availability::Selectable {
							return Err(DomainError::UtxoUnavailable {
								outpoint: outpoint.clone(),
								availability: utxo.availability,
							});
						}
						if utxo.observation_conflict.is_some() {
							return Err(DomainError::ObservationConflict {
								outpoint: outpoint.clone(),
								onchain_state: utxo.onchain_state,
							});
						}
						return Err(DomainError::UtxoPolicyBlocked {
							outpoint: outpoint.clone(),
							reason: format!(
								"utxo is not safely spendable: onchain_state={:?}, lock_present={}",
								utxo.onchain_state,
								lock.is_some(),
							),
						});
					}
				},
			}
		}
		Ok(())
	}

	fn current_operation(&self, operation_id: &OperationId) -> DomainResult<PersistedOperation> {
		self.store
			.get_operation(operation_id)?
			.ok_or_else(|| DomainError::OperationNotFound(operation_id.clone()))
	}
}
