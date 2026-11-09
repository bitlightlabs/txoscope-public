use crate::{Availability, LockPurpose, OnchainState, OperationPhase};
use strum::IntoEnumIterator;

// Iterate the enums themselves so new variants are automatically checked.
macro_rules! check_vocabulary {
	($ty:ty) => {{
		#[cfg(feature = "schema")]
		let mut canonical_values = Vec::new();
		for variant in <$ty>::iter() {
			let canonical: &'static str = variant.into();
			assert_eq!(canonical.parse::<$ty>().unwrap(), variant);
			assert!(canonical
				.bytes()
				.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'));
			#[cfg(feature = "serde")]
			{
				let json = serde_json::Value::String(canonical.into());
				assert_eq!(serde_json::to_value(variant).unwrap(), json);
				assert_eq!(serde_json::from_value::<$ty>(json).unwrap(), variant);
				for invalid in
					[format!("{variant:?}"), canonical.to_uppercase(), format!(" {canonical}")]
				{
					assert!(serde_json::from_value::<$ty>(invalid.into()).is_err());
				}
			}
			#[cfg(feature = "schema")]
			canonical_values.push(canonical);
		}
		#[cfg(feature = "serde")]
		for invalid in
			[serde_json::Value::Null, serde_json::json!(0), serde_json::json!("future_value")]
		{
			assert!(serde_json::from_value::<$ty>(invalid).is_err());
		}
		#[cfg(feature = "schema")]
		{
			let schema = serde_json::to_value(<$ty as utoipa::PartialSchema>::schema()).unwrap();
			assert_eq!(schema["type"], "string");
			assert_eq!(schema["enum"], serde_json::json!(canonical_values));
			assert_eq!(<$ty as utoipa::ToSchema>::name(), stringify!($ty));
		}
	}};
}

#[test]
fn status_vocabulary_matches_canonical_strum_values() {
	check_vocabulary!(OnchainState);
	check_vocabulary!(Availability);
	check_vocabulary!(OperationPhase);
	check_vocabulary!(LockPurpose);
}

#[test]
fn legacy_vocabulary_is_rejected() {
	assert!("rgb_receive".parse::<crate::Capability>().is_err());
	assert!("l2_settlement".parse::<crate::Capability>().is_err());
	assert!("awaiting_l2_finality".parse::<OperationPhase>().is_err());
	assert!("blocked_by_l2".parse::<Availability>().is_err());
	#[cfg(feature = "serde")]
	{
		assert!(serde_json::from_str::<OperationPhase>(r#""awaiting_l2_finality""#).is_err());
		assert!(serde_json::from_str::<Availability>(r#""blocked_by_l2""#).is_err());
	}
	for alias in [
		"single_target_reserve",
		"anchor_with_fee_support",
		"rgb_receive_preparation",
		"rgb_l1_send",
		"channel_funding",
	] {
		assert!(alias.parse::<LockPurpose>().is_err());
		#[cfg(feature = "serde")]
		assert!(serde_json::from_value::<LockPurpose>(alias.into()).is_err());
	}
}
