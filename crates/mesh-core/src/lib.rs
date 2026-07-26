//! Core parser, expansion, and runtime for the mesh shell.
//!
//! The [`run`] entry point owns the read/tokenize/dispatch loop. The binary crate
//! deliberately contains only process startup so other frontends and tests can
//! use the shell implementation without depending on an executable crate.

/// Print a diagnostic line on stderr, dropping it if the write fails.
///
/// The shell's own messages must never abort it, and `eprintln!` panics when the
/// write fails. Stderr can be a full disk or a closed pipe — a redirected builtin
/// (`pwd extra 2>/dev/full`) applies the target to the shell's *own* fd 2 — so
/// every diagnostic goes through here. A message that cannot be delivered is
/// dropped: there is nowhere left to report the failure of reporting.
///
/// The line is formatted first and written once. `write!`-per-piece would make
/// every piece of the format string its own `write(2)`, and a background redirect
/// helper is a *separate process* sharing this stderr, so its message and the
/// shell's job notices would interleave mid-line — `mesh: [1] 4242` followed by
/// the orphaned remainder of the helper's error. One write under `PIPE_BUF` is
/// atomic, so each line stays whole.
macro_rules! note {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let line = format!("{}\n", format_args!($($arg)*));
        let _ = std::io::stderr().write_all(line.as_bytes());
    }};
}

mod builtins;
mod completion;
mod environ;
mod exec;
mod expand;
mod funcs;
mod options;
pub mod parser;
mod reaper;
mod repl;
mod vars;
mod whence;

pub use repl::run;
