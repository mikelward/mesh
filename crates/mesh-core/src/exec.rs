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

/// Foreground jobs suspended with Ctrl-Z. Background launch syntax arrives
/// later; for now every entry starts as a stopped foreground pipeline.
pub struct JobTable {
    jobs: Vec<Job>,
    next_id: usize,
}

struct Job {
    id: usize,
    pgid: libc::pid_t,
    command: String,
    outcomes: Vec<Outcome>,
    shell_modes: Option<libc::termios>,
    state: JobState,
}

#[derive(Clone, Copy, PartialEq)]
enum JobState {
    Running,
    Stopped,
}

impl JobTable {
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 1,
        }
    }

    /// Resume a job in the foreground. With no operand, use the most recently
    /// registered job; explicit references accept `N` and `%N`.
    pub fn foreground(&mut self, args: &[String]) -> u8 {
        let Some(index) = self.resolve(args, "fg") else {
            return 1;
        };
        let mut job = self.jobs.remove(index);
        set_foreground_group(job.pgid);
        if signal_group(job.pgid, libc::SIGCONT, "fg").is_err() {
            reclaim_terminal(job.shell_modes.as_ref());
            return 1;
        }
        job.state = JobState::Running;
        let result = wait_outcomes(&mut job.outcomes);
        reclaim_terminal(job.shell_modes.as_ref());
        match result {
            WaitResult::Complete(status) => status,
            WaitResult::Stopped(status) => {
                job.state = JobState::Stopped;
                eprintln!("[{}] Stopped {}", job.id, job.command);
                self.jobs.push(job);
                status
            }
        }
    }

    /// Continue a stopped job without giving it the terminal.
    pub fn background(&mut self, args: &[String]) -> u8 {
        let Some(index) = self.resolve(args, "bg") else {
            return 1;
        };
        let job = &mut self.jobs[index];
        if signal_group(job.pgid, libc::SIGCONT, "bg").is_err() {
            return 1;
        }
        job.state = JobState::Running;
        eprintln!("[{}] Running {}", job.id, job.command);
        0
    }

    pub fn list(&mut self, args: &[String]) -> u8 {
        if !args.is_empty() {
            eprintln!("mesh: jobs: too many arguments");
            return 1;
        }
        self.reap();
        for job in &self.jobs {
            let state = if job.state == JobState::Stopped {
                "Stopped"
            } else {
                "Running"
            };
            println!("[{}] {state} {}", job.id, job.command);
        }
        0
    }

    /// Report jobs which completed since the preceding prompt and remove them.
    pub fn reap(&mut self) {
        let mut index = 0;
        while index < self.jobs.len() {
            if self.jobs[index].state == JobState::Running {
                match poll_outcomes(&mut self.jobs[index].outcomes) {
                    Some(WaitResult::Complete(status)) => {
                        let job = self.jobs.remove(index);
                        eprintln!("[{}] Done ({status}) {}", job.id, job.command);
                        continue;
                    }
                    Some(WaitResult::Stopped(_)) => self.jobs[index].state = JobState::Stopped,
                    None => {}
                }
            }
            index += 1;
        }
    }

    fn resolve(&self, args: &[String], name: &str) -> Option<usize> {
        if args.len() > 1 {
            eprintln!("mesh: {name}: too many arguments");
            return None;
        }
        if self.jobs.is_empty() {
            eprintln!("mesh: {name}: no current job");
            return None;
        }
        let Some(reference) = args.first() else {
            return Some(self.jobs.len() - 1);
        };
        let id = reference
            .strip_prefix('%')
            .unwrap_or(reference)
            .parse::<usize>();
        match id
            .ok()
            .and_then(|id| self.jobs.iter().position(|job| job.id == id))
        {
            Some(index) => Some(index),
            None => {
                eprintln!("mesh: {name}: {reference}: no such job");
                None
            }
        }
    }
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
pub fn run(words: &[String], jobs: &mut JobTable) -> u8 {
    run_pipeline(
        vec![Cmd {
            words: words.to_vec(),
            redirs: Vec::new(),
        }],
        jobs,
    )
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
pub fn run_pipeline(cmds: Vec<Cmd>, jobs: &mut JobTable) -> u8 {
    let command_text = cmds
        .iter()
        .map(|cmd| cmd.words.join(" "))
        .collect::<Vec<_>>()
        .join(" | ");
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
        if interactive {
            // The interactive shell ignores terminal-generated signals while
            // it owns the prompt. Restore them only in children of that mode;
            // a non-interactive invocation must preserve its caller's choices.
            // Hand off the terminal before exec so a newly started program
            // cannot race ahead and receive SIGTTIN.
            unsafe {
                command.pre_exec(|| {
                    restore_job_signals()?;
                    set_foreground_group(libc::getpgrp());
                    Ok(())
                });
            }
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
                // The child hook hands the terminal to the new process group
                // before exec. If the first stage cannot exec, no successful
                // child records that group for the normal reclaim path below.
                if interactive && process_group.is_none() {
                    // SAFETY: getpgrp takes no arguments and cannot fail.
                    set_foreground_group(unsafe { libc::getpgrp() });
                }
                outcomes.push(Outcome::Failed(spawn_error_code(&cmd.words[0], &err)));
            }
        }
    }

    let foreground = process_group;
    if let Some(pgid) = foreground {
        set_foreground_group(pgid);
    }

    // pipefail: the last stage to fail wins. A SIGPIPE is ignored only for a
    // stage whose stdout fed a pipe (a downstream stage could have closed it).
    let result = wait_outcomes(&mut outcomes);
    if interactive {
        // getpgrp, rather than getpid, also handles a mesh process launched in
        // a process group established by its parent shell. Reclaim even when
        // spawn failed: the child pre-exec hook may already have handed the
        // terminal to its short-lived process group before exec reported the
        // failure to the parent.
        // SAFETY: getpgrp takes no arguments and cannot fail.
        let shell_group = unsafe { libc::getpgrp() };
        set_foreground_group(shell_group);
        if let Some(modes) = shell_modes {
            restore_terminal_modes(&modes);
        }
    }
    match result {
        WaitResult::Complete(status) => status,
        WaitResult::Stopped(status) => {
            if let Some(pgid) = foreground {
                let id = jobs.next_id;
                jobs.next_id += 1;
                eprintln!("[{id}] Stopped {command_text}");
                jobs.jobs.push(Job {
                    id,
                    pgid,
                    command: command_text,
                    outcomes,
                    shell_modes,
                    state: JobState::Stopped,
                });
            }
            status
        }
    }
}

