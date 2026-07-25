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
    /// `n>&-`: close the descriptor rather than pointing it anywhere.
    Close,
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
    /// `|&`, spelled out: `2>&1` appended after a stage's own redirections.
    ///
    /// `DESIGN.md` defines `|&` as exactly that, so it is represented as exactly
    /// that. Carried instead as a flag beside the redirections, it became a
    /// question every destination had to be asked about separately — "and where
    /// did stdout end up?" — and each new answer stdout could have needed
    /// another arm. Stdout resolving to the *incoming* pipe (`1<&0 |&`) was
    /// simply the arm nobody wrote, and `ERR` leaked to the shell's stderr.
    /// Resolved in source order like any other duplication, stderr follows
    /// wherever stdout went for free, and `> out |&` differs from `2> log |&`
    /// because the order says so rather than because a branch restates it.
    fn merge_stderr() -> Self {
        Self {
            fd: libc::STDERR_FILENO,
            kind: RedirKind::Out,
            target: RedirTarget::Descriptor(libc::STDOUT_FILENO),
        }
    }

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

/// One live job as `$sh.jobs` reports it — the shell's own view, without the
/// terminal modes and wait state the table keeps for its own use.
pub struct JobInfo {
    pub id: usize,
    /// The process group leader. For a single command that is the command's own
    /// pid; for a pipeline it is the leader, which is what a signal wants.
    pub pid: libc::pid_t,
    pub command: String,
    pub state: &'static str,
    /// The exit status, once the job has finished.
    pub status: Option<u8>,
}

