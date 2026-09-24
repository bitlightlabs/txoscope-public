use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock {
	fn now_utc_unix_secs(&self) -> u64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UtcClock;

impl Clock for UtcClock {
	/// Returns UTC as Unix seconds for internal time arithmetic.
	fn now_utc_unix_secs(&self) -> u64 {
		SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
	}
}
