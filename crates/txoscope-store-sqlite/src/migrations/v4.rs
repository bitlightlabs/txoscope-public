//! Frozen v3 -> v4 data vocabulary. Do not decode historical rows with current
//! domain enums. The caller owns the transaction and schema-version update.
use rusqlite::{params, types::ValueRef, Transaction};

use crate::codec::SqliteCodec;

use super::Migration;

pub(super) struct V4;

impl V4 {
	fn invalid(&self, message: impl Into<String>) -> rusqlite::Error {
		SqliteCodec::corrupted_state_error(format!("sqlite v3 -> v4: {}", message.into()))
	}

	fn reject(&self, tx: &Transaction<'_>, table: &str, predicate: &str) -> rusqlite::Result<()> {
		if tx.prepare(&format!("SELECT 1 FROM {table} WHERE {predicate} LIMIT 1"))?.exists([])? {
			return Err(self.invalid(format!("invalid {table} row: {predicate}")));
		}
		Ok(())
	}

	fn validate_types(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		// Validate actual SQLite storage classes (including nullable pairs), not
		// affinities or coercible values. Never CAST potentially corrupt data.
		for (table, text, optional_text, integers, optional_integers) in [
			("meta", "key,value", "", "", ""),
			("accounts", "account_id,name,capabilities", "", "", ""),
			(
				"scopes",
				"scope_id,account_id,descriptor_ref,derivation_scope,capabilities",
				"",
				"priority",
				"",
			),
			(
				"utxos",
				"outpoint,txid,account_id,scope_id,onchain_state,availability,role",
				"binding_operation_id,binding_phase,conflict_message",
				"vout,value_sats",
				"conflict_recorded_at",
			),
			(
				"locks",
				"outpoint,operation_id,account_id,scope_id,purpose,owner",
				"",
				"created_at",
				"expires_at",
			),
			("operations", "operation_id,phase", "", "updated_at", "expires_at"),
			("intents", "operation_id,change_scope_id,status", "", "recorded_at", "expires_at"),
			("intent_inputs", "operation_id,outpoint", "", "position", ""),
			("intent_effects", "operation_id,effect_kind,outpoint,purpose", "", "position", ""),
		] {
			for (columns, is_text, nullable) in [
				(text, true, false),
				(optional_text, true, true),
				(integers, false, false),
				(optional_integers, false, true),
			] {
				for column in columns.split(',') {
					if column.is_empty() {
						continue;
					}
					let mut stmt = tx.prepare(&format!("SELECT {column} FROM {table}"))?;
					let mut rows = stmt.query([])?;
					while let Some(row) = rows.next()? {
						let valid = match row.get_ref(0)? {
							ValueRef::Null => nullable,
							ValueRef::Text(bytes) => is_text && std::str::from_utf8(bytes).is_ok(),
							ValueRef::Integer(n) => !is_text && n >= 0,
							_ => false,
						};
						if !valid {
							return Err(
								self.invalid(format!("invalid type/value in {table}.{column}"))
							);
						}
					}
				}
			}
		}
		self.reject(tx, "utxos", "vout > 4294967295")?;
		self.reject(tx, "scopes", "priority > 4294967295")?;
		let tick: String =
			tx.query_row("SELECT value FROM meta WHERE key = 'tick'", [], |r| r.get(0))?;
		if tick.parse::<u64>().is_err() {
			return Err(self.invalid("invalid tick"));
		}
		Ok(())
	}

	fn validate_vocabulary(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		const PHASES: &str = "'selected','constructing','signed','broadcast','awaiting_confirmation','awaiting_l2_finality','rolling_back'";
		const PURPOSES: &str = "'manual_reservation','btc_send','fee_support','planned_selection','single_target_reserve','anchor_with_fee_support','rgb_receive_preparation','rgb_l1_send','channel_funding'";
		for (table, column, values) in [
			("operations", "phase", PHASES),
			("utxos", "binding_phase", PHASES),
			("locks", "purpose", PURPOSES),
			("intent_effects", "purpose", PURPOSES),
			("utxos", "onchain_state", "'unknown','unconfirmed_incoming','confirmed','spending_in_flight','spent'"),
			("utxos", "availability", "'unknown','selectable','reserved','locked','blocked_by_policy','blocked_by_l2','recovering'"),
			("utxos", "role", "'unknown','general','spend_preferred','reserve','recovery_output','consolidation_candidate'"),
			("intents", "status", "'pending','committed','failed'"),
			("intent_effects", "effect_kind", "'lock_utxo'"),
		] {
			self.reject(tx, table, &format!("{column} NOT IN ({values})"))?;
		}
		for table in ["utxos", "locks", "intent_inputs", "intent_effects"] {
			let mut stmt = tx.prepare(&format!("SELECT outpoint FROM {table}"))?;
			let mut rows = stmt.query([])?;
			while let Some(row) = rows.next()? {
				let encoded: String = row.get(0)?;
				let valid = match encoded.rsplit_once(':') {
					Some((_, index)) => match index.parse::<u32>() {
						Ok(n) => n.to_string() == index,
						Err(_) => false,
					},
					None => false,
				};
				if !valid {
					return Err(self.invalid(format!("noncanonical outpoint in {table}")));
				}
			}
		}
		self.reject(tx, "utxos", "outpoint != txid || ':' || vout")?;
		Ok(())
	}