impl JobTable {
    /// Whether any job is registered, so a caller can skip work when none is.
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The live jobs, in registration order — the order `$sh.jobs` preserves.
    ///
    /// Polls rather than reads, so a process that has already exited reports
    /// `done` with its status instead of a stale `running`. It deliberately does
    /// **not** remove the finished job the way [`JobTable::reap`] does: a
    /// completed job stays available to a later `fg`, and taking it out from
    /// under one merely because something read `$sh.jobs` would make an
    /// observation change behavior. Reaping still reports and removes it at its
    /// own time.
    pub fn info(&mut self) -> Vec<JobInfo> {
        self.jobs
            .iter_mut()
            .map(|job| {
                let mut status = None;
                if job.state == JobState::Running {
                    match poll_outcomes(&mut job.outcomes) {
                        Some(WaitResult::Complete(code)) => status = Some(code),
                        Some(WaitResult::Stopped(_)) => job.state = JobState::Stopped,
                        None => {}
                    }
                }
                JobInfo {
                    id: job.id,
                    pid: job.pgid,
                    command: job.command.clone(),
                    state: match (status, job.state) {
                        (Some(_), _) => "done",
                        (None, JobState::Running) => "running",
                        (None, JobState::Stopped) => "stopped",
                    },
                    status,
                }
            })
            .collect()
    }

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
            hangup_group(job.pgid, libc::SIGHUP);
            if job.state == JobState::Stopped {
                hangup_group(job.pgid, libc::SIGCONT);
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
    mut err_file: Option<File>,
    // Descriptors above the standard three, installed by the child itself since
    // there is no `Stdio` slot to hand them to.
    mut extra_files: Vec<(libc::c_int, File)>,
    // Descriptors that resolved to this stage's outgoing pipe but are not the
    // stdout or stderr that carry it. Each is given its own handle on the write
    // end below, once the pipe exists.
    extra_pipe_out: PipeCopies,
    // Did stdout resolve to this stage's outgoing pipe? Passed in rather than
    // re-derived, so a duplication that moved it (`f >&2 | g`) reaches the fork
    // too. Any *other* descriptor holding the pipe — stderr under `|&`
    // included — arrives in `extra_pipe_out`.
    stdout_to_pipe: bool,
    // Descriptors `n>&-` closed, applied after every file is installed.
    closed: Vec<libc::c_int>,
    interactive: bool,
    background: bool,
    process_group: Option<libc::pid_t>,
    index: usize,
    jobs: &mut JobTable,
    run: &mut dyn FnMut(usize, &Cmd, &mut JobTable) -> u8,
) -> std::io::Result<(libc::pid_t, bool, Option<File>)> {
    use std::io::Write;

    let redirects_stdout = background && stdout_is_redirected(cmd);

    // stdout: a redirection wins over the pipe to the next stage; the last stage
    // with neither inherits the shell's stdout.
    let mut piped_out = false;
    // The pipe is needed when either stream resolved to it — `2>&1 > f |` keeps
    // it for stderr alone, and `>&2 |` moves stdout off it so none is made.
    let wants_pipe =
        !is_last && !redirects_stdout && (stdout_to_pipe || !extra_pipe_out.is_empty());
    let (pipe_write, read_end) = if wants_pipe {
        let (read, write) = new_pipe()?;
        piped_out = true;
        (Some(write), Some(read))
    } else {
        (None, None)
    };
    // Descriptors holding the pipe without a slot of their own join the list the
    // child installs by hand.
    if let Some(write) = &pipe_write {
        extra_files.extend(extra_pipe_out.handles(write)?);
    }
    let mut child_out = match out_file {
        Some(file) => Some(file),
        None if stdout_to_pipe => pipe_write,
        None => None,
    };
    // stdin: a redirection wins over the incoming pipe; `Null` is the EOF case.
    let mut child_in = match (in_file, incoming) {
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
            // Every descriptor this stage takes, installed together so none can
            // overwrite a handle another still needs — the standard three have
            // no `Stdio` slot here either, this being a fork rather than a
            // spawn. `install_descriptors` closes each original as it goes,
            // which matters most for a stage that never `exec`s: a second handle
            // on the outgoing pipe would outlive it and keep a reader waiting.
            let mut installs: Vec<(libc::c_int, File)> = Vec::new();
            if let Some(file) = child_in.take() {
                installs.push((libc::STDIN_FILENO, file));
            }
            if let Some(file) = child_out.take() {
                installs.push((libc::STDOUT_FILENO, file));
            }
            if let Some(file) = err_file.take() {
                installs.push((libc::STDERR_FILENO, file));
            }
            installs.extend(extra_files);
            if install_descriptors(installs, &closed).is_err() {
                libc::_exit(1);
            }

            // A background stage's own targets are opened *here*, in the child,
            // as the external path defers them to its helper — so a FIFO open
            // cannot block the shell before the job is registered.
            //
            // Opened after the installs above, not before, because resolution
            // duplicates from the descriptors as they *now* stand: `|&` is a
            // `2>&1` in this list, and copying fd 1 has to copy the stage's
            // stdout rather than the shell's.
            if background && !cmd.redirs.is_empty() {
                let redirs = staged_redirs(cmd, is_last);
                let inherited = live_descriptors(&redirs);
                let mut closing = Vec::new();
                let deferred = match open_paths(&redirs, &inherited)
                    .and_then(|files| resolve_sources(&redirs, files, inherited_seed()))
                    .and_then(|sources| {
                        closing = sources.closed();
                        sources_to_files(sources)
                    }) {
                    Ok(files) => files,
                    Err((path, err)) => {
                        note!("mesh: {path}: {err}");
                        libc::_exit(1);
                    }
                };
                if install_descriptors(deferred, &closing).is_err() {
                    libc::_exit(1);
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
    drop(err_file);
    Ok((pid, piped_out, read_end))
}

/// One stage's redirections with `|&` spelled out as the `2>&1` it is.
fn staged_redirs(cmd: &Cmd, is_last: bool) -> Vec<Redirection> {
    let mut redirs = cmd.redirs.clone();
    if cmd.pipe_stderr && !is_last {
        redirs.push(Redirection::merge_stderr());
    }
    redirs
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
    // Which descriptors the shell holds, asked **once, on this thread, before
    // any stage opens anything**. The opening threads share one descriptor
    // table, so a probe inside them could see a sibling's freshly opened target
    // as inherited — and then `4>&3` in one stage would copy another stage's
    // `9> victim` instead of failing, non-deterministically.
    let inherited = live_descriptors(
        &cmds
            .iter()
            .enumerate()
            .flat_map(|(idx, cmd)| staged_redirs(cmd, idx + 1 == n))
            .collect::<Vec<_>>(),
    );
    let opened: Vec<Result<Opened, (String, std::io::Error)>> = if background {
        // A background stage opens nothing here: its targets are opened by the
        // helper (or by the forked child), so a blocking open cannot hold up the
        // shell before the job is registered.
        (0..n).map(|_| Ok(Opened::none())).collect()
    } else {
        std::thread::scope(|scope| {
            let handles: Vec<_> = cmds
                .iter()
                .enumerate()
                .map(|(idx, cmd)| {
                    let redirs = staged_redirs(cmd, idx + 1 == n);
                    let inherited = &inherited;
                    scope.spawn(move || open_paths(&redirs, inherited))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Ok(Opened::none())))
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
        let seed: Sources = [
            (
                libc::STDIN_FILENO,
                match &incoming {
                    NextIn::Inherit => Source::Inherit(libc::STDIN_FILENO),
                    NextIn::Null => Source::Null,
                    NextIn::Pipe(_) => Source::PipeIn,
                },
            ),
            (
                libc::STDOUT_FILENO,
                if is_last {
                    Source::Inherit(libc::STDOUT_FILENO)
                } else {
                    Source::PipeOut
                },
            ),
            (libc::STDERR_FILENO, Source::Inherit(libc::STDERR_FILENO)),
        ]
        .into_iter()
        .collect();
        let redirs = staged_redirs(&cmd, is_last);
        let sources = match redir_result.and_then(|files| resolve_sources(&redirs, files, seed)) {
            Ok(sources) => sources,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                outcomes.push(Outcome::Failed(1));
                continue;
            }
        };
        // A descriptor that resolved to this stage's outgoing pipe cannot become
        // a file here — the pipe is made below — so it is carried as intent.
        let stdout_to_pipe = sources.is_pipe(libc::STDOUT_FILENO);
        // Descriptors that copied a pipe rather than the standard slot that
        // carries it — `3>&1 | g`, `f | g 3<&0`. No file can stand for a pipe,
        // so each is handed its own handle on the real one below.
        let extra_pipe_out = sources.extra_pipe_out();
        let extra_pipe_in = sources.extra_pipe_in();
        let closed = sources.closed();
        let files = match sources_to_files(sources) {
            Ok(files) => files,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                outcomes.push(Outcome::Failed(1));
                continue;
            }
        };
        let (mut in_file, mut out_file, mut err_file) = (None, None, None);
        // Anything above the standard three is carried separately and installed
        // in the child, since only these have a `Stdio` slot to be handed to.
        let mut extra_files = Vec::new();
        for (fd, file) in files {
            match fd {
                libc::STDIN_FILENO => in_file = Some(file),
                libc::STDOUT_FILENO => out_file = Some(file),
                libc::STDERR_FILENO => err_file = Some(file),
                other => extra_files.push((other, file)),
            }
        }
        // The incoming pipe belongs to the stage before this one, so a
        // descriptor that copied it takes its own handle rather than a file.
        if !extra_pipe_in.is_empty()
            && let NextIn::Pipe(prev) = &incoming
        {
            match extra_pipe_in.handles(prev) {
                Ok(copies) => extra_files.extend(copies),
                Err(error) => {
                    note!("mesh: pipe: {error}");
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
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
                extra_files,
                extra_pipe_out,
                stdout_to_pipe,
                closed.clone(),
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
            match background_redirect_command(&cmd, &redirs) {
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
        // A background external defers its opens to the helper, so — exactly as in
        // `fork_in_shell` — the shell must read fd 1's fate from `cmd.redirs`
        // rather than from an opened file. Its stdout ends on that file, so a
        // `SIGPIPE` from the stage is real and `piped_out` must not excuse it,
        // the way it does for a stage that really writes to the pipe.
        let defers_stdout = background && stdout_is_redirected(&cmd);
        let mut piped_out = false;
        let mut combined_pipe = None;
        // The write end, kept in hand whenever more than stdout needs a handle
        // on the pipe. `Stdio::piped()` would make one this loop cannot reach.
        let mut pipe_write = None;
        // A descriptor above the standard slots claimed the pipe, so the pipe has
        // to exist even when stdout went elsewhere.
        let extras_on_pipe = !is_last && !extra_pipe_out.is_empty();
        if let Some(file) = out_file {
            if extras_on_pipe {
                // `2>&1 > f |`: stdout goes to the file, but another descriptor
                // already took the pipe, so it still has to exist and feed the
                // next stage.
                let (read, write) = match new_pipe() {
                    Ok(pair) => pair,
                    Err(error) => {
                        note!("mesh: pipe: {error}");
                        outcomes.push(Outcome::Failed(1));
                        continue;
                    }
                };
                pipe_write = Some(write);
                combined_pipe = Some(read);
                piped_out = true;
            }
            command.stdout(file);
        } else if !is_last && stdout_to_pipe {
            if extras_on_pipe || defers_stdout {
                // Own the pipe rather than letting `Stdio::piped()` make it, so
                // every descriptor that resolved to it can be given the write end.
                //
                // A deferred stage needs it for a different reason: the helper
                // resolves that stage's redirections itself, seeded with whatever
                // the shell handed it, so `3>&1 > file |` copies the pipe onto
                // fd 3 there only if fd 1 *is* the pipe when the helper starts.
                // Handing it `Stdio::piped()` would work, but the read end has to
                // reach the next stage even though `piped_out` stays false.
                let (read, write) = match new_pipe() {
                    Ok(pair) => pair,
                    Err(error) => {
                        note!("mesh: pipe: {error}");
                        outcomes.push(Outcome::Failed(1));
                        continue;
                    }
                };
                if extras_on_pipe {
                    match write.try_clone() {
                        Ok(clone) => pipe_write = Some(clone),
                        Err(error) => {
                            note!("mesh: pipe: {error}");
                            outcomes.push(Outcome::Failed(1));
                            continue;
                        }
                    }
                }
                command.stdout(write);
                combined_pipe = Some(read);
            } else {
                command.stdout(Stdio::piped());
            }
            piped_out = !defers_stdout;
        } else if extras_on_pipe {
            // Stdout moved off the pipe without becoming a file (`1<&0 3>&1 |`),
            // yet fd 3 still holds it and the next stage still has to be fed.
            match new_pipe() {
                Ok((read, write)) => {
                    pipe_write = Some(write);
                    combined_pipe = Some(read);
                    piped_out = true;
                }
                Err(error) => {
                    note!("mesh: pipe: {error}");
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
            }
        }
        if let Some(write) = &pipe_write {
            match extra_pipe_out.handles(write) {
                Ok(copies) => extra_files.extend(copies),
                Err(error) => {
                    note!("mesh: pipe: {error}");
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
            }
        }

        // stderr: whatever resolution said, `|&` included — it is an ordinary
        // `2>&1` in the list now, so nothing here has to know about it.
        if let Some(file) = err_file {
            command.stderr(file);
        }

        // Descriptors with no `Stdio` slot are installed by the child itself
        // between fork and exec, which is after the standard three are in place.
        // The files move into the closure, which is what keeps them open until
        // then; `dup2` clears `FD_CLOEXEC` on the descriptor it creates, so they
        // survive the exec that the originals would not.
        if !extra_files.is_empty() || !closed.is_empty() {
            // Taken on the first call so the hook owns the handles and can close
            // each original once it is copied. `pre_exec` runs once.
            let mut pending = Some(extra_files);
            let closing = closed.clone();
            // SAFETY: `install_descriptors` uses only `fcntl`, `dup2` and
            // `close`, all async-signal-safe, and allocates nothing — the bar
            // `pre_exec` sets, since the child may fork while another thread
            // holds the allocator.
            unsafe {
                command.pre_exec(move || {
                    if let Some(files) = pending.take() {
                        install_descriptors(files, &closing)?;
                    }
                    Ok(())
                });
            }
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
    redirs: &[Redirection],
) -> Result<Command, (String, std::io::Error)> {
    let executable = std::env::current_exe().map_err(|err| (cmd.words[0].clone(), err))?;
    let mut command = Command::new(executable);
    command
        .arg("--mesh-background-redirect")
        .arg(redirs.len().to_string());
    // Each redirection travels as `KIND FD PATH`, so the descriptor survives the
    // hand-off to the helper as well as the direction does.
    // Each redirection travels as `KIND FD TARGET`, so the descriptor and whether
    // the target is a path or another descriptor both survive the hand-off.
    // `|&` travels as the `2>&1` it is, one more entry in the list — the helper
    // opens the targets, so only it can apply that duplication *after* them,
    // which is where the spec puts it.
    for Redirection { fd, kind, target } in redirs {
        command.arg(match (kind, target) {
            // A heredoc body cannot cross as argv — it is arbitrary text, and the
            // helper would have to re-quote it. Backgrounding one is refused
            // earlier instead.
            (_, RedirTarget::Heredoc(_)) => "heredoc",
            (_, RedirTarget::Close) => "close",
            (_, RedirTarget::Descriptor(_)) => "dup",
            (RedirKind::In, _) => "in",
            (RedirKind::Out, _) => "out",
            (RedirKind::Append, _) => "append",
        });
        command.arg(fd.to_string());
        command.arg(match target {
            RedirTarget::Path(path) => path.clone(),
            RedirTarget::Close => "-".to_owned(),
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
    let Some(count) = args.first().and_then(|arg| arg.parse::<usize>().ok()) else {
        return std::process::ExitCode::from(1);
    };
    let Some(words_start) = count.checked_mul(3).and_then(|count| count.checked_add(1)) else {
        return std::process::ExitCode::from(1);
    };
    if words_start >= args.len() {
        return std::process::ExitCode::from(1);
    }
    let mut redirs = Vec::new();
    for triple in args[1..words_start].chunks_exact(3) {
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
            "close" => (RedirKind::Out, RedirTarget::Close),
            "heredoc" => (RedirKind::In, RedirTarget::Heredoc(triple[2].clone())),
            _ => return std::process::ExitCode::from(1),
        };
        redirs.push(Redirection { fd, kind, target });
    }
    let inherited = live_descriptors(&redirs);
    let mut closing = Vec::new();
    let opened = match open_paths(&redirs, &inherited)
        .and_then(|files| resolve_sources(&redirs, files, inherited_seed()))
        .and_then(|sources| {
            closing = sources.closed();
            sources_to_files(sources)
        }) {
        Ok(files) => files,
        Err((path, err)) => {
            note!("mesh: {path}: {err}");
            return std::process::ExitCode::from(1);
        }
    };
    let words = &args[words_start..];
    let mut command = Command::new(&words[0]);
    command.args(&words[1..]);
    // Anything past the standard three has no `Stdio` slot, so the child puts it
    // in place itself — the same split the pipeline path makes.
    let mut extra_files = Vec::new();
    for (fd, file) in opened {
        match fd {
            libc::STDIN_FILENO => {
                command.stdin(file);
            }
            libc::STDOUT_FILENO => {
                command.stdout(file);
            }
            libc::STDERR_FILENO => {
                command.stderr(file);
            }
            other => extra_files.push((other, file)),
        }
    }
    if !extra_files.is_empty() || !closing.is_empty() {
        let mut pending = Some(extra_files);
        // SAFETY: `install_descriptors` uses only async-signal-safe calls and
        // allocates nothing, the bar `pre_exec` sets.
        unsafe {
            command.pre_exec(move || {
                if let Some(files) = pending.take() {
                    install_descriptors(files, &closing)?;
                }
                Ok(())
            });
        }
    }
    let err = command.exec();
    std::process::ExitCode::from(spawn_error_code(&words[0], &err))
}

/// Hang up a job as the shell exits, where a group that is already gone is the
/// ordinary case rather than something to report. [`JobTable::info`] reaps a
/// finished job to answer `$sh.jobs` but deliberately leaves it in the table, so
/// by exit its last member can be long gone; a still-running one can also exit
/// between this call and the `kill`. Either way `ESRCH` means the hangup had
/// nothing left to do, which is success — bash exits silently over a finished
/// job too. Every other failure stays a diagnostic.
fn hangup_group(pgid: libc::pid_t, signal: libc::c_int) {
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return;
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::ESRCH) {
        note!("mesh: exit: {err}");
    }
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

    // Which targets the shell already holds, asked **before** anything is
    // opened. An open takes the lowest free descriptor, so asking afterwards
    // would find `3> file`'s own freshly opened file sitting on fd 3, save it as
    // if it had been there all along, and put it back on the way out — leaving
    // fd 3 open on the shell for good.
    let already_open: Vec<libc::c_int> = redirs
        .iter()
        .map(|redir| redir.fd)
        // SAFETY: `fcntl(F_GETFD)` only reads a descriptor's flags, and answers
        // whether it is open at all without disturbing it.
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .collect();

    let sources = resolve_sources(
        redirs,
        // Distinct from `already_open` above: that is which *targets* the shell
        // holds, for the save; this is which descriptors a duplication may copy.
        open_paths(redirs, &live_descriptors(redirs))?,
        inherited_seed(),
    )?;
    // `n>&-` closes one of the shell's own descriptors for the body's duration,
    // so it is saved and restored exactly as a redirected one is.
    let closed = sources.closed();
    let opened = sources_to_files(sources)?;
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
                RedirTarget::Close => "&-".to_owned(),
                RedirTarget::Heredoc(_) => "<<".to_owned(),
            })
            .unwrap_or_default()
    };

    let mut swapped: Vec<(libc::c_int, libc::c_int)> = Vec::new();
    let restore = |swapped: &mut Vec<(libc::c_int, libc::c_int)>| {
        // Restore in reverse so the descriptors return to their original state.
        for (saved, target) in swapped.drain(..).rev() {
            unsafe {
                // `-1` marks a descriptor this redirection created, so there is
                // nothing to put back — closing it is the restore.
                if saved < 0 {
                    libc::close(target);
                } else {
                    libc::dup2(saved, target);
                    libc::close(saved);
                }
            }
        }
    };
    // Save every target that is currently open **before** installing any of
    // them, and save it clear of every target — otherwise saving fd 4 can land
    // on fd 3, which the next redirection then overwrites, and the restore puts
    // back whatever replaced it. A target nothing has open needs no save at all,
    // which is what keeps `3> file` working with no free descriptor to spare.
    let clear_of_targets = opened
        .iter()
        .map(|(fd, _)| *fd)
        .max()
        .unwrap_or(libc::STDERR_FILENO)
        .max(libc::STDERR_FILENO)
        .saturating_add(1);
    for target in opened
        .iter()
        .map(|(fd, _)| *fd)
        .chain(closed.iter().copied())
    {
        let target = &target;
        if !already_open.contains(target) {
            // Nothing had it: the redirection creates it, so the restore closes
            // it rather than putting something back.
            swapped.push((-1, *target));
            continue;
        }
        // SAFETY: duplicating a descriptor this process owns onto a free one
        // above every target, so no later install can overwrite the copy.
        let saved = unsafe { libc::fcntl(*target, libc::F_DUPFD_CLOEXEC, clear_of_targets) };
        if saved < 0 {
            let err = std::io::Error::last_os_error();
            restore(&mut swapped);
            return Err((path_for(*target), err));
        }
        swapped.push((saved, *target));
    }
    // Installing is then the same ordered permutation a child does, so an open
    // that landed on another redirection's target is copied before it is
    // overwritten.
    let targets: Vec<libc::c_int> = opened.iter().map(|(fd, _)| *fd).collect();
    if let Err(err) = install_descriptors(opened, &closed) {
        let target = targets.first().copied().unwrap_or(libc::STDERR_FILENO);
        restore(&mut swapped);
        return Err((path_for(target), err));
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
    PipeOut,
    /// The pipe feeding this stage from the one before it. Distinct from
    /// `PipeOut` because a duplication can copy either onto any descriptor, and
    /// the two are different pipes: `f | g 3<&0 4>&1` wants both at once.
    PipeIn,
    /// An opened file.
    File(File),
    /// Closed by `n>&-`. Distinct from "absent": the descriptor was named, so a
    /// later duplication of it is `EBADF` rather than a copy of whatever the
    /// shell happens to hold, and the child must actively close it.
    Closed,
}

impl Source {
    /// Copy this destination for a duplication. A file needs a second handle on
    /// the same open file description, which is what `dup2` would have given.
    fn duplicate(&self) -> std::io::Result<Source> {
        Ok(match self {
            Source::Inherit(fd) => Source::Inherit(*fd),
            Source::Null => Source::Null,
            Source::PipeOut => Source::PipeOut,
            Source::PipeIn => Source::PipeIn,
            Source::Closed => Source::Closed,
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
            // A descriptor that copied an EOF stdin needs a `/dev/null` of its
            // own: only stdin has a slot the caller fills with one, so anything
            // else would be left closed and read `EBADF` instead of end-of-file.
            Source::Null if fd != libc::STDIN_FILENO => Some(File::open("/dev/null")?),
            _ => None,
        })
    }
}

/// Where each descriptor ends up, keyed by descriptor rather than positional.
///
/// The standard three are always present; a redirection may introduce others
/// (`3< file`), so this is a map and not a `[Source; 3]`. Kept as a sorted `Vec`
/// because it holds three entries in almost every command and a handful at most.
#[derive(Default)]
pub(crate) struct Sources(Vec<(libc::c_int, Source)>);

impl Sources {
    fn get(&self, fd: libc::c_int) -> Option<&Source> {
        self.0
            .iter()
            .find(|(candidate, _)| *candidate == fd)
            .map(|(_, source)| source)
    }

    fn set(&mut self, fd: libc::c_int, source: Source) {
        match self.0.iter_mut().find(|(candidate, _)| *candidate == fd) {
            Some((_, slot)) => *slot = source,
            None => {
                self.0.push((fd, source));
                self.0.sort_by_key(|(fd, _)| *fd);
            }
        }
    }

    /// Whether this descriptor's destination is the stage's outgoing pipe, which
    /// cannot become a file here because the caller makes the pipe.
    fn is_pipe(&self, fd: libc::c_int) -> bool {
        matches!(self.get(fd), Some(Source::PipeOut))
    }

    /// Descriptors holding the outgoing pipe *besides* stdout and stderr, which
    /// the caller wires through `Stdio` slots of their own. `3>&1 | g` puts fd 3
    /// here: no file can stand for it, so the caller hands it the write end.
    fn extra_pipe_out(&self) -> PipeCopies {
        self.extras(
            |source| matches!(source, Source::PipeOut),
            // Only stdout keeps a slot of its own: the caller needs the read end
            // back from `Stdio::piped()`. Stderr on the pipe is just another
            // descriptor holding it.
            &[libc::STDOUT_FILENO],
        )
    }

    /// Descriptors `n>&-` closed, which the caller closes once the files it does
    /// install are in place.
    fn closed(&self) -> Vec<libc::c_int> {
        self.0
            .iter()
            .filter(|(_, source)| matches!(source, Source::Closed))
            .map(|(fd, _)| *fd)
            .collect()
    }

    /// Descriptors holding the incoming pipe besides stdin, which the caller
    /// wires as the stage's standard input.
    fn extra_pipe_in(&self) -> PipeCopies {
        self.extras(
            |source| matches!(source, Source::PipeIn),
            &[libc::STDIN_FILENO],
        )
    }

    fn extras(&self, want: impl Fn(&Source) -> bool, wired: &[libc::c_int]) -> PipeCopies {
        PipeCopies(
            self.0
                .iter()
                .filter(|(fd, source)| want(source) && !wired.contains(fd))
                .map(|(fd, _)| *fd)
                .collect(),
        )
    }
}

/// Descriptors that resolved to a pipe the caller owns.
///
/// A pipe cannot become a file, so `sources_to_files` drops these and the caller
/// hands each one its own handle on the real pipe once it exists.
struct PipeCopies(Vec<libc::c_int>);

impl PipeCopies {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// One handle per descriptor. Where these land is not the caller's problem:
    /// `install_descriptors` orders the copies so none overwrites another.
    fn handles(&self, pipe: &File) -> std::io::Result<Vec<(libc::c_int, File)>> {
        self.0
            .iter()
            .map(|fd| Ok((*fd, pipe.try_clone()?)))
            .collect()
    }
}

impl FromIterator<(libc::c_int, Source)> for Sources {
    fn from_iter<T: IntoIterator<Item = (libc::c_int, Source)>>(entries: T) -> Self {
        let mut sources = Self::default();
        for (fd, source) in entries {
            sources.set(fd, source);
        }
        sources
    }
}

/// The pre-redirection destinations for a context that simply inherits the
/// standard three — the background helper, and a background stage opening its
/// own targets.
fn inherited_seed() -> Sources {
    [
        (libc::STDIN_FILENO, Source::Inherit(libc::STDIN_FILENO)),
        (libc::STDOUT_FILENO, Source::Inherit(libc::STDOUT_FILENO)),
        (libc::STDERR_FILENO, Source::Inherit(libc::STDERR_FILENO)),
    ]
    .into_iter()
    .collect()
}

/// Turn resolved sources into the files each descriptor should be given.
/// A pipe yields nothing: it is created by the caller that owns it.
fn sources_to_files(
    sources: Sources,
) -> Result<Vec<(libc::c_int, File)>, (String, std::io::Error)> {
    // Collisions between these — an open landing on another's target — are
    // handled at installation, by `install_descriptors`, rather than by moving
    // anything here.
    let mut files = Vec::new();
    for (fd, source) in sources.0 {
        if let Some(file) = source.into_file(fd).map_err(|e| (format!("&{fd}"), e))? {
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

/// One past the highest descriptor this process may hold, from `RLIMIT_NOFILE`.
///
/// `c_int::MAX` when the limit cannot be read or does not fit, which leaves the
/// kernel to refuse the descriptor as it would have anyway.
fn descriptor_limit() -> libc::c_int {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` fills the struct it is given and touches nothing else.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } < 0 {
        return libc::c_int::MAX;
    }
    libc::c_int::try_from(limit.rlim_cur).unwrap_or(libc::c_int::MAX)
}

/// Make a pipe as an owned read/write pair.
///
/// Used wherever the shell needs the write end in hand — to give more than one
/// descriptor a handle on it — rather than letting `Stdio::piped()` hide it.
fn new_pipe() -> std::io::Result<(File, File)> {
    let mut fds = [0; 2];
    // SAFETY: `pipe` fills the two-element array it is given.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors come from a successful `pipe`, and `File` takes
    // responsibility for closing them from here.
    let ends = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    // `pipe` does not set close-on-exec, and `dup2` clears it on the descriptor
    // it creates — so the copy a stage is given survives `exec` while these
    // originals do not. Without this the raw ends stay inheritable, and anything
    // the stage `exec`s passes them to its own detached children: `sh -c 'sleep 5
    // >/dev/null 2>/dev/null 3>/dev/null & echo hi' 3>&1 | cat` left `cat`
    // waiting five seconds on a write end nothing was writing to.
    for end in [&ends.0, &ends.1] {
        set_cloexec(end)?;
    }
    Ok(ends)
}

/// Mark a descriptor close-on-exec, matching what Rust sets on every file it
/// opens. `pipe2` would fold this into the creation, but it is not portable.
fn set_cloexec(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let raw = file.as_raw_fd();
    // SAFETY: both calls take a descriptor this process owns.
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFD);
        if flags < 0 || libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
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
///
/// A **duplication is validated as the walk reaches it**, so a bad one stops the
/// walk before any later target is created. `true 2>&7 > existing` must fail
/// without emptying `existing`, exactly as bash leaves it alone: redirections
/// apply in source order, and an order that has already destroyed a file by the
/// time it reports the failure is not that order. Validation needs no files, only
/// which descriptors *exist* at each point, so it costs nothing to do here.
fn open_paths(
    redirs: &[Redirection],
    inherited: &[libc::c_int],
) -> Result<Opened, (String, std::io::Error)> {
    // A descriptor the process could never hold is refused first, before any
    // target is created or truncated, and named in the error the way bash names
    // it. Left to `dup2` it would surface as `EBADF` against the *command*, from
    // a hook that runs after the opens.
    // A close needs no descriptor at all, so the limit has nothing to say about
    // it: `999999>&-` asks for something already true, and bash accepts it.
    let limit = descriptor_limit();
    for redir in redirs {
        if redir.fd >= limit && !matches!(redir.target, RedirTarget::Close) {
            return Err((
                format!("&{}", redir.fd),
                std::io::Error::from_raw_os_error(libc::EBADF),
            ));
        }
    }
    let mut live = inherited.to_vec();
    let mut opened = Vec::with_capacity(redirs.len());
    for Redirection { fd, kind, target } in redirs {
        opened.push(match target {
            RedirTarget::Descriptor(from) => {
                if !live.contains(from) {
                    return Err((
                        format!("&{from}"),
                        std::io::Error::from_raw_os_error(libc::EBADF),
                    ));
                }
                None
            }
            RedirTarget::Close => None,
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
        if matches!(target, RedirTarget::Close) {
            // `n>&-` takes the descriptor away, so a later `m>&n` is `EBADF`.
            live.retain(|live| live != fd);
        } else if !live.contains(fd) {
            // Whatever it was, this redirection gives `fd` a destination, so a
            // later duplication may copy it.
            live.push(*fd);
        }
    }
    Ok(Opened {
        files: opened,
        inherited: inherited.to_vec(),
    })
}

/// What one stage's opening pass produced: a file per path target, and the
/// descriptors the shell already held when the pass began.
struct Opened {
    files: Vec<Option<File>>,
    /// Carried rather than re-probed, because probing again after the opens
    /// would find this stage's own files on the low descriptors.
    inherited: Vec<libc::c_int>,
}

impl Opened {
    /// Nothing opened and nothing inherited — a stage that defers its opens.
    fn none() -> Self {
        Self {
            files: Vec::new(),
            inherited: Vec::new(),
        }
    }
}

/// The descriptors a duplication may copy before any redirection has been
/// applied: the standard three, plus any *other* descriptor this stage names
/// that the shell already holds.
///
/// The second part is what makes a nested redirection work — in
/// `func f() { sh -c '…' 4>&3 }` run as `f 3> out`, the outer redirection has put
/// fd 3 on the shell, so `4>&3` is a copy of something real rather than the
/// `EBADF` a bare "only 0, 1 and 2 are inherited" rule would give.
///
/// Asked **before** any of this stage's own targets are opened, since an open
/// takes the lowest free descriptor and would otherwise be mistaken for a
/// descriptor that had been there all along — `2>&3 3> log` would silently copy
/// its own `log` instead of failing, and only when the open happened to land on
/// fd 3.
fn live_descriptors(redirs: &[Redirection]) -> Vec<libc::c_int> {
    let mut live = vec![libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO];
    for candidate in redirs.iter().flat_map(|redir| match &redir.target {
        RedirTarget::Descriptor(from) => Some(*from),
        _ => None,
    }) {
        // SAFETY: `fcntl(F_GETFD)` only reads a descriptor's flags, and answers
        // whether it is open at all without disturbing it.
        if !live.contains(&candidate) && unsafe { libc::fcntl(candidate, libc::F_GETFD) } >= 0 {
            live.push(candidate);
        }
    }
    live
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
    opened: Opened,
    seed: Sources,
) -> Result<Sources, (String, std::io::Error)> {
    let Opened { files, inherited } = opened;
    let mut state = seed;
    for (redir, file) in redirs.iter().zip(files) {
        let resolved = match (&redir.target, file) {
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
            // Duplicating a descriptor nothing has opened is `EBADF`, the same
            // answer the kernel gives — a copy of nothing is not an inheritance
            // of the shell's own fd of that number.
            (RedirTarget::Close, _) => Source::Closed,
            (RedirTarget::Descriptor(from), _) => match state.get(*from) {
                Some(Source::Closed) | None if !inherited.contains(from) => {
                    return Err((
                        format!("&{from}"),
                        std::io::Error::from_raw_os_error(libc::EBADF),
                    ));
                }
                Some(source) => source.duplicate().map_err(|e| (format!("&{from}"), e))?,
                // Not one this stage has touched, but one the shell holds — an
                // enclosing `f 3> out` around a nested `4>&3`. The seed carries
                // only the standard three, so without this a valid copy of a
                // real descriptor reads as a copy of nothing.
                None if inherited.contains(from) => Source::Inherit(*from),
                None => {
                    return Err((
                        format!("&{from}"),
                        std::io::Error::from_raw_os_error(libc::EBADF),
                    ));
                }
            },
        };
        state.set(redir.fd, resolved);
    }
    Ok(state)
}

/// Put the descriptor `raw` on descriptor `fd`, so it survives a following `exec`.
///
/// Not simply `dup2`: handed the descriptor it already is, `dup2` returns success
/// without doing anything — pointedly without clearing `FD_CLOEXEC`, which Rust
/// sets on every file it opens — so the descriptor would close at `exec` and the
/// command would see `EBADF` on a redirection that looked applied. `3< file` hit
/// exactly that whenever the open landed on fd 3, while a higher one like `9<`
/// worked, `dup2` there really making a new descriptor.
fn install_descriptor(raw: libc::c_int, fd: libc::c_int) -> std::io::Result<()> {
    // SAFETY: both calls take a descriptor this process owns, and both are
    // async-signal-safe, which is the bar `pre_exec` sets.
    unsafe {
        if raw == fd {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        } else if libc::dup2(raw, fd) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Put every file on its descriptor, in an order where none is overwritten while
/// another still needs to read it, closing each original once it is copied.
///
/// An open takes the lowest free descriptor, so `4> four 3> three` lands `four`
/// on fd 3 — the descriptor the *other* redirection targets. Installing fd 3
/// first destroys `four` before anything copies it to fd 4, and the mirror
/// spelling collides the other way, so no fixed order works. A target is
/// therefore installed only once no **pending** handle still lives on it, which
/// drains every arrangement except a true cycle (`3>&4 4>&3`); a cycle is broken
/// by moving one handle aside, and the descriptor it moves to is free by
/// definition, so it cannot be another pending target.
///
/// Lifting every handle above the highest target would also be collision-safe and
/// is simpler, but it demands a free descriptor *above every target* — which
/// `RLIMIT_NOFILE` can refuse for a redirection the kernel would otherwise allow:
/// at `ulimit -n 4`, `3> file` has nowhere above fd 3 to go, while bash runs it.
///
/// Closing the original is not just tidiness. A stage that never `exec`s — a
/// forked builtin or function — would otherwise hold a second handle on the same
/// pipe for its whole life, and everything it starts inherits it, so a reader
/// waits on a write end nothing is writing to.
fn install_descriptors(
    mut files: Vec<(libc::c_int, File)>,
    closed: &[libc::c_int],
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    while !files.is_empty() {
        let ready = (0..files.len()).find(|&index| {
            let target = files[index].0;
            !files
                .iter()
                .enumerate()
                .any(|(other, (_, file))| other != index && file.as_raw_fd() == target)
        });
        let Some(index) = ready else {
            // Every remaining target is occupied by another pending handle.
            // SAFETY: `fcntl` duplicates a descriptor this process owns onto the
            // lowest free one, which no pending target can be — they are all
            // occupied, or one of them would have been ready.
            let moved = unsafe { libc::fcntl(files[0].1.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            if moved < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Assigning drops the old handle, which closes the descriptor the
            // cycle was waiting on.
            // SAFETY: `moved` is a fresh descriptor this process now owns.
            files[0].1 = unsafe { File::from_raw_fd(moved) };
            continue;
        };
        let (target, file) = files.swap_remove(index);
        let raw = file.as_raw_fd();
        install_descriptor(raw, target)?;
        if raw == target {
            // `install_descriptor` only cleared close-on-exec; this *is* the
            // stage's descriptor, so dropping the handle would close it.
            std::mem::forget(file);
        }
    }
    // `n>&-`, applied last: a descriptor closed earlier could still have been a
    // source another install had to read, and source order has already decided
    // that nothing later copies this one.
    for fd in closed {
        // SAFETY: `close` takes a descriptor this process owns and is
        // async-signal-safe. A descriptor that was not open is `EBADF`, which is
        // the same nothing the redirection asked for.
        unsafe { libc::close(*fd) };
    }
    Ok(())
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
