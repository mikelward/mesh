//! External command execution.
//!
//! Launches external commands, optionally connected by pipes and with `<` / `>`
//! / `>>` redirections, and maps results to exit statuses. Interactive commands
//! run in a process group that owns the terminal while it is in the foreground;
//! non-interactive commands remain in mesh's group so signals still reach them.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::io::IsTerminal;
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirKind {
    In,
    Out,
    Append,
}

/// What a redirection points a descriptor at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirTarget {
    /// A path to open, per the direction.
    Path(String),
    /// Another descriptor, whose destination *at this point in the sequence* is
    /// copied — `2>&1` takes wherever stdout goes by then.
    Descriptor(libc::c_int),
    /// A heredoc body, already interpolated. It becomes an unlinked temporary
    /// file rather than a pipe, so a body larger than the pipe buffer cannot
    /// deadlock the shell against a command that has not started reading.
    Heredoc(String),
}

/// One redirection: which descriptor it retargets, in which direction, and what
/// it points at. `fd` lets `2> log` reach stderr while a bare `> log` keeps its
/// default of stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirection {
    pub fd: libc::c_int,
    pub kind: RedirKind,
    pub target: RedirTarget,
}

impl Redirection {
    /// The descriptor a direction retargets when no `N` prefix named one.
    pub fn default_fd(kind: RedirKind) -> libc::c_int {
        match kind {
            RedirKind::In => libc::STDIN_FILENO,
            RedirKind::Out | RedirKind::Append => libc::STDOUT_FILENO,
        }
    }
}

/// A pipeline stage: an expanded argv and its redirections (in source order;
/// for a given direction the last one wins, as in POSIX shells).
pub struct Cmd {
    pub words: Vec<String>,
    pub redirs: Vec<Redirection>,
    pub pipe_stderr: bool,
    /// A builtin or user function, which mesh runs itself instead of `exec`ing.
    /// It still gets its own forked process, so the stages of a pipeline run
    /// concurrently — see [`fork_in_shell`].
    pub in_shell: bool,
}

