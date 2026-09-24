use rand::Rng;

use crate::OperationId;

/// Produces opaque operation IDs for manual reservations.
pub trait ReservationIdGenerator {
	fn generate(&self, now_utc_unix_secs: u64) -> OperationId;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RandomReservationIdGenerator;

impl ReservationIdGenerator for RandomReservationIdGenerator {
	fn generate(&self, now_utc_unix_secs: u64) -> OperationId {
		let nonce: String = rand::thread_rng()
			.sample_iter(&rand::distributions::Alphanumeric)
			.take(32)
			.map(char::from)
			.collect();
		OperationId::new(format!("manual-reservation-{now_utc_unix_secs}-{nonce}"))
	}
}