	fn migrate_capabilities(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		for (table, id) in [("accounts", "account_id"), ("scopes", "scope_id")] {
			let mut stmt = tx.prepare(&format!("SELECT {id}, capabilities FROM {table}"))?;
			let mut records = Vec::new();
			let mut rows = stmt.query([])?;
			while let Some(row) = rows.next()? {
				records.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
			}
			drop(rows);
			drop(stmt);
			for (key, encoded) in records {
				let mut capabilities = Vec::new();
				if !encoded.is_empty() {
					for value in encoded.split(',') {
						let value = match value {
							"l2_settlement" => continue,
							"rgb_receive" => "single_target_reserve",
							"btc_spend"
							| "btc_receive"
							| "fee_support"
							| "reserve_support"
							| "single_target_reserve"
							| "consolidation_source"
							| "consolidation_target" => value,
							_ => {
								return Err(
									self.invalid(format!("unknown capability in {table}: {value}"))
								)
							},
						};
						if !capabilities.contains(&value) {
							capabilities.push(value);
						}
					}
				}
				tx.execute(
					&format!("UPDATE {table} SET capabilities = ?1 WHERE {id} = ?2"),
					params![capabilities.join(","), key],
				)?;
			}
		}
		Ok(())
	}

	fn validate_relationships(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		self.validate_scope_ownership(tx)?;
		self.validate_bindings_and_locks(tx)?;
		self.validate_active_selections(tx)?;
		self.validate_spending_state(tx)
	}

