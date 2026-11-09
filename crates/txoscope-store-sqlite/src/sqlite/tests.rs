// RATIONALE: Rust tests and SQLite trace callbacks require free functions.
// Replay connection setup and temporary-file ownership live in the private helper module.

use super::*;
use std::{
	cell::RefCell,
	sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
	thread,
	time::Duration,
};
use tempfile::TempDir;
use txoscope_core::{
	backend::{LockPurpose, Orchestrator, StateStore},
	Capability, CapabilitySet,
};

mod helper;

const WAIT: Duration = Duration::from_secs(10);

// SQLite's trace callback runs immediately before executing BEGIN. In the old
// implementation this is after the unprotected lookup; with IMMEDIATE it is
// before any lookup. Pause only this thread/connection, without production hooks.
thread_local! {
	static BEGIN_GATE: RefCell<Option<(Sender<()>, Receiver<()>)>> = const { RefCell::new(None) };
	static BEGIN_RESUMED: RefCell<Option<Result<(), RecvTimeoutError>>> = const { RefCell::new(None) };
}

fn pause_before_begin(sql: &str) {
	if sql.starts_with("BEGIN") {
		BEGIN_GATE.with(|gate| {
			if let Some((reached, resume)) = gate.borrow_mut().take() {
				let _ = reached.send(());
				let result = resume.recv_timeout(WAIT);
				BEGIN_RESUMED.with(|resumed| *resumed.borrow_mut() = Some(result));
			}
		});
	}
}

fn selection(txid: &str) -> OperationSelection {
	let outpoint = OutPoint::new(txid, 0);
	OperationSelection {
		operation_id: "concurrent-intent".into(),
		selected_outpoints: vec![outpoint.clone()],
		change_scope_id: "scope".into(),
		effects: vec![SelectionEffect::LockUtxo { outpoint, purpose: LockPurpose::BtcSend }],
		expires_at: Some(100),
	}
}

fn competing_persist(
	winner_selection: OperationSelection, contender_selection: OperationSelection,
	status: IntentStatus,
) -> (TempDir, SqliteStateStore, OperationIntent, DomainResult<OperationIntent>) {
	let dir = TempDir::new().unwrap();
	let path = dir.path().join("intents.sqlite");
	let mut winner = SqliteStateStore::open(&path).unwrap();
	winner
		.put_account(Account::new(
			"btc",
			"BTC",
			vec![],
			CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
		))
		.unwrap();
	winner
		.put_scope(WalletScope::new(
			"scope",
			"btc",
			"descriptor",
			"derivation",
			CapabilitySet::new([Capability::BtcSpend, Capability::BtcReceive]),
			10,
		))
		.unwrap();
	for txid in ["winner", "contender"] {
		winner.put_utxo(ManagedUtxo::new(OutPoint::new(txid, 0), "btc", "scope", 60_000)).unwrap();
	}
	let mut contender = SqliteStateStore::open(&path).unwrap();
	let (reached_tx, reached_rx) = mpsc::channel();
	let (resume_tx, resume_rx) = mpsc::channel();
	let result = thread::scope(|scope| {
		let worker = scope.spawn(move || {
			BEGIN_GATE.with(|gate| *gate.borrow_mut() = Some((reached_tx, resume_rx)));
			contender.conn.trace(Some(pause_before_begin));
			let result = contender.persist_intent(contender_selection);
			// Assert outside SQLite's C callback, where a panic can propagate normally.
			BEGIN_RESUMED.with(|resumed| {
				resumed
					.borrow_mut()
					.take()
					.expect("contender must pause at BEGIN")
					.expect("contender must resume only after the winner records its intent");
			});
			result
		});
		reached_rx.recv_timeout(WAIT).expect("contender must reach BEGIN");
		winner.persist_intent(winner_selection.clone()).unwrap();
		match status {
			IntentStatus::Pending => {},
			IntentStatus::Committed => {
				winner.mark_intent_committed(&winner_selection.operation_id).unwrap()
			},
			IntentStatus::Failed => {
				winner.mark_intent_failed(&winner_selection.operation_id).unwrap()
			},
		}
		let recorded = winner.get_intent(&winner_selection.operation_id).unwrap().unwrap();
		let tick = winner.current_tick().unwrap();
		resume_tx.send(()).unwrap();
		let result = worker.join().unwrap();
		assert_eq!(winner.current_tick().unwrap(), tick, "retry must not advance tick");
		assert_eq!(
			winner.get_intent(&winner_selection.operation_id).unwrap(),
			Some(recorded.clone())
		);
		(recorded, result)
	});
	(dir, winner, result.0, result.1)
}

