use rusqlite::Transaction;

use super::{v1::V1, Migration};

pub(super) struct V2;

impl V2 {
	fn add_conflict_columns(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		for (column, ty) in [("conflict_message", "TEXT"), ("conflict_recorded_at", "INTEGER")] {
			let exists: bool = tx.query_row(
				"SELECT EXISTS(SELECT 1 FROM pragma_table_info('utxos') WHERE name = ?1)",
				[column],
				|row| row.get(0),
			)?;
			if !exists {
				tx.execute_batch(&format!("ALTER TABLE utxos ADD COLUMN {column} {ty}"))?;
			}
		}
		Ok(())
	}
}

impl Migration for V2 {
	fn version(&self) -> i64 {
		2
	}

	fn name(&self) -> &'static str {
		"observation conflicts"
	}

	fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		// Historical v1 files may omit lazily created tables. Only this upgrade
		// repairs them; opening a current schema never does.
		V1.upgrade(tx)?;
		self.add_conflict_columns(tx)
	}
}
