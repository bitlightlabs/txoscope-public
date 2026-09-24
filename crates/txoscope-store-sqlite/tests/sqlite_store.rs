use rusqlite::{params, Connection};
use tempfile::{NamedTempFile, TempDir};
use txoscope_core::{
	backend::{
		BtcSendRequest, LockPurpose, OperationSelection, Orchestrator, SelectionEffect, StateRead,
		StateStore,
	},
	Account, Availability, Capability, CapabilitySet, DomainError, ManagedUtxo, OperationPhase,
	OutPoint, WalletScope,
};
use txoscope_store_sqlite::SqliteStateStore;

type SqliteOrchestrator = Orchestrator<SqliteStateStore>;

fn temp_store() -> SqliteStateStore {
	let file = NamedTempFile::new().unwrap();
	SqliteStateStore::open(file.path()).unwrap()
}

fn persistent_store() -> (TempDir, std::path::PathBuf) {
	let dir = TempDir::new().unwrap();
	let path = dir.path().join("txoscope.sqlite");
	(dir, path)
}

fn base_store() -> SqliteStateStore {
	let mut store = temp_store();
	store
		.put_account(Account::new(
			"btc",
			"BTC",
			vec![],
			CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
		))
		.unwrap();
	store
		.put_scope(WalletScope::new(
			"btc_primary_scope",
			"btc",
			"wpkh(desc-btc)",
			"m/84'/0'/0'/0/*",
			CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
			10,
		))
		.unwrap();
	store
}

fn initialize_base_store(store: &mut SqliteStateStore) {
	store
		.put_account(Account::new(
			"btc",
			"BTC",
			vec![],
			CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
		))
		.unwrap();
	store
		.put_scope(WalletScope::new(
			"btc_primary_scope",
			"btc",
			"wpkh(desc-btc)",
			"m/84'/0'/0'/0/*",
			CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
			10,
		))
		.unwrap();
}

