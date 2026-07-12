use std::time::{Duration, Instant};

pub const UI_STALL_THRESHOLD: Duration = Duration::from_millis(16);
pub const STORAGE_THRESHOLD: Duration = Duration::from_millis(20);
pub const PROCESS_THRESHOLD: Duration = Duration::from_millis(250);
pub const RPC_THRESHOLD: Duration = Duration::from_millis(500);

/// Emits one diagnostic when an operation exceeds its expected latency budget.
/// Fast operations stay silent so performance logging does not become its own
/// source of noise during rendering or streaming.
pub struct SlowOperation {
    operation: &'static str,
    context: String,
    started_at: Instant,
    threshold: Duration,
}

impl SlowOperation {
    pub fn new(operation: &'static str, threshold: Duration, context: impl Into<String>) -> Self {
        Self {
            operation,
            context: context.into(),
            started_at: Instant::now(),
            threshold,
        }
    }
}

impl Drop for SlowOperation {
    fn drop(&mut self) {
        let elapsed = self.started_at.elapsed();
        if elapsed < self.threshold {
            return;
        }

        eprintln!(
            "[perf] slow operation={} elapsed_ms={:.1} threshold_ms={} {}",
            self.operation,
            elapsed.as_secs_f64() * 1_000.0,
            self.threshold.as_millis(),
            self.context,
        );
    }
}
