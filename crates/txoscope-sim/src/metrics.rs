#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetricsDelta {
	pub prepare_successes: u64,
	pub prepare_failures: u64,
	pub committed_operations: u64,
	pub failed_operations: u64,
	pub cleanup_release_count: u64,
	pub replay_successes: u64,
	pub replay_failures: u64,
	pub lock_conflicts: u64,
	pub crash_count: u64,
	pub selected_input_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SimMetrics {
	pub prepare_successes: u64,
	pub prepare_failures: u64,
	pub committed_operations: u64,
	pub failed_operations: u64,
	pub cleanup_release_count: u64,
	pub replay_successes: u64,
	pub replay_failures: u64,
	pub lock_conflicts: u64,
	pub crash_count: u64,
	pub total_selected_input_count: u64,
	pub steps: usize,
}

impl SimMetrics {
	pub fn apply_delta(&mut self, delta: &MetricsDelta) {
		self.prepare_successes += delta.prepare_successes;
		self.prepare_failures += delta.prepare_failures;
		self.committed_operations += delta.committed_operations;
		self.failed_operations += delta.failed_operations;
		self.cleanup_release_count += delta.cleanup_release_count;
		self.replay_successes += delta.replay_successes;
		self.replay_failures += delta.replay_failures;
		self.lock_conflicts += delta.lock_conflicts;
		self.crash_count += delta.crash_count;
		self.total_selected_input_count += delta.selected_input_count;
		self.steps += 1;
	}
}
