use rusqlite::{types::Value, Connection};
use txoscope_core::{
	backend::{IntentStatus, Orchestrator, SelectionEffect, StateRead},
	Availability, Capability, CapabilitySet, LockPurpose, ObservationScope, ObservationSnapshot,
	ObservedUtxo, OnchainState, OperationPhase, OutPoint,
};
use txoscope_store_sqlite::SqliteStateStore;

const V3: &str = include_str!("fixtures/pre_typed_ids_v3.sql");
const HISTORICAL: &str = "
UPDATE accounts SET capabilities = 'btc_spend,rgb_receive,l2_settlement,reserve_support';
UPDATE scopes SET capabilities = 'btc_spend,rgb_receive,l2_settlement,reserve_support';
UPDATE operations SET phase = 'awaiting_l2_finality';
UPDATE utxos SET binding_phase = 'awaiting_l2_finality', onchain_state = 'spending_in_flight', availability = 'blocked_by_l2';
UPDATE locks SET purpose = 'rgb_l1_send';
UPDATE intent_effects SET purpose = 'channel_funding';";

fn snapshot(conn: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
	let tables = conn
		.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
		.unwrap()
		.query_map([], |r| r.get::<_, String>(0))
		.unwrap()
		.collect::<Result<Vec<_>, _>>()
		.unwrap();
	let mut result = Vec::new();
	for table in tables.iter().map(String::as_str).chain(["sqlite_master"]) {
		let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();
		let columns = stmt.column_count();
		let rows = stmt
			.query_map([], |r| (0..columns).map(|i| r.get(i)).collect())
			.unwrap()
			.collect::<Result<Vec<Vec<Value>>, _>>()
			.unwrap();
		result.push((table.into(), rows));
	}
	let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
	result.push(("user_version".into(), vec![vec![Value::Integer(version)]]));
	result
}

#[test]
fn historical_v3_migrates_and_release_reopen_lagging_unspent_cannot_reselect() {
	for chain in ["spending_in_flight", "spent"] {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("historical.sqlite");
		let conn = Connection::open(&path).unwrap();
		conn.execute_batch(V3).unwrap();
		conn.execute_batch(HISTORICAL).unwrap();
		conn.execute("UPDATE utxos SET onchain_state = ?1", [chain]).unwrap();
		let mut before_release = None;
		for _ in 0..2 {
			let store = SqliteStateStore::open(&path).unwrap();
			let utxo = store.list_utxos().unwrap().remove(0);
			let binding = utxo.operation_binding.as_ref().unwrap();
			assert_eq!(binding.phase, OperationPhase::AwaitingConfirmation);
			assert_eq!(
				store.get_operation(&binding.operation_id).unwrap().unwrap().phase,
				binding.phase
			);
			assert_eq!(utxo.availability, Availability::BlockedBySpend);
			assert!(!utxo.is_policy_spendable());
			assert_eq!(
				store.get_lock(&utxo.outpoint).unwrap().unwrap().purpose,
				LockPurpose::PlannedSelection
			);
			assert_eq!(
				store.get_account(&utxo.account_id).unwrap().unwrap().capabilities,
				CapabilitySet::new([
					Capability::BtcSpend,
					Capability::SingleTargetReserve,
					Capability::ReserveSupport
				])
			);
			assert_eq!(
				store.get_scope(&utxo.scope_id).unwrap().unwrap().capabilities,
				store.get_account(&utxo.account_id).unwrap().unwrap().capabilities
			);
			assert_eq!(store.current_tick().unwrap(), 10);
			let raw: String =
				conn.query_row("SELECT purpose FROM intent_effects", [], |r| r.get(0)).unwrap();
			assert_eq!(raw, "planned_selection");
			let current = snapshot(&conn);
			if let Some(previous) = &before_release {
				assert_eq!(&current, previous);
			}
			before_release = Some(current);
		}
		let store = SqliteStateStore::open(&path).unwrap();
		let utxo = store.list_utxos().unwrap().remove(0);
		let operation_id = utxo.operation_binding.as_ref().unwrap().operation_id.clone();
		let mut orch = Orchestrator::with_store(store);
		orch.release_operation(&operation_id).unwrap();
		drop(orch);
		for _ in 0..2 {
			let mut orch = Orchestrator::with_store(SqliteStateStore::open(&path).unwrap());
			orch.reconcile_snapshot(
				ObservationSnapshot::new(20, ObservationScope::Full).add_utxo(
					ObservedUtxo::new(
						utxo.outpoint.clone(),
						utxo.account_id.clone(),
						utxo.scope_id.clone(),
						utxo.value_sats,
					)
					.confirmed(100, 20),
				),
			)
			.unwrap();
			let current = orch.get_utxo(&utxo.outpoint).unwrap().unwrap();
			assert_eq!(
				current.onchain_state,
				if chain == "spent" { OnchainState::Spent } else { OnchainState::SpendingInFlight }
			);
			assert_eq!(current.availability, Availability::BlockedBySpend);
			assert!(current.operation_binding.is_none());
			assert!(orch.get_lock(&utxo.outpoint).unwrap().is_none());
			assert!(!current.can_be_locked(false));
		}
		assert_eq!(
			conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0)).unwrap(),
			4
		);
	}
}

