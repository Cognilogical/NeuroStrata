use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use super::CircuitBreakerConfig;

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitState {
    Closed,    // Normal operation
    Open,      // Failing, reject requests
    HalfOpen,  // Testing if service recovered
}

/// Circuit breaker to protect against cascading failures.
/// Implements exponential backoff on failures.
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    state: std::sync::Mutex<CircuitState>,
    failure_count: AtomicU32,
    last_failure_time: AtomicU64, // Unix timestamp in seconds
    backoff_until: AtomicU64,     // Unix timestamp in seconds
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: std::sync::Mutex::new(CircuitState::Closed),
            failure_count: AtomicU32::new(0),
            last_failure_time: AtomicU64::new(0),
            backoff_until: AtomicU64::new(0),
        }
    }

    /// Check if a request should be allowed through.
    pub fn allow_request(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let state = self.state.lock().unwrap();
        
        match *state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let backoff_until = self.backoff_until.load(Ordering::SeqCst);
                if now >= backoff_until {
                    // Time to try half-open
                    drop(state);
                    let mut state = self.state.lock().unwrap();
                    *state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // Allow one test request
                true
            }
        }
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::SeqCst);
        let mut state = self.state.lock().unwrap();
        *state = CircuitState::Closed;
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.last_failure_time.store(now, Ordering::SeqCst);
        let failures = self.failure_count.fetch_add(1, Ordering::SeqCst) + 1;

        if failures >= self.config.failure_threshold {
            // Calculate exponential backoff
            let backoff_duration = self.calculate_backoff(failures);
            let backoff_until = now + backoff_duration.as_secs();
            
            self.backoff_until.store(backoff_until, Ordering::SeqCst);
            
            let mut state = self.state.lock().unwrap();
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
        let backoff_ms = (self.config.initial_backoff_ms as f64)
            * self.config.backoff_multiplier.powf(exponent);
        
        let backoff_ms = backoff_ms.min(self.config.max_backoff_ms as f64);
        Duration::from_millis(backoff_ms as u64)
    }

    /// Get current circuit state (for monitoring).
    pub fn state(&self) -> String {
        let state = self.state.lock().unwrap();
        format!("{:?}", *state)
    }

    /// Get failure count (for monitoring).
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::SeqCst)
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
}