/// Running background jobs and foreground jobs suspended with Ctrl-Z.
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
    job_modes: Option<libc::termios>,
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
        if let Some(modes) = &job.job_modes {
            restore_terminal_modes(modes);
        }
        if signal_group(job.pgid, libc::SIGCONT, "fg").is_err() {
            reclaim_terminal(job.shell_modes.as_ref());
            return 1;
        }
        job.state = JobState::Running;
        let result = wait_outcomes(&mut job.outcomes);
        let stopped_modes = matches!(result, WaitResult::Stopped(_))
            .then(terminal_modes)
            .flatten();
        reclaim_terminal(job.shell_modes.as_ref());
        match result {
            WaitResult::Complete(status) => status,
            WaitResult::Stopped(status) => {
                job.job_modes = stopped_modes;
                job.state = JobState::Stopped;
                note!("[{}] Stopped {}", job.id, job.command);
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
        note!("[{}] Running {}", job.id, job.command);
        0
    }

    /// List the jobs. `reap` is false in a forked stage, which is not the parent
    /// of these pids: its `waitpid` would fail with `ECHILD` and report every
    /// running job as finished. Such a child lists the table it inherited.
    pub fn list(&mut self, args: &[String], reap: bool) -> u8 {
        if !args.is_empty() {
            note!("mesh: jobs: too many arguments");
            return 1;
        }
        if reap {
            self.reap();
        }
        let mut status = 0;
        for job in &self.jobs {
            let state = if job.state == JobState::Stopped {
                "Stopped"
            } else {
                "Running"
            };
            // A write failure is a failing status, not a panic: `jobs` can be
            // redirected now that builtins run in the shell.
            status |= crate::builtins::print_line(
                "jobs",
                &format!("[{}] {state} {}", job.id, job.command),
            );
        }
        status
    }

    /// Report jobs which completed since the preceding prompt and remove them.
    pub fn reap(&mut self) {
        let mut index = 0;
        while index < self.jobs.len() {
            if self.jobs[index].state == JobState::Running {
                match poll_outcomes(&mut self.jobs[index].outcomes) {
                    Some(WaitResult::Complete(status)) => {
                        let job = self.jobs.remove(index);
                        note!("[{}] Done ({status}) {}", job.id, job.command);
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
            note!("mesh: {name}: too many arguments");
            return None;
        }
        if self.jobs.is_empty() {
            note!("mesh: {name}: no current job");
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
                note!("mesh: {name}: {reference}: no such job");
                None
            }
        }
    }
}

impl Drop for JobTable {
    fn drop(&mut self) {
        for job in &self.jobs {
            let _ = signal_group(job.pgid, libc::SIGHUP, "exit");
            if job.state == JobState::Stopped {
                let _ = signal_group(job.pgid, libc::SIGCONT, "exit");
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
            pipe_stderr: false,
            in_shell: false,
        }],
        jobs,
        false,
        &mut |_, _, _| unreachable!("an external command is never an in-shell stage"),
    )
    .status
}

/// Run one in-shell stage — a builtin or a function — in a forked child.
///
/// A pipeline's stages have to run **concurrently**: an upstream stage that
/// writes more than the pipe buffer blocks until a downstream reader drains it,
/// so running a builtin to completion in the shell and handing its output on
/// would deadlock on anything larger than 64 KiB (and could never express `yes |
/// head`). Forking gives the stage its own process, exactly as an external
/// command gets, at the cost POSIX shells already pay: state a piped builtin
/// changes — a `cd`, an assignment — is confined to that child, as in bash.
///
/// Returns the child pid and, when this stage pipes onward, the read end for the
/// next stage.
#[allow(clippy::too_many_arguments)]
fn fork_in_shell(
    cmd: &Cmd,
    is_last: bool,
    incoming: NextIn,
    in_file: Option<File>,
    out_file: Option<File>,
    err_file: Option<File>,
    // `stdout_to_pipe` / `stderr_to_pipe`: did each stream resolve to this
    // stage's outgoing pipe? Passed in rather than re-derived from `pipe_stderr`,
    // so a duplication that put stderr on the pipe (`f 2>&1 | g`) reaches the
    // fork too.
    stdout_to_pipe: bool,
    stderr_to_pipe: bool,
    interactive: bool,
    background: bool,
    process_group: Option<libc::pid_t>,
    index: usize,
    jobs: &mut JobTable,
    run: &mut dyn FnMut(usize, &Cmd, &mut JobTable) -> u8,
) -> std::io::Result<(libc::pid_t, bool, Option<File>)> {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let redirects_stdout = background && stdout_is_redirected(cmd);

    // stdout: a redirection wins over the pipe to the next stage; the last stage
    // with neither inherits the shell's stdout.
    let mut piped_out = false;
    // The pipe is needed when either stream resolved to it — `2>&1 > f |` keeps
    // it for stderr alone, and `>&2 |` moves stdout off it so none is made.
    let wants_pipe = !is_last && !redirects_stdout && (stdout_to_pipe || stderr_to_pipe);
    let (pipe_write, read_end) = if wants_pipe {
        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        piped_out = true;
        // SAFETY: both descriptors come from a successful `pipe`.
        unsafe {
            (
                Some(File::from_raw_fd(fds[1])),
                Some(File::from_raw_fd(fds[0])),
            )
        }
    } else {
        (None, None)
    };
    // Stderr takes its own handle on the pipe, so stdout can still go elsewhere.
    let child_err_pipe = match (&pipe_write, stderr_to_pipe) {
        (Some(write), true) => Some(write.try_clone()?),
        _ => None,
    };
    let child_out = match out_file {
        Some(file) => Some(file),
        None if stdout_to_pipe => pipe_write,
        None => None,
    };
    // stdin: a redirection wins over the incoming pipe; `Null` is the EOF case.
    let child_in = match (in_file, incoming) {
        (Some(file), _) => Some(file),
        (None, NextIn::Pipe(file)) => Some(file),
        (None, NextIn::Null) => Some(File::open("/dev/null")?),
        (None, NextIn::Inherit) => None,
    };

    // Anything buffered belongs to the parent; flushing first keeps the child
    // from inheriting it and printing a duplicate.
    let _ = std::io::stdout().flush();

    // SAFETY: fork has no arguments. The child touches only async-signal-safe
    // syscalls before running the stage, and leaves via `_exit` so no destructor
    // (notably `JobTable`'s, which signals jobs) runs twice.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if pid == 0 {
        // The child never reads from the pipe it writes to; holding the read end
        // open would keep the next stage from ever seeing EOF.
        drop(read_end);
        // A background stage's redirections are opened *here*, in the child, as
        // the external path defers them to its helper process — so a FIFO open
        // cannot block the shell before the job is registered.
        let deferred = if background && !cmd.redirs.is_empty() {
            match open_paths(&cmd.redirs)
                .and_then(|files| resolve_sources(&cmd.redirs, files, inherited_seed()))
                .and_then(sources_to_files)
            {
                Ok(files) => files,
                Err((path, err)) => {
                    note!("mesh: {path}: {err}");
                    // SAFETY: `_exit` ends the child without running the parent's
                    // destructors, exactly as the success path below does.
                    unsafe { libc::_exit(1) };
                }
            }
        } else {
            Vec::new()
        };
        unsafe {
            // Rust sets SIGPIPE to SIG_IGN at startup, so a write to a closed
            // pipe would return EPIPE here and the stage would report a failure
            // instead of dying quietly. `Command` restores the default for an
            // external child; do the same, so `f | head -3` ends the way the
            // pipefail rule assumes — killed by SIGPIPE, and not counted.
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            if (interactive || background) && !in_forked_stage() {
                libc::setpgid(0, process_group.unwrap_or(0));
            }
            if interactive {
                let _ = restore_job_signals();
                if !background {
                    set_foreground_group(libc::getpgrp());
                }
            }
            if let Some(file) = &child_in {
                libc::dup2(file.as_raw_fd(), libc::STDIN_FILENO);
            }
            if let Some(file) = &child_out {
                libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO);
            }
            if let Some(file) = &err_file {
                libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
            }
            if let Some(file) = &child_err_pipe {
                libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
            }
            // A background stage's own targets, per descriptor, before the `|&`
            // copy below — a `> out` must move stdout while `|&` can still see
            // where it went.
            for (fd, file) in &deferred {
                libc::dup2(file.as_raw_fd(), *fd);
            }
            // `|&` *is* `2>&1` appended after the command's redirections
            // (`DESIGN.md`), so it copies wherever stdout finally points and wins
            // over an explicit `2>`. Duplicating from the live descriptor rather
            // than from the pipe is what makes `f > out |& next` send both
            // streams to `out` whether or not the stage is backgrounded — adding
            // `&` must not change where the data goes.
            if cmd.pipe_stderr && !is_last {
                libc::dup2(libc::STDOUT_FILENO, libc::STDERR_FILENO);
            }
            // Only the duplicates above are the stage's descriptors; the originals
            // are a second open handle on the same pipe or file. `_exit` never
            // drops them, so everything this stage starts inherits them — and a
            // stray *write* end keeps the reader from ever seeing EOF: `func f() {
            // sleep 60 > /dev/null & }; f | cat` left `cat` waiting for the sleep
            // even though nothing was writing to it. Neither `libc::pipe` nor the
            // `dup2` targets set close-on-exec, so closing here is what covers a
            // nested child whether it execs or not.
            //
            // A descriptor already at 0/1/2 is the one being kept rather than a
            // copy of it: `dup2(n, n)` is a no-op, so closing it would take the
            // stage's own stream away.
            for fd in child_in
                .iter()
                .chain(child_out.iter())
                .chain(err_file.iter())
                .map(|file| file.as_raw_fd())
                .chain(deferred.iter().map(|(_, file)| file.as_raw_fd()))
            {
                if fd > libc::STDERR_FILENO {
                    libc::close(fd);
                }
            }
        }
        // From here on this process is not the interactive shell: it owns none of
        // the shell's jobs and must not take the terminal for anything it runs.
        mark_forked_stage();
        let code = run(index, cmd, jobs);
        let _ = std::io::stdout().flush();
        // SAFETY: `_exit` ends the child without unwinding or running atexit
        // handlers, which belong to the parent's copy of the shell.
        unsafe { libc::_exit(code as libc::c_int) };
    }
    // The parent must release the write end, or the reader never sees EOF.
    drop(child_out);
    drop(child_in);
    drop(child_err_pipe);
    drop(err_file);
    Ok((pid, piped_out, read_end))
}

/// How the next stage receives its stdin.
enum NextIn {
    /// The first stage with no `<` inherits the shell's stdin.
    Inherit,
    /// EOF (`/dev/null`): the previous stage sent its stdout elsewhere (a
    /// redirect) or failed to spawn, so there is no producer for this stage.
    Null,
    /// The previous stage's stdout, piped in. Held as a `File` rather than a
    /// `Stdio` because an in-shell stage runs in a fork and needs the raw
    /// descriptor to `dup2`; `Stdio` is one-way.
    Pipe(File),
}

