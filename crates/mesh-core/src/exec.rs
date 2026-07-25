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
    /// Job ids, most recently current first: `%+` is the head and `%-` the one
    /// behind it. A job moves to the head when it is registered, when it stops,
    /// and when `bg` starts it again — the events that make it the one you most
    /// likely mean.
    ///
    /// The **whole** order is kept rather than just those two, because the pair
    /// alone cannot survive the head leaving: promoting `%-` needs a third job
    /// to fill the slot it vacates, and once `bg` or a stop has moved something
    /// forward, registration order is no longer the answer. `bg 2`, `bg 1`,
    /// `bg 3` over four jobs leaves recency `3 1 2 4`, where reconstructing from
    /// the table gave job 4 rather than job 2.
    ///
    /// Ids rather than indices, so removing a job cannot silently repoint them.
    recency: Vec<usize>,
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
    /// Carries the `128 + signal` the stop reported, so `wait` can answer for a
    /// job that is already stopped when it is asked. The stop cannot be read
    /// back from the process later: `WUNTRACED` reports a stop once, and a
    /// second `waitpid` on a process that is still stopped simply blocks.
    Stopped(u8),
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
        // Noting a stop here rather than inside the walk, which holds the table
        // borrowed: a job found stopped becomes the current one, exactly as it
        // would have had `reap` been the one to notice.
        let mut newly_stopped = Vec::new();
        let info = self
            .jobs
            .iter_mut()
            .map(|job| {
                let mut status = None;
                if job.state == JobState::Running {
                    match poll_outcomes(&mut job.outcomes) {
                        Some(WaitResult::Complete(code)) => status = Some(code),
                        Some(WaitResult::Stopped(code)) => {
                            job.state = JobState::Stopped(code);
                            newly_stopped.push(job.id);
                        }
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
                        (None, JobState::Stopped(_)) => "stopped",
                    },
                    status,
                }
            })
            .collect();
        for id in newly_stopped {
            self.mark_current(id);
        }
        info
    }

    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 1,
            recency: Vec::new(),
        }
    }

    /// Resume a job in the foreground. With no operand, use the most recently
    /// registered job; explicit references accept `N` and `%N`.
    pub fn foreground(&mut self, args: &[String]) -> u8 {
        let Some(index) = self.resolve(args, "fg") else {
            return 1;
        };
        // A finished job has nothing left to continue, and signaling its empty
        // process group only fails with `ESRCH`. Hand back the status the record
        // carries — the reason a completed job is kept in the table at all — and
        // report it the way the prompt-time reap would have. Since every command
        // refreshes `$sh.jobs`, this is not a rare race: running anything at all
        // between the job finishing and the `fg` used to be enough to turn a
        // usable status into `mesh: fg: No such process`.
        if let Some(WaitResult::Complete(status)) = poll_outcomes(&mut self.jobs[index].outcomes) {
            let job = self.jobs.remove(index);
            self.forget(job.id);
            note!("[{}] Done ({status}) {}", job.id, job.command);
            return status;
        }
        // The job leaves the table only once the resume has actually taken.
        // Removing it first meant a `SIGCONT` that did not land dropped it on
        // the floor: gone from `jobs`, unreachable by a later `fg` or `wait`,
        // never reaped or reported — and with `%+` / `%-` still holding its id,
        // so both fell through to the one surviving job and aliased each other.
        let job = &self.jobs[index];
        let (pgid, shell_modes) = (job.pgid, job.shell_modes);
        set_foreground_group(pgid);
        if let Some(modes) = &job.job_modes {
            restore_terminal_modes(modes);
        }
        if signal_group(pgid, libc::SIGCONT, "fg").is_err() {
            reclaim_terminal(shell_modes.as_ref());
            return 1;
        }
        let mut job = self.jobs.remove(index);
        job.state = JobState::Running;
        let result = wait_outcomes(&mut job.outcomes);
        let stopped_modes = matches!(result, WaitResult::Stopped(_))
            .then(terminal_modes)
            .flatten();
        reclaim_terminal(job.shell_modes.as_ref());
        match result {
            WaitResult::Complete(status) => {
                self.forget(job.id);
                status
            }
            WaitResult::Stopped(status) => {
                job.job_modes = stopped_modes;
                job.state = JobState::Stopped(status);
                note!("[{}] Stopped {}", job.id, job.command);
                let id = job.id;
                self.jobs.push(job);
                self.mark_current(id);
                status
            }
        }
    }

    /// Wait for a job to finish and report its status, leaving it where it is.
    ///
    /// This is [`JobTable::foreground`] without the foreground: no `SIGCONT`,
    /// and the terminal stays with the shell, so a background job keeps running
    /// as a background job and only its status comes back. A job that already
    /// finished answers from the status its record carries, so waiting after the
    /// fact reports the same thing as waiting through it.
    ///
    /// The job leaves the table once it is waited for, which is what keeps the
    /// prompt from also announcing `[1] Done (0) …` for a status the caller has
    /// already been given.
    ///
    /// `interactive` decides whether Ctrl-C may abandon the wait. Only the
    /// interactive shell ignores SIGINT on its own account, so only there is an
    /// uninterruptible wait a trap the user cannot get out of. A non-interactive
    /// shell keeps whatever disposition it was given — including an ignore
    /// inherited from a batch parent, which is that parent saying interrupts are
    /// not to take effect here.
    pub fn wait(&mut self, args: &[String], interactive: bool) -> u8 {
        // Bare `wait` means "every child, with an aggregate status" in bash,
        // while `fg`'s no-operand default means "the most recent one". The two
        // read alike and differ in what they do, so the operand is required
        // until `DESIGN.md` settles the aggregate rather than guessing here.
        if args.is_empty() {
            note!("mesh: wait: expected a job");
            return 1;
        }
        let Some(index) = self.resolve(args, "wait") else {
            return 1;
        };
        // A stopped job does not finish on its own, so waiting for one would
        // block until something else continued it — from a prompt this shell is
        // no longer reading. Report the stop instead, as bash does.
        if let JobState::Stopped(status) = self.jobs[index].state {
            let job = &self.jobs[index];
            note!("[{}] Stopped {}", job.id, job.command);
            return status;
        }
        let on_interrupt = if interactive {
            OnInterrupt::Abandon
        } else {
            OnInterrupt::Resume
        };
        let result = wait_outcomes_until(&mut self.jobs[index].outcomes, on_interrupt);
        match result {
            Some(WaitResult::Complete(status)) => {
                let job = self.jobs.remove(index);
                self.forget(job.id);
                status
            }
            Some(WaitResult::Stopped(status)) => {
                let job = &mut self.jobs[index];
                job.state = JobState::Stopped(status);
                let id = job.id;
                note!("[{}] Stopped {}", job.id, job.command);
                self.mark_current(id);
                status
            }
            // Ctrl-C gives up on the wait, not on the job: it keeps running and
            // keeps its place in the table.
            None => INTERRUPT_CODE,
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
        let id = job.id;
        note!("[{}] Running {}", job.id, job.command);
        self.mark_current(id);
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
            let state = if matches!(job.state, JobState::Stopped(_)) {
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
                        self.forget(job.id);
                        note!("[{}] Done ({status}) {}", job.id, job.command);
                        continue;
                    }
                    Some(WaitResult::Stopped(code)) => {
                        self.jobs[index].state = JobState::Stopped(code);
                        let id = self.jobs[index].id;
                        self.mark_current(id);
                    }
                    None => {}
                }
            }
            index += 1;
        }
    }

    /// Make `id` the current job, pushing the one it displaces behind it.
    fn mark_current(&mut self, id: usize) {
        self.recency.retain(|&known| known != id);
        self.recency.insert(0, id);
    }

    /// Drop a job that has left the table, promoting everything behind it.
    fn forget(&mut self, id: usize) {
        self.recency.retain(|&known| known != id);
    }

    fn index_of(&self, id: usize) -> Option<usize> {
        self.jobs.iter().position(|job| job.id == id)
    }

    /// The job at `depth` in recency order — 0 for `%+`, 1 for `%-`.
    fn by_recency(&self, depth: usize) -> Option<usize> {
        self.recency.get(depth).and_then(|id| self.index_of(*id))
    }

    /// Where `%+` points, falling back to the newest job — a table with jobs in
    /// it always has a current one, even if nothing has marked it yet.
    fn current_index(&self) -> Option<usize> {
        self.by_recency(0)
            .or_else(|| self.jobs.len().checked_sub(1))
    }

    /// Find a job by reference. `fg` / `bg` / `wait` only ever take a job, so a
    /// **bare id** (`fg 2`) is unambiguous; `%` covers the rest — `%2` by id,
    /// `%%` / `%+` for the current job, `%-` for the one before it, and
    /// `%prefix` for the most recent command that starts with it.
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
            return self.current_index();
        };
        let found = match reference.strip_prefix('%') {
            Some("%" | "+") => self.current_index(),
            Some("-") => self.by_recency(1),
            Some(rest) if rest.starts_with('?') => {
                // `DESIGN.md` keeps `%?string` for the substring match and defers
                // it; saying so beats reporting it as a job that does not exist.
                note!(
                    "mesh: {name}: {reference}: matching a command by substring is not implemented"
                );
                return None;
            }
            // An id wins over a prefix, so `%1` stays job 1 rather than the most
            // recent command that happens to begin with "1".
            Some(rest) => match rest.parse::<usize>() {
                Ok(id) => self.index_of(id),
                // A bare `%` names nothing, and `starts_with("")` would match the
                // newest job rather than say so.
                Err(_) if rest.is_empty() => None,
                Err(_) => self
                    .jobs
                    .iter()
                    .rposition(|job| job.command.starts_with(rest)),
            },
            None => reference
                .parse::<usize>()
                .ok()
                .and_then(|id| self.index_of(id)),
        };
        match found {
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
            if matches!(job.state, JobState::Stopped(_)) {
                hangup_group(job.pgid, libc::SIGCONT);
            }
        }
    }
}

