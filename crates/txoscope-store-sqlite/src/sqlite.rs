use crate::codec::SqliteCodec;

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use txoscope_core::{
	backend::{
		IntentStatus, OperationIntent, OperationLock, OperationSelection, PersistedOperation,
		ReconcilePlan, ReconciliationAction, SelectionEffect, StateBatch, StateRead, StateStore,
	},
	Account, AccountId, Availability, DerivationScope, DescriptorRef, DomainError, DomainResult,
	ManagedUtxo, ObservationConflict, OperationBinding, OperationId, OperationPhase, OutPoint,
	WalletScope, WalletScopeId,
};

#[derive(Debug)]
pub struct SqliteStateStore {
	conn: Connection,
}

const SCHEMA_VERSION: i64 = 3;

type IntentHeader = (WalletScopeId, IntentStatus, u64, Option<u64>);

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
			.execute_batch(
				"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS accounts (
                account_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                capabilities TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS scopes (
                scope_id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                descriptor_ref TEXT NOT NULL,
                derivation_scope TEXT NOT NULL,
                capabilities TEXT NOT NULL,
                priority INTEGER NOT NULL,
                FOREIGN KEY(account_id) REFERENCES accounts(account_id)
            );

            CREATE TABLE IF NOT EXISTS utxos (
                outpoint TEXT PRIMARY KEY,
                txid TEXT NOT NULL,
                vout INTEGER NOT NULL,
                account_id TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                value_sats INTEGER NOT NULL,
                onchain_state TEXT NOT NULL,
                availability TEXT NOT NULL,
                role TEXT NOT NULL,
                binding_operation_id TEXT,
                binding_phase TEXT,
                conflict_message TEXT,
                conflict_recorded_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS locks (
                outpoint TEXT PRIMARY KEY,
                operation_id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                purpose TEXT NOT NULL,
                owner TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS operations (
                operation_id TEXT PRIMARY KEY,
                phase TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                expires_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS intents (
                operation_id TEXT PRIMARY KEY,
                change_scope_id TEXT NOT NULL,
                status TEXT NOT NULL,
                recorded_at INTEGER NOT NULL,
                expires_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS intent_inputs (
                operation_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                outpoint TEXT NOT NULL,
                PRIMARY KEY(operation_id, position),
                FOREIGN KEY(operation_id) REFERENCES intents(operation_id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS intent_effects (
                operation_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                effect_kind TEXT NOT NULL,
                outpoint TEXT NOT NULL,
                purpose TEXT NOT NULL,
                PRIMARY KEY(operation_id, position),
                FOREIGN KEY(operation_id) REFERENCES intents(operation_id) ON DELETE CASCADE
            );
            ",
			)
			.map_err(SqliteCodec::map_sql_error)?;

		self.ensure_current_schema()?;

		self.conn
			.execute("INSERT OR IGNORE INTO meta(key, value) VALUES('tick', '0')", [])
			.map_err(SqliteCodec::map_sql_error)?;

		Ok(())
	}

	fn ensure_current_schema(&mut self) -> DomainResult<()> {
		let current_version: i64 = self
			.conn
			.pragma_query_value(None, "user_version", |row| row.get(0))
			.map_err(SqliteCodec::map_sql_error)?;

		if current_version > SCHEMA_VERSION {
			return Err(DomainError::Storage(format!(
				"unsupported sqlite schema version: {current_version}"
			)));
		}

		if current_version < 2 {
			let utxo_info = self.table_columns("utxos")?;
			if !utxo_info.contains(&"conflict_message".to_string()) {
				self.conn
					.execute("ALTER TABLE utxos ADD COLUMN conflict_message TEXT", [])
					.map_err(SqliteCodec::map_sql_error)?;
			}
			if !utxo_info.contains(&"conflict_recorded_at".to_string()) {
				self.conn
					.execute("ALTER TABLE utxos ADD COLUMN conflict_recorded_at INTEGER", [])
					.map_err(SqliteCodec::map_sql_error)?;
			}
			self.conn.pragma_update(None, "user_version", 2).map_err(SqliteCodec::map_sql_error)?;
		}

		if current_version < 3 {
			for table in ["locks", "operations", "intents"] {
				let columns = self.table_columns(table)?;
				if !columns.contains(&"expires_at".to_string()) {
					self.conn
						.execute(&format!("ALTER TABLE {table} ADD COLUMN expires_at INTEGER"), [])
						.map_err(SqliteCodec::map_sql_error)?;
				}
			}
		}

		for (table, query) in [
			(
				"utxos",
				"SELECT outpoint, txid, vout, account_id, scope_id, value_sats, onchain_state, availability, role, binding_operation_id, binding_phase, conflict_message, conflict_recorded_at FROM utxos LIMIT 0",
			),
			(
				"locks",
				"SELECT outpoint, operation_id, account_id, scope_id, purpose, owner, created_at, expires_at FROM locks LIMIT 0",
			),
			(
				"operations",
				"SELECT operation_id, phase, updated_at, expires_at FROM operations LIMIT 0",
			),
			(
				"intents",
				"SELECT operation_id, change_scope_id, status, recorded_at, expires_at FROM intents LIMIT 0",
			),
		] {
			self.conn.prepare(query).map_err(|err| {
				DomainError::Storage(format!(
					"unsupported sqlite schema in table '{table}': {err}"
				))
			})?;
		}

		self.conn
			.pragma_update(None, "user_version", SCHEMA_VERSION)
			.map_err(SqliteCodec::map_sql_error)?;

		Ok(())
	}

	fn table_columns(&self, table: &str) -> DomainResult<Vec<String>> {
		let mut stmt = self
			.conn
			.prepare(&format!("PRAGMA table_info({table})"))
			.map_err(SqliteCodec::map_sql_error)?;
		let rows = stmt.query_map([], |row| row.get(1)).map_err(SqliteCodec::map_sql_error)?;
		Self::collect_rows(rows)
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
		if let Some(existing) = self.get_intent(&selection.operation_id)? {
			if existing.selection != selection {
				return Err(DomainError::IntentConflict { operation_id: selection.operation_id });
			}
			return Ok(existing);
		}
		let tx = self.conn.transaction().map_err(SqliteCodec::map_sql_error)?;
		let recorded_at = Self::next_tick_tx(&tx)?;
		tx.execute(
            "
            INSERT OR REPLACE INTO intents(operation_id, change_scope_id, status, recorded_at, expires_at)
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
		tx.execute(
			"DELETE FROM intent_inputs WHERE operation_id = ?1",
			params![selection.operation_id.as_str()],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		tx.execute(
			"DELETE FROM intent_effects WHERE operation_id = ?1",
			params![selection.operation_id.as_str()],
		)
		.map_err(SqliteCodec::map_sql_error)?;
		for (position, outpoint) in selection.selected_outpoints.iter().enumerate() {
			tx.execute(
				"INSERT INTO intent_inputs(operation_id, position, outpoint) VALUES (?1, ?2, ?3)",
				params![selection.operation_id.as_str(), position as i64, outpoint.to_string()],
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
                            position as i64,
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
					let already_applied = utxo.operation_binding.as_ref().is_some_and(|binding| {
						binding.operation_id == batch.selection.operation_id
							&& binding.phase == batch.phase
					}) && existing_lock
						.as_ref()
						.is_some_and(|lock| lock.operation_id == batch.selection.operation_id);
					if already_applied {
						let lock = existing_lock.expect("existing lock checked above");
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
