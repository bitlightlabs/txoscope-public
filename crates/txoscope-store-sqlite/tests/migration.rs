use rusqlite::{params, Connection};
use txoscope_core::{
	backend::{
		IntentStatus, LockPurpose, OperationIntent, OperationLock, OperationSelection,
		PersistedOperation, SelectionEffect, StateBatch, StateRead, StateStore,
	},
	Account, AccountId, Availability, Capability, CapabilitySet, DomainError, ManagedUtxo,
	OperationBinding, OperationId, OperationPhase, OutPoint, WalletScope, WalletScopeId,
};
use txoscope_store_sqlite::SqliteStateStore;

#[test]
fn sqlite_v1_to_v3_migration_preserves_legacy_capabilities() {
	let temp_dir = tempfile::tempdir().unwrap();
	let db_path = temp_dir.path().join("test.db");

	{
		let conn = Connection::open(&db_path).unwrap();
		conn.execute_batch(
			"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta(key, value) VALUES('tick', '7');
            CREATE TABLE accounts (
                account_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                capabilities TEXT NOT NULL
            );
            CREATE TABLE scopes (
                scope_id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                descriptor_ref TEXT NOT NULL,
                derivation_scope TEXT NOT NULL,
                capabilities TEXT NOT NULL,
                priority INTEGER NOT NULL,
                FOREIGN KEY(account_id) REFERENCES accounts(account_id)
            );
            CREATE TABLE utxos (
                outpoint TEXT PRIMARY KEY,
                txid TEXT NOT NULL,
                vout INTEGER NOT NULL,
                account_id TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                value_sats INTEGER NOT NULL,
                onchain_state TEXT NOT NULL,
                asset_state TEXT NOT NULL,
                availability TEXT NOT NULL,
                role TEXT NOT NULL,
                binding_operation_id TEXT,
                binding_phase TEXT,
                rgb_amount INTEGER NOT NULL
            );
            INSERT INTO accounts(account_id, name, capabilities)
            VALUES('rgb', 'L2', 'btc_receive,rgb_receive,reserve_support');
            INSERT INTO scopes(scope_id, account_id, descriptor_ref, derivation_scope, capabilities, priority)
            VALUES('rgb_scope', 'rgb', 'node_rgb_wallet', 'rgb', 'btc_receive,rgb_receive,reserve_support', 0);
            INSERT INTO utxos(
                outpoint, txid, vout, account_id, scope_id, value_sats, onchain_state, asset_state,
                availability, role, binding_operation_id, binding_phase, rgb_amount
            )
            VALUES(
                'tx1:0', 'tx1', 0, 'rgb', 'rgb_scope', 10000, 'confirmed', 'legacy', 'selectable',
                'general', NULL, NULL, 0
            );
            PRAGMA user_version = 1;
        ",
		)
		.unwrap();
	}

	let store = SqliteStateStore::open(&db_path).unwrap();

	let expected_capabilities = CapabilitySet::new([
		Capability::BtcReceive,
		Capability::SingleTargetReserve,
		Capability::ReserveSupport,
	]);
	assert_eq!(
		store.get_account(&"rgb".into()).unwrap().unwrap().capabilities,
		expected_capabilities
	);
	assert_eq!(
		store.get_scope(&"rgb_scope".into()).unwrap().unwrap().capabilities,
		expected_capabilities
	);
	assert_eq!(
		store.get_utxo(&OutPoint::new("tx1", 0)).unwrap().unwrap().outpoint,
		OutPoint::new("tx1", 0)
	);
	assert_eq!(store.current_tick().unwrap(), 7);

	let conn = Connection::open(&db_path).unwrap();
	let current_version: i64 =
		conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
	assert_eq!(current_version, 3);

	let table_info: Vec<String> = conn
		.prepare("PRAGMA table_info(utxos)")
		.unwrap()
		.query_map([], |row| row.get(1))
		.unwrap()
		.collect::<Result<Vec<_>, _>>()
		.unwrap();
	assert!(table_info.contains(&"conflict_message".to_string()));
	assert!(table_info.contains(&"conflict_recorded_at".to_string()));

	for table in ["locks", "operations", "intents"] {
		let columns: Vec<String> = conn
			.prepare(&format!("PRAGMA table_info({table})"))
			.unwrap()
			.query_map([], |row| row.get(1))
			.unwrap()
			.collect::<Result<Vec<_>, _>>()
			.unwrap();
		assert!(columns.contains(&"expires_at".to_string()));
	}
}

