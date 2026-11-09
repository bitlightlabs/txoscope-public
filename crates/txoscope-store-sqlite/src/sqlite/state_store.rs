//! Atomic state mutations and committed reservation validation.

use rusqlite::{params, OptionalExtension, TransactionBehavior};
use txoscope_core::{
	backend::{
		IntentStatus, OperationIntent, OperationSelection, ReconcilePlan, ReconciliationAction,
		SelectionEffect, StateBatch, StateRead, StateStore,
	},
	Account, DomainError, DomainResult, ManagedUtxo, ObservationConflict, OperationId,
	OperationPhase, OutPoint, WalletScope,
};

use super::SqliteStateStore;
use crate::codec::SqliteCodec;

impl StateStore for SqliteStateStore {
	fn put_account(&mut self, account: Account) -> DomainResult<()> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		tx.execute(
			"INSERT OR REPLACE INTO accounts(account_id, name, capabilities) VALUES (?1, ?2, ?3)",
			params![
				account.id.as_str(),
				account.name,
				SqliteCodec::encode_capabilities(&account.capabilities)
			],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		Self::next_tick_tx(&tx)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn put_scope(&mut self, scope: WalletScope) -> DomainResult<()> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		let exists = tx
			.query_row(
				"SELECT 1 FROM accounts WHERE account_id = ?1",
				params![scope.account_id.as_str()],
				|_| Ok(()),
			)
			.optional()
			.map_err(SqliteCodec::map_sql_error)?;
		if exists.is_none() {
			return Err(DomainError::AccountNotFound(scope.account_id));
		}
		tx.execute(
            "
            INSERT OR REPLACE INTO scopes(scope_id, account_id, descriptor_ref, derivation_scope, capabilities, priority)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                scope.id.as_str(),
                scope.account_id.as_str(),
                scope.descriptor_ref.0,
                scope.derivation_scope.0,
                SqliteCodec::encode_capabilities(&scope.capabilities),
                scope.priority,
            ],
        )
        .map_err(SqliteCodec::map_sql_error)?;
		Self::next_tick_tx(&tx)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn put_utxo(&mut self, utxo: ManagedUtxo) -> DomainResult<()> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		Self::write_utxo_tx(&tx, &utxo)?;
		Self::next_tick_tx(&tx)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn advance_tick(&mut self, delta: u64) -> DomainResult<()> {
		let next = self.current_tick()?.saturating_add(delta.max(1));
		self.conn
			.execute("UPDATE meta SET value = ?1 WHERE key = 'tick'", params![next.to_string()])
			.map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn persist_intent(&mut self, selection: OperationSelection) -> DomainResult<OperationIntent> {
		// Reserve the writer before reading: an operation ID must never be rebound by
		// another connection between checking its intent and inserting it.
		let tx = self
			.conn
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(SqliteCodec::map_sql_error)?;
		if let Some((change_scope_id, status, recorded_at, expires_at)) =
			Self::read_intent_header(&tx, &selection.operation_id)?
		{
			let existing = Self::read_intent_selection(
				&tx,
				&selection.operation_id,
				change_scope_id,
				expires_at,
			)?;
			if existing != selection {
				return Err(DomainError::IntentConflict { operation_id: selection.operation_id });
			}
			return Ok(OperationIntent { selection: existing, status, recorded_at });
		}
		let recorded_at = Self::next_tick_tx(&tx)?;
		tx.execute(
			"
            INSERT INTO intents(operation_id, change_scope_id, status, recorded_at, expires_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
			params![
				selection.operation_id.as_str(),
				selection.change_scope_id.as_str(),
				SqliteCodec::encode(IntentStatus::Pending),
				recorded_at,
				selection.expires_at,
			],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		for (position, outpoint) in selection.selected_outpoints.iter().enumerate() {
			tx.execute(
				"INSERT INTO intent_inputs(operation_id, position, outpoint) VALUES (?1, ?2, ?3)",
				params![selection.operation_id.as_str(), position, outpoint.to_string()],
			)
			.map_err(SqliteCodec::map_sql_error)?;
		}
		for (position, effect) in selection.effects.iter().enumerate() {
			match effect {
				SelectionEffect::LockUtxo { outpoint, purpose } => {
					tx.execute(
                        "
                        INSERT INTO intent_effects(operation_id, position, effect_kind, outpoint, purpose)
                        VALUES (?1, ?2, ?3, ?4, ?5)
                        ",
                        params![
                            selection.operation_id.as_str(),
                            position,
                            "lock_utxo",
                            outpoint.to_string(),
                            SqliteCodec::encode(*purpose),
                        ],
                    )
                    .map_err(SqliteCodec::map_sql_error)?;
				},
			}
		}
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(OperationIntent { selection, status: IntentStatus::Pending, recorded_at })
	}

	fn mark_intent_committed(&mut self, operation_id: &OperationId) -> DomainResult<()> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		if let Some((_, IntentStatus::Failed, _, _)) = Self::read_intent_header(&tx, operation_id)?
		{
			return Err(DomainError::IntentNotPending(operation_id.clone()));
		}
		let changed = tx
			.execute(
				"UPDATE intents SET status = ?1 WHERE operation_id = ?2",
				params![SqliteCodec::encode(IntentStatus::Committed), operation_id.as_str()],
			)
			.map_err(SqliteCodec::map_sql_error)?;
		if changed == 0 {
			return Err(DomainError::IntentNotFound(operation_id.clone()));
		}
		Self::next_tick_tx(&tx)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn mark_intent_failed(&mut self, operation_id: &OperationId) -> DomainResult<()> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		let (_, status, _, _) = Self::read_intent_header(&tx, operation_id)?
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		match status {
			IntentStatus::Committed => {
				return Err(DomainError::IntentNotPending(operation_id.clone()))
			},
			IntentStatus::Failed => return Ok(()),
			IntentStatus::Pending => {},
		}
		let phase = tx
			.query_row(
				"SELECT phase FROM operations WHERE operation_id = ?1",
				params![operation_id.as_str()],
				|row| SqliteCodec::decode::<OperationPhase>(&row.get::<_, String>(0)?),
			)
			.optional()
			.map_err(SqliteCodec::map_sql_error)?;
		match phase {
			Some(OperationPhase::Selected) => {
				Self::release_operation_tx(&tx, operation_id, OperationPhase::RollingBack)?;
			},
			None | Some(OperationPhase::RollingBack) => {},
			Some(phase) => {
				return Err(DomainError::InvalidOperationTransition {
					operation_id: operation_id.clone(),
					from: phase,
					to: OperationPhase::RollingBack,
				})
			},
		}
		tx.execute(
			"UPDATE intents SET status = 'failed' WHERE operation_id = ?1",
			params![operation_id.as_str()],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		Self::next_tick_tx(&tx)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn apply_batch(&mut self, batch: StateBatch) -> DomainResult<()> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		if let Some((change_scope_id, status, _, expires_at)) =
			Self::read_intent_header(&tx, &batch.selection.operation_id)?
		{
			if status == IntentStatus::Failed {
				return Err(DomainError::IntentNotPending(batch.selection.operation_id));
			}
			let recorded = Self::read_intent_selection(
				&tx,
				&batch.selection.operation_id,
				change_scope_id,
				expires_at,
			)?;
			if recorded != batch.selection {
				return Err(DomainError::IntentConflict {
					operation_id: batch.selection.operation_id,
				});
			}
			if status == IntentStatus::Committed {
				return recorded.validate_committed_reservation(
					Self::read_operation(&tx, &batch.selection.operation_id)?,
					|id| Self::get_utxo_tx(&tx, id),
					|id| Self::get_lock_tx(&tx, id),
				);
			}
		}
		let existing_phase = tx
			.query_row(
				"SELECT phase FROM operations WHERE operation_id = ?1",
				params![batch.selection.operation_id.as_str()],
				|row| SqliteCodec::decode::<OperationPhase>(&row.get::<_, String>(0)?),
			)
			.optional()
			.map_err(SqliteCodec::map_sql_error)?;
		if let Some(phase) = existing_phase {
			if phase != batch.phase || phase == OperationPhase::RollingBack {
				return Err(DomainError::InvalidOperationTransition {
					operation_id: batch.selection.operation_id,
					from: phase,
					to: batch.phase,
				});
			}
		}
		batch.selection.validate_capabilities(
			|id| Self::read_account(&tx, id),
			|id| Self::read_scope(&tx, id),
			|id| Self::get_utxo_tx(&tx, id),
		)?;
		for effect in &batch.selection.effects {
			match effect {
				SelectionEffect::LockUtxo { outpoint, purpose } => {
					Self::validate_lock_effect_tx(&tx, &batch.selection, batch.phase, outpoint)?;
					let mut utxo = Self::get_utxo_tx(&tx, outpoint)?
						.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
					let existing_lock = Self::get_lock_tx(&tx, outpoint)?;
					let applied_lock = existing_lock.filter(|lock| {
						lock.operation_id == batch.selection.operation_id
							&& utxo.operation_binding.as_ref().is_some_and(|binding| {
								binding.operation_id == batch.selection.operation_id
									&& binding.phase == batch.phase
							})
					});
					if let Some(lock) = applied_lock {
						if lock.purpose != *purpose
							|| lock.expires_at != batch.selection.expires_at
							|| lock.account_id != utxo.account_id
							|| lock.scope_id != utxo.scope_id
						{
							return Err(DomainError::IntentConflict {
								operation_id: batch.selection.operation_id,
							});
						}
						// Preserve the original diagnostic owner label during recovery.
						continue;
					}

					utxo.reserve(batch.selection.operation_id.clone(), batch.phase);
					Self::write_utxo_tx(&tx, &utxo)?;
					tx.execute(
                        "
                        INSERT OR REPLACE INTO locks(outpoint, operation_id, account_id, scope_id, purpose, owner, created_at, expires_at)
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                        ",
                        params![
                            outpoint.to_string(),
                            batch.selection.operation_id.as_str(),
                            utxo.account_id.as_str(),
                            utxo.scope_id.as_str(),
                            SqliteCodec::encode(*purpose),
                            batch.owner,
                            Self::read_tick_tx(&tx)?.saturating_add(1),
                            batch.selection.expires_at,
                        ],
                    )
                    .map_err(SqliteCodec::map_sql_error)?;
				},
			}
		}

		let updated_at = Self::next_tick_tx(&tx)?;
		tx.execute(
			"
            INSERT OR REPLACE INTO operations(operation_id, phase, updated_at, expires_at)
            VALUES (?1, ?2, ?3, ?4)
            ",
			params![
				batch.selection.operation_id.as_str(),
				SqliteCodec::encode(batch.phase),
				updated_at,
				batch.selection.expires_at,
			],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn apply_reconciliation(&mut self, plan: ReconcilePlan) -> DomainResult<()> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		for action in plan.actions {
			match action {
				ReconciliationAction::DiscoverNew(observed) => {
					if Self::get_utxo_tx(&tx, &observed.outpoint)?.is_some() {
						continue;
					}
					let utxo = ManagedUtxo::discover_from_observed(
						observed.outpoint.clone(),
						observed.account_id,
						observed.scope_id,
						observed.value_sats,
						&observed.confirmation,
					);
					Self::write_utxo_tx(&tx, &utxo)?;
				},
				ReconciliationAction::RefreshObserved(_) => {
					// Observation metadata does not overwrite local ownership facts.
				},
				ReconciliationAction::UpdateOnchainState { outpoint, new_state } => {
					if let Some(mut utxo) = Self::get_utxo_tx(&tx, &outpoint)? {
						if !matches!(
							utxo.onchain_state,
							txoscope_core::OnchainState::Spent
								| txoscope_core::OnchainState::SpendingInFlight
						) {
							utxo.onchain_state = new_state;
						}
						Self::write_utxo_tx(&tx, &utxo)?;
					}
				},
				ReconciliationAction::MarkSpent { outpoint, spending_txid: _ } => {
					if let Some(mut utxo) = Self::get_utxo_tx(&tx, &outpoint)? {
						utxo.mark_spent();
						Self::write_utxo_tx(&tx, &utxo)?;
						tx.execute(
							"DELETE FROM locks WHERE outpoint = ?1",
							params![outpoint.to_string()],
						)
						.map_err(SqliteCodec::map_sql_error)?;
					}
				},
				ReconciliationAction::ReportConflict { outpoint, message, recorded_at } => {
					if let Some(mut utxo) = Self::get_utxo_tx(&tx, &outpoint)? {
						utxo.observation_conflict =
							Some(ObservationConflict { message, recorded_at });
						Self::write_utxo_tx(&tx, &utxo)?;
					}
				},
				ReconciliationAction::ClearConflict(outpoint) => {
					if let Some(mut utxo) = Self::get_utxo_tx(&tx, &outpoint)? {
						utxo.observation_conflict = None;
						Self::write_utxo_tx(&tx, &utxo)?;
					}
				},
				ReconciliationAction::UpdateAvailability { outpoint, availability } => {
					if let Some(mut utxo) = Self::get_utxo_tx(&tx, &outpoint)? {
						utxo.availability = availability;
						Self::write_utxo_tx(&tx, &utxo)?;
					}
				},
				ReconciliationAction::MarkMissing(outpoint) => {
					if let Some(mut utxo) = Self::get_utxo_tx(&tx, &outpoint)? {
						utxo.mark_missing();
						Self::write_utxo_tx(&tx, &utxo)?;
					}
				},
			}
		}
		Self::next_tick_tx(&tx)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn release_operation(
		&mut self, operation_id: &OperationId, released_phase: OperationPhase,
	) -> DomainResult<Vec<OutPoint>> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		let released = Self::release_operation_tx(&tx, operation_id, released_phase)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(released)
	}

	fn release_selected_operation(
		&mut self, operation_id: &OperationId,
	) -> DomainResult<Option<Vec<OutPoint>>> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		if !Self::read_operation(&tx, operation_id)?
			.is_some_and(|operation| operation.phase == OperationPhase::Selected)
		{
			return Ok(None);
		}
		let released = Self::release_operation_tx(&tx, operation_id, OperationPhase::RollingBack)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(Some(released))
	}

	fn complete_operation(
		&mut self, operation_id: &OperationId, completed_phase: OperationPhase,
	) -> DomainResult<Vec<OutPoint>> {
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		let operation = Self::read_operation(&tx, operation_id)?
			.ok_or_else(|| DomainError::OperationNotFound(operation_id.clone()))?;
		let (_, status, _, _) = Self::read_intent_header(&tx, operation_id)?
			.ok_or_else(|| DomainError::IntentNotFound(operation_id.clone()))?;
		if status != IntentStatus::Committed {
			return Err(DomainError::IntentNotCommitted(operation_id.clone()));
		}
		if !operation.validate_advance(completed_phase)? {
			return Ok(Vec::new());
		}
		let mut stmt = tx
			.prepare("SELECT outpoint FROM utxos WHERE binding_operation_id = ?1 ORDER BY outpoint")
			.map_err(SqliteCodec::map_sql_error)?;
		let rows = stmt
			.query_map(params![operation_id.as_str()], |row| {
				SqliteCodec::decode_outpoint(&row.get::<_, String>(0)?)
			})
			.map_err(SqliteCodec::map_sql_error)?;
		let outpoints = Self::collect_rows(rows)?;
		drop(stmt);

		let mut completed = Vec::new();
		for outpoint in &outpoints {
			let mut utxo = Self::get_utxo_tx(&tx, outpoint)?
				.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
			utxo.advance_binding(completed_phase);
			Self::write_utxo_tx(&tx, &utxo)?;
			completed.push(outpoint.clone());
		}
		tx.execute(
			"UPDATE locks SET expires_at = NULL WHERE operation_id = ?1",
			params![operation_id.as_str()],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		let updated_at = Self::next_tick_tx(&tx)?;
		tx.execute(
			"
            INSERT OR REPLACE INTO operations(operation_id, phase, updated_at, expires_at)
            VALUES (?1, ?2, ?3, ?4)
            ",
			params![
				operation_id.as_str(),
				SqliteCodec::encode(completed_phase),
				updated_at,
				Option::<u64>::None
			],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)?;
		Ok(completed)
	}
}
