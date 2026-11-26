//! Small shared primitives with no subsystem ownership.

use std::fmt;
use std::sync::{Mutex, MutexGuard};

/// A mutex was poisoned: the thread that held it panicked mid-mutation.
///
/// The guarded value may be half-updated, so adopting it would pretend a
/// failed mutation completed coherently. Callers either fail the operation
/// with this error (`Result` sites) or degrade the subsystem for that one
/// operation (the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockPoisoned;

impl fmt::Display for LockPoisoned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("mutex lock is poisoned")
    }
}

impl std::error::Error for LockPoisoned {}

/// Acquires `m`, mapping poisoning to the typed [`LockPoisoned`] error.
///
/// This is the one acquisition policy for the daemon: never panic, never
/// silently recover the guarded payload.
pub fn lock_or_err<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, LockPoisoned> {
    m.lock().map_err(|_| LockPoisoned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_locks_a_healthy_mutex() {
        let mutex = Mutex::new(41);
        let mut guard = lock_or_err(&mutex).expect("a healthy mutex locks");
        *guard += 1;
        drop(guard);
        assert_eq!(*mutex.lock().expect("re-lock"), 42);
    }

    #[test]
    fn helper_maps_poison_to_error() {
        let mutex = Mutex::new(0);
        std::thread::scope(|scope| {
            let poisoner = scope.spawn(|| {
                let _guard = mutex.lock().expect("poison target lock");
                panic!("poison the mutex on purpose");
            });
            assert!(poisoner.join().is_err(), "the poisoner must unwind");
        });

        let error = lock_or_err(&mutex).expect_err("poison must map to LockPoisoned");
        assert!(
            error.to_string().contains("poisoned"),
            "the error message must name poisoning: {error}"
        );
        let _: &dyn std::error::Error = &error;
    }
}