/// `128 + SIGPIPE(13)` — an upstream stage killed because a later stage closed
/// the pipe early. Under our pipefail rule this does not count as a failure.
const SIGPIPE_CODE: u8 = 128 + 13;

/// `128 + SIGINT(2)` — the status a wait reports when Ctrl-C ended the wait
/// rather than the job, matching what an interrupted command itself reports.
const INTERRUPT_CODE: u8 = 128 + 2;

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

/// What a forked stage does once its descriptors are in place.
enum StageBody<'a> {
    /// Replace the child with an external command.
    ///
    /// `execvp` rather than `Command::spawn`: `spawn` makes a private
    /// close-on-exec pipe for the child to report an `exec` failure on, it takes
    /// a low descriptor, and installing this stage's own descriptors overwrote
    /// it — `mesh_no_such_command 4> out` put Rust's binary error packet into
    /// `out` and exited 1 instead of reporting `command not found` and 127.
    /// `std` exposes no way to see or reserve that descriptor, and `pre_exec`
    /// runs after the pipe already exists. `execvp` sets `errno` instead, so the
    /// child reports 126/127 itself and no private pipe is involved.
    Exec(Program),
    /// A builtin or a function, run by this copy of the shell.
    ///
    /// A pipeline's stages have to run **concurrently**: an upstream stage that
    /// writes more than the pipe buffer blocks until a downstream reader drains
    /// it, so running a builtin to completion in the shell and handing its
    /// output on would deadlock on anything larger than 64 KiB (and could never
    /// express `yes | head`). Forking gives the stage its own process, exactly
    /// as an external command gets, at the cost POSIX shells already pay: state
    /// a piped builtin changes — a `cd`, an assignment — is confined to that
    /// child, as in bash.
    InShell {
        index: usize,
        jobs: &'a mut JobTable,
        run: &'a mut dyn FnMut(usize, &Cmd, &mut JobTable) -> u8,
    },
}

/// A command's path and argv as `execvp` wants them, converted **before** the
/// fork: `CString::new` allocates, and a forked child must not.
struct Program {
    /// Owns what `argv` points into. Never read directly.
    _words: Vec<std::ffi::CString>,
    /// `argv[0]` is the program, and the list is NULL-terminated.
    argv: Vec<*const libc::c_char>,
}