#[test]
fn v3_migration_preserves_pending_and_failed_invalid_selection_intents() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("persist-first.sqlite");
	let conn = Connection::open(&path).unwrap();
	conn.execute_batch(V3).unwrap();
	let store = SqliteStateStore::open(&path).unwrap();
	let original_operation = store.list_operations().unwrap().remove(0);
	let original_intent = store.get_intent(&original_operation.operation_id).unwrap().unwrap();
	let mut orch = Orchestrator::with_store(store);
	let mut expected = Vec::new();
	for status in [IntentStatus::Pending, IntentStatus::Failed] {
		for invalid in ["missing-scope", "missing-utxo", "empty-inputs", "mismatched-effects"] {
			let mut selection = original_intent.selection.clone();
			selection.operation_id = format!("{status:?}-{invalid}").into();
			match invalid {
				"missing-scope" => selection.change_scope_id = "absent-scope".into(),
				"missing-utxo" => {
					let outpoint = OutPoint::new("absent-input", 0);
					selection.selected_outpoints = vec![outpoint.clone()];
					selection.effects = vec![SelectionEffect::LockUtxo {
						outpoint,
						purpose: LockPurpose::PlannedSelection,
					}];
				},
				"empty-inputs" => {
					selection.selected_outpoints.clear();
					selection.effects.clear();
				},
				"mismatched-effects" => selection.effects.clear(),
				_ => unreachable!(),
			}
			// These are legitimate persist-first records, not manually fabricated corruption:
			// a crash before commit leaves Pending; admission failure records Failed.
			orch.persist_selection_intent(selection.clone()).unwrap();
			if status == IntentStatus::Failed {
				assert!(orch.commit_recorded_intent(&selection.operation_id).is_err(), "{invalid}");
			}
			let intent = orch.get_intent(&selection.operation_id).unwrap().unwrap();
			assert_eq!(intent.status, status, "{invalid}");
			assert_eq!(intent.selection, selection);
			expected.push(intent);
		}
	}
	let tick = orch.current_tick().unwrap();
	drop(orch);
	// v3 has the same table layout and persist-first intent representation. Restore its
	// version and historical vocabulary to exercise the upgrade alongside these records.
	conn.execute_batch(HISTORICAL).unwrap();
	conn.pragma_update(None, "user_version", 3).unwrap();
	let mut migrated_snapshot = None;
	for _ in 0..2 {
		let store = SqliteStateStore::open(&path).unwrap();
		for intent in &expected {
			assert_eq!(
				store.get_intent(&intent.selection.operation_id).unwrap().as_ref(),
				Some(intent)
			);
			assert!(store.get_operation(&intent.selection.operation_id).unwrap().is_none());
		}
		let mut pending = store.list_pending_intents().unwrap();
		pending.sort_by(|a, b| a.selection.operation_id.cmp(&b.selection.operation_id));
		let mut expected_pending = expected
			.iter()
			.filter(|i| i.status == IntentStatus::Pending)
			.cloned()
			.collect::<Vec<_>>();
		expected_pending.sort_by(|a, b| a.selection.operation_id.cmp(&b.selection.operation_id));
		assert_eq!(pending, expected_pending);
		assert_eq!(store.current_tick().unwrap(), tick);
		assert_eq!(store.list_operations().unwrap().len(), 1);
		let utxo = store.list_utxos().unwrap().remove(0);
		assert_eq!(utxo.availability, Availability::BlockedBySpend);
		assert_eq!(utxo.operation_binding.unwrap().operation_id, original_operation.operation_id);
		assert_eq!(
			store.get_lock(&utxo.outpoint).unwrap().unwrap().operation_id,
			original_operation.operation_id
		);
		drop(store);
		assert_eq!(
			conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0)).unwrap(),
			4
		);
		let current = snapshot(&conn);
		if let Some(previous) = &migrated_snapshot {
			assert_eq!(&current, previous);
		}
		migrated_snapshot = Some(current);
	}
}

