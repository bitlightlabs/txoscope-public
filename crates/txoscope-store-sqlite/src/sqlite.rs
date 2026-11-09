use crate::codec::SqliteCodec;

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use txoscope_core::{
	backend::{
		IntentStatus, OperationIntent, OperationLock, OperationSelection, PersistedOperation,
		SelectionEffect, StateRead,
	},
	Account, AccountId, Availability, DerivationScope, DescriptorRef, DomainError, DomainResult,
	ManagedUtxo, ObservationConflict, OperationBinding, OperationId, OperationPhase, OutPoint,
	WalletScope, WalletScopeId,
};

#[derive(Debug)]
#[non_exhaustive]
pub struct SqliteStateStore {
	conn: Connection,
}

type IntentHeader = (WalletScopeId, IntentStatus, u64, Option<u64>);

mod state_store;

#[cfg(test)]
mod tests;

impl SqliteStateStore {
	pub fn open(path: impl AsRef<Path>) -> DomainResult<Self> {
		let conn = Connection::open_with_flags(
			path,
			OpenFlags::SQLITE_OPEN_CREATE
				| OpenFlags::SQLITE_OPEN_READ_WRITE
				| OpenFlags::SQLITE_OPEN_NO_MUTEX,
		)
		.map_err(SqliteCodec::map_sql_error)?;
		let mut store = Self { conn };
		store.initialize()?;
		Ok(store)
	}

	pub fn open_in_memory() -> DomainResult<Self> {
		let conn = Connection::open_in_memory().map_err(SqliteCodec::map_sql_error)?;
		let mut store = Self { conn };
		store.initialize()?;
		Ok(store)
	}

	fn initialize(&mut self) -> DomainResult<()> {
		self.conn
			.busy_timeout(std::time::Duration::from_secs(5))
			.map_err(SqliteCodec::map_sql_error)?;
		self.conn
			.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
			.map_err(SqliteCodec::map_sql_error)?;
		crate::migrations::SqliteSchema::current()?.upgrade(&mut self.conn)?;
		// Do not alter journal mode before accepting the database version and schema.
		self.enable_wal()?;
		Ok(())
	}

	fn enable_wal(&self) -> DomainResult<()> {
		// Journal-mode lock upgrades can return BUSY without invoking SQLite's
		// busy handler. Retry after the standalone statement releases its locks.
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
		loop {
			match self.conn.pragma_update(None, "journal_mode", "WAL") {
				Ok(()) => return Ok(()),
				Err(rusqlite::Error::SqliteFailure(error, _))
					if matches!(
						error.code,
						rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
					) && std::time::Instant::now() < deadline =>
				{
					std::thread::sleep(std::time::Duration::from_millis(10));
				},
				Err(error) => return Err(SqliteCodec::map_sql_error(error)),
			}
		}
	}

	fn collect_rows<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> DomainResult<Vec<T>> {
		rows.collect::<Result<Vec<_>, _>>().map_err(SqliteCodec::map_sql_error)
	}

	fn next_tick_tx(tx: &Transaction<'_>) -> DomainResult<u64> {
		let current = Self::read_tick_tx(tx)?;
		let next = current.saturating_add(1);
		tx.execute("UPDATE meta SET value = ?1 WHERE key = 'tick'", params![next.to_string()])
			.map_err(SqliteCodec::map_sql_error)?;
		Ok(next)
	}

	fn read_tick_tx(tx: &Transaction<'_>) -> DomainResult<u64> {
		let value: String = tx
			.query_row("SELECT value FROM meta WHERE key = 'tick'", [], |row| row.get(0))
			.map_err(SqliteCodec::map_sql_error)?;
		value.parse::<u64>().map_err(|_| {
			DomainError::CorruptedState(format!("invalid tick value in sqlite meta table: {value}"))
		})
	}

	fn read_tick(&self) -> DomainResult<u64> {
		let value: String = self
			.conn
			.query_row("SELECT value FROM meta WHERE key = 'tick'", [], |row| row.get(0))
			.map_err(SqliteCodec::map_sql_error)?;
		value.parse::<u64>().map_err(|_| {
			DomainError::CorruptedState(format!("invalid tick value in sqlite meta table: {value}"))
		})
	}

