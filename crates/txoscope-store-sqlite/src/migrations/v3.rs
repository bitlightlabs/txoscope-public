use rusqlite::Transaction;

use super::{v1::V1, Migration};

pub(super) struct V3;

impl V3 {
	fn add_expiry_columns(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		for table in ["locks", "operations", "intents"] {
			let exists: bool = tx.query_row(
				"SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'expires_at')",
				[table],
				|row| row.get(0),
			)?;
			if !exists {
				tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN expires_at INTEGER"))?;
			}
		}
		Ok(())
	}
}

impl Migration for V3 {
	fn version(&self) -> i64 {
		3
	}

	fn name(&self) -> &'static str {
		"reservation expiry"
	}

	fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		// Historical v2 initialization also allowed missing baseline tables.
		V1.upgrade(tx)?;
		self.add_expiry_columns(tx)
	}
}