/// A spawned stage awaiting its status, or a stage that failed before running.
enum Outcome {
    /// `piped_out` is true when this stage's stdout fed a downstream pipe (the
    /// only case where a SIGPIPE can legitimately come from a later stage
    /// closing the pipe).
    ///
    /// Identified by pid rather than `Child` so a stage mesh forked itself — a
    /// builtin or function — is waited on exactly like a spawned one.
    Running {
        pid: libc::pid_t,
        piped_out: bool,
    },
    Completed {
        code: u8,
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
/// Interactive foreground pipelines and all background pipelines get their own
/// process group. Non-interactive foreground pipelines stay in mesh's process
/// group so signals sent to the invoking group reach all stages.
pub fn run_pipeline(
    cmds: Vec<Cmd>,
    jobs: &mut JobTable,
    background: bool,
    run_in_shell: &mut dyn FnMut(usize, &Cmd, &mut JobTable) -> u8,
) -> PipelineStatus {
    let command_text = cmds
        .iter()
        .map(|cmd| cmd.words.join(" "))
        .collect::<Vec<_>>()
        .join(" | ");
    let n = cmds.len();
    let interactive = shell_stdin_is_terminal();
    // A forked stage runs the shell's code but owns no jobs: nested background
    // work stays in *this* stage's process group — the one the shell already
    // tracks — instead of starting a group only this copy of the table knows.
    let forked = in_forked_stage();
    let mut outcomes: Vec<Outcome> = Vec::new();
    // A background job must not consume commands from the shell's input.
    let mut next_stdin = initial_stdin(background, interactive);
    let mut process_group = None;
    let shell_modes = interactive.then(terminal_modes).flatten();

    // Open each stage's redirections concurrently — each stage still opens its
    // own in source order, but different stages open at the same time, so a FIFO
    // opened by one stage does not block a peer opened by another stage of the
    // same pipeline (`cat < fifo | cmd > fifo`) before the writer is spawned.
    let opened = if background {
        (0..n).map(|_| Ok(Vec::new())).collect()
    } else {
        std::thread::scope(|scope| {
            let handles: Vec<_> = cmds
                .iter()
                .map(|cmd| scope.spawn(move || open_paths(&cmd.redirs)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Ok(Vec::new())))
                .collect::<Vec<_>>()
        })
    };

    for ((idx, cmd), redir_result) in cmds.into_iter().enumerate().zip(opened) {
        let is_last = idx + 1 == n;
        // Default the following stage to EOF; a successful piped spawn upgrades
        // it to the real pipe. So a redirected or failed stage leaves the next
        // one reading `/dev/null` rather than the shell's stdin.
        let incoming = std::mem::replace(&mut next_stdin, NextIn::Null);

        // Seed each descriptor with where it points *before* any redirection, so
        // a duplication copies the pipe or the terminal as it stands at that
        // point in the sequence.
        let seed = [
            match &incoming {
                NextIn::Inherit => Source::Inherit(libc::STDIN_FILENO),
                NextIn::Null => Source::Null,
                NextIn::Pipe(_) => Source::Pipe,
            },
            if is_last {
                Source::Inherit(libc::STDOUT_FILENO)
            } else {
                Source::Pipe
            },
            Source::Inherit(libc::STDERR_FILENO),
        ];
        let sources = match redir_result.and_then(|files| resolve_sources(&cmd.redirs, files, seed))
        {
            Ok(sources) => sources,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                outcomes.push(Outcome::Failed(1));
                continue;
            }
        };
        // A descriptor that resolved to this stage's outgoing pipe cannot become
        // a file here — the pipe is made below — so it is carried as intent.
        let stdout_to_pipe = matches!(sources[1], Source::Pipe);
        let stderr_to_pipe = matches!(sources[2], Source::Pipe);
        let files = match sources_to_files(sources) {
            Ok(files) => files,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                outcomes.push(Outcome::Failed(1));
                continue;
            }
        };
        let (mut in_file, mut out_file, mut err_file) = (None, None, None);
        for (fd, file) in files {
            match fd {
                libc::STDIN_FILENO => in_file = Some(file),
                libc::STDOUT_FILENO => out_file = Some(file),
                _ => err_file = Some(file),
            }
        }

        if cmd.in_shell {
            match fork_in_shell(
                &cmd,
                is_last,
                incoming,
                in_file,
                out_file,
                err_file,
                stdout_to_pipe,
                stderr_to_pipe,
                interactive,
                background,
                process_group,
                idx,
                jobs,
                run_in_shell,
            ) {
                Ok((pid, piped_out, read_end)) => {
                    if (interactive || background) && !forked {
                        let pgid = process_group.unwrap_or(pid);
                        process_group = Some(pgid);
                        // Repeat setpgid in the parent to close the race with the
                        // child's own call, exactly as the spawn path does.
                        // SAFETY: scalar arguments; `pid` came from a successful fork.
                        unsafe {
                            libc::setpgid(pid, pgid);
                        }
                    }
                    if let Some(read) = read_end {
                        next_stdin = NextIn::Pipe(read);
                    }
                    outcomes.push(Outcome::Running { pid, piped_out });
                }
                Err(err) => {
                    note!("mesh: {}: {err}", cmd.words[0]);
                    outcomes.push(Outcome::Failed(1));
                }
            }
            continue;
        }

        let mut command = if background && !cmd.redirs.is_empty() {
            match background_redirect_command(&cmd, cmd.pipe_stderr && !is_last) {
                Ok(command) => command,
                Err((path, err)) => {
                    note!("mesh: {path}: {err}");
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
            }
        } else {
            let mut command = Command::new(&cmd.words[0]);
            command.args(&cmd.words[1..]);
            command
        };
        if (interactive || background) && !forked {
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
                if background {
                    command.pre_exec(restore_job_signals);
                } else {
                    command.pre_exec(|| {
                        restore_job_signals()?;
                        set_foreground_group(libc::getpgrp());
                        Ok(())
                    });
                }
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
                    command.stdin(Stdio::from(prev));
                }
            }
        }