impl Program {
    /// Fails on an interior NUL, which `execvp` could not express — the same
    /// `InvalidInput` `Command::spawn` gave it — and on an empty `words`, which
    /// the caller already rules out but which would leave no `argv[0]` to pass.
    fn new(words: &[String]) -> std::io::Result<Self> {
        if words.is_empty() {
            return Err(std::io::Error::from(ErrorKind::InvalidInput));
        }
        let converted = words
            .iter()
            .map(|word| std::ffi::CString::new(word.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| std::io::Error::from(ErrorKind::InvalidInput))?;
        let mut argv: Vec<*const libc::c_char> =
            converted.iter().map(|word| word.as_ptr()).collect();
        argv.push(std::ptr::null());
        Ok(Self {
            _words: converted,
            argv,
        })
    }

    /// Replace this process with the command, or return why it could not.
    ///
    /// # Safety
    /// Only after `fork`, in the child: on success nothing of this process
    /// survives, and on failure the caller must `_exit` rather than return into
    /// the shell's copy of itself.
    unsafe fn exec(&self) -> std::io::Error {
        // SAFETY: `argv` holds at least the program and a NULL terminator, and
        // every pointer in it is owned by `_words`, which outlives this call.
        unsafe { libc::execvp(self.argv[0], self.argv.as_ptr()) };
        std::io::Error::last_os_error()
    }
}

/// Run one pipeline stage in a forked child, with every descriptor it takes
/// installed before its body starts.
///
/// Both stage kinds fork: an external command so that the child can `execvp`
/// itself and report its own failure, and a builtin or function so that the
/// stages of a pipeline run concurrently. What differs is only what runs after
/// the descriptors are in place — see [`StageBody`].
///
/// Returns the child pid and, when this stage pipes onward, the read end for the
/// next stage.
#[allow(clippy::too_many_arguments)]
fn fork_stage(
    cmd: &Cmd,
    is_last: bool,
    incoming: NextIn,
    in_file: Option<File>,
    out_file: Option<File>,
    mut err_file: Option<File>,
    // Descriptors above the standard three, carried separately only because the
    // three below are named individually; the child installs all of them
    // together.
    mut extra_files: Vec<(libc::c_int, File)>,
    // Descriptors that resolved to this stage's outgoing pipe but are not the
    // stdout or stderr that carry it. Each is given its own handle on the write
    // end below, once the pipe exists. Empty for a background stage, which makes
    // its own copies after the fork.
    extra_pipe_out: PipeCopies,
    // Did stdout resolve to this stage's outgoing pipe? Passed in rather than
    // re-derived, so a duplication that moved it (`f >&2 | g`) reaches the fork
    // too. Any *other* descriptor holding the pipe — stderr under `|&`
    // included — arrives in `extra_pipe_out`. For a background stage this is
    // whether *anything* ends up on the pipe: the child is handed it on fd 1,
    // the seed its own walk starts from, and moves it from there.
    stdout_to_pipe: bool,
    // Descriptors `n>&-` closed, applied after every file is installed.
    closed: Vec<libc::c_int>,
    interactive: bool,
    background: bool,
    process_group: Option<libc::pid_t>,
    body: StageBody<'_>,
) -> std::io::Result<(libc::pid_t, bool, Option<File>)> {
    use std::io::Write;

    // stdout: a redirection wins over the pipe to the next stage; the last stage
    // with neither inherits the shell's stdout.
    let mut piped_out = false;
    // The pipe is needed when either stream resolved to it — `2>&1 > f |` keeps
    // it for stderr alone, and `>&2 |` moves stdout off it so none is made.
    let wants_pipe = !is_last && (stdout_to_pipe || !extra_pipe_out.is_empty());
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

    // SAFETY: fork has no arguments. The child installs its descriptors with
    // async-signal-safe calls alone, and leaves via `_exit` so no destructor
    // (notably `JobTable`'s, which signals jobs) runs twice. What follows the
    // installs — a background stage's own opens, a stage body, a report of a
    // failed `execvp` — does allocate, which is the same bar a forked builtin
    // has always run under: mesh only forks from its execution loop, and the
    // threads that resolve a pipeline's redirections are joined before it.
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
            // instead of dying quietly. Restoring the default here is what makes
            // `f | head -3` end the way the pipefail rule assumes — killed by
            // SIGPIPE, and not counted.
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
            // overwrite a handle another still needs — the standard three
            // included, this being a fork rather than a spawn.
            // `install_descriptors` closes each original as it goes,
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
            // rather than in the shell — so a FIFO open cannot block the shell
            // before the job is registered. Forking ourselves is what makes that
            // possible for an external stage too: `Command::spawn` waits for the
            // child to reach `exec`, so a blocking open in a `pre_exec` hook
            // would block the shell after all, which is why these opens used to
            // need a re-executed helper process to happen in.
            //
            // Opened after the installs above, not before, because resolution
            // duplicates from the descriptors as they *now* stand: `|&` is a
            // `2>&1` in this list, and copying fd 1 has to copy the stage's
            // stdout rather than the shell's.
            //
            // The list to ask about emptiness is therefore the *staged* one, not
            // `cmd.redirs`: a bare `f |& g &` has no redirections of its own and
            // still has a duplication to apply, which asking `cmd.redirs` skipped
            // — stderr stayed on the shell's.
            let redirs = staged_redirs(cmd, is_last);
            if background && !redirs.is_empty() {
                let inherited = live_descriptors(&redirs);
                let mut closing = Vec::new();
                let deferred =
                    match resolve_redirs(&redirs, &inherited, inherited_seed(), Acquire::Now)
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
        let code = match body {
            StageBody::Exec(program) => {
                // SAFETY: this is the child of a successful fork, and the only
                // thing after a failed `execvp` is reporting it and `_exit`.
                let error = unsafe { program.exec() };
                // The report goes to *this stage's* stderr, wherever its
                // redirections put it, which is where bash puts it too:
                // `nosuchcmd 2> log` leaves nothing on the terminal.
                spawn_error_code(&cmd.words[0], &error)
            }
            StageBody::InShell { index, jobs, run } => {
                // From here on this process is not the interactive shell: it owns
                // none of the shell's jobs and must not take the terminal for
                // anything it runs.
                mark_forked_stage();
                run(index, cmd, jobs)
            }
        };
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
    /// The previous stage's stdout, piped in. Held as a `File` because every
    /// stage runs in a fork and installs the descriptor itself, with `dup2`.
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

    // Resolve each stage's redirections concurrently — each stage still walks
    // its own in source order, acquiring every descriptor as it reaches it, but
    // different stages walk at the same time, so a FIFO opened by one stage does
    // not block a peer opened by another stage of the same pipeline
    // (`cat < fifo | cmd > fifo`) before the writer is spawned.
    // Which descriptors the shell holds, asked **once, on this thread, before
    // any stage opens anything**. The resolving threads share one descriptor
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
    let seeds: Vec<Sources> = (0..n)
        .map(|idx| stage_seed(idx, n, background, interactive))
        .collect();
    // A background stage's targets are opened by the stage itself, after the
    // fork, so a blocking open cannot hold up the shell before the job is
    // registered. It is still resolved here — without opening anything — because
    // the shell has to wire the pipes before it forks.
    let acquire = if background {
        Acquire::Deferred
    } else {
        Acquire::Now
    };
    let resolved: Vec<Result<Sources, (String, std::io::Error)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = cmds
            .iter()
            .enumerate()
            .zip(seeds)
            .map(|((idx, cmd), seed)| {
                let redirs = staged_redirs(cmd, idx + 1 == n);
                let inherited = &inherited;
                scope.spawn(move || resolve_redirs(&redirs, inherited, seed, acquire))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    Err((
                        "redirection".to_owned(),
                        std::io::Error::other("resolving the redirections panicked"),
                    ))
                })
            })
            .collect::<Vec<_>>()
    });

    for ((idx, cmd), redir_result) in cmds.into_iter().enumerate().zip(resolved) {
        let is_last = idx + 1 == n;
        // Default the following stage to EOF; a successful piped spawn upgrades
        // it to the real pipe. So a redirected or failed stage leaves the next
        // one reading `/dev/null` rather than the shell's stdin.
        let incoming = std::mem::replace(&mut next_stdin, NextIn::Null);

        let mut sources = match redir_result {
            // A background stage applies its own redirections after the fork and
            // reports its own failure from there, where the redirection actually
            // happens — so `&` still means "started", as it does in bash.
            //
            // The plan is dropped rather than kept up to the failure because the
            // stage never reaches its command: nothing is written to a pipe the
            // earlier redirections would have claimed. What bash *does* put
            // there is the diagnostic itself — `2>&1 4>&9 | cat` pipes `Bad file
            // descriptor` — and mesh does not, in the foreground either, because
            // it resolves a stage completely before installing any of it. That
            // is a standing divergence with a cost to closing; it is written up
            // in TODO.md rather than papered over here.
            Err(_) if background => Sources::default(),
            Ok(sources) => sources,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                outcomes.push(Outcome::Failed(1));
                continue;
            }
        };
        // The seed assumed a pipe from the stage before this one, which is only
        // now known.
        if !matches!(incoming, NextIn::Pipe(_)) {
            sources.no_incoming_pipe();
        }
        // A descriptor that resolved to this stage's outgoing pipe cannot become
        // a file here — the pipe is made below — so it is carried as intent.
        let mut stdout_to_pipe = sources.is_pipe(libc::STDOUT_FILENO);
        // Descriptors that copied a pipe rather than the standard slot that
        // carries it — `3>&1 | g`, `f | g 3<&0`. No file can stand for a pipe,
        // so each is handed its own handle on the real one below.
        let mut extra_pipe_out = PipeCopies::default();
        // The `n>&-` closes, which for a background stage the shell must *not*
        // apply: the child closes for itself, after the walk it redoes — and
        // that walk reads the descriptors the shell handed it.
        let mut closed: Vec<libc::c_int> = Vec::new();
        let (mut in_file, mut out_file, mut err_file) = (None, None, None);
        // Anything above the standard three is carried separately, only because
        // the three below are named individually.
        let mut extra_files: Vec<(libc::c_int, File)> = Vec::new();
        if background {
            // The stage redoes this walk for itself, from the descriptors the
            // shell hands it, so what it is handed is the *seed*: the incoming
            // pipe on fd 0 and the outgoing one on fd 1, wherever its own walk
            // goes on to move them. All the shell takes from the resolution is
            // whether the pipe is wanted at all — `> file |` leaves nothing on
            // it, while `3>&1 > file 1>&3 |` and `2>&1 > file |` both do.
            stdout_to_pipe = sources.holds_pipe_out();
        } else {
            extra_pipe_out = sources.extra_pipe_out();
            closed = sources.closed();
            let extra_pipe_in = sources.extra_pipe_in();
            let files = match sources_to_files(sources) {
                Ok(files) => files,
                Err((path, err)) => {
                    note!("mesh: {path}: {err}");
                    outcomes.push(Outcome::Failed(1));
                    continue;
                }
            };
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
        }

        let body = if cmd.in_shell {
            StageBody::InShell {
                index: idx,
                jobs: &mut *jobs,
                run: &mut *run_in_shell,
            }
        } else {
            match Program::new(&cmd.words) {
                Ok(program) => StageBody::Exec(program),
                Err(error) => {
                    outcomes.push(Outcome::Failed(spawn_error_code(&cmd.words[0], &error)));
                    continue;
                }
            }
        };
        match fork_stage(
            &cmd,
            is_last,
            incoming,
            in_file,
            out_file,
            err_file,
            extra_files,
            extra_pipe_out,
            stdout_to_pipe,
            closed,
            interactive,
            background,
            process_group,
            body,
        ) {
            Ok((pid, piped_out, read_end)) => {
                if (interactive || background) && !forked {
                    let pgid = process_group.unwrap_or(pid);
                    process_group = Some(pgid);
                    // Repeat setpgid in the parent to close the race with the
                    // child's own call: whichever runs first, the group is set
                    // before the stage can be signalled.
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
            jobs.mark_current(id);
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
                    state: JobState::Stopped(status),
                });
                jobs.mark_current(id);
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

/// Where stage `idx` of an `n`-stage pipeline points *before* its own
/// redirections, so a duplication copies the pipe or the terminal as it stands
/// at that point in the sequence.
///
/// Built for every stage up front, because resolution runs on the stage's own
/// thread and so cannot wait for the stage before it to be spawned. That is why
/// stdin is the incoming pipe for every stage but the first even though the one
/// before it may yet send its stdout elsewhere; [`Sources::no_incoming_pipe`]
/// corrects the guess once the answer is known.
fn stage_seed(idx: usize, n: usize, background: bool, interactive: bool) -> Sources {
    let stdin = match (idx, initial_stdin(background, interactive)) {
        (0, NextIn::Null) => Source::Null,
        (0, _) => Source::Inherit(libc::STDIN_FILENO),
        _ => Source::PipeIn,
    };
    let stdout = if idx + 1 == n {
        Source::Inherit(libc::STDOUT_FILENO)
    } else {
        Source::PipeOut
    };
    [
        (libc::STDIN_FILENO, stdin),
        (libc::STDOUT_FILENO, stdout),
        (libc::STDERR_FILENO, Source::Inherit(libc::STDERR_FILENO)),
    ]
    .into_iter()
    .collect()
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

/// What a wait does about a SIGINT that arrives while it blocks.
#[derive(Clone, Copy, PartialEq)]
enum OnInterrupt {
    /// Keep waiting. The job holds the terminal, so the keystroke went to the
    /// job itself and the wait is about to end on its own.
    Resume,
    /// Stop waiting and leave the job running. A background job never receives
    /// the keystroke, so without this the shell has no way out of the wait.
    Abandon,
}

fn wait_outcomes(outcomes: &mut [Outcome]) -> WaitResult {
    wait_outcomes_until(outcomes, OnInterrupt::Resume).expect("a resuming wait is never abandoned")
}

/// Wait for every process behind a job. `None` is a wait abandoned by SIGINT
/// under [`OnInterrupt::Abandon`]; the processes it had not reached are still
/// running, and their outcomes still say so.
fn wait_outcomes_until(outcomes: &mut [Outcome], on_interrupt: OnInterrupt) -> Option<WaitResult> {
    let _catcher = (on_interrupt == OnInterrupt::Abandon)
        .then(SigintCatcher::install)
        .flatten();
    let mut status = 0;
    let mut stopped = None;
    for outcome in &mut *outcomes {
        let (code, piped_out, did_stop, completed) = match outcome {
            Outcome::Running { pid, piped_out } => {
                match wait_for_job(*pid, on_interrupt) {
                    Ok((code, stopped)) => (code, *piped_out, stopped, !stopped),
                    Err(err) if err.kind() == ErrorKind::Interrupted => return None,
                    // Anything else is a wait that cannot be completed, which
                    // the caller has to be able to move past.
                    Err(_) => (1, *piped_out, false, true),
                }
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
    Some(stopped.map_or(WaitResult::Complete(status), WaitResult::Stopped))
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

/// Fork, run `body` in the child, and wait for it — the machinery behind
/// `fork { … }`.
///
/// The isolation is the process boundary itself: a `cd`, an assignment, an
/// `export`, a umask change in the child cannot reach the parent, and neither can
/// an `exit`, which ends the child and becomes this call's status rather than
/// ending the shell.
///
/// The child leaves through `_exit` so no destructor runs a second time — notably
/// `JobTable`'s, which signals jobs on drop and would otherwise reach the parent's
/// jobs from a child that never owned them. Buffered output is flushed *before*
/// forking, since a copy of the buffer would otherwise be printed twice.
pub(crate) fn fork_and_wait(interactive: bool, body: impl FnOnce() -> u8) -> std::io::Result<u8> {
    use std::io::Write;

    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();

    // Job control belongs to the process that *owns* it. A `fork` inside an
    // already-forked stage — `func f() { fork { … } }` backgrounded with `f &` —
    // still reports an interactive session, but that process is not the shell:
    // taking the terminal there hands it to a background job's group, and once
    // that job exits the real shell no longer owns it and the prompt stops
    // reading. `in_forked_stage` is the existing answer to "am I the shell", so
    // it is asked here rather than trusted to every caller.
    let job_control = interactive && !in_forked_stage();
    let shell_modes = if job_control { terminal_modes() } else { None };

    // SAFETY: fork has no arguments. The child runs `body` and leaves via
    // `_exit`, so it never unwinds back through the parent's stack or runs a
    // destructor the parent still owns.
    //
    // Running the interpreter in the child is what a forked pipeline stage does
    // too, and it carries the same caveat: inside `$(…)` the capture readers are
    // live threads, and forking away from a thread that holds a lock strands it.
    // Latent rather than fixed — the readers own no Rust-level lock — and the fix
    // belongs to the capture side for every fork at once. See `TODO.md`.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if pid == 0 {
        // This process is no longer the interactive shell. It owns none of the
        // jobs in the `JobTable` it inherited — the pids in there are the
        // parent's children, not its own — so `jobs` must not `waitpid` on them
        // and `fg` must not hand them the terminal. The same mark a forked
        // pipeline stage sets, for the same reason.
        mark_forked_stage();
        // Job control, in the shape a forked pipeline stage uses. All of it is
        // gated on `interactive`, because a *noninteractive* mesh must pass its
        // caller's signal dispositions through untouched: a batch runner that
        // launches a script with `SIGINT` ignored means it to stay ignored, and
        // entering a `fork` block is not a reason to change the contract.
        if job_control {
            // SAFETY: scalar arguments; this is the child, so 0 means itself.
            unsafe { libc::setpgid(0, 0) };
            // An interactive shell ignores the terminal signals so Ctrl-C reaches
            // the foreground job rather than the shell. A child that goes on to
            // `exec` has these restored for it; this one stays in mesh code, so
            // without it `fork { loop { } }` ignores Ctrl-C and leaves the
            // session recoverable only by an external kill.
            let _ = restore_job_signals();
            // Its own group, holding the terminal: a stop or an interrupt from
            // the keyboard then reaches the subshell *and its descendants* as one
            // unit, which is what makes resuming it able to resume them.
            // SAFETY: getpgrp takes no arguments and cannot fail.
            set_foreground_group(unsafe { libc::getpgrp() });
        }
        let code = body();
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        // SAFETY: leaving without unwinding, as above.
        unsafe { libc::_exit(i32::from(code)) };
    }
    // A subshell has no job-table entry to be resumed from, so a stop would
    // otherwise strand it: the parent would return while the child sat stopped
    // forever, reachable only by an external `kill`. Until backgrounding is
    // wired into the table, continue it and keep waiting — Ctrl-Z on a subshell
    // does nothing rather than losing one.
    // Set from the parent as well as the child, so neither side races the other
    // into needing the group that is not there yet.
    if job_control {
        // SAFETY: scalar arguments naming this process's own child.
        unsafe { libc::setpgid(pid, pid) };
    }
    // The wait's *outcome* rather than its status, because a failure has to leave
    // by the same door: the child took the terminal, so returning early on an error
    // would leave the foreground group naming a process that is gone and the next
    // prompt unable to read. `ECHILD` makes that reachable rather than theoretical
    // — a mesh started with `SIGCHLD` ignored has its children auto-reaped, and the
    // wait then fails for a subshell that exited perfectly well. `JobTable::foreground`
    // already reclaims on every path out, including its error return; this matches it.
    let outcome = loop {
        // `Resume`: the subshell holds the terminal, so a Ctrl-C goes to it and
        // this wait has nothing to give up on — the same reading a foreground
        // job's wait takes.
        let (status, stopped) = match wait_for_job(pid, OnInterrupt::Resume) {
            Ok(waited) => waited,
            Err(error) => break Err(error),
        };
        if !stopped {
            break Ok(status);
        }
        note!("mesh: fork: a subshell cannot be suspended yet");
        // The whole **group**, not just the leader. A stop from the keyboard
        // reaches every process in the foreground group, so `fork { sleep 100 }`
        // stops the subshell *and* the sleep; continuing only the leader would
        // leave the sleep stopped with nothing left to resume it once the
        // subshell exited.
        // SAFETY: scalar arguments. Negating a pid names its process group,
        // which exists only when this fork made one.
        unsafe { libc::kill(if job_control { -pid } else { pid }, libc::SIGCONT) };
    };
    if job_control {
        reclaim_terminal(shell_modes.as_ref());
    }
    outcome
}

/// Wait for a child to exit, be signaled, or stop. `Child::wait` only reports
/// termination, which would leave mesh blocked after Ctrl-Z. Reporting a stop
/// now lets the shell reclaim the terminal; the job-table task will retain the
/// process and make it available to `fg` / `bg`.
///
/// `on_interrupt` decides what a SIGINT arriving mid-wait means: for a job that
/// holds the terminal the signal went to the job, so the wait resumes, while a
/// wait on a *background* job reports the interruption as `ErrorKind::Interrupted`
/// for the caller to give up on.
fn wait_for_job(pid: libc::pid_t, on_interrupt: OnInterrupt) -> std::io::Result<(u8, bool)> {
    loop {
        // Checked before blocking as well as after, since a SIGINT that lands
        // between installing the handler and entering `waitpid` sets the flag
        // and is then gone — there is no `EINTR` left for it to cause.
        if on_interrupt == OnInterrupt::Abandon && SIGINT_SEEN.swap(false, Ordering::SeqCst) {
            return Err(std::io::Error::from(ErrorKind::Interrupted));
        }
        let mut status = 0;
        // SAFETY: `pid` is a live child PID and status points to writable
        // storage. WUNTRACED requests the state transition needed for Ctrl-Z.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                // Some other signal (a resized window, say) also lands here, so
                // the flag rather than the `EINTR` is what says to give up.
                if on_interrupt == OnInterrupt::Abandon && SIGINT_SEEN.swap(false, Ordering::SeqCst)
                {
                    return Err(err);
                }
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

/// Set by [`SigintCatcher`]'s handler; read by the wait that installed it.
static SIGINT_SEEN: AtomicBool = AtomicBool::new(false);

/// The thread whose `waitpid` the catcher exists to cut short, as a
/// `pthread_t`. Zero when no wait is in progress.
static WAITING_THREAD: AtomicUsize = AtomicUsize::new(0);

extern "C" fn note_interrupt(signal: libc::c_int) {
    SIGINT_SEEN.store(true, Ordering::SeqCst);
    // Only the thread that runs the handler gets its `waitpid` cut short, and a
    // process-directed SIGINT goes to whichever thread does not block it. That
    // is not always the waiter: `$(…)` and `:capture` run their body with a
    // reader thread draining the pipe, so the signal can land there and set this
    // flag while the wait itself sleeps on. Forwarding it to the waiter makes
    // the `EINTR` happen where it is needed, whoever received the signal.
    let waiting = WAITING_THREAD.load(Ordering::SeqCst);
    if waiting == 0 {
        return;
    }
    // SAFETY: `waiting` is a `pthread_t` published by `install` for a thread
    // that is blocked in the wait it belongs to, and cleared before that thread
    // returns. `pthread_self`, `pthread_equal` and `pthread_kill` are all
    // async-signal-safe; the equality test is what stops the forward from
    // recurring once it arrives.
    unsafe {
        let waiter = waiting as libc::pthread_t;
        if libc::pthread_equal(libc::pthread_self(), waiter) == 0 {
            libc::pthread_kill(waiter, signal);
        }
    }
}

/// Make SIGINT interrupt a blocking wait, putting the previous disposition back
/// when the wait ends.
///
/// The interactive shell ignores SIGINT, which is the right answer while a
/// foreground job runs: that job holds the terminal, so the keystroke reaches
/// *it* and the shell has nothing to do. A background job never receives it, so
/// an ignored SIGINT would leave `wait` blocked in `waitpid` with no way out —
/// Ctrl-C would do nothing at all until the job finished on its own.
///
/// Installing a handler is not enough by itself. `signal(3)` sets `SA_RESTART`
/// here, which would resume the `waitpid` rather than fail it, so this goes
/// through `sigaction` with no flags.
struct SigintCatcher {
    previous: libc::sigaction,
}

impl SigintCatcher {
    /// `None` when SIGINT is not currently ignored. A non-interactive shell
    /// keeps the default disposition, under which Ctrl-C ends the shell the way
    /// it does during any other command; a wait is not the place to change that.
    fn install() -> Option<Self> {
        // SAFETY: a zeroed `sigaction`/`sigset_t` is the documented starting
        // point, and every field is written before the struct is used. The
        // handler only stores to an atomic, which is async-signal-safe.
        unsafe {
            // SIGINT is blocked across the swap so one arriving mid-swap is held
            // pending instead of meeting the ignore that is on its way out —
            // where it would be discarded, setting no flag and leaving the wait
            // blocked until the job finished on its own. Unblocking below
            // delivers anything held to the handler that is by then installed.
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut blocked);
            libc::sigaddset(&mut blocked, libc::SIGINT);
            let mut unblocked: libc::sigset_t = std::mem::zeroed();
            if libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut unblocked) != 0 {
                return None;
            }
            let restore_mask =
                || libc::pthread_sigmask(libc::SIG_SETMASK, &unblocked, std::ptr::null_mut());

            let mut previous: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGINT, std::ptr::null(), &mut previous) != 0
                || previous.sa_sigaction != libc::SIG_IGN
            {
                restore_mask();
                return None;
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction =
                note_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = 0;
            // Both published while still blocked, so unblocking cannot deliver a
            // SIGINT whose flag this then wipes, nor one the handler would find
            // no waiter to forward to.
            SIGINT_SEEN.store(false, Ordering::SeqCst);
            WAITING_THREAD.store(libc::pthread_self() as usize, Ordering::SeqCst);
            let installed = libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) == 0;
            if !installed {
                WAITING_THREAD.store(0, Ordering::SeqCst);
            }
            restore_mask();
            installed.then_some(Self { previous })
        }
    }
}

impl Drop for SigintCatcher {
    fn drop(&mut self) {
        // The waiter is cleared first: once the disposition is back to the
        // shell's own ignore there is nothing to forward, and a stale
        // `pthread_t` here would name a thread that has moved on.
        WAITING_THREAD.store(0, Ordering::SeqCst);
        // SAFETY: `previous` was filled in by `sigaction` for this same signal.
        unsafe {
            libc::sigaction(libc::SIGINT, &self.previous, std::ptr::null_mut());
        }
        SIGINT_SEEN.store(false, Ordering::SeqCst);
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
        // valid disposition. This runs in a forked child, before its body.
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

    let sources = resolve_redirs(
        redirs,
        // Distinct from `already_open` above: that is which *targets* the shell
        // holds, for the save; this is which descriptors a duplication may copy.
        &live_descriptors(redirs),
        inherited_seed(),
        Acquire::Now,
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
    /// Inherit the shell's descriptor — remembering *which*, since a `2>&1`
    /// that reaches it has to duplicate the shell's fd 1, not fd 2.
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
    /// A target this stage will open *for itself*, after the fork — a background
    /// stage's path or heredoc, or a copy of one. It stands for "somewhere that
    /// is not a pipe", which is all the shell needs from a resolution it makes
    /// without opening anything. See [`Acquire::Deferred`].
    Deferred,
}

/// Whether a walk takes the descriptors it resolves, or only says where they
/// would land.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Acquire {
    /// Open each path and perform each duplication as the walk reaches it.
    Now,
    /// Open nothing and duplicate nothing: a **background** stage applies its
    /// own redirections after the fork, so that a blocking open cannot hold up
    /// the shell before the job is registered. The shell still has to know where
    /// the stage's descriptors end up — whether fd 1 is still on the pipe after
    /// `3>&1 > file 1>&3`, and whether anything else took it — and reading that
    /// off `cmd.redirs` ("does any redirection name fd 1?") answered a different
    /// question, tripping on the `>` even though source order puts stdout back.
    Deferred,
}

impl Source {
    /// Copy this destination onto `fd` for a duplication, **taking the
    /// descriptor here** rather than at installation.
    ///
    /// A file gets a second handle on the same open file description, which is
    /// what `dup2` would have given; the shell's own descriptor is duplicated;
    /// an EOF stdin becomes a `/dev/null` of its own, since only stdin has a
    /// slot the caller fills with one. Each of those can fail — `EMFILE` at the
    /// descriptor limit — and failing *where the redirection stands* is the
    /// point: a later `>` must not have truncated by then.
    ///
    /// A pipe is the exception: it does not exist yet, so it stays intent and
    /// the caller hands out handles once it has made it. Under
    /// [`Acquire::Deferred`] nothing is taken at all — the copy is the stage's
    /// to make, once it is a process of its own.
    fn copy(&self, fd: libc::c_int, acquire: Acquire) -> std::io::Result<Source> {
        if acquire == Acquire::Deferred {
            return Ok(match self {
                Source::Inherit(from) => Source::Inherit(*from),
                Source::Null => Source::Null,
                Source::PipeOut => Source::PipeOut,
                Source::PipeIn => Source::PipeIn,
                Source::Closed => Source::Closed,
                Source::File(_) | Source::Deferred => Source::Deferred,
            });
        }
        Ok(match self {
            Source::File(file) => Source::File(file.try_clone()?),
            Source::Inherit(from) if *from != fd => Source::File(dup_shell_fd(*from)?),
            Source::Inherit(from) => Source::Inherit(*from),
            Source::Null if fd != libc::STDIN_FILENO => Source::File(File::open("/dev/null")?),
            Source::Null => Source::Null,
            Source::PipeOut => Source::PipeOut,
            Source::PipeIn => Source::PipeIn,
            // A copy of a closed descriptor is closed too — for the shell's own
            // fd of that number, which `resolve_redirs` lets through when it
            // holds one. A copy of a close this stage made is `EBADF` there.
            Source::Closed => Source::Closed,
            // Only a deferred walk produces one, and a deferred walk never
            // acquires.
            Source::Deferred => Source::Deferred,
        })
    }

    /// The file this descriptor should be given, or `None` when it already
    /// points where it needs to.
    ///
    /// [`Source::copy`] took every descriptor a duplication needed as the walk
    /// reached it, so what is left here is the seed's own destinations — which
    /// each already sit on the descriptor they name — and the pipes the caller
    /// makes for itself.
    fn into_file(self, fd: libc::c_int) -> std::io::Result<Option<File>> {
        Ok(match self {
            Source::File(file) => Some(file),
            // A descriptor that copied an EOF stdin needs a `/dev/null` of its
            // own: only stdin has a slot the caller fills with one, so anything
            // else would be left closed and read `EBADF` instead of end-of-file.
            // Reached when the stage before this one turned out to produce no
            // pipe after all — see [`Sources::no_incoming_pipe`].
            Source::Null if fd != libc::STDIN_FILENO => Some(File::open("/dev/null")?),
            // `Source::Deferred` is not among them: a deferred resolution is a
            // plan the *stage* carries out, and the shell never turns one into
            // files it would have to open.
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

    /// Whether *any* descriptor ends up on the stage's outgoing pipe, and so
    /// whether the pipe is wanted at all. `> file |` leaves nothing on it and
    /// the next stage reads end-of-file; `2>&1 > file |` leaves stderr.
    fn holds_pipe_out(&self) -> bool {
        self.0
            .iter()
            .any(|(_, source)| matches!(source, Source::PipeOut))
    }

    /// Descriptors holding the outgoing pipe *besides* stdout, which the caller
    /// wires as the stage's standard output. `3>&1 | g` puts fd 3 here: no file
    /// can stand for a pipe, so the caller hands it the write end.
    fn extra_pipe_out(&self) -> PipeCopies {
        self.extras(
            |source| matches!(source, Source::PipeOut),
            // Only stdout is excluded: the caller wires it as the stage's
            // standard output. Stderr on the pipe is just another descriptor
            // holding it.
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

    /// The stage before this one produced no pipe after all — it sent its stdout
    /// elsewhere, or failed to start — so every descriptor that copied the
    /// incoming pipe reads end-of-file instead.
    ///
    /// Resolution seeds stdin with the pipe because the seed is built *before*
    /// the previous stage is spawned, which is what lets every stage resolve on
    /// its own thread. This is where that guess is corrected.
    fn no_incoming_pipe(&mut self) {
        for (_, source) in &mut self.0 {
            if matches!(source, Source::PipeIn) {
                *source = Source::Null;
            }
        }
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
#[derive(Default)]
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
/// The shell keeps the write end in hand so that every descriptor which resolved
/// to the pipe can be given a handle on it.
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
/// Each destination is **acquired where the walk reaches it** — the descriptor
/// limit is checked, then the path is opened or the duplication performed — so
/// the source-order guarantee is structural rather than something each new
/// failure mode has to be taught. `> a > b` truncates both, `true 2>&7 >
/// existing` fails without emptying `existing`, and `true 3> foo 4>&3 >
/// existing` fails on the duplication it cannot afford instead of after the `>`
/// has already truncated. Opening every target up front and only then applying
/// the duplications let a duplication be proved well-formed, a later `>`
/// truncate, and only then turn out to be unaffordable.
///
/// `seed` supplies the destinations before any redirection — the incoming pipe or
/// terminal for stdin, and the outgoing pipe for stdout when this is not the last
/// stage — which is what makes `f 2>&1 | g` put both streams into it.
/// `inherited` is which descriptors the process held *before* the walk; the
/// caller asks, because an open takes the lowest free descriptor and a probe from
/// inside the walk would find the walk's own files.
fn resolve_redirs(
    redirs: &[Redirection],
    inherited: &[libc::c_int],
    seed: Sources,
    acquire: Acquire,
) -> Result<Sources, (String, std::io::Error)> {
    let limit = descriptor_limit();
    let mut state = seed;
    for Redirection { fd, kind, target } in redirs {
        // A descriptor the process could never hold is refused where it stands,
        // named in the error the way bash names it. Left to `dup2` it would
        // surface as `EBADF` against the *command*, from a hook that runs after
        // every open. A close needs no descriptor at all, so the limit has
        // nothing to say about it: `999999>&-` asks for something already true,
        // and bash accepts it.
        if *fd >= limit && !matches!(target, RedirTarget::Close) {
            return Err((
                format!("&{fd}"),
                std::io::Error::from_raw_os_error(libc::EBADF),
            ));
        }
        let resolved = match target {
            RedirTarget::Close => Source::Closed,
            RedirTarget::Descriptor(from) => {
                let named = |error| (format!("&{from}"), error);
                match state.get(*from) {
                    // Closed by an earlier `n>&-`, and gone. The walk's own
                    // answer settles it: the shell may still physically hold a
                    // descriptor of that number — from an enclosing `f 3> out`
                    // around a nested `3>&- 4>&3` — but source order says this
                    // stage took it away, so the copy is `EBADF`. Falling
                    // through to `inherited` there reached past the close to the
                    // enclosing redirection's file, and the command ran.
                    Some(Source::Closed) => {
                        return Err(named(std::io::Error::from_raw_os_error(libc::EBADF)));
                    }
                    Some(source) => source.copy(*fd, acquire).map_err(named)?,
                    // Not one this stage has touched, but one the shell holds —
                    // an enclosing `f 3> out` around a nested `4>&3`. The seed
                    // carries only the standard three, so without this a valid
                    // copy of a real descriptor reads as a copy of nothing.
                    None if inherited.contains(from) => {
                        Source::Inherit(*from).copy(*fd, acquire).map_err(named)?
                    }
                    // Duplicating a descriptor nothing has opened is `EBADF`,
                    // the same answer the kernel gives — a copy of nothing is
                    // not an inheritance of the shell's own fd of that number.
                    None => return Err(named(std::io::Error::from_raw_os_error(libc::EBADF))),
                }
            }
            RedirTarget::Heredoc(body) if acquire == Acquire::Now => {
                Source::File(heredoc_file(body).map_err(|e| ("<<".to_owned(), e))?)
            }
            RedirTarget::Path(path) if acquire == Acquire::Now => {
                let file = match kind {
                    RedirKind::In => File::open(path),
                    RedirKind::Out => File::create(path),
                    RedirKind::Append => OpenOptions::new().create(true).append(true).open(path),
                };
                Source::File(file.map_err(|e| (path.clone(), e))?)
            }
            RedirTarget::Heredoc(_) | RedirTarget::Path(_) => Source::Deferred,
        };
        state.set(*fd, resolved);
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
    // async-signal-safe, the bar a forked child sets.
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
        JobTable, NextIn, RedirKind, RedirTarget, Redirection, SigintCatcher, initial_stdin,
        restore_job_signals, restore_terminal_modes, run, set_foreground_group,
        shell_stdin_is_terminal, terminal_fd, terminal_modes, with_redirections,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A SIGINT delivered to *another* thread still cuts the waiter's wait short.
    ///
    /// A process-directed signal goes to whichever thread does not block it, and
    /// only the thread that runs the handler has its blocking call interrupted.
    /// `$(…)` and `:capture` run their body with a reader thread draining the
    /// pipe, so the signal can land there while the wait sleeps on — which is
    /// why the catcher forwards it to the thread that is actually waiting.
    #[test]
    fn an_interrupt_on_another_thread_reaches_the_waiter() {
        // The catcher only installs over the shell's own ignore, so this stands
        // that in. The disposition is process-wide, so it goes back at the end;
        // nothing else in this suite reads it outside a fork of its own.
        let mut original: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: scalar out-parameter for this signal, zeroed before the call.
        unsafe {
            libc::sigaction(libc::SIGINT, std::ptr::null(), &mut original);
            libc::signal(libc::SIGINT, libc::SIG_IGN);
        }

        static INSTALLED: AtomicBool = AtomicBool::new(false);
        static INTERRUPTED: AtomicBool = AtomicBool::new(false);
        let waiter = std::thread::spawn(|| {
            let catcher = SigintCatcher::install();
            INSTALLED.store(catcher.is_some(), Ordering::SeqCst);
            // A bounded sleep rather than a `pause`, so a forward that never
            // arrives ends the test instead of stranding this thread. Waking
            // early is the whole assertion: `nanosleep` fails with `EINTR` only
            // when a handler ran *here*.
            let nap = libc::timespec {
                tv_sec: 5,
                tv_nsec: 0,
            };
            // SAFETY: a valid timespec and a writable out-parameter.
            let slept = unsafe { libc::nanosleep(&nap, std::ptr::null_mut()) };
            INTERRUPTED.store(slept < 0, Ordering::SeqCst);
        });

        for _ in 0..500 {
            if INSTALLED.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Long enough for the waiter to be inside `nanosleep`: a signal arriving
        // before it gets there would prove nothing about interrupting a wait.
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Deliberately *this* thread, standing in for the capture reader that a
        // process-directed SIGINT can land on instead of the waiter.
        // SAFETY: signalling a live thread of this process.
        unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGINT) };

        waiter.join().expect("waiter thread panicked");
        let installed = INSTALLED.load(Ordering::SeqCst);
        let interrupted = INTERRUPTED.load(Ordering::SeqCst);
        // SAFETY: `original` was filled in by `sigaction` for this same signal.
        unsafe { libc::sigaction(libc::SIGINT, &original, std::ptr::null_mut()) };

        assert!(installed, "the catcher did not install over SIG_IGN");
        assert!(
            interrupted,
            "a SIGINT on another thread left the waiter sleeping"
        );
    }

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