#[test]
fn every_purpose_alias_is_migrated_in_locks_and_effects_without_granting_capabilities() {
	for alias in [
		"single_target_reserve",
		"anchor_with_fee_support",
		"rgb_receive_preparation",
		"rgb_l1_send",
		"channel_funding",
	] {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("aliases.sqlite");
		let conn = Connection::open(&path).unwrap();
		conn.execute_batch(V3).unwrap();
		for table in ["locks", "intent_effects"] {
			conn.execute(&format!("UPDATE {table} SET purpose = ?1"), [alias]).unwrap();
		}
		conn.execute_batch("UPDATE accounts SET capabilities = 'l2_settlement'; UPDATE scopes SET capabilities = 'l2_settlement';").unwrap();
		let store = SqliteStateStore::open(&path).unwrap();
		let utxo = store.list_utxos().unwrap().remove(0);
		assert_eq!(
			store.get_account(&utxo.account_id).unwrap().unwrap().capabilities,
			CapabilitySet::default()
		);
		assert_eq!(
			store.get_scope(&utxo.scope_id).unwrap().unwrap().capabilities,
			CapabilitySet::default()
		);
		for table in ["locks", "intent_effects"] {
			let value: String =
				conn.query_row(&format!("SELECT purpose FROM {table}"), [], |r| r.get(0)).unwrap();
			assert_eq!(value, "planned_selection");
		}
	}
}

fn assert_rollback(mutation: &str) {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("rollback.sqlite");
	let conn = Connection::open(&path).unwrap();
	conn.execute_batch(V3).unwrap();
	conn.execute_batch(HISTORICAL).unwrap();
	conn.execute_batch(mutation).unwrap();
	let before = snapshot(&conn);
	for _ in 0..2 {
		assert!(SqliteStateStore::open(&path).is_err(), "accepted: {mutation}");
		assert_eq!(snapshot(&conn), before, "mutated: {mutation}");
	}
}

