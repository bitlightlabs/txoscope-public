use std::collections::HashMap;

use txoscope_core::{
	backend::{
		IntentStatus, OperationIntent, OperationLock, OperationSelection, PersistedOperation,
		ReconcilePlan, ReconciliationAction, SelectionEffect, StateBatch, StateRead, StateStore,
	},
	Account, AccountId, Availability, DomainError, DomainResult, ManagedUtxo, ObservationConflict,
	OperationId, OperationPhase, OutPoint, WalletScope, WalletScopeId,
};

#[derive(Debug, Default, Clone)]
pub struct InMemoryStateStore {
	accounts: HashMap<AccountId, Account>,
	scopes: HashMap<WalletScopeId, WalletScope>,
	utxos: HashMap<String, ManagedUtxo>,
	locks: HashMap<String, OperationLock>,
	operations: HashMap<OperationId, PersistedOperation>,
	intents: HashMap<OperationId, OperationIntent>,
	tick: u64,
}

impl InMemoryStateStore {
	fn next_tick(&mut self) -> u64 {
		self.tick = self.tick.saturating_add(1);
		self.tick
	}

	fn validate_lock_effect(
		utxos: &HashMap<String, ManagedUtxo>, locks: &HashMap<String, OperationLock>,
		selection: &OperationSelection, phase: OperationPhase, outpoint: &OutPoint,
	) -> DomainResult<()> {
		let utxo = utxos
			.get(&outpoint.to_string())
			.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
		let lock = locks.get(&outpoint.to_string());
		let lock_present = lock.is_some();
		if utxo.can_be_locked(lock_present) {
			return Ok(());
		}
		if utxo.operation_binding.as_ref().is_some_and(|binding| {
			binding.operation_id == selection.operation_id && binding.phase == phase
		}) && lock.is_some_and(|lock| lock.operation_id == selection.operation_id)
		{
			return Ok(());
		}
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
		Err(DomainError::UtxoPolicyBlocked {
			outpoint: outpoint.clone(),
			reason: format!(
				"utxo is not safely spendable: onchain_state={:?}, lock_present={}",
				utxo.onchain_state, lock_present,
			),
		})
	}
}

impl StateRead for InMemoryStateStore {
	fn get_account(&self, account_id: &AccountId) -> DomainResult<Option<Account>> {
		Ok(self.accounts.get(account_id).cloned())
	}

	fn get_scope(&self, scope_id: &WalletScopeId) -> DomainResult<Option<WalletScope>> {
		Ok(self.scopes.get(scope_id).cloned())
	}

	fn get_utxo(&self, outpoint: &OutPoint) -> DomainResult<Option<ManagedUtxo>> {
		Ok(self.utxos.get(&outpoint.to_string()).cloned())
	}

	fn get_lock(&self, outpoint: &OutPoint) -> DomainResult<Option<OperationLock>> {
		Ok(self.locks.get(&outpoint.to_string()).cloned())
	}

	fn get_operation(
		&self, operation_id: &OperationId,
	) -> DomainResult<Option<PersistedOperation>> {
		Ok(self.operations.get(operation_id).cloned())
	}

	fn get_intent(&self, operation_id: &OperationId) -> DomainResult<Option<OperationIntent>> {
		Ok(self.intents.get(operation_id).cloned())
	}

	fn list_utxos(&self) -> DomainResult<Vec<ManagedUtxo>> {
		Ok(self.utxos.values().cloned().collect())
	}

	fn list_operations(&self) -> DomainResult<Vec<PersistedOperation>> {
		Ok(self.operations.values().cloned().collect())
	}

	fn list_pending_intents(&self) -> DomainResult<Vec<OperationIntent>> {
		Ok(self
			.intents
			.values()
			.filter(|intent| intent.status == IntentStatus::Pending)
			.cloned()
			.collect())
	}

	fn current_tick(&self) -> DomainResult<u64> {
		Ok(self.tick)
	}
}

impl StateStore for InMemoryStateStore {
	fn put_account(&mut self, account: Account) -> DomainResult<()> {
		self.accounts.insert(account.id.clone(), account);
		self.next_tick();
		Ok(())
	}

	fn put_scope(&mut self, scope: WalletScope) -> DomainResult<()> {
		let account = self
			.accounts
			.get_mut(&scope.account_id)
			.ok_or_else(|| DomainError::AccountNotFound(scope.account_id.clone()))?;
		account.scopes.push(scope.id.clone());
		self.scopes.insert(scope.id.clone(), scope);
		self.next_tick();
		Ok(())
	}

	fn put_utxo(&mut self, utxo: ManagedUtxo) -> DomainResult<()> {
		self.utxos.insert(utxo.outpoint.to_string(), utxo);
		self.next_tick();
		Ok(())
	}

	fn advance_tick(&mut self, delta: u64) -> DomainResult<()> {
		self.tick = self.tick.saturating_add(delta.max(1));
		Ok(())
	}