        // stdout: an output redirection wins over the pipe to the next stage;
        // otherwise pipe to the next stage; otherwise inherit (only the last).
        //
        // stdout is decided *first* because `|&` is `2>&1` appended after the
        // command's own redirections (`DESIGN.md`), so it copies wherever stdout
        // finally points: `> out |&` takes stderr to the file and leaves the next
        // stage empty, and `2> log |&` loses the log to the pipe. Deciding stderr
        // first — a combined pipe that `2>` then overrode — gave the opposite
        // answer in both cases, and disagreed with an in-shell stage, which
        // dup2s in this order.
        // Stderr ends on this stage's pipe alongside stdout either because `|&`
        // asked for it or because a duplication resolved it there while stdout was
        // still the pipe. `2>&1 > f` is the case that is *not* this: stderr took
        // the pipe and stdout then moved to a file, so only stderr keeps it.
        let merge_stderr = (cmd.pipe_stderr && !is_last) || (stderr_to_pipe && stdout_to_pipe);
        let stderr_alone_on_pipe = stderr_to_pipe && !stdout_to_pipe;
        // A background external defers its opens to the helper, so — exactly as in
        // `fork_in_shell` — the shell must read fd 1's fate from `cmd.redirs`
        // rather than from an opened file. Without this the stage takes the pipe
        // branch below and records `piped_out`, which excuses a SIGPIPE that the
        // foreground spelling reports.
        let defers_stdout = background && stdout_is_redirected(&cmd);
        let mut piped_out = false;
        let mut combined_pipe = None;
        let mut merged_stderr = None;
        if let Some(file) = out_file {
            if merge_stderr {
                match file.try_clone() {
                    Ok(clone) => merged_stderr = Some(clone),
                    Err(error) => {
                        note!("mesh: {}: {error}", cmd.words[0]);
                        outcomes.push(Outcome::Failed(1));
                        continue;
                    }
                }
            } else if stderr_alone_on_pipe && !is_last {
                // `2>&1 > f |`: stdout goes to the file, but stderr already took
                // the pipe, so the pipe still has to exist and feed the next stage.
                let mut fds = [0; 2];
                if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                    note!("mesh: pipe: {}", std::io::Error::last_os_error());
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
                merged_stderr = Some(unsafe { File::from_raw_fd(fds[1]) });
                combined_pipe = Some(unsafe { File::from_raw_fd(fds[0]) });
                piped_out = true;
            }
            command.stdout(file);
        } else if !is_last && !defers_stdout && stdout_to_pipe {
            if merge_stderr {
                // Own the pipe rather than letting `Stdio::piped()` make it, so
                // both descriptors can be given its write end.
                let mut fds = [0; 2];
                if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                    note!("mesh: pipe: {}", std::io::Error::last_os_error());
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
                let read = unsafe { File::from_raw_fd(fds[0]) };
                let write = unsafe { File::from_raw_fd(fds[1]) };
                match write.try_clone() {
                    Ok(clone) => merged_stderr = Some(clone),
                    Err(error) => {
                        note!("mesh: pipe: {error}");
                        outcomes.push(Outcome::Failed(1));
                        continue;
                    }
                }
                command.stdout(write);
                combined_pipe = Some(read);
            } else {
                command.stdout(Stdio::piped());
            }
            piped_out = true;
        }

        // stderr: `|&`'s copy of the final stdout wins over an explicit `2>`,
        // being appended after it; otherwise the explicit target applies.
        if let Some(file) = merged_stderr {
            command.stderr(file);
        } else if let Some(file) = err_file {
            command.stderr(file);
        }

        match command.spawn() {
            Ok(mut child) => {
                if (interactive || background) && !forked {
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
                if let Some(pipe) = combined_pipe {
                    next_stdin = NextIn::Pipe(pipe);
                } else if piped_out && let Some(out) = child.stdout.take() {
                    next_stdin = NextIn::Pipe(File::from(std::os::fd::OwnedFd::from(out)));
                }
                outcomes.push(Outcome::Running {
                    pid: child.id() as libc::pid_t,
                    piped_out,
                });
            }
            Err(err) => {
                // The child hook hands the terminal to the new process group
                // before exec. If the first stage cannot exec, no successful
                // child records that group for the normal reclaim path below.
                if interactive && !background && process_group.is_none() {
                    // SAFETY: getpgrp takes no arguments and cannot fail.
                    set_foreground_group(unsafe { libc::getpgrp() });
                }
                outcomes.push(Outcome::Failed(spawn_error_code(&cmd.words[0], &err)));
            }
        }
    }

    let foreground = (!background).then_some(process_group).flatten();
    if let Some(pgid) = foreground {
        set_foreground_group(pgid);
    }

    if background {
        // A forked stage does not register jobs: the table belongs to the shell,
        // and this copy of it dies with the stage. The children just started are
        // in the stage's own group, which is the group the shell tracks — so a
        // signal to the job reaches them for as long as the job lives. `&` means
        // "do not wait" inside a fork too, so the stage returns rather than
        // falling through to `wait_outcomes`; the job then completes while the
        // nested child runs on, exactly as it does in a POSIX shell.
        if forked {
            return PipelineStatus::whole(0);
        }
        if let Some(pgid) = process_group {
            let id = jobs.next_id;
            jobs.next_id += 1;
            note!("[{id}] {pgid}");
            jobs.jobs.push(Job {
                id,
                pgid,
                command: command_text,
                outcomes,
                shell_modes: None,
                job_modes: None,
                state: JobState::Running,
            });
            return PipelineStatus::whole(0);
        }
        let result = wait_outcomes(&mut outcomes);
        let stages = stage_codes(&outcomes);
        let (WaitResult::Complete(status) | WaitResult::Stopped(status)) = result;
        return PipelineStatus { status, stages };
    }

    // pipefail: the last stage to fail wins. A SIGPIPE is ignored only for a
    // stage whose stdout fed a pipe (a downstream stage could have closed it).
    let result = wait_outcomes(&mut outcomes);
    // Read before `outcomes` can move into a stopped job's record.
    let stages = stage_codes(&outcomes);
    let job_modes = matches!(result, WaitResult::Stopped(_))
        .then(terminal_modes)
        .flatten();
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
    let status = match result {
        WaitResult::Complete(status) => status,
        WaitResult::Stopped(status) => {
            if let Some(pgid) = foreground {
                let id = jobs.next_id;
                jobs.next_id += 1;
                note!("[{id}] Stopped {command_text}");
                jobs.jobs.push(Job {
                    id,
                    pgid,
                    command: command_text,
                    outcomes,
                    shell_modes,
                    job_modes,
                    state: JobState::Stopped,
                });
            }
            status
        }
    };
    PipelineStatus { status, stages }
}

fn initial_stdin(background: bool, interactive: bool) -> NextIn {
    if background && !interactive {
        NextIn::Null
    } else {
        NextIn::Inherit
    }
}

enum WaitResult {
    Complete(u8),
    Stopped(u8),
}

/// What a finished pipeline reports.
pub struct PipelineStatus {
    /// The pipefail status the shell adopts: the last stage that failed, with an
    /// upstream `SIGPIPE` forgiven, else 0.
    pub status: u8,
    /// One entry per stage, in pipeline order, as each actually exited — a
    /// forgiven `SIGPIPE` shows here as `141`, because the point of the list is
    /// to say what happened rather than to repeat `status`.
    pub stages: Vec<u8>,
}

impl PipelineStatus {
    /// A pipeline whose stages are not individually known — a backgrounded
    /// launch, where the shell reports only that starting it succeeded.
    fn whole(status: u8) -> Self {
        Self {
            status,
            stages: vec![status],
        }
    }
}

