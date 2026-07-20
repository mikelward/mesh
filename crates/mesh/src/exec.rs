//! External command execution.
//!
//! Launches external commands, optionally connected by pipes and with `<` / `>`
//! / `>>` redirections, and maps results to exit statuses. `std::process::Command`
//! resolves names against the inherited `PATH`. No job control yet.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};

use crate::lexer::RedirKind;

/// A pipeline stage: an expanded argv and its redirections (in source order;
/// for a given direction the last one wins, as in POSIX shells).
pub struct Cmd {
    pub words: Vec<String>,
    pub redirs: Vec<(RedirKind, String)>,
}

/// `128 + SIGPIPE(13)` — an upstream stage killed because a later stage closed
/// the pipe early. Under our pipefail rule this does not count as a failure.
const SIGPIPE_CODE: u8 = 128 + 13;

/// Run `words[0]` with `words[1..]` as arguments and return its exit status.
///
/// `words` is guaranteed non-empty by the caller. Status conventions follow
/// POSIX shells: `127` for a command that could not be found, `126` for one
/// that could not be executed, and `128 + signal` when the child is killed by a
/// signal. These line up with the result/status model in `DESIGN.md`.
pub fn run(words: &[String]) -> u8 {
    match Command::new(&words[0]).args(&words[1..]).status() {
        Ok(status) => status_to_code(status),
        Err(err) => spawn_error_code(&words[0], &err),
    }
}

/// How the next stage receives its stdin.
enum NextIn {
    /// The first stage with no `<` inherits the shell's stdin.
    Inherit,
    /// EOF (`/dev/null`): the previous stage sent its stdout elsewhere (a
    /// redirect) or failed to spawn, so there is no producer for this stage.
    Null,
    /// The previous stage's stdout, piped in.
    Pipe(ChildStdout),
}

/// A spawned stage awaiting its status, or a stage that failed before running.
enum Outcome {
    /// `piped_out` is true when this stage's stdout fed a downstream pipe (the
    /// only case where a SIGPIPE can legitimately come from a later stage
    /// closing the pipe).
    Running {
        child: Child,
        piped_out: bool,
    },
    Failed(u8),
}

/// Run a pipeline of external commands connected by pipes, applying each stage's
/// redirections. The status is **pipefail, ignoring upstream SIGPIPE**: the last
/// stage to fail wins, except a stage whose stdout fed a pipe and was killed by
/// SIGPIPE (a later stage closed the pipe early) is not counted — so `false |
/// true` is `1`, `big | head` is `0`, but a SIGPIPE in the final stage still
/// counts.
///
/// `cmds` is non-empty and every stage is an external command (builtins in a
/// pipeline / with redirection are not supported yet, and are rejected earlier).
pub fn run_pipeline(cmds: Vec<Cmd>) -> u8 {
    let n = cmds.len();
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut next_stdin = NextIn::Inherit;

    for (idx, cmd) in cmds.into_iter().enumerate() {
        let is_last = idx + 1 == n;
        // Default the following stage to EOF; a successful piped spawn upgrades
        // it to the real pipe. So a redirected or failed stage leaves the next
        // one reading `/dev/null` rather than the shell's stdin.
        let incoming = std::mem::replace(&mut next_stdin, NextIn::Null);
        let mut command = Command::new(&cmd.words[0]);
        command.args(&cmd.words[1..]);

        // stdin: an input redirection wins over the incoming pipe/EOF/terminal.
        if let Some(path) = last_redir(&cmd.redirs, RedirKind::In) {
            match File::open(path) {
                Ok(file) => {
                    command.stdin(file);
                }
                Err(err) => {
                    eprintln!("mesh: {path}: {err}");
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
            }
        } else {
            match incoming {
                NextIn::Inherit => {}
                NextIn::Null => {
                    command.stdin(Stdio::null());
                }
                NextIn::Pipe(prev) => {
                    command.stdin(prev);
                }
            }
        }

        // stdout: an output redirection wins over the pipe to the next stage;
        // otherwise pipe to the next stage; otherwise inherit (only the last).
        let mut piped_out = false;
        if let Some((kind, path)) = last_out_redir(&cmd.redirs) {
            match open_out(kind, path) {
                Ok(file) => {
                    command.stdout(file);
                }
                Err(err) => {
                    eprintln!("mesh: {path}: {err}");
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
            }
        } else if !is_last {
            command.stdout(Stdio::piped());
            piped_out = true;
        }

        match command.spawn() {
            Ok(mut child) => {
                if piped_out {
                    if let Some(out) = child.stdout.take() {
                        next_stdin = NextIn::Pipe(out);
                    }
                }
                outcomes.push(Outcome::Running { child, piped_out });
            }
            Err(err) => {
                outcomes.push(Outcome::Failed(spawn_error_code(&cmd.words[0], &err)));
            }
        }
    }

    // pipefail: the last stage to fail wins. A SIGPIPE is ignored only for a
    // stage whose stdout fed a pipe (a downstream stage could have closed it).
    let mut status = 0;
    for outcome in outcomes {
        let (code, piped_out) = match outcome {
            Outcome::Running {
                mut child,
                piped_out,
            } => (child.wait().map(status_to_code).unwrap_or(1), piped_out),
            Outcome::Failed(code) => (code, false),
        };
        if code != 0 && !(piped_out && code == SIGPIPE_CODE) {
            status = code;
        }
    }
    status
}

/// The path of the last redirection of `kind`, if any (last wins).
fn last_redir(redirs: &[(RedirKind, String)], kind: RedirKind) -> Option<&str> {
    redirs
        .iter()
        .rev()
        .find(|(k, _)| *k == kind)
        .map(|(_, path)| path.as_str())
}

/// The last output redirection (`>` or `>>`), if any (last wins).
fn last_out_redir(redirs: &[(RedirKind, String)]) -> Option<(RedirKind, &str)> {
    redirs
        .iter()
        .rev()
        .find(|(k, _)| matches!(k, RedirKind::Out | RedirKind::Append))
        .map(|(k, path)| (*k, path.as_str()))
}

/// Open an output-redirection target: `>` truncates (or creates), `>>` appends.
fn open_out(kind: RedirKind, path: &str) -> std::io::Result<File> {
    match kind {
        RedirKind::Append => OpenOptions::new().create(true).append(true).open(path),
        _ => File::create(path),
    }
}

/// Map a spawn error to a status and report it (`127` not-found, else `126`).
fn spawn_error_code(name: &str, err: &std::io::Error) -> u8 {
    match err.kind() {
        ErrorKind::NotFound => {
            eprintln!("mesh: command not found: {name}");
            127
        }
        ErrorKind::PermissionDenied => {
            eprintln!("mesh: permission denied: {name}");
            126
        }
        _ => {
            eprintln!("mesh: {name}: {err}");
            126
        }
    }
}

/// Map an `ExitStatus` to a shell exit code (`128 + signal` when signaled).
fn status_to_code(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128u8.wrapping_add(signal as u8);
        }
    }
    1
}
