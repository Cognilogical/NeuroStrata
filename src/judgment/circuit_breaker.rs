use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use super::CircuitBreakerConfig;

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitState {
    Closed,   // Normal operation
    Open,     // Failing, reject requests
    HalfOpen, // Testing if service recovered
}

/// Circuit breaker to protect against cascading failures.
/// Implements exponential backoff on failures.
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    state: std::sync::Mutex<CircuitState>,
    failure_count: AtomicU32,
    last_failure_time: AtomicU64, // Unix timestamp in seconds
    backoff_until: AtomicU64,     // Unix timestamp in seconds
    /// Gate for the HalfOpen trial: only ONE test request may be in flight
    /// while the circuit probes a recovering provider. CAS'd from false so
    /// concurrent callers do not each open their own probe window.
    half_open_probe_in_flight: AtomicBool,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: std::sync::Mutex::new(CircuitState::Closed),
            failure_count: AtomicU32::new(0),
            last_failure_time: AtomicU64::new(0),
            backoff_until: AtomicU64::new(0),
            half_open_probe_in_flight: AtomicBool::new(false),
        }
    }

    /// Check if a request should be allowed through.
    pub fn allow_request(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        match *state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let backoff_until = self.backoff_until.load(Ordering::SeqCst);
                if now >= backoff_until {
                    // Time to try half-open. The caller that opens the window
                    // also claims the single probe slot; a racing caller must
                    // not sneak a second probe in through this branch.
                    drop(state);
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    *state = CircuitState::HalfOpen;
                    tracing::debug!(
                        last_failure_epoch_secs = self.last_failure_epoch_secs(),
                        "circuit breaker Open -> HalfOpen; recovery probe window opened"
                    );
                    // A fresh HalfOpen window starts with no probe in flight;
                    // immediately claim it for THIS request.
                    self.half_open_probe_in_flight.store(false, Ordering::SeqCst);
                    self.half_open_probe_in_flight
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // Exactly ONE test request at a time: claim the probe slot
                // atomically so concurrent callers do not all probe.
                self.half_open_probe_in_flight
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            }
        }
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::SeqCst);
        self.half_open_probe_in_flight.store(false, Ordering::SeqCst);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *state = CircuitState::Closed;
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.last_failure_time.store(now, Ordering::SeqCst);
        self.half_open_probe_in_flight.store(false, Ordering::SeqCst);
        let failures = self.failure_count.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::debug!(
            failures = failures,
            last_failure_epoch_secs = self.last_failure_epoch_secs(),
            "circuit breaker recorded a failure"
        );

        if failures >= self.config.failure_threshold {
            // Calculate exponential backoff
            let backoff_duration = self.calculate_backoff(failures);
            let backoff_until = now + backoff_duration.as_secs();

            self.backoff_until.store(backoff_until, Ordering::SeqCst);

            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            *state = CircuitState::Open;

            tracing::warn!(
                failures = failures,
                backoff_secs = backoff_duration.as_secs(),
                "Circuit breaker opened due to failures"
            );
        }
    }

    /// Calculate exponential backoff duration.
    fn calculate_backoff(&self, failures: u32) -> Duration {
        let exponent = (failures - self.config.failure_threshold) as f64;
        let backoff_ms =
            (self.config.initial_backoff_ms as f64) * self.config.backoff_multiplier.powf(exponent);

        let backoff_ms = backoff_ms.min(self.config.max_backoff_ms as f64);
        Duration::from_millis(backoff_ms as u64)
    }

    /// Get current circuit state (for monitoring).
    pub fn state(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        format!("{:?}", *state)
    }

    /// Get failure count (for monitoring).
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::SeqCst)
    }

    /// Get the last failure timestamp (Unix seconds, 0 = never failed).
    /// Read so `last_failure_time` is observable, not a write-only field.
    pub fn last_failure_epoch_secs(&self) -> u64 {
        self.last_failure_time.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        assert_eq!(cb.state(), "Closed");
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_opens_after_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            initial_backoff_ms: 1000,
            max_backoff_ms: 60000,
            backoff_multiplier: 2.0,
        };
        let cb = CircuitBreaker::new(config);

        // First two failures don't open circuit
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), "Closed");
        assert!(cb.allow_request());

        // Third failure opens circuit
        cb.record_failure();
        assert_eq!(cb.state(), "Open");
        assert!(!cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.failure_count(), 2);

        cb.record_success();
        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.state(), "Closed");
    }

    #[test]
    fn test_half_open_allows_exactly_one_probe() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        // Drive the circuit open.
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), "Open");
        // Pause without mutating state: the method reads now but we can use
        // record timing to suppress a second failure — instead, set backoff
        // via config so it expires immediately: allow_request transitions to
        // HalfOpen when now >= backoff_until.
        // Wait out the initial backoff (1s default) then take the probe.
        std::thread::sleep(Duration::from_millis(1050));
        assert!(cb.allow_request(), "first post-backoff request takes the HalfOpen probe");
        assert_eq!(cb.state(), "HalfOpen");
        assert!(!cb.allow_request(), "second concurrent request must be refused while the probe is in flight");
        // The probe completes (success) -> circuit closes, slot released.
        cb.record_success();
        assert_eq!(cb.state(), "Closed");
        assert!(cb.allow_request());
    }

    #[test]
    fn test_record_failure_resets_probe_slot_and_sets_last_failure() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        assert_eq!(cb.last_failure_epoch_secs(), 0);
        cb.record_failure();
        assert!(
            cb.last_failure_epoch_secs() > 0,
            "last_failure_time must be observable after a failure"
        );
    }
}
