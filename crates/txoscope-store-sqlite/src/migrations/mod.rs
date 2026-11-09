//! Ordered, forward-only schema and data migrations. Never decode old rows with
//! current domain enums: each step owns the SQL vocabulary of its source version.
use rusqlite::{Connection, Transaction, TransactionBehavior};
use txoscope_core::{DomainError, DomainResult};

use crate::codec::SqliteCodec;

mod v1;
mod v2;
mod v3;
mod v4;

/// A version owns its historical schema and vocabulary. Implementations only
/// change data/schema through the supplied transaction; the runner owns commit
/// and user_version advancement.
trait Migration {
	fn version(&self) -> i64;
	fn name(&self) -> &'static str;
	fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()>;
}

const MIGRATIONS: &[&dyn Migration] = &[&v1::V1, &v2::V2, &v3::V3, &v4::V4];

/// Owns the supported schema and its ordered upgrade path. Callers cannot choose
/// a target version independently from the migrations or bypass final validation.
pub(crate) struct SqliteSchema<'a> {
	migrations: &'a [&'a dyn Migration],
}

impl SqliteSchema<'static> {
	pub(crate) fn current() -> DomainResult<Self> {
		Self::new(MIGRATIONS)
	}
}

impl<'a> SqliteSchema<'a> {
	fn new(migrations: &'a [&'a dyn Migration]) -> DomainResult<Self> {
		if migrations.is_empty() {
			return Err(DomainError::Storage("non-contiguous sqlite migration registry".into()));
		}
		for (index, migration) in migrations.iter().enumerate() {
			if migration.version() != index as i64 + 1 {
				return Err(DomainError::Storage(
					"non-contiguous sqlite migration registry".into(),
				));
			}
		}
		Ok(Self { migrations })
	}

	fn version(&self) -> i64 {
		self.migrations.len() as i64
	}

	pub(crate) fn upgrade(&self, conn: &mut Connection) -> DomainResult<()> {
		// Read the version only after acquiring the writer lock. Concurrent openers
		// must not both decide to apply the same migration from a stale version.
		let tx = conn
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(SqliteCodec::map_sql_error)?;
		let version: i64 = tx
			.pragma_query_value(None, "user_version", |row| row.get(0))
			.map_err(SqliteCodec::map_sql_error)?;
		if !(0..=self.version()).contains(&version) {
			return Err(DomainError::Storage(format!(
				"unsupported sqlite schema version: {version}"
			)));
		}
		if version == 0 {
			let occupied: bool = tx
				.query_row(
					"SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name NOT LIKE 'sqlite_%')",
					[],
					|row| row.get(0),
				)
				.map_err(SqliteCodec::map_sql_error)?;
			if occupied {
				return Err(DomainError::Storage("unversioned non-empty sqlite database".into()));
			}
		}
		for migration in self.migrations {
			if migration.version() <= version {
				continue;
			}
			if let Err(err) = migration.upgrade(&tx) {
				return Err(DomainError::Storage(format!(
					"sqlite migration {} ({}) failed: {err}",
					migration.version(),
					migration.name(),
				)));
			}
			tx.pragma_update(None, "user_version", migration.version())
				.map_err(SqliteCodec::map_sql_error)?;
		}
		self.validate(&tx)?;
		tx.commit().map_err(SqliteCodec::map_sql_error)
	}

	fn validate(&self, tx: &Transaction<'_>) -> DomainResult<()> {
		for (table, columns) in [
		("meta", "key, value"),
		("accounts", "account_id, name, capabilities"),
		("scopes", "scope_id, account_id, descriptor_ref, derivation_scope, capabilities, priority"),
		("utxos", "outpoint, txid, vout, account_id, scope_id, value_sats, onchain_state, availability, role, binding_operation_id, binding_phase, conflict_message, conflict_recorded_at"),
		("locks", "outpoint, operation_id, account_id, scope_id, purpose, owner, created_at, expires_at"),
		("operations", "operation_id, phase, updated_at, expires_at"),
		("intents", "operation_id, change_scope_id, status, recorded_at, expires_at"),
		("intent_inputs", "operation_id, position, outpoint"),
		("intent_effects", "operation_id, position, effect_kind, outpoint, purpose"),
	] {
		tx.prepare(&format!("SELECT {columns} FROM {table} LIMIT 0")).map_err(|err|
			DomainError::Storage(format!("unsupported sqlite schema in table '{table}': {err}")))?;
	}
		let tick: String = tx
			.query_row("SELECT value FROM meta WHERE key = 'tick'", [], |row| row.get(0))
			.map_err(SqliteCodec::map_sql_error)?;
		tick.parse::<u64>()
			.map_err(|_| DomainError::CorruptedState("invalid sqlite tick".into()))?;
		if tx
			.prepare("PRAGMA foreign_key_check")
			.map_err(SqliteCodec::map_sql_error)?
			.exists([])
			.map_err(SqliteCodec::map_sql_error)?
		{
			return Err(DomainError::CorruptedState("sqlite foreign key check failed".into()));
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	struct RewriteProbe;

	impl Migration for RewriteProbe {
		fn version(&self) -> i64 {
			5
		}
		fn name(&self) -> &'static str {
			"data rewrite"
		}
		fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
			assert_eq!(tx.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))?, 4);
			tx.execute_batch(
				"CREATE TABLE probe (id INTEGER); UPDATE meta SET value = '99' WHERE key = 'tick';",
			)
		}
	}

	struct FinalStep {
		fail: bool,
	}

	impl Migration for FinalStep {
		fn version(&self) -> i64 {
			6
		}
		fn name(&self) -> &'static str {
			"final step"
		}
		fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
			assert_eq!(tx.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))?, 5);
			if self.fail {
				tx.execute_batch("INSERT INTO missing_table VALUES (1)")?;
			}
			Ok(())
		}
	}

	struct InvalidSchema;

	impl Migration for InvalidSchema {
		fn version(&self) -> i64 {
			5
		}
		fn name(&self) -> &'static str {
			"invalid final schema"
		}
		fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
			tx.execute_batch("DROP TABLE intent_effects")
		}
	}

	#[test]
	fn failed_later_step_rolls_back_schema_data_and_version_then_retries() {
		let mut conn = Connection::open_in_memory().unwrap();
		SqliteSchema::current().unwrap().upgrade(&mut conn).unwrap();
		let before: i64 =
			conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
		let mut steps = MIGRATIONS.to_vec();
		steps.push(&RewriteProbe);
		steps.push(&FinalStep { fail: true });
		assert!(SqliteSchema::new(&steps).unwrap().upgrade(&mut conn).is_err());
		assert_eq!(
			conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0)).unwrap(),
			4
		);
		assert_eq!(
			conn.query_row::<String, _, _>("SELECT value FROM meta WHERE key = 'tick'", [], |r| r
				.get(0))
				.unwrap(),
			"0"
		);
		assert_eq!(
			conn.query_row::<i64, _, _>("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
				.unwrap(),
			before
		);
		steps[5] = &FinalStep { fail: false };
		SqliteSchema::new(&steps).unwrap().upgrade(&mut conn).unwrap();
		assert_eq!(
			conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0)).unwrap(),
			6
		);
		assert_eq!(
			conn.query_row::<String, _, _>("SELECT value FROM meta WHERE key = 'tick'", [], |r| r
				.get(0))
				.unwrap(),
			"99"
		);
	}

	#[test]
	fn validation_failure_also_rolls_back_bootstrap() {
		let mut conn = Connection::open_in_memory().unwrap();
		let mut steps = MIGRATIONS.to_vec();
		steps.push(&InvalidSchema);
		assert!(SqliteSchema::new(&steps).unwrap().upgrade(&mut conn).is_err());
		assert_eq!(
			conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0)).unwrap(),
			0
		);
		assert_eq!(
			conn.query_row::<i64, _, _>("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
				.unwrap(),
			0
		);
		SqliteSchema::current().unwrap().upgrade(&mut conn).unwrap();
	}

	#[test]
	fn schema_rejects_empty_or_non_contiguous_upgrade_paths() {
		assert!(SqliteSchema::new(&[]).is_err());
		assert!(SqliteSchema::new(&MIGRATIONS[1..]).is_err());
		let mut steps = MIGRATIONS.to_vec();
		steps.swap(0, 1);
		assert!(SqliteSchema::new(&steps).is_err());
	}
}