/// Each stage's own status, read after waiting. A stage still `Running` is one
/// that was backgrounded rather than waited for; it has produced no status yet,
/// so it reports the 0 that launching it did.
fn stage_codes(outcomes: &[Outcome]) -> Vec<u8> {
    outcomes
        .iter()
        .map(|outcome| match outcome {
            Outcome::Running { .. } => 0,
            Outcome::Completed { code, .. } | Outcome::Failed(code) => *code,
        })
        .collect()
}

fn wait_outcomes(outcomes: &mut [Outcome]) -> WaitResult {
    let mut status = 0;
    let mut stopped = None;
    for outcome in &mut *outcomes {
        let (code, piped_out, did_stop, completed) = match outcome {
            Outcome::Running { pid, piped_out } => {
                let (code, stopped) = wait_for_job(*pid).unwrap_or((1, false));
                (code, *piped_out, stopped, !stopped)
            }
            Outcome::Completed { code, piped_out } => (*code, *piped_out, false, false),
            Outcome::Failed(code) => (*code, false, false, false),
        };
        if completed {
            *outcome = Outcome::Completed { code, piped_out };
        }
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
    for outcome in &mut *outcomes {
        let Outcome::Running { pid, piped_out } = outcome else {
            continue;
        };
        let pid = *pid;
        let piped_out = *piped_out;
        let mut raw = 0;
        let result = unsafe { libc::waitpid(pid, &mut raw, libc::WNOHANG | libc::WUNTRACED) };
        if result == 0 {
            any_running = true;
        } else if result > 0 && libc::WIFSTOPPED(raw) {
            return Some(WaitResult::Stopped(128 + libc::WSTOPSIG(raw) as u8));
        } else if result > 0 {
            let code = wait_status(raw);
            *outcome = Outcome::Completed { code, piped_out };
            if code != 0 && !(piped_out && code == SIGPIPE_CODE) {
                status = code;
            }
        }
    }
    if !any_running {
        status = outcomes.iter().fold(0, |status, outcome| match outcome {
            Outcome::Completed { code, piped_out }
                if *code != 0 && !(*piped_out && *code == SIGPIPE_CODE) =>
            {
                *code
            }
            Outcome::Failed(code) if *code != 0 => *code,
            _ => status,
        });
    }
    (!any_running).then_some(WaitResult::Complete(status))
}

/// Open background redirects in the child, after `spawn` has returned control
/// to mesh. This keeps FIFO opens non-blocking for the shell without adding a
/// PATH-resolved wrapper executable.
/// Whether a stage's own redirections will take fd 1 off the pipe.
///
/// A **background** stage's targets are opened after the fork — in the forked
/// child, or in the re-executed helper — so the shell has no opened file to look
/// at even when one of them moves stdout. That matters beyond where the bytes go:
/// `piped_out` is what tells the wait logic a SIGPIPE was the ignorable "the
/// reader went away" case, so it has to describe where fd 1 *ends up*. With a
/// `> out` a SIGPIPE is a real failure, exactly as it is in the foreground.
///
/// The open mode is not consulted, only the descriptor: `1< file` replaces fd 1
/// just as `> file` does, and the foreground path likewise sorts opened targets by
/// `fd` alone. `resolve_fds` keeps the last redirection per descriptor, so any
/// redirection naming fd 1 takes it off the pipe.
fn stdout_is_redirected(cmd: &Cmd) -> bool {
    cmd.redirs
        .iter()
        .any(|redir| redir.fd == libc::STDOUT_FILENO)
}

fn background_redirect_command(
    cmd: &Cmd,
    merge_stderr: bool,
) -> Result<Command, (String, std::io::Error)> {
    let executable = std::env::current_exe().map_err(|err| (cmd.words[0].clone(), err))?;
    let mut command = Command::new(executable);
    command
        .arg("--mesh-background-redirect")
        // `|&` travels too: the helper opens the targets, so only it can apply the
        // implicit `2>&1` *after* them, which is where it belongs.
        .arg(if merge_stderr { "merge" } else { "plain" })
        .arg(cmd.redirs.len().to_string());
    // Each redirection travels as `KIND FD PATH`, so the descriptor survives the
    // hand-off to the helper as well as the direction does.
    // Each redirection travels as `KIND FD TARGET`, so the descriptor and whether
    // the target is a path or another descriptor both survive the hand-off.
    for Redirection { fd, kind, target } in &cmd.redirs {
        command.arg(match (kind, target) {
            // A heredoc body cannot cross as argv — it is arbitrary text, and the
            // helper would have to re-quote it. Backgrounding one is refused
            // earlier instead.
            (_, RedirTarget::Heredoc(_)) => "heredoc",
            (_, RedirTarget::Descriptor(_)) => "dup",
            (RedirKind::In, _) => "in",
            (RedirKind::Out, _) => "out",
            (RedirKind::Append, _) => "append",
        });
        command.arg(fd.to_string());
        command.arg(match target {
            RedirTarget::Path(path) => path.clone(),
            RedirTarget::Descriptor(from) => from.to_string(),
            RedirTarget::Heredoc(body) => body.clone(),
        });
    }
    command.args(&cmd.words);
    Ok(command)
}

/// Internal executable mode used to defer potentially blocking opens until
/// after the background child has completed `exec`.
pub fn run_background_redirect(args: Vec<String>) -> std::process::ExitCode {
    let merge_stderr = match args.first().map(String::as_str) {
        Some("merge") => true,
        Some("plain") => false,
        _ => return std::process::ExitCode::from(1),
    };
    let Some(count) = args.get(1).and_then(|arg| arg.parse::<usize>().ok()) else {
        return std::process::ExitCode::from(1);
    };
    let Some(words_start) = count.checked_mul(3).and_then(|count| count.checked_add(2)) else {
        return std::process::ExitCode::from(1);
    };
    if words_start >= args.len() {
        return std::process::ExitCode::from(1);
    }
    let mut redirs = Vec::new();
    for triple in args[2..words_start].chunks_exact(3) {
        let Ok(fd) = triple[1].parse::<libc::c_int>() else {
            return std::process::ExitCode::from(1);
        };
        let (kind, target) = match triple[0].as_str() {
            "in" => (RedirKind::In, RedirTarget::Path(triple[2].clone())),
            "out" => (RedirKind::Out, RedirTarget::Path(triple[2].clone())),
            "append" => (RedirKind::Append, RedirTarget::Path(triple[2].clone())),
            "dup" => {
                let Ok(from) = triple[2].parse::<libc::c_int>() else {
                    return std::process::ExitCode::from(1);
                };
                (RedirKind::Out, RedirTarget::Descriptor(from))
            }
            "heredoc" => (RedirKind::In, RedirTarget::Heredoc(triple[2].clone())),
            _ => return std::process::ExitCode::from(1),
        };
        redirs.push(Redirection { fd, kind, target });
    }
    let opened = match open_paths(&redirs)
        .and_then(|files| resolve_sources(&redirs, files, inherited_seed()))
        .and_then(sources_to_files)
    {
        Ok(files) => files,
        Err((path, err)) => {
            note!("mesh: {path}: {err}");
            return std::process::ExitCode::from(1);
        }
    };
    let words = &args[words_start..];
    let mut command = Command::new(&words[0]);
    command.args(&words[1..]);
    // `|&` is `2>&1` appended *after* the command's own redirections, so stderr
    // follows wherever stdout finally points and an explicit `2>` loses to it.
    // Taken before the targets are handed over, since `stdout` consumes its file.
    let merged_stderr = if merge_stderr {
        match opened.iter().find(|(fd, _)| *fd == libc::STDOUT_FILENO) {
            Some((_, file)) => match file.try_clone() {
                Ok(clone) => Some(Stdio::from(clone)),
                Err(err) => {
                    note!("mesh: {}: {err}", words[0]);
                    return std::process::ExitCode::from(1);
                }
            },
            // No `>` moved stdout, so it is still the pipe the shell connected.
            // Inheriting names it explicitly, which is what overrides an
            // explicit `2>` that this loop is about to apply.
            None => Some(Stdio::inherit()),
        }
    } else {
        None
    };
    for (fd, file) in opened {
        match fd {
            libc::STDIN_FILENO => command.stdin(file),
            libc::STDOUT_FILENO => command.stdout(file),
            _ => command.stderr(file),
        };
    }
    if let Some(stderr) = merged_stderr {
        command.stderr(stderr);
    }
    let err = command.exec();
    std::process::ExitCode::from(spawn_error_code(&words[0], &err))
}

fn signal_group(pgid: libc::pid_t, signal: libc::c_int, label: &str) -> Result<(), ()> {
    if unsafe { libc::kill(-pgid, signal) } < 0 {
        note!("mesh: {label}: {}", std::io::Error::last_os_error());
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
    (unsafe { libc::tcgetattr(terminal_fd(), modes.as_mut_ptr()) } == 0)
        .then(|| unsafe { modes.assume_init() })
}

fn restore_terminal_modes(modes: &libc::termios) {
    // SAFETY: `modes` came from tcgetattr for this terminal. Errors are best
    // effort here: command status must remain the foreground job's status.
    unsafe {
        libc::tcsetattr(terminal_fd(), libc::TCSADRAIN, modes);
    }
}

/// Wait for a child to exit, be signaled, or stop. `Child::wait` only reports
/// termination, which would leave mesh blocked after Ctrl-Z. Reporting a stop
/// now lets the shell reclaim the terminal; the job-table task will retain the
/// process and make it available to `fg` / `bg`.
fn wait_for_job(pid: libc::pid_t) -> std::io::Result<(u8, bool)> {
    loop {
        let mut status = 0;
        // SAFETY: `pid` is a live child PID and status points to writable
        // storage. WUNTRACED requests the state transition needed for Ctrl-Z.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
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
        libc::tcsetpgrp(terminal_fd(), pgid);
        libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
    }
}

/// Open every redirection in source order so each file's create/truncate side
/// effect and any error happens in order, as POSIX shells do (`> a > b` opens
/// both). Returns the final stdin/stdout target — the last redirection of each
/// direction wins. On the first failure, returns the offending path and error.
#[allow(clippy::type_complexity)]
/// Apply `redirs` to **this** process's stdin/stdout for the duration of `body`,
/// restoring the original descriptors afterward, and return `body`'s result.
///
/// An external command configures its redirections on the child it spawns, but an
/// in-shell function runs inside the shell itself, so there is no child to
/// configure: its `>`/`>>`/`<` have to be swapped onto the shell's own
/// descriptors around the call. Rust's buffered stdout is flushed on both sides
/// of the swap so output lands in the file it was written for.
///
/// Opening a target can fail (a missing input, an unwritable path) and so can the
/// descriptor swap itself (`EMFILE` near the descriptor limit). Either way the
/// error is returned with the path, every swap already made is rolled back, and
/// `body` does **not** run — it must never execute against a half-applied
/// redirection.
pub(crate) fn with_redirections<T>(
    redirs: &[Redirection],
    body: impl FnOnce() -> T,
) -> Result<T, (String, std::io::Error)> {
    use std::io::Write;

    let opened = sources_to_files(resolve_sources(
        redirs,
        open_paths(redirs)?,
        inherited_seed(),
    )?)?;
    // Anything already buffered belongs to the *previous* stdout.
    let _ = std::io::stdout().flush();

    // The path a descriptor resolved to, for an error message. `resolve_fds`
    // applies "last one wins" per descriptor, so match that here.
    let path_for = |fd: libc::c_int| {
        redirs
            .iter()
            .rev()
            .find(|redir| redir.fd == fd)
            .map(|redir| match &redir.target {
                RedirTarget::Path(path) => path.clone(),
                RedirTarget::Descriptor(from) => format!("&{from}"),
                RedirTarget::Heredoc(_) => "<<".to_owned(),
            })
            .unwrap_or_default()
    };

    let mut swapped: Vec<(libc::c_int, libc::c_int)> = Vec::new();
    let restore = |swapped: &mut Vec<(libc::c_int, libc::c_int)>| {
        // Restore in reverse so the descriptors return to their original state.
        for (saved, target) in swapped.drain(..).rev() {
            unsafe {
                libc::dup2(saved, target);
                libc::close(saved);
            }
        }
    };
    for (target, file) in &opened {
        match swap_descriptor(file, *target) {
            Ok(saved) => swapped.push((saved, *target)),
            Err(err) => {
                restore(&mut swapped);
                return Err((path_for(*target), err));
            }
        }
    }

    // While stdin is swapped to a file, fd 0 no longer says whether this is an
    // interactive session, so publish the saved descriptor for `shell_stdin`.
    let stdin_saved = swapped
        .iter()
        .find(|(_, target)| *target == libc::STDIN_FILENO)
        .map(|(saved, _)| *saved);
    let published = stdin_saved.filter(|_| SHELL_STDIN.load(Ordering::Relaxed) < 0);
    if let Some(saved) = published {
        SHELL_STDIN.store(saved, Ordering::Relaxed);
    }

    let result = body();

    if published.is_some() {
        SHELL_STDIN.store(-1, Ordering::Relaxed);
    }
    // Flush what the body wrote before restoring, or it would land on the
    // restored descriptor instead of the redirection target.
    let _ = std::io::stdout().flush();
    restore(&mut swapped);
    Ok(result)
}

/// `dup` `target` aside and point it at `file`, returning the saved descriptor.
fn swap_descriptor(file: &File, target: libc::c_int) -> Result<libc::c_int, std::io::Error> {
    use std::os::fd::AsRawFd;
    let saved = unsafe { libc::dup(target) };
    if saved < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::dup2(file.as_raw_fd(), target) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(saved) };
        return Err(error);
    }
    Ok(saved)
}