	fn persist_intent(&mut self, selection: OperationSelection) -> DomainResult<OperationIntent> {
		if let Some(existing) = self.intents.get(&selection.operation_id) {
			if existing.selection != selection {
				return Err(DomainError::IntentConflict { operation_id: selection.operation_id });
			}
			return Ok(existing.clone());
		}
		let recorded_at = self.next_tick();
		let intent = OperationIntent {
			selection: selection.clone(),
			status: IntentStatus::Pending,
			recorded_at,
		};
		self.intents.insert(selection.operation_id.clone(), intent.clone());
		Ok(intent)
	}

	fn mark_intent_committed(&mut self, operation_id: &OperationId) -> DomainResult<()> {
		let intent = self
			.intents
			.get_mut(operation_id)
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		if intent.status == IntentStatus::Failed {
			return Err(DomainError::IntentNotPending(operation_id.clone()));
		}
		intent.status = IntentStatus::Committed;
		self.next_tick();
		Ok(())
	}

	fn mark_intent_failed(&mut self, operation_id: &OperationId) -> DomainResult<()> {
		let intent = self
			.get_intent(operation_id)?
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		match intent.status {
			IntentStatus::Committed => {
				return Err(DomainError::IntentNotPending(operation_id.clone()))
			},
			IntentStatus::Failed => return Ok(()),
			IntentStatus::Pending => {},
		}
		if let Some(operation) = self.get_operation(operation_id)? {
			match operation.phase {
				OperationPhase::Selected => {
					self.release_operation(operation_id, OperationPhase::RollingBack)?;
				},
				OperationPhase::RollingBack => {},
				phase => {
					return Err(DomainError::InvalidOperationTransition {
						operation_id: operation_id.clone(),
						from: phase,
						to: OperationPhase::RollingBack,
					})
				},
			}
		}
		self.intents.get_mut(operation_id).expect("intent checked above").status =
			IntentStatus::Failed;
		self.next_tick();
		Ok(())
	}

	fn apply_batch(&mut self, batch: StateBatch) -> DomainResult<()> {
		if let Some(intent) = self.get_intent(&batch.selection.operation_id)? {
			if intent.status == IntentStatus::Failed {
				return Err(DomainError::IntentNotPending(batch.selection.operation_id));
			}
			if intent.selection != batch.selection {
				return Err(DomainError::IntentConflict {
					operation_id: batch.selection.operation_id,
				});
			}
		}
		if let Some(operation) = self.get_operation(&batch.selection.operation_id)? {
			if operation.phase != batch.phase || operation.phase == OperationPhase::RollingBack {
				return Err(DomainError::InvalidOperationTransition {
					operation_id: batch.selection.operation_id,
					from: operation.phase,
					to: batch.phase,
				});
			}
		}
		batch.selection.validate_capabilities(
			|id| self.get_account(id),
			|id| self.get_scope(id),
			|id| self.get_utxo(id),
		)?;
		let mut next_utxos = self.utxos.clone();
		let mut next_locks = self.locks.clone();
		let mut next_operations = self.operations.clone();

		for effect in &batch.selection.effects {
			match effect {
				SelectionEffect::LockUtxo { outpoint, purpose } => {
					Self::validate_lock_effect(
						&next_utxos,
						&next_locks,
						&batch.selection,
						batch.phase,
						outpoint,
					)?;
					let utxo = next_utxos
						.get_mut(&outpoint.to_string())
						.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
					let already_applied = utxo.operation_binding.as_ref().is_some_and(|binding| {
						binding.operation_id == batch.selection.operation_id
							&& binding.phase == batch.phase
					}) && next_locks
						.get(&outpoint.to_string())
						.is_some_and(|lock| lock.operation_id == batch.selection.operation_id);
					if already_applied {
						let lock = next_locks
							.get(&outpoint.to_string())
							.expect("existing lock checked above");
						if lock.purpose != *purpose
							|| lock.expires_at != batch.selection.expires_at
							|| lock.account_id != utxo.account_id
							|| lock.scope_id != utxo.scope_id
						{
							return Err(DomainError::IntentConflict {
								operation_id: batch.selection.operation_id,
							});
						}
						// Owner is a diagnostic label; keep the original label on replay.
						continue;
					}
					utxo.reserve(batch.selection.operation_id.clone(), batch.phase);
					next_locks.insert(
						outpoint.to_string(),
						OperationLock {
							operation_id: batch.selection.operation_id.clone(),
							account_id: utxo.account_id.clone(),
							scope_id: utxo.scope_id.clone(),
							purpose: *purpose,
							owner: batch.owner.clone(),
							created_at: self.tick.saturating_add(1),
							expires_at: batch.selection.expires_at,
						},
					);
				},
			}
		}

		let updated_at = self.next_tick();
		next_operations.insert(
			batch.selection.operation_id.clone(),
			PersistedOperation {
				operation_id: batch.selection.operation_id.clone(),
				phase: batch.phase,
				updated_at,
				expires_at: batch.selection.expires_at,
			},
		);

		self.utxos = next_utxos;
		self.locks = next_locks;
		self.operations = next_operations;
		Ok(())
	}