#[test]
fn migration_reopen_store_reads_existing_state() {
	let temp_dir = tempfile::tempdir().unwrap();
	let db_path = temp_dir.path().join("test.db");

	let capabilities = CapabilitySet::new([
		Capability::BtcReceive,
		Capability::SingleTargetReserve,
		Capability::ReserveSupport,
	]);
	let account = Account::new("rgb", "L2", Vec::new(), capabilities.clone());
	let scope =
		WalletScope::new("rgb_scope", "rgb", "node_rgb_wallet", "rgb", capabilities.clone(), 0);
	let utxo = ManagedUtxo::new(OutPoint::new("tx1", 0), "rgb", "rgb_scope", 10_000);

	let mut store = SqliteStateStore::open(&db_path).unwrap();
	store.put_account(account.clone()).unwrap();
	store.put_scope(scope.clone()).unwrap();
	store.put_utxo(utxo.clone()).unwrap();
	let persisted_tick = store.current_tick().unwrap();
	drop(store);

	let reopened = SqliteStateStore::open(&db_path).unwrap();
	assert_eq!(reopened.get_account(&"rgb".into()).unwrap().unwrap().id, account.id);
	assert_eq!(reopened.get_scope(&"rgb_scope".into()).unwrap(), Some(scope));
	assert_eq!(reopened.get_utxo(&OutPoint::new("tx1", 0)).unwrap(), Some(utxo));
	assert_eq!(reopened.current_tick().unwrap(), persisted_tick);
}

const LEGACY_ACCOUNT: &str = " account: Alice / 客户 #01 ";
const LEGACY_SCOPE: &str = " scope:main/外部 #02 ";
const LEGACY_OPERATION: &str = " operation:send/签名 #03 ";
const LEGACY_OUTPOINT: &str = "wallet:opaque-tx:4294967295";
const LEGACY_V3: &str = include_str!("fixtures/pre_typed_ids_v3.sql");

fn legacy_outpoint() -> OutPoint {
	OutPoint::new("wallet:opaque-tx", u32::MAX)
}

fn normalized_schema(conn: &Connection) -> String {
	let schema: String = conn
		.query_row(
			"SELECT group_concat(sql, char(10)) FROM (SELECT sql FROM sqlite_master ORDER BY name)",
			[],
			|row| row.get(0),
		)
		.unwrap();
	schema.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_raw_identifiers(conn: &Connection) {
	for (table, column, expected) in [
		("accounts", "account_id", LEGACY_ACCOUNT),
		("scopes", "scope_id", LEGACY_SCOPE),
		("scopes", "account_id", LEGACY_ACCOUNT),
		("utxos", "account_id", LEGACY_ACCOUNT),
		("utxos", "scope_id", LEGACY_SCOPE),
		("utxos", "binding_operation_id", LEGACY_OPERATION),
		("locks", "operation_id", LEGACY_OPERATION),
		("locks", "account_id", LEGACY_ACCOUNT),
		("locks", "scope_id", LEGACY_SCOPE),
		("operations", "operation_id", LEGACY_OPERATION),
		("intents", "operation_id", LEGACY_OPERATION),
		("intents", "change_scope_id", LEGACY_SCOPE),
		("intent_inputs", "operation_id", LEGACY_OPERATION),
		("intent_effects", "operation_id", LEGACY_OPERATION),
		("utxos", "outpoint", LEGACY_OUTPOINT),
		("locks", "outpoint", LEGACY_OUTPOINT),
		("intent_inputs", "outpoint", LEGACY_OUTPOINT),
		("intent_effects", "outpoint", LEGACY_OUTPOINT),
		("utxos", "txid", "wallet:opaque-tx"),
	] {
		let declared_type: String = conn
			.query_row(
				"SELECT type FROM pragma_table_info(?1) WHERE name = ?2",
				params![table, column],
				|row| row.get(0),
			)
			.unwrap();
		assert_eq!(declared_type, "TEXT", "{table}.{column}");
		let rows: Vec<(String, String)> = conn
			.prepare(&format!("SELECT {column}, typeof({column}) FROM {table}"))
			.unwrap()
			.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
			.unwrap()
			.collect::<Result<_, _>>()
			.unwrap();
		assert_eq!(rows, vec![(expected.into(), "text".into())], "{table}.{column}");
	}
	let vout: (u32, String) = conn
		.query_row("SELECT vout, typeof(vout) FROM utxos", [], |row| Ok((row.get(0)?, row.get(1)?)))
		.unwrap();
	assert_eq!(vout, (u32::MAX, "integer".into()));
	assert_eq!(
		conn.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0)).unwrap(),
		3
	);
	let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0)).unwrap();
	assert_eq!(integrity, "ok");
	assert!(!conn.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
}