#[test]
fn sqlite_store_enables_wal_journal_mode() {
	let file = NamedTempFile::new().unwrap();
	let path = file.path().to_path_buf();
	let store = SqliteStateStore::open(&path).unwrap();
	let tick = store.current_tick().unwrap();
	assert_eq!(tick, 0);
	drop(store);

	let conn = Connection::open(path).unwrap();
	let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
	assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn sqlite_store_can_persist_and_recover_pending_intent() {
	let (_dir, path) = persistent_store();
	let mut store = SqliteStateStore::open(&path).unwrap();
	initialize_base_store(&mut store);
	store
		.put_utxo(ManagedUtxo::new(OutPoint::new("txr1", 0), "btc", "btc_primary_scope", 70_000))
		.unwrap();

	let selection = SqliteOrchestrator::with_store(store)
		.propose_operation(BtcSendRequest::new("op-recover", "btc", 50_000))
		.unwrap();

	let mut store = SqliteStateStore::open(&path).unwrap();
	let existing = store.get_utxo(&OutPoint::new("txr1", 0)).unwrap().unwrap();
	assert_eq!(existing.value_sats, 70_000);
	store.persist_intent(selection).unwrap();
	drop(store);

	let mut orch = SqliteOrchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	let report = orch.recover_pending_intents().unwrap();
	assert_eq!(report.replayed_operations, vec![txoscope_core::OperationId::from("op-recover")]);
	assert!(report.failed_operations.is_empty());
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txr1", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
	assert_eq!(
		orch.get_operation(&"op-recover".into()).unwrap().unwrap().phase,
		OperationPhase::Selected
	);
}

#[test]
fn sqlite_store_preserves_selected_operation_across_reopen() {
	let (_dir, path) = persistent_store();
	let mut orch = SqliteOrchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	orch.add_account(Account::new(
		"btc",
		"BTC",
		vec![],
		CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
	))
	.unwrap();
	orch.add_scope(WalletScope::new(
		"btc_primary_scope",
		"btc",
		"wpkh(desc-btc)",
		"m/84'/0'/0'/0/*",
		CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
		10,
	))
	.unwrap();
	orch.add_utxo(ManagedUtxo::new(OutPoint::new("txp1", 0), "btc", "btc_primary_scope", 90_000))
		.unwrap();
	orch.prepare_operation(BtcSendRequest::new("op-persist", "btc", 50_000)).unwrap();
	drop(orch);

	let reopened = SqliteOrchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	assert_eq!(
		reopened.get_operation(&"op-persist".into()).unwrap().unwrap().phase,
		OperationPhase::Selected
	);
	assert_eq!(
		reopened.get_utxo(&OutPoint::new("txp1", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
}

#[test]
fn sqlite_store_cleanup_releases_selected_operation() {
	let mut orch = SqliteOrchestrator::with_store(base_store());
	orch.add_utxo(ManagedUtxo::new(OutPoint::new("txc1", 0), "btc", "btc_primary_scope", 90_000))
		.unwrap();

	orch.prepare_operation(BtcSendRequest::new("op-cleanup", "btc", 50_000)).unwrap();

	let report = orch.cleanup_stale_reservations(0).unwrap();
	assert_eq!(report.released_operations, vec![txoscope_core::OperationId::from("op-cleanup")]);
	assert_eq!(report.released_outpoints, vec![OutPoint::new("txc1", 0)]);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txc1", 0)).unwrap().unwrap().availability,
		Availability::Selectable
	);
}

#[test]
fn sqlite_store_rejects_reusing_operation_id_for_different_selection() {
	let mut store = temp_store();
	initialize_base_store(&mut store);
	store
		.put_utxo(ManagedUtxo::new(OutPoint::new("txi1", 0), "btc", "btc_primary_scope", 60_000))
		.unwrap();
	store
		.put_utxo(ManagedUtxo::new(OutPoint::new("txi2", 0), "btc", "btc_primary_scope", 70_000))
		.unwrap();

	store
		.persist_intent(OperationSelection {
			operation_id: "op-reused".into(),
			selected_outpoints: vec![OutPoint::new("txi1", 0)],
			change_scope_id: "btc_primary_scope".into(),
			effects: vec![SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("txi1", 0),
				purpose: LockPurpose::BtcSend,
			}],
			expires_at: None,
		})
		.unwrap();

	let err = store
		.persist_intent(OperationSelection {
			operation_id: "op-reused".into(),
			selected_outpoints: vec![OutPoint::new("txi2", 0)],
			change_scope_id: "btc_primary_scope".into(),
			effects: vec![SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("txi2", 0),
				purpose: LockPurpose::BtcSend,
			}],
			expires_at: None,
		})
		.unwrap_err();

	assert_eq!(err, DomainError::IntentConflict { operation_id: "op-reused".into() });
}

#[test]
fn sqlite_store_complete_operation_verifies_planned_inputs_and_allows_phase_advance() {
	let mut orch = SqliteOrchestrator::with_store(base_store());
	orch.add_utxo(ManagedUtxo::new(OutPoint::new("txdone", 0), "btc", "btc_primary_scope", 80_000))
		.unwrap();

	orch.prepare_operation(BtcSendRequest::new("op-complete", "btc", 50_000)).unwrap();

	let err = orch
		.complete_operation(
			&"op-complete".into(),
			OperationPhase::Signed,
			vec![OutPoint::new("wrong", 0)],
		)
		.unwrap_err();
	assert_eq!(err, DomainError::PlannedInputsMismatch { operation_id: "op-complete".into() });

	let completed = orch
		.complete_operation(
			&"op-complete".into(),
			OperationPhase::Signed,
			vec![OutPoint::new("txdone", 0)],
		)
		.unwrap();
	assert_eq!(completed, vec![OutPoint::new("txdone", 0)]);
	assert_eq!(
		orch.get_utxo(&OutPoint::new("txdone", 0)).unwrap().unwrap().availability,
		Availability::Reserved
	);
	assert_eq!(
		orch.get_operation(&"op-complete".into()).unwrap().unwrap().phase,
		OperationPhase::Signed
	);

	let advanced = orch
		.complete_operation(
			&"op-complete".into(),
			OperationPhase::Broadcast,
			Vec::<OutPoint>::new(),
		)
		.unwrap();
	assert_eq!(advanced, vec![OutPoint::new("txdone", 0)]);
	assert_eq!(
		orch.get_operation(&"op-complete".into()).unwrap().unwrap().phase,
		OperationPhase::Broadcast
	);
}

#[test]
fn sqlite_store_complete_operation_fails_on_corrupted_bound_row_without_partial_commit() {
	let (_dir, path) = persistent_store();
	let mut orch = SqliteOrchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	orch.add_account(Account::new(
		"btc",
		"BTC",
		vec![],
		CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
	))
	.unwrap();
	orch.add_scope(WalletScope::new(
		"btc_primary_scope",
		"btc",
		"wpkh(desc-btc)",
		"m/84'/0'/0'/0/*",
		CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
		10,
	))
	.unwrap();
	orch.add_utxo(ManagedUtxo::new(
		OutPoint::new("tx-corrupt", 0),
		"btc",
		"btc_primary_scope",
		80_000,
	))
	.unwrap();
	orch.prepare_operation(BtcSendRequest::new("op-corrupt", "btc", 50_000)).unwrap();
	drop(orch);

	let conn = Connection::open(&path).unwrap();
	conn.execute(
		"UPDATE utxos SET outpoint = CAST(X'FF' AS BLOB) WHERE txid = ?1 AND vout = ?2",
		params!["tx-corrupt", 0],
	)
	.unwrap();
	drop(conn);

	let mut orch = SqliteOrchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	let err = orch
		.complete_operation(
			&"op-corrupt".into(),
			OperationPhase::Signed,
			vec![OutPoint::new("tx-corrupt", 0)],
		)
		.unwrap_err();
	assert!(matches!(err, DomainError::CorruptedState(_)));

	let conn = Connection::open(&path).unwrap();
	let phase: String = conn
		.query_row(
			"SELECT phase FROM operations WHERE operation_id = ?1",
			params!["op-corrupt"],
			|row| row.get(0),
		)
		.unwrap();
	assert_eq!(phase, "selected");
	let (outpoint_type, availability, binding_operation_id, binding_phase): (
		String,
		String,
		Option<String>,
		Option<String>,
	) = conn
		.query_row(
			"SELECT typeof(outpoint), availability, binding_operation_id, binding_phase
             FROM utxos WHERE txid = ?1 AND vout = ?2",
			params!["tx-corrupt", 0],
			|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
		)
		.unwrap();
	assert_eq!(outpoint_type, "blob");
	assert_eq!(availability, "reserved");
	assert_eq!(binding_operation_id, Some("op-corrupt".to_string()));
	assert_eq!(binding_phase, Some("selected".to_string()));
}

#[test]
fn sqlite_store_get_intent_reports_corrupted_state_for_unknown_effect_kind() {
	let (_dir, path) = persistent_store();
	let mut orch = SqliteOrchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	orch.add_account(Account::new(
		"btc",
		"BTC",
		vec![],
		CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
	))
	.unwrap();
	orch.add_scope(WalletScope::new(
		"btc_primary_scope",
		"btc",
		"wpkh(desc-btc)",
		"m/84'/0'/0'/0/*",
		CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
		10,
	))
	.unwrap();
	orch.add_utxo(ManagedUtxo::new(
		OutPoint::new("tx-intent", 0),
		"btc",
		"btc_primary_scope",
		80_000,
	))
	.unwrap();
	orch.prepare_operation(BtcSendRequest::new("op-intent", "btc", 50_000)).unwrap();
	drop(orch);

	let conn = Connection::open(&path).unwrap();
	conn.execute(
		"UPDATE intent_effects SET effect_kind = 'bogus_effect' WHERE operation_id = ?1",
		params!["op-intent"],
	)
	.unwrap();
	drop(conn);

	let store = SqliteStateStore::open(&path).unwrap();
	let err = store.get_intent(&"op-intent".into()).unwrap_err();
	assert!(
		matches!(err, DomainError::CorruptedState(message) if message.contains("unknown intent effect kind"))
	);
}

#[test]
fn recovery_after_reopen_rejects_revoked_scope_capabilities() {
	use txoscope_core::backend::IntentStatus;
	let (_dir, path) = persistent_store();
	let mut store = SqliteStateStore::open(&path).unwrap();
	initialize_base_store(&mut store);
	store
		.put_utxo(ManagedUtxo::new(OutPoint::new("pending", 0), "btc", "btc_primary_scope", 50_000))
		.unwrap();
	let mut orch = Orchestrator::with_store(store);
	orch.prepare_pending_operation(BtcSendRequest::new("revoked", "btc", 10_000)).unwrap();
	orch.add_scope(WalletScope::new(
		"btc_primary_scope",
		"btc",
		"descriptor",
		"external",
		CapabilitySet::default(),
		0,
	))
	.unwrap();
	drop(orch);

	let mut orch = Orchestrator::with_store(SqliteStateStore::open(&path).unwrap());
	let report = orch.recover_pending_intents().unwrap();
	assert_eq!(report.failed_operations, vec![txoscope_core::OperationId::from("revoked")]);
	assert!(report.replayed_operations.is_empty());
	assert!(orch.get_lock(&OutPoint::new("pending", 0)).unwrap().is_none());
	assert_eq!(
		orch.get_utxo(&OutPoint::new("pending", 0)).unwrap().unwrap().availability,
		Availability::Selectable
	);
	assert!(orch.get_operation(&"revoked".into()).unwrap().is_none());
	assert_eq!(orch.get_intent(&"revoked".into()).unwrap().unwrap().status, IntentStatus::Failed);
	drop(orch);
	let store = SqliteStateStore::open(&path).unwrap();
	assert_eq!(store.get_intent(&"revoked".into()).unwrap().unwrap().status, IntentStatus::Failed);
	assert!(store.get_lock(&OutPoint::new("pending", 0)).unwrap().is_none());
}

#[test]
fn fee_support_intents_preserve_requirements_across_reopen() {
	use txoscope_core::backend::IntentStatus;
	for (revoke, applied) in [(false, false), (true, false), (false, true), (true, true)] {
		let (_dir, path) = persistent_store();
		let mut store = SqliteStateStore::open(&path).unwrap();
		store
			.put_account(Account::new(
				"asset",
				"Asset",
				vec![],
				CapabilitySet::new([Capability::ReserveSupport, Capability::FeeSupport]),
			))
			.unwrap();
		for (scope, caps) in [
			("carrier", vec![Capability::ReserveSupport]),
			("fee", vec![Capability::ReserveSupport, Capability::FeeSupport]),
		] {
			store
				.put_scope(WalletScope::new(
					scope,
					"asset",
					"descriptor",
					"external",
					CapabilitySet::new(caps),
					0,
				))
				.unwrap();
			store
				.put_utxo(ManagedUtxo::new(OutPoint::new(scope, 0), "asset", scope, 50_000))
				.unwrap();
		}
		let selection = OperationSelection {
			operation_id: "asset-plan".into(),
			selected_outpoints: vec![OutPoint::new("carrier", 0), OutPoint::new("fee", 0)],
			change_scope_id: "carrier".into(),
			effects: vec![
				SelectionEffect::LockUtxo {
					outpoint: OutPoint::new("carrier", 0),
					purpose: LockPurpose::PlannedSelection,
				},
				SelectionEffect::LockUtxo {
					outpoint: OutPoint::new("fee", 0),
					purpose: LockPurpose::FeeSupport,
				},
			],
			expires_at: None,
		};
		store.persist_intent(selection.clone()).unwrap();
		if applied {
			store
				.apply_batch(txoscope_core::backend::StateBatch {
					selection: selection.clone(),
					phase: OperationPhase::Selected,
					owner: "test".into(),
				})
				.unwrap();
		}
		if revoke {
			store
				.put_scope(WalletScope::new(
					"fee",
					"asset",
					"descriptor",
					"external",
					CapabilitySet::new([Capability::ReserveSupport]),
					0,
				))
				.unwrap();
		}
		drop(store);
		let store = SqliteStateStore::open(&path).unwrap();
		assert_eq!(store.get_intent(&"asset-plan".into()).unwrap().unwrap().selection, selection);
		let mut orch = Orchestrator::with_store(store);
		let report = orch.recover_pending_intents().unwrap();
		if revoke {
			assert_eq!(
				report.failed_operations,
				vec![txoscope_core::OperationId::from("asset-plan")]
			);
			assert!(orch.get_lock(&OutPoint::new("carrier", 0)).unwrap().is_none());
			assert!(orch.get_lock(&OutPoint::new("fee", 0)).unwrap().is_none());
			if applied {
				assert_eq!(
					orch.get_operation(&"asset-plan".into()).unwrap().unwrap().phase,
					OperationPhase::RollingBack
				);
			} else {
				assert!(orch.get_operation(&"asset-plan".into()).unwrap().is_none());
			}
			assert_eq!(
				orch.get_intent(&"asset-plan".into()).unwrap().unwrap().status,
				IntentStatus::Failed
			);
		} else {
			assert_eq!(
				report.replayed_operations,
				vec![txoscope_core::OperationId::from("asset-plan")]
			);
			assert_eq!(
				orch.get_lock(&OutPoint::new("fee", 0)).unwrap().unwrap().purpose,
				LockPurpose::FeeSupport
			);
		}
	}
}

#[test]
fn apply_batch_rechecks_permissions_after_another_connection_revokes_them() {
	use txoscope_core::backend::StateBatch;
	let (_dir, path) = persistent_store();
	let mut store = SqliteStateStore::open(&path).unwrap();
	initialize_base_store(&mut store);
	store
		.put_utxo(ManagedUtxo::new(OutPoint::new("tx", 0), "btc", "btc_primary_scope", 50_000))
		.unwrap();
	let selection = OperationSelection {
		operation_id: "revoked".into(),
		selected_outpoints: vec![OutPoint::new("tx", 0)],
		change_scope_id: "btc_primary_scope".into(),
		effects: vec![SelectionEffect::LockUtxo {
			outpoint: OutPoint::new("tx", 0),
			purpose: LockPurpose::BtcSend,
		}],
		expires_at: None,
	};
	selection
		.validate_capabilities(
			|id| store.get_account(id),
			|id| store.get_scope(id),
			|id| store.get_utxo(id),
		)
		.unwrap();
	let mut other = SqliteStateStore::open(&path).unwrap();
	other
		.put_scope(WalletScope::new(
			"btc_primary_scope",
			"btc",
			"descriptor",
			"external",
			CapabilitySet::default(),
			0,
		))
		.unwrap();
	let tick = store.current_tick().unwrap();
	assert_eq!(
		store.apply_batch(StateBatch {
			selection,
			phase: OperationPhase::Selected,
			owner: "test".into()
		}),
		Err(DomainError::MissingCapabilities { scope_id: "btc_primary_scope".into() })
	);
	assert_eq!(store.current_tick().unwrap(), tick);
	assert!(store.get_lock(&OutPoint::new("tx", 0)).unwrap().is_none());
	assert!(store.get_operation(&"revoked".into()).unwrap().is_none());
	assert_eq!(
		store.get_utxo(&OutPoint::new("tx", 0)).unwrap().unwrap().availability,
		Availability::Selectable
	);
}

#[test]
fn failed_intent_cleanup_is_atomic_and_terminal() {
	use txoscope_core::backend::{IntentStatus, StateBatch};
	let (_dir, path) = persistent_store();
	let mut store = SqliteStateStore::open(&path).unwrap();
	initialize_base_store(&mut store);
	store
		.put_utxo(ManagedUtxo::new(OutPoint::new("crash", 0), "btc", "btc_primary_scope", 50_000))
		.unwrap();
	let selection = OperationSelection {
		operation_id: "crash".into(),
		selected_outpoints: vec![OutPoint::new("crash", 0)],
		change_scope_id: "btc_primary_scope".into(),
		effects: vec![SelectionEffect::LockUtxo {
			outpoint: OutPoint::new("crash", 0),
			purpose: LockPurpose::BtcSend,
		}],
		expires_at: None,
	};
	store.persist_intent(selection.clone()).unwrap();
	let batch = StateBatch { selection, phase: OperationPhase::Selected, owner: "test".into() };
	store.apply_batch(batch.clone()).unwrap();
	let injector = Connection::open(&path).unwrap();
	injector.execute_batch("CREATE TRIGGER fail_intent_rejection BEFORE UPDATE OF status ON intents WHEN NEW.status = 'failed' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
	let tick = store.current_tick().unwrap();
	assert!(matches!(store.mark_intent_failed(&"crash".into()), Err(DomainError::Storage(_))));
	assert_eq!(store.current_tick().unwrap(), tick);
	assert!(store.get_lock(&OutPoint::new("crash", 0)).unwrap().is_some());
	assert_eq!(
		store.get_operation(&"crash".into()).unwrap().unwrap().phase,
		OperationPhase::Selected
	);
	assert_eq!(store.get_intent(&"crash".into()).unwrap().unwrap().status, IntentStatus::Pending);
	injector.execute_batch("DROP TRIGGER fail_intent_rejection;").unwrap();
	store.mark_intent_failed(&"crash".into()).unwrap();
	store.mark_intent_failed(&"crash".into()).unwrap();
	assert!(store.get_lock(&OutPoint::new("crash", 0)).unwrap().is_none());
	assert_eq!(
		store.get_operation(&"crash".into()).unwrap().unwrap().phase,
		OperationPhase::RollingBack
	);
	assert_eq!(store.get_intent(&"crash".into()).unwrap().unwrap().status, IntentStatus::Failed);
	assert_eq!(store.apply_batch(batch), Err(DomainError::IntentNotPending("crash".into())));
	assert_eq!(
		store.mark_intent_committed(&"crash".into()),
		Err(DomainError::IntentNotPending("crash".into()))
	);
}

#[test]
fn storage_failure_keeps_pending_intent_retryable_and_preserves_existing_locks() {
	use txoscope_core::backend::{IntentStatus, StateBatch};
	for applied in [false, true] {
		for recover in [false, true] {
			let (_dir, path) = persistent_store();
			let mut store = SqliteStateStore::open(&path).unwrap();
			initialize_base_store(&mut store);
			store
				.put_utxo(ManagedUtxo::new(
					OutPoint::new("retry", 0),
					"btc",
					"btc_primary_scope",
					50_000,
				))
				.unwrap();
			let selection = OperationSelection {
				operation_id: "retry".into(),
				selected_outpoints: vec![OutPoint::new("retry", 0)],
				change_scope_id: "btc_primary_scope".into(),
				effects: vec![SelectionEffect::LockUtxo {
					outpoint: OutPoint::new("retry", 0),
					purpose: LockPurpose::BtcSend,
				}],
				expires_at: None,
			};
			store.persist_intent(selection.clone()).unwrap();
			if applied {
				store
					.apply_batch(StateBatch {
						selection,
						phase: OperationPhase::Selected,
						owner: "original".into(),
					})
					.unwrap();
			}
			let injector = Connection::open(&path).unwrap();
			injector.execute_batch("CREATE TRIGGER fail_batch BEFORE INSERT ON operations BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();
			let mut orch = Orchestrator::with_store(store);
			let result = if recover {
				orch.recover_pending_intents().map(|_| ())
			} else {
				orch.commit_recorded_intent(&"retry".into())
			};
			assert!(matches!(result, Err(DomainError::Storage(_))));
			assert_eq!(
				orch.get_intent(&"retry".into()).unwrap().unwrap().status,
				IntentStatus::Pending
			);
			assert_eq!(orch.get_lock(&OutPoint::new("retry", 0)).unwrap().is_some(), applied);
			injector.execute_batch("DROP TRIGGER fail_batch;").unwrap();
			orch.commit_recorded_intent(&"retry".into()).unwrap();
			assert_eq!(
				orch.get_intent(&"retry".into()).unwrap().unwrap().status,
				IntentStatus::Committed
			);
			if applied {
				assert_eq!(
					orch.get_lock(&OutPoint::new("retry", 0)).unwrap().unwrap().owner,
					"original"
				);
			}
		}
	}
}

#[test]
fn batch_replay_rejects_rollback_and_lock_identity_changes() {
	use txoscope_core::backend::StateBatch;
	for mode in 0..3 {
		let mut store = base_store();
		let caps = CapabilitySet::new([Capability::ReserveSupport, Capability::FeeSupport]);
		store.put_account(Account::new("btc", "BTC", vec![], caps.clone())).unwrap();
		store
			.put_scope(WalletScope::new(
				"btc_primary_scope",
				"btc",
				"descriptor",
				"external",
				caps,
				0,
			))
			.unwrap();
		store
			.put_utxo(ManagedUtxo::new(
				OutPoint::new("replay", 0),
				"btc",
				"btc_primary_scope",
				50_000,
			))
			.unwrap();
		let selection = OperationSelection {
			operation_id: "replay".into(),
			selected_outpoints: vec![OutPoint::new("replay", 0)],
			change_scope_id: "btc_primary_scope".into(),
			effects: vec![SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("replay", 0),
				purpose: LockPurpose::FeeSupport,
			}],
			expires_at: Some(100),
		};
		let mut batch =
			StateBatch { selection, phase: OperationPhase::Selected, owner: "test".into() };
		store.apply_batch(batch.clone()).unwrap();
		match mode {
			0 => {
				store.release_operation(&"replay".into(), OperationPhase::RollingBack).unwrap();
			},
			1 => {
				batch.selection.effects = vec![SelectionEffect::LockUtxo {
					outpoint: OutPoint::new("replay", 0),
					purpose: LockPurpose::PlannedSelection,
				}];
				store
					.put_scope(WalletScope::new(
						"btc_primary_scope",
						"btc",
						"descriptor",
						"external",
						CapabilitySet::new([Capability::ReserveSupport]),
						0,
					))
					.unwrap();
			},
			_ => {
				batch.selection.expires_at = Some(200);
			},
		}
		let old_lock = store.get_lock(&OutPoint::new("replay", 0)).unwrap();
		let tick = store.current_tick().unwrap();
		let result = store.apply_batch(batch);
		if mode == 0 {
			assert!(matches!(result, Err(DomainError::InvalidOperationTransition { .. })));
		} else {
			assert!(matches!(result, Err(DomainError::IntentConflict { .. })));
		}
		assert_eq!(store.get_lock(&OutPoint::new("replay", 0)).unwrap(), old_lock);
		assert_eq!(store.current_tick().unwrap(), tick);
	}
}

#[test]
fn recovery_of_competing_intents_does_not_depend_on_operation_id_order() {
	use txoscope_core::backend::{IntentStatus, StateBatch};
	for (plan_id, send_id) in [("a-plan", "b-send"), ("b-plan", "a-send")] {
		let (_dir, path) = persistent_store();
		let mut store = SqliteStateStore::open(&path).unwrap();
		let caps = CapabilitySet::new([Capability::BtcSpend, Capability::ReserveSupport]);
		store.put_account(Account::new("btc", "BTC", vec![], caps.clone())).unwrap();
		store
			.put_scope(WalletScope::new("scope", "btc", "descriptor", "external", caps, 0))
			.unwrap();
		store
			.put_utxo(ManagedUtxo::new(OutPoint::new("shared", 0), "btc", "scope", 50_000))
			.unwrap();
		let selection = |id: &str, purpose| OperationSelection {
			operation_id: id.into(),
			selected_outpoints: vec![OutPoint::new("shared", 0)],
			change_scope_id: "scope".into(),
			effects: vec![SelectionEffect::LockUtxo {
				outpoint: OutPoint::new("shared", 0),
				purpose,
			}],
			expires_at: None,
		};
		store.persist_intent(selection(plan_id, LockPurpose::PlannedSelection)).unwrap();
		let send = selection(send_id, LockPurpose::BtcSend);
		store.persist_intent(send.clone()).unwrap();
		store
			.apply_batch(StateBatch {
				selection: send,
				phase: OperationPhase::Selected,
				owner: "test".into(),
			})
			.unwrap();
		store
			.put_account(Account::new(
				"btc",
				"BTC",
				vec![],
				CapabilitySet::new([Capability::ReserveSupport]),
			))
			.unwrap();
		drop(store);
		let mut orch = Orchestrator::with_store(SqliteStateStore::open(&path).unwrap());
		let report = orch.recover_pending_intents().unwrap();
		assert_eq!(report.replayed_operations, vec![txoscope_core::OperationId::from(plan_id)]);
		assert_eq!(report.failed_operations, vec![txoscope_core::OperationId::from(send_id)]);
		assert_eq!(
			orch.get_intent(&plan_id.into()).unwrap().unwrap().status,
			IntentStatus::Committed
		);
		assert_eq!(
			orch.get_lock(&OutPoint::new("shared", 0)).unwrap().unwrap().operation_id,
			plan_id.into()
		);
		assert!(orch.pending_intents().unwrap().is_empty());
	}
}

#[test]
fn corrupt_outpoint_components_roll_back_all_inputs_during_lifecycle_writes() {
	for release in [false, true] {
		let (_directory, path) = persistent_store();
		let mut store = SqliteStateStore::open(&path).unwrap();
		initialize_base_store(&mut store);
		for txid in ["a", "z"] {
			store
				.put_utxo(ManagedUtxo::new(
					OutPoint::new(txid, 0),
					"btc",
					"btc_primary_scope",
					50_000,
				))
				.unwrap();
		}
		let mut orch = Orchestrator::with_store(store);
		orch.prepare_operation(BtcSendRequest::new("corrupt", "btc", 100_000)).unwrap();
		let before_operation = orch.get_operation(&"corrupt".into()).unwrap();
		let before_utxo = orch.get_utxo(&OutPoint::new("a", 0)).unwrap();
		let before_lock = orch.get_lock(&OutPoint::new("a", 0)).unwrap();
		let before_tick = orch.current_tick().unwrap();
		drop(orch);
		let conn = Connection::open(&path).unwrap();
		conn.execute("UPDATE utxos SET txid = 'different' WHERE outpoint = 'z:0'", []).unwrap();
		let mut store = SqliteStateStore::open(&path).unwrap();
		// SQL key order processes a:0 before encountering the inconsistent z:0 row.
		let result = if release {
			store.release_operation(&"corrupt".into(), OperationPhase::RollingBack)
		} else {
			store.complete_operation(&"corrupt".into(), OperationPhase::Signed)
		};
		assert!(matches!(result, Err(DomainError::CorruptedState(_))));
		assert_eq!(store.get_operation(&"corrupt".into()).unwrap(), before_operation);
		assert_eq!(store.get_utxo(&OutPoint::new("a", 0)).unwrap(), before_utxo);
		assert_eq!(store.get_lock(&OutPoint::new("a", 0)).unwrap(), before_lock);
		assert!(store.get_lock(&OutPoint::new("z", 0)).unwrap().is_some());
		assert_eq!(store.current_tick().unwrap(), before_tick);
		let corrupt_row: (String, String, String, String) = conn.query_row(
            "SELECT txid, availability, binding_operation_id, binding_phase FROM utxos WHERE outpoint = 'z:0'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();
		assert_eq!(
			corrupt_row,
			("different".into(), "reserved".into(), "corrupt".into(), "selected".into())
		);
	}
}
