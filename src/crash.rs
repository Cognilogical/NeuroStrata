//! A user-space crash breadcrumb, written before the process dies.
//!
//! The daemon died on 2026-10-20 with a native SIGSEGV inside the
//! statically-linked Kuzu/LadybugDB engine: no Rust panic, no stderr, no
//! stack-overflow message. The kernel logged `status=11/SEGV` and then
//! nothing, because this host's `kernel.core_pattern` pipes cores to
//! apport, which declines user-service crashes -- so no core survived and
//! the backtrace went with it. The next crash of the same shape would be
//! exactly as undiagnosable.
//!
//! This module writes what the process knows about itself at the moment it
//! faults: the signal, the pid, the thread, the time, and a best-effort
//! backtrace. That is the difference between "the daemon died" and "the
//! daemon died inside the engine, on this thread, at this point".
//!
//! Two rules shape the whole file:
//!
//! * **No privilege, ever.** Everything is written under the user's own
//!   config directory. Nothing is installed system-wide, nothing needs
//!   root, and the handler is never a prerequisite for anything.
//! * **The crash always wins.** Every failure path here is swallowed. If
//!   the directory cannot be created, the file cannot be written, the
//!   backtrace cannot be captured -- the process still dies of the signal
//!   it was going to die of, with the same status.
//!
//! A third fact is worth stating, because it was measured rather than
//! assumed: the handler runs on the alternate signal stack, and `SA_ONSTACK`
//! puts *every* signal there, not only the stack overflows. The Rust
//! standard library gives each thread an 8192-byte alternate stack, and the
//! backtrace capture does not fit in that -- it faults at 16 KiB and
//! completes at 32 KiB. A handler that tried to capture a backtrace on the
//! standard library's stack would fault inside itself and lose the
//! breadcrumb, so the threads that can fault are given a stack of their own
//! and the capture is skipped where there is no room for it.

#[cfg(unix)]
mod unix_impl {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::OnceLock;

    /// Where the handler writes. Set before any handler is installed, so the
    /// handler never sees a half-initialised value; leaked, so it is still
    /// there while the process is dying.
    static CRASH_DIR: OnceLock<CString> = OnceLock::new();

    /// The signals worth a breadcrumb. SIGSEGV and SIGBUS are the engine's
    /// native faults; SIGABRT catches an abort() out of the C++ core, and
    /// SIGFPE a division that goes wrong in native code.
    const CRASH_SIGNALS: [libc::c_int; 4] =
        [libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT, libc::SIGFPE];

    /// The alternate signal stack, in bytes.
    ///
    /// The handler runs on this stack, and it has to hold the handler's whole
    /// frame: the path buffer, the report buffer and the backtrace capture.
    /// The standard library's own alternate stack is 8192 bytes, and the
    /// capture alone does not fit in that (measured: it faults at 16 KiB and
    /// completes at 32 KiB). This is comfortably above both.
    const ALT_STACK_SIZE: usize = 64 * 1024;

    /// The smallest alternate stack the backtrace capture survives on.
    ///
    /// Measured, not guessed: at 16 KiB the capture faults inside the
    /// handler, at 32 KiB it completes.
    const BACKTRACE_MIN_STACK: usize = 32 * 1024;

    /// PATH_MAX. The path is built in a fixed buffer rather than a `PathBuf`
    /// because the handler must not allocate: a heap that is already broken
    /// is exactly the situation the handler may be running in. It is also
    /// deliberately smaller than PATH_MAX, because this buffer lives on the
    /// alternate stack and the alternate stack is small.
    const PATH_BUF: usize = 2048;