fn legacy_selection() -> OperationSelection {
	OperationSelection {
		operation_id: OperationId::from(LEGACY_OPERATION),
		selected_outpoints: vec![legacy_outpoint()],
		change_scope_id: WalletScopeId::from(LEGACY_SCOPE),
		effects: vec![SelectionEffect::LockUtxo {
			outpoint: legacy_outpoint(),
			purpose: LockPurpose::PlannedSelection,
		}],
		expires_at: Some(2_000_000_000),
	}
}

#[test]
fn frozen_v3_database_reads_typed_identifiers_without_schema_or_value_changes() {
	let directory = tempfile::tempdir().unwrap();
	let path = directory.path().join("pre-typed-ids.sqlite");
	let conn = Connection::open(&path).unwrap();
	conn.execute_batch(LEGACY_V3).unwrap();
	let schema = normalized_schema(&conn);
	assert_raw_identifiers(&conn);
	drop(conn);

	let account_id = AccountId::from(LEGACY_ACCOUNT);
	let scope_id = WalletScopeId::from(LEGACY_SCOPE);
	let operation_id = OperationId::from(LEGACY_OPERATION);
	let capabilities = CapabilitySet::new([Capability::BtcSpend, Capability::ReserveSupport]);
	for _ in 0..2 {
		let store = SqliteStateStore::open(&path).unwrap();
		assert_eq!(
			store.get_account(&account_id).unwrap(),
			Some(Account::new(
				account_id.clone(),
				"Legacy account",
				vec![scope_id.clone()],
				capabilities.clone(),
			))
		);
		assert_eq!(
			store.get_scope(&scope_id).unwrap(),
			Some(WalletScope::new(
				scope_id.clone(),
				account_id.clone(),
				"fixture:descriptor",
				"external",
				capabilities.clone(),
				7,
			))
		);
		let mut expected_utxo = ManagedUtxo::new(
			OutPoint::new("wallet:opaque-tx", u32::MAX),
			account_id.clone(),
			scope_id.clone(),
			50_000,
		);
		expected_utxo.availability = Availability::Reserved;
		expected_utxo.operation_binding = Some(OperationBinding {
			operation_id: operation_id.clone(),
			phase: OperationPhase::Selected,
		});
		assert_eq!(store.get_utxo(&legacy_outpoint()).unwrap(), Some(expected_utxo.clone()));
		assert_eq!(store.list_utxos().unwrap(), vec![expected_utxo]);
		assert_eq!(
			store.get_lock(&legacy_outpoint()).unwrap(),
			Some(OperationLock {
				operation_id: operation_id.clone(),
				account_id: account_id.clone(),
				scope_id: scope_id.clone(),
				purpose: LockPurpose::PlannedSelection,
				owner: "fixture:owner/旧版".into(),
				created_at: 9,
				expires_at: Some(2_000_000_000),
			})
		);
		let operation = PersistedOperation {
			operation_id: operation_id.clone(),
			phase: OperationPhase::Selected,
			updated_at: 9,
			expires_at: Some(2_000_000_000),
		};
		assert_eq!(store.get_operation(&operation_id).unwrap(), Some(operation.clone()));
		assert_eq!(store.list_operations().unwrap(), vec![operation]);
		assert_eq!(
			store.get_intent(&operation_id).unwrap(),
			Some(OperationIntent {
				selection: legacy_selection(),
				status: IntentStatus::Committed,
				recorded_at: 8,
			})
		);
		assert!(store.list_pending_intents().unwrap().is_empty());
		assert_eq!(store.current_tick().unwrap(), 10);
		drop(store);

		let conn = Connection::open(&path).unwrap();
		assert_eq!(normalized_schema(&conn), schema);
		assert_raw_identifiers(&conn);
		let tick: String = conn
			.query_row("SELECT value FROM meta WHERE key = 'tick'", [], |row| row.get(0))
			.unwrap();
		assert_eq!(tick, "10");
	}
}