/// The shell's own stdin while an in-process redirection has fd 0 pointed at a
/// file, or `-1` when fd 0 *is* the shell's stdin. Only the outermost redirection
/// publishes here, so a nested one cannot mistake an outer redirect's file for the
/// session's terminal.
static SHELL_STDIN: AtomicI32 = AtomicI32::new(-1);

/// Is the shell session interactive — i.e. is *its* stdin a terminal?
///
/// Read this rather than fd 0 directly: a function running under `f < file` has
/// fd 0 temporarily pointed at that file, and treating the session as
/// non-interactive there would skip process-group setup and signal restoration,
/// leaving a child with mesh's ignored terminal signals (so Ctrl-C could not
/// reach it).
/// The descriptor that refers to the shell's controlling terminal.
///
/// Job control — reading/restoring terminal modes and handing the terminal to a
/// foreground process group — must use this rather than fd 0: under `f < file`
/// fd 0 is a regular file, and `tcsetpgrp`/`tcgetattr` on it fail, which would
/// leave mesh's own group in the foreground so Ctrl-C never reached the child.
fn terminal_fd() -> libc::c_int {
    let saved = SHELL_STDIN.load(Ordering::Relaxed);
    if saved >= 0 {
        saved
    } else {
        libc::STDIN_FILENO
    }
}

