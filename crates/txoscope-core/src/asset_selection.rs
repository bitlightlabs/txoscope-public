//! Pure asset selection policy over eligible, caller-supplied facts.
use crate::{DomainError, DomainResult, OutPoint};
use std::collections::BTreeSet;

/// An input whose ownership, capabilities and availability were checked by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetCandidate {
	pub outpoint: OutPoint,
	/// Zero denotes an allocation-free fee-support input.
	pub asset_amount: u64,
	pub value_sats: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetSelectionConstraints {
	pub exact_outpoints: Option<Vec<OutPoint>>,
	pub allow_asset_merge: bool,
	/// Allows allocation-free inputs; their account and scope must grant `FeeSupport`.
	pub allow_fee_support: bool,
}

/// Deterministic selection without stores, clocks, reservations or side effects.
///
/// Automatic selection prefers larger bitcoin carriers, then lexical outpoints.
/// Exact selection preserves caller order and overrides automatic merge preference.
pub struct AssetSelectionPolicy;

impl AssetSelectionPolicy {
	/// Validate caller inputs before querying eligibility or changing expired reservations.
	/// An empty eligible set is valid input and selection reports insufficient assets.
	pub fn validate_inputs<'a>(
		outpoints: impl IntoIterator<Item = &'a OutPoint>, target_asset: u64,
		constraints: &AssetSelectionConstraints,
	) -> DomainResult<()> {
		if target_asset == 0 {
			return Err(DomainError::InsufficientFunds);
		}
		let mut seen = BTreeSet::new();
		for outpoint in outpoints {
			if !seen.insert(outpoint.clone()) {
				return Err(DomainError::InvalidRequest {
					reason: "duplicate asset candidate".into(),
				});
			}
		}
		if let Some(outpoints) = &constraints.exact_outpoints {
			if outpoints.is_empty() {
				return Err(DomainError::InvalidRequest {
					reason: "exact asset selection requires at least one outpoint".into(),
				});
			}
			let mut seen = BTreeSet::new();
			for outpoint in outpoints {
				if !seen.insert(outpoint) {
					return Err(DomainError::InvalidRequest {
						reason: "duplicate exact input".into(),
					});
				}
			}
		}
		Ok(())
	}

	/// Select eligible inputs, enforcing exact-input and automatic-selection constraints.
	pub fn select(
		candidates: &[AssetCandidate], target_asset: u64, target_value: u64,
		constraints: &AssetSelectionConstraints,
	) -> DomainResult<Vec<OutPoint>> {
		Self::validate_inputs(
			candidates.iter().map(|candidate| &candidate.outpoint),
			target_asset,
			constraints,
		)?;
		let mut eligible: Vec<_> = candidates.iter().collect();
		let selected = match &constraints.exact_outpoints {
			Some(outpoints) => {
				for outpoint in outpoints {
					if !eligible.iter().any(|c| &c.outpoint == outpoint) {
						return Err(DomainError::UtxoPolicyBlocked {
							outpoint: outpoint.clone(),
							reason: "input is not an available asset or fee-support candidate"
								.into(),
						});
					}
				}
				for candidate in &eligible {
					if outpoints.contains(&candidate.outpoint)
						&& candidate.asset_amount == 0
						&& !constraints.allow_fee_support
					{
						return Err(DomainError::UtxoPolicyBlocked {
							outpoint: candidate.outpoint.clone(),
							reason: "fee-support inputs are disabled".into(),
						});
					}
				}
				outpoints.clone()
			},
			None => {
				// Prefer large carriers, with a stable outpoint tie-break, to avoid dust anchors.
				eligible.sort_by(|a, b| {
					b.value_sats
						.cmp(&a.value_sats)
						.then(a.outpoint.to_string().cmp(&b.outpoint.to_string()))
				});
				let merge = constraints.allow_asset_merge;
				let mut selected = Vec::new();
				let (mut assets, mut sats) = (0u128, 0u128);
				for candidate in eligible.iter().filter(|c| c.asset_amount > 0) {
					if !merge && candidate.asset_amount < target_asset {
						continue;
					}
					selected.push(candidate.outpoint.clone());
					assets += candidate.asset_amount as u128;
					sats += candidate.value_sats as u128;
					if !merge || (assets >= target_asset as u128 && sats >= target_value as u128) {
						break;
					}
				}
				if assets >= target_asset as u128 && constraints.allow_fee_support {
					for candidate in eligible.iter().filter(|c| c.asset_amount == 0) {
						if sats >= target_value as u128 {
							break;
						}
						selected.push(candidate.outpoint.clone());
						sats += candidate.value_sats as u128;
					}
				}
				selected
			},
		};
		let (mut assets, mut sats) = (0u128, 0u128);
		for outpoint in &selected {
			let candidate = eligible.iter().find(|c| &c.outpoint == outpoint).ok_or_else(|| {
				DomainError::CorruptedState(format!(
					"selected asset candidate disappeared: {outpoint}"
				))
			})?;
			assets += candidate.asset_amount as u128;
			sats += candidate.value_sats as u128;
		}
		if assets < target_asset as u128 {
			return Err(DomainError::InsufficientAsset {
				needed: target_asset as u128,
				have: assets,
			});
		}
		if sats < target_value as u128 {
			return Err(DomainError::InsufficientValue {
				needed: target_value as u128,
				have: sats,
			});
		}
		Ok(selected)
	}
}