#[test]
fn typed_identifier_writes_keep_the_legacy_text_format_and_schema() {
	let directory = tempfile::tempdir().unwrap();
	let path = directory.path().join("typed-ids.sqlite");
	let account_id = AccountId::from(LEGACY_ACCOUNT);
	let scope_id = WalletScopeId::from(LEGACY_SCOPE);
	let operation_id = OperationId::from(LEGACY_OPERATION);
	let capabilities = CapabilitySet::new([Capability::BtcSpend, Capability::ReserveSupport]);
	let mut store = SqliteStateStore::open(&path).unwrap();
	store
		.put_account(Account::new(
			account_id.clone(),
			"Typed account",
			vec![],
			capabilities.clone(),
		))
		.unwrap();
	store
		.put_scope(WalletScope::new(
			scope_id.clone(),
			account_id.clone(),
			"descriptor",
			"external",
			capabilities,
			0,
		))
		.unwrap();
	store
		.put_utxo(ManagedUtxo::new(
			OutPoint::new("wallet:opaque-tx", u32::MAX),
			account_id,
			scope_id,
			50_000,
		))
		.unwrap();
	let selection = legacy_selection();
	store.persist_intent(selection.clone()).unwrap();
	store
		.apply_batch(StateBatch {
			selection: selection.clone(),
			phase: OperationPhase::Selected,
			owner: "typed-writer".into(),
		})
		.unwrap();
	store.mark_intent_committed(&operation_id).unwrap();
	drop(store);

	let reference = Connection::open_in_memory().unwrap();
	reference.execute_batch(LEGACY_V3).unwrap();
	let conn = Connection::open(&path).unwrap();
	assert_eq!(normalized_schema(&conn), normalized_schema(&reference));
	assert_raw_identifiers(&conn);
	drop(conn);

	let reopened = SqliteStateStore::open(&path).unwrap();
	assert_eq!(reopened.get_intent(&operation_id).unwrap().unwrap().selection, selection);
	assert_eq!(reopened.get_lock(&legacy_outpoint()).unwrap().unwrap().operation_id, operation_id);
	drop(reopened);
	assert_raw_identifiers(&Connection::open(&path).unwrap());
}

#[test]
fn malformed_or_noncanonical_persisted_intent_outpoints_are_corruption_without_mutation() {
	for table in ["intent_inputs", "intent_effects"] {
		for encoded in ["missing-separator", "tx:invalid", "tx:4294967296", "tx:00", "tx:+0"] {
			let directory = tempfile::tempdir().unwrap();
			let path = directory.path().join("corrupt-intent.sqlite");
			let conn = Connection::open(&path).unwrap();
			conn.execute_batch(LEGACY_V3).unwrap();
			let mut store = SqliteStateStore::open(&path).unwrap();
			let operation_id = OperationId::from(LEGACY_OPERATION);
			let before_operation = store.get_operation(&operation_id).unwrap();
			let before_utxo = store.get_utxo(&legacy_outpoint()).unwrap();
			let before_lock = store.get_lock(&legacy_outpoint()).unwrap();
			let before_tick = store.current_tick().unwrap();
			conn.execute(&format!("UPDATE {table} SET outpoint = ?1"), [encoded]).unwrap();
			conn.execute("UPDATE intents SET status = 'pending'", []).unwrap();

			assert!(
				matches!(store.get_intent(&operation_id), Err(DomainError::CorruptedState(_))),
				"{table}: {encoded}"
			);
			assert!(
				matches!(store.list_pending_intents(), Err(DomainError::CorruptedState(_))),
				"{table}: {encoded}"
			);
			assert!(
				matches!(
					store.apply_batch(StateBatch {
						selection: legacy_selection(),
						phase: OperationPhase::Selected,
						owner: "retry".into(),
					}),
					Err(DomainError::CorruptedState(_))
				),
				"{table}: {encoded}"
			);
			assert_eq!(store.get_operation(&operation_id).unwrap(), before_operation);
			assert_eq!(store.get_utxo(&legacy_outpoint()).unwrap(), before_utxo);
			assert_eq!(store.get_lock(&legacy_outpoint()).unwrap(), before_lock);
			assert_eq!(store.current_tick().unwrap(), before_tick);
			let persisted: String = conn
				.query_row(&format!("SELECT outpoint FROM {table}"), [], |row| row.get(0))
				.unwrap();
			assert_eq!(persisted, encoded);
		}
	}
}

#[test]
fn persisted_utxo_keys_must_be_canonical_and_match_their_components() {
	for (column, value) in [
		("outpoint", "missing-separator"),
		("outpoint", "wallet:opaque-tx:04294967295"),
		("outpoint", "wallet:opaque-tx:4294967296"),
		("txid", "wallet:different"),
	] {
		let directory = tempfile::tempdir().unwrap();
		let path = directory.path().join("corrupt-utxo.sqlite");
		let conn = Connection::open(&path).unwrap();
		conn.execute_batch(LEGACY_V3).unwrap();
		conn.execute(&format!("UPDATE utxos SET {column} = ?1"), [value]).unwrap();
		let store = SqliteStateStore::open(&path).unwrap();
		assert!(
			matches!(store.list_utxos(), Err(DomainError::CorruptedState(_))),
			"{column}: {value}"
		);
		if column == "txid" {
			assert!(matches!(
				store.get_utxo(&legacy_outpoint()),
				Err(DomainError::CorruptedState(_))
			));
		}
		assert_eq!(store.current_tick().unwrap(), 10);
		let persisted: String =
			conn.query_row(&format!("SELECT {column} FROM utxos"), [], |row| row.get(0)).unwrap();
		assert_eq!(persisted, value);
	}
}