#[test]
fn ambiguous_unknown_and_inconsistent_rows_roll_back_data_schema_and_version() {
	for mutation in [
		"UPDATE utxos SET onchain_state = 'confirmed'",
		"UPDATE utxos SET onchain_state = 'unknown'",
		"UPDATE utxos SET onchain_state = 'unconfirmed_incoming'",
		"UPDATE utxos SET availability = 'reserved'",
		"UPDATE utxos SET onchain_state = 'confirmed', availability = 'reserved'",
		"UPDATE utxos SET availability = 'future_state'",
		"UPDATE utxos SET onchain_state = 'future_state'",
		"UPDATE utxos SET role = 'future_role'",
		"UPDATE operations SET phase = 'future_phase'",
		"UPDATE utxos SET binding_phase = 'future_phase'",
		"UPDATE utxos SET binding_phase = 'awaiting_confirmation'",
		"UPDATE utxos SET binding_operation_id = NULL",
		"UPDATE utxos SET binding_phase = NULL",
		"UPDATE utxos SET conflict_message = 'conflict'",
		"UPDATE utxos SET conflict_recorded_at = 1",
		"UPDATE utxos SET binding_operation_id = 'absent'",
		"UPDATE utxos SET account_id = 'absent'",
		"UPDATE utxos SET scope_id = 'absent'",
		"UPDATE utxos SET txid = 'different'",
		"UPDATE locks SET operation_id = 'absent'",
		"UPDATE locks SET account_id = 'absent'",
		"UPDATE locks SET scope_id = 'absent'",
		"UPDATE locks SET purpose = 'future_purpose'",
		"UPDATE intent_effects SET purpose = 'future_purpose'",
		"UPDATE intent_effects SET purpose = 'btc_send'",
		"UPDATE intent_effects SET effect_kind = 'future_effect'",
		"UPDATE intents SET status = 'future_status'",
		"UPDATE intents SET status = 'pending'",
		"UPDATE intents SET change_scope_id = 'absent'",
		"UPDATE accounts SET capabilities = 'rgb_receive,unknown'",
		"UPDATE scopes SET capabilities = 'l2_settlement,'",
		"UPDATE intent_inputs SET position = 2",
		"UPDATE intent_effects SET position = 2",
		"UPDATE intent_inputs SET outpoint = 'absent:0'",
		"UPDATE intent_effects SET outpoint = 'absent:0'",
		"DELETE FROM locks",
		"DELETE FROM operations",
		"DELETE FROM meta",
		"UPDATE utxos SET binding_operation_id = NULL, binding_phase = NULL, onchain_state = 'confirmed', availability = 'selectable'; DELETE FROM locks",
		"CREATE TRIGGER fail_rewrite BEFORE UPDATE ON intent_effects BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
		"DROP TABLE intent_effects",
	] { assert_rollback(mutation); }
}

#[test]
fn actual_storage_types_and_ranges_are_validated_without_coercion() {
	for (table, columns) in [
		("accounts", "account_id,name,capabilities"),
		("scopes", "scope_id,account_id,capabilities,descriptor_ref,derivation_scope"),
		("utxos", "outpoint,txid,account_id,scope_id,onchain_state,availability,role,binding_operation_id,binding_phase"),
		("operations", "operation_id,phase"),
		("locks", "outpoint,operation_id,account_id,scope_id,purpose,owner"),
		("intents", "operation_id,change_scope_id,status"),
		("intent_inputs", "operation_id,outpoint"),
		("intent_effects", "operation_id,outpoint,effect_kind,purpose"),
		("meta", "key,value"),
	] {
		for column in columns.split(',') {
			assert_rollback(&format!("PRAGMA foreign_keys = OFF; UPDATE {table} SET {column} = X'626164'"));
		}
	}
	for (table, columns) in [
		("scopes", "priority"),
		("utxos", "vout,value_sats,conflict_recorded_at"),
		("locks", "created_at,expires_at"),
		("operations", "updated_at,expires_at"),
		("intents", "recorded_at,expires_at"),
		("intent_inputs", "position"),
		("intent_effects", "position"),
	] {
		for column in columns.split(',') {
			for value in ["1.5", "'invalid'", "X'31'", "-1"] {
				assert_rollback(&format!("UPDATE {table} SET {column} = {value}"));
			}
		}
	}
	assert_rollback("UPDATE scopes SET priority = 4294967296");
	assert_rollback("UPDATE utxos SET vout = 4294967296");
	assert_rollback("UPDATE utxos SET binding_operation_id = NULL, binding_phase = X'00'");
}