enum WaitResult {
    Complete(u8),
    Stopped(u8),
}

fn wait_outcomes(outcomes: &mut [Outcome]) -> WaitResult {
    let mut status = 0;
    let mut stopped = None;
    for outcome in outcomes {
        let (code, piped_out, did_stop) = match outcome {
            Outcome::Running { child, piped_out } => {
                let (code, stopped) = wait_for_job(child).unwrap_or((1, false));
                (code, *piped_out, stopped)
            }
            Outcome::Failed(code) => (*code, false, false),
        };
        if did_stop {
            stopped = Some(code);
        }
        if code != 0 && !(piped_out && code == SIGPIPE_CODE) {
            status = code;
        }
    }
    stopped.map_or(WaitResult::Complete(status), WaitResult::Stopped)
}

fn poll_outcomes(outcomes: &mut [Outcome]) -> Option<WaitResult> {
    let mut any_running = false;
    let mut status = 0;
    for outcome in outcomes {
        let Outcome::Running { child, piped_out } = outcome else {
            continue;
        };
        let mut raw = 0;
        let result = unsafe {
            libc::waitpid(
                child.id() as libc::pid_t,
                &mut raw,
                libc::WNOHANG | libc::WUNTRACED,
            )
        };
        if result == 0 {
            any_running = true;
        } else if result > 0 && libc::WIFSTOPPED(raw) {
            return Some(WaitResult::Stopped(128 + libc::WSTOPSIG(raw) as u8));
        } else if result > 0 {
            let code = wait_status(raw);
            if code != 0 && !(*piped_out && code == SIGPIPE_CODE) {
                status = code;
            }
        }
    }
    (!any_running).then_some(WaitResult::Complete(status))
}

fn signal_group(pgid: libc::pid_t, signal: libc::c_int, label: &str) -> Result<(), ()> {
    if unsafe { libc::kill(-pgid, signal) } < 0 {
        eprintln!("mesh: {label}: {}", std::io::Error::last_os_error());
        Err(())
    } else {
        Ok(())
    }
}

fn reclaim_terminal(modes: Option<&libc::termios>) {
    let shell_group = unsafe { libc::getpgrp() };
    set_foreground_group(shell_group);
    if let Some(modes) = modes {
        restore_terminal_modes(modes);
    }
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
fn wait_for_job(child: &mut Child) -> std::io::Result<(u8, bool)> {
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
            return Ok((libc::WEXITSTATUS(status) as u8, false));
        }
        if libc::WIFSIGNALED(status) {
            return Ok((128u8.wrapping_add(libc::WTERMSIG(status) as u8), false));
        }
        if libc::WIFSTOPPED(status) {
            return Ok((128u8.wrapping_add(libc::WSTOPSIG(status) as u8), true));
        }
    }
}

fn wait_status(status: libc::c_int) -> u8 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status) as u8
    } else if libc::WIFSIGNALED(status) {
        128u8.wrapping_add(libc::WTERMSIG(status) as u8)
    } else {
        1
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
    use super::{JobTable, restore_job_signals, restore_terminal_modes, run, terminal_modes};

    #[test]
    fn job_builtins_fail_cleanly_with_an_empty_table() {
        let mut jobs = JobTable::new();
        assert_eq!(jobs.foreground(&[]), 1);
        assert_eq!(jobs.background(&[]), 1);
        assert_eq!(jobs.list(&[]), 0);
    }

    #[test]
    fn spawn_failure_reclaims_the_terminal() {
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
        #[cfg(target_os = "macos")]
        let tiocsctty = libc::c_ulong::from(libc::TIOCSCTTY);
        #[cfg(not(target_os = "macos"))]
        let tiocsctty = libc::TIOCSCTTY;
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0
            || unsafe { libc::setsid() } < 0
            || unsafe { libc::ioctl(slave, tiocsctty, 0) } < 0
            || unsafe { libc::dup2(slave, libc::STDIN_FILENO) } < 0
        {
            unsafe { libc::_exit(1) };
        }
        let shell_group = unsafe { libc::getpgrp() };
        if unsafe { libc::tcsetpgrp(slave, shell_group) } < 0 {
            unsafe { libc::_exit(2) };
        }

        let mut jobs = JobTable::new();
        let status = run(&["mesh_command_that_does_not_exist_42".into()], &mut jobs);
        let foreground = unsafe { libc::tcgetpgrp(slave) };
        unsafe {
            libc::_exit(if status == 127 && foreground == shell_group {
                0
            } else {
                3
            });
        }
    }

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