	fn read_account(conn: &Connection, account_id: &AccountId) -> DomainResult<Option<Account>> {
		conn.query_row(
			"SELECT account_id, name, capabilities FROM accounts WHERE account_id = ?1",
			params![account_id.as_str()],
			|row| {
				Ok(Account {
					id: row.get::<_, String>(0)?.into(),
					name: row.get(1)?,
					scopes: Self::account_scopes(conn, account_id)?,
					capabilities: SqliteCodec::decode_capabilities(&row.get::<_, String>(2)?)?,
				})
			},
		)
		.optional()
		.map_err(SqliteCodec::map_sql_error)
	}

	fn read_scope(
		conn: &Connection, scope_id: &WalletScopeId,
	) -> DomainResult<Option<WalletScope>> {
		conn
            .query_row(
                "SELECT scope_id, account_id, descriptor_ref, derivation_scope, capabilities, priority FROM scopes WHERE scope_id = ?1",
                params![scope_id.as_str()],
                |row| {
                    Ok(WalletScope {
                        id: row.get::<_, String>(0)?.into(),
                        account_id: row.get::<_, String>(1)?.into(),
                        descriptor_ref: DescriptorRef(row.get(2)?),
                        derivation_scope: DerivationScope(row.get(3)?),
                        capabilities: SqliteCodec::decode_capabilities(&row.get::<_, String>(4)?)?,
                        priority: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(SqliteCodec::map_sql_error)
	}

	fn release_operation_tx(
		tx: &Transaction<'_>, operation_id: &OperationId, released_phase: OperationPhase,
	) -> DomainResult<Vec<OutPoint>> {
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

		let mut released = Vec::new();
		for outpoint in &outpoints {
			let mut utxo = Self::get_utxo_tx(tx, outpoint)?
				.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
			utxo.release_binding();
			Self::write_utxo_tx(tx, &utxo)?;
			released.push(outpoint.clone());
		}
		tx.execute("DELETE FROM locks WHERE operation_id = ?1", params![operation_id.as_str()])
			.map_err(SqliteCodec::map_sql_error)?;
		let updated_at = Self::next_tick_tx(tx)?;
		tx.execute(
			"
            INSERT OR REPLACE INTO operations(operation_id, phase, updated_at, expires_at)
            VALUES (?1, ?2, ?3, ?4)
            ",
			params![
				operation_id.as_str(),
				SqliteCodec::encode(released_phase),
				updated_at,
				Option::<u64>::None
			],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		Ok(released)
	}

	fn account_scopes(
		conn: &Connection, account_id: &AccountId,
	) -> rusqlite::Result<Vec<WalletScopeId>> {
		let mut stmt = conn.prepare(
			"SELECT scope_id FROM scopes WHERE account_id = ?1 ORDER BY priority, scope_id",
		)?;
		let rows = stmt
			.query_map(params![account_id.as_str()], |row| Ok(row.get::<_, String>(0)?.into()))?;
		rows.collect::<Result<Vec<_>, _>>()
	}

	fn get_utxo_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagedUtxo> {
		let binding_operation_id: Option<String> = row.get("binding_operation_id")?;
		let binding_phase: Option<String> = row.get("binding_phase")?;
		let conflict_message: Option<String> = row.get("conflict_message")?;
		let conflict_recorded_at: Option<u64> = row.get("conflict_recorded_at")?;
		if binding_operation_id.is_some() != binding_phase.is_some() {
			return Err(SqliteCodec::corrupted_state_error(
				"sqlite utxos row has mismatched binding_operation_id and binding_phase columns",
			));
		}
		if conflict_message.is_some() != conflict_recorded_at.is_some() {
			return Err(SqliteCodec::corrupted_state_error(
				"sqlite utxos row has mismatched conflict_message and conflict_recorded_at columns",
			));
		}

		let outpoint = SqliteCodec::decode_outpoint(&row.get::<_, String>("outpoint")?)?;
		let components = OutPoint::new(row.get::<_, String>("txid")?, row.get::<_, u32>("vout")?);
		if outpoint != components {
			return Err(SqliteCodec::corrupted_state_error(
				"sqlite utxos outpoint disagrees with txid/vout columns",
			));
		}
		Ok(ManagedUtxo {
			outpoint,
			account_id: row.get::<_, String>("account_id")?.into(),
			scope_id: row.get::<_, String>("scope_id")?.into(),
			value_sats: row.get("value_sats")?,
			onchain_state: SqliteCodec::decode(&row.get::<_, String>("onchain_state")?)?,
			availability: SqliteCodec::decode(&row.get::<_, String>("availability")?)?,
			role: SqliteCodec::decode(&row.get::<_, String>("role")?)?,
			operation_binding: match (binding_operation_id, binding_phase) {
				(Some(operation_id), Some(phase)) => Some(OperationBinding {
					operation_id: operation_id.into(),
					phase: SqliteCodec::decode(&phase)?,
				}),
				_ => None,
			},
			observation_conflict: match (conflict_message, conflict_recorded_at) {
				(Some(message), Some(recorded_at)) => {
					Some(ObservationConflict { message, recorded_at })
				},
				_ => None,
			},
		})
	}

	fn get_lock_row(conn: &Connection, outpoint: &OutPoint) -> DomainResult<Option<OperationLock>> {
		conn.query_row(
            "SELECT operation_id, account_id, scope_id, purpose, owner, created_at, expires_at FROM locks WHERE outpoint = ?1",
            params![outpoint.to_string()],
            |row| {
                Ok(OperationLock {
                    operation_id: row.get::<_, String>(0)?.into(),
                    account_id: row.get::<_, String>(1)?.into(),
                    scope_id: row.get::<_, String>(2)?.into(),
                    purpose: SqliteCodec::decode(&row.get::<_, String>(3)?)?,
                    owner: row.get(4)?,
                    created_at: row.get(5)?,
                    expires_at: row.get(6)?,
                })
            },
        )
		.optional()
		.map_err(SqliteCodec::map_sql_error)
	}

	fn get_lock_tx(
		tx: &Transaction<'_>, outpoint: &OutPoint,
	) -> DomainResult<Option<OperationLock>> {
		tx.query_row(
            "SELECT operation_id, account_id, scope_id, purpose, owner, created_at, expires_at FROM locks WHERE outpoint = ?1",
            params![outpoint.to_string()],
            |row| {
                Ok(OperationLock {
                    operation_id: row.get::<_, String>(0)?.into(),
                    account_id: row.get::<_, String>(1)?.into(),
                    scope_id: row.get::<_, String>(2)?.into(),
                    purpose: SqliteCodec::decode(&row.get::<_, String>(3)?)?,
                    owner: row.get(4)?,
                    created_at: row.get(5)?,
                    expires_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(SqliteCodec::map_sql_error)
	}

	fn get_utxo_tx(tx: &Transaction<'_>, outpoint: &OutPoint) -> DomainResult<Option<ManagedUtxo>> {
		tx.query_row(
			"SELECT * FROM utxos WHERE outpoint = ?1",
			params![outpoint.to_string()],
			Self::get_utxo_from_row,
		)
		.optional()
		.map_err(SqliteCodec::map_sql_error)
	}

	fn validate_lock_effect_tx(
		tx: &Transaction<'_>, selection: &OperationSelection, phase: OperationPhase,
		outpoint: &OutPoint,
	) -> DomainResult<()> {
		let utxo = Self::get_utxo_tx(tx, outpoint)?
			.ok_or_else(|| DomainError::UtxoNotFound(outpoint.clone()))?;
		let lock = Self::get_lock_tx(tx, outpoint)?;
		let lock_present = lock.is_some();
		if utxo.can_be_locked(lock_present) {
			return Ok(());
		}
		if utxo.operation_binding.as_ref().is_some_and(|binding| {
			binding.operation_id == selection.operation_id && binding.phase == phase
		}) && lock.as_ref().is_some_and(|lock| lock.operation_id == selection.operation_id)
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

	fn write_utxo_tx(tx: &Transaction<'_>, utxo: &ManagedUtxo) -> DomainResult<()> {
		let (binding_operation_id, binding_phase) = utxo
			.operation_binding
			.as_ref()
			.map(|binding| {
				(
					Some(binding.operation_id.as_str()),
					Some(SqliteCodec::encode(binding.phase).to_string()),
				)
			})
			.unwrap_or((None, None));
		let (conflict_message, conflict_recorded_at) = utxo
			.observation_conflict
			.as_ref()
			.map(|c| (Some(c.message.clone()), Some(c.recorded_at)))
			.unwrap_or((None, None));

		tx.execute(
			"
            INSERT OR REPLACE INTO utxos(
                outpoint, txid, vout, account_id, scope_id, value_sats, onchain_state,
                availability, role, binding_operation_id, binding_phase, conflict_message,
                conflict_recorded_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ",
			params![
				utxo.outpoint.to_string(),
				utxo.outpoint.txid,
				utxo.outpoint.vout,
				utxo.account_id.as_str(),
				utxo.scope_id.as_str(),
				utxo.value_sats,
				SqliteCodec::encode(utxo.onchain_state),
				SqliteCodec::encode(utxo.availability),
				SqliteCodec::encode(utxo.role),
				binding_operation_id,
				binding_phase,
				conflict_message,
				conflict_recorded_at,
			],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		Ok(())
	}

	fn read_intent_selection(
		conn: &Connection, operation_id: &OperationId, change_scope_id: WalletScopeId,
		expires_at: Option<u64>,
	) -> DomainResult<OperationSelection> {
		let mut input_stmt = conn
			.prepare(
				"SELECT outpoint FROM intent_inputs WHERE operation_id = ?1 ORDER BY position ASC",
			)
			.map_err(SqliteCodec::map_sql_error)?;
		let input_rows = input_stmt
			.query_map(params![operation_id.as_str()], |row| {
				SqliteCodec::decode_outpoint(&row.get::<_, String>(0)?)
			})
			.map_err(SqliteCodec::map_sql_error)?;
		let selected_outpoints = Self::collect_rows(input_rows)?;

		let mut effect_stmt = conn
            .prepare(
                "SELECT effect_kind, outpoint, purpose FROM intent_effects WHERE operation_id = ?1 ORDER BY position ASC",
            )
            .map_err(SqliteCodec::map_sql_error)?;
		let effect_rows = effect_stmt
			.query_map(params![operation_id.as_str()], |row| {
				let kind: String = row.get(0)?;
				match kind.as_str() {
					"lock_utxo" => Ok(SelectionEffect::LockUtxo {
						outpoint: SqliteCodec::decode_outpoint(&row.get::<_, String>(1)?)?,
						purpose: SqliteCodec::decode(&row.get::<_, String>(2)?)?,
					}),
					_ => Err(SqliteCodec::corrupted_state_error(format!(
						"unknown intent effect kind '{kind}' for operation {operation_id}",
					))),
				}
			})
			.map_err(SqliteCodec::map_sql_error)?;
		let effects = Self::collect_rows(effect_rows)?;

		Ok(OperationSelection {
			operation_id: operation_id.clone(),
			selected_outpoints,
			change_scope_id,
			effects,
			expires_at,
		})
	}

	fn read_operation(
		conn: &Connection, operation_id: &OperationId,
	) -> DomainResult<Option<PersistedOperation>> {
		conn.query_row(
			"SELECT operation_id, phase, updated_at, expires_at FROM operations WHERE operation_id = ?1",
			params![operation_id.as_str()],
			|row| {
				Ok(PersistedOperation {
					operation_id: row.get::<_, String>(0)?.into(),
					phase: SqliteCodec::decode(&row.get::<_, String>(1)?)?,
					updated_at: row.get(2)?,
					expires_at: row.get(3)?,
				})
			},
		)
		.optional()
		.map_err(SqliteCodec::map_sql_error)
	}

	fn read_intent_header(
		conn: &Connection, operation_id: &OperationId,
	) -> DomainResult<Option<IntentHeader>> {
		conn.query_row(
            "SELECT change_scope_id, status, recorded_at, expires_at FROM intents WHERE operation_id = ?1",
            params![operation_id.as_str()],
            |row| {
                Ok((
                    WalletScopeId::from(row.get::<_, String>(0)?),
                    SqliteCodec::decode(&row.get::<_, String>(1)?)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, Option<u64>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(SqliteCodec::map_sql_error)
	}
}
impl StateRead for SqliteStateStore {
	fn get_account(&self, account_id: &AccountId) -> DomainResult<Option<Account>> {
		Self::read_account(&self.conn, account_id)
	}

	fn get_scope(&self, scope_id: &WalletScopeId) -> DomainResult<Option<WalletScope>> {
		Self::read_scope(&self.conn, scope_id)
	}

	fn get_utxo(&self, outpoint: &OutPoint) -> DomainResult<Option<ManagedUtxo>> {
		self.conn
			.query_row(
				"SELECT * FROM utxos WHERE outpoint = ?1",
				params![outpoint.to_string()],
				Self::get_utxo_from_row,
			)
			.optional()
			.map_err(SqliteCodec::map_sql_error)
	}

	fn get_lock(&self, outpoint: &OutPoint) -> DomainResult<Option<OperationLock>> {
		Self::get_lock_row(&self.conn, outpoint)
	}

	fn get_operation(
		&self, operation_id: &OperationId,
	) -> DomainResult<Option<PersistedOperation>> {
		Self::read_operation(&self.conn, operation_id)
	}

	fn get_intent(&self, operation_id: &OperationId) -> DomainResult<Option<OperationIntent>> {
		let (change_scope_id, status, recorded_at, expires_at) =
			match Self::read_intent_header(&self.conn, operation_id)? {
				Some(header) => header,
				None => return Ok(None),
			};
		let selection =
			Self::read_intent_selection(&self.conn, operation_id, change_scope_id, expires_at)?;
		Ok(Some(OperationIntent { selection, status, recorded_at }))
	}

	fn list_utxos(&self) -> DomainResult<Vec<ManagedUtxo>> {
		let mut stmt = self
			.conn
			.prepare("SELECT * FROM utxos ORDER BY outpoint")
			.map_err(SqliteCodec::map_sql_error)?;
		let rows =
			stmt.query_map([], Self::get_utxo_from_row).map_err(SqliteCodec::map_sql_error)?;
		Self::collect_rows(rows)
	}

	fn list_operations(&self) -> DomainResult<Vec<PersistedOperation>> {
		let mut stmt = self
            .conn
            .prepare(
                "SELECT operation_id, phase, updated_at, expires_at FROM operations ORDER BY operation_id",
            )
			.map_err(SqliteCodec::map_sql_error)?;
		let rows = stmt
			.query_map([], |row| {
				Ok(PersistedOperation {
					operation_id: row.get::<_, String>(0)?.into(),
					phase: SqliteCodec::decode(&row.get::<_, String>(1)?)?,
					updated_at: row.get(2)?,
					expires_at: row.get(3)?,
				})
			})
			.map_err(SqliteCodec::map_sql_error)?;
		Self::collect_rows(rows)
	}

	fn list_pending_intents(&self) -> DomainResult<Vec<OperationIntent>> {
		let mut stmt = self
			.conn
			.prepare(
				"SELECT operation_id, change_scope_id, status, recorded_at, expires_at
             FROM intents WHERE status = 'pending' ORDER BY operation_id",
			)
			.map_err(SqliteCodec::map_sql_error)?;
		let rows = stmt
			.query_map([], |row| {
				Ok((
					OperationId::from(row.get::<_, String>(0)?),
					WalletScopeId::from(row.get::<_, String>(1)?),
					SqliteCodec::decode(&row.get::<_, String>(2)?)?,
					row.get::<_, u64>(3)?,
					row.get::<_, Option<u64>>(4)?,
				))
			})
			.map_err(SqliteCodec::map_sql_error)?;
		rows.map(|row| {
			let (operation_id, change_scope_id, status, recorded_at, expires_at) =
				row.map_err(SqliteCodec::map_sql_error)?;
			let selection = Self::read_intent_selection(
				&self.conn,
				&operation_id,
				change_scope_id,
				expires_at,
			)?;
			Ok(OperationIntent { selection, status, recorded_at })
		})
		.collect()
	}

	fn current_tick(&self) -> DomainResult<u64> {
		self.read_tick()
	}
}
