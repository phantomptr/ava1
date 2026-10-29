//! Where the library's diagnostic lines go: a transfer's bottleneck summary, a stage-timer line
//! (`AVA1_TIMING=1`), the lane buffer sizes the kernel granted. Standard error by default; an
//! application sends them to its own log (or nowhere) with [`set_sink`].
//!
//! ```
//! ava1::diag::set_sink(Some(Box::new(|line: &str| println!("ava1: {line}"))));
//! ava1::diag::set_sink(None); // back to standard error
//! ```
use std::fmt;
use std::sync::RwLock;

type Sink = Box<dyn Fn(&str) + Send + Sync>;

static SINK: RwLock<Option<Sink>> = RwLock::new(None);

/// Routes every later diagnostic line to `sink` (`None`: standard error again).
pub fn set_sink(sink: Option<Sink>) {
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = sink;
}

/// One line. Never panics: a closed standard error (a parent that died) is ignored.
pub(crate) fn line(args: fmt::Arguments<'_>) {
    let guard = SINK.read().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(f) => f(&args.to_string()),
        None => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "{args}");
        }
    }
}
