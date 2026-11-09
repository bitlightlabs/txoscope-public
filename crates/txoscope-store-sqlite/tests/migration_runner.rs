use rusqlite::Connection;
use txoscope_store_sqlite::SqliteStateStore;

fn fingerprint(conn: &Connection) -> (i64, String, String) {
	let version = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
	let schema = conn.query_row(
		"SELECT coalesce(group_concat(sql, char(10)), '') FROM (SELECT sql FROM sqlite_master ORDER BY name)",
		[], |r| r.get(0),
	).unwrap();
	let journal = conn.pragma_query_value(None, "journal_mode", |r| r.get(0)).unwrap();
	(version, schema, journal)
}

#[test]
fn rejects_future_and_unversioned_databases_without_changing_them() {
	for version in [0, 999] {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("state.db");
		let conn = Connection::open(&path).unwrap();
		conn.execute_batch(
			"CREATE TABLE unrelated (value TEXT); INSERT INTO unrelated VALUES ('keep');",
		)
		.unwrap();
		conn.pragma_update(None, "user_version", version).unwrap();
		let before = fingerprint(&conn);
		assert!(SqliteStateStore::open(&path).is_err());
		assert_eq!(fingerprint(&conn), before);
		assert_eq!(
			conn.query_row::<String, _, _>("SELECT value FROM unrelated", [], |r| r.get(0))
				.unwrap(),
			"keep"
		);
	}
}

#[test]
fn current_schema_is_validated_without_silently_recreating_missing_tables() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("state.db");
	drop(SqliteStateStore::open(&path).unwrap());
	let conn = Connection::open(&path).unwrap();
	conn.execute_batch("DROP TABLE intent_effects").unwrap();
	let before = fingerprint(&conn);
	assert!(SqliteStateStore::open(&path).is_err());
	assert_eq!(fingerprint(&conn), before);
}

#[test]
fn concurrent_openers_migrate_once_and_reopen_without_schema_changes() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("state.db");
	// Create the file before the threads compete for the migration writer lock.
	drop(Connection::open(&path).unwrap());
	let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
	let workers: Vec<_> = (0..4)
		.map(|_| {
			let path = path.clone();
			let barrier = barrier.clone();
			std::thread::spawn(move || {
				barrier.wait();
				SqliteStateStore::open(path).unwrap();
			})
		})
		.collect();
	for worker in workers {
		worker.join().unwrap();
	}
	let conn = Connection::open(&path).unwrap();
	let before = fingerprint(&conn);
	drop(SqliteStateStore::open(&path).unwrap());
	assert_eq!(fingerprint(&conn), before);
	assert_eq!(before.0, 4);
}

#[test]
fn v2_upgrade_adds_expiry_columns() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("state.db");
	let conn = Connection::open(&path).unwrap();
	conn.execute_batch(include_str!("../src/migrations/v1.sql")).unwrap();
	conn.execute_batch("INSERT INTO meta VALUES ('tick', '12'); ALTER TABLE utxos ADD COLUMN conflict_message TEXT; ALTER TABLE utxos ADD COLUMN conflict_recorded_at INTEGER; PRAGMA user_version = 2;").unwrap();
	drop(SqliteStateStore::open(&path).unwrap());
	assert_eq!(fingerprint(&conn).0, 4);
	for table in ["locks", "operations", "intents"] {
		assert!(conn.prepare(&format!("SELECT expires_at FROM {table}")).is_ok());
	}
}