    /// Install the crash breadcrumb.
    ///
    /// Best effort by design. The directory is created if it can be, and
    /// every failure is returned to the caller rather than raised: the
    /// caller is expected to warn and carry on, because a missing
    /// breadcrumb must never stop the daemon from starting.
    pub fn install(crash_dir: &Path) -> std::io::Result<()> {
        // Best effort: a directory that cannot be created must not stop the
        // process from starting, so the failure is not returned.
        let _ = std::fs::create_dir_all(crash_dir);

        // Stored as a C string because the handler copies the bytes into a
        // stack buffer: it cannot allocate, and it cannot hold a borrow of
        // anything that might be gone by the time it runs.
        match CString::new(crash_dir.as_os_str().as_bytes()) {
            Ok(path) => {
                // Leaked on purpose: the handler reads it while the process
                // is dying, and freeing it then would be a use-after-free.
                let _ = CRASH_DIR.set(path);
            }
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the crash directory path contains a NUL byte",
                ));
            }
        }

        install_alt_stack()?;
        for sig in CRASH_SIGNALS {
            install_handler(sig)?;
        }
        Ok(())
    }

    /// Give the calling thread an alternate signal stack big enough for the
    /// handler.
    ///
    /// A handler that runs on the alternate stack needs that stack to hold
    /// its whole frame, and `SA_ONSTACK` puts *every* signal there -- not
    /// only the stack overflows. The Rust standard library installs an
    /// 8192-byte alternate stack for each thread it creates, and the
    /// backtrace capture does not fit in that. On such a thread the handler
    /// would fault inside itself and the breadcrumb would be lost, so the
    /// threads that can fault are given one from here.
    ///
    /// Installing twice on the same thread would leak the first buffer, so
    /// the current stack is checked first and left alone when it is already
    /// big enough.
    pub fn install_alt_stack() -> std::io::Result<()> {
        // SAFETY: a null new stack pointer asks the kernel only to report
        // the current alternate stack.
        let mut current: libc::stack_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaltstack(std::ptr::null_mut(), &mut current) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if current.ss_flags & libc::SS_DISABLE == 0 && current.ss_size >= ALT_STACK_SIZE {
            // This thread already has a stack of ours.
            return Ok(());
        }

        let mut bytes = vec![0u8; ALT_STACK_SIZE + 16];
        let raw = bytes.as_mut_ptr() as usize;
        // 16-byte alignment: x86-64 moves 16 bytes at a time, and the
        // alternate stack has to be aligned for the code that runs on it.
        let aligned = (raw + 15) & !15usize;
        // Leaked: the buffer has to outlive every signal, and the process
        // that owns it is about to die anyway.
        std::mem::forget(bytes);

        let alt_stack = libc::stack_t {
            ss_sp: aligned as *mut libc::c_void,
            ss_flags: 0,
            ss_size: ALT_STACK_SIZE,
        };
        // SAFETY: `alt_stack` describes a live, leaked, aligned buffer.
        if unsafe { libc::sigaltstack(&alt_stack, std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn install_handler(sig: libc::c_int) -> std::io::Result<()> {
        // SAFETY: a zeroed sigaction is a valid "no handler" value, and the
        // handler below has the SA_SIGINFO signature this call promises.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = crash_handler as *const () as libc::sighandler_t;
        // SA_ONSTACK runs the handler on the alternate stack, so a
        // stack-overflow fault is catchable at all.
        // SA_RESETHAND restores the default disposition when the handler is
        // entered, so a fault inside the handler cannot recurse: the second
        // SIGSEGV ends the process the way the first one would have.
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
        // SAFETY: `action` is a fully initialised sigaction.
        if unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// The crash handler itself.
    ///
    /// The order is the design: the cheap report goes down first, and the
    /// expensive one goes down second. A backtrace that hangs, allocates
    /// into a broken heap or faults again cannot take the report with it.
    extern "C" fn crash_handler(
        sig: libc::c_int,
        _info: *mut libc::siginfo_t,
        _context: *mut libc::c_void,
    ) {
        let mut path = [0u8; PATH_BUF];
        if let Some(len) = build_crash_path(&mut path) {
            // The path is NUL-terminated in place: the handler cannot afford
            // a second buffer to copy it into, because the whole frame has
            // to fit on the alternate signal stack.
            path[len] = 0;
            let path = &path[..len];
            write_minimal_report(path, sig);
            // Best effort. catch_unwind keeps a panic inside the handler
            // from turning a SIGSEGV into an abort; it cannot catch a
            // second fault, which is why the report above came first -- and
            // why the capture is skipped when the alternate stack is too
            // small to hold it.
            if alt_stack_has_room() {
                let _ = std::panic::catch_unwind(|| {
                    let backtrace = std::backtrace::Backtrace::force_capture();
                    append_backtrace(path, &backtrace);
                });
            }
        }

        // Die the way the process was going to die: of the signal, not of
        // an exit code, so systemd still reports status=11/SEGV and the
        // kernel still writes a core if the host is set up to keep one.
        //
        // The signal is blocked while its own handler runs -- SA_NODEFER is
        // not set -- so the re-raise below would only queue it: raise()
        // would return, and the process would leave with an exit code
        // instead of dying of the signal the kernel logged. Unblocking it
        // first is what makes the re-raise fatal.
        // SAFETY: `set` is a local sigset_t; sigemptyset and sigaddset only
        // write into it, and sigprocmask is async-signal-safe.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, sig);
            libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());

            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
            // Belt and braces: raise() with the default disposition and
            // the signal unblocked does not return.
            libc::_exit(128 + sig);
        }
    }

    /// Build `<crash_dir>/crash-<pid>-<epoch>.txt` in the caller's buffer.
    ///
    /// The digits are written by hand: `format!` allocates, and the handler
    /// may be running on a heap that is already broken.
    fn build_crash_path(buf: &mut [u8]) -> Option<usize> {
        let dir = CRASH_DIR.get()?;
        let mut n = 0usize;
        n = push_bytes(buf, n, dir.as_bytes())?;
        n = push_bytes(buf, n, b"/crash-")?;
        n = push_u64(buf, n, unsafe { libc::getpid() } as u64)?;
        n = push_bytes(buf, n, b"-")?;
        n = push_u64(buf, n, epoch_seconds())?;
        n = push_bytes(buf, n, b".txt")?;
        Some(n)
    }

    /// The size of the alternate stack the calling thread has.
    ///
    /// Zero when the thread has none.
    pub fn alt_stack_size() -> usize {
        current_alt_stack_size()
    }

    /// Whether the alternate stack has room for the backtrace capture.
    ///
    /// The capture is not small, and the handler runs on the alternate
    /// stack. On a thread whose alternate stack is too small the capture
    /// would fault inside the handler, which would cost the breadcrumb that
    /// was already written -- so it is only attempted where there is room.
    fn alt_stack_has_room() -> bool {
        alt_stack_size() >= BACKTRACE_MIN_STACK
    }

    /// The size of the alternate stack the handler is running on.
    ///
    /// Zero when the thread has none.
    fn current_alt_stack_size() -> usize {
        // SAFETY: a null new stack pointer asks the kernel only to report
        // the current alternate stack.
        let mut current: libc::stack_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaltstack(std::ptr::null_mut(), &mut current) } != 0 {
            return 0;
        }
        if current.ss_flags & libc::SS_DISABLE != 0 {
            return 0;
        }
        current.ss_size
    }

    /// The report body: everything the handler knows without allocating.
    fn build_minimal_report(buf: &mut [u8], sig: libc::c_int) -> Option<usize> {
        let mut n = 0usize;
        n = push_bytes(buf, n, b"NeuroStrata crash breadcrumb\n")?;
        n = push_bytes(buf, n, b"signal=")?;
        n = push_u64(buf, n, sig as u64)?;
        n = push_bytes(buf, n, b"\nsignal_name=")?;
        n = push_bytes(buf, n, signal_name(sig))?;
        n = push_bytes(buf, n, b"\npid=")?;
        n = push_u64(buf, n, unsafe { libc::getpid() } as u64)?;
        n = push_bytes(buf, n, b"\nthread_id=")?;
        n = push_u64(buf, n, thread_id())?;
        n = push_bytes(buf, n, b"\nepoch_seconds=")?;
        n = push_u64(buf, n, epoch_seconds())?;
        // The size of the alternate stack the handler ran on. It says why a
        // backtrace may be missing: the capture needs 32 KiB of it, and the
        // standard library gives a thread 8 KiB.
        n = push_bytes(buf, n, b"\nalt_stack_bytes=")?;
        n = push_u64(buf, n, alt_stack_size() as u64)?;
        n = push_bytes(buf, n, b"\n")?;
        Some(n)
    }

    /// Write the report with async-signal-safe calls only -- open, write,
    /// close -- because a handler that takes a lock or allocates can
    /// deadlock against the thread that is already dying.
    fn write_minimal_report(path: &[u8], sig: libc::c_int) {
        let fd = open_crash_file(path);
        if fd < 0 {
            return;
        }
        let mut report = [0u8; 256];
        if let Some(len) = build_minimal_report(&mut report, sig) {
            write_all(fd, &report[..len]);
        }
        // SAFETY: `fd` was just opened here and is not used again.
        unsafe { libc::close(fd) };
    }

    /// Open the crash file for writing, 0600.
    ///
    /// The path must already be NUL-terminated: the handler cannot afford a
    /// second buffer to copy it into, because the whole frame has to fit on
    /// the alternate signal stack, which is small.
    ///
    /// O_EXCL first: a file with this name belongs to another crash. If it
    /// somehow exists anyway the fallback truncates it -- a breadcrumb that
    /// is one crash old beats no breadcrumb at all.
    fn open_crash_file(path: &[u8]) -> libc::c_int {
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC;
        // SAFETY: `path` is a NUL-terminated path.
        let fd = unsafe {
            libc::open(
                path.as_ptr() as *const libc::c_char,
                flags | libc::O_EXCL,
                0o600,
            )
        };
        let fd = if fd >= 0 {
            fd
        } else {
            // SAFETY: as above.
            unsafe {
                libc::open(
                    path.as_ptr() as *const libc::c_char,
                    flags | libc::O_TRUNC,
                    0o600,
                )
            }
        };
        if fd >= 0 {
            // The mode passed to open() is filtered by the umask, so it is
            // set explicitly: the breadcrumb belongs to its owner alone.
            // SAFETY: `fd` is open.
            unsafe { libc::fchmod(fd, 0o600) };
        }
        fd
    }

    /// Append the backtrace to the breadcrumb.
    ///
    /// Best effort in the strong sense: the file may be gone, the heap may
    /// be broken, the unwinder may fail. None of that is allowed to stop
    /// the crash from happening.
    fn append_backtrace(path: &[u8], backtrace: &std::backtrace::Backtrace) {
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(path));
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(file, "\n--- backtrace ---");
            let _ = writeln!(file, "{}", backtrace);
            let _ = file.flush();
        }
    }

    /// write(2) until everything is out or the write fails.
    fn write_all(fd: libc::c_int, bytes: &[u8]) {
        let mut offset = 0usize;
        while offset < bytes.len() {
            // SAFETY: the slice is live for the duration of the call.
            let written = unsafe {
                libc::write(
                    fd,
                    bytes[offset..].as_ptr() as *const libc::c_void,
                    bytes.len() - offset,
                )
            };
            if written < 0 {
                // EINTR is not a failure: the write is retried.
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return;
            }
            if written == 0 {
                return;
            }
            offset += written as usize;
        }
    }

    /// The signal's own name, so the report says SIGSEGV and not just "11".
    fn signal_name(sig: libc::c_int) -> &'static [u8] {
        match sig {
            libc::SIGSEGV => b"SIGSEGV",
            libc::SIGBUS => b"SIGBUS",
            libc::SIGABRT => b"SIGABRT",
            libc::SIGFPE => b"SIGFPE",
            _ => b"SIGUNKNOWN",
        }
    }

    /// The thread that faulted. `gettid` is a syscall on Linux; the BSDs
    /// have no equivalent, so the pthread handle stands in there.
    #[cfg(target_os = "linux")]
    fn thread_id() -> u64 {
        // SAFETY: gettid takes no arguments and cannot fail.
        unsafe { libc::syscall(libc::SYS_gettid) as u64 }
    }

    #[cfg(not(target_os = "linux"))]
    fn thread_id() -> u64 {
        // SAFETY: pthread_self takes no arguments and cannot fail.
        unsafe { libc::pthread_self() as u64 }
    }

    /// Seconds since the epoch, UTC. `time()` is async-signal-safe, and the
    /// epoch needs no timezone.
    fn epoch_seconds() -> u64 {
        // SAFETY: a null pointer asks for the time only.
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        if now > 0 {
            now as u64
        } else {
            0
        }
    }

    /// Append bytes, or give up if the buffer is full.
    fn push_bytes(buf: &mut [u8], n: usize, bytes: &[u8]) -> Option<usize> {
        if n + bytes.len() >= buf.len() {
            return None;
        }
        buf[n..n + bytes.len()].copy_from_slice(bytes);
        Some(n + bytes.len())
    }

    /// Append a number's decimal digits, one at a time.
    fn push_u64(buf: &mut [u8], n: usize, value: u64) -> Option<usize> {
        let mut digits = [0u8; 20];
        let mut len = 0usize;
        let mut rest = value;
        if rest == 0 {
            digits[0] = b'0';
            len = 1;
        }
        while rest > 0 {
            digits[len] = b'0' + (rest % 10) as u8;
            rest /= 10;
            len += 1;
        }
        let mut n = n;
        for i in (0..len).rev() {
            if n >= buf.len() {
                return None;
            }
            buf[n] = digits[i];
            n += 1;
        }
        Some(n)
    }
}