#[test]
fn concurrent_different_selection_conflicts_and_commits_only_winner_inputs() {
	for status in [IntentStatus::Pending, IntentStatus::Committed, IntentStatus::Failed] {
		let expected = selection("winner");
		let (_dir, store, recorded, result) =
			competing_persist(expected.clone(), selection("contender"), status);
		assert_eq!(
			result,
			Err(DomainError::IntentConflict { operation_id: expected.operation_id.clone() })
		);
		assert_eq!(recorded.selection, expected);
		assert_eq!(recorded.status, status);
		if status == IntentStatus::Pending {
			let mut orchestrator = Orchestrator::with_store(store);
			orchestrator.commit_recorded_intent(&expected.operation_id).unwrap();
			assert_eq!(
				orchestrator.get_lock(&OutPoint::new("winner", 0)).unwrap().unwrap().operation_id,
				expected.operation_id
			);
			assert!(orchestrator.get_lock(&OutPoint::new("contender", 0)).unwrap().is_none());
			assert_eq!(
				orchestrator.get_utxo(&OutPoint::new("winner", 0)).unwrap().unwrap().availability,
				Availability::Reserved
			);
			assert_eq!(
				orchestrator
					.get_utxo(&OutPoint::new("contender", 0))
					.unwrap()
					.unwrap()
					.availability,
				Availability::Selectable
			);
		}
	}
}

#[test]
fn concurrent_identical_selection_preserves_intent_and_tick_in_every_status() {
	for status in [IntentStatus::Pending, IntentStatus::Committed, IntentStatus::Failed] {
		let expected = selection("winner");
		let (_dir, _store, recorded, result) =
			competing_persist(expected.clone(), expected, status);
		assert_eq!(result.unwrap(), recorded);
		assert_eq!(recorded.status, status);
	}
}

#[test]
fn committed_replay_checks_release_from_another_connection_after_reading_intent() {
	let fixture = helper::ReplayFixture::new();
	let mut writer = fixture.writer;
	let mut replay = fixture.replay;
	let operation_id = fixture.operation_id;

	let (reached_tx, reached_rx) = mpsc::channel();
	let (resume_tx, resume_rx) = mpsc::channel();

	thread::scope(|scope| {
		let worker = scope.spawn(|| {
			BEGIN_GATE.with(|gate| *gate.borrow_mut() = Some((reached_tx, resume_rx)));
			replay.conn.trace(Some(pause_before_begin));
			let result = Orchestrator::with_store(replay).commit_recorded_intent(&operation_id);
			BEGIN_RESUMED.with(|resumed| {
				resumed
					.borrow_mut()
					.take()
					.expect(
						"missing BEGIN callback prevents verification of the release interleaving",
					)
					.expect("replay synchronization failed before validation could resume");
			});
			result
		});

		reached_rx
			.recv_timeout(WAIT)
			.expect("replay failed to reach BEGIN, so concurrent release cannot be tested");
		writer.release_operation(&operation_id, OperationPhase::RollingBack).unwrap();
		let released = writer.get_operation(&operation_id).unwrap();
		let tick = writer.current_tick().unwrap();
		resume_tx.send(()).unwrap();

		assert_eq!(
			worker.join().unwrap(),
			Err(DomainError::InvalidOperationTransition {
				operation_id: operation_id.clone(),
				from: OperationPhase::RollingBack,
				to: OperationPhase::Selected,
			})
		);
		assert_eq!(writer.get_operation(&operation_id).unwrap(), released);
		assert_eq!(writer.current_tick().unwrap(), tick);
		assert!(writer.get_lock(&OutPoint::new("input", 0)).unwrap().is_none());
	});
}
