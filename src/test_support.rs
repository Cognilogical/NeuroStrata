//! Support shared by the whole crate's test build.
//!
//! `std::env` is process-global, so tests that mutate the same variable
//! must not run concurrently: one test's `remove_var` strands another's
//! `expect`. The env-mutating tests over shared variables hold
//! [`ENV_LOCK`] for their whole body — the daemon tests
//! (`NEUROSTRATA_PROJECT_ROOT`), the config tests (`TYPESAFE_API_KEY`),
//! and the store tests (`NEUROSTRATA_WRITE_TIMEOUT_SECS`). New tests
//! that mutate a shared variable must use this lock; a test mutating a
//! variable no other test reads does not race and needs no lock.
//!
//! The credential files are the second process-global resource:
//! `daemon.token` and `operator.token` under `~/.config/neurostrata`
//! are created by every spawned test daemon at startup and reread by
//! its shutdown path, while the server proxy tests rewrite and delete
//! them. Tests on either side hold [`TOKEN_FILE_TEST_LOCK`] for their
//! whole credential-dependent span.

/// The crate-wide lock serializing the env-mutating tests that route
/// through it; held for each test's full body. Poisoning (a panic while
/// holding it) is tolerated — `.unwrap_or_else(|e| e.into_inner())`.
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The crate-wide lock serializing every test that touches the real
/// credential files. A daemon test holds it from before its daemon
/// spawn (startup creates the files) until after its daemon task has
/// finished (shutdown rereads them); a server token test holds it for
/// its full body. Without the shared lock, a daemon shutting down
/// while a proxy test held a fixture — or no — token would read the
/// wrong credential, its `/shutdown` would answer 401, and the test
/// would hang on the daemon's join. Poisoning is tolerated the same
/// way as [`ENV_LOCK`].
pub(crate) static TOKEN_FILE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
