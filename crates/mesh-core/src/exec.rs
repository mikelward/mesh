//! External command execution.
//!
//! Launches external commands, optionally connected by pipes and with `<` / `>`
//! / `>>` redirections, and maps results to exit statuses. Interactive commands
//! run in a process group that owns the terminal while it is in the foreground;
//! non-interactive commands remain in mesh's group so signals still reach them.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};

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
    run_pipeline(vec![Cmd {
        words: words.to_vec(),
        redirs: Vec::new(),
    }])
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
/// Interactive pipelines get a foreground process group; non-interactive ones
/// stay in mesh's process group so signals sent to the invoking group reach all
/// stages.
pub fn run_pipeline(cmds: Vec<Cmd>) -> u8 {
    let n = cmds.len();
    let interactive = std::io::stdin().is_terminal();
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut next_stdin = NextIn::Inherit;
    let mut process_group = None;
    let shell_modes = interactive.then(terminal_modes).flatten();

    // Open each stage's redirections concurrently — each stage still opens its
    // own in source order, but different stages open at the same time, so a FIFO
    // opened by one stage does not block a peer opened by another stage of the
    // same pipeline (`cat < fifo | cmd > fifo`) before the writer is spawned.
    let opened = std::thread::scope(|scope| {
        let handles: Vec<_> = cmds
            .iter()
            .map(|cmd| scope.spawn(move || open_redirs(&cmd.redirs)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Ok((None, None))))
            .collect::<Vec<_>>()
    });

    for ((idx, cmd), redir_result) in cmds.into_iter().enumerate().zip(opened) {
        let is_last = idx + 1 == n;
        // Default the following stage to EOF; a successful piped spawn upgrades
        // it to the real pipe. So a redirected or failed stage leaves the next
        // one reading `/dev/null` rather than the shell's stdin.
        let incoming = std::mem::replace(&mut next_stdin, NextIn::Null);

        let (in_file, out_file) = match redir_result {
            Ok(files) => files,
            Err((path, err)) => {
                eprintln!("mesh: {path}: {err}");
                outcomes.push(Outcome::Failed(1));
                continue;
            }
        };

        let mut command = Command::new(&cmd.words[0]);
        command.args(&cmd.words[1..]);
        if interactive {
            // A zero process group makes the first child a group leader. Later
            // stages join it, so terminal signals address the entire pipeline.
            command.process_group(process_group.unwrap_or(0));
        }
        // The interactive shell ignores terminal-generated signals while it
        // owns the prompt. Children must restore the ordinary dispositions so
        // Ctrl-C/Ctrl-Z/Ctrl-\\ affect the foreground job instead of being
        // inherited as ignored across exec.
        unsafe {
            command.pre_exec(restore_job_signals);
        }

        // stdin: an input redirection wins over the incoming pipe/EOF/terminal.
        if let Some(file) = in_file {
            command.stdin(file);
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
        if let Some(file) = out_file {
            command.stdout(file);
        } else if !is_last {
            command.stdout(Stdio::piped());
            piped_out = true;
        }

        match command.spawn() {
            Ok(mut child) => {
                if interactive {
                    let pgid = process_group.unwrap_or_else(|| child.id() as i32);
                    process_group = Some(pgid);
                    // Repeat setpgid in the parent to close the race between
                    // spawn and exec; EACCES means the child won that race.
                    // SAFETY: setpgid has no pointer arguments and these PIDs
                    // came directly from successful child creation.
                    unsafe {
                        libc::setpgid(child.id() as libc::pid_t, pgid);
                    }
                }
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

    let foreground = process_group;
    if let Some(pgid) = foreground {
        set_foreground_group(pgid);
        // A child that reached a read before the handoff may have been stopped
        // by SIGTTIN. Resume the group only after it owns the terminal.
        // SAFETY: a negative PID addresses the process group created above.
        unsafe {
            libc::kill(-pgid, libc::SIGCONT);
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
            } => (wait_for_job(&mut child).unwrap_or(1), piped_out),
            Outcome::Failed(code) => (code, false),
        };
        if code != 0 && !(piped_out && code == SIGPIPE_CODE) {
            status = code;
        }
    }
    if foreground.is_some() {
        // getpgrp, rather than getpid, also handles a mesh process launched in
        // a process group established by its parent shell.
        // SAFETY: getpgrp takes no arguments and cannot fail.
        let shell_group = unsafe { libc::getpgrp() };
        set_foreground_group(shell_group);
        if let Some(modes) = shell_modes {
            restore_terminal_modes(&modes);
        }
    }
    status
}

fn terminal_modes() -> Option<libc::termios> {
    let mut modes = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr initializes `modes` on success.
    (unsafe { libc::tcgetattr(libc::STDIN_FILENO, modes.as_mut_ptr()) } == 0)
        .then(|| unsafe { modes.assume_init() })
}

fn restore_terminal_modes(modes: &libc::termios) {
    // SAFETY: `modes` came from tcgetattr for this terminal. Errors are best
    // effort here: command status must remain the foreground job's status.
    unsafe {
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSADRAIN, modes);
    }
}

/// Wait for a child to exit, be signaled, or stop. `Child::wait` only reports
/// termination, which would leave mesh blocked after Ctrl-Z. Reporting a stop
/// now lets the shell reclaim the terminal; the job-table task will retain the
/// process and make it available to `fg` / `bg`.
fn wait_for_job(child: &mut Child) -> std::io::Result<u8> {
    loop {
        let mut status = 0;
        // SAFETY: child.id() is a live child PID and status points to writable
        // storage. WUNTRACED requests the state transition needed for Ctrl-Z.
        let result =
            unsafe { libc::waitpid(child.id() as libc::pid_t, &mut status, libc::WUNTRACED) };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if libc::WIFEXITED(status) {
            return Ok(libc::WEXITSTATUS(status) as u8);
        }
        if libc::WIFSIGNALED(status) {
            return Ok(128u8.wrapping_add(libc::WTERMSIG(status) as u8));
        }
        if libc::WIFSTOPPED(status) {
            return Ok(128u8.wrapping_add(libc::WSTOPSIG(status) as u8));
        }
    }
}

/// Restore signals whose interactive-shell dispositions must not cross exec.
fn restore_job_signals() -> std::io::Result<()> {
    for signal in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
        libc::SIGTERM,
    ] {
        // SAFETY: signal is one of the valid constants above, and SIG_DFL is a
        // valid disposition. This runs after fork in Command's child hook.
        if unsafe { libc::signal(signal, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Give fd 0's controlling terminal to `pgid`. SIGTTOU must be blocked while a
/// background shell performs the handoff, or the kernel can suspend the shell.
fn set_foreground_group(pgid: libc::pid_t) {
    // SAFETY: all calls use scalar values. The old signal mask is initialized
    // by the first pthread_sigmask call before it is used by the second.
    unsafe {
        let mut block: libc::sigset_t = std::mem::zeroed();
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut block);
        libc::sigaddset(&mut block, libc::SIGTTOU);
        libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut old);
        libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
        libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
    }
}

/// Open every redirection in source order so each file's create/truncate side
/// effect and any error happens in order, as POSIX shells do (`> a > b` opens
/// both). Returns the final stdin/stdout target — the last redirection of each
/// direction wins. On the first failure, returns the offending path and error.
#[allow(clippy::type_complexity)]
fn open_redirs(
    redirs: &[(RedirKind, String)],
) -> Result<(Option<File>, Option<File>), (String, std::io::Error)> {
    let mut stdin_file = None;
    let mut stdout_file = None;
    for (kind, path) in redirs {
        match kind {
            RedirKind::In => stdin_file = Some(File::open(path).map_err(|e| (path.clone(), e))?),
            RedirKind::Out => {
                stdout_file = Some(File::create(path).map_err(|e| (path.clone(), e))?)
            }
            RedirKind::Append => {
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|e| (path.clone(), e))?;
                stdout_file = Some(file);
            }
        }
    }
    Ok((stdin_file, stdout_file))
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

#[cfg(test)]
mod tests {
    use super::{restore_job_signals, restore_terminal_modes, terminal_modes};

    #[test]
    fn child_restores_sigint_to_default() {
        // Isolate disposition changes in a fork so this test cannot interfere
        // with the test harness or concurrently running tests.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());
        if pid == 0 {
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
            }
            restore_job_signals().unwrap();
            unsafe {
                libc::raise(libc::SIGINT);
                libc::_exit(99);
            }
        }

        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGINT);
    }

    #[test]
    fn saved_terminal_modes_can_be_restored() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid != 0 {
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
            return;
        }
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(
            unsafe { libc::dup2(slave, libc::STDIN_FILENO) },
            libc::STDIN_FILENO
        );

        let saved = terminal_modes().expect("PTY has terminal modes");
        let mut changed = saved;
        changed.c_lflag ^= libc::ECHO;
        assert_eq!(
            unsafe { libc::tcsetattr(slave, libc::TCSANOW, &changed) },
            0
        );
        restore_terminal_modes(&saved);
        let restored = terminal_modes().expect("PTY still has terminal modes");
        assert_eq!(restored.c_lflag & libc::ECHO, saved.c_lflag & libc::ECHO);

        unsafe {
            libc::close(master);
            libc::close(slave);
            libc::_exit(0);
        }
    }
}