#[cfg(unix)]
pub use unix_impl::{install, install_alt_stack};

/// The size of the calling thread's alternate signal stack, or zero when it
/// has none.
///
/// Test-only: the daemon never asks for this, it only ever installs one.
#[cfg(test)]
pub fn alt_stack_size() -> usize {
    #[cfg(unix)]
    {
        unix_impl::alt_stack_size()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The crash breadcrumb is a unix facility: there is no signal handling to
/// install elsewhere, so there is nothing to install and nothing to report.
#[cfg(not(unix))]
pub fn install(_crash_dir: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Nothing to do off unix: no signals, no alternate stack.
#[cfg(not(unix))]
pub fn install_alt_stack() -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::install;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;

    /// The child half of a test runs under this variable. Without it the
    /// test is the parent that spawns the child.
    const CHILD_VAR: &str = "NEUROSTRATA_CRASH_TEST_CHILD";
    const DIR_VAR: &str = "NEUROSTRATA_CRASH_TEST_DIR";

    /// The names these tests are selected by in the child process.
    const SEGFAULT_TEST_NAME: &str =
        "crash::tests::a_segfault_leaves_a_breadcrumb_and_still_dies_of_the_signal";
    const OVERFLOW_TEST_NAME: &str =
        "crash::tests::a_stack_overflow_on_a_worker_thread_leaves_a_breadcrumb";

    /// A real SIGSEGV, in a real child process, leaves a breadcrumb behind
    /// and still kills the process with the signal.
    ///
    /// The child is a separate process on purpose: a signal raised inside a
    /// test would take the test harness down with it, and the assertions
    /// here are about what the parent observes -- the wait status and the
    /// file that is left behind.
    #[test]
    fn a_segfault_leaves_a_breadcrumb_and_still_dies_of_the_signal() {
        if let Ok(dir) = std::env::var(DIR_VAR) {
            // The child: install the handler, then fault for real.
            install(&PathBuf::from(dir)).expect("the child installs the handler");
            // SAFETY: raising SIGSEGV with the handler installed is the
            // point of the test. The handler re-raises it with the default
            // disposition, so the process dies here.
            unsafe { libc::raise(libc::SIGSEGV) };
            // Unreachable in practice. A non-zero exit here would mean the
            // handler did not kill the process, and the parent must not
            // mistake that for a pass.
            std::process::exit(99);
        }

        let (status, breadcrumbs) = run_child(SEGFAULT_TEST_NAME);
        assert_eq!(
            status.signal(),
            Some(libc::SIGSEGV),
            "the child must still die of SIGSEGV, not of an exit code: {status:?}"
        );
        assert_eq!(
            breadcrumbs.len(),
            1,
            "exactly one breadcrumb should have been written: {breadcrumbs:?}"
        );
        assert!(
            breadcrumbs[0].1.contains("SIGSEGV"),
            "the report names the signal: {}",
            breadcrumbs[0].1
        );
    }

    /// A stack overflow on a worker thread leaves a breadcrumb.
    ///
    /// This is the case the alternate signal stack exists for: the faulting
    /// thread has no room left to run a handler on, so the handler has to
    /// run on the alternate stack the runtime installed for that thread.
    /// Without one the handler would fault on the exhausted stack and the
    /// process would die with nothing left behind -- which is exactly what
    /// the daemon did on 2026-10-20.
    ///
    /// The overflow is on a worker thread, not the main one, because that is
    /// where the daemon faults: the store runs on the tokio pool.
    #[test]
    fn a_stack_overflow_on_a_worker_thread_leaves_a_breadcrumb() {
        if let Ok(dir) = std::env::var(DIR_VAR) {
            install(&PathBuf::from(dir)).expect("the child installs the handler");
            let handle = std::thread::spawn(|| overflow_the_stack(0));
            let _ = handle.join();
            // The thread should have died in the handler. Reaching here
            // means the stack did not overflow, and the parent must not
            // mistake that for a pass.
            std::process::exit(99);
        }

        let (status, breadcrumbs) = run_child(OVERFLOW_TEST_NAME);
        assert_eq!(
            status.signal(),
            Some(libc::SIGSEGV),
            "the child must still die of SIGSEGV: {status:?}"
        );
        assert_eq!(
            breadcrumbs.len(),
            1,
            "exactly one breadcrumb should have been written: {breadcrumbs:?}"
        );
        assert!(
            breadcrumbs[0].1.contains("SIGSEGV"),
            "the report names the signal: {}",
            breadcrumbs[0].1
        );
    }

    /// Run the named test in a child process and collect what it left behind.
    ///
    /// Returns the child's wait status and the breadcrumbs it wrote.
    fn run_child(test_name: &str) -> (std::process::ExitStatus, Vec<(PathBuf, String)>) {
        let dir = std::env::temp_dir().join(format!(
            "ns-crash-breadcrumb-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("the crash directory exists");

        let exe = std::env::current_exe().expect("the test binary");
        let mut child = std::process::Command::new(exe)
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CHILD_VAR, "1")
            .env(DIR_VAR, &dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the child starts");
        let child_pid = child.id();
        let status = wait_with_timeout(&mut child, std::time::Duration::from_secs(60));

        // Read the breadcrumbs before the cleanup, so a failed assertion
        // does not leave the directory behind.
        let mut breadcrumbs: Vec<(PathBuf, String)> = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("the crash directory is readable") {
            let path = entry.expect("the directory entry is readable").path();
            if path.extension().and_then(|e| e.to_str()) == Some("txt") {
                let contents = std::fs::read_to_string(&path).unwrap_or_default();
                breadcrumbs.push((path, contents));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        for (path, contents) in &breadcrumbs {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            assert!(
                name.starts_with(&format!("crash-{child_pid}-")),
                "the breadcrumb is named crash-<pid>-<epoch>.txt: {}",
                path.display()
            );
            assert!(
                contents.contains(&child_pid.to_string()),
                "the report names the pid: {contents}"
            );
        }
        (status, breadcrumbs)
    }

    /// Wait for the child, but not forever.
    ///
    /// A test that hangs is worse than a test that fails: the child is given
    /// a deadline and killed if it overruns it.
    fn wait_with_timeout(
        child: &mut std::process::Child,
        limit: std::time::Duration,
    ) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => {}
                Err(e) => panic!("the child could not be waited on: {e}"),
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                panic!("the child did not die within {limit:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Recurse until the thread's stack is exhausted.
    ///
    /// The recursion is deliberately unbounded. The counter exists only so
    /// that the compiler cannot prove the call is infinite and turn it into
    /// a loop -- a loop would not overflow anything.
    #[inline(never)]
    fn overflow_the_stack(depth: u64) -> u8 {
        let local = [0u8; 64 * 1024];
        std::hint::black_box(&local);
        let deeper = if depth == u64::MAX {
            0
        } else {
            overflow_the_stack(depth + 1)
        };
        local[0].wrapping_add(deeper)
    }
}