	fn apply_reconciliation(&mut self, plan: ReconcilePlan) -> DomainResult<()> {
		for action in plan.actions {
			match action {
				ReconciliationAction::DiscoverNew(observed) => {
					if self.utxos.contains_key(&observed.outpoint.to_string()) {
						continue;
					}
					let utxo = ManagedUtxo::discover_from_observed(
						observed.outpoint.clone(),
						observed.account_id,
						observed.scope_id,
						observed.value_sats,
						&observed.confirmation,
					);
					self.utxos.insert(utxo.outpoint.to_string(), utxo);
				},
				ReconciliationAction::RefreshObserved(_) => {
					// Observation metadata does not overwrite local ownership facts.
				},
				ReconciliationAction::UpdateOnchainState { outpoint, new_state } => {
					if let Some(utxo) = self.utxos.get_mut(&outpoint.to_string()) {
						if !matches!(
							utxo.onchain_state,
							txoscope_core::OnchainState::Spent
								| txoscope_core::OnchainState::SpendingInFlight
						) {
							utxo.onchain_state = new_state;
						}
					}
				},
				ReconciliationAction::MarkSpent { outpoint, spending_txid: _ } => {
					let outpoint_id = outpoint.to_string();
					if let Some(utxo) = self.utxos.get_mut(&outpoint_id) {
						utxo.mark_spent();
					}
					self.locks.remove(&outpoint_id);
				},
				ReconciliationAction::ReportConflict { outpoint, message, recorded_at } => {
					if let Some(utxo) = self.utxos.get_mut(&outpoint.to_string()) {
						utxo.observation_conflict =
							Some(ObservationConflict { message, recorded_at });
					}
				},
				ReconciliationAction::ClearConflict(outpoint) => {
					if let Some(utxo) = self.utxos.get_mut(&outpoint.to_string()) {
						utxo.observation_conflict = None;
					}
				},
				ReconciliationAction::UpdateAvailability { outpoint, availability } => {
					if let Some(utxo) = self.utxos.get_mut(&outpoint.to_string()) {
						utxo.availability = availability;
					}
				},
				ReconciliationAction::MarkMissing(outpoint) => {
					if let Some(utxo) = self.utxos.get_mut(&outpoint.to_string()) {
						utxo.mark_missing();
					}
				},
			}
		}
		self.next_tick();
		Ok(())
	}

	fn release_operation(
		&mut self, operation_id: &OperationId, released_phase: OperationPhase,
	) -> DomainResult<Vec<OutPoint>> {
		let mut next_utxos = self.utxos.clone();
		let mut released = Vec::new();
		for utxo in next_utxos.values_mut() {
			if utxo
				.operation_binding
				.as_ref()
				.is_some_and(|binding| binding.operation_id == *operation_id)
			{
				utxo.release_binding();
				released.push(utxo.outpoint.clone());
			}
		}

		let mut next_locks = self.locks.clone();
		next_locks.retain(|_, lock| lock.operation_id != *operation_id);
		let updated_at = self.next_tick();
		self.utxos = next_utxos;
		self.locks = next_locks;
		self.operations.insert(
			operation_id.clone(),
			PersistedOperation {
				operation_id: operation_id.clone(),
				phase: released_phase,
				updated_at,
				expires_at: None,
			},
		);
		Ok(released)
	}

	fn release_selected_operation(
		&mut self, operation_id: &OperationId,
	) -> DomainResult<Option<Vec<OutPoint>>> {
		if !self
			.operations
			.get(operation_id)
			.is_some_and(|operation| operation.phase == OperationPhase::Selected)
		{
			return Ok(None);
		}
		self.release_operation(operation_id, OperationPhase::RollingBack).map(Some)
	}

	fn complete_operation(
		&mut self, operation_id: &OperationId, completed_phase: OperationPhase,
	) -> DomainResult<Vec<OutPoint>> {
		let operation = self
			.operations
			.get(operation_id)
			.ok_or_else(|| DomainError::OperationNotFound(operation_id.clone()))?;
		let intent = self
			.intents
			.get(operation_id)
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		if intent.status != IntentStatus::Committed {
			return Err(DomainError::IntentNotCommitted(operation_id.clone()));
		}
		if !operation.validate_advance(completed_phase)? {
			return Ok(Vec::new());
		}
		let mut next_utxos = self.utxos.clone();
		let mut completed = Vec::new();
		for utxo in next_utxos.values_mut() {
			if utxo
				.operation_binding
				.as_ref()
				.is_some_and(|binding| binding.operation_id == *operation_id)
			{
				utxo.advance_binding(completed_phase);
				completed.push(utxo.outpoint.clone());
			}
		}
		let mut next_locks = self.locks.clone();
		for lock in next_locks.values_mut().filter(|lock| lock.operation_id == *operation_id) {
			lock.expires_at = None;
		}
		let updated_at = self.next_tick();
		self.utxos = next_utxos;
		self.locks = next_locks;
		self.operations.insert(
			operation_id.clone(),
			PersistedOperation {
				operation_id: operation_id.clone(),
				phase: completed_phase,
				updated_at,
				expires_at: None,
			},
		);
		Ok(completed)
	}
}
