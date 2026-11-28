//! The lib's tests read their own log lines through one shared logger: `log` takes one logger per process, so a test
//! that installed its own would fail any other test that did the same. Lines are kept per test thread.

use std::cell::RefCell;
use std::sync::Once;

struct Capture;

thread_local! {
    static LINES: RefCell<Vec<(log::Level, String)>> = const { RefCell::new(Vec::new()) };
}

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        LINES.with_borrow_mut(|lines| lines.push((record.level(), record.args().to_string())));
    }

    fn flush(&self) {}
}

/// The lines that `run` logs on this thread, with their levels.
pub(crate) fn logged(run: impl FnOnce()) -> Vec<(log::Level, String)> {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        log::set_logger(&Capture).unwrap();
        log::set_max_level(log::LevelFilter::Info);
    });
    LINES.with_borrow_mut(Vec::clear);
    run();
    LINES.take()
}
