use rusqlite::Transaction;

use super::Migration;

pub(super) struct V1;

impl Migration for V1 {
	fn version(&self) -> i64 {
		1
	}

	fn name(&self) -> &'static str {
		"initial tables"
	}

	fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		tx.execute_batch(include_str!("v1.sql"))?;
		tx.execute("INSERT OR IGNORE INTO meta(key, value) VALUES('tick', '0')", [])?;
		Ok(())
	}
}
