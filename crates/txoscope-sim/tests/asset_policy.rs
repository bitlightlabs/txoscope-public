use txoscope_core::{AssetCandidate, AssetSelectionConstraints, AssetSelectionPolicy, DomainError};

fn candidate(id: &str, asset_amount: u64, value_sats: u64) -> AssetCandidate {
	AssetCandidate { outpoint: format!("{id}:0").parse().unwrap(), asset_amount, value_sats }
}

#[test]
fn automatic_selection_is_order_independent_and_respects_constraints() {
	// Exhaust every small candidate balance, target, and policy combination.
	for a in 0..=3 {
		for b in 0..=3 {
			for value in 0..=3 {
				let candidates =
					vec![candidate("a", a, value), candidate("b", b, 2), candidate("fee", 0, 3)];
				for target_asset in 1..=6 {
					for target_value in 0..=8 {
						for merge in [false, true] {
							for fee in [false, true] {
								let constraints = AssetSelectionConstraints {
									exact_outpoints: None,
									allow_asset_merge: merge,
									allow_fee_support: fee,
								};
								let result = AssetSelectionPolicy::select(
									&candidates,
									target_asset,
									target_value,
									&constraints,
								);
								for order in [[0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]]
								{
									let shuffled = order.map(|i| candidates[i].clone());
									assert_eq!(
										result,
										AssetSelectionPolicy::select(
											&shuffled,
											target_asset,
											target_value,
											&constraints
										)
									);
								}
								if let Ok(selected) = result {
									let inputs: Vec<_> = selected
										.iter()
										.map(|op| {
											candidates.iter().find(|c| c.outpoint == *op).unwrap()
										})
										.collect();
									assert!(
										inputs.iter().map(|c| c.asset_amount).sum::<u64>()
											>= target_asset
									);
									assert!(
										inputs.iter().map(|c| c.value_sats).sum::<u64>()
											>= target_value
									);
									assert!(
										merge
											|| inputs.iter().filter(|c| c.asset_amount > 0).count()
												== 1
									);
									assert!(fee || inputs.iter().all(|c| c.asset_amount > 0));
									assert_eq!(
										selected
											.iter()
											.collect::<std::collections::BTreeSet<_>>()
											.len(),
										selected.len()
									);
								}
							}
						}
					}
				}
			}
		}
	}
}

#[test]
fn exact_selection_obeys_fee_policy_and_preserves_order() {
	let candidates = vec![candidate("asset", 5, 1), candidate("fee", 0, 10)];
	let exact = vec![candidates[1].outpoint.clone(), candidates[0].outpoint.clone()];
	let mut constraints = AssetSelectionConstraints {
		exact_outpoints: Some(exact.clone()),
		allow_asset_merge: false,
		allow_fee_support: true,
	};
	assert_eq!(AssetSelectionPolicy::select(&candidates, 5, 11, &constraints).unwrap(), exact);
	constraints.allow_fee_support = false;
	assert!(matches!(
		AssetSelectionPolicy::select(&candidates, 5, 11, &constraints),
		Err(DomainError::UtxoPolicyBlocked { .. })
	));
	constraints.exact_outpoints = Some(vec!["missing:0".parse().unwrap()]);
	assert!(matches!(
		AssetSelectionPolicy::select(&candidates, 5, 0, &constraints),
		Err(DomainError::UtxoPolicyBlocked { .. })
	));
}

#[test]
fn validates_inputs_and_uses_wide_totals() {
	let candidates = vec![candidate("a", u64::MAX, u64::MAX), candidate("b", u64::MAX, u64::MAX)];
	let mut constraints = AssetSelectionConstraints {
		exact_outpoints: Some(candidates.iter().map(|c| c.outpoint.clone()).collect()),
		allow_asset_merge: false,
		allow_fee_support: false,
	};
	assert_eq!(
		AssetSelectionPolicy::select(&candidates, u64::MAX, u64::MAX, &constraints).unwrap().len(),
		2
	);
	assert!(matches!(
		AssetSelectionPolicy::select(
			&[candidates[0].clone(), candidates[0].clone()],
			1,
			0,
			&constraints
		),
		Err(DomainError::InvalidRequest { .. })
	));
	constraints.exact_outpoints = Some(vec![candidates[0].outpoint.clone(); 2]);
	assert!(matches!(
		AssetSelectionPolicy::select(&candidates, 1, 0, &constraints),
		Err(DomainError::InvalidRequest { .. })
	));
	constraints.exact_outpoints = Some(vec![]);
	assert!(matches!(
		AssetSelectionPolicy::select(&candidates, 1, 0, &constraints),
		Err(DomainError::InvalidRequest { .. })
	));
	constraints.exact_outpoints = None;
	assert_eq!(
		AssetSelectionPolicy::select(&[], 1, 0, &constraints),
		Err(DomainError::InsufficientAsset { needed: 1, have: 0 })
	);
	assert_eq!(
		AssetSelectionPolicy::select(&candidates, 0, 0, &constraints),
		Err(DomainError::InsufficientFunds)
	);
	assert_eq!(
		AssetSelectionPolicy::select(&[candidate("a", 2, 1)], 2, 2, &constraints),
		Err(DomainError::InsufficientValue { needed: 2, have: 1 })
	);
}

#[test]
fn selects_large_carriers_then_fee_support_with_lexical_ties() {
	let candidates = vec![candidate("b", 3, 2), candidate("a", 2, 2), candidate("fee", 0, 10)];
	let mut constraints = AssetSelectionConstraints {
		exact_outpoints: None,
		allow_asset_merge: true,
		allow_fee_support: true,
	};
	assert_eq!(
		AssetSelectionPolicy::select(&candidates, 5, 8, &constraints).unwrap(),
		vec![
			candidates[1].outpoint.clone(),
			candidates[0].outpoint.clone(),
			candidates[2].outpoint.clone()
		]
	);
	constraints.allow_fee_support = false;
	assert_eq!(
		AssetSelectionPolicy::select(&candidates, 5, 8, &constraints),
		Err(DomainError::InsufficientValue { needed: 8, have: 4 })
	);
	constraints.allow_asset_merge = false;
	assert_eq!(
		AssetSelectionPolicy::select(&candidates, 5, 0, &constraints),
		Err(DomainError::InsufficientAsset { needed: 5, have: 0 })
	);
	assert_eq!(
		AssetSelectionPolicy::select(&candidates, 3, 2, &constraints).unwrap(),
		vec![candidates[0].outpoint.clone()]
	);
}
