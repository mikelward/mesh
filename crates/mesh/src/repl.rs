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
use crate::lexer::{Piece, Word};
use crate::vars::Vars;
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

/// Tokenize and run one line of input against the variable store. Empty lines
/// are a no-op that keeps the previous status.
fn run_line(text: &str, last: u8, vars: &mut Vars) -> Step {
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

    match classify(tokens) {
        Line::Assign { name, rhs } => match assign(&name, rhs, vars) {
            Ok(()) => Step::Continue(0),
            Err(msg) => {
                eprintln!("mesh: {msg}");
                Step::Continue(1)
            }
        },
        Line::Command(tokens) => {
            let words = match expand::expand(tokens, vars) {
                Ok(words) => words,
                Err(err) => {
                    eprintln!("mesh: {err}");
                    return Step::Continue(1);
                }
            };
            if words.is_empty() {
                // A command whose words all expanded away (e.g. a glob with no
                // matches) is an empty-list result — status 0 per `DESIGN.md`.
                return Step::Continue(0);
            }
            match builtins::dispatch(&words) {
                Some(Builtin::Exit(code)) => Step::Exit(code),
                Some(Builtin::Status(code)) => Step::Continue(code),
                None => Step::Continue(exec::run(&words)),
            }
        }
    }
}

/// A classified line: a variable binding or a command.
enum Line {
    Assign { name: String, rhs: Vec<Word> },
    Command(Vec<Word>),
}

/// Classify a non-empty token list. An assignment is a leading `name = value`
/// (spaced) or `name=value` (unspaced, the whole statement); position separates
/// it from a `k=v` argument after a command word (`git commit --author=me`).
///
/// Deferred: prefix env (`FOO=1 cmd` — use `env FOO=1 cmd`), and `name=value`
/// followed by more words.
fn classify(mut tokens: Vec<Word>) -> Line {
    // Spaced: `name` `=` value…
    if tokens.len() >= 2 && is_equals(&tokens[1]) {
        if let Some(name) = bare_ident(&tokens[0]) {
            let name = name.to_string();
            let rhs = tokens.split_off(2);
            return Line::Assign { name, rhs };
        }
    }
    // Unspaced: a single word `name=value`.
    if tokens.len() == 1 {
        if let Some((name, rhs)) = split_unspaced_assignment(&tokens[0]) {
            return Line::Assign {
                name,
                rhs: vec![rhs],
            };
        }
    }
    Line::Command(tokens)
}

/// If `word` is a single unquoted identifier, return it.
fn bare_ident(word: &Word) -> Option<&str> {
    match word.0.as_slice() {
        [
            Piece::Text {
                text,
                expandable: true,
            },
        ] if is_ident(text) => Some(text),
        _ => None,
    }
}

/// Is `word` exactly the bare token `=`?
fn is_equals(word: &Word) -> bool {
    matches!(word.0.as_slice(), [Piece::Text { text, expandable: true }] if text == "=")
}

/// Split a single word `name=value…` into the name and a word for the value, if
/// the leading unquoted text is `ident=…`. `value` keeps any later pieces (so
/// `x=$y` binds `x` to the value of `$y`).
fn split_unspaced_assignment(word: &Word) -> Option<(String, Word)> {
    let [
        Piece::Text {
            text,
            expandable: true,
        },
        rest @ ..,
    ] = word.0.as_slice()
    else {
        return None;
    };
    let (name, after) = text.split_once('=')?;
    if !is_ident(name) {
        return None;
    }
    let mut value: Vec<Piece> = Vec::new();
    if !after.is_empty() {
        value.push(Piece::Text {
            text: after.to_string(),
            expandable: true,
        });
    }
    value.extend(rest.iter().map(clone_piece));
    Some((name.to_string(), Word(value)))
}

fn clone_piece(piece: &Piece) -> Piece {
    match piece {
        Piece::Text { text, expandable } => Piece::Text {
            text: text.clone(),
            expandable: *expandable,
        },
        Piece::Var(v) => Piece::Var(crate::lexer::VarRef {
            name: v.name.clone(),
            member: v.member.clone(),
        }),
    }
}

/// A kebab identifier: an alphabetic/`_` head, then alphanumeric/`_`/interior-`-`.
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    let mut prev_hyphen = false;
    for c in chars {
        if c.is_ascii_alphanumeric() || c == '_' {
            prev_hyphen = false;
        } else if c == '-' {
            prev_hyphen = true;
        } else {
            return false;
        }
    }
    !prev_hyphen // no trailing hyphen
}

/// Bind `name` to the expansion of `rhs`. Only single-value assignments are
/// supported for now; a list (glob/multiple words) or empty result is an error.
fn assign(name: &str, rhs: Vec<Word>, vars: &mut Vars) -> Result<(), String> {
    let mut args = expand::expand(rhs, vars).map_err(|e| e.to_string())?;
    match args.len() {
        1 => {
            vars.set(name, args.pop().unwrap());
            Ok(())
        }
        0 => Err(format!("{name}: assignment needs a value")),
        _ => Err(format!("{name}: list assignment not supported yet")),
    }
}

/// Interactive loop: reedline line editing with an in-memory history. Ctrl-D on
/// an empty line exits (reedline's default — a non-empty line is unaffected);
/// Ctrl-C cancels the current line and returns to the prompt without exiting.
fn run_interactive() -> ExitCode {
    let mut editor = Reedline::create();
    let mut last: u8 = 0;
    let mut vars = Vars::new();
    loop {
        let prompt = MeshPrompt { failed: last != 0 };
        match editor.read_line(&prompt) {
            Ok(signal) => match handle_signal(signal, last, &mut vars) {
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
fn handle_signal(signal: Signal, last: u8, vars: &mut Vars) -> Step {
    match signal {
        Signal::Success(line) => run_line(&line, last, vars),
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
    let mut vars = Vars::new();
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
        match run_line(text, last, &mut vars) {
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
    use crate::vars::Vars;
    use reedline::Signal;

    #[test]
    fn ctrl_d_exits_with_the_last_status() {
        let mut vars = Vars::new();
        assert_eq!(handle_signal(Signal::CtrlD, 7, &mut vars), Step::Exit(7));
    }

    #[test]
    fn ctrl_c_re_prompts_keeping_status() {
        let mut vars = Vars::new();
        assert_eq!(
            handle_signal(Signal::CtrlC, 7, &mut vars),
            Step::Continue(7)
        );
    }

    #[test]
    fn a_submitted_exit_line_exits() {
        let mut vars = Vars::new();
        let signal = Signal::Success("exit 5".to_string());
        assert_eq!(handle_signal(signal, 0, &mut vars), Step::Exit(5));
    }

    #[test]
    fn a_submitted_blank_line_keeps_the_status() {
        let mut vars = Vars::new();
        assert_eq!(run_line("   ", 3, &mut vars), Step::Continue(3));
    }

    #[test]
    fn assignment_then_read() {
        let mut vars = Vars::new();
        assert_eq!(run_line("x = hello", 0, &mut vars), Step::Continue(0));
        assert_eq!(vars.get("x"), Some("hello"));
    }

    #[test]
    fn unspaced_assignment() {
        let mut vars = Vars::new();
        assert_eq!(run_line("n=42", 0, &mut vars), Step::Continue(0));
        assert_eq!(vars.get("n"), Some("42"));
    }
}