/// Set once in a forked in-shell stage. Such a child runs the shell's code but
/// is not the interactive shell: it owns no jobs and must not touch the
/// terminal.
static IN_FORKED_STAGE: AtomicBool = AtomicBool::new(false);

/// Mark this process as a forked stage, so nothing it runs afterwards performs
/// job control.
///
/// Without it a nested pipeline inherits the *shell's* answers — stdin is still
/// a terminal, this call is not itself backgrounded — and so builds a new
/// foreground process group and calls `tcsetpgrp`. In `func f() { sleep 5 }`
/// backgrounded with `f &`, that hands the terminal from mesh to the nested
/// `sleep`, and the prompt stops accepting input.
pub(crate) fn mark_forked_stage() {
    IN_FORKED_STAGE.store(true, Ordering::Relaxed);
}

/// Whether this process is a forked stage rather than the shell itself.
fn in_forked_stage() -> bool {
    IN_FORKED_STAGE.load(Ordering::Relaxed)
}

fn shell_stdin_is_terminal() -> bool {
    if IN_FORKED_STAGE.load(Ordering::Relaxed) {
        return false;
    }
    let saved = SHELL_STDIN.load(Ordering::Relaxed);
    if saved >= 0 {
        return unsafe { libc::isatty(saved) == 1 };
    }
    std::io::stdin().is_terminal()
}

/// Where a descriptor ends up pointing for one stage.
///
/// Duplication (`2>&1`) copies whatever the referenced descriptor holds *at that
/// point in the sequence*, so resolution walks the redirections in order and
/// carries this state per descriptor rather than reducing to "last file wins".
#[derive(Debug)]
pub(crate) enum Source {
    /// Inherit the shell's descriptor — remembering *which*, since stderr
    /// following an inherited stdout means the shell's fd 1, not fd 2.
    Inherit(libc::c_int),
    /// EOF: `/dev/null`, for a stage with no producer.
    Null,
    /// This stage's outgoing pipe.
    Pipe,
    /// An opened file.
    File(File),
}

impl Source {
    /// Copy this destination for a duplication. A file needs a second handle on
    /// the same open file description, which is what `dup2` would have given.
    fn duplicate(&self) -> std::io::Result<Source> {
        Ok(match self {
            Source::Inherit(fd) => Source::Inherit(*fd),
            Source::Null => Source::Null,
            Source::Pipe => Source::Pipe,
            Source::File(file) => Source::File(file.try_clone()?),
        })
    }

    /// The file this descriptor should be given, or `None` when it already
    /// points where it needs to.
    ///
    /// An `Inherit` of a *different* descriptor is the whole point of `2>&1`:
    /// `Stdio::inherit()` would hand the child its own fd 2, so the shell's fd 1
    /// has to be duplicated into an owned handle instead. Applied uniformly to
    /// all three descriptors, so `>&2` moves stdout just as `2>&1` moves stderr.
    fn into_file(self, fd: libc::c_int) -> std::io::Result<Option<File>> {
        Ok(match self {
            Source::File(file) => Some(file),
            Source::Inherit(from) if from != fd => Some(dup_shell_fd(from)?),
            _ => None,
        })
    }
}

/// The pre-redirection destinations for a context that simply inherits all three
/// — the background helper, and a background stage opening its own targets.
fn inherited_seed() -> [Source; 3] {
    [
        Source::Inherit(libc::STDIN_FILENO),
        Source::Inherit(libc::STDOUT_FILENO),
        Source::Inherit(libc::STDERR_FILENO),
    ]
}

/// Turn resolved sources into the files each descriptor should be given.
/// `Source::Pipe` yields nothing: the pipe is created by the caller that owns it.
fn sources_to_files(
    sources: [Source; 3],
) -> Result<Vec<(libc::c_int, File)>, (String, std::io::Error)> {
    let mut files = Vec::new();
    for (fd, source) in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .zip(sources)
    {
        let described = format!("&{fd}");
        if let Some(file) = source.into_file(fd).map_err(|e| (described, e))? {
            files.push((fd, file));
        }
    }
    Ok(files)
}

/// Spill a heredoc body into a temporary file, rewound and unlinked.
///
/// Unlinking immediately means the file has no name for anything else to reach,
/// and disappears when the last descriptor closes — so a command that outlives
/// the shell still reads its input, and nothing is left behind if it does not.
fn heredoc_file(body: &str) -> std::io::Result<File> {
    use std::io::{Seek, SeekFrom, Write};

    // The pid keeps concurrent shells apart and the counter keeps heredocs
    // within one shell apart, including the concurrent opens of one pipeline's
    // stages. An address would not: two empty bodies share one sentinel pointer,
    // so `cat << A | cat << B` with empty bodies raced for a single name.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let mut file = loop {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mesh-heredoc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => {
                // Unlink at once: nothing can reach it by name while it is in
                // use, and it disappears when the last descriptor closes.
                let _ = std::fs::remove_file(&path);
                break file;
            }
            // A leftover from an earlier shell with the same pid; take the next.
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    file.write_all(body.as_bytes())?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

