use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Runs a blocking call on a worker thread, giving up after `limit`.
///
/// Display configuration goes through WindowServer IPC, and
/// `CGCompleteDisplayConfiguration` can block indefinitely when WindowServer is
/// wedged. Without a ceiling that hangs the caller — on the main thread it
/// hangs the whole UI.
///
/// A timed-out call is abandoned, not cancelled: the worker thread stays parked
/// in the syscall until it returns on its own. That leaks one thread per
/// timeout, which is the accepted cost of not freezing the app.
pub fn run_with_timeout<T, F>(limit: Duration, fallback: T, work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(limit).unwrap_or(fallback)
}
