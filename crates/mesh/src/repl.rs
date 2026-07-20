//! The read / tokenize / dispatch loop.
//!
//! Interactive (TTY) input goes through [`reedline`] for line editing, history,
//! and Ctrl-C/Ctrl-D handling. Piped / non-interactive input keeps the std-only
//! unbuffered fd-0 byte reader, so a spawned child still inherits any bytes that
//! follow its command line and the integration tests need no terminal.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};

use crate::builtins::{self, Builtin};
use crate::{exec, expand, lexer};

/// Run the shell until end-of-input or `exit`, returning the last status as the
/// process exit code.
///
/// Interactive line editing needs **both** stdin and stdout to be terminals:
/// reedline reads keys from the tty and renders its prompt and cursor-position
/// queries through stdout. If stdout is redirected (`mesh >session.log`), those
/// control bytes would corrupt the file and the cursor query could hang, so we
/// fall back to the plain line reader. (A prompt on the controlling terminal
/// even when stdout is redirected would need reedline to write to `/dev/tty`;
/// that refinement is deferred.)
pub fn run() -> ExitCode {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        run_interactive()
    } else {
        run_piped()
    }
}

/// What to do after handling one input line.
#[derive(Debug, PartialEq)]
enum Step {
    /// A line ran; carry this status as the new "last status".
    Continue(u8),
    /// `exit` was invoked; leave the shell with this status.
    Exit(u8),
}

/// Tokenize and dispatch one line of input. Empty lines are a no-op that keeps
/// the previous status.
fn run_line(text: &str, last: u8) -> Step {
    let tokens = match lexer::split(text) {
        Ok(tokens) => tokens,
        Err(err) => {
            eprintln!("mesh: {err}");
            return Step::Continue(2); // syntax error
        }
    };
    if tokens.is_empty() {
        // A blank line is not a command; the last status is unchanged.
        return Step::Continue(last);
    }
    let words = expand::expand(tokens);
    if words.is_empty() {
        // A real command whose words all expanded away (e.g. a glob with no
        // matches) is an empty-list result — status 0 per `DESIGN.md`, not the
        // previous status.
        return Step::Continue(0);
    }
    match builtins::dispatch(&words) {
        Some(Builtin::Exit(code)) => Step::Exit(code),
        Some(Builtin::Status(code)) => Step::Continue(code),
        None => Step::Continue(exec::run(&words)),
    }
}

/// Interactive loop: reedline line editing with an in-memory history. Ctrl-D on
/// an empty line exits (reedline's default — a non-empty line is unaffected);
/// Ctrl-C cancels the current line and returns to the prompt without exiting.
fn run_interactive() -> ExitCode {
    let mut editor = Reedline::create();
    let mut last: u8 = 0;
    loop {
        let prompt = MeshPrompt { failed: last != 0 };
        match editor.read_line(&prompt) {
            Ok(signal) => match handle_signal(signal, last) {
                Step::Exit(code) => return ExitCode::from(code),
                Step::Continue(code) => last = code,
            },
            Err(err) => {
                eprintln!("mesh: line editor error: {err}");
                return ExitCode::from(1);
            }
        }
    }
}

/// Map a reedline signal to the next [`Step`]. Extracted from the read loop so
/// the interactive control flow is unit-testable without a terminal.
///
/// `Ctrl-D` exits (reedline only emits it on an empty line, so this is the
/// exit-on-empty behavior); `Ctrl-C` — and any future signal — cancels the
/// current line and re-prompts, keeping the last status.
fn handle_signal(signal: Signal, last: u8) -> Step {
    match signal {
        Signal::Success(line) => run_line(&line, last),
        Signal::CtrlD => Step::Exit(last),
        _ => Step::Continue(last),
    }
}

/// Piped / non-interactive loop: read commands unbuffered from fd 0 so bytes
/// past a command's newline stay in the pipe/file for a child that inherits
/// stdin. A malformed (non-UTF-8) line is rejected loudly and skipped.
fn run_piped() -> ExitCode {
    // `ManuallyDrop` keeps us from closing fd 0 when the shell exits.
    let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut last: u8 = 0;
    let mut line = Vec::new();

    loop {
        line.clear();
        match read_line(&mut *stdin, &mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(err) => {
                eprintln!("mesh: read error: {err}");
                return ExitCode::from(1);
            }
        }

        let text = match std::str::from_utf8(&line) {
            Ok(text) => text,
            Err(_) => {
                eprintln!("mesh: invalid UTF-8 in input");
                last = 1;
                continue;
            }
        };
        match run_line(text, last) {
            Step::Exit(code) => return ExitCode::from(code),
            Step::Continue(code) => last = code,
        }
    }
    ExitCode::from(last)
}

/// Read one line (up to and including the newline) into `out`, one byte at a
/// time so nothing beyond the newline is consumed. Returns the number of bytes
/// read; 0 signals EOF.
fn read_line(reader: &mut impl Read, out: &mut Vec<u8>) -> io::Result<usize> {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break, // EOF
            Ok(_) => {
                out.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(out.len())
}

/// The minimal two-glyph prompt: `mesh$` after success, `mesh!` after failure.
/// The full status-dashboard prompt from `DESIGN.md` is a later milestone.
struct MeshPrompt {
    failed: bool,
}

impl Prompt for MeshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<'_, str> {
        if self.failed {
            Cow::Borrowed("mesh! ")
        } else {
            Cow::Borrowed("mesh$ ")
        }
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("... ")
    }
    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("search: ")
    }
}

#[cfg(test)]
mod tests {
    use super::{Step, handle_signal, run_line};
    use reedline::Signal;

    #[test]
    fn ctrl_d_exits_with_the_last_status() {
        assert_eq!(handle_signal(Signal::CtrlD, 7), Step::Exit(7));
    }

    #[test]
    fn ctrl_c_re_prompts_keeping_status() {
        assert_eq!(handle_signal(Signal::CtrlC, 7), Step::Continue(7));
    }

    #[test]
    fn a_submitted_exit_line_exits() {
        let signal = Signal::Success("exit 5".to_string());
        assert_eq!(handle_signal(signal, 0), Step::Exit(5));
    }

    #[test]
    fn a_submitted_blank_line_keeps_the_status() {
        assert_eq!(run_line("   ", 3), Step::Continue(3));
    }
}