/// Duplicate one of the shell's own descriptors into an owned `File`.
fn dup_shell_fd(fd: libc::c_int) -> std::io::Result<File> {
    // SAFETY: `fd` is one of the standard descriptors, and `dup` returns a fresh
    // owned descriptor that `File` takes responsibility for closing.
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(duplicated) })
}

/// Open every **path** target, in source order, leaving a hole where a
/// redirection duplicates a descriptor instead.
///
/// Opening is split from applying so stages can still open concurrently — a FIFO
/// opened by one stage must not block a peer — while the order-sensitive part
/// stays strictly sequential. Every target is opened even when a later
/// redirection supersedes it, so `> a > b` still truncates `a`, as in bash.
fn open_paths(redirs: &[Redirection]) -> Result<Vec<Option<File>>, (String, std::io::Error)> {
    let mut opened = Vec::with_capacity(redirs.len());
    for Redirection { kind, target, .. } in redirs {
        opened.push(match target {
            RedirTarget::Descriptor(_) => None,
            RedirTarget::Heredoc(body) => {
                Some(heredoc_file(body).map_err(|e| ("<<".to_owned(), e))?)
            }
            RedirTarget::Path(path) => {
                let file = match kind {
                    RedirKind::In => File::open(path),
                    RedirKind::Out => File::create(path),
                    RedirKind::Append => OpenOptions::new().create(true).append(true).open(path),
                };
                Some(file.map_err(|e| (path.clone(), e))?)
            }
        });
    }
    Ok(opened)
}

/// Resolve one stage's redirections into where stdin, stdout, and stderr end up,
/// applying them **in source order** so `> out 2>&1` and `2>&1 > out` differ the
/// way they do in every other shell: the first sends both to the file, the second
/// copies stdout's *original* destination onto stderr and only then moves stdout.
///
/// `seed` supplies the destinations before any redirection — the incoming pipe or
/// terminal for stdin, and the outgoing pipe for stdout when this is not the last
/// stage — which is what makes `f 2>&1 | g` put both streams into it.
fn resolve_sources(
    redirs: &[Redirection],
    opened: Vec<Option<File>>,
    seed: [Source; 3],
) -> Result<[Source; 3], (String, std::io::Error)> {
    let mut state = seed;
    for (redir, file) in redirs.iter().zip(opened) {
        state[redir.fd as usize] = match (&redir.target, file) {
            (RedirTarget::Path(_) | RedirTarget::Heredoc(_), Some(file)) => Source::File(file),
            (RedirTarget::Path(path), None) => {
                return Err((
                    path.clone(),
                    std::io::Error::other("redirection target was not opened"),
                ));
            }
            (RedirTarget::Heredoc(_), None) => {
                return Err((
                    "<<".to_owned(),
                    std::io::Error::other("heredoc body was not opened"),
                ));
            }
            (RedirTarget::Descriptor(from), _) => state[*from as usize]
                .duplicate()
                .map_err(|e| (format!("&{from}"), e))?,
        };
    }
    Ok(state)
}

/// Map a spawn error to a status and report it (`127` not-found, else `126`).
fn spawn_error_code(name: &str, err: &std::io::Error) -> u8 {
    match err.kind() {
        ErrorKind::NotFound => {
            note!("mesh: command not found: {name}");
            127
        }
        ErrorKind::PermissionDenied => {
            note!("mesh: permission denied: {name}");
            126
        }
        _ => {
            note!("mesh: {name}: {err}");
            126
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        JobTable, NextIn, RedirKind, RedirTarget, Redirection, initial_stdin, restore_job_signals,
        restore_terminal_modes, run, set_foreground_group, shell_stdin_is_terminal, terminal_fd,
        terminal_modes, with_redirections,
    };

    #[test]
    fn redirecting_stdin_keeps_the_session_interactive() {
        // `f < file` points fd 0 at a regular file for the duration of the call.
        // The *session* is still interactive, and `run_pipeline` must see that:
        // otherwise it skips process-group setup and signal restoration, leaving a
        // child with mesh's ignored terminal signals so Ctrl-C could not reach it.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid != 0 {
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0, "child assertions failed");
            return;
        }
        // Child: give fd 0 a *controlling* terminal, so the session reads as
        // interactive and job control (`tcsetpgrp`) is permitted.
        let mut master = -1;
        let mut slave = -1;
        let ok = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
            && unsafe { libc::setsid() } >= 0
            && unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } >= 0
            && unsafe { libc::dup2(slave, libc::STDIN_FILENO) } == libc::STDIN_FILENO
            && shell_stdin_is_terminal();

        let path = std::env::temp_dir().join(format!("mesh-redir-tty-{}", std::process::id()));
        let wrote = std::fs::write(&path, "input\n").is_ok();
        let redirs = [Redirection {
            fd: libc::STDIN_FILENO,
            kind: RedirKind::In,
            target: RedirTarget::Path(path.to_string_lossy().into_owned()),
        }];
        // Inside the redirection fd 0 is the file, but the session is still a TTY
        // and job control must still reach the *terminal*: reading its modes and
        // handing it to a process group both have to work, or a child would run
        // with mesh's ignored terminal signals and Ctrl-C could not stop it.
        let group = unsafe { libc::getpgrp() };
        let inside = with_redirections(&redirs, || {
            let fd_zero_is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
            let modes_readable = terminal_modes().is_some();
            set_foreground_group(group);
            let foreground = unsafe { libc::tcgetpgrp(terminal_fd()) };
            (
                shell_stdin_is_terminal(),
                fd_zero_is_tty,
                modes_readable,
                foreground,
            )
        });
        let _ = std::fs::remove_file(&path);
        // Session still interactive, fd 0 is not the tty, terminal modes readable,
        // and the terminal's foreground group is the one we just set.
        let inside_ok = matches!(inside, Ok((true, false, true, fg)) if fg == group);
        // ...and it is restored afterward.
        let after_ok = shell_stdin_is_terminal() && terminal_modes().is_some();

        // Exit without closing the PTY: closing the master of the controlling
        // terminal would SIGHUP this session before `_exit` reported the result.
        let _ = master;
        let _ = slave;
        unsafe { libc::_exit(i32::from(!(ok && wrote && inside_ok && after_ok))) };
    }

    #[test]
    fn interactive_background_jobs_keep_terminal_stdin() {
        assert!(matches!(initial_stdin(true, true), NextIn::Inherit));
        assert!(matches!(initial_stdin(true, false), NextIn::Null));
    }

    #[test]
    fn job_builtins_fail_cleanly_with_an_empty_table() {
        let mut jobs = JobTable::new();
        assert_eq!(jobs.foreground(&[]), 1);
        assert_eq!(jobs.background(&[]), 1);
        assert_eq!(jobs.list(&[], true), 0);
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
            || unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } < 0
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