	fn validate_scope_ownership(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		self.reject(
			tx,
			"scopes s",
			"NOT EXISTS (SELECT 1 FROM accounts a WHERE a.account_id = s.account_id)",
		)?;
		self.reject(
			tx,
			"utxos u",
			"NOT EXISTS (SELECT 1 FROM scopes s WHERE s.scope_id = u.scope_id AND s.account_id
			= u.account_id)",
		)?;
		Ok(())
	}

	fn validate_bindings_and_locks(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		self.reject(
			tx,
			"utxos",
			"(binding_operation_id IS NULL) != (binding_phase IS NULL) OR (conflict_message IS
			NULL) != (conflict_recorded_at IS NULL)",
		)?;
		self.reject(
			tx,
			"utxos u",
			"binding_operation_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM operations o WHERE
			o.operation_id = u.binding_operation_id AND o.phase = u.binding_phase AND o.phase
			!= 'rolling_back')",
		)?;
		self.reject(
			tx,
			"utxos u",
			"binding_operation_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM locks l WHERE
			l.outpoint = u.outpoint AND l.operation_id = u.binding_operation_id)",
		)?;
		self.reject(
			tx,
			"locks l",
			"NOT EXISTS (SELECT 1 FROM utxos u WHERE u.outpoint = l.outpoint AND
			u.binding_operation_id = l.operation_id AND u.account_id = l.account_id AND
			u.scope_id = l.scope_id)",
		)?;
		self.reject(
			tx,
			"locks l",
			"EXISTS (SELECT 1 FROM intents i WHERE i.operation_id = l.operation_id) AND NOT
			EXISTS (SELECT 1 FROM intent_effects e WHERE e.operation_id = l.operation_id AND
			e.outpoint = l.outpoint AND e.purpose = l.purpose)",
		)?;
		Ok(())
	}

	fn validate_active_selections(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		// Pending/failed requests can contain invalid selections. Only active
		// operations require matching scopes, inputs and effects; orphan child rows
		// are always corrupt, independently of selection validation.
		for table in ["intent_inputs", "intent_effects"] {
			self.reject(
				tx,
				&format!("{table} x"),
				"NOT EXISTS (SELECT 1 FROM intents i WHERE i.operation_id = x.operation_id)",
			)?;
		}
		self.reject(
			tx,
			"intents i",
			"EXISTS (SELECT 1 FROM operations o WHERE o.operation_id = i.operation_id AND
			o.phase != 'rolling_back') AND (NOT EXISTS (SELECT 1 FROM scopes s WHERE
			s.scope_id = i.change_scope_id))",
		)?;
		self.reject(
			tx,
			"intent_inputs x",
			"EXISTS (SELECT 1 FROM operations o WHERE o.operation_id = x.operation_id AND
			o.phase != 'rolling_back') AND (NOT EXISTS (SELECT 1 FROM intents i WHERE
			i.operation_id = x.operation_id) OR NOT EXISTS (SELECT 1 FROM utxos u WHERE
			u.outpoint = x.outpoint))",
		)?;
		self.reject(
			tx,
			"intent_effects x",
			"EXISTS (SELECT 1 FROM operations o WHERE o.operation_id = x.operation_id AND
			o.phase != 'rolling_back') AND (NOT EXISTS (SELECT 1 FROM intent_inputs i WHERE
			i.operation_id = x.operation_id AND i.outpoint = x.outpoint))",
		)?;
		self.reject(
			tx,
			"intent_inputs x",
			"EXISTS (SELECT 1 FROM operations o WHERE o.operation_id = x.operation_id AND
			o.phase != 'rolling_back') AND (NOT EXISTS (SELECT 1 FROM intent_effects e WHERE
			e.operation_id = x.operation_id AND e.outpoint = x.outpoint))",
		)?;
		self.reject(
			tx,
			"intents i",
			"EXISTS (SELECT 1 FROM operations o WHERE o.operation_id = i.operation_id AND
			o.phase != 'rolling_back') AND (NOT EXISTS (SELECT 1 FROM intent_inputs x WHERE
			x.operation_id = i.operation_id))",
		)?;
		self.reject(
			tx,
			"operations o",
			"phase IN ('constructing','signed','broadcast','awaiting_confirmation') AND EXISTS
			(SELECT 1 FROM intents i WHERE i.operation_id = o.operation_id AND i.status !=
			'committed')",
		)?;
		for table in ["intent_inputs", "intent_effects"] {
			let sql = format!(
				"SELECT x.operation_id FROM {table} x
				 WHERE EXISTS (SELECT 1 FROM operations o
				               WHERE o.operation_id = x.operation_id AND o.phase != 'rolling_back')
				 GROUP BY x.operation_id
				 HAVING min(position) != 0 OR max(position) != count(*) - 1
				        OR count(DISTINCT outpoint) != count(*)"
			);
			if tx.prepare(&sql)?.exists([])? {
				return Err(
					self.invalid(format!("invalid positions or duplicate outpoints in {table}"))
				);
			}
		}
		Ok(())
	}

	fn validate_spending_state(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		// Late phases require chain evidence, even after mark_spent removes
		// bindings and locks. A phase alone must never supply missing chain facts.
		self.reject(
			tx,
			"utxos",
			"binding_phase IN ('broadcast','awaiting_confirmation') AND onchain_state NOT IN
			('spending_in_flight','spent')",
		)?;
		self.reject(
			tx,
			"operations o",
			"phase IN ('broadcast','awaiting_confirmation') AND (EXISTS (SELECT 1 FROM
			intent_inputs i JOIN utxos u ON u.outpoint = i.outpoint WHERE i.operation_id =
			o.operation_id AND u.onchain_state NOT IN ('spending_in_flight','spent')) OR (NOT
			EXISTS (SELECT 1 FROM intent_inputs i WHERE i.operation_id = o.operation_id) AND
			NOT EXISTS (SELECT 1 FROM utxos u WHERE u.binding_operation_id = o.operation_id)))",
		)?;
		Ok(())
	}

	fn validate_original_state(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		// Check before coalescing phases so a mismatched historical pair is not hidden.
		self.reject(
			tx,
			"utxos u",
			"binding_operation_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM operations o WHERE
			o.operation_id = u.binding_operation_id AND o.phase = u.binding_phase)",
		)?;
		self.reject(
			tx,
			"utxos",
			"binding_phase IN ('broadcast','awaiting_confirmation','awaiting_l2_finality') AND
			availability NOT IN ('blocked_by_l2','recovering')",
		)?;
		// A historical policy label is not evidence that a transaction was sent.
		self.reject(
			tx,
			"utxos",
			"availability = 'blocked_by_l2' AND onchain_state NOT IN
			('spent','spending_in_flight')",
		)?;
		Ok(())
	}

	fn migrate_status_and_purposes(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		tx.execute_batch("UPDATE operations SET phase = 'awaiting_confirmation' WHERE phase = 'awaiting_l2_finality';
		UPDATE utxos SET binding_phase = 'awaiting_confirmation' WHERE binding_phase = 'awaiting_l2_finality';
		UPDATE utxos SET availability = 'blocked_by_spend' WHERE availability = 'blocked_by_l2';")?;
		for table in ["locks", "intent_effects"] {
			tx.execute(&format!("UPDATE {table} SET purpose = 'planned_selection' WHERE purpose IN ('single_target_reserve','anchor_with_fee_support','rgb_receive_preparation','rgb_l1_send','channel_funding')"), [])?;
		}
		Ok(())
	}
}

impl Migration for V4 {
	fn version(&self) -> i64 {
		4
	}

	fn name(&self) -> &'static str {
		"protocol-neutral vocabulary"
	}

	fn upgrade(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
		self.validate_types(tx)?;
		self.validate_vocabulary(tx)?;
		self.validate_original_state(tx)?;
		self.migrate_capabilities(tx)?;
		self.migrate_status_and_purposes(tx)?;
		self.validate_relationships(tx)
	}
}
