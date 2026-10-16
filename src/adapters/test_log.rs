//! Test-only recording logger (#130 DP-1).
//!
//! `log` allows one global logger per process and the test harness runs tests
//! on parallel threads, so the logger is installed once and records only while
//! at least one [`LogCapture`] is alive. Each record keeps the emitting thread
//! so a test can match its own messages positively while still checking every
//! thread's records for leaked values.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::thread::ThreadId;

use log::{Level, LevelFilter, Log, Metadata, Record};

struct RecordingLogger;

static LOGGER: RecordingLogger = RecordingLogger;
static INSTALL: Once = Once::new();
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static RECORDS: Mutex<Vec<(ThreadId, Level, String)>> = Mutex::new(Vec::new());

fn records() -> MutexGuard<'static, Vec<(ThreadId, Level, String)>> {
    RECORDS.lock().unwrap_or_else(|e| e.into_inner())
}

impl Log for RecordingLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        ACTIVE.load(Ordering::SeqCst) > 0
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        records().push((std::thread::current().id(), record.level(), message));
    }

    fn flush(&self) {}
}

/// Records every log message emitted, on any thread, while it is alive.
pub(crate) struct LogCapture {
    start: usize,
}

impl LogCapture {
    pub(crate) fn start() -> Self {
        INSTALL.call_once(|| {
            log::set_logger(&LOGGER).expect("no other logger is installed in unit tests");
            log::set_max_level(LevelFilter::Trace);
        });
        let records = records();
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        Self {
            start: records.len(),
        }
    }

    /// Messages recorded since this capture started, from every thread.
    pub(crate) fn all_messages(&self) -> Vec<String> {
        records()[self.start..]
            .iter()
            .map(|(_, _, m)| m.clone())
            .collect()
    }

    /// `(level, message)` recorded since this capture started on the calling thread.
    pub(crate) fn this_thread(&self) -> Vec<(Level, String)> {
        let me = std::thread::current().id();
        records()[self.start..]
            .iter()
            .filter(|(t, _, _)| *t == me)
            .map(|(_, l, m)| (*l, m.clone()))
            .collect()
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}
