//! The read / tokenize / dispatch loop.
//!
//! Interactive (TTY) input goes through [`reedline`] for line editing, history,
//! and Ctrl-C/Ctrl-D handling. Piped / non-interactive input keeps the std-only
//! unbuffered fd-0 byte reader, so a spawned child still inherits any bytes that
//! follow its command line and the integration tests need no terminal.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, OnceLock, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use reedline::{
    Color, ColumnarMenu, Completer, EditCommand, Emacs, History, HistoryItem, HistoryItemId,
    HistorySessionId, KeyCode, KeyModifiers, Keybindings, MenuBuilder, Osc133Markers, Prompt,
    PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, SearchDirection,
    SearchQuery, Signal, SimpleMatchHighlighter, Span, SqliteBackedHistory, Suggestion,
    default_emacs_keybindings,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::builtins::{self, Builtin};
use crate::completion::{CompletionCache, CompletionSpec, ValueHint, rank_candidates};
use crate::expand::{Piece, VarRef, Word};
use crate::funcs::{FuncDef, Funcs};
use crate::vars::{self, RegexValue, Value, Vars};
use crate::{environ, exec, expand, parser};

const COMPLETION_MENU: &str = "completion_menu";

/// The mutable shell session threaded through the run loop: variable scopes,
/// defined functions, and the job table.
struct Shell {
    vars: Vars,
    funcs: Funcs,
    jobs: exec::JobTable,
    control: Option<parser::ControlKind>,
    /// True in a forked pipeline stage or background job: this process runs the
    /// shell's code but owns none of its children, so job control is not its to
    /// perform.
    forked: bool,
    loop_depth: usize,
    prompt: PromptConfig,
    /// The **result so far** of the body being run: the value of the last
    /// executable that produced one, or the status view of one that did not. A
    /// bare `return` carries this out (`DESIGN.md` §"Result and `return`").
    result: Value,
    /// What the executable that just ran did to `result`. Reported by execution
    /// rather than inferred from the AST: an `if` whose branch yielded a value
    /// has produced one, an expression that failed has not, and a statement a
    /// guard skipped produced nothing at all.
    produced: Produced,
    /// How many times `$sh.status` has been recorded. Compared across running an
    /// executable to tell a **leaf** — a command, an assignment — from a
    /// **compound** whose status is its body's: if the body recorded, the
    /// compound must not overwrite it, so `if … { false | true }` keeps the
    /// pipeline's per-stage list rather than flattening it to one entry.
    /// A counter rather than a list of compound node kinds, so a new kind of
    /// executable cannot forget to be on it.
    status_records: u64,
}

impl Shell {
    /// Publish the status the next command will see as `$sh.status`, with the
    /// per-stage breakdown `$sh.pipestatus` reports.
    fn record_status(&mut self, status: u8, stages: Vec<u8>) {
        self.vars.set_status(status, stages);
        self.status_records += 1;
    }

    /// Copy the live job table into the variable store, where `$sh.jobs` reads
    /// it. Expansion is handed only the store, so the alternative is refreshing
    /// at every site that touches the table — a launch, a reap, `fg`, `bg` — and
    /// one missed site would serve a stale answer. Doing it on the executable
    /// funnel instead means the sync cannot be forgotten: everything that could
    /// read `$sh.jobs` is an executable, and every executable passes here.
    fn sync_jobs(&mut self) {
        // A fork may not ask the table anything: it is not the parent of the
        // pids it inherited, so `waitpid` fails with `ECHILD` and every job
        // would look finished. It keeps the snapshot it inherited instead —
        // what the shell knew when it forked, which is the most a stage can
        // truthfully say, and better than the empty map that overwriting gives.
        if self.forked {
            return;
        }
        // Children the shell still owns but no longer has a *job* for — what a
        // plain `disown` leaves behind, and what a forked stage's background
        // children always were. Nothing else would collect them: the handler
        // only forwards, and the table is the only other thing that drains, so
        // an empty table used to mean a discarded child stayed a zombie for the
        // rest of the session. Still no syscall in the common case — once
        // everything is reaped the reaper owns nothing and this walks an empty
        // set.
        crate::reaper::drain();
        // No jobs means no syscall, which is the common case.
        let jobs = if self.jobs.has_jobs() {
            self.jobs.info()
        } else {
            Vec::new()
        };
        self.vars.set_jobs(
            jobs.into_iter()
                .map(|job| {
                    (
                        job.id,
                        Value::Map(vec![
                            // Carried in the record as well as being the key: a
                            // handle reaches its record without one.
                            ("id".to_owned(), Value::Integer(job.id as i64)),
                            ("pid".to_owned(), Value::Integer(i64::from(job.pid))),
                            ("cmd".to_owned(), Value::String(job.command)),
                            ("state".to_owned(), Value::String(job.state.to_owned())),
                            // Empty while a job runs, filling in when it
                            // finishes — the 8-bit view `$sh.status` gives.
                            (
                                "status".to_owned(),
                                Value::String(
                                    job.status.map(|code| code.to_string()).unwrap_or_default(),
                                ),
                            ),
                        ]),
                    )
                })
                .collect(),
        );
    }
}

/// What an executable contributed to the **result so far**.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Produced {
    /// Nothing of its own: its status *is* its result, as for any command.
    Status,
    /// A value, already stored in `Shell::result`.
    Value,
    /// Nothing at all — a guard skipped it, so the previous result still stands.
    Nothing,
}

#[derive(Default)]
struct PromptConfig {
    text: Option<String>,
    hooks: Vec<PromptHook>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptEvent {
    PrePrompt,
    PreExec,
    PostExec,
    Exit,
}

impl PromptEvent {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "preprompt" => Some(Self::PrePrompt),
            "preexec" => Some(Self::PreExec),
            "postexec" => Some(Self::PostExec),
            "exit" => Some(Self::Exit),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PromptHook {
    event: PromptEvent,
    name: String,
    function: String,
}

impl Shell {
    fn new() -> Self {
        Self {
            vars: Vars::new(),
            funcs: Funcs::new(),
            jobs: exec::JobTable::new(),
            control: None,
            forked: false,
            loop_depth: 0,
            prompt: PromptConfig::default(),
            result: Value::String(String::new()),
            produced: Produced::Status,
            status_records: 0,
        }
    }
}

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
    let options = match StartupOptions::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            note!("mesh: {message}");
            return ExitCode::from(2);
        }
    };
    // Before anything can spawn a child. Every wait now reads from the reaper's
    // store rather than calling `waitpid` itself, so this is not the optional
    // extra a prompt-only notification would be — a shell without it would find
    // the store permanently empty and wait forever.
    crate::reaper::install();
    match &options.invocation {
        Invocation::Print(text) => {
            ExitCode::from(builtins::write_stdout("stdout", text.as_bytes()))
        }
        Invocation::Command(text) => run_batch(&text.clone(), &options),
        Invocation::Script(path) => match read_script(path) {
            Ok(text) => run_batch(&text, &options),
            Err(code) => ExitCode::from(code),
        },
        Invocation::Stdin => run_piped(&options),
        Invocation::Default => {
            if io::stdin().is_terminal() && io::stdout().is_terminal() {
                run_interactive(&options)
            } else {
                run_piped(&options)
            }
        }
    }
}

/// Read a script file, reporting failures with the shell's own status
/// conventions: `127` when it is not there, `126` when it is there but cannot be
/// read — the same codes an unrunnable command yields.
fn read_script(path: &Path) -> Result<String, u8> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) => {
            note!("mesh: {}: {error}", path.display());
            Err(if error.kind() == io::ErrorKind::NotFound {
                127
            } else {
                126
            })
        }
    }
}

/// Run a whole batch of mesh source — a script file or a `-c` string — as the
/// entire session: startup files, the source, then the logout file.
///
/// The source is parsed as one unit, so a syntax error anywhere rejects the
/// whole batch and nothing runs (`DESIGN.md` §"Error handling"); this is why a
/// half-valid script cannot leave the shell in a partly-configured state.
fn run_batch(text: &str, options: &StartupOptions) -> ExitCode {
    let mut shell = Shell::new();
    shell
        .vars
        .set_invocation(options.name.clone(), options.args.clone());
    let (origin, source) = options.origin(false);
    shell.vars.set_origin(origin, source);
    let last = match run_startup_files(options, false, 0, &mut shell) {
        Step::Continue(code) => code,
        Step::Exit(code) => return ExitCode::from(run_logout(options, code, &mut shell)),
        Step::Return(value) => {
            return ExitCode::from(run_logout(options, status_of(&value), &mut shell));
        }
    };
    let code = match run_line(text, last, false, &mut shell) {
        Step::Continue(code) | Step::Exit(code) => code,
        Step::Return(_) => unreachable!("top-level return handled in run_line"),
    };
    ExitCode::from(run_logout(options, code, &mut shell))
}

/// Where this invocation's commands come from, decided by `-c` / `-s` and the
/// first operand.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    /// Neither a script nor `-c`: an interactive session when stdin and stdout
    /// are both terminals, otherwise commands read from stdin.
    Default,
    /// `-s` — read commands from stdin even on a terminal.
    Stdin,
    /// `-c TEXT` — run one command string.
    Command(String),
    /// A script path operand — run that file.
    Script(PathBuf),
    /// `--help` / `--version` — print this text and exit successfully.
    Print(String),
}

#[derive(Debug, PartialEq, Eq)]
struct StartupOptions {
    login: bool,
    no_rc: bool,
    rc_file: Option<PathBuf>,
    save_history: bool,
    invocation: Invocation,
    /// Positional arguments, exposed as the `$sh.args` list.
    args: Vec<String>,
    /// The shell-or-script name, exposed as `$sh.name` (bash's `$0`).
    name: String,
}

impl StartupOptions {
    /// The input this invocation establishes: `DESIGN.md`'s **origin**, plus the
    /// path for the origins that are files.
    ///
    /// Kept separate from interactivity on purpose — `mesh -i script.mesh` is both
    /// interactive and a script — so `interactive` only decides between the two
    /// readings of a bare invocation, which is the one place they overlap.
    fn origin(&self, interactive: bool) -> (vars::Origin, String) {
        match &self.invocation {
            Invocation::Script(path) => (vars::Origin::Script, path.to_string_lossy().into_owned()),
            Invocation::Command(_) => (vars::Origin::Command, String::new()),
            Invocation::Stdin => (vars::Origin::Stdin, String::new()),
            Invocation::Default | Invocation::Print(_) => (
                if interactive {
                    vars::Origin::Interactive
                } else {
                    vars::Origin::Stdin
                },
                String::new(),
            ),
        }
    }
}

impl Default for StartupOptions {
    fn default() -> Self {
        Self {
            login: false,
            no_rc: false,
            rc_file: None,
            save_history: true,
            invocation: Invocation::Default,
            args: Vec::new(),
            name: "mesh".to_owned(),
        }
    }
}

const USAGE: &str = "\
Usage: mesh [OPTIONS] [SCRIPT [ARG ...]]
       mesh [OPTIONS] -c COMMAND [ARG ...]

Options:
  -c COMMAND           Run COMMAND, then exit
  -s                   Read commands from stdin, even on a terminal
  -l, --login          Run as a login shell (also sources login.mesh)
      --rcfile FILE    Use FILE instead of rc.mesh
      --norc           Skip rc.mesh
      --no-save-history  Keep this session's history in memory only
  -h, --help           Print help
  -V, --version        Print version

With no SCRIPT and no -c, mesh is interactive when stdin and stdout are
terminals, and otherwise reads commands from stdin.
";

impl StartupOptions {
    /// Parse the command line. Option parsing stops at the first operand, as it
    /// does in POSIX shells, so a script's own flags reach the script rather
    /// than mesh: `mesh deploy.mesh --login` passes `--login` along.
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-l" | "--login" => options.login = true,
                "--norc" => options.no_rc = true,
                "--no-save-history" | "--no-history" => options.save_history = false,
                "-h" | "--help" => {
                    options.invocation = Invocation::Print(USAGE.to_owned());
                    return Ok(options);
                }
                "-V" | "--version" => {
                    options.invocation =
                        Invocation::Print(format!("mesh {}\n", env!("CARGO_PKG_VERSION")));
                    return Ok(options);
                }
                "--rcfile" => {
                    let path = args
                        .next()
                        .ok_or_else(|| "--rcfile requires a file path".to_owned())?;
                    options.rc_file = Some(path.into());
                }
                "-c" => {
                    let text = args
                        .next()
                        .ok_or_else(|| "-c requires a command string".to_owned())?;
                    options.invocation = Invocation::Command(text);
                    options.args = args.collect();
                    return Ok(options);
                }
                "-s" => {
                    options.invocation = Invocation::Stdin;
                    options.args = args.collect();
                    return Ok(options);
                }
                // `--` ends option parsing without itself being an operand, so
                // a script whose name looks like an option can still be run.
                "--" => {
                    options.take_operands(args);
                    return Ok(options);
                }
                _ if arg.starts_with('-') && arg != "-" => {
                    return Err(format!("unknown option `{arg}`"));
                }
                // The first operand is the script; everything after it is an
                // argument to that script, options included.
                _ => {
                    options.take_operands(std::iter::once(arg).chain(args));
                    return Ok(options);
                }
            }
        }
        Ok(options)
    }

    /// Consume the operand list: the first names the script, the rest are its
    /// arguments. `$sh.name` becomes the script's name, as bash's `$0` does.
    fn take_operands(&mut self, operands: impl Iterator<Item = String>) {
        let mut operands = operands;
        if let Some(script) = operands.next() {
            self.name = script.clone();
            self.invocation = Invocation::Script(script.into());
            self.args = operands.collect();
        }
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty() && Path::new(path).is_absolute())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .map(|dir| dir.join("mesh"))
}

fn history_path() -> Option<PathBuf> {
    history_path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

fn history_path_from(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    xdg_state_home
        .filter(|path| !path.is_empty() && Path::new(path).is_absolute())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|path| !path.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })
        .map(|dir| dir.join("mesh/history.sqlite3"))
}

fn prepare_history_path(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

struct TimestampedHistory<H>(H);

impl<H: History> History for TimestampedHistory<H> {
    fn save(&mut self, mut item: HistoryItem) -> reedline::Result<HistoryItem> {
        item.start_timestamp
            .get_or_insert_with(|| std::time::SystemTime::now().into());
        self.0.save(item)
    }

    fn load(&self, id: HistoryItemId) -> reedline::Result<HistoryItem> {
        self.0.load(id)
    }

    fn count(&self, query: SearchQuery) -> reedline::Result<i64> {
        self.0.count(query)
    }

    fn search(&self, query: SearchQuery) -> reedline::Result<Vec<HistoryItem>> {
        self.0.search(query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> reedline::Result<()> {
        self.0.update(id, updater)
    }

    fn clear(&mut self) -> reedline::Result<()> {
        self.0.clear()
    }

    fn delete(&mut self, id: HistoryItemId) -> reedline::Result<()> {
        self.0.delete(id)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.0.sync()
    }

    fn session(&self) -> Option<HistorySessionId> {
        self.0.session()
    }
}

fn open_history(
    path: PathBuf,
    session: Option<HistorySessionId>,
    session_started: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<TimestampedHistory<SqliteBackedHistory>, String> {
    let history = SqliteBackedHistory::with_file(path.clone(), session, session_started)
        .map_err(|err| err.to_string())?;
    rusqlite::Connection::open(path)
        .and_then(|connection| {
            connection.execute(
                "UPDATE history SET start_timestamp = 0 WHERE start_timestamp IS NULL",
                [],
            )
        })
        .map_err(|err| err.to_string())?;
    Ok(TimestampedHistory(history))
}

fn startup_files(options: &StartupOptions, interactive: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(dir) = config_dir() {
        files.push(dir.join("env.mesh"));
        if options.login {
            files.push(dir.join("login.mesh"));
        }
    }
    if interactive
        && !options.no_rc
        && let Some(path) = options
            .rc_file
            .clone()
            .or_else(|| config_dir().map(|dir| dir.join("rc.mesh")))
    {
        files.push(path);
    }
    files
}

fn run_config_file(path: &Path, last: u8, shell: &mut Shell) -> Step {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Step::Continue(last),
        Err(error) => {
            note!("mesh: {}: {error}", path.display());
            return Step::Continue(1);
        }
    };
    // A startup file *is* a sourced file, so it reports itself the same way — which
    // is what lets `rc.mesh` locate a sibling through `$sh.source`.
    run_sourced_text(&text, path, last, shell)
}

/// `source FILE` — run a file's mesh code in **this** shell, so what it defines
/// and assigns persists after it returns.
///
/// Exactly one operand. Extra arguments would be positional parameters for the
/// sourced file, and mesh has no way to set those yet (`shift` / `set --` are
/// deferred in `DESIGN.md`), so they are refused rather than silently ignored.
fn source_file(args: &[String], last: u8, shell: &mut Shell) -> Step {
    let [path] = args else {
        let message = if args.is_empty() {
            "source: needs a file to run"
        } else {
            "source: takes exactly one file; arguments for a sourced file are not \
             supported yet"
        };
        note!("mesh: {message}");
        return Step::Continue(2);
    };
    let path = Path::new(path);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            note!("mesh: source: {}: {error}", path.display());
            // The statuses `mesh FILE` itself uses for the same two failures, so a
            // missing or unreadable file answers the same however it is reached.
            return Step::Continue(if error.kind() == io::ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };
    run_sourced_text(&text, path, last, shell)
}

/// Run a file's text as a nested input, reporting itself through `$sh.origin` and
/// `$sh.source` while it does.
///
/// The frame is popped on **every** path out, including the ones that leave through
/// `exit` or a `return`, so a `$sh.source` read after the file finishes can never
/// name it.
fn run_sourced_text(text: &str, path: &Path, last: u8, shell: &mut Shell) -> Step {
    shell
        .vars
        .push_input(vars::Origin::Sourced, path.to_string_lossy().into_owned());
    let step = run_line(text, last, false, shell);
    shell.vars.pop_input();
    let step = match step {
        // `return` from a sourced file's top level ends *that file* and nothing
        // further: the value it carries becomes this `source`'s status, so a nested
        // source returns to its own caller rather than unwinding the whole stack.
        Step::Return(value) => Step::Continue(status_of(&value)),
        step => step,
    };
    // `source` is a **status-producing command**, so whatever the file's last
    // statement produced stops here. Without this the file's value carries out
    // through `run_recorded`, and `func f() { source lib.mesh }` returns whatever
    // `lib.mesh` happened to end with — an integer, a list — where every other
    // command yields its status. Set on both paths, since a startup file is run
    // outside `run_recorded` and would otherwise leave the value behind.
    if let Step::Continue(code) = step {
        shell.result = Value::Integer(i64::from(code));
        shell.produced = Produced::Status;
        // Published here rather than left to `run_recorded`, which a startup file
        // never passes through: a `return` in `env.mesh` is converted above, and
        // without this the next file — and the script after it — would read
        // `$sh.status` as whatever ran before. Recording again is harmless for the
        // `source` builtin, whose `run_recorded` then sees the status it wanted.
        shell.record_status(code, vec![code]);
    }
    step
}

fn run_startup_files(
    options: &StartupOptions,
    interactive: bool,
    mut last: u8,
    shell: &mut Shell,
) -> Step {
    for path in startup_files(options, interactive) {
        match run_config_file(&path, last, shell) {
            Step::Continue(code) => last = code,
            flow => return flow,
        }
    }
    Step::Continue(last)
}

fn run_logout(options: &StartupOptions, last: u8, shell: &mut Shell) -> u8 {
    if options.login
        && let Some(path) = config_dir().map(|dir| dir.join("logout.mesh"))
    {
        let _ = run_config_file(&path, last, shell);
    }
    // Clear the title on the way out, and last, so mesh has the final word over a
    // `logout.mesh` that writes one. Every exit path arrives here, which is what
    // makes this the place: `exit` would otherwise leave the window named after
    // that command forever, and Ctrl-D would leave it naming the directory of a
    // shell that is gone. An empty title is the reset every terminal understands.
    set_title(shell.vars.interactive(), "");
    last
}

/// What to do after handling one input line.
#[derive(Debug, PartialEq)]
enum Step {
    /// A line ran; carry this status as the new "last status".
    Continue(u8),
    /// `exit` was invoked; leave the shell with this status.
    Exit(u8),
    /// `return` was invoked; unwind the current function carrying its result
    /// value. Exit status is a *view* of that value ([`status_of`]). At top level
    /// (no function) `run_line` reports it as a recoverable error instead.
    Return(Value),
}

impl Step {
    /// The exit status this step contributes as the new "last status".
    fn status(&self) -> u8 {
        match self {
            Step::Continue(code) | Step::Exit(code) => *code,
            Step::Return(value) => status_of(value),
        }
    }
}

/// A mesh value's exit status — the "status is a view of the result" rule from
/// `DESIGN.md` §"Functions": an integer is its own status, a boolean inverts
/// (`true` → 0, `false` → 1, the Unix convention), and every other value type is
/// `0` (producing a value is success).
fn status_of(value: &Value) -> u8 {
    match value {
        Value::Integer(code) => code.rem_euclid(256) as u8,
        Value::Boolean(ok) => u8::from(!ok),
        _ => 0,
    }
}

/// Parse and run one input unit against the session. `in_function` is true while
/// running a function body: there a `return` unwinds; at top level it is a
/// recoverable error.
fn run_line(text: &str, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    // Input the parser rejected never reaches `run_recorded`, so this is the only
    // place its status can be published. Without it the shell carries 2 to the
    // next command while `$sh.status` still reports whatever ran before.
    let reject = |shell: &mut Shell| {
        shell.record_status(2, vec![2]);
        Step::Continue(2)
    };
    let step = match parser::parse(text) {
        Ok(parser::ParseOutcome::Complete(source)) => run_source(&source, last, in_function, shell),
        Ok(parser::ParseOutcome::Incomplete) => {
            note!("mesh: syntax error: unexpected end of input");
            reject(shell)
        }
        Ok(parser::ParseOutcome::IncompleteHeredoc(delimiter)) => {
            note!("mesh: syntax error: heredoc missing its `{delimiter}` delimiter");
            reject(shell)
        }
        Err(error) => {
            note!("mesh: {error}");
            reject(shell)
        }
    };
    if !in_function && shell.loop_depth == 0 {
        shell.control = None;
    }
    step
}

/// Execute the syntax tree recursively.  Keeping execution on the tree (rather
/// than splitting the original text again) makes nesting and short-circuiting
/// obey exactly the same structure the parser accepted.
fn run_source(
    source: &parser::Source,
    mut status: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    // What this *body* produced, reported to whatever ran it: the last statement
    // that produced anything. A body where nothing ran, or whose every statement
    // was skipped by a guard, produced nothing — so it does not pass the
    // surrounding code's result off as its own. Same rule as `eval_body`.
    let mut produced = Produced::Nothing;
    for statement in &source.statements {
        match run_statement(statement, status, in_function, shell) {
            Step::Continue(code) => status = code,
            flow => return flow,
        }
        if shell.produced != Produced::Nothing {
            produced = shell.produced;
        }
        if shell.control.is_some() {
            break;
        }
    }
    shell.produced = produced;
    Step::Continue(status)
}

fn run_statement(
    statement: &parser::Statement,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    run_and_or(
        &statement.and_or,
        statement.background,
        last,
        in_function,
        shell,
    )
}

fn run_and_or(
    node: &parser::AndOr,
    background: bool,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    if background && !node.rest.is_empty() {
        note!("mesh: background conditional lists are not supported yet");
        // This returns *above* `run_recorded`, which is what normally records a
        // statement's result, so the rejection is recorded here. Otherwise the
        // value some earlier statement produced would still stand, and a bare
        // `return` after it would carry that instead of the failure.
        shell.result = Value::Integer(2);
        shell.produced = Produced::Status;
        return Step::Continue(2);
    }
    let mut step = run_recorded(&node.first, background, last, in_function, shell);
    for (op, executable) in &node.rest {
        let Step::Continue(status) = step else {
            return step;
        };
        let run = match op {
            parser::AndOrOp::And => status == 0,
            parser::AndOrOp::Or => status != 0,
        };
        if run {
            step = run_recorded(executable, background, status, in_function, shell);
        }
    }
    step
}

/// Run one executable and record the **result so far** it leaves behind, which a
/// later bare `return` carries out. An expression records its own value; for
/// anything else — a command, a background statement — the status *is* the
/// result. Recording per executable rather than per statement is what makes
/// `false || return` carry the `false`, since the `return` runs inside the list.
fn run_recorded(
    executable: &parser::Executable,
    background: bool,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    // What was produced is *observed*, not predicted: an expression that failed
    // produced nothing, an `if` whose branch yielded a value has produced one,
    // and a guard that failed left the executable unrun. A nested executable's
    // report carries out, so the branch's value survives the `if` that ran it.
    // Before the executable expands its words, so `$sh.jobs` reflects anything
    // the previous one launched or reaped.
    shell.sync_jobs();
    shell.produced = Produced::Status;
    let records = shell.status_records;
    let step = run_executable(executable, background, last, in_function, shell);
    if let Step::Continue(code) = step
        && shell.produced == Produced::Status
    {
        shell.result = Value::Integer(i64::from(code));
    }
    // Keep a nested breakdown only when it really describes *this* executable's
    // status. Two ways it may not: nothing nested recorded at all (an assignment,
    // a builtin, an `if` whose branch never ran), or something did but the
    // compound went on to report a different code — `func f() { false | true
    // return 7 }` ends at 7, not at the pipeline's 1. Either way this executable
    // produced the status itself and is one "stage".
    //
    // Stated as an invariant: after this, `$sh.status` is always the code just
    // returned, and `$sh.pipestatus` always breaks down the run that produced it.
    if let Step::Continue(code) = step
        && (shell.status_records == records || shell.vars.status() != code)
    {
        shell.record_status(code, vec![code]);
    }
    step
}

fn run_executable(
    node: &parser::Executable,
    background: bool,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    use parser::Executable::*;
    if background && let Some(what) = not_backgroundable(node) {
        note!("mesh: &: backgrounding {what} is not supported yet");
        return Step::Continue(2);
    }
    match node {
        Pipeline(pipeline) => run_ast_pipeline(pipeline, background, last, shell),
        Assignment {
            pattern,
            append,
            value,
            global,
        } => match eval_operand_of(value, last, in_function, shell) {
            // A right-hand side that raised `break`/`continue` produced no value to
            // bind — the loop is unwinding, so leave the target as it was rather
            // than overwriting it with the placeholder.
            Ok(_) if shell.control.is_some() => Step::Continue(last),
            Ok(value) => {
                let result = if *append {
                    let parser::BindingPattern::Name(name) = pattern else {
                        unreachable!("the parser restricts += to names")
                    };
                    if *global {
                        shell.vars.append_global(name, value)
                    } else {
                        shell.vars.append(name, value)
                    }
                } else {
                    bind_pattern(pattern, &value, &mut shell.vars, *global)
                };
                result.map_or_else(
                    |error| {
                        note!("mesh: {error}");
                        Step::Continue(1)
                    },
                    |_| Step::Continue(0),
                )
            }
            Err(step) => step,
        },
        Unset { targets, global } => {
            let mut status = 0;
            for target in targets {
                let name = match target {
                    // A place inside a binding: remove the entry, leaving the
                    // binding itself in place.
                    parser::UnsetTarget::Member(target) => {
                        if let Err(error) = unset_member(target, *global, shell) {
                            note!("mesh: unset: {error}");
                            status = 1;
                        }
                        continue;
                    }
                    parser::UnsetTarget::Name(name) => name,
                };
                if crate::vars::is_reserved_namespace(&name.value) {
                    note!("mesh: unset: `{}` is reserved", name.value);
                    status = 1;
                    continue;
                }
                // Unsetting what was never bound anywhere is the error; removing
                // nothing *here* while an outer scope still has it is not, since
                // `unset` is defined on the current scope only.
                let bound = shell.vars.is_bound(&name.value);
                if *global {
                    shell.vars.unset_global(&name.value);
                } else {
                    shell.vars.unset(&name.value);
                }
                if !bound {
                    note!("mesh: unset: {}: unbound variable", name.value);
                    status = 1;
                }
            }
            Step::Continue(status)
        }
        // An environment write is global by design: it changes what children
        // inherit, so a function-local scope would defeat the point
        // (`DESIGN.md` §"Variables and assignment").
        EnvAssignment { key, append, value } => {
            match eval_operand_of(value, last, in_function, shell) {
                // As for an ordinary assignment: a right-hand side that raised
                // `break`/`continue` produced no value, so leave the variable
                // alone.
                Ok(_) if shell.control.is_some() => Step::Continue(last),
                Ok(value) => environ::write(key, value, *append).map_or_else(
                    |error| {
                        note!("mesh: {error}");
                        Step::Continue(1)
                    },
                    |_| Step::Continue(0),
                ),
                Err(step) => step,
            }
        }
        MemberAssignment {
            target,
            append,
            value,
            global,
        } => {
            match eval_operand_of(value, last, in_function, shell) {
                // As for an ordinary assignment: a right-hand side that raised
                // `break`/`continue` produced no value, so leave the place alone.
                Ok(_) if shell.control.is_some() => Step::Continue(last),
                Ok(value) => assign_into_member(target, value, *append, *global, shell)
                    .map_or_else(
                        |error| {
                            note!("mesh: {error}");
                            Step::Continue(1)
                        },
                        |()| Step::Continue(0),
                    ),
                Err(step) => step,
            }
        }
        Function {
            name,
            parameters,
            body,
        } => {
            if name == "func" || name == "return" || name == "not" || builtins::is_builtin(name) {
                note!("mesh: func: `{name}` is a reserved name and cannot be a function name");
                return Step::Continue(2);
            }
            // Parameter names are already validated (distinct, not `env`) by the
            // parser's `parameters()`.
            shell.funcs.define(
                name.clone(),
                FuncDef {
                    params: parameters.clone(),
                    body: body.clone(),
                },
            );
            Step::Continue(0)
        }
        If(expression) => run_ast_if(expression, last, in_function, shell),
        Match(expression) => run_ast_match(expression, last, in_function, shell),
        For {
            bindings,
            iterable,
            body,
        } => run_ast_for(bindings, iterable, body, last, in_function, shell),
        While { condition, body } => run_ast_while(Some(condition), body, last, in_function, shell),
        Loop { body } => run_ast_while(None, body, last, in_function, shell),
        Fork { body } => run_forked_block(body, in_function, shell),
        Control { kind, value, guard } => {
            match guard_allows(guard.as_ref(), last, in_function, shell) {
                Ok(true) => {}
                // Skipped entirely: it produced neither a value nor a status of
                // its own, so the result so far is still whatever came before.
                Ok(false) => {
                    shell.produced = Produced::Nothing;
                    return Step::Continue(last);
                }
                Err(step) => return step,
            }
            match kind {
                parser::ControlKind::Return => {
                    // A `return` leaves the innermost unit that has an invoker to
                    // return *to*: a function, or a sourced file — whose `source`
                    // command takes the returned value's status. A script, a `-c`
                    // string, and a typed line have no caller, so there it stays an
                    // error, the same distinction bash draws.
                    if !in_function && !shell.vars.in_sourced_file() {
                        note!("mesh: return: not inside a function or sourced file");
                        return Step::Continue(1);
                    }
                    // `return val` unwinds carrying any value (its status is a view
                    // of the value); a bare `return` carries the last status, so it
                    // reads the same as `exit` with no argument (`DESIGN.md`).
                    match value
                        .as_ref()
                        .map(|v| eval_expr(v, last, in_function, shell))
                        .transpose()
                    {
                        Ok(Some(value)) => Step::Return(value),
                        // A bare `return` carries the result so far, not a
                        // freshly minted status (`DESIGN.md`).
                        Ok(None) => Step::Return(shell.result.clone()),
                        Err(step) => step,
                    }
                }
                parser::ControlKind::Break | parser::ControlKind::Continue => {
                    if shell.loop_depth == 0 {
                        note!(
                            "mesh: {}: not inside a loop",
                            if matches!(kind, parser::ControlKind::Break) {
                                "break"
                            } else {
                                "continue"
                            }
                        );
                        shell.control = Some(*kind);
                        Step::Continue(1)
                    } else {
                        shell.control = Some(*kind);
                        Step::Continue(0)
                    }
                }
            }
        }
        Expression { expression, guard } => {
            match guard_allows(guard.as_ref(), last, in_function, shell) {
                Ok(true) => {}
                // Skipped entirely — see the `Control` arm above.
                Ok(false) => {
                    shell.produced = Produced::Nothing;
                    return Step::Continue(last);
                }
                Err(step) => return step,
            }
            if runs_as_command(expression)
                && let parser::Expr::Scalar(word) = expression
            {
                return run_pipeline(
                    vec![Stage {
                        words: vec![expansion_word(&word.value)],
                        redirs: Vec::new(),
                        pipe_stderr: false,
                    }],
                    background,
                    last,
                    shell,
                );
            }
            // An expression used as a statement reports the status *view* of its
            // value (`DESIGN.md` §"Results and status"): an integer is its own
            // status, a boolean inverts, anything else is 0. That is what lets
            // `f() && …` and a function whose body ends in `1 == 2` behave the
            // way the value says they should.
            match eval_expr(expression, last, in_function, shell) {
                // An expression cut short by `break`/`continue` yielded a
                // placeholder, not a value: recording it would leave the loop's
                // unwinding visible as this statement's result.
                Ok(_) if shell.control.is_some() => {
                    shell.produced = Produced::Nothing;
                    Step::Continue(last)
                }
                Ok(value) => {
                    let code = status_of(&value);
                    shell.result = value;
                    shell.produced = Produced::Value;
                    Step::Continue(code)
                }
                Err(step) => step,
            }
        }
    }
}

/// What `&` cannot defer yet, as the noun to name in the diagnostic — or `None`
/// for an executable that *can* be backgrounded.
///
/// Only a pipeline has a child to hand the work to. Everything else runs in this
/// shell: an expression produces its value here (a backgrounded value call would
/// have to send its result back across a fork), and a loop, an assignment, or a
/// definition changes this shell's own state. Refusing beats running the
/// statement synchronously, which would break the promise `&` makes to the
/// statements after it.
fn not_backgroundable(node: &parser::Executable) -> Option<&'static str> {
    use parser::Executable::*;
    Some(match node {
        Pipeline(_) => return None,
        // A lone quoted scalar is a command, so it becomes a pipeline stage and
        // is backgrounded like any other command.
        Expression { expression, .. } if runs_as_command(expression) => return None,
        Expression { .. } => "an expression",
        Assignment { .. } => "an assignment",
        Unset { .. } => "an `unset`",
        EnvAssignment { .. } => "an environment assignment",
        MemberAssignment { .. } => "an assignment",
        Function { .. } => "a function definition",
        If(_) => "an `if`",
        Match(_) => "a `match`",
        For { .. } => "a `for` loop",
        While { .. } => "a `while` loop",
        Loop { .. } => "a `loop`",
        // A backgrounded subshell is two processes deep and needs a job-table
        // entry of its own to be resumable; until it has one, refusing says so
        // rather than silently running it in the foreground.
        Fork { .. } => "a `fork` block",
        Control { kind, .. } => match kind {
            parser::ControlKind::Return => "`return`",
            parser::ControlKind::Break => "`break`",
            parser::ControlKind::Continue => "`continue`",
        },
    })
}

/// Whether an expression statement is really a **command**: a lone scalar word
/// carrying a quoted piece, the spelling that runs a program whose path needs
/// quoting (`"/opt/my program"`). Such a statement produces a status, not a
/// value, which is why its result is recorded like any other command's.
fn runs_as_command(expression: &parser::Expr) -> bool {
    matches!(expression, parser::Expr::Scalar(word)
    if word.value.pieces.iter().any(|piece| match piece {
        parser::WordPiece::Text { quote, .. } | parser::WordPiece::Variable { quote, .. } => {
            !matches!(quote, parser::QuoteMode::Bare)
        }
    }))
}

fn guard_allows(
    guard: Option<&parser::Guard>,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<bool, Step> {
    match guard {
        None => Ok(true),
        // A guard only decides *whether* the statement runs; it is not the
        // statement, so its bookkeeping is the operand's — see `eval_operand_of`.
        // A guard that raised `break`/`continue` produced no truth value, so the
        // statement it guards does not run: the caller reports it as skipped and
        // the flag travels on to the loop it belongs to.
        Some(guard) => eval_operand_of(&guard.condition, last, in_function, shell)
            .map(|value| shell.control.is_none() && truthy(&value) != guard.unless),
    }
}

/// Report a compound executable's own result once its body has run.
///
/// A construct that *ran* but produced no value — an empty branch, an `if` with
/// no branch to take, an unmatched `match`, a loop whose body never ran — results
/// in the **empty string**. Not the result the code before it recorded (the
/// construct did run, so it is no longer the result so far) and not its own
/// status: that is what the same construct yields in value position, where
/// `x = if false { … }` is `""`.
fn compound_result(step: Step, shell: &mut Shell) -> Step {
    if matches!(step, Step::Continue(_)) && shell.produced == Produced::Nothing {
        shell.result = Value::String(String::new());
        shell.produced = Produced::Value;
    }
    step
}

fn run_ast_if(node: &parser::IfExpr, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    let code = match condition_status(&node.condition, last, in_function, shell) {
        Ok(Some(code)) => code,
        // The condition raised `break`/`continue`, so there is no truth value to
        // branch on. Run nothing and leave the flag for the enclosing loop; the
        // `if` produced neither a value nor a status of its own.
        Ok(None) => {
            shell.produced = Produced::Nothing;
            return Step::Continue(last);
        }
        Err(step) => return step,
    };
    if code == 0 {
        compound_result(run_source(&node.then_body, 0, in_function, shell), shell)
    } else {
        match &node.else_branch {
            Some(parser::ElseBranch::If(next)) => run_ast_if(next, code, in_function, shell),
            Some(parser::ElseBranch::Block(body)) => {
                compound_result(run_source(body, 0, in_function, shell), shell)
            }
            // No branch to take: the `if` ran and produced nothing, so its result
            // is the empty string rather than the previous statement's.
            None => {
                shell.produced = Produced::Nothing;
                compound_result(Step::Continue(0), shell)
            }
        }
    }
}

fn run_ast_match(node: &parser::MatchExpr, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    let subject = match eval_operand_of(&node.value, last, in_function, shell) {
        Ok(value) => value,
        Err(step) => return step,
    };
    // No subject to match against: the expression raised `break`/`continue`, so
    // no arm may be selected on the strength of the placeholder.
    if shell.control.is_some() {
        shell.produced = Produced::Nothing;
        return Step::Continue(last);
    }
    for arm in &node.arms {
        let matched = match match_bindings(&arm.pattern, &subject, last, in_function, shell) {
            Ok(matched) => matched,
            Err(step) => return step,
        };
        // A pattern is an expression too (`id(if true { break })`), so it can
        // raise control while being compared. "No match" against a placeholder is
        // not an answer: stop rather than fall through to a later arm.
        if shell.control.is_some() {
            shell.produced = Produced::Nothing;
            return Step::Continue(last);
        }
        let Some(bindings) = matched else { continue };
        let snapshot = shell.vars.active_snapshot();
        commit_bindings(bindings, &mut shell.vars);
        if let Some(guard) = &arm.guard {
            match eval_operand_of(guard, last, in_function, shell) {
                // The guard raised `break`/`continue`, so it produced no truth
                // value. Its falsy placeholder must not be read as "this arm does
                // not match" — that would go on to try later arms and run one.
                Ok(_) if shell.control.is_some() => {
                    shell.vars.restore_active(snapshot);
                    shell.produced = Produced::Nothing;
                    return Step::Continue(last);
                }
                Ok(value) if truthy(&value) => {}
                Ok(_) => {
                    shell.vars.restore_active(snapshot);
                    continue;
                }
                Err(step) => return step,
            }
        }
        return compound_result(run_source(&arm.body, 0, in_function, shell), shell);
    }
    // No arm matched, so nothing produced a value — as for a branchless `if`.
    shell.produced = Produced::Nothing;
    compound_result(Step::Continue(0), shell)
}

/// The status a condition reports, or `None` when evaluating it raised
/// `break`/`continue`.
///
/// A pending control flag means the condition never produced a truth value, so
/// no caller may take a branch, test a loop, or run a guarded statement on the
/// strength of it — the placeholder an unwinding expression yields is not an
/// answer. Each caller decides what "no answer" means for it; none of them may
/// treat it as false.
fn condition_status(
    condition: &parser::Executable,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<u8>, Step> {
    if let parser::Executable::Assignment {
        pattern,
        append: false,
        value,
        ..
    } = condition
        && matches!(pattern, parser::BindingPattern::List(_))
    {
        let value = eval_operand_of(value, last, in_function, shell)?;
        if shell.control.is_some() {
            return Ok(None);
        }
        return match pattern_bindings(pattern, &value) {
            Ok(Some(bindings)) => {
                commit_bindings(bindings, &mut shell.vars);
                Ok(Some(0))
            }
            Ok(None) => Ok(Some(1)),
            Err(message) => Err(runtime_message(message)),
        };
    }
    if let parser::Executable::Expression {
        expression,
        guard: None,
    } = condition
    {
        let value = eval_operand_of(expression, last, in_function, shell)?;
        return Ok(shell
            .control
            .is_none()
            .then(|| if truthy(&value) { 0 } else { 1 }));
    }
    match run_executable(condition, false, last, in_function, shell) {
        Step::Continue(_) if shell.control.is_some() => Ok(None),
        Step::Continue(code) => Ok(Some(code)),
        step => Err(step),
    }
}

fn run_ast_for(
    bindings: &[parser::BindingPattern],
    iterable: &parser::Expr,
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    if let Err(message) = validate_patterns(bindings) {
        note!("mesh: for: {message}");
        return Step::Continue(2);
    }
    let value = match eval_operand_of(iterable, last, in_function, shell) {
        Ok(v) => v,
        Err(step) => return step,
    };
    // The iterable raised `break`/`continue`. It is evaluated before this loop
    // exists — `loop_depth` still counts the enclosing one — so the flag belongs
    // to that loop and there is nothing here to iterate. Leaving it set is what
    // lets the enclosing loop see it; iterating the placeholder would run this
    // body, and the statements after it, first.
    if shell.control.is_some() {
        shell.produced = Produced::Nothing;
        return Step::Continue(last);
    }
    let values = match iteration_values(value, bindings.len()) {
        Ok(values) => values,
        Err(message) => return runtime_message(message),
    };
    let mut status = 0;
    // The loop collects a value per completed pass, exactly as `eval_for_expr`
    // does, because a `for` *is* the same construct in either position: its
    // result is the aggregate, not the last pass. Without this the loop would
    // leave its final iteration's value behind as the result so far, and a bare
    // `return` after it would carry that scalar instead of the list.
    let mut results = Vec::new();
    shell.loop_depth += 1;
    for values in values {
        if let Err(message) = bind_iteration(bindings, values, shell) {
            shell.loop_depth -= 1;
            return runtime_message(message);
        }
        match run_source(body, 0, in_function, shell) {
            Step::Continue(code) => status = code,
            flow => {
                shell.loop_depth -= 1;
                return flow;
            }
        }
        let pass = if shell.produced == Produced::Nothing {
            Value::String(String::new())
        } else {
            shell.result.clone()
        };
        match shell.control.take() {
            Some(parser::ControlKind::Break) => break,
            Some(parser::ControlKind::Continue) => continue,
            Some(parser::ControlKind::Return) => unreachable!(),
            None => results.push(pass),
        }
    }
    shell.loop_depth -= 1;
    shell.result = Value::List(results);
    shell.produced = Produced::Value;
    Step::Continue(status)
}

/// Run `while COND { … }`, or `loop { … }` when there is no condition.
///
/// Both are the same machine: test, run the body, repeat. `loop` is the case
/// whose test always passes, so it can only be left through `break` — or through
/// a `return` or an error, which unwind past the loop like any other flow.
/// `fork { … }` — run the body in a forked child and report its status.
///
/// The isolation is the process boundary: the child's `cd`, assignments, and
/// environment writes cannot reach the parent, and an `exit` inside it ends the
/// child, so its status arrives here as an ordinary result instead of ending the
/// shell. Only bytes cross back, as `DESIGN.md` says of a subshell — the child's
/// stdout is the shell's, so what it prints appears, but no value returns.
fn run_forked_block(body: &parser::Source, in_function: bool, shell: &mut Shell) -> Step {
    // A subshell is a status, never a value: nothing typed survives the process
    // boundary, so whatever the surrounding code had produced is not passed off
    // as this block's own.
    shell.produced = Produced::Status;
    let status = exec::fork_and_wait(shell.vars.interactive(), || {
        // Runs after the fork, so this marks the *child's* copy of the shell:
        // it is not the parent of the pids in the job table it inherited, so
        // `jobs` must not `waitpid` on them — that fails with `ECHILD` and
        // reports every running job as finished — and `$sh.jobs` keeps the
        // snapshot it inherited. The same flag a forked pipeline stage sets.
        shell.forked = true;
        // Seeded at 0, as every other compound body is. A subshell is a fresh
        // boundary: `false; fork { }` reporting 1 would carry a failure from
        // outside it across the very edge the construct exists to draw.
        match run_source(body, 0, in_function, shell) {
            Step::Continue(code) | Step::Exit(code) => code,
            // A `return` that reached the top of a subshell body has no caller
            // left inside it; its value's status is what the child exits with.
            Step::Return(value) => status_of(&value),
        }
    });
    match status {
        Ok(code) => {
            shell.result = Value::Integer(i64::from(code));
            shell.record_status(code, vec![code]);
            Step::Continue(code)
        }
        Err(error) => {
            note!("mesh: fork: {error}");
            Step::Continue(1)
        }
    }
}

fn run_ast_while(
    condition: Option<&parser::Executable>,
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    // The loop's own result: 0 when the body never runs, so a condition that is
    // false from the start succeeds.
    let mut status = 0;
    // What the condition reads as "the last status" — the preceding command on
    // the first test, then whatever the body just did, matching `if`.
    let mut previous = last;
    // A body that never runs leaves this in place, so the loop reports having
    // produced nothing; each pass's `run_source` overwrites it otherwise.
    shell.produced = Produced::Nothing;
    shell.loop_depth += 1;
    loop {
        if let Some(condition) = condition {
            match condition_status(condition, previous, in_function, shell) {
                Ok(Some(0)) => {}
                Ok(Some(_)) => break,
                // The condition raised `break`/`continue`. It sits inside this
                // loop's header, and `loop_depth` already counts this loop, so it
                // targets this loop.
                Ok(None) => match shell.control.take() {
                    Some(parser::ControlKind::Break) => break,
                    // `continue` skips to the next pass, which re-tests the
                    // condition — it may have changed state that makes the next
                    // test answer differently. A condition that changes nothing
                    // spins, exactly as `while true { continue }` does.
                    _ => continue,
                },
                Err(step) => {
                    shell.loop_depth -= 1;
                    return step;
                }
            }
        }
        match run_source(body, 0, in_function, shell) {
            Step::Continue(code) => {
                status = code;
                previous = code;
            }
            flow => {
                shell.loop_depth -= 1;
                return flow;
            }
        }
        match shell.control.take() {
            Some(parser::ControlKind::Break) => break,
            Some(parser::ControlKind::Continue) => continue,
            Some(parser::ControlKind::Return) => unreachable!("`return` unwinds as a Step"),
            None => {}
        }
    }
    shell.loop_depth -= 1;
    compound_result(Step::Continue(status), shell)
}

fn iteration_values(value: Value, binding_count: usize) -> Result<Vec<Vec<Value>>, String> {
    match (value, binding_count) {
        (Value::Map(entries), 2) => Ok(entries
            .into_iter()
            .map(|(key, value)| vec![Value::String(key), value])
            .collect()),
        (Value::Map(_), _) => Err("map iteration requires `for key, value in map`".into()),
        (_, 2) => Err("two loop bindings require a map value".into()),
        (Value::List(values), 1) => Ok(values.into_iter().map(|value| vec![value]).collect()),
        (value, 1) => Ok(vec![vec![value]]),
        (_, _) => Err("a loop requires one binding, or two bindings for a map".into()),
    }
}

fn bind_iteration(
    bindings: &[parser::BindingPattern],
    values: Vec<Value>,
    shell: &mut Shell,
) -> Result<(), String> {
    let mut pending = Vec::new();
    for (pattern, value) in bindings.iter().zip(&values) {
        let Some(mut found) = pattern_bindings(pattern, value)? else {
            return Err("loop value does not match its binding pattern".into());
        };
        pending.append(&mut found);
    }
    validate_bindings(&pending)?;
    commit_bindings(pending, &mut shell.vars);
    Ok(())
}

/// Bind every name a pattern names, in one scope: `global` chooses the
/// session-global one, so `global [a b] = $pair` puts both there rather than
/// splitting them across scopes.
fn bind_pattern(
    pattern: &parser::BindingPattern,
    value: &Value,
    vars: &mut Vars,
    global: bool,
) -> Result<(), String> {
    let bindings = pattern_bindings(pattern, value)?
        .ok_or_else(|| "value does not match binding pattern".to_string())?;
    validate_bindings(&bindings)?;
    for (name, value) in bindings {
        if global {
            vars.set_value_global(&name, value);
        } else {
            vars.set_value(&name, value);
        }
    }
    Ok(())
}

fn validate_patterns(patterns: &[parser::BindingPattern]) -> Result<(), String> {
    fn names(pattern: &parser::BindingPattern, out: &mut Vec<(String, Value)>) {
        match pattern {
            parser::BindingPattern::Name(name) | parser::BindingPattern::Rest(name) => {
                out.push((name.clone(), Value::String(String::new())));
            }
            parser::BindingPattern::List(patterns) => {
                for pattern in patterns {
                    names(pattern, out);
                }
            }
            parser::BindingPattern::Ignore => {}
        }
    }
    let mut bindings = Vec::new();
    for pattern in patterns {
        names(pattern, &mut bindings);
    }
    validate_bindings(&bindings)
}

fn pattern_bindings(
    pattern: &parser::BindingPattern,
    value: &Value,
) -> Result<Option<Vec<(String, Value)>>, String> {
    use parser::BindingPattern::*;
    match pattern {
        Name(name) => Ok(Some(vec![(name.clone(), value.clone())])),
        Ignore => Ok(Some(Vec::new())),
        Rest(_) => Err("`...rest` is only valid inside a list pattern".into()),
        List(patterns) => {
            let Value::List(values) = value else {
                return Ok(None);
            };
            let rest = patterns
                .iter()
                .position(|pattern| matches!(pattern, Rest(_)));
            let fixed = patterns.len() - usize::from(rest.is_some());
            if rest.map_or(values.len() != fixed, |_| values.len() < fixed) {
                return Ok(None);
            }
            let mut bindings = Vec::new();
            for (index, pattern) in patterns.iter().enumerate() {
                match pattern {
                    Rest(name) => {
                        let tail_fixed = patterns.len() - index - 1;
                        bindings.push((
                            name.clone(),
                            Value::List(values[index..values.len() - tail_fixed].to_vec()),
                        ));
                    }
                    _ => {
                        let value_index = if rest.is_some_and(|rest| index > rest) {
                            values.len() - (patterns.len() - index)
                        } else {
                            index
                        };
                        let Some(mut found) = pattern_bindings(pattern, &values[value_index])?
                        else {
                            return Ok(None);
                        };
                        bindings.append(&mut found);
                    }
                }
            }
            validate_bindings(&bindings)?;
            Ok(Some(bindings))
        }
    }
}

fn validate_bindings(bindings: &[(String, Value)]) -> Result<(), String> {
    for (index, (name, _)) in bindings.iter().enumerate() {
        if vars::is_reserved_namespace(name) {
            return Err(format!(
                "`{name}` is a reserved name and cannot be a binding"
            ));
        }
        if bindings[..index].iter().any(|(old, _)| old == name) {
            return Err(format!("duplicate binding `{name}`"));
        }
    }
    Ok(())
}

fn commit_bindings(bindings: Vec<(String, Value)>, vars: &mut Vars) {
    for (name, value) in bindings {
        vars.set_value(&name, value);
    }
}

struct Stage {
    words: Vec<Word>,
    redirs: Vec<Redir>,
    pipe_stderr: bool,
}

struct Redir {
    kind: exec::RedirKind,
    /// The descriptor a `N>` prefix named, or `None` for the direction's default.
    fd: Option<u32>,
    /// What [`Redir::target`] names — the four readings are mutually exclusive,
    /// so they are one choice rather than a set of flags to keep consistent.
    means: Means,
    target: Word,
}

/// What a redirection's target word names once expanded.
enum Means {
    /// A path to open — `> file`, `< file`.
    Path,
    /// A descriptor to duplicate — `2>&1`, `<&0`.
    Descriptor,
    /// The input text itself — `<<< word`. The word expands like any other, and
    /// a trailing newline is added, so `cmd <<< hi` feeds `hi\n` as bash does.
    Text,
    /// A heredoc body, carried here rather than in `target` because the parser
    /// already read it; the delimiter's quoting decides whether it interpolates.
    Document(parser::HeredocBody),
}

fn run_ast_pipeline(
    node: &parser::Pipeline,
    background: bool,
    last: u8,
    shell: &mut Shell,
) -> Step {
    let mut stages = Vec::with_capacity(node.stages.len());
    for (index, command) in node.stages.iter().enumerate() {
        match guard_allows(command.guard.as_ref(), last, false, shell) {
            Ok(true) => {}
            // Skipped entirely, as for a guarded expression or control word: it
            // produced neither a value nor a status, so the previous result still
            // stands rather than being replaced by the inherited one.
            Ok(false) => {
                shell.produced = Produced::Nothing;
                return Step::Continue(last);
            }
            Err(step) => return step,
        }
        let mut words = Vec::new();
        let mut redirs = Vec::new();
        for item in &command.items {
            match item {
                parser::CommandItem::Word(word) => words.push(expansion_word(&word.value)),
                parser::CommandItem::Redirect {
                    kind,
                    fd,
                    target,
                    body,
                } => {
                    let target = expansion_word(&target.value);
                    match kind {
                        // `&> file` is defined as `> file 2>&1`, so desugar it
                        // into exactly that pair rather than carrying a third
                        // mechanism through the executor.
                        parser::RedirectKind::Both => {
                            redirs.push(Redir {
                                kind: exec::RedirKind::Out,
                                fd: Some(1),
                                means: Means::Path,
                                target,
                            });
                            redirs.push(Redir {
                                kind: exec::RedirKind::Out,
                                fd: Some(2),
                                means: Means::Descriptor,
                                target: one_word("1"),
                            });
                        }
                        parser::RedirectKind::Heredoc => {
                            let Some(body) = body else {
                                note!("mesh: heredoc: missing body");
                                return Step::Continue(1);
                            };
                            redirs.push(Redir {
                                kind: exec::RedirKind::In,
                                fd: Some(0),
                                means: Means::Document(body.value.clone()),
                                target,
                            });
                        }
                        parser::RedirectKind::HereString => redirs.push(Redir {
                            kind: exec::RedirKind::In,
                            fd: Some(0),
                            means: Means::Text,
                            target,
                        }),
                        // The duplication kinds stay split by side so an absent
                        // `N` takes the direction's descriptor: `>&2` retargets
                        // stdout, `<&0` stdin.
                        _ => redirs.push(Redir {
                            kind: match kind {
                                parser::RedirectKind::Input | parser::RedirectKind::DuplicateIn => {
                                    exec::RedirKind::In
                                }
                                parser::RedirectKind::Append => exec::RedirKind::Append,
                                _ => exec::RedirKind::Out,
                            },
                            fd: *fd,
                            means: if matches!(
                                kind,
                                parser::RedirectKind::DuplicateOut
                                    | parser::RedirectKind::DuplicateIn
                            ) {
                                Means::Descriptor
                            } else {
                                Means::Path
                            },
                            target,
                        }),
                    }
                }
            }
        }
        stages.push(Stage {
            words,
            redirs,
            pipe_stderr: node.pipe_stderr.get(index).copied().unwrap_or(false),
        });
    }
    run_pipeline(stages, background, last, shell)
}

/// Adapt a parser word at the expansion boundary without recreating source text.
/// Quote modes map directly to the expansion layer's literal/expandable bit, so
/// escaped and quoted pieces can never acquire syntax in a second lexer pass.
/// Interpolate a heredoc body: `$…` references resolve and the double-quote
/// escape set applies, and nothing else does.
///
/// Deliberately *not* `expand_word`: a body is data, so it is never tilde
/// expanded, globbed, or word-split. Only the variable and escape rules a `"…"`
/// string uses carry over, which is what an unquoted `<< END` promises in
/// `DESIGN.md`.
fn interpolate_heredoc(text: &str, vars: &Vars) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().expect("i is a char boundary");
        if c == '\\'
            && let Some(next) = text[i + 1..].chars().next()
        {
            let simple = match next {
                'n' => Some('\n'),
                't' => Some('\t'),
                'r' => Some('\r'),
                'e' => Some('\u{1b}'),
                '\\' => Some('\\'),
                '"' => Some('"'),
                '$' => Some('$'),
                _ => None,
            };
            if let Some(decoded) = simple {
                out.push(decoded);
                i += 1 + next.len_utf8();
                continue;
            }
            if next == 'u' {
                // `\u` *is* a recognized escape, so a malformed one is an error
                // rather than literal text — `"\u{zz}"` is a syntax error and a
                // heredoc promises the same escape set. Only an escape the set
                // does not contain at all falls through to the rule below.
                let (decoded, end) = parser::decode_unicode_escape(text, i + 2)
                    .ok_or("heredoc: syntax error: invalid \\u{…} escape")?;
                out.push(decoded);
                i = end;
                continue;
            }
            // An unknown escape stays as written. A body carries data — shell
            // snippets, JSON, Windows paths — where a stray backslash is
            // ordinary text, so rejecting it the way a `"…"` literal does would
            // make the common case unusable.
            out.push(c);
            i += 1;
            continue;
        }
        if c == '$' {
            // Extent comes from the command grammar itself — `variable_end` plus
            // the `variable_access_prefix` continuation the tokenizer applies to
            // the text after a reference — so a heredoc and a `"…"` string agree
            // on where `$outer.inner.key` or `$m.key[0]:upper` ends. A malformed
            // reference is a syntax error here exactly as it is in a string, so
            // `${bad` cannot quietly become literal text.
            let end = parser::variable_end(text, i).map_err(|error| format!("heredoc: {error}"))?;
            if end > i + 1 {
                // A braced `${…}` is already delimited and absorbs no following
                // access, the same exception the tokenizer's merge step makes.
                // Otherwise the continuation runs over the *word* after the
                // reference, which is what the tokenizer hands it: in a command
                // the following text piece is already split at whitespace, while
                // a body is one long run, so `$xs:len` at end of line would
                // otherwise offer `len\n…` as the modifier name and be rejected.
                let end = if text[i..end].ends_with('}') {
                    end
                } else {
                    let tail = &text[end..];
                    let word = tail.find(char::is_whitespace).unwrap_or(tail.len());
                    end + parser::variable_access_prefix(&tail[..word])
                };
                let reference = expansion_variable(&text[i..end], parser::QuoteMode::Double);
                out.push_str(&expand::resolve(&reference, vars).map_err(|e| e.to_string())?);
                i = end;
                continue;
            }
        }
        out.push(c);
        i += c.len_utf8();
    }
    Ok(out)
}

fn expansion_word(word: &parser::Word) -> Word {
    Word(
        word.pieces
            .iter()
            .map(|piece| match piece {
                parser::WordPiece::Text { text, quote } => Piece::Text {
                    text: text.clone(),
                    expandable: matches!(quote, parser::QuoteMode::Bare),
                },
                parser::WordPiece::Variable { name, quote } => {
                    Piece::Var(expansion_variable(name, *quote))
                }
            })
            .collect(),
    )
}

/// One resolved step of an assignment path. A subscript stays text because which
/// it *is* depends on the value it lands on — a list index or a map key — the same
/// decision [`expand::resolve_value`] makes on the way in.
enum PathStep {
    Member(String),
    Subscript(String),
}

/// `$m.key = v`, `$xs[0] += v` — write *into* a bound collection.
///
/// Subscripts resolve against the variable store first, before anything is
/// borrowed mutably, so `$xs[$i] = v` works and the borrow of the place stays
/// uncontested. Then the path is walked to the last step and the write happens
/// there.
///
/// Nothing is auto-created along the way: a missing intermediate key is an error,
/// not an empty map conjured to hold the write. That is the fail-loud rule the rest
/// of the language follows — `$m.typo.key = v` should say so rather than silently
/// build a structure nobody asked for. The **last** step is the one exception for a
/// map, where assigning a new key is the ordinary way to add one.
fn assign_into_member(
    target: &str,
    value: Value,
    append: bool,
    global: bool,
    shell: &mut Shell,
) -> Result<(), String> {
    let (root, steps) = resolve_path(target, "assign to", &shell.vars)?;
    // Through `Vars::update`, so a failed path leaves no local shadow behind: the
    // write runs on a copy and is installed only once the whole thing succeeds.
    shell.vars.update(&root, global, |root| {
        write_at(root, &steps, value, append, target)
    })
}

/// Split a target's accesses into steps, resolving subscripts against the variable
/// store **before** anything is borrowed mutably — that is what lets `$xs[$i]` work
/// and keeps the borrow of the place uncontested. Returns the root name beside them.
///
/// `verb` names the operation in the one error this raises, so `unset` and an
/// assignment describe a slice in their own words while sharing the walk.
fn resolve_path(
    target: &str,
    verb: &str,
    vars: &vars::Vars,
) -> Result<(String, Vec<PathStep>), String> {
    let vref = expansion_variable(target, parser::QuoteMode::Bare);
    let mut steps = Vec::with_capacity(vref.accesses.len());
    for access in &vref.accesses {
        steps.push(match access {
            expand::Access::Member(key) => PathStep::Member(key.clone()),
            expand::Access::Subscript(subscript) => PathStep::Subscript(
                expand::subscript_key(subscript, vars).map_err(|error| error.to_string())?,
            ),
            // A slice names a copy of a run of elements, not a place — and for
            // either operation it would have to answer what changing a list's
            // length means, which `DESIGN.md` does not.
            expand::Access::Slice { .. } => {
                return Err(format!("{target}: cannot {verb} a slice"));
            }
        });
    }
    Ok((vref.name, steps))
}

/// `unset $m.key`, `unset $xs[0]` — remove an entry from a bound collection.
///
/// Walks to the **parent** of the last step and removes there, so it shares
/// `descend` with the assignment path rather than repeating it. Removing from a list
/// shifts what follows, which is what makes `unset $xs[0]` mean "drop the first
/// element" rather than "leave a hole".
fn unset_member(target: &str, global: bool, shell: &mut Shell) -> Result<(), String> {
    let (root, steps) = resolve_path(target, "unset", &shell.vars)?;
    shell
        .vars
        .update(&root, global, |root| remove_at(root, &steps, target))
}

/// Remove the entry a resolved path's last step names, as the whole effect of the
/// statement — so a failed removal changes nothing, the same guarantee `write_at`
/// gives the assignment it shares [`Vars::update`] with.
fn remove_at(root: &mut Value, steps: &[PathStep], target: &str) -> Result<(), String> {
    let (last, path) = steps.split_last().expect("the parser required one access");
    let mut place = root;
    for step in path {
        place = descend(place, step, target)?;
    }
    match (place, last) {
        (Value::Map(entries), PathStep::Member(key) | PathStep::Subscript(key)) => {
            let position = entries
                .iter()
                .position(|(candidate, _)| candidate == key)
                .ok_or_else(|| format!("{target}: no `{key}` in this map"))?;
            entries.remove(position);
            Ok(())
        }
        (Value::List(values), PathStep::Subscript(index)) => {
            let offset = list_offset(values.len(), index, target)?;
            values.remove(offset);
            Ok(())
        }
        (Value::List(_), PathStep::Member(key)) => {
            Err(format!("{target}: a list has no `{key}` member"))
        }
        (other, _) => Err(format!("{target}: cannot unset from {}", value_kind(other))),
    }
}

/// Walk a resolved path from `root` to the place its last step names, and write
/// there. Split out so it is the *whole* effect of an assignment — either all of it
/// happens or none of it does, which is what lets the caller apply it to a copy.
fn write_at(
    root: &mut Value,
    steps: &[PathStep],
    value: Value,
    append: bool,
    target: &str,
) -> Result<(), String> {
    let (last, path) = steps.split_last().expect("the parser required one access");
    let mut place = root;
    for step in path {
        place = descend(place, step, target)?;
    }
    let destination = match (&mut *place, last) {
        (Value::Map(entries), PathStep::Member(key) | PathStep::Subscript(key)) => {
            if let Some(position) = entries.iter().position(|(candidate, _)| candidate == key) {
                &mut entries[position].1
            } else if append {
                // Nothing to append *to*. Naming the absent key beats silently
                // treating `+=` as a first write.
                return Err(format!("{target}: no `{key}` in this map"));
            } else {
                entries.push((key.clone(), value));
                return Ok(());
            }
        }
        (Value::List(values), PathStep::Subscript(index)) => list_slot(values, index, target)?,
        (Value::List(_), PathStep::Member(key)) => {
            return Err(format!("{target}: a list has no `{key}` member"));
        }
        (other, _) => {
            return Err(format!(
                "{target}: cannot assign into {}",
                value_kind(other)
            ));
        }
    };
    if append {
        vars::append_into(destination, value, target)
    } else {
        *destination = value;
        Ok(())
    }
}

/// Follow one intermediate step to the place it names, which must already exist.
fn descend<'a>(
    place: &'a mut Value,
    step: &PathStep,
    target: &str,
) -> Result<&'a mut Value, String> {
    match (place, step) {
        (Value::Map(entries), PathStep::Member(key) | PathStep::Subscript(key)) => entries
            .iter_mut()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("{target}: no `{key}` in this map")),
        (Value::List(values), PathStep::Subscript(index)) => list_slot(values, index, target),
        (Value::List(_), PathStep::Member(key)) => {
            Err(format!("{target}: a list has no `{key}` member"))
        }
        (other, _) => Err(format!("{target}: cannot index into {}", value_kind(other))),
    }
}

/// The slot an index names, negative counting from the end as a read does. A list
/// is only ever written *in place*: an out-of-range index is an error rather than a
/// grow, since there is no value to fill the gap with.
fn list_slot<'a>(
    values: &'a mut [Value],
    index: &str,
    target: &str,
) -> Result<&'a mut Value, String> {
    let offset = list_offset(values.len(), index, target)?;
    Ok(&mut values[offset])
}

/// Resolve an index against a list's length, negative counting from the end as a
/// read does. Shared by the write and the removal so one rule covers both: an
/// out-of-range index is an error either way, since a write has no value to fill a
/// gap with and a removal has nothing to drop.
fn list_offset(len: usize, index: &str, target: &str) -> Result<usize, String> {
    let index: i64 = index
        .parse()
        .map_err(|_| format!("{target}: list index must be an integer"))?;
    let offset = if index < 0 {
        len as i128 + i128::from(index)
    } else {
        i128::from(index)
    };
    usize::try_from(offset)
        .ok()
        .filter(|offset| *offset < len)
        .ok_or_else(|| format!("{target}: list index out of range"))
}

fn expansion_variable(source: &str, quote: parser::QuoteMode) -> VarRef {
    let inner = source
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
        .or_else(|| source.strip_prefix('$'))
        .unwrap_or(source);
    let name_end = inner.find(['.', '[', ':']).unwrap_or(inner.len());
    let name = inner[..name_end].to_string();
    let mut rest = &inner[name_end..];
    let mut accesses = Vec::new();
    let mut modifiers = Vec::new();
    while !rest.is_empty() {
        if let Some(value) = rest.strip_prefix('.') {
            let end = value.find(['.', '[', ':']).unwrap_or(value.len());
            accesses.push(expand::Access::Member(value[..end].to_string()));
            rest = &value[end..];
        } else if rest.starts_with('[') {
            let close = parser::subscript_end(rest).expect("parser validated variable access");
            let index = &rest[1..close - 1];
            accesses.push(if let Some((start, end)) = index.split_once("..=") {
                expand::Access::Slice {
                    start: parse_bound(start),
                    end: parse_bound(end),
                    inclusive: true,
                }
            } else if let Some((start, end)) = index.split_once("..") {
                expand::Access::Slice {
                    start: parse_bound(start),
                    end: parse_bound(end),
                    inclusive: false,
                }
            } else {
                expand::Access::Subscript(index.to_string())
            });
            rest = &rest[close..];
        } else if let Some(value) = rest.strip_prefix(':') {
            let end = value.find(':').unwrap_or(value.len());
            if let Some(modifier) = expand::Modifier::from_name(&value[..end]) {
                modifiers.push(modifier);
            }
            rest = &value[end..];
        } else {
            unreachable!("parser validated variable access")
        }
    }
    VarRef {
        name,
        accesses,
        modifiers,
        quoted: !matches!(quote, parser::QuoteMode::Bare),
    }
}

/// Read through a job handle before an access applies to it.
///
/// Expansion resolves handles on its own side of the language, so every access
/// form here has to as well — otherwise whether a handle can be read depends on
/// how it reached the access rather than on what it is. `($j).state` needed
/// this, and `($j)["state"]` needed it separately; anything that later grows a
/// third way to reach into a value should call this rather than learn it again.
///
/// A bare handle never comes through here, which is what leaves it with no byte
/// form and lets `kill $j` mean a job.
fn through_handle(value: Value, shell: &Shell) -> Result<Value, Step> {
    match value {
        Value::Job(id) => match shell.vars.job_record(id) {
            Some(record) => Ok(record),
            None => runtime_error(format!("job {id} is no longer in the job table")),
        },
        other => Ok(other),
    }
}

fn parse_bound(value: &str) -> Option<i64> {
    (!value.is_empty()).then(|| value.parse().expect("parser validated list bound"))
}

/// Evaluate a sub-expression, returning `None` when it raised pending control flow
/// (`break`/`continue`).
///
/// `break`/`continue` travel on `shell.control` rather than through `Step`, so a
/// child can "succeed" with a placeholder while the loop is really unwinding. A
/// wrapper that then applies its own operation would report a spurious error
/// (`member access .foo requires a map`) about a value that was never produced,
/// so every wrapper stops here instead.
fn eval_operand(
    expr: &parser::Expr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<Value>, Step> {
    let value = eval_expr(expr, last, in_function, shell)?;
    Ok(shell.control.is_none().then_some(value))
}

/// Evaluate an expression that is an **operand** of the statement around it — a
/// condition, a `match` subject, a `for` iterable, an assignment's right-hand
/// side, a guard — rather than the statement's own result.
///
/// Such an operand can run statements of its own (`if (if c { false; … })`), and
/// what those recorded is the operand's bookkeeping, not the statement's. So
/// `result`/`produced` are put back afterwards and the enclosing executable goes
/// on to report its own result — an assignment records its status, a compound
/// records its branch's value, a `return` carries what the *body* produced.
///
/// `shell.control` is deliberately *not* restored: a `break` raised inside an
/// operand is real control flow and still belongs to the loop it names.
fn eval_operand_of(
    expr: &parser::Expr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let saved_result = shell.result.clone();
    let saved_produced = shell.produced;
    let value = eval_expr(expr, last, in_function, shell);
    shell.result = saved_result;
    shell.produced = saved_produced;
    value
}

/// The value an expression yields when control flow is already unwinding — never
/// consumed, since the statement layer acts on `shell.control` instead.
fn control_placeholder() -> Value {
    Value::String(String::new())
}

fn eval_expr(
    expr: &parser::Expr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    use parser::{BinaryOp as B, Expr as E, ListItem, MapItem, UnaryOp as U};
    // A `break`/`continue` raised by an earlier sub-expression (`(if c { break }) + 1`)
    // is pending control flow: the rest of the expression must not run. Yield a
    // placeholder so no further side effect happens and no operator is applied to
    // a value that was never really produced; the statement layer acts on
    // `shell.control`.
    if shell.control.is_some() {
        return Ok(control_placeholder());
    }
    match expr {
        E::Scalar(word) => expand::expand_values(vec![expansion_word(&word.value)], &shell.vars)
            .map_err(|e| {
                note!("mesh: {e}");
                Step::Continue(1)
            })
            .map(|mut v| {
                if v.len() == 1 {
                    v.pop().unwrap()
                } else {
                    Value::List(v)
                }
            }),
        E::Regex(pattern) => {
            let value = RegexValue::new(pattern.clone());
            compile_regex(&value).map_err(runtime_message)?;
            Ok(Value::Regex(value))
        }
        E::Glob(pattern) => Ok(Value::Glob(pattern.clone())),
        E::Variable(name) => {
            let reference = expansion_variable(&name.value, parser::QuoteMode::Bare);
            expand::resolve_value(&reference, &shell.vars).map_err(|error| {
                note!("mesh: {error}");
                Step::Continue(1)
            })
        }
        E::List(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    ListItem::Value(v) => out.push(eval_expr(v, last, in_function, shell)?),
                    ListItem::Spread(v) => match eval_expr(v, last, in_function, shell)? {
                        Value::List(mut v) => out.append(&mut v),
                        value => out.push(value),
                    },
                }
            }
            Ok(Value::List(out))
        }
        E::Map(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    MapItem::Pair(key, value) => {
                        let key = match eval_expr(key, last, in_function, shell)? {
                            Value::String(key) => key,
                            // Numeric-looking and boolean barewords in key position
                            // are key bytes, not typed map keys.
                            Value::Integer(key) => key.to_string(),
                            Value::Boolean(key) => key.to_string(),
                            _ => return runtime_error("map key must be a string"),
                        };
                        let value = eval_expr(value, last, in_function, shell)?;
                        if let Some((_, old)) = out.iter_mut().find(|(old, _)| old == &key) {
                            *old = value;
                        } else {
                            out.push((key, value));
                        }
                    }
                    MapItem::Spread(value) => match eval_expr(value, last, in_function, shell)? {
                        Value::Map(values) => {
                            for (key, value) in values {
                                if let Some((_, old)) = out.iter_mut().find(|(old, _)| old == &key)
                                {
                                    *old = value;
                                } else {
                                    out.push((key, value));
                                }
                            }
                        }
                        _ => return runtime_error("only a map can be spread into a map"),
                    },
                }
            }
            Ok(Value::Map(out))
        }
        E::Group(inner) => eval_expr(inner, last, in_function, shell),
        E::Unary {
            op: U::Not,
            expression,
        } => Ok(bool_value(!truthy(&eval_expr(
            expression,
            last,
            in_function,
            shell,
        )?))),
        E::Unary {
            op: U::Negate,
            expression,
        } => {
            let operand = eval_expr(expression, last, in_function, shell)?;
            // As in `Binary`: an operand that raised control flow produced no value
            // to negate, so skip the operator rather than report a type error.
            if shell.control.is_some() {
                return Ok(control_placeholder());
            }
            number(&operand)
                .and_then(|n| n.checked_neg().ok_or_else(|| "numeric overflow".into()))
                .map(Value::Integer)
                .map_err(|m| {
                    note!("mesh: {m}");
                    Step::Continue(1)
                })
        }
        E::Unary {
            op: U::Spread,
            expression,
        } => eval_expr(expression, last, in_function, shell),
        E::Binary { left, op, right } => {
            let l = eval_expr(left, last, in_function, shell)?;
            if *op == B::And && !truthy(&l) {
                return Ok(bool_value(false));
            }
            if *op == B::Or && truthy(&l) {
                return Ok(bool_value(true));
            }
            let r = eval_expr(right, last, in_function, shell)?;
            // An operand that raised `break`/`continue` produced no real value, so
            // do not apply the operator to the placeholder (which would report a
            // spurious type error); the pending control flow is what matters.
            if shell.control.is_some() {
                return Ok(control_placeholder());
            }
            eval_binary(l, *op, r).map_err(|m| {
                note!("mesh: {m}");
                Step::Continue(1)
            })
        }
        E::Member { value, name } => {
            if let E::Variable(variable) = value.as_ref()
                && variable.value.trim_start_matches('$') == "env"
            {
                return std::env::var_os(name)
                    .map(|value| Value::String(value.to_string_lossy().into_owned()))
                    .ok_or_else(|| {
                        note!("mesh: $env.{name}: not set");
                        Step::Continue(1)
                    });
            }
            let Some(value) = eval_operand(value, last, in_function, shell)? else {
                return Ok(control_placeholder());
            };
            let value = through_handle(value, shell)?;
            match value {
                Value::Map(entries) => map_lookup(&entries, name),
                _ => runtime_error(format!("member access .{name} requires a map")),
            }
        }
        E::Index { value, index } => {
            let Some(value) = eval_operand(value, last, in_function, shell)? else {
                return Ok(control_placeholder());
            };
            let value = through_handle(value, shell)?;
            if let E::Range {
                start,
                end,
                inclusive,
            } = index.as_ref()
            {
                // `Ok(None)` here means "no bound written"; a bound whose expression
                // raised control flow is reported separately through `broke`.
                let mut broke = false;
                let mut bound = |expression: &Option<Box<E>>| -> Result<Option<i64>, Step> {
                    let Some(expression) = expression.as_ref() else {
                        return Ok(None);
                    };
                    let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                        broke = true;
                        return Ok(None);
                    };
                    number(&value).map(Some).map_err(runtime_message)
                };
                let (low, high) = (bound(start)?, bound(end)?);
                if broke {
                    return Ok(control_placeholder());
                }
                return match value {
                    Value::List(values) => Ok(Value::List(
                        expand::slice(&values, low, high, *inclusive).to_vec(),
                    )),
                    Value::String(_)
                    | Value::Integer(_)
                    | Value::Boolean(_)
                    | Value::Regex(_)
                    | Value::Glob(_)
                    | Value::Stream(_)
                    | Value::Job(_)
                    | Value::Function(_) => runtime_error("cannot slice a scalar value"),
                    Value::Map(_) => runtime_error("cannot slice a map value"),
                };
            }
            let Some(index_value) = eval_operand(index, last, in_function, shell)? else {
                return Ok(control_placeholder());
            };
            match value {
                Value::List(values) => {
                    let index = number(&index_value).map_err(runtime_message)?;
                    let offset = if index < 0 {
                        values.len() as i128 + index as i128
                    } else {
                        index as i128
                    };
                    usize::try_from(offset)
                        .ok()
                        .and_then(|i| values.get(i))
                        .cloned()
                        .ok_or_else(|| {
                            note!("mesh: list index {index} out of range");
                            Step::Continue(1)
                        })
                }
                Value::String(_)
                | Value::Integer(_)
                | Value::Boolean(_)
                | Value::Regex(_)
                | Value::Glob(_)
                | Value::Stream(_)
                | Value::Job(_)
                | Value::Function(_) => runtime_error("cannot index a scalar value"),
                Value::Map(entries) => {
                    let key = match index_value {
                        Value::String(key) => key,
                        Value::Integer(key) => key.to_string(),
                        Value::Boolean(key) => key.to_string(),
                        _ => return runtime_error("map key must be a string"),
                    };
                    map_lookup(&entries, &key)
                }
            }
        }
        E::Modifier {
            value,
            name,
            arguments,
        } => {
            // `:capture` is an *invocation-level* modifier, not a value one: it has
            // to wrap execution, because by the time a value modifier saw the
            // return value the stdout would already have streamed away — the same
            // reason `$(…)` is a wrapper rather than a postfix (`DESIGN.md`
            // §"Calling for a value"). So it is recognized on the call, before the
            // call runs, rather than applied to what the call produced.
            if name == "capture" {
                if arguments.is_some() {
                    return runtime_error("modifier :capture does not take arguments");
                }
                let E::Call {
                    callee,
                    arguments: call_arguments,
                } = value.as_ref()
                else {
                    return runtime_error(
                        ":capture applies to a call — write `f(…):capture`, or `$(…)` for a \
                         command's output",
                    );
                };
                return capture_call(callee, call_arguments, last, in_function, shell);
            }
            let Some(value) = eval_operand(value, last, in_function, shell)? else {
                return Ok(control_placeholder());
            };
            if let Some(arguments) = arguments {
                if matches!(value, Value::Regex(_)) {
                    return runtime_error(format!("modifier :{name} does not take arguments"));
                }
                return eval_modifier_with_arguments(
                    name,
                    value,
                    arguments,
                    last,
                    in_function,
                    shell,
                );
            }
            apply_argument_free_modifier(name, value)
        }
        E::If(node) => eval_if_expr(node, last, in_function, shell),
        E::Match(node) => eval_match_expr(node, last, in_function, shell),
        E::For {
            bindings,
            iterable,
            body,
        } => eval_for_expr(bindings, iterable, body, last, in_function, shell),
        E::BackgroundJob(pipeline) => {
            // The handle is the whole point of `j = cmd &`: `$j.pid` is mesh's
            // replacement for bash's `$!`, and `$j` is what `fg` / `wait` /
            // `kill` take. Which job the launch created is read off the table
            // rather than threaded back through the pipeline's status.
            let launched = shell.jobs.next_id();
            match run_ast_pipeline(pipeline, true, last, shell) {
                // A launch that registered nothing — inside a forked stage, or
                // one that failed before it had a process group — has no handle
                // to give, so the status stands in as it did before.
                Step::Continue(code) => Ok(if shell.jobs.holds(launched) {
                    Value::Job(launched)
                } else {
                    Value::Integer(i64::from(code))
                }),
                step => Err(step),
            }
        }
        E::Capture(source) => capture_source(source, last, in_function, shell),
        E::Range {
            start,
            end,
            inclusive,
        } => {
            let Some(start) = range_endpoint(start.as_deref(), 0, last, in_function, shell)? else {
                return Ok(control_placeholder());
            };
            let Some(end) = end.as_deref() else {
                return runtime_error("an open-ended range cannot be used as a value");
            };
            let Some(end) = range_endpoint(Some(end), 0, last, in_function, shell)? else {
                return Ok(control_placeholder());
            };
            let stop = if *inclusive {
                end.checked_add(1)
                    .ok_or_else(|| runtime_message("range endpoint overflow"))?
            } else {
                end
            };
            Ok(Value::List((start..stop).map(Value::Integer).collect()))
        }
        E::Call { callee, arguments } => eval_call(callee, arguments, last, in_function, shell),
        // A lambda evaluates to a function value, carrying its signature and body.
        // Nothing else is captured: a call gets a fresh function-local scope and
        // the globals, the same two levels a named `func` gets (`DESIGN.md`
        // §"Variables and assignment"), so a lambda written inside a function
        // cannot read that function's locals.
        E::Lambda { parameters, body } => Ok(Value::Function(vars::FuncValue::lambda(
            parameters.clone(),
            body.clone(),
        ))),
        // A bare `:name` is the function that applies that modifier — the value, not
        // an application, so nothing is applied here. Whether the name is one the
        // engine can actually apply is checked at the call, where the argument that
        // would have failed a lambda body is in hand (`apply_modifier_ref`).
        //
        // `:capture` is the exception, and it has to be refused *here* rather than at
        // the call: it wraps an **invocation** rather than transforming a value
        // (`GRAMMAR.md`), so by the time a call could reject it the very call it was
        // meant to capture has already run uncaptured. Refusing the value means there
        // is nothing to call.
        E::ModifierRef(name) if name == "capture" => runtime_error(
            ":capture applies to a call — write `f(…):capture`, or `$(…)` for a command's output",
        ),
        E::ModifierRef(name) => Ok(Value::Function(vars::FuncValue::modifier(name.clone()))),
    }
}

fn range_endpoint(
    expression: Option<&parser::Expr>,
    default: i64,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<i64>, Step> {
    let Some(expression) = expression else {
        return Ok(Some(default));
    };
    // `None` means the endpoint raised `break`/`continue`; the caller stops rather
    // than type-checking a value that was never produced.
    let Some(value) = eval_operand(expression, last, in_function, shell)? else {
        return Ok(None);
    };
    match value {
        Value::Integer(value) => Ok(Some(value)),
        _ => runtime_error("range endpoints must be integers"),
    }
}

/// How to name a computed callee in a diagnostic. A variable is the readable
/// case and the only one reachable today; anything else is described by shape
/// rather than spelled back, since the parser keeps no source text for it.
fn callee_description(callee: &parser::Expr) -> String {
    match callee {
        parser::Expr::Variable(name) => name.value.trim_start_matches('$').to_string(),
        _ => "call target".to_string(),
    }
}

fn eval_call(
    callee: &parser::Expr,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    // A bare name names the function store (`f(...)`) or the `re(...)` builder.
    // Anything else has to *produce* a function value — today a lambda reached
    // through the variable it was bound to, `$double(5)`. Reaching a lambda needs
    // the `$` because a bare word is a literal string everywhere else, so
    // `double(5)` would look for a *declared* `double`.
    let parser::Expr::Scalar(word) = callee else {
        // The callee is the call's operand, not its result, so its own
        // bookkeeping is put back — the same treatment a condition or an
        // assignment's right-hand side gets.
        let target = eval_operand_of(callee, last, in_function, shell)?;
        if shell.control.is_some() {
            return Ok(control_placeholder());
        }
        let Value::Function(function) = target else {
            return runtime_error(format!(
                "{}: value is not callable",
                callee_description(callee)
            ));
        };
        // A modifier reference has no signature to match against: it takes exactly
        // one thing and applies itself to it, so the argument list is checked here
        // rather than by `bind_arguments`.
        if let Some(modifier) = function.modifier_name() {
            return call_modifier_ref(modifier, arguments, last, in_function, shell);
        }
        let (params, body) = function
            .as_lambda()
            .expect("a callable is a lambda or a modifier reference");
        return call_signature_for_value(
            &callee_description(callee),
            params,
            body,
            arguments,
            last,
            in_function,
            shell,
        );
    };
    let name = word.value.text();
    if name != "re" {
        // A user function called for its value; an external (or unknown) command
        // has no return value — point at `$(…)` for its output instead.
        if shell.funcs.get(&name).is_some() {
            return call_func_for_value(&name, arguments, last, in_function, shell);
        }
        return runtime_error(format!(
            "{name}: a command has no return value; use `$({name} …)` to capture its output"
        ));
    }
    let mut pattern = None;
    let mut literal = false;
    let mut case_insensitive = false;
    for argument in arguments {
        match argument {
            parser::Argument::Positional(expression) if pattern.is_none() => {
                match eval_expr(expression, last, in_function, shell)? {
                    Value::String(value) => pattern = Some(value),
                    _ => return runtime_error("re() pattern must be a string"),
                }
            }
            parser::Argument::Named(name, expression)
                if matches!(name.as_str(), "literal" | "ignore-case") =>
            {
                let Value::Boolean(value) = eval_expr(expression, last, in_function, shell)? else {
                    return runtime_error(format!("re() `{name}` must be a boolean"));
                };
                if name == "literal" {
                    literal = value;
                } else {
                    case_insensitive = value;
                }
            }
            parser::Argument::Spread(_) => {
                return runtime_error("re() does not accept spread arguments");
            }
            _ => return runtime_error("invalid re() argument"),
        }
    }
    let Some(mut pattern) = pattern else {
        return runtime_error("re() requires one pattern string");
    };
    if literal {
        pattern = regex::escape(&pattern);
    }
    let mut value = RegexValue::new(pattern);
    value.case_insensitive = case_insensitive;
    compile_regex(&value).map_err(runtime_message)?;
    Ok(Value::Regex(value))
}

/// Is `name` a modifier that requires a parenthesized argument list? Used only to
/// give a clearer error when such a modifier is written bare (`:split` without
/// arguments) rather than the generic "not implemented yet".
fn modifier_takes_arguments(name: &str) -> bool {
    matches!(name, "join" | "split" | "map" | "filter" | "each")
}

/// Apply an argument-free modifier to a value.
///
/// Which modifier `:name` *is* depends on the value: on a regex the flag names are
/// its own (`:i`, `:x`, `:extended`), and `:x` in particular is the extended-syntax
/// flag there and the executable-file filter everywhere else. Shared by the postfix
/// path and [`apply_modifier_ref`] so a reference cannot answer differently from the
/// `$r:i` it is defined to mean.
fn apply_argument_free_modifier(name: &str, mut value: Value) -> Result<Value, Step> {
    if let Value::Regex(regex) = &mut value {
        match name {
            "i" | "ignorecase" => regex.case_insensitive = true,
            "m" | "multiline" => regex.multi_line = true,
            "s" | "dotall" => regex.dot_matches_new_line = true,
            "x" | "extended" => regex.ignore_whitespace = true,
            _ => return runtime_error(format!("modifier :{name} is not valid for a regex")),
        }
        compile_regex(regex).map_err(runtime_message)?;
        return Ok(value);
    }
    let Some(modifier) = expand::Modifier::from_name(name) else {
        if modifier_takes_arguments(name) {
            return runtime_error(format!("modifier :{name} requires an argument"));
        }
        return runtime_error(format!("modifier :{name} is not implemented yet"));
    };
    expand::apply_modifier(value, modifier).map_err(|error| runtime_message(error.to_string()))
}

/// Evaluate a modifier that carries a parenthesized argument list (`:split(SEP)`,
/// `:map(f)`). The argument-free set is handled by [`expand::apply_modifier`].
fn eval_modifier_with_arguments(
    name: &str,
    value: Value,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    match name {
        "split" | "join" => {
            let Some(separator) =
                single_string_argument(name, arguments, last, in_function, shell)?
            else {
                return Ok(control_placeholder());
            };
            let result = if name == "split" {
                expand::split_value(value, &separator)
            } else {
                expand::join_value(value, &separator)
            };
            result.map_err(runtime_message)
        }
        // The higher-order modifiers. Each takes one **callable** — a lambda, or a
        // named function reached through a variable — and applies it per element.
        // They are the reason lambdas exist (`DESIGN.md` §"Calling for a value, and
        // lambdas"), and they go through the same call machinery a written call
        // does, so a `return`, a bad arity, or an `exit` inside the callable behaves
        // exactly as it would anywhere else.
        "map" | "filter" | "each" => {
            let Some(callable) =
                single_callable_argument(name, arguments, last, in_function, shell)?
            else {
                return Ok(control_placeholder());
            };
            let Value::List(elements) = value else {
                return runtime_error(format!(
                    "modifier :{name} requires a list; for a map use `:keys` or `:values` first"
                ));
            };
            // Named once, not per element: the label only reaches a diagnostic.
            let label = format!(":{name}");
            // `:each` never pushes, so it reserves nothing: holding a second
            // list-sized allocation for a vector that is discarded would double the
            // peak for the one modifier that produces no list.
            let mut mapped = Vec::with_capacity(if name == "each" { 0 } else { elements.len() });
            for element in elements {
                match name {
                    // The element is handed over, not copied: `:map` and `:each` have
                    // no use for it afterwards, and a nested collection would be a
                    // deep clone per iteration. Only `:filter` keeps it, because it
                    // is the thing being kept.
                    "map" => {
                        mapped.push(call_callable_for_value(&label, &callable, element, shell)?);
                    }
                    "filter" => {
                        let produced =
                            call_callable_for_value(&label, &callable, element.clone(), shell)?;
                        // A **boolean**, not a truthy value. mesh's truthiness is the
                        // shell's — an integer is true when it is *zero* — so reading
                        // a predicate loosely would make `:filter(func(x) { $x })`
                        // keep the zeros, and `:filter(:dir)` keep everything, since
                        // a dirname is always a non-empty string. `DESIGN.md` raises
                        // exactly that footgun as an open question and leans loud; a
                        // predicate that must say `true` or `false` cannot fall into
                        // it.
                        match produced {
                            Value::Boolean(true) => mapped.push(element),
                            Value::Boolean(false) => {}
                            other => {
                                return runtime_error(format!(
                                    "modifier :filter predicate must return a boolean, got {}",
                                    value_kind(&other)
                                ));
                            }
                        }
                    }
                    // `:each` runs for effect. Its result is the empty string —
                    // mesh's "nothing produced", the same answer an empty function
                    // body gives — rather than the list, so a chain cannot silently
                    // read side-effecting code as a transform.
                    _ => {
                        call_callable_for_value(&label, &callable, element, shell)?;
                    }
                }
            }
            Ok(match name {
                "map" => Value::List(mapped),
                "filter" => Value::List(mapped),
                _ => Value::String(String::new()),
            })
        }
        _ if expand::Modifier::from_name(name).is_some() => {
            runtime_error(format!("modifier :{name} does not take arguments"))
        }
        _ => runtime_error(format!(
            "modifier :{name} arguments are not implemented yet"
        )),
    }
}

/// Evaluate a modifier's single positional argument and require it to be callable.
/// `None` means the argument raised `break`/`continue`: the caller stops rather than
/// type-checking a value that was never produced. Without that, the placeholder an
/// unwinding `eval_expr` returns is read as the argument itself and reported as a
/// type error — hiding the control flow behind a diagnostic about a string.
fn single_callable_argument(
    name: &str,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<vars::FuncValue>, Step> {
    let [parser::Argument::Positional(expression)] = arguments else {
        return runtime_error(format!("modifier :{name} takes exactly one argument"));
    };
    let Some(value) = eval_operand(expression, last, in_function, shell)? else {
        return Ok(None);
    };
    match value {
        Value::Function(function) => Ok(Some(function)),
        other => runtime_error(format!(
            "modifier :{name} argument must be a function, got {}",
            value_kind(&other)
        )),
    }
}

/// How to name a value's type in a diagnostic.
fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "a string",
        Value::Integer(_) => "an integer",
        Value::Boolean(_) => "a boolean",
        Value::List(_) => "a list",
        Value::Map(_) => "a map",
        Value::Regex(_) => "a regex",
        Value::Glob(_) => "a glob",
        Value::Stream(_) | Value::Job(_) => "a stream handle",
        Value::Function(_) => "a function",
    }
}

/// Evaluate a modifier's single positional argument and require it to be a string.
///
/// `None` for pending control flow, as in [`single_callable_argument`]. Reading the
/// placeholder as the argument was worse here than a wrong type name: an empty
/// separator has its own rule, so `"a b":split(if c { break })` reported
/// "separator must not be empty" and buried the `break` entirely.
fn single_string_argument(
    name: &str,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<String>, Step> {
    let [parser::Argument::Positional(expression)] = arguments else {
        return runtime_error(format!("modifier :{name} takes exactly one argument"));
    };
    let Some(value) = eval_operand(expression, last, in_function, shell)? else {
        return Ok(None);
    };
    match value {
        Value::String(value) => Ok(Some(value)),
        _ => runtime_error(format!("modifier :{name} argument must be a string")),
    }
}

fn runtime_message(message: impl std::fmt::Display) -> Step {
    note!("mesh: {message}");
    Step::Continue(1)
}

fn runtime_error<T>(message: impl std::fmt::Display) -> Result<T, Step> {
    Err(runtime_message(message))
}

/// Capturing the shell's own output descriptors.
///
/// The invariant: **every `thread::scope` around a [`Diverted`] holds a
/// [`RestoreOnUnwind`]**, or a panic inside the scope hangs the shell — the join
/// waits on a reader that waits on an EOF the still-diverted write end will never
/// send. `Diverted` is private to this module and only the two `with_*_captured`
/// helpers own a scope, so a new capture path cannot skip the guard.
mod capture {
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::FromRawFd;

    use super::{Shell, Step, runtime_error, runtime_message};

    /// One descriptor diverted to a pipe for the duration of a capture: the pipe's
    /// read end, and the `dup` of the original to put back afterwards.
    struct Diverted {
        reader: File,
        /// The `dup` of the original descriptor, or `-1` once put back. A `Cell` so
        /// restoring takes `&self`: the pipe's read end is borrowed by the draining
        /// thread at the moment the descriptor has to go back.
        saved: std::cell::Cell<i32>,
        fd: i32,
    }

    impl Diverted {
        /// Point `fd` at a fresh pipe, keeping a copy of what was there.
        ///
        /// Everything this holds is **close-on-exec**, so a command the capture runs
        /// inherits only the standard descriptors `dup2` installs. Without that, the
        /// backup of the real stdout is just another open descriptor in the child, and
        /// `sh -c 'echo escaped >&5'` writes straight past the capture to the terminal;
        /// the pipe's own read end leaks the same way. `dup2` clears the flag on the
        /// descriptor it installs, so 0/1/2 still cross `exec` as they must.
        fn new(fd: i32) -> io::Result<Self> {
            let mut fds = [0; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // `F_DUPFD_CLOEXEC` rather than `dup` + a second `fcntl`: one call, and no
            // window where a concurrent `exec` could inherit the backup.
            let saved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            let read_cloexec = unsafe { libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC) };
            if saved < 0 || read_cloexec < 0 || unsafe { libc::dup2(fds[1], fd) } < 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::close(fds[0]);
                    libc::close(fds[1]);
                    if saved >= 0 {
                        libc::close(saved);
                    }
                }
                return Err(error);
            }
            // The write end lives on only as `fd` itself; keeping this copy open would
            // stop the reader from ever seeing EOF.
            unsafe { libc::close(fds[1]) };
            Ok(Self {
                reader: unsafe { File::from_raw_fd(fds[0]) },
                saved: std::cell::Cell::new(saved),
                fd,
            })
        }

        /// Put the original descriptor back. Called explicitly before the pipe is
        /// drained, so a diagnostic raised while reading still reaches the real stderr;
        /// `Drop` covers every path that leaves before reaching that point.
        ///
        /// Idempotent: `saved` is cleared, so the later `Drop` does nothing rather than
        /// `dup2`-ing a descriptor that has already been closed.
        fn restore(&self) {
            let saved = self.saved.replace(-1);
            if saved < 0 {
                return;
            }
            unsafe {
                libc::dup2(saved, self.fd);
                libc::close(saved);
            }
        }
    }

    impl Drop for Diverted {
        /// A capture must never leave the shell's own descriptor pointing at a pipe
        /// with no reader — later output would be lost or fail with `EPIPE`, and the
        /// `dup` would leak. Diverting the *second* descriptor can fail (a process near
        /// `RLIMIT_NOFILE`), and an argument can raise control flow mid-capture, so the
        /// restore cannot rely on reaching the end of the function.
        fn drop(&mut self) {
            self.restore();
        }
    }

    /// Puts diverted descriptors back if the scope they were diverted for **unwinds**.
    ///
    /// Must be a **closure local**, so it drops before `thread::scope` joins. It
    /// borrows rather than owns because the reader threads borrow the same
    /// descriptors, which would keep their own `Drop` from running here (`E0713`).
    /// The second is optional: `$(…)` diverts stdout alone, `:capture` both.
    struct RestoreOnUnwind<'a>(&'a Diverted, Option<&'a Diverted>);

    impl Drop for RestoreOnUnwind<'_> {
        fn drop(&mut self) {
            self.0.restore();
            if let Some(second) = self.1 {
                second.restore();
            }
        }
    }

    /// Divert both of the shell's output descriptors to pipes, run `body`, and return
    /// what it produced beside the two captured streams.
    ///
    /// Both pipes are drained on their own threads, which is load-bearing rather than
    /// tidy: reading them in sequence deadlocks as soon as `body` fills the 64 KiB
    /// buffer on the channel that is not being read yet.
    ///
    /// The descriptors go back *before* the pipes are drained, so a diagnostic raised
    /// while reading still reaches the real stderr — and `Diverted::drop` covers the
    /// paths that leave early, including a failure to divert the second descriptor.
    pub(super) fn with_channels_captured<T>(
        shell: &mut Shell,
        body: impl FnOnce(&mut Shell) -> T,
    ) -> Result<(T, String, String), Step> {
        let out = Diverted::new(libc::STDOUT_FILENO).map_err(runtime_message)?;
        // Should this one fail, `out` is dropped on the way out and stdout goes back;
        // leaving it on a pipe with no reader would lose every later write.
        let err = Diverted::new(libc::STDERR_FILENO).map_err(runtime_message)?;
        let mut out_reader = &out.reader;
        let mut err_reader = &err.reader;
        let (produced, out_read, err_read) = std::thread::scope(|scope| {
            let _restore = RestoreOnUnwind(&out, Some(&err));
            let read_out = scope.spawn(move || {
                let mut text = String::new();
                out_reader.read_to_string(&mut text).map(|_| text)
            });
            let read_err = scope.spawn(move || {
                let mut text = String::new();
                err_reader.read_to_string(&mut text).map(|_| text)
            });
            let produced = body(shell);
            let _ = io::stdout().flush();
            out.restore();
            err.restore();
            (produced, read_out.join(), read_err.join())
        });
        let captured = |joined: std::thread::Result<io::Result<String>>| match joined {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(error)) => runtime_error(error),
            Err(_) => runtime_error("capture reader panicked"),
        };
        Ok((produced, captured(out_read)?, captured(err_read)?))
    }

    /// Divert **stdout only** — what `$(…)` captures — run `body`, and return what it
    /// produced beside the captured text.
    ///
    /// Separate from [`with_channels_captured`] because `$(…)` deliberately leaves
    /// stderr alone, but built the same way and under the module's invariant.
    pub(super) fn with_stdout_captured<T>(
        shell: &mut Shell,
        body: impl FnOnce(&mut Shell) -> T,
    ) -> Result<(T, String), Step> {
        let diverted = Diverted::new(libc::STDOUT_FILENO).map_err(runtime_message)?;
        let mut reader = &diverted.reader;
        let (produced, read) = std::thread::scope(|scope| {
            let _restore = RestoreOnUnwind(&diverted, None);
            let read = scope.spawn(move || {
                let mut output = String::new();
                reader.read_to_string(&mut output).map(|_| output)
            });
            let produced = body(shell);
            let _ = io::stdout().flush();
            diverted.restore();
            (produced, read.join())
        });
        match read {
            Ok(Ok(output)) => Ok((produced, output)),
            Ok(Err(error)) => runtime_error(error),
            Err(_) => runtime_error("capture reader panicked"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{Diverted, with_channels_captured, with_stdout_captured};
        use crate::repl::Shell;
        use std::sync::{Mutex, MutexGuard};

        /// These tests hijack the **process-wide** stdout, so they cannot run
        /// concurrently with each other: two overlapping diversions interleave their
        /// save and restore, and one ends up handing the other's pipe back as "the
        /// original" — stranding a reader that then waits for an EOF nobody will
        /// send. libtest runs tests in parallel by default, so they take this lock.
        fn exclusive() -> MutexGuard<'static, ()> {
            static LOCK: Mutex<()> = Mutex::new(());
            LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        #[test]
        fn a_diverted_descriptor_goes_back_when_it_is_dropped() {
            let _exclusive = exclusive();
            use std::io::{Read, Write};
            use std::os::fd::{AsRawFd, FromRawFd};

            // Stand in for "the shell's stdout": a pipe whose read end we can inspect.
            let mut original = [0; 2];
            assert!(unsafe { libc::pipe(original.as_mut_ptr()) } >= 0);
            let mut original_read = unsafe { std::fs::File::from_raw_fd(original[0]) };
            let mut target = unsafe { std::fs::File::from_raw_fd(original[1]) };
            let fd = target.as_raw_fd();

            {
                let diverted = Diverted::new(fd).expect("divert");
                let mut writer = unsafe { std::fs::File::from_raw_fd(libc::dup(fd)) };
                writeln!(writer, "captured").unwrap();
                drop(writer);
                // Left without an explicit restore, exactly as an early return would.
                drop(diverted);
            }

            writeln!(target, "after").unwrap();
            drop(target);
            let mut landed = String::new();
            original_read.read_to_string(&mut landed).unwrap();
            assert_eq!(
                landed, "after\n",
                "the descriptor should be back, and only post-restore writes should land"
            );
        }

        /// A panic inside the capture must not hang the shell. `thread::scope` joins the
        /// readers while unwinding, and a reader waiting for EOF on a live write end
        /// would wait forever — so the descriptors have to go back *before* that join.
        /// A panicking body is the reachable version of `Scope::spawn` failing when the
        /// OS refuses a thread; both unwind through the same path.
        #[test]
        fn a_panic_inside_a_capture_does_not_hang() {
            let _exclusive = exclusive();
            // Written straight to the descriptor: this test runs under libtest, which
            // intercepts `print!` above the fd layer, so a `print!` would never reach
            // the pipe being tested.
            fn write_fd(fd: i32, text: &str) {
                unsafe {
                    libc::write(fd, text.as_ptr().cast(), text.len());
                }
            }

            let mut shell = Shell::new();
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = with_channels_captured(&mut shell, |_| {
                    write_fd(libc::STDOUT_FILENO, "before the panic");
                    panic!("body blew up");
                });
            }));
            assert!(
                unwound.is_err(),
                "the panic should propagate, not be swallowed"
            );

            // Reaching here at all is the point — a hang is the failure this guards
            // against. That the next capture still carries a write through proves the
            // descriptors went back: one left on a reader-less pipe would lose it.
            //
            // `contains`, not `==`: this diverts the *process-wide* stdout while libtest
            // runs other tests on other threads, so anything they write lands in the
            // capture too. Asserting equality passed locally and failed in CI on a
            // neighbour's progress line. The exact bytes are pinned by
            // `a_diverted_descriptor_goes_back_when_it_is_dropped`, which owns its
            // descriptor and cannot race.
            let mut shell = Shell::new();
            let (_, out, err) = with_channels_captured(&mut shell, |_| {
                write_fd(libc::STDOUT_FILENO, "after-the-panic");
                write_fd(libc::STDERR_FILENO, "err-after-the-panic");
            })
            .expect("capture again");
            assert!(out.contains("after-the-panic"), "{out:?}");
            assert!(err.contains("err-after-the-panic"), "{err:?}");
        }

        /// `$(…)` diverts one descriptor where `:capture` diverts two, but the hazard is
        /// the same and so is the guard: `thread::scope` joins the reader while
        /// unwinding, so the descriptor has to go back first or the join waits for an EOF
        /// that cannot come.
        ///
        /// This is the case that was missed once. `capture_source` was moved onto
        /// [`Diverted`] for the close-on-exec and always-restore guarantees and kept the
        /// original hang-on-panic shape, because the guard lived inside the *other*
        /// capture's closure. Both now go through a helper that owns the scope, so the
        /// guard is not something a caller can forget.
        #[test]
        fn a_panic_inside_a_single_descriptor_capture_does_not_hang() {
            let _exclusive = exclusive();
            fn write_fd(fd: i32, text: &str) {
                unsafe {
                    libc::write(fd, text.as_ptr().cast(), text.len());
                }
            }

            let mut shell = Shell::new();
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = with_stdout_captured(&mut shell, |_| {
                    write_fd(libc::STDOUT_FILENO, "before the panic");
                    panic!("body blew up");
                });
            }));
            assert!(
                unwound.is_err(),
                "the panic should propagate, not be swallowed"
            );

            // Reaching here is the assertion — the failure this guards against is a hang.
            // `contains` rather than `==`: this diverts process-wide stdout while libtest
            // runs other tests on other threads, so their output lands here too.
            let mut shell = Shell::new();
            let (_, out) = with_stdout_captured(&mut shell, |_| {
                write_fd(libc::STDOUT_FILENO, "after-the-panic")
            })
            .expect("capture again");
            assert!(out.contains("after-the-panic"), "{out:?}");
        }
    }
}

/// `f(…):capture` — run the call and return a **record of every channel**:
/// `.value` (the return value), `.out` and `.err` (its stdout / stderr), and
/// `.status` (the exit int), per `DESIGN.md` §"Calling for a value".
///
/// Both descriptors are diverted for the duration, and both are drained on
/// threads: a body that fills the 64 KiB pipe buffer on one channel would
/// otherwise block forever while nothing was reading it.
///
/// `.out`/`.err` are the bytes **as written** — no trailing-newline trim, unlike
/// `$(…)` — because the record is meant to bake in no split policy: `:lines`,
/// `:split`, and friends are how you divide them up.
fn capture_call(
    callee: &parser::Expr,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    // A *command* — builtin or external — has no return value, so its record comes
    // back without `.value`. This is the one case where a value call on a command
    // is allowed at all, since it asks for the channel record rather than a value
    // the command lacks.
    let command = match callee {
        parser::Expr::Scalar(word) => {
            let name = word.value.text();
            (name != "re" && shell.funcs.get(&name).is_none()).then_some(name)
        }
        _ => None,
    };
    if let Some(name) = command {
        return capture_command(&name, arguments, last, in_function, shell);
    }

    let (outcome, out_text, err_text) = capture::with_channels_captured(shell, |shell| {
        eval_call(callee, arguments, last, in_function, shell)
    })?;
    // A runtime error in the call fails the enclosing statement, as it does for a
    // plain value call — `:capture` observes a call's channels, it does not turn a
    // failure into data. Its diagnostic went to the captured stderr, so it is
    // re-reported here or it would vanish with the record.
    let value = match outcome {
        Ok(value) => value,
        Err(step) => {
            if !err_text.is_empty() {
                note!("{}", err_text.trim_end_matches('\n'));
            }
            return Err(step);
        }
    };
    Ok(channel_record(
        Some(value.clone()),
        out_text,
        err_text,
        status_of(&value),
    ))
}

/// `cmd(args):capture` on a **command**: the same record minus `.value`, since
/// there is none — reading it is then a loud no-such-field, exactly as
/// `DESIGN.md` specifies.
///
/// "Command" covers a builtin as well as an external. Both are reached through
/// `run_expanded`, the post-expansion half of `run_command`, so `puts(x):capture`
/// runs *the builtin* rather than looking for a program called `puts` — and
/// `pwd():capture` does not silently reach `/bin/pwd`. It also means an `exit`
/// still leaves the shell: its `Step` unwinds out of the capture, with the
/// descriptors put back on the way.
///
/// Positional arguments only. A command has no signature and no canonical
/// named-option encoding, so a `key: value` pair or a map spread has nothing to
/// bind to; pass the intended argv token as a positional instead.
fn capture_command(
    name: &str,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    // The arguments are evaluated *inside* the capture, not before it. An argument
    // can print — `echo(side()):capture` — and `:capture` is defined over the whole
    // invocation, so everything written while evaluating the call belongs in the
    // record. Doing it first would put that output on the terminal and leave the
    // record holding only the command's own, which is not what the same argument
    // does in a captured mesh call.
    let (outcome, out_text, err_text) = capture::with_channels_captured(shell, |shell| {
        let mut words = vec![name.to_string()];
        for argument in arguments {
            match argument {
                parser::Argument::Positional(expression) => {
                    let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                        return Ok(None);
                    };
                    words.extend(argv_words(&value, name)?);
                }
                parser::Argument::Spread(expression) => {
                    let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                        return Ok(None);
                    };
                    match value {
                        Value::List(values) => {
                            for value in &values {
                                words.extend(argv_words(value, name)?);
                            }
                        }
                        _ => {
                            return runtime_error(format!(
                                "{name}: only a list can be spread into a command's arguments"
                            ));
                        }
                    }
                }
                parser::Argument::Named(option, _) => {
                    return runtime_error(format!(
                        "{name}: `{option}:` needs a signature to bind to; an external takes \
                         positional arguments only — pass `\"--{option}=…\"` as one"
                    ));
                }
            }
        }
        // `run_expanded` resolves builtins → functions → external exactly as command
        // position does. A function name cannot arrive here (the caller sent it to
        // the value-call path), so this is a builtin or an external.
        match run_expanded(words, last, shell) {
            Step::Continue(status) => Ok(Some(status)),
            // `exit` leaves the shell rather than reporting a status into a record.
            step => Err(step),
        }
    })?;
    // An argument that failed did so with its diagnostic on the captured stderr, so
    // it is re-reported here rather than vanishing with the record — the same rule
    // `capture_call` applies to a call that never ran.
    let status = match outcome {
        Ok(Some(status)) => status,
        // Control flow is unwinding; the statement layer acts on `shell.control`.
        Ok(None) => return Ok(control_placeholder()),
        Err(step) => {
            if !err_text.is_empty() {
                note!("{}", err_text.trim_end_matches('\n'));
            }
            return Err(step);
        }
    };
    // A nonzero exit is the point of asking, not a failure: `grep(x):capture` on no
    // match reports status 1 in the record rather than failing the statement.
    Ok(channel_record(None, out_text, err_text, status))
}

/// The record `:capture` returns. An ordered map, so `$r.value` / `$r.out` read it
/// through the usual member access and a missing `.value` is the usual loud
/// "map key not found".
fn channel_record(value: Option<Value>, out: String, err: String, status: u8) -> Value {
    let mut entries = Vec::with_capacity(4);
    if let Some(value) = value {
        entries.push(("value".to_string(), value));
    }
    entries.push(("out".to_string(), Value::String(out)));
    entries.push(("err".to_string(), Value::String(err)));
    entries.push(("status".to_string(), Value::Integer(status.into())));
    Value::Map(entries)
}

/// The argv tokens a value contributes to an external's command line — the same
/// bytes-only rule expansion applies, since an external takes bytes.
fn argv_words(value: &Value, name: &str) -> Result<Vec<String>, Step> {
    match value {
        Value::String(text) => Ok(vec![text.clone()]),
        Value::Integer(number) => Ok(vec![number.to_string()]),
        Value::Boolean(flag) => Ok(vec![flag.to_string()]),
        Value::List(_) => runtime_error(format!(
            "{name}: a list needs `...` to become command arguments"
        )),
        Value::Map(_) => runtime_error(format!("{name}: a map cannot be a command argument")),
        Value::Regex(_) | Value::Glob(_) => {
            runtime_error(format!("{name}: a pattern cannot be a command argument"))
        }
        Value::Stream(_) => runtime_error(format!("{name}: a stream handle has no text form")),
        // The handle is the *reference*: `kill` and the job builtins take one
        // directly, which is exactly why it never becomes bytes for anything
        // else — `kill $j` is a job where `kill 49001` is a pid.
        Value::Job(_) => runtime_error(format!("{name}: a job handle has no text form")),
        Value::Function(_) => runtime_error(format!("{name}: a function value has no text form")),
    }
}

fn capture_source(
    source: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let (step, output) =
        capture::with_stdout_captured(shell, |shell| run_source(source, last, in_function, shell))?;
    match step {
        Step::Continue(0) => Ok(Value::String(output.trim_end_matches('\n').to_string())),
        Step::Continue(code) => Err(Step::Continue(code)),
        step => Err(step),
    }
}

fn eval_if_expr(
    node: &parser::IfExpr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let Some(code) = condition_status(&node.condition, last, in_function, shell)? else {
        // As in `run_ast_if`: no truth value, so no branch runs.
        return Ok(control_placeholder());
    };
    if code == 0 {
        eval_value_body(&node.then_body, 0, in_function, shell)
    } else {
        match &node.else_branch {
            Some(parser::ElseBranch::If(next)) => eval_if_expr(next, code, in_function, shell),
            Some(parser::ElseBranch::Block(body)) => eval_value_body(body, 0, in_function, shell),
            None => Ok(Value::String(String::new())),
        }
    }
}

fn eval_match_expr(
    node: &parser::MatchExpr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let subject = eval_operand_of(&node.value, last, in_function, shell)?;
    if shell.control.is_some() {
        // As in `run_ast_match`: no subject, so no arm.
        return Ok(control_placeholder());
    }
    for arm in &node.arms {
        let matched = match_bindings(&arm.pattern, &subject, last, in_function, shell)?;
        // As in `run_ast_match`: a pattern that raised control stops the match.
        if shell.control.is_some() {
            return Ok(control_placeholder());
        }
        let Some(bindings) = matched else { continue };
        let snapshot = shell.vars.active_snapshot();
        validate_bindings(&bindings).map_err(runtime_message)?;
        commit_bindings(bindings, &mut shell.vars);
        if let Some(guard) = &arm.guard {
            let passed = truthy(&eval_operand_of(guard, last, in_function, shell)?);
            // As in `run_ast_match`: a guard that raised `break`/`continue`
            // produced no truth value, so no later arm may be tried.
            if shell.control.is_some() {
                shell.vars.restore_active(snapshot);
                return Ok(control_placeholder());
            }
            if !passed {
                shell.vars.restore_active(snapshot);
                continue;
            }
        }
        return eval_value_body(&arm.body, 0, in_function, shell);
    }
    Ok(Value::String(String::new()))
}

fn match_bindings(
    pattern: &parser::MatchPattern,
    subject: &Value,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<Vec<(String, Value)>>, Step> {
    let bindings = match pattern {
        parser::MatchPattern::Wildcard => Some(Vec::new()),
        parser::MatchPattern::Binding(pattern) => {
            pattern_bindings(pattern, subject).map_err(runtime_message)?
        }
        parser::MatchPattern::Value(pattern) => {
            let pattern_value = eval_expr(pattern, last, in_function, shell)?;
            let matched = match pattern_value {
                Value::Regex(regex) => match subject {
                    Value::String(text) => compile_regex(&regex)
                        .map_err(runtime_message)?
                        .is_match(text),
                    _ => false,
                },
                Value::Glob(pattern) => match subject {
                    Value::String(text) => glob::Pattern::new(&pattern)
                        .map_err(|error| runtime_message(format!("invalid glob pattern: {error}")))?
                        .matches(text),
                    _ => false,
                },
                Value::List(values) if matches!(pattern, parser::Expr::Range { .. }) => {
                    values.contains(subject)
                }
                value => value == *subject,
            };
            matched.then(Vec::new)
        }
        parser::MatchPattern::Alternation(patterns) => {
            let mut matched = None;
            for pattern in patterns {
                if let Some(bindings) = match_bindings(pattern, subject, last, in_function, shell)?
                {
                    matched = Some(bindings);
                    break;
                }
            }
            matched
        }
    };
    if let Some(bindings) = &bindings {
        validate_bindings(bindings).map_err(runtime_message)?;
    }
    Ok(bindings)
}

fn eval_value_body(
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    if let [statement] = body.statements.as_slice()
        && !statement.background
        && statement.and_or.rest.is_empty()
        && let parser::Executable::Pipeline(parser::Pipeline { stages, .. }) =
            &statement.and_or.first
        && let [parser::Command { items, guard: None }] = stages.as_slice()
        && let [parser::CommandItem::Word(word)] = items.as_slice()
    {
        return eval_expr(
            &parser::Expr::Scalar(word.clone()),
            last,
            in_function,
            shell,
        );
    }
    let value_final = body.statements.last().is_some_and(|statement| {
        !statement.background
            && statement.and_or.rest.is_empty()
            && matches!(
                statement.and_or.first,
                parser::Executable::Expression { .. }
                    | parser::Executable::If(_)
                    | parser::Executable::Match(_)
                    | parser::Executable::For { .. }
            )
    });
    if value_final {
        eval_body(body, last, in_function, shell)
    } else {
        capture_source(body, last, in_function, shell)
    }
}

fn eval_for_expr(
    bindings: &[parser::BindingPattern],
    iterable: &parser::Expr,
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    validate_patterns(bindings).map_err(runtime_message)?;
    let iterable = eval_operand_of(iterable, last, in_function, shell)?;
    if shell.control.is_some() {
        // As in `run_ast_for`: the flag belongs to the enclosing loop, and there
        // is nothing to iterate.
        return Ok(control_placeholder());
    }
    let values = iteration_values(iterable, bindings.len()).map_err(runtime_message)?;
    let mut results = Vec::new();
    shell.loop_depth += 1;
    for values in values {
        if let Err(message) = bind_iteration(bindings, values, shell) {
            shell.loop_depth -= 1;
            return runtime_error(message);
        }
        let result = match eval_body(body, 0, in_function, shell) {
            Ok(value) => value,
            Err(step) => {
                shell.loop_depth -= 1;
                return Err(step);
            }
        };
        match shell.control.take() {
            Some(parser::ControlKind::Break) => break,
            Some(parser::ControlKind::Continue) => continue,
            Some(parser::ControlKind::Return) => unreachable!(),
            None => results.push(result),
        }
    }
    shell.loop_depth -= 1;
    Ok(Value::List(results))
}

fn eval_body(
    body: &parser::Source,
    mut last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    // Whether *this* body produced anything. An empty body — or one whose every
    // statement was skipped — yields the empty string rather than inheriting
    // whatever the surrounding code last recorded.
    let mut recorded = false;
    for (index, statement) in body.statements.iter().enumerate() {
        let final_statement = index + 1 == body.statements.len();
        if final_statement && !statement.background && statement.and_or.rest.is_empty() {
            match &statement.and_or.first {
                parser::Executable::Expression { expression, guard } => {
                    // A guarded final expression still yields the body's value —
                    // when its guard passes. A guard that fails leaves it unrun,
                    // which produces nothing rather than producing emptiness, so
                    // an earlier statement's result still stands: the same answer
                    // a bare `return` in its place would carry.
                    return match guard_allows(guard.as_ref(), last, in_function, shell) {
                        Ok(true) => eval_expr(expression, last, in_function, shell),
                        Ok(false) if recorded => Ok(shell.result.clone()),
                        Ok(false) => Ok(Value::String(String::new())),
                        Err(step) => Err(step),
                    };
                }
                parser::Executable::If(node) => {
                    return eval_if_expr(node, last, in_function, shell);
                }
                parser::Executable::Match(node) => {
                    return eval_match_expr(node, last, in_function, shell);
                }
                parser::Executable::For {
                    bindings,
                    iterable,
                    body,
                } => {
                    return eval_for_expr(bindings, iterable, body, last, in_function, shell);
                }
                _ => {}
            }
        }
        match run_statement(statement, last, in_function, shell) {
            Step::Continue(code) => last = code,
            flow => return Err(flow),
        }
        recorded |= shell.produced != Produced::Nothing;
        if shell.control.is_some() {
            return Ok(Value::String(String::new()));
        }
    }
    // The last statement was not a tail expression — an `&&`/`||` list, a
    // command, a background job. Its recorded result is still the body's, the
    // same one a bare `return` would have carried out.
    if recorded {
        Ok(shell.result.clone())
    } else {
        Ok(Value::String(String::new()))
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.is_empty() && s != "false" && s != "0",
        Value::Integer(value) => *value == 0,
        Value::Boolean(value) => *value,
        Value::List(v) => !v.is_empty(),
        Value::Map(v) => !v.is_empty(),
        // A handle exists, which is all truthiness asks; `$j.state` is the
        // question to ask about a job, not whether the handle is "set".
        Value::Regex(_)
        | Value::Glob(_)
        | Value::Stream(_)
        | Value::Job(_)
        | Value::Function(_) => true,
    }
}

fn map_lookup(entries: &[(String, Value)], key: &str) -> Result<Value, Step> {
    entries
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.clone())
        .ok_or_else(|| runtime_message(format!("map key `{key}` not found")))
}
fn bool_value(value: bool) -> Value {
    Value::Boolean(value)
}
fn number(value: &Value) -> Result<i64, String> {
    match value {
        Value::Integer(value) => Ok(*value),
        _ => Err("expected integer".into()),
    }
}
fn checked_div(left: i64, right: i64) -> Result<i64, String> {
    if right == 0 {
        return Err("division by zero".into());
    }
    left.checked_div(right)
        .ok_or_else(|| "numeric overflow".into())
}

fn compile_regex(value: &RegexValue) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(&value.pattern)
        .case_insensitive(value.case_insensitive)
        .multi_line(value.multi_line)
        .dot_matches_new_line(value.dot_matches_new_line)
        .ignore_whitespace(value.ignore_whitespace)
        .build()
        .map_err(|error| format!("invalid regex: {error}"))
}

fn eval_binary(left: Value, op: parser::BinaryOp, right: Value) -> Result<Value, String> {
    use parser::BinaryOp::*;
    Ok(match op {
        Equal => bool_value(left == right),
        NotEqual => bool_value(left != right),
        Add => Value::Integer(
            number(&left)?
                .checked_add(number(&right)?)
                .ok_or("numeric overflow")?,
        ),
        Subtract => Value::Integer(
            number(&left)?
                .checked_sub(number(&right)?)
                .ok_or("numeric overflow")?,
        ),
        Multiply => Value::Integer(
            number(&left)?
                .checked_mul(number(&right)?)
                .ok_or("numeric overflow")?,
        ),
        Divide => Value::Integer(checked_div(number(&left)?, number(&right)?)?),
        Remainder => {
            let left = number(&left)?;
            let right = number(&right)?;
            if right == 0 {
                return Err("division by zero".into());
            }
            Value::Integer(left.checked_rem(right).ok_or("numeric overflow")?)
        }
        Less | LessEqual | Greater | GreaterEqual => {
            let ordering = match (&left, &right) {
                (Value::Integer(left), Value::Integer(right)) => left.cmp(right),
                (Value::String(left), Value::String(right)) => left.cmp(right),
                _ => return Err("comparison requires two integers or two strings".into()),
            };
            bool_value(match op {
                Less => ordering.is_lt(),
                LessEqual => !ordering.is_gt(),
                Greater => ordering.is_gt(),
                GreaterEqual => !ordering.is_lt(),
                _ => unreachable!(),
            })
        }
        And => bool_value(truthy(&left) && truthy(&right)),
        Or => bool_value(truthy(&left) || truthy(&right)),
        In => match right {
            Value::List(values) => bool_value(values.contains(&left)),
            Value::Map(values) => match left {
                Value::String(key) => {
                    bool_value(values.iter().any(|(candidate, _)| candidate == &key))
                }
                _ => return Err("map key must be a string".into()),
            },
            Value::String(text) => match left {
                Value::String(needle) => bool_value(text.contains(&needle)),
                _ => return Err("left operand of `in` must be a string".into()),
            },
            Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => {
                return Err("right operand of `in` must be a collection or string".into());
            }
        },
        Match | NotMatch => {
            let Value::String(text) = left else {
                return Err("left operand of `~` must be a string".into());
            };
            let matched = match right {
                Value::Regex(regex) => compile_regex(&regex)?.is_match(&text),
                Value::Glob(pattern) => glob::Pattern::new(&pattern)
                    .map_err(|error| format!("invalid glob pattern: {error}"))?
                    .matches(&text),
                Value::String(_) => return Err(
                    "right operand of `~` must be a regex or bare glob; use re(...) for a string pattern".into(),
                ),
                _ => return Err("right operand of `~` must be a regex or bare glob".into()),
            };
            bool_value(if op == Match { matched } else { !matched })
        }
    })
}

/// Run one pipeline. A single stage keeps the full command surface (assignments,
/// builtins, functions). A multi-stage pipeline (`|`) is external commands only
/// for now.
fn run_pipeline(mut stages: Vec<Stage>, background: bool, last: u8, shell: &mut Shell) -> Step {
    if stages.len() == 1 {
        run_single(stages.pop().unwrap(), background, last, shell)
    } else {
        run_multi(stages, background, last, shell)
    }
}

/// Run a one-stage pipeline. Without redirections this is the full command
/// surface: an assignment or a builtin/function/external command. A redirected
/// in-shell command — builtin or function — runs in the shell with the targets
/// applied to its own descriptors around the call, since there is no child to
/// configure. Backgrounding needs a child, so it goes through the pipeline path,
/// which forks the stage.
fn run_single(stage: Stage, background: bool, last: u8, shell: &mut Shell) -> Step {
    let Stage {
        words,
        redirs,
        pipe_stderr: _,
    } = stage;
    if redirs.is_empty() && !background {
        return run_command(words, last, shell);
    }
    // A function takes typed arguments in every position, so it is resolved before
    // the external argv rule turns a bare list into an error. Foreground: the
    // redirection applies to the shell's own descriptors around the in-process
    // call, since there is no child to configure. Background: there *is* a child —
    // the fork the pipeline path makes.
    if let Some(name) = command_name(&words, &shell.vars)
        && shell.funcs.get(&name).is_some()
    {
        // Expand the arguments *before* the targets are opened: `f * > summary`
        // must not see the `summary` the redirection is about to create. (The
        // external path likewise builds its argv before opening.)
        let arg_words: Vec<Word> = words.into_iter().skip(1).collect();
        let args = match expand_function_args(arg_words, &shell.vars) {
            Ok(args) => args,
            Err(step) => return step,
        };
        let opened = match expand_redirs(redirs, &shell.vars) {
            Ok(redirs) => redirs,
            Err(err) => {
                note!("mesh: {err}");
                return Step::Continue(1);
            }
        };
        if background {
            return Step::Continue(run_stages(
                vec![exec::Cmd {
                    words: vec![name],
                    redirs: opened,
                    pipe_stderr: false,
                    in_shell: true,
                }],
                vec![StageBody::Function(args)],
                background,
                last,
                shell,
            ));
        }
        return match exec::with_redirections(&opened, || dispatch_function_call(&name, args, shell))
        {
            Ok(step) => step,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                Step::Continue(1)
            }
        };
    }
    // A job builtin keeps its arguments typed here too, so a handle reaches it
    // through a redirection exactly as it does without one.
    let expanded = match job_builtin_words(&words, &shell.vars) {
        Some(words) => words,
        None => expand::expand(words, &shell.vars).map_err(|err| {
            note!("mesh: {err}");
            Step::Continue(1)
        }),
    };
    let argv = match expanded {
        Ok(argv) => argv,
        Err(step) => return step,
    };
    if argv.is_empty() {
        note!("mesh: redirection with no command is not supported yet");
        return Step::Continue(1);
    }
    // `return` is control flow handled on the no-redirection path; with a
    // redirection or in the background it never reaches that handler, so reject
    // it rather than launch an external `return` while the body keeps running.
    if argv[0] == "return" {
        note!("mesh: return: cannot be redirected or backgrounded");
        return Step::Continue(2);
    }
    // A redirected builtin runs in the shell like a redirected function: the
    // targets apply to the shell's own descriptors around the call, so there is
    // nothing to configure on a child.
    let builtin = builtins::is_builtin(&argv[0]);
    let opened = match expand_redirs(redirs, &shell.vars) {
        Ok(redirs) => redirs,
        Err(err) => {
            note!("mesh: {err}");
            return Step::Continue(1);
        }
    };
    if builtin && !background {
        return match exec::with_redirections(&opened, || run_expanded(argv, last, shell)) {
            Ok(step) => step,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                Step::Continue(1)
            }
        };
    }
    // Backgrounding needs a child even for an in-shell command, so it goes
    // through the pipeline path, which forks for a builtin. (A function was
    // resolved above, where its arguments keep their types.)
    Step::Continue(run_stages(
        vec![exec::Cmd {
            words: argv,
            redirs: opened,
            pipe_stderr: false,
            in_shell: builtin,
        }],
        vec![if builtin {
            StageBody::Builtin
        } else {
            StageBody::External
        }],
        background,
        last,
        shell,
    ))
}

/// Run a multi-stage pipeline (`a | b | c`). Each stage is an external command, a
/// builtin, or a function; the in-shell ones run in a forked stage so all the
/// stages run concurrently.
fn run_multi(stages: Vec<Stage>, background: bool, last: u8, shell: &mut Shell) -> Step {
    let mut cmds = Vec::with_capacity(stages.len());
    let mut bodies = Vec::with_capacity(stages.len());
    for stage in stages {
        let Stage {
            words,
            redirs,
            pipe_stderr,
        } = stage;
        // Command words expand before the redirect targets, the order `run_single`
        // uses, so a stage reports the same first failure the unpiped command
        // does — and `f * > summary` cannot glob the file the redirection is
        // about to create.
        //
        // Resolve a function *before* expanding for argv, exactly as
        // `run_command` does, so a bare list argument reaches it as one typed
        // value rather than meeting the external-command rule.
        let function =
            command_name(&words, &shell.vars).filter(|name| shell.funcs.get(name).is_some());
        let (stage_words, body) = if let Some(name) = function {
            let arg_words: Vec<Word> = words.into_iter().skip(1).collect();
            let args = match expand_function_args(arg_words, &shell.vars) {
                Ok(args) => args,
                Err(step) => return step,
            };
            // The arguments are typed values, and mesh has no implicit
            // stringification for a list or map, so only the name goes into the
            // words a job listing echoes back.
            (vec![name], StageBody::Function(args))
        } else {
            // A stage expands its own words, so a job builtin needs the same
            // handle-preserving conversion here as it gets elsewhere.
            let expanded = match job_builtin_words(&words, &shell.vars) {
                Some(words) => words,
                None => expand::expand(words, &shell.vars).map_err(|err| {
                    note!("mesh: {err}");
                    Step::Continue(1)
                }),
            };
            let argv = match expanded {
                Ok(argv) => argv,
                Err(step) => return step,
            };
            if argv.is_empty() {
                note!("mesh: empty command in a pipeline");
                return Step::Continue(1);
            }
            // `return` unwinds the enclosing function; it has no meaning as a
            // pipeline stage, so reject it rather than launch an external
            // `return`.
            if argv[0] == "return" {
                note!("mesh: return: cannot be used in a pipeline");
                return Step::Continue(2);
            }
            let body = if builtins::is_builtin(&argv[0]) {
                StageBody::Builtin
            } else {
                StageBody::External
            };
            (argv, body)
        };
        let opened = match expand_redirs(redirs, &shell.vars) {
            Ok(redirs) => redirs,
            Err(err) => {
                note!("mesh: {err}");
                return Step::Continue(1);
            }
        };
        cmds.push(exec::Cmd {
            words: stage_words,
            redirs: opened,
            pipe_stderr,
            in_shell: !matches!(body, StageBody::External),
        });
        bodies.push(body);
    }
    Step::Continue(run_stages(cmds, bodies, background, last, shell))
}

/// What an in-shell stage runs, kept beside the `exec::Cmd` describing it.
enum StageBody {
    /// An external program: `exec` runs it, there is no in-shell body.
    External,
    /// A builtin, run from the stage's expanded words.
    Builtin,
    /// A function, with its arguments already expanded as **typed values** — the
    /// same guarantee a plain call gives, so `f $xs` still passes one list.
    Function(Vec<(Value, bool)>),
}

/// Run `cmds` as a pipeline, giving each in-shell stage the body in `bodies`
/// (parallel to `cmds`) to run in its fork.
///
/// The job table is moved out of the shell for the call, since the pipeline holds
/// it while a stage body holds `&mut Shell`; the child swaps it back before
/// running, so a `jobs` stage still lists the real jobs.
fn run_stages(
    cmds: Vec<exec::Cmd>,
    bodies: Vec<StageBody>,
    background: bool,
    last: u8,
    shell: &mut Shell,
) -> u8 {
    debug_assert_eq!(cmds.len(), bodies.len());
    let mut jobs = std::mem::replace(&mut shell.jobs, exec::JobTable::new());
    // No fork may reap: it is not the parent of the pids in the table it
    // inherited, so its `waitpid` fails with `ECHILD` and reports every job as
    // finished. The shell therefore refreshes the table before forking a stage
    // that can *look* at it, and only such a stage: reaping removes finished jobs,
    // so doing it for `puts hi | cat` would take a completed job out from under a
    // later `fg`, which the unpiped `puts hi` leaves alone.
    //
    // A plain `jobs` qualifies (`jobs --help` and a misuse never reach the
    // listing). So does any *function* stage, conservatively: its body can reach
    // `jobs` and is not statically knowable.
    //
    // The `[N] Done` notice is the **shell's**, and stays here. It is not the
    // stage's output even when a stage's `jobs` is what prompted the reap: bash
    // writes it to the shell's stderr whether the command is piped or not
    // (`jobs 2> log | cat` and `jobs 2> log` both leave `log` empty), and it is
    // the only process that knows the reap happened. Handing it to a stage instead
    // meant guessing which stage would run `jobs` — unknowable for a function
    // body, and wrong outright with two of them — and losing the notice whenever
    // that stage never started.
    if !shell.forked
        && cmds.iter().zip(&bodies).any(|(cmd, body)| match body {
            StageBody::Builtin => cmd.words == ["jobs"],
            StageBody::Function(_) => true,
            StageBody::External => false,
        })
    {
        jobs.reap();
    }
    let outcome = exec::run_pipeline(cmds, &mut jobs, background, &mut |index, cmd, jobs| {
        std::mem::swap(&mut shell.jobs, jobs);
        let status = run_stage_in_shell(&bodies[index], cmd, last, shell);
        std::mem::swap(&mut shell.jobs, jobs);
        status
    });
    shell.jobs = jobs;
    // The only place that knows each stage's own status, so the only place that
    // can record `$sh.pipestatus`; `run_recorded` fills in a one-entry list for
    // everything else that produces a status.
    shell.record_status(outcome.status, outcome.stages);
    outcome.status
}

/// Run a builtin or function that is a pipeline stage. Called in the forked
/// child, so an `exit` ends that child and any state it changes dies with it.
///
/// `last` is the status the pipeline started from, which a status-sensitive
/// builtin — a bare `exit` — still reads.
fn run_stage_in_shell(body: &StageBody, cmd: &exec::Cmd, last: u8, shell: &mut Shell) -> u8 {
    shell.forked = true;
    let step = match body {
        StageBody::Function(args) => dispatch_function_call(&cmd.words[0], args.clone(), shell),
        // Not `builtins::dispatch`: `jobs`, `fg`, `bg`, and the prompt builtins
        // are dispatched by the shell, and would otherwise fall through to an
        // external lookup and report "command not found".
        StageBody::Builtin => run_expanded(cmd.words.clone(), last, shell),
        StageBody::External => unreachable!("an external stage has no in-shell body"),
    };
    match step {
        Step::Continue(code) | Step::Exit(code) => code,
        Step::Return(value) => status_of(&value),
    }
}

/// Expand each redirection target to exactly one path. Zero or several words is
/// an ambiguous redirect (a glob/list target is not a single file).
///
/// Backgrounding one used to be refused here: a background external deferred
/// its opens to a helper process reached through argv, and input *text* cannot
/// travel that way — arbitrary bytes, a body past the argument limit, an
/// embedded NUL. The stage forks and `execvp`s itself now, so the body reaches
/// its own process as memory and the temporary is written there.
fn expand_redirs(redirs: Vec<Redir>, vars: &Vars) -> Result<Vec<exec::Redirection>, String> {
    let mut out = Vec::with_capacity(redirs.len());
    for redir in redirs {
        if let Means::Document(body) = redir.means {
            // The delimiter is *syntactic* — the parser already used it to find
            // the body, and only its quoting reaches here. Expanding it would
            // make `<< $missing` an unbound-variable error and `<< *` an
            // ambiguous redirect depending on the directory, for a word whose
            // expansion is then thrown away.
            let text = if body.raw {
                body.text
            } else {
                interpolate_heredoc(&body.text, vars)?
            };
            out.push(exec::Redirection {
                fd: libc::STDIN_FILENO,
                kind: exec::RedirKind::In,
                target: exec::RedirTarget::Heredoc(text),
            });
            continue;
        }
        let mut words = expand::expand(vec![redir.target], vars).map_err(|e| e.to_string())?;
        if words.len() != 1 {
            return Err(format!(
                "ambiguous redirect: target expanded to {} words",
                words.len()
            ));
        }
        let word = words.pop().unwrap();
        let target = match redir.means {
            // `2>&-` closes the descriptor rather than pointing it anywhere.
            // Spelled out here rather than left to the number parse, so `-` is a
            // meaning of its own and not a descriptor that failed to parse.
            Means::Descriptor if word == "-" => exec::RedirTarget::Close,
            Means::Descriptor => {
                // `2>&1`: the target names a descriptor, so it must read as one.
                let from = word.parse::<i32>().map_err(|_| {
                    format!("`>&{word}`: the target of a duplication must be a descriptor")
                })?;
                if from < 0 {
                    return Err(format!("`>&{from}`: a descriptor cannot be negative"));
                }
                exec::RedirTarget::Descriptor(from)
            }
            // `<<< word` reaches the command the way a heredoc body does — an
            // unlinked temporary file — so a long one cannot deadlock against a
            // command that has not started reading. The trailing newline is
            // bash's, and is what makes `<<< $x | wc -l` say 1.
            Means::Text => exec::RedirTarget::Heredoc(format!("{word}\n")),
            Means::Path | Means::Document(_) => exec::RedirTarget::Path(word),
        };
        out.push(exec::Redirection {
            fd: redir.fd.map_or_else(
                || {
                    // `>&1` with no prefix duplicates onto stdout; `<&0` onto stdin.
                    exec::Redirection::default_fd(redir.kind)
                },
                |fd| fd as i32,
            ),
            kind: redir.kind,
            target,
        });
    }
    Ok(out)
}

/// A single bare literal word, for a desugaring that needs one.
fn one_word(text: &str) -> Word {
    Word(vec![Piece::Text {
        text: text.to_owned(),
        expandable: false,
    }])
}

/// Run one command with no redirections: classify it as an assignment or a
/// command and act. `last` is the previous status (the default for a bare `exit`
/// or `return`).
/// Expand a function call's argument words into typed values. Each is tagged with
/// whether it came from a bare literal word, so an attached `--flag=value` types
/// its value the same way the token would type positionally (see
/// [`expand::expand_call_values`]).
///
/// Kept separate from [`dispatch_function_call`] because a redirected call must
/// expand its arguments *before* the redirection targets are opened: creating or
/// truncating a target must not change what a glob argument matches.
fn expand_function_args(arg_words: Vec<Word>, vars: &Vars) -> Result<Vec<(Value, bool)>, Step> {
    expand::expand_call_values(arg_words, vars).map_err(|err| {
        note!("mesh: {err}");
        Step::Continue(1)
    })
}

/// Run an in-shell function call whose arguments are already expanded: generated
/// `--help` first, then the call itself.
fn dispatch_function_call(name: &str, args: Vec<(Value, bool)>, shell: &mut Shell) -> Step {
    // Intercept `--help` only when the signature does not claim it; a function
    // that declares a `--help` flag observes the switch itself (`DESIGN.md`
    // §"Command resolution and help").
    let declares_help = shell.funcs.get(name).is_some_and(|def| def.declares_help());
    if !declares_help && auto_help_requested(&args) {
        let help = shell.funcs.get(name).expect("declared function").help(name);
        return Step::Continue(builtins::print_generated_help(name, &help));
    }
    // The `--` terminator and flag parsing are handled during argument binding in
    // `call_func`. A command-position call parses flags.
    call_func(name, args, true, shell)
}

fn run_command(tokens: Vec<Word>, last: u8, shell: &mut Shell) -> Step {
    // Resolve an in-shell function *before* the external-argv rule turns a
    // bare list argument into an error, so an unspread list reaches the
    // function intact as one typed value (`DESIGN.md` §"Arguments do not
    // word-split"). Functions can never share a name with a builtin or the
    // `return`/job control words (definition rejects those), so resolving
    // one here does not reorder the builtins → functions → external chain.
    if let Some(name) = command_name(&tokens, &shell.vars)
        && shell.funcs.get(&name).is_some()
    {
        let arg_words: Vec<Word> = tokens.into_iter().skip(1).collect();
        let args = match expand_function_args(arg_words, &shell.vars) {
            Ok(args) => args,
            Err(step) => return step,
        };
        return dispatch_function_call(&name, args, shell);
    }
    if let Some(words) = job_builtin_words(&tokens, &shell.vars) {
        return match words {
            Ok(words) => run_expanded(words, last, shell),
            Err(step) => step,
        };
    }
    let words = match expand::expand(tokens, &shell.vars) {
        Ok(words) => words,
        Err(err) => {
            note!("mesh: {err}");
            return Step::Continue(1);
        }
    };
    run_expanded(words, last, shell)
}

/// A job builtin's words, with any **handle** argument turned into the `%id`
/// reference the builtin understands. `None` when this is not a job builtin.
///
/// Each argument is expanded on its own, and only a handle takes the typed
/// route: everything else keeps exactly the text ordinary expansion produces.
/// That distinction is load-bearing, because a job builtin's options are not
/// just data. Expanding them as values types `-0` as the integer `0` and drops
/// the sign along with it, which turns `kill -0 $pid` — ask whether it is
/// alive — into `kill 0 $pid`, and pid 0 is *the caller's own process group*.
///
/// Shared by every path that runs a command, since they expand separately:
/// `kill $j`, `kill $j 2>/dev/null` and `kill $j | cat` have to agree.
fn job_builtin_words(words: &[Word], vars: &Vars) -> Option<Result<Vec<String>, Step>> {
    let name = command_name(words, vars)?;
    if !matches!(name.as_str(), "fg" | "bg" | "wait" | "kill" | "disown") {
        return None;
    }
    let mut expanded = vec![name];
    for word in words.iter().skip(1) {
        match job_reference_word(word, vars, &expanded[0]) {
            Ok(Some(references)) => expanded.extend(references),
            Ok(None) => match expand::expand(vec![word.clone()], vars) {
                Ok(strings) => expanded.extend(strings),
                Err(err) => {
                    note!("mesh: {err}");
                    return Some(Err(Step::Continue(1)));
                }
            },
            Err(step) => return Some(Err(step)),
        }
    }
    Some(Ok(expanded))
}

/// The `%id` this word names, if it is a job handle; `None` leaves it to
/// ordinary expansion.
///
/// A handle becomes **`%id`** rather than a bare id on purpose: `fg 2` and
/// `fg %2` mean the same job, but for `kill` a bare number is a *pid*, and `$j`
/// must never be able to arrive as one.
fn job_reference_word(word: &Word, vars: &Vars, name: &str) -> Result<Option<Vec<String>>, Step> {
    let Ok(values) = expand::expand_values(vec![word.clone()], vars) else {
        // Whatever is wrong with it, ordinary expansion reports it below in the
        // terms the rest of the shell uses.
        return Ok(None);
    };
    // A word can produce several values — `kill ...$handles` spreads a list, and
    // `kill` takes independent targets — so this is about the word as a whole:
    // a handle anywhere in it means the word names jobs, and every value it
    // produced is converted. A word with no handle in it is left alone, which is
    // what keeps an option like `-0` the text it was written as.
    // A map is included so a plain one still gets the job builtin's own answer:
    // "a map is not a job" says what is wrong, where the generic argv message
    // ("needs `...`") advises a spread that would not help.
    let names_jobs = values
        .iter()
        .any(|value| job_id_of(value).is_some() || matches!(value, Value::Map(_)));
    if !names_jobs {
        return Ok(None);
    }
    let mut references = Vec::new();
    for value in &values {
        match job_id_of(value) {
            Some(id) => references.push(format!("%{id}")),
            // Alongside a handle, anything that has a byte form is still a
            // reference the builtin can read — a `%+` or a pid in the same list.
            None if !matches!(value, Value::Map(_)) => {
                references.extend(argv_words(value, name)?);
            }
            None => return runtime_error(format!("{name}: a map is not a job")),
        }
    }
    Ok(Some(references))
}

/// The job id this value names, if it is a handle — and **only** a handle.
///
/// Reading an `id` out of any map would make a handle forgeable: `m = [id: 1]`
/// is ordinary data, and `kill $m` must not signal job 1 on the strength of a
/// field name. Not being forgeable is the point of the handle being its own
/// value. `$sh.jobs[2]` is still a reference, because the table publishes
/// handles rather than records.
fn job_id_of(value: &Value) -> Option<usize> {
    match value {
        Value::Job(id) => Some(*id),
        _ => None,
    }
}

/// Run a command whose words are already expanded: `return`, generated help, the
/// prompt and job-control builtins, then the builtin → external chain. A function
/// has already been resolved by the caller, which still has its unexpanded words
/// and so can keep its arguments typed.
fn run_expanded(words: Vec<String>, last: u8, shell: &mut Shell) -> Step {
    if words.is_empty() {
        // A command whose words all expanded away (e.g. a glob with no
        // matches) is an empty-list result — status 0 per `DESIGN.md`.
        return Step::Continue(0);
    }
    // `return` ends the enclosing function (a recoverable error at top
    // level; `run_line` decides which by `in_function`).
    if words[0] == "return" {
        return make_return(&words[1..], shell);
    }
    if builtins::is_builtin(&words[0]) && auto_help_requested_strings(&words[1..]) {
        return Step::Continue(builtins::print_help(&words[0]));
    }
    match words[0].as_str() {
        "prompt" => return configure_prompt(&words[1..], shell),
        "prompt-hook" => return configure_prompt_hook(&words[1..], shell),
        // `source` runs mesh code in *this* shell, so it belongs here rather than
        // in `builtins::dispatch`, which is handed only words and a status.
        "source" => return source_file(&words[1..], last, shell),
        _ => {}
    }
    // Job control belongs to the shell that owns the jobs. A forked stage is not
    // the parent of those pids, so it can list what it inherited but cannot wait
    // on them or hand them the terminal — the same answer bash gives in a
    // subshell.
    let job_status = match words[0].as_str() {
        // `kill` is deliberately absent: it neither waits nor touches the
        // terminal, and signalling needs permission rather than parenthood, so a
        // forked stage can do it with the table it inherited — `kill %1 | cat`
        // works as bash's does.
        "fg" | "bg" | "wait" | "disown" if shell.forked => {
            note!("mesh: {}: no job control in a pipeline stage", words[0]);
            Some(1)
        }
        "fg" => Some(shell.jobs.foreground(&words[1..])),
        "bg" => Some(shell.jobs.background(&words[1..])),
        "wait" => {
            let interactive = shell.vars.interactive();
            Some(shell.jobs.wait(&words[1..], interactive))
        }
        "kill" => Some(shell.jobs.kill(&words[1..])),
        "disown" => Some(shell.jobs.disown(&words[1..])),
        "jobs" => Some(shell.jobs.list(&words[1..], !shell.forked)),
        _ => None,
    };
    if let Some(code) = job_status {
        return Step::Continue(code);
    }
    // Command resolution: builtins, then external (a function was already
    // resolved above).
    match builtins::dispatch(&words, last) {
        Some(Builtin::Exit(code)) => Step::Exit(code),
        Some(Builtin::Status(code)) => Step::Continue(code),
        None => Step::Continue(exec::run(&words, &mut shell.jobs)),
    }
}

fn auto_help_requested(args: &[(Value, bool)]) -> bool {
    args.iter()
        .map(|(arg, _)| arg)
        .take_while(|arg| !matches!(arg, Value::String(value) if value == "--"))
        .any(|arg| matches!(arg, Value::String(value) if value == "--help"))
}

fn auto_help_requested_strings(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--help")
}

fn configure_prompt(args: &[String], shell: &mut Shell) -> Step {
    match args {
        [] => {
            let text = shell.prompt.text.clone();
            Step::Continue(builtins::print_line(
                "prompt",
                text.as_deref().unwrap_or("mesh$ "),
            ))
        }
        [flag] if flag == "--reset" => {
            shell.prompt.text = None;
            Step::Continue(0)
        }
        [text] => {
            shell.prompt.text = Some(text.clone());
            Step::Continue(0)
        }
        _ => {
            note!("mesh: prompt: expected one prompt string or --reset");
            Step::Continue(2)
        }
    }
}

fn configure_prompt_hook(args: &[String], shell: &mut Shell) -> Step {
    let invalid = || {
        note!("mesh: prompt-hook: expected [EVENT] NAME FUNCTION or --remove [EVENT] NAME");
        Step::Continue(2)
    };
    match args {
        [flag, name] if flag == "--remove" => {
            shell
                .prompt
                .hooks
                .retain(|hook| hook.event != PromptEvent::PrePrompt || hook.name != *name);
            Step::Continue(0)
        }
        [flag, event, name] if flag == "--remove" => {
            let Some(event) = PromptEvent::parse(event) else {
                return invalid();
            };
            shell
                .prompt
                .hooks
                .retain(|hook| hook.event != event || hook.name != *name);
            Step::Continue(0)
        }
        [name, function] => register_prompt_hook(PromptEvent::PrePrompt, name, function, shell),
        [event, name, function] => {
            let Some(event) = PromptEvent::parse(event) else {
                return invalid();
            };
            register_prompt_hook(event, name, function, shell)
        }
        _ => invalid(),
    }
}

fn register_prompt_hook(event: PromptEvent, name: &str, function: &str, shell: &mut Shell) -> Step {
    if shell.funcs.get(function).is_none() {
        note!("mesh: prompt-hook: `{function}` is not a function");
        return Step::Continue(1);
    }
    if let Some(hook) = shell
        .prompt
        .hooks
        .iter_mut()
        .find(|hook| hook.event == event && hook.name == name)
    {
        hook.function = function.to_string();
    } else {
        shell.prompt.hooks.push(PromptHook {
            event,
            name: name.to_string(),
            function: function.to_string(),
        });
    }
    Step::Continue(0)
}

/// The OSC 133 marks the *shell* owns. reedline emits `A` and `B` — the prompt's
/// own boundaries — because it is the one drawing the prompt; `C` and `D` bracket
/// the command, which only the shell knows.
enum SemanticMark {
    /// `C` — the prompt is finished and what follows is the command's output.
    OutputStart,
    /// `D` — the command is over, with the status it ended on.
    CommandDone(u8),
    /// `D` with no status — the line was abandoned, so there is no command and
    /// no outcome to report, but the input region reedline opened at `B` still
    /// has to be closed.
    CommandAbandoned,
}

/// Write an OSC 133 mark, so a terminal can tell prompt from input from output:
/// jump between commands, fold their output, badge a failure. `DESIGN.md`
/// "terminal control" lists the sequence set; this is the pair with boundaries
/// mesh already has, at `PreExec` and `PostExec`.
///
/// **Only when the session is interactive.** `set_interactive` is recorded by
/// the interactive loop rather than derived from `isatty`, so `mesh -s` on a
/// terminal — which reads commands without being a session — stays quiet, and so
/// does every piped run the test suite asserts byte-exact output from. A mark on
/// stdout that the caller did not ask for is corruption, not decoration.
///
/// Terminated with `ST` rather than `BEL`, matching what reedline emits for `A`
/// and `B`, so one stream does not mix the two spellings.
///
/// Failure to write is ignored: the command's status is the command's, and a
/// decoration that could change it would be worse than a missing decoration.
fn semantic_mark(interactive: bool, mark: SemanticMark) {
    if !interactive {
        return;
    }
    let sequence = match mark {
        SemanticMark::OutputStart => "\x1b]133;C\x1b\\".to_string(),
        SemanticMark::CommandDone(status) => format!("\x1b]133;D;{status}\x1b\\"),
        SemanticMark::CommandAbandoned => "\x1b]133;D\x1b\\".to_string(),
    };
    use std::io::Write as _;
    let mut out = io::stdout();
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
}

/// `OSC 7` — report the working directory, so a new tab or split opens where the
/// shell is instead of at `$HOME`. `DESIGN.md` "terminal control" lists it, and
/// asks for it at startup as well as on a change: a fresh remote shell has to say
/// where it is before the first `cd`, or a split of it lands in the wrong place.
///
/// Reported once per prompt rather than from `cd`, which covers both at one call
/// site — the first prompt is the startup report, and any later change reaches the
/// next prompt whatever moved the shell: `cd`, a `func` that cds internally, or a
/// startup file. It re-sends an unchanged directory, which is what the sequence is
/// for (a terminal that asked to be told holds the last value it was told).
///
/// The *physical* directory, from `getcwd`, since that is the path a new terminal
/// can chdir to. Interactive-only and failure-ignoring for the same reasons as
/// [`semantic_mark`].
fn report_cwd(interactive: bool) {
    if !interactive {
        return;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let sequence = format!("\x1b]7;{}\x1b\\", cwd_url(&hostname(), &cwd));
    use std::io::Write as _;
    let mut out = io::stdout();
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
}

/// The `file://host/path` URL `OSC 7` carries.
///
/// The host is what lets a terminal tell a local directory from one inside an
/// `ssh` session; an empty host is a valid `file:` URL and the honest answer when
/// the system will not say what it is called.
///
/// Bytes outside RFC 3986's unreserved set are percent-encoded, `/` excepted so
/// the path keeps its structure. A path is bytes, not text — a directory whose
/// name is not UTF-8 is still a directory to `cd` into — so this encodes the raw
/// bytes rather than going through a lossy string first.
fn cwd_url(host: &[u8], path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut url = String::from("file://");
    percent_encode(host, &mut url);
    percent_encode(path.as_os_str().as_bytes(), &mut url);
    url
}

/// Percent-encode into `out`, leaving unreserved bytes and `/` as they are.
fn percent_encode(bytes: &[u8], out: &mut String) {
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
}

/// This host's name for `OSC 7`, or empty when it cannot be read.
///
/// `$env.HOSTNAME` is not consulted: it is not in the environment a login shell
/// is given on either platform mesh targets, and a stale exported copy would name
/// the wrong machine after an `ssh`, which is exactly the case the host field is
/// there to distinguish.
fn hostname() -> Vec<u8> {
    let mut buffer = [0_u8; 256];
    // SAFETY: `gethostname` writes at most `buffer.len()` bytes through the
    // pointer, which is valid and writable for exactly that many.
    let read = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if read != 0 {
        return Vec::new();
    }
    // A name too long for the buffer is truncated without a NUL on some
    // platforms, so fall back to the whole buffer when there is none.
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    buffer[..end].to_vec()
}

/// How long a title may get, in characters. A title bar is a handful of
/// centimetres and every terminal elides what does not fit — but it elides the
/// *end*, so a long `find` invocation would push what identifies the window off
/// the edge. Cutting it here keeps the decision with the shell.
const TITLE_LIMIT: usize = 96;

/// Set the window and tab title, per `DESIGN.md` "terminal control".
///
/// Automatic: at the prompt it says where the shell is, and while a command runs
/// it says what is running, which is what makes a row of tabs readable at a
/// glance. The off switch that item asks for wants to be
/// `$sh.options.osc-title`, alongside the `$sh.options.bold-input` already
/// floated in `TODO.md`, and waits on `$sh` becoming a writable place.
///
/// Interactive-only and failure-ignoring, for the same reasons as
/// [`semantic_mark`] and [`report_cwd`].
fn set_title(interactive: bool, text: &str) {
    if !interactive {
        return;
    }
    let Some(sequence) = title_sequence(session_term().as_deref(), text) else {
        return;
    };
    use std::io::Write as _;
    let mut out = io::stdout();
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
}

/// `$env.TERM` as it stood at the first title, held for the session.
///
/// Read once rather than per title, because the terminal on the other end does not
/// change: `$env.TERM = dumb` mid-session is a claim about a terminal, not a new
/// one. Reading it each time let the *clear* at exit consult a different answer
/// than the title it was clearing, so that assignment left the window holding the
/// command that made it — raised in review on #238. A startup file's `$env.TERM` is
/// still honored, since the first title comes after those have run.
fn session_term() -> Option<OsString> {
    static TERM: OnceLock<Option<OsString>> = OnceLock::new();
    TERM.get_or_init(|| std::env::var_os("TERM")).clone()
}

/// The title sequence for this `$env.TERM`, or `None` for a terminal that has no
/// title to set.
///
/// Three answers rather than one, because the sequence is not portable:
///
/// - **screen and tmux** take `ESC k … ST`, naming the *window* inside the
///   multiplexer. Sent OSC 0 instead, tmux would forward it to the outer terminal
///   and the pane's own name would never change.
/// - **A terminal in [`TITLE_TERMS`]** takes OSC 0, which sets the window and the
///   icon name together.
/// - **Anything else** is sent nothing.
///
/// Terminated with `BEL`, not the `ST` mesh uses elsewhere. The title is the
/// oldest and most widely implemented of these sequences, every shell's `PS1`
/// idiom spells it `\e]0;…\a`, and terminals exist that answer to that spelling
/// alone — so this is the one place where matching the installed base beats
/// matching the rest of the file.
fn title_sequence(term: Option<&std::ffi::OsStr>, text: &str) -> Option<String> {
    let term = term?.to_str()?;
    if names_terminal(term, "screen") || names_terminal(term, "tmux") {
        return Some(format!("\x1bk{}\x1b\\", title_text(text)));
    }
    TITLE_TERMS
        .iter()
        .any(|family| names_terminal(term, family))
        .then(|| format!("\x1b]0;{}\x07", title_text(text)))
}

/// The terminal families that take an OSC 0 title.
///
/// An allowlist because the two ways of being wrong are not equally bad: a
/// terminal missing from here quietly has no title, while one wrongly assumed to
/// take one *prints the title text at every prompt*. `TODO.md` carries the reasons
/// in full, and terminfo as the follow-up that would replace the list with data.
const TITLE_TERMS: &[&str] = &[
    "alacritty",
    "contour",
    "foot",
    "ghostty",
    "gnome",
    "iterm",
    "kitty",
    "konsole",
    "mintty",
    "mlterm",
    "putty",
    "rxvt",
    "st",
    "stterm",
    "terminator",
    "termite",
    "vte",
    "wezterm",
    "xterm",
];

/// Is `term` this terminal family, or a variant of it?
///
/// Terminfo separates a variant from its family with `-` or `.`
/// (`xterm-256color`, `screen.xterm-256color`), so a family name ends at one of
/// those or at the end of the string. A bare prefix test would be wrong in the
/// direction the allowlist exists to avoid: `st52` is an Atari VT52 with no title,
/// and it starts with `st`.
fn names_terminal(term: &str, family: &str) -> bool {
    term.strip_prefix(family)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '.']))
}

/// A string safe to put in a title: control characters replaced by spaces, and no
/// longer than [`TITLE_LIMIT`] characters.
///
/// The stripping is not tidiness. Both things mesh titles — the command line and
/// the working directory — carry text it did not choose, and a filename may hold
/// an `ESC`: `touch $'\e]0;x\a'` in a directory the user then `cd`s into would
/// otherwise close mesh's sequence early and start one of its own, with the rest
/// of the title as its payload. A control character cannot draw anything in a
/// title bar, so replacing it costs nothing and ends the question. Spaces rather
/// than deletion so a pasted two-line command does not read as one joined word.
fn title_text(text: &str) -> String {
    let mut title: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(TITLE_LIMIT)
        .collect();
    if text.chars().count() > TITLE_LIMIT {
        // Trim first: a cut that lands mid-word reads better without the space
        // before the ellipsis.
        title = title.trim_end().to_string();
        title.push('…');
    }
    title
}

/// What the title says at the prompt: `user@host: directory`, the form a terminal
/// window has carried since `xterm` — the shell is idle, so the useful thing to
/// say is where it is idle. `$HOME` shortens to `~`, as in a prompt.
///
/// Assembled from parameters rather than read from the environment so it is
/// testable without one.
fn prompt_title(user: &str, host: &[u8], cwd: &Path, home: Option<&Path>) -> String {
    let mut title = String::new();
    if !user.is_empty() {
        title.push_str(user);
    }
    if !host.is_empty() {
        if !title.is_empty() {
            title.push('@');
        }
        title.push_str(&String::from_utf8_lossy(host));
    }
    if !title.is_empty() {
        title.push_str(": ");
    }
    title.push_str(&abbreviated_home(cwd, home));
    title
}

/// `/home/mikel/src` as `~/src`, when it is under `home`. Whole-component
/// matching, so `/home/mikelward` is not `~ward` when `$HOME` is `/home/mikel`.
fn abbreviated_home(cwd: &Path, home: Option<&Path>) -> String {
    let Some(home) = home.filter(|home| home.as_os_str().len() > 1) else {
        return cwd.to_string_lossy().into_owned();
    };
    match cwd.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => cwd.to_string_lossy().into_owned(),
    }
}

/// The title while a command runs: the command line itself, so a tab says what it
/// is busy with. Where the shell is is *already* on screen in the scrollback above
/// it; what is running may not be, once the output scrolls.
fn running_title(command: &str) -> String {
    command.trim().to_owned()
}

/// The prompt title, read from the environment. `$env.USER` is consulted for the
/// user name because that is the name the session was opened under; `getpwuid`
/// would answer with the account behind a `sudo -u`, which is not what a title is
/// reporting.
fn environment_prompt_title() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    prompt_title(&user, &hostname(), &cwd, home.as_deref())
}

fn run_prompt_hooks(event: PromptEvent, args: Vec<Value>, shell: &mut Shell) {
    let hooks: Vec<String> = shell
        .prompt
        .hooks
        .iter()
        .filter(|hook| hook.event == event)
        .map(|hook| hook.function.clone())
        .collect();
    // Hook arguments are computed values (command text, status, elapsed), not
    // user syntax, so none is a bare literal token — and none is flag syntax, so
    // flag parsing is disabled and every value binds positionally. Without this a
    // command line of `--` or `--word` would be read as a terminator or flag and
    // a hook declared `func hook(cmd)` would fail with an arity/unknown-flag error.
    let args: Vec<(Value, bool)> = args.into_iter().map(|value| (value, false)).collect();
    // A hook runs on the shell's schedule, not the user's, so what it runs must
    // not become `$sh.status` — the loop already discards the hook's own `Step`
    // for that reason, and a `preprompt` hook that merely prints would otherwise
    // report 0 where the user's failed command should still be visible.
    let saved = shell.vars.status_snapshot();
    for function in hooks {
        let _ = call_func(&function, args.clone(), false, shell);
    }
    shell.vars.restore_status(saved);
}

/// Expand just the command word to its name, if it resolves to a single string —
/// used to look up an in-shell function before the arguments are expanded. A word
/// that expands to zero or several words (an empty glob, a multi-match glob, a
/// bare list) is not a function name, so this returns `None` and the byte-string
/// path takes over.
fn command_name(tokens: &[Word], vars: &Vars) -> Option<String> {
    let first = tokens.first()?;
    let cloned = first.clone();
    let mut argv = expand::expand(vec![cloned], vars).ok()?;
    (argv.len() == 1).then(|| argv.pop().unwrap())
}

/// Build the [`Step::Return`] for a `return` command word: no argument uses the
/// last status; a single argument is typed like any bare scalar (`7` → the
/// integer `7`, `true`/`false` → booleans, else a string) and carried as the
/// result, its status a view of that value. A surplus operand is reported and
/// does not unwind (the function keeps running).
fn make_return(args: &[String], shell: &Shell) -> Step {
    match args {
        // Bare: the result so far (`DESIGN.md` §"Result and `return`").
        [] => Step::Return(shell.result.clone()),
        [value] => Step::Return(expand::typed_scalar(value)),
        _ => {
            note!("mesh: return: too many arguments");
            Step::Continue(1)
        }
    }
}

/// Call the function `name` with already-expanded typed `args`. Binds the
/// parameters (positionals, `--flags`, and any `...rest`) in a fresh local scope,
/// runs the body, and returns the function's status — an explicit `return`, else
/// the last command's status. A list argument counts as **one** positional (it
/// arrives intact as a list value); a bad argument count or flag is a recoverable
/// error.
fn call_func(name: &str, args: Vec<(Value, bool)>, flags_enabled: bool, shell: &mut Shell) -> Step {
    let (params, body) = match shell.funcs.get(name) {
        Some(def) => (def.params.clone(), def.body.clone()),
        None => {
            return Step::Continue(exec::run(&[name.to_string()], &mut shell.jobs));
        }
    };

    // Isolate the caller's loop state for the whole call — argument binding
    // included — so a `break`/`continue` inside an omitted block-bearing default
    // is reported as outside a loop rather than being attributed to the caller's
    // loop. Restored on every exit path below.
    let caller_loop_depth = std::mem::replace(&mut shell.loop_depth, 0);
    // The callee starts with no result of its own, so a bare `return` before
    // anything ran carries the empty string rather than the caller's value. The
    // mark travels with it: a callee that ended in a value must not leave the
    // caller thinking *this call* produced one, or the call's status goes
    // unrecorded.
    let caller_result = std::mem::replace(&mut shell.result, Value::String(String::new()));
    let caller_produced = std::mem::replace(&mut shell.produced, Produced::Status);
    shell.vars.push_scope();
    let bound = bind_arguments(name, &params, args, flags_enabled, shell);
    // A default that ran `break`/`continue` (already reported as outside a loop)
    // may have left `shell.control` set; clear it so it neither short-circuits the
    // body nor leaks back to the caller.
    if matches!(
        shell.control,
        Some(parser::ControlKind::Break | parser::ControlKind::Continue)
    ) {
        shell.control = None;
    }
    if let Err(step) = bound {
        shell.vars.pop_scope();
        shell.loop_depth = caller_loop_depth;
        shell.result = caller_result;
        shell.produced = caller_produced;
        // A default's `return N` ends the call with that status, like the body's;
        // an `exit`/runtime step unwinds unchanged.
        return match step {
            Step::Return(value) => Step::Continue(status_of(&value)),
            other => other,
        };
    }
    // Binding may have evaluated a default whose body produced a result; that
    // belongs to the setup, not to this call. The body starts from nothing, so a
    // bare `return` before it produces anything still carries the empty string.
    shell.result = Value::String(String::new());
    shell.produced = Produced::Status;
    let executed = run_source(&body, 0, true, shell);
    shell.loop_depth = caller_loop_depth;
    shell.result = caller_result;
    shell.produced = caller_produced;
    if matches!(
        shell.control,
        Some(parser::ControlKind::Break | parser::ControlKind::Continue)
    ) {
        shell.control = None;
    }
    let result = match executed {
        Step::Return(value) => Step::Continue(status_of(&value)),
        other => other,
    };
    shell.vars.pop_scope();
    result
}

/// Call a user function for its **value** (`x = f(arg, key: value)`): evaluate the
/// value-mode arguments in the caller's scope, bind them in a fresh callee scope,
/// run the body, and return its result — the last expression's value, or the
/// value carried by an explicit `return`. Stdout still streams: the value and
/// byte channels are independent (`DESIGN.md` §"Calling for a value"). Loop state
/// is isolated exactly as in [`call_func`].
fn call_func_for_value(
    name: &str,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let Some((params, body)) = shell
        .funcs
        .get(name)
        .map(|def| (def.params.clone(), def.body.clone()))
    else {
        unreachable!("call_func_for_value is only reached for a declared function");
    };
    call_signature_for_value(name, &params, &body, arguments, last, in_function, shell)
}

/// The body of [`call_func_for_value`], over a signature and body rather than a
/// name in the function store — a lambda has the same two, reached through the
/// variable it is bound to (`$double(5)`), and calls identically.
///
/// `name` names the callee in diagnostics only: the declared name for a `func`,
/// the variable read for a lambda.
#[allow(clippy::too_many_arguments)]
fn call_signature_for_value(
    name: &str,
    params: &[parser::Param],
    body: &parser::Source,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    // Copied, not taken: an argument body records results of its own
    // (`f(if c { a; b })`), and those belong to neither the caller nor the callee,
    // so the copy is put back on every path below. The caller's own result stays
    // in place while the arguments run, because the arguments are still the
    // caller's code — a bare `return` one of them raises (`f(if c { return })`)
    // carries the caller's result so far, exactly as it would outside the call.
    let caller_result = shell.result.clone();
    let caller_produced = shell.produced;

    // Arguments are evaluated in the caller's scope, before *any* callee state is
    // touched, so `f($x)` reads the caller's `$x` — and so caller-owned control
    // flow raised by an argument (`f(if c { return e })`) propagates out unchanged
    // rather than entering the callee cleanup below, which would normalize a
    // `return` into this call's value. Loop state stays the caller's until the
    // call itself begins, so an argument's `break` belongs to the caller's loop.
    let scanned = evaluate_value_arguments(name, params, arguments, last, in_function, shell);
    // A `break`/`continue` an argument raised belongs to the caller's loop, so it
    // is checked before the argument outcome — an out-of-loop one arrives as an
    // error *and* leaves the flag set, and both need answering together.
    if shell.control.is_some() {
        shell.result = caller_result;
        shell.produced = caller_produced;
        // With a loop to leave, hand the flag back and skip the call. Without
        // one, the `break` was already reported as an error: clear the flag and
        // fail the statement, or the rest of the script would never run.
        if shell.loop_depth == 0 {
            shell.control = None;
            return Err(Step::Continue(1));
        }
        return Ok(Value::String(String::new()));
    }
    let (positionals, switches_on, flag_values) = match scanned {
        Ok(scanned) => scanned,
        Err(step) => {
            shell.result = caller_result;
            shell.produced = caller_produced;
            return Err(step);
        }
    };

    run_call_body_for_value(
        body,
        caller_result,
        caller_produced,
        |shell| bind_scanned(name, params, positionals, switches_on, flag_values, shell),
        shell,
    )
}

/// The **callee half** of a value-mode call: a fresh scope, the binding, the body,
/// and the outcome mapped to a value — with the caller's loop state and result put
/// back on every path out.
///
/// `bind` is the only thing that differs between call shapes, which is why it is a
/// parameter rather than two copies of this: a source-level call arrives with its
/// arguments already scanned from syntax, while a **synthesized** one — a
/// higher-order modifier handing a list element to a callable — arrives with a
/// value and no syntax at all. Both then have to isolate loop state, reset the
/// result, and normalize `return` identically, and the way to guarantee that is
/// for there to be one of it.
fn run_call_body_for_value(
    body: &parser::Source,
    caller_result: Value,
    caller_produced: Produced,
    bind: impl FnOnce(&mut Shell) -> Result<(), Step>,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let caller_loop_depth = std::mem::replace(&mut shell.loop_depth, 0);
    // Callee territory starts here, so the arguments' transient results are
    // dropped: a default's own bare `return` carries the callee's result so far,
    // not the caller's and not an argument's, as in `call_func`.
    shell.result = Value::String(String::new());
    shell.produced = Produced::Status;
    shell.vars.push_scope();
    let outcome = match bind(shell) {
        Ok(()) => {
            // A default body may itself have produced a result; that belongs to
            // the setup, so the body starts from nothing too.
            shell.result = Value::String(String::new());
            shell.produced = Produced::Status;
            eval_body(body, 0, true, shell)
        }
        Err(step) => Err(step),
    };
    // A `break`/`continue` the callee left set escaped its own loops — already
    // reported as "not inside a loop". It must not leak back to the caller, and
    // like any runtime error it fails the call instead of yielding a value, so
    // `x = f() && …` short-circuits exactly as the command-mode call does.
    let escaped = matches!(
        shell.control,
        Some(parser::ControlKind::Break | parser::ControlKind::Continue)
    );
    if escaped {
        shell.control = None;
    }
    shell.vars.pop_scope();
    shell.loop_depth = caller_loop_depth;
    shell.result = caller_result;
    shell.produced = caller_produced;
    match outcome {
        // `exit` unwinds regardless: it leaves the shell, not just this call.
        Err(step @ Step::Exit(_)) => Err(step),
        // The diagnostic is already on stderr, so fail quietly with its status.
        _ if escaped => Err(Step::Continue(1)),
        Ok(value) => Ok(value),
        // An explicit `return val` yields its value; a runtime step unwinds.
        Err(Step::Return(value)) => Ok(value),
        Err(other) => Err(other),
    }
}

/// Call a function **value** with one already-computed argument, for its value.
///
/// This is the higher-order path: `$xs:map(f)` has an element, not source text, so
/// there is nothing to evaluate and the value binds positionally. Flag parsing is
/// off for the same reason it is off for a prompt hook — the argument is data, so a
/// string that happens to read `--force` is a string, not an option.
fn call_callable_for_value(
    name: &str,
    function: &vars::FuncValue,
    argument: Value,
    shell: &mut Shell,
) -> Result<Value, Step> {
    // A modifier reference is applied, not run: there is no body, no scope to push,
    // and nothing it can do to the caller's result — so none of the save/restore
    // `run_call_body_for_value` exists for applies.
    if let Some(modifier) = function.modifier_name() {
        return apply_modifier_ref(modifier, argument);
    }
    let (params, body) = function
        .as_lambda()
        .expect("a callable is a lambda or a modifier reference");
    let caller_result = shell.result.clone();
    let caller_produced = shell.produced;
    run_call_body_for_value(
        body,
        caller_result,
        caller_produced,
        |shell| bind_arguments(name, params, vec![(argument, false)], false, shell),
        shell,
    )
}

/// Call a modifier reference through the `$m(…)` syntax.
///
/// A reference has no signature for [`bind_arguments`] to match, but its arguments
/// are still ordinary call arguments: they go through [`evaluate_value_arguments`]
/// so a spread explodes into elements exactly as it does for a one-parameter lambda
/// (`$m(...$xs)`), and only then is the count checked. The empty parameter list is
/// the right description rather than a stand-in — a modifier takes one value and has
/// no options, so a `--flag` argument has nothing to bind to and says so.
fn call_modifier_ref(
    modifier: &str,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let label = format!("`:{modifier}`");
    // Copied and put back on every path below, as `call_signature_for_value` does:
    // an argument body records results of its own (`$m(if c { a; b })`) that belong
    // to neither side. There is no callee body here to hand them to, so the restore
    // is written out rather than left to `run_call_body_for_value`.
    let caller_result = shell.result.clone();
    let caller_produced = shell.produced;
    let scanned = evaluate_value_arguments(&label, &[], arguments, last, in_function, shell);
    // A `break`/`continue` an argument raised belongs to the caller's loop, and is
    // answered before the argument outcome — an out-of-loop one arrives as an error
    // *and* leaves the flag set. Leaving it set stops the enclosing function where
    // the same call through a lambda recovers and runs on.
    if shell.control.is_some() {
        shell.result = caller_result;
        shell.produced = caller_produced;
        if shell.loop_depth == 0 {
            shell.control = None;
            return Err(Step::Continue(1));
        }
        return Ok(Value::String(String::new()));
    }
    let (positionals, switches_on, flag_values) = match scanned {
        Ok(scanned) => scanned,
        Err(step) => {
            shell.result = caller_result;
            shell.produced = caller_produced;
            return Err(step);
        }
    };
    shell.result = caller_result;
    shell.produced = caller_produced;
    debug_assert!(
        switches_on.is_empty() && flag_values.is_empty(),
        "no parameters means no option can bind"
    );
    let [argument] = <[Value; 1]>::try_from(positionals).map_err(|positionals: Vec<Value>| {
        runtime_message(format!(
            "{label}: expected 1 argument, got {}",
            positionals.len()
        ))
    })?;
    apply_modifier_ref(modifier, argument)
}

/// Apply the modifier a bare `:name` reference denotes.
///
/// Only the argument-free modifiers can be referenced this way: `:join` and
/// `:split` need a separator and `:map`/`:filter`/`:each` need a callable, so there
/// is no one-argument function for them to denote. A name the parser recognized but
/// the engine cannot yet apply reports that rather than being silently dropped.
fn apply_modifier_ref(name: &str, argument: Value) -> Result<Value, Step> {
    // Checked here rather than left to the shared path: a reference to `:join` is
    // wrong whatever it would be applied to, so say that instead of complaining
    // about the value it met.
    if modifier_takes_arguments(name) {
        return runtime_error(format!(
            "`:{name}` takes arguments, so it is not a one-argument function"
        ));
    }
    apply_argument_free_modifier(name, argument)
}

/// Match `args` against `params` and bind each parameter in the current (already
/// pushed) scope. Positionals bind left to right, `--flags` in any order, and a
/// `...rest` collects the leftovers; a bare `--` ends flag parsing. Returns the
/// exit status to report on a bad argument count, an unknown/misused flag, or a
/// default that fails to evaluate.
///
/// `flags_enabled` gates the `--`/`--flag` interpretation: a command-position
/// call parses flags, but synthesized calls whose arguments are computed values
/// rather than user syntax (prompt hooks) disable it so every value binds
/// positionally — a command line of `--`/`--word` is then data, not flag syntax.
fn bind_arguments(
    name: &str,
    params: &[parser::Param],
    args: Vec<(Value, bool)>,
    flags_enabled: bool,
    shell: &mut Shell,
) -> Result<(), Step> {
    // Scan the call-site arguments, separating positionals from flags. Only a
    // `Value::String` beginning with `--` is a flag candidate; everything else
    // (and everything after a bare `--`) is a positional. With `flags_enabled`
    // false the scan is skipped entirely and every argument is a positional.
    let mut positional_values: Vec<Value> = Vec::new();
    let mut switches_on: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut flag_values: std::collections::HashMap<&str, Value> = std::collections::HashMap::new();
    let mut flags_ended = false;
    for (arg, bare) in args {
        if flags_enabled
            && !flags_ended
            && let Value::String(text) = &arg
        {
            if text == "--" {
                flags_ended = true;
                continue;
            }
            if let Some(body) = text.strip_prefix("--")
                && !body.is_empty()
            {
                bind_dashed_option(name, params, body, bare, &mut switches_on, &mut flag_values)?;
                continue;
            }
        }
        positional_values.push(arg);
    }

    bind_scanned(
        name,
        params,
        positional_values,
        switches_on,
        flag_values,
        shell,
    )
}

/// Bind a function's parameters from already-separated call arguments — the
/// leftover **positionals** (left to right, with the surplus collected by a
/// `...rest`), the **switches** turned on, and the **valued flags** — applying
/// the arity rules and evaluating any omitted parameter's default in declaration
/// order. Shared by the command-mode [`bind_arguments`] (which parses `--flag`
/// tokens) and the value-mode `bind_value_arguments` (which reads `key: value`
/// options), so both modes bind identically once arguments are separated.
fn bind_scanned<'p>(
    name: &str,
    params: &'p [parser::Param],
    positional_values: Vec<Value>,
    switches_on: std::collections::HashSet<&'p str>,
    mut flag_values: std::collections::HashMap<&'p str, Value>,
    shell: &mut Shell,
) -> Result<(), Step> {
    use parser::ParamKind;

    // Arity: every required positional must be filled; without a rest, surplus
    // positionals are an error.
    let required = params
        .iter()
        .filter(|param| matches!(param.kind, ParamKind::Required))
        .count();
    let maximum = params
        .iter()
        .filter(|param| matches!(param.kind, ParamKind::Required | ParamKind::Optional(_)))
        .count();
    let has_rest = params
        .iter()
        .any(|param| matches!(param.kind, ParamKind::Rest));
    let supplied = positional_values.len();
    if supplied < required {
        if has_rest || maximum > required {
            note!("mesh: {name}: expected at least {required} argument(s), got {supplied}");
        } else {
            note!("mesh: {name}: expected {required} argument(s), got {supplied}");
        }
        return Err(Step::Continue(2));
    }
    if !has_rest && supplied > maximum {
        if maximum > required {
            note!("mesh: {name}: expected at most {maximum} argument(s), got {supplied}");
        } else {
            note!("mesh: {name}: expected {maximum} argument(s), got {supplied}");
        }
        return Err(Step::Continue(2));
    }

    // Bind every parameter in declaration order, consuming supplied positionals in
    // sequence. Binding in order means a default — positional or flag — can
    // reference any earlier-declared parameter, whatever its kind. A missing
    // positional is optional (guaranteed by the arity check) and takes its default.
    let mut supplied = positional_values.into_iter();
    for param in params {
        match &param.kind {
            ParamKind::Required => {
                let value = supplied.next().expect("a required positional is validated");
                shell.vars.set_value(&param.name, value);
            }
            ParamKind::Optional(default) => {
                let value = match supplied.next() {
                    Some(value) => value,
                    None => evaluate_default(name, &param.name, default, shell)?,
                };
                shell.vars.set_value(&param.name, value);
            }
            ParamKind::Rest => {
                shell
                    .vars
                    .set_value(&param.name, Value::List(supplied.by_ref().collect()));
            }
            ParamKind::Switch => {
                let on = switches_on.contains(param.name.as_str());
                shell.vars.set_value(&param.name, Value::Boolean(on));
            }
            ParamKind::Flag(default) => {
                let value = match flag_values.remove(param.name.as_str()) {
                    Some(value) => value,
                    None => evaluate_default(name, &param.name, default, shell)?,
                };
                shell.vars.set_value(&param.name, value);
            }
        }
    }
    Ok(())
}

/// Evaluate a **value-mode** call's arguments (`f(arg, key: value, ...$spread)`)
/// in the **caller's** scope into the separated form [`bind_scanned`] expects:
/// leftover positionals, switches turned on, and valued flags. A `key: value`
/// option binds the switch/flag of that name (`force: true` ≡ `--force`,
/// `tag: v2` ≡ `--tag=v2`); a spread contributes a list's elements as positionals
/// or a map's entries as options. Positionals are positional-only, so a `key:` on
/// a positional parameter — or an unknown name — is an error.
#[allow(clippy::type_complexity)]
fn evaluate_value_arguments<'p>(
    name: &str,
    params: &'p [parser::Param],
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<
    (
        Vec<Value>,
        std::collections::HashSet<&'p str>,
        std::collections::HashMap<&'p str, Value>,
    ),
    Step,
> {
    let mut positionals: Vec<Value> = Vec::new();
    let mut switches_on: std::collections::HashSet<&'p str> = std::collections::HashSet::new();
    let mut flag_values: std::collections::HashMap<&'p str, Value> =
        std::collections::HashMap::new();
    let mut flags_ended = false;
    for argument in arguments {
        // Every argument form begins by evaluating one expression in the caller's
        // scope, so evaluate here and check for caller-owned loop control once.
        let expression = match argument {
            parser::Argument::Positional(expression)
            | parser::Argument::Named(_, expression)
            | parser::Argument::Spread(expression) => expression,
        };
        let bare = is_bare_literal_word(expression);
        let value = eval_expr(expression, last, in_function, shell)?;
        // A `break`/`continue` the argument raised belongs to the caller's loop.
        // Stop at once — before binding this argument or evaluating any later one —
        // so `f(if c { break }, 1 / 0)` reports no division error and a switch whose
        // expression broke is never type-checked. The caller sees `shell.control`
        // set and skips the call.
        if shell.control.is_some() {
            return Ok((positionals, switches_on, flag_values));
        }
        match argument {
            parser::Argument::Positional(_) => scan_call_value(
                name,
                params,
                value,
                bare,
                &mut flags_ended,
                &mut positionals,
                &mut switches_on,
                &mut flag_values,
            )?,
            parser::Argument::Named(key, _) => {
                reject_option_after_terminator(name, key, flags_ended)?;
                bind_named_option(name, params, key, value, &mut switches_on, &mut flag_values)?;
            }
            parser::Argument::Spread(_) => match value {
                // A spread explodes into call arguments, so each element goes
                // through the same scan: a `--` element terminates option parsing
                // and a `--flag` element binds its option, exactly as in command
                // mode. Elements are values, not literal tokens, so none is `bare`.
                Value::List(items) => {
                    for item in items {
                        scan_call_value(
                            name,
                            params,
                            item,
                            false,
                            &mut flags_ended,
                            &mut positionals,
                            &mut switches_on,
                            &mut flag_values,
                        )?;
                    }
                }
                Value::Map(entries) => {
                    for (key, value) in entries {
                        reject_option_after_terminator(name, &key, flags_ended)?;
                        bind_named_option(
                            name,
                            params,
                            &key,
                            value,
                            &mut switches_on,
                            &mut flag_values,
                        )?;
                    }
                }
                _ => {
                    return runtime_error(format!(
                        "{name}: a spread argument must be a list (of positionals) or a map (of options)"
                    ));
                }
            },
        }
    }
    Ok((positionals, switches_on, flag_values))
}

/// A bare `--` ends option parsing, so no option may follow it — and a `key:
/// value` pair (or a spread map entry) has no positional meaning to fall back to,
/// unlike a dashed word. Report it rather than silently binding the option the
/// terminator just said would not be one.
fn reject_option_after_terminator(name: &str, key: &str, flags_ended: bool) -> Result<(), Step> {
    if flags_ended {
        note!("mesh: {name}: option `{key}:` cannot follow `--`");
        return Err(Step::Continue(2));
    }
    Ok(())
}

/// Route one value-mode call argument to the right place: a bare `--` ends option
/// parsing (everything after it is positional, even if it looks like a flag), a
/// `--name`/`--name=value` string binds that option, and anything else is a
/// positional. Shared by direct positional arguments and spread elements so both
/// follow the command-mode rules (`DESIGN.md` §"Functions").
#[allow(clippy::too_many_arguments)]
fn scan_call_value<'p>(
    name: &str,
    params: &'p [parser::Param],
    value: Value,
    bare: bool,
    flags_ended: &mut bool,
    positionals: &mut Vec<Value>,
    switches_on: &mut std::collections::HashSet<&'p str>,
    flag_values: &mut std::collections::HashMap<&'p str, Value>,
) -> Result<(), Step> {
    if !*flags_ended && let Value::String(text) = &value {
        if text == "--" {
            *flags_ended = true;
            return Ok(());
        }
        if let Some(body) = text.strip_prefix("--")
            && !body.is_empty()
        {
            return bind_dashed_option(name, params, body, bare, switches_on, flag_values);
        }
    }
    positionals.push(value);
    Ok(())
}

/// Is `expression` a single unquoted literal word? Such an argument types like the
/// same token written in command position, so a dashed `--n=2` written inside a
/// value call binds the integer `2` while `--n="2"` keeps its string type.
fn is_bare_literal_word(expression: &parser::Expr) -> bool {
    let parser::Expr::Scalar(word) = expression else {
        return false;
    };
    word.value.pieces.iter().all(|piece| {
        matches!(
            piece,
            parser::WordPiece::Text {
                quote: parser::QuoteMode::Bare,
                ..
            }
        )
    })
}

/// Bind one **dashed** option token — `body` is the text after the leading `--`,
/// so `force` or `tag=v2` — to the switch or valued flag it names. `bare` marks a
/// value that came from an unquoted literal word, so `--n=2` types as the integer
/// `2` while `--n="2"` stays a string. Shared by command mode and value mode: the
/// two spellings are interchangeable (`DESIGN.md` §"Calling for a value"), so
/// `f(prod, --force)` binds the same switch as `f(prod, force: true)`.
fn bind_dashed_option<'p>(
    name: &str,
    params: &'p [parser::Param],
    body: &str,
    bare: bool,
    switches_on: &mut std::collections::HashSet<&'p str>,
    flag_values: &mut std::collections::HashMap<&'p str, Value>,
) -> Result<(), Step> {
    use parser::ParamKind;
    let (flag, inline) = match body.split_once('=') {
        Some((flag, value)) => (flag, Some(value.to_owned())),
        None => (body, None),
    };
    let declared = params.iter().find(|param| {
        param.name == flag && matches!(param.kind, ParamKind::Switch | ParamKind::Flag(_))
    });
    let Some(declared) = declared else {
        note!("mesh: {name}: unknown flag `--{flag}`");
        return Err(Step::Continue(2));
    };
    match &declared.kind {
        ParamKind::Switch => {
            if inline.is_some() {
                note!("mesh: {name}: flag `--{flag}` is a switch and takes no value");
                return Err(Step::Continue(2));
            }
            switches_on.insert(declared.name.as_str());
        }
        ParamKind::Flag(_) => {
            let Some(value) = inline else {
                note!("mesh: {name}: flag `--{flag}` requires a value (write `--{flag}=VALUE`)");
                return Err(Step::Continue(2));
            };
            // Last occurrence wins for a valued flag. A bare literal value is typed
            // like the same token passed positionally, so `--n=2` binds the integer
            // `2`; a quoted or interpolated value (`--n="2"`, `--n=$s`) keeps its
            // expanded string type.
            let value = if bare {
                expand::typed_scalar(&value)
            } else {
                Value::String(value)
            };
            flag_values.insert(declared.name.as_str(), value);
        }
        _ => unreachable!("only flags are collected here"),
    }
    Ok(())
}

/// Bind one value-mode `key: value` option to the switch or flag named `key`.
/// `force: true`/`false` sets a switch on/off; `tag: v` sets a valued flag (last
/// wins). A positional parameter is passed by position only, and an unknown name
/// is an error.
fn bind_named_option<'p>(
    name: &str,
    params: &'p [parser::Param],
    key: &str,
    value: Value,
    switches_on: &mut std::collections::HashSet<&'p str>,
    flag_values: &mut std::collections::HashMap<&'p str, Value>,
) -> Result<(), Step> {
    use parser::ParamKind;
    let Some(param) = params.iter().find(|param| param.name == key) else {
        note!("mesh: {name}: unknown option `{key}:`");
        return Err(Step::Continue(2));
    };
    match &param.kind {
        ParamKind::Switch => {
            let Value::Boolean(on) = value else {
                note!(
                    "mesh: {name}: switch `{key}:` takes a boolean (`{key}: true` or `{key}: false`)"
                );
                return Err(Step::Continue(2));
            };
            if on {
                switches_on.insert(param.name.as_str());
            } else {
                switches_on.remove(param.name.as_str());
            }
        }
        ParamKind::Flag(_) => {
            flag_values.insert(param.name.as_str(), value);
        }
        ParamKind::Required | ParamKind::Optional(_) | ParamKind::Rest => {
            note!(
                "mesh: {name}: `{key}` is a positional parameter, passed by position not `{key}:`"
            );
            return Err(Step::Continue(2));
        }
    }
    Ok(())
}

/// Evaluate a parameter's default expression in the function's fresh scope. A
/// nonlocal `exit`/`return` from the default is real control flow and unwinds
/// through the call; any other failure (a runtime error, or a `break`/`continue`
/// reported as outside a loop) fails the binding with a recoverable status.
fn evaluate_default(
    name: &str,
    param: &str,
    default: &parser::Expr,
    shell: &mut Shell,
) -> Result<Value, Step> {
    eval_expr(default, 0, true, shell).map_err(|step| match step {
        exit @ Step::Exit(_) => exit,
        ret @ Step::Return(_) => ret,
        _ => {
            note!("mesh: {name}: could not evaluate default for `{param}`");
            Step::Continue(2)
        }
    })
}

/// Return whether the parser needs another physical line to complete the input.
fn needs_more_input(text: &str) -> bool {
    !matches!(pending_input(text), Pending::Complete)
}

/// Why buffered input still needs another physical line, if it does.
enum Pending {
    /// A complete unit: run it.
    Complete,
    /// Inside a heredoc body, waiting for a line equal to this delimiter — the
    /// one case a reader can settle without re-parsing.
    Heredoc(String),
    /// Open for some other reason: a `func` body, a trailing `|`. Only a fresh
    /// parse of the whole buffer can tell when that closes.
    Other,
}

fn pending_input(text: &str) -> Pending {
    let trimmed = text.trim_start();
    let func_header = trimmed.strip_prefix("func").is_some_and(|rest| {
        rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace)
    });
    // A `func` header is judged by the brace scanner rather than the parser, so
    // that a malformed one is dispatched (and diagnosed) instead of buffering.
    let by_braces = || {
        if func_definition_is_open(text) {
            Pending::Other
        } else {
            Pending::Complete
        }
    };
    match parser::parse(text) {
        Ok(parser::ParseOutcome::IncompleteHeredoc(delimiter)) => Pending::Heredoc(delimiter),
        Ok(parser::ParseOutcome::Incomplete) if func_header => by_braces(),
        Ok(parser::ParseOutcome::Incomplete) => Pending::Other,
        Err(_) if func_header => by_braces(),
        Ok(parser::ParseOutcome::Complete(_)) | Err(_) => Pending::Complete,
    }
}

/// Does the buffered `func` definition in `text` need more input lines?
///
/// The one completeness question [`parse`](parser::parse) cannot answer for the
/// reader: a *malformed* `func` header must be dispatched so its error is
/// reported, not buffered until it swallows the commands after it. Once a body has
/// opened this is **brace-driven** ([`body_awaits_close`]) rather than parse-driven, so
/// `func f(x {` still buffers through to its matching `}` and stays quarantined.
///
/// Before any body `{` appears, the header may still legitimately be incomplete:
/// the grammar's `")" ws? "{"` lets the opening brace sit on a later line
/// (`ws` includes a newline). So a header that is a valid *incomplete* prefix —
/// still forming its signature, or closed with only whitespace after `)` — keeps
/// buffering ([`header_awaits_body`]); an already-malformed header is dispatched
/// immediately.
fn func_definition_is_open(text: &str) -> bool {
    match body_open_offset(text) {
        // A body has opened: buffer until its first matching `}`. Trailing text
        // after the close (or a reopened brace) still dispatches so the parser
        // reports it.
        Some(open) => body_awaits_close(&text[open..]),
        // No body has opened yet — the header is still forming, or is malformed.
        None => header_awaits_body(text),
    }
}

/// Byte offset of the body's opening `{`, located via the **signature grammar**
/// rather than a literal brace search, so a `{` that belongs elsewhere — inside a
/// following command (`func f()`⏎`puts '{'`) or hidden by a malformed quoted
/// parameter — is not mistaken for the body opener. The body opens only where a
/// `{` sits right after the signature `func name(params)` (whitespace between `)`
/// and `{`), or — for a malformed header whose `(` never closed (`func f(x {`) —
/// at the first `{` in the parameter region, so that definition still buffers
/// through to its matching `}` and stays quarantined. Returns `None` when no body
/// has opened, i.e. the header is still forming or is malformed without a brace.
fn body_open_offset(text: &str) -> Option<usize> {
    let paren = text.find('(');
    let brace = text.find('{');
    match paren {
        // `(` present and before any `{`: this is the signature. Find its matching
        // `)` honoring nested delimiters and quotes, so a `)`/`{` inside a default
        // expression (`x = (1 + 2)`, `x = {a: 1}`) is not mistaken for the closer.
        Some(open) if brace.is_none_or(|b| open < b) => {
            match signature_close(&text[open + 1..]) {
                // Signature closed: the body opens at the first non-whitespace after
                // `)` only if that is `{` (a real body opener); otherwise the `{`, if
                // any, is separate content and no body opens from this header.
                Some(rel) => {
                    let close = open + 1 + rel;
                    let tail = &text[close + 1..];
                    let trimmed = tail.trim_start();
                    trimmed
                        .starts_with('{')
                        .then(|| close + 1 + (tail.len() - trimmed.len()))
                }
                // The signature `(` never closed. If the partial list is still a
                // valid prefix, the signature is legitimately forming — including a
                // block-bearing default like `x = if c { … }` whose `{` is not the
                // body — so no body has opened yet; keep buffering for the `)`. Only
                // a provably-malformed header (`func f(x {`) treats an inner `{` as
                // the body opener so it buffers to the matching `}` and quarantines.
                None => {
                    let params = &text[open + 1..];
                    if params_valid(params) {
                        None
                    } else {
                        params.find('{').map(|rel| open + 1 + rel)
                    }
                }
            }
        }
        // `{` before any `(` (or no `(` at all): a body opener only if the header
        // text before it is a valid name prefix (`func f {`); otherwise the `{`
        // belongs to following content (`func`⏎`puts '{'`), not this header.
        _ => {
            let brace = brace?;
            let head = text[..brace]
                .trim_start()
                .strip_prefix("func")
                .unwrap_or("")
                .trim();
            (head.is_empty() || is_ident_prefix(head)).then_some(brace)
        }
    }
}

/// With no body `{` seen yet, is `text` a valid *incomplete* `func` header still
/// awaiting its `{`? True only while the header could still become a well-formed
/// `func name(params)` — the name so far is a valid identifier (or empty), and
/// once the signature's `)` is present it is preceded by a proper `name(` and
/// followed by only whitespace. Anything already impossible (a bad name, a
/// missing `(`, non-whitespace after `)`) returns false, so a malformed header is
/// dispatched immediately — its parse error reported — rather than buffering and
/// swallowing the commands that follow it.
fn header_awaits_body(text: &str) -> bool {
    // Only the *leading* whitespace is stripped here; the trailing newline is kept
    // so `params_valid` can tell a final name finalized by the line break (a
    // duplicate/reserved name to reject) from one the cursor is still extending.
    // The name check and the closed-signature branch trim locally where needed.
    let rest = text
        .trim_start()
        .strip_prefix("func")
        .unwrap_or("")
        .trim_start();
    let Some(paren) = rest.find('(') else {
        // No `(` yet: still forming the name. Keep reading while what we have is a
        // valid identifier *prefix* (or just `func`); an impossible head (`_`, a
        // digit) or an embedded space can never become a name, so dispatch it.
        let name = rest.trim_end();
        return name.is_empty() || is_ident_prefix(name);
    };
    // The name is everything before the `(` and must be a valid identifier.
    if !parser::valid_name(rest[..paren].trim()) {
        return false;
    }
    let after_open = &rest[paren + 1..];
    match signature_close(after_open) {
        // Params still forming: keep reading only while the partial list could
        // still be completed into a valid one (`func f(,` and `func f(a,,` can
        // never be repaired, so they dispatch immediately; `func f(...`, `func
        // f(--`, and `func f(a =` are valid new-form prefixes and keep reading).
        None => params_valid(after_open),
        // Signature closed: hand the finished parameter list to the real parser so
        // any signature it rejects — a bad shape (`(,)`, `(a,a)`) *or* a malformed
        // default expression (`x = ]`) — dispatches immediately instead of
        // buffering the commands after it. Only whitespace may sit between the
        // signature's `)` and the body `{`.
        Some(close) => {
            signature_parses(&after_open[..close]) && after_open[close + 1..].trim().is_empty()
        }
    }
}

/// Does the finished parameter list `params` form a valid signature? Validated by
/// the real parser (on a synthesized `func …(params) {}`), so default expressions
/// and the ordering rules are checked exactly as an executed definition would be —
/// the completeness helpers only approximate a *still-forming* list.
fn signature_parses(params: &str) -> bool {
    let probe = format!("func probe({params}) {{}}");
    matches!(parser::parse(&probe), Ok(parser::ParseOutcome::Complete(_)))
}

/// Is `s` a valid *prefix* of a kebab identifier — an ASCII-letter head followed
/// by identifier-body characters (alphanumeric, `_`, or `-`)? Used to decide
/// whether a still-forming name or parameter token could yet become a valid name;
/// an impossible head (`_`, a digit) or a stray character is rejected at once.
fn is_ident_prefix(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Byte offset (within `after_open`, the text just past the signature's opening
/// `(`) of the `)` that closes the signature. Delimiters, comments, quotes, raw
/// strings, and escapes are resolved by the **real tokenizer**
/// ([`parser::tokenize`]) rather than a bespoke char scan, so this cannot
/// disagree with the parser about where a token boundary is — a `)` inside a
/// default's string (`x = ")"`), nested delimiters (`x = (1 + 2)`, `x = [a, b]`),
/// or a comment (`x = 1 # )`) is never mistaken for the closer, and contextual
/// operators (`x = /#tag`) are boundaried exactly as the parser sees them.
/// Returns `None` while the signature is still open — including an unterminated
/// quote, which makes `tokenize` fail and leaves the reader buffering.
fn signature_close(after_open: &str) -> Option<usize> {
    use parser::TokenKind;
    // An unterminated construct (open quote/raw string) fails to tokenize; the
    // signature is still open, so `?` returns `None` and the reader buffers.
    let tokens = parser::tokenize(after_open).ok()?;
    // The open delimiters seen so far, by kind, so a close is matched against the
    // right one rather than a bare depth counter. `in_default` is true between a
    // signature-level `=` and the next signature-level `,`/`)`, marking that the
    // current parameter carries a default expression.
    let mut stack: Vec<char> = Vec::new();
    let mut in_default = false;
    for token in &tokens {
        match &token.value {
            // `$( … )` command capture opens with `CaptureStart` and closes with a
            // plain `RParen`, so it nests like a paren — otherwise the capture's
            // `)` would be mistaken for the signature close.
            TokenKind::LParen | TokenKind::CaptureStart => stack.push('('),
            TokenKind::LBracket => stack.push('['),
            TokenKind::LBrace => stack.push('{'),
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                let opener = match &token.value {
                    TokenKind::RParen => '(',
                    TokenKind::RBracket => '[',
                    _ => '{',
                };
                match stack.last().copied() {
                    // A matched close pops its nesting level and keeps scanning.
                    Some(open) if open == opener => {
                        stack.pop();
                    }
                    // A mismatched close inside a stray brace (`func f(x {`, no
                    // `=`) is the quarantine case: leave it to `body_open_offset`,
                    // which buffers a forming block default and quarantines a stray
                    // brace to its matching `}`.
                    Some('{') if !in_default => return None,
                    // Every other close resolves the signature region here: the
                    // signature's own `)` at the outer level, a mismatched close
                    // inside a default's `( … )`/`[ … ]`/`{ … }`, or a top-level
                    // stray `]`/`}`. Returning its offset hands the region to
                    // `signature_parses`, which accepts a real `)` close and
                    // rejects any malformed shape so the reader dispatches.
                    _ => return Some(token.span.start),
                }
            }
            TokenKind::Equal if stack.is_empty() => in_default = true,
            TokenKind::Comma if stack.is_empty() => in_default = false,
            _ => {}
        }
    }
    None
}

/// Does the still-open `func` parameter list `list` (the text just past `(`, no
/// `)` yet) let the definition keep buffering? Delegates to the **real** parser
/// ([`parser::params_prefix_status`]) so there is no second copy of the
/// signature grammar to drift from it: only a shape the parser can never accept
/// dispatches, while a valid or still-incomplete prefix keeps reading.
///
/// The reader-specific concern is a final token the user may still be typing.
/// [`strip_growing_tail`] removes it so the parser judges only the settled prefix
/// — a growing bare word (`a = 1, b`, where `b` may yet become `b = 2`) or a
/// growing string. Whether a growing string is allowed depends on where it sits:
/// after a `=` the parser is still expecting a value (an unterminated, possibly
/// multi-line string default keeps buffering), but at a name boundary a string
/// can never be a parameter name, so it dispatches.
fn params_valid(list: &str) -> bool {
    use parser::PrefixStatus;
    let (settled, tail) = strip_growing_tail(list);
    match parser::params_prefix_status(settled) {
        PrefixStatus::Malformed => false,
        // Still expecting more (a value after `=`, another parameter after `,`):
        // any growing tail — a name, a string default — is fine, keep reading.
        PrefixStatus::Incomplete => true,
        // A clean boundary: a parameter name is expected next. A growing bare word
        // is a valid name-start, but a growing string can never be a name.
        PrefixStatus::Complete => !matches!(tail, GrowingTail::Quote),
    }
}

/// A final token in a parameter list that the user may still be extending.
enum GrowingTail {
    /// Nothing strippable — the list ends at a settled boundary.
    None,
    /// A trailing bare word abutting end-of-input (a growing name/`--flag`/rest).
    Word,
    /// A trailing unterminated string (`x = "ab…`) — its value is still being
    /// typed and may run onto later lines.
    Quote,
}

/// Split off a final token that abuts the end of `list` and is still being typed,
/// returning the settled prefix before it and what kind of tail it was. Trailing
/// whitespace means the last token is already settled, so `list` is returned
/// whole with [`GrowingTail::None`]. A growing token implies no newline has been
/// typed yet, so there is never a following command to swallow — buffering it is
/// safe, and the settled prefix is what [`params_valid`] hands to the parser.
fn strip_growing_tail(list: &str) -> (&str, GrowingTail) {
    match parser::tokenize(list) {
        Ok(tokens) => match tokens.last() {
            Some(last)
                if matches!(last.value, parser::TokenKind::Word(_))
                    && last.span.end == list.len() =>
            {
                (&list[..last.span.start], GrowingTail::Word)
            }
            _ => (list, GrowingTail::None),
        },
        // Tokenizing failed on an unterminated string: strip from the opening
        // quote (its span starts there) so the settled prefix before it decides.
        // Any other tokenize failure isn't a growing tail — leave it for the
        // parser to reject.
        Err(error) if matches!(error.kind, parser::ParseErrorKind::Unterminated('\'' | '"')) => {
            (&list[..error.span.start], GrowingTail::Quote)
        }
        Err(_) => (list, GrowingTail::None),
    }
}

/// Is the `func` body starting at the `{` in `text` still waiting for its matching
/// `}`?
///
/// One question, and the reader's quarantine depends on it: until the `}` arrives,
/// every line belongs to the definition rather than to the top level.
///
/// Asked of the **real tokenizer** whenever the buffer lexes, so quoting, raw
/// strings, escapes, comments, and `${…}` interpolation are resolved exactly as the
/// parser resolves them, with no second set of rules to keep in step. A brace inside
/// a string (`puts "{"`), behind a backslash (`puts \\{`), or belonging to an
/// interpolation (`puts ${x}`) is not an `LBrace` token at all, so it cannot count.
///
/// The **first** `}` that returns the depth to zero settles it, not the final net
/// depth: trailing text that reopens a brace (`func f() {} {`) must not keep the
/// definition pending, so it is dispatched and the parser reports the documented
/// "unexpected text after the closing `}`" error.
///
/// When the buffer does **not** lex, the tokenizer cannot answer at all — it is
/// all-or-nothing, so one bad escape hides every brace after it — and both ways of
/// guessing are wrong in practice: assuming "still open" hangs on
/// `func f() { puts "\z" }`, whose `}` is right there, while assuming "closed" runs
/// the body's own later lines at the top level. The offending text is inside a
/// string, so it cannot move a brace; the braces are still there to be counted, and
/// [`scan_braces`] counts them without ever failing. Deciding by error *kind*, or by
/// blanking spans and retokenizing, both looked cheaper and both produced a string of
/// wrong answers — a zero-width diagnostic (`${}`) has nothing to blank, and errors
/// past a retry bound silently reverted to "open".
fn body_awaits_close(text: &str) -> bool {
    match parser::tokenize(text) {
        Ok(tokens) => first_close_at_depth_zero(&tokens).is_none(),
        Err(_) => scan_braces(text, 0).close.is_none(),
    }
}

/// The result of a bare-level brace scan (see [`scan_braces`]).
struct BraceScan {
    /// Byte offset of the `}` that first returned the depth to 0, if one was
    /// reached — used to split a `func` body from whatever follows its `}`.
    close: Option<usize>,
    /// Net `{` minus `}` at the bare (unquoted) level over the whole input.
    #[allow(dead_code)]
    depth: i32,
}

/// Count bare-level `{` / `}` in `text`, honoring the same quote, raw, escape, and
/// interpolation rules the tokenizer applies.
///
/// The **fallback** for [`body_awaits_close`] when `tokenize` fails, and the reason
/// it exists: the tokenizer is all-or-nothing, so a single bad escape hides every
/// brace after it, and a reader that cannot see the body's `}` either hangs on a
/// finished definition or leaks the body's own lines to the top level. This never
/// fails, which is the property that question needs.
///
/// An unterminated quote ends the scan at the line boundary (the rest of the input
/// is inside that string), so no `}` inside it is counted and buffering continues.
///
/// Counting starts from `start_depth` (0 for a whole-line check, 1 for a body
/// whose opening `{` has already been consumed).
fn scan_braces(text: &str, start_depth: i32) -> BraceScan {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut depth = start_depth;
    let mut close = None;
    // Raw-string eligibility: a raw prefix `r'`/`r"` is recognized only at a word
    // start or right after a bare `=`, as the tokenizer does.
    let mut word_start = true;
    let mut after_equals = false;
    let mut k = 0;
    while k < chars.len() {
        let (byte, c) = chars[k];
        let raw_eligible = word_start || after_equals;
        // Default: an ordinary bare-word char. Boundary arms below re-enable
        // raw eligibility exactly where the lexer would start a fresh word.
        word_start = false;
        after_equals = false;
        match c {
            _ if c.is_whitespace() => {
                word_start = true;
                k += 1;
            }
            // A backslash escapes the next char, so `\{` / `\}` are literal. A
            // `\`-newline is a line boundary, though: a function body runs line by
            // line, so the next word starts fresh there (a following raw prefix is
            // raw), exactly as an unescaped newline would reset it.
            '\\' => {
                if chars.get(k + 1).map(|&(_, c)| c) == Some('\n') {
                    word_start = true;
                }
                k += 2;
            }
            '\'' | '"' => match skip_quote(&chars, k + 1, c, true) {
                Some(next) => k = next,
                None => return BraceScan { close, depth },
            },
            'r' if raw_eligible
                && matches!(chars.get(k + 1).map(|&(_, c)| c), Some('\'') | Some('"')) =>
            {
                match skip_quote(&chars, k + 2, chars[k + 1].1, false) {
                    Some(next) => k = next,
                    None => return BraceScan { close, depth },
                }
            }
            // A bare `${…}` interpolation: its braces belong to the interpolation,
            // not to block structure, so skip to its close (as `parse_var` does)
            // without counting them. An unterminated `${` is a line-local error, so
            // it ends at the line boundary and leaves no dangling `{`.
            '$' if chars.get(k + 1).map(|&(_, c)| c) == Some('{') => {
                k = skip_interpolation(&chars, k + 2);
            }
            // A bare `{`/`}` is a block delimiter and a word boundary: the body's
            // first word begins right after the opening `{` (so a `func f(){r'…'}`
            // raw prefix is raw), and a fresh word follows the closing `}`.
            '{' => {
                depth += 1;
                word_start = true;
                k += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 && close.is_none() {
                    close = Some(byte);
                }
                word_start = true;
                k += 1;
            }
            // Operators a fresh word starts after (so a following
            // `r'…'` is raw): `;`, `|`/`||`, `<`, `>`/`>>`.
            ';' | '|' | '<' | '>' => {
                word_start = true;
                k += 1;
            }
            // Both `&&` (a separator) and a lone `&` (the background operator)
            // start a fresh word after them — so a
            // raw prefix that immediately follows (`true&r'…'`) is recognized as
            // raw here too, and the scan cannot mis-read it as a plain quote and
            // swallow the block's closing `}`.
            '&' => {
                word_start = true;
                if chars.get(k + 1).map(|&(_, c)| c) == Some('&') {
                    k += 2;
                } else {
                    k += 1;
                }
            }
            // A bare `=` lets a raw prefix begin the value (`k=r'v'`).
            '=' => {
                after_equals = true;
                k += 1;
            }
            _ => k += 1,
        }
    }
    BraceScan { close, depth }
}

/// Skip a quoted string from just past the opening `quote`. With `escapes`, a
/// backslash escapes the next char (matching `"…"`/`'…'`); without it (raw
/// `r'…'`/`r"…"`) no escape applies. An unterminated quote ends at the next line
/// break, so a brace after it is not miscounted. Returns the index past the
/// close (or the line boundary), or `None` at end of input with no close.
fn skip_quote(chars: &[(usize, char)], start: usize, quote: char, escapes: bool) -> Option<usize> {
    let mut k = start;
    while k < chars.len() {
        let c = chars[k].1;
        if c == '\n' {
            return Some(k); // unterminated quote ends at the line boundary
        }
        if escapes && c == '\\' {
            // A backslash escapes the next char, but never across a line break.
            if chars.get(k + 1).map(|&(_, c)| c) == Some('\n') {
                return Some(k + 1);
            }
            k += 2;
            continue;
        }
        if c == quote {
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

/// Index of the first `}` that returns brace depth to zero, if one is reached.
fn first_close_at_depth_zero(tokens: &[parser::Token]) -> Option<usize> {
    let mut depth = 0_i32;
    for (index, token) in tokens.iter().enumerate() {
        match token.value {
            parser::TokenKind::LBrace => depth += 1,
            parser::TokenKind::RBrace => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Line-incremental completeness for an input buffer that grows one physical
/// line at a time.
///
/// Completeness normally has to be re-derived from the whole buffer, because any
/// line can close the unit — a `func` body's `}` can sit anywhere. A heredoc body
/// is different: it is bulk data, and the only line that can end it is one equal
/// to the delimiter. Re-parsing after every body line therefore costs a full
/// tokenize of an ever-growing buffer, making ingestion quadratic in the body's
/// length — a 20,000-line heredoc read through a pipe took seconds, and larger
/// ones far worse. While a heredoc is open this answers from the newest line
/// alone and re-parses only once its delimiter finally arrives.
#[derive(Default)]
struct HeredocGate {
    awaiting: Option<String>,
}

impl HeredocGate {
    /// Would `line` leave an already-open heredoc body still open? Read-only and
    /// free of any parse, so a caller that only needs the answer (not the state
    /// update) can ask cheaply too.
    fn still_open(&self, line: &str) -> bool {
        self.awaiting
            .as_deref()
            .is_some_and(|delimiter| line.trim_end_matches(['\n', '\r']) != delimiter)
    }

    /// Does `text` — whose newest physical line is `line` — need more input?
    /// Records whichever heredoc is left open afterwards, so a second heredoc
    /// queued on the same command line is waited for in turn.
    fn needs_more_input(&mut self, text: &str, line: &str) -> bool {
        if self.still_open(line) {
            return true;
        }
        // The delimiter arrived (or none was open): re-derive from the whole
        // buffer, which is one full parse per heredoc rather than one per line.
        match pending_input(text) {
            Pending::Heredoc(delimiter) => {
                self.awaiting = Some(delimiter);
                true
            }
            Pending::Other => {
                self.awaiting = None;
                true
            }
            Pending::Complete => {
                self.awaiting = None;
                false
            }
        }
    }

    /// Forget any open heredoc: the buffer it belonged to was abandoned.
    fn reset(&mut self) {
        self.awaiting = None;
    }
}

#[derive(Default)]
struct ArgumentRecall {
    arguments: Vec<String>,
    inserted: Option<(usize, String, usize)>,
    previous: Option<String>,
}

impl ArgumentRecall {
    fn load(&mut self, history: &dyn History, session: Option<HistorySessionId>) {
        let Ok(entries) =
            history.search(SearchQuery::everything(SearchDirection::Backward, session))
        else {
            return;
        };
        let mut pending_by_session: Vec<(Option<HistorySessionId>, String)> = Vec::new();
        for entry in entries.into_iter().rev() {
            let index = pending_by_session
                .iter()
                .position(|(session, _)| *session == entry.session_id)
                .unwrap_or_else(|| {
                    pending_by_session.push((entry.session_id, String::new()));
                    pending_by_session.len() - 1
                });
            let pending = &mut pending_by_session[index].1;
            pending.push_str(&entry.command_line);
            pending.push('\n');
            if !needs_more_input(pending) {
                self.remember(pending.trim_end_matches('\n'));
                pending.clear();
            }
        }
    }

    fn remember(&mut self, line: &str) {
        self.inserted = None;
        // A blank submission is not an event — reedline never persists one, so
        // keep the prior command for last-argument recall and history designators.
        if line.trim().is_empty() {
            return;
        }
        self.previous = Some(line.to_owned());
        if let Some(argument) = last_argument(line) {
            self.arguments.push(argument);
        }
    }

    /// The most recently completed command line, the event `!^` / `!$` / `!*`
    /// expand against.
    fn previous(&self) -> Option<&str> {
        self.previous.as_deref()
    }

    fn insert(&mut self, editor: &mut Reedline) {
        let buffer = editor.current_buffer_contents();
        let cursor = editor.current_insertion_point();
        let repeated = self.inserted.as_ref().filter(|(start, text, _)| {
            cursor == *start + text.len()
                && buffer
                    .get(*start..cursor)
                    .is_some_and(|value| value == text)
        });
        let (start, old_len, index) = match repeated {
            Some((start, text, index)) => (*start, text.graphemes(true).count(), index + 1),
            None => (cursor, 0, 0),
        };
        let Some(argument) = self.arguments.iter().rev().nth(index).cloned() else {
            return;
        };
        editor.run_edit_commands(&[
            EditCommand::MoveToPosition {
                position: start,
                select: false,
            },
            EditCommand::ReplaceChars(old_len, argument.clone()),
        ]);
        self.inserted = Some((start, argument, index));
    }
}

fn persist_logical_history(
    history: &mut dyn History,
    session: Option<HistorySessionId>,
    signal: &Signal,
    pending: &str,
    gate: &HeredocGate,
    saved_submissions: usize,
    rewritten: bool,
) -> reedline::Result<()> {
    // A single physical line reedline already stored verbatim needs no work
    // unless history expansion rewrote it, in which case the stored raw row is
    // replaced below with the expanded command the shell actually ran.
    if pending.is_empty() && !rewritten {
        return Ok(());
    }
    let completed = completed_command(signal, pending, gate);
    if completed.is_none() && !matches!(signal, Signal::CtrlC | Signal::CtrlD) {
        return Ok(());
    }

    remove_recent_history_rows(history, session, saved_submissions)?;

    // An expansion that leaves only whitespace (a bare `!*` with no arguments,
    // with or without surrounding spaces) runs nothing, so drop the raw rows
    // above but store no replacement.
    if let Some(command) = completed.filter(|command| !command.trim().is_empty()) {
        let mut item = HistoryItem::from_command_line(command);
        item.session_id = session;
        history.save(item)?;
    }
    Ok(())
}

/// Delete the `count` most recent rows for `session`, newest first. Reassembly
/// of a multi-line command and history expansion both use this to drop the raw
/// per-line rows reedline saves before re-storing the logical command; a failed
/// expansion uses it to discard the line that never ran.
fn remove_recent_history_rows(
    history: &mut dyn History,
    session: Option<HistorySessionId>,
    count: usize,
) -> reedline::Result<()> {
    let mut remaining = count;
    if remaining == 0 {
        return Ok(());
    }
    let entries = history.search(SearchQuery::everything(SearchDirection::Backward, session))?;
    for entry in entries
        .into_iter()
        .filter(|entry| entry.session_id == session)
    {
        if remaining == 0 {
            break;
        }
        if let Some(id) = entry.id {
            history.delete(id)?;
            remaining -= 1;
        }
    }
    Ok(())
}

fn last_argument(line: &str) -> Option<String> {
    command_words(line)?.get(1..)?.last().cloned()
}

/// The source text of each word in the last pipeline stage of the last
/// statement, command word first. Backs last-argument recall and the
/// `!^` / `!$` / `!*` history designators, which slice this list.
fn command_words(line: &str) -> Option<Vec<String>> {
    let parser::ParseOutcome::Complete(source) = parser::parse(line).ok()? else {
        return None;
    };
    let statement = source.statements.last()?;
    let executable = statement
        .and_or
        .rest
        .last()
        .map_or(&statement.and_or.first, |(_, executable)| executable);
    let parser::Executable::Pipeline(pipeline) = executable else {
        return None;
    };
    let words: Vec<String> = pipeline
        .stages
        .last()?
        .items
        .iter()
        .filter_map(|item| match item {
            parser::CommandItem::Word(word) => line.get(word.span.clone()).map(str::to_owned),
            parser::CommandItem::Redirect { .. } => None,
        })
        .collect();
    (!words.is_empty()).then_some(words)
}

/// Locate the interactive `!^` / `!$` / `!*` history word designators in `line`
/// that sit in bare (unquoted, unescaped) text, honoring the parser's quote,
/// escape, raw-string, and interpolation rules so a designator inside any of them
/// stays literal. Strings span physical lines exactly as the parser reads them,
/// so `prefix` — the already-buffered lines of a multi-line command — seeds the
/// quote and raw-eligibility state. Returns each designator's byte offset within
/// `line` (not the combined text) and its character, left to right.
fn history_designators(prefix: &str, line: &str) -> Vec<(usize, char)> {
    let prefix_len = prefix.len();
    let combined: Vec<(usize, char)> = prefix
        .char_indices()
        .chain(line.char_indices().map(|(byte, c)| (byte + prefix_len, c)))
        .collect();
    let mut hits = Vec::new();
    // Raw-string eligibility, tracked exactly as `split_line` / `scan_braces`.
    let mut word_start = true;
    let mut after_equals = false;
    // Heredoc delimiters queued on the current line, consumed at its newline.
    let mut heredocs: Vec<String> = Vec::new();
    let mut k = 0;
    while k < combined.len() {
        let (byte, c) = combined[k];
        let raw_eligible = word_start || after_equals;
        word_start = false;
        after_equals = false;
        match c {
            // The newline after a `<<delim` line begins the queued heredoc bodies;
            // their raw document text never expands, so skip past each body and its
            // closing delimiter line (as `consume_heredocs` records them).
            '\n' if !heredocs.is_empty() => {
                k += 1;
                for delimiter in std::mem::take(&mut heredocs) {
                    loop {
                        let line_start = k;
                        while k < combined.len() && combined[k].1 != '\n' {
                            k += 1;
                        }
                        let mut body_line: String =
                            combined[line_start..k].iter().map(|&(_, c)| c).collect();
                        if body_line.ends_with('\r') {
                            body_line.pop();
                        }
                        let at_end = k >= combined.len();
                        if !at_end {
                            k += 1;
                        }
                        if body_line == delimiter || at_end {
                            break;
                        }
                    }
                }
                word_start = true;
            }
            _ if c.is_whitespace() => {
                word_start = true;
                k += 1;
            }
            // A bare backslash escapes the next char, so `\!` / `\'` are literal.
            // A `\`-newline is a line continuation the parser drops without
            // starting a word, so it preserves token-start eligibility (a comment
            // or raw prefix can still begin the next line); any other escaped char
            // becomes word text.
            '\\' => {
                if combined.get(k + 1).map(|&(_, c)| c) == Some('\n') {
                    word_start = raw_eligible;
                }
                k += 2;
            }
            '\'' | '"' => k = skip_string(&combined, k + 1, c, true),
            'r' if raw_eligible
                && matches!(combined.get(k + 1).map(|&(_, c)| c), Some('\'') | Some('"')) =>
            {
                k = skip_string(&combined, k + 2, combined[k + 1].1, false);
            }
            '$' if combined.get(k + 1).map(|&(_, c)| c) == Some('{') => {
                k = skip_interpolation(&combined, k + 2);
            }
            // A bare `#` (at a token start) begins a comment: the parser discards
            // the rest of the physical line, so no designator past it expands.
            '#' if raw_eligible => {
                while k < combined.len() && combined[k].1 != '\n' {
                    k += 1;
                }
            }
            '!' => match combined.get(k + 1).map(|&(_, c)| c) {
                Some(designator @ ('^' | '$' | '*')) => {
                    if byte >= prefix_len {
                        hits.push((byte - prefix_len, designator));
                    }
                    k += 2;
                }
                // `!=` / `!~` are operator tokens, hence word boundaries; a lone
                // `!` is ordinary word text.
                Some('=' | '~') => {
                    word_start = true;
                    k += 2;
                }
                _ => k += 1,
            },
            // `<<` is the heredoc operator: capture its delimiter word so the body
            // on the following line(s) can be skipped as literal document text.
            // A bare designator in the delimiter still expands, so its hit is
            // recorded while the delimiter is read.
            '<' if combined.get(k + 1).map(|&(_, c)| c) == Some('<') => {
                let (delimiter, next) =
                    read_heredoc_delimiter(&combined, k + 2, prefix_len, &mut hits);
                if let Some(delimiter) = delimiter {
                    heredocs.push(delimiter);
                }
                word_start = true;
                k = next;
            }
            // Punctuation the parser's tokenizer treats as its own token is a word
            // boundary, so the next word is fresh and a raw prefix there is raw:
            // the first word of a compact `func f(){…}` body or `f(…, …)` call,
            // and — since the parser re-merges adjacent tokens into a command word
            // — the raw prefix in `key:r'…'` or `a.r'…'` too.
            '{' | '}' | '(' | ')' | '[' | ']' | ',' | ':' | '.' | ';' | '|' | '<' | '>' => {
                word_start = true;
                k += 1;
            }
            '&' => {
                word_start = true;
                k += if combined.get(k + 1).map(|&(_, c)| c) == Some('&') {
                    2
                } else {
                    1
                };
            }
            '=' => {
                after_equals = true;
                k += 1;
            }
            _ => k += 1,
        }
    }
    hits
}

/// Read a heredoc delimiter word starting just past `<<`, returning its text
/// (quotes stripped and pieces concatenated, matching `word.text()`) and the
/// index after it. Leading spaces/tabs are skipped; the word is a run of bare,
/// quoted, and escaped pieces (`"EO"F` → `EOF`) ending at whitespace or token
/// punctuation. A bare `!^` / `!$` / `!*` in the delimiter is a designator that
/// still expands, so its hit is recorded (respecting the `prefix` boundary).
/// Returns `None` when no delimiter word is present.
fn read_heredoc_delimiter(
    chars: &[(usize, char)],
    mut k: usize,
    prefix_len: usize,
    hits: &mut Vec<(usize, char)>,
) -> (Option<String>, usize) {
    while k < chars.len() && matches!(chars[k].1, ' ' | '\t') {
        k += 1;
    }
    let mut delimiter = String::new();
    let mut consumed = false;
    while let Some(&(byte, ch)) = chars.get(k) {
        match ch {
            ' ' | '\t' | '\n' => break,
            '{' | '}' | '(' | ')' | '[' | ']' | ',' | ':' | '.' | ';' | '|' | '<' | '>' | '&'
            | '=' => break,
            '\'' | '"' => {
                consumed = true;
                k += 1;
                while let Some(&(_, inner)) = chars.get(k) {
                    if inner == '\\'
                        && let Some(&(_, escaped)) = chars.get(k + 1)
                    {
                        delimiter.push(escaped);
                        k += 2;
                        continue;
                    }
                    if inner == ch {
                        k += 1;
                        break;
                    }
                    delimiter.push(inner);
                    k += 1;
                }
            }
            '\\' => {
                consumed = true;
                if let Some(&(_, escaped)) = chars.get(k + 1) {
                    delimiter.push(escaped);
                    k += 2;
                } else {
                    k += 1;
                }
            }
            '!' if matches!(chars.get(k + 1).map(|&(_, c)| c), Some('^' | '$' | '*')) => {
                let designator = chars[k + 1].1;
                if byte >= prefix_len {
                    hits.push((byte - prefix_len, designator));
                }
                delimiter.push('!');
                delimiter.push(designator);
                consumed = true;
                k += 2;
            }
            _ => {
                delimiter.push(ch);
                consumed = true;
                k += 1;
            }
        }
    }
    (consumed.then_some(delimiter), k)
}

/// Skip from just past an opening `quote` to just past its close, using the
/// tokenizer's string rules: with `escapes`, a backslash escapes the next char (so
/// `\'` / `\"` do not close). A newline is an ordinary character — `"…"` / `'…'`
/// span physical lines exactly as the tokenizer reads them — and an unterminated
/// string runs to the end of the text. Returns the index past the close, or past
/// the end when unterminated.
fn skip_string(chars: &[(usize, char)], start: usize, quote: char, escapes: bool) -> usize {
    let mut k = start;
    while k < chars.len() {
        let c = chars[k].1;
        if escapes && c == '\\' {
            k += 2;
            continue;
        }
        if c == quote {
            return k + 1;
        }
        k += 1;
    }
    k
}

/// Skip a bare `${…}` interpolation from just past the `{`. Its braces do not
/// count as block structure. Stops at the closing `}` or a line break.
fn skip_interpolation(chars: &[(usize, char)], start: usize) -> usize {
    let mut k = start;
    while k < chars.len() {
        match chars[k].1 {
            '}' => return k + 1,
            '\n' => return k,
            _ => k += 1,
        }
    }
    k
}

/// Expand the interactive `!^` / `!$` / `!*` history word designators against
/// the previous command line, leaving everything else byte-for-byte unchanged.
///
/// This runs as a pre-parse pass but stays quote-safe by delegating the scan to
/// [`history_designators`], which honors the parser's own quote, escape,
/// raw-string, and interpolation rules: a `!` inside a string (including one that
/// spans continuation lines), after a backslash, or not followed by a supported
/// designator is left literal (the deferred `!!` / `!string` / `!n` forms fall
/// through here). `!^` is the first argument of the previous command, `!$` its
/// last argument, and `!*` all of them joined by spaces. An empty argument list —
/// a bare command, an assignment, or any line without sliceable command words —
/// leaves `!*` empty but makes `!^` / `!$` an error; only a truly absent event
/// (no previous command) errors for all three.
///
/// `pending` is the already-buffered part of a multi-line command (empty for a
/// single line); it seeds the scanner's quote and raw-eligibility state so a
/// designator inside a quote opened on an earlier line stays literal.
fn expand_history_designators(
    line: &str,
    pending: &str,
    previous: Option<&str>,
) -> Result<String, String> {
    let designators = history_designators(pending, line);
    if designators.is_empty() {
        return Ok(line.to_owned());
    }
    let mut words: Option<Vec<String>> = None;
    let mut computed = false;
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for (offset, designator) in designators {
        out.push_str(&line[last..offset]);
        if previous.is_none() {
            return Err(format!("!{designator}: no previous command"));
        }
        if !computed {
            words = previous.and_then(command_words);
            computed = true;
        }
        // A previous line with no sliceable command words — an assignment, a
        // control-flow statement, an unparsable line — has an empty argument
        // list, exactly like a bare command.
        let arguments = words
            .as_deref()
            .and_then(|words| words.get(1..))
            .unwrap_or_default();
        let no_arguments = || format!("!{designator}: previous command has no arguments");
        match designator {
            '^' => out.push_str(arguments.first().ok_or_else(no_arguments)?),
            '$' => out.push_str(arguments.last().ok_or_else(no_arguments)?),
            _ => out.push_str(&arguments.join(" ")),
        }
        last = offset + '!'.len_utf8() + designator.len_utf8();
    }
    out.push_str(&line[last..]);
    Ok(out)
}

fn run_interactive(options: &StartupOptions) -> ExitCode {
    if let Err(err) = wait_until_foreground() {
        note!("mesh: could not acquire terminal foreground: {err}");
        return ExitCode::from(1);
    }
    if let Err(err) = ignore_interactive_signals() {
        note!("mesh: could not configure interactive signals: {err}");
        return ExitCode::from(1);
    }
    let completion = Arc::new(RwLock::new(CompletionState::default()));
    let keybindings = interactive_keybindings();
    let completion_menu = completion_menu();
    let mut editor = Reedline::create()
        // `A` and `B` — where the prompt starts and where the user's input does.
        // The shell emits `C` and `D` itself, at `PreExec` and `PostExec`; see
        // `semantic_mark`. Both halves have to be present for a terminal to make
        // sense of the stream, and only reedline knows where it drew the prompt.
        .with_semantic_markers(Some(Osc133Markers::boxed()))
        // Bracketed paste (`CSI ?2004 h`), per `DESIGN.md` "terminal control":
        // pasted text is *inserted*, not executed line by line. reedline's guard
        // defaults to off, so without this a paste's newlines each arrive as
        // Enter and every line but the last runs before it can be read.
        .use_bracketed_paste(true)
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
        .with_quick_completions(true)
        .with_highlighter(Box::new(input_highlighter()))
        .with_visual_selection_style(nu_ansi_term::Style::default())
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu)))
        .with_completer(Box::new(MeshCompleter {
            state: Arc::clone(&completion),
        }));
    let mut argument_recall = ArgumentRecall::default();
    let mut history_session = None;
    if options.save_history
        && let Some(path) = history_path()
    {
        let session = Reedline::create_history_session_id();
        let session_started = Some(std::time::SystemTime::now().into());
        let history = prepare_history_path(&path)
            .map_err(|err| err.to_string())
            .and_then(|()| open_history(path, session, session_started));
        match history {
            Ok(history) => {
                argument_recall.load(&history, session);
                history_session = session;
                editor = editor
                    .with_history(Box::new(history))
                    .with_history_session_id(session);
            }
            Err(err) => note!("mesh: could not open history database: {err}"),
        }
    }
    let mut shell = Shell::new();
    shell
        .vars
        .set_invocation(options.name.clone(), options.args.clone());
    // The only loop that is an interactive session. `mesh -s` on a terminal
    // reads commands but is not one, so this is recorded by the loop rather than
    // derived from `isatty`.
    shell.vars.set_interactive(true);
    let (origin, source) = options.origin(true);
    shell.vars.set_origin(origin, source);
    let mut last = match run_startup_files(options, true, 0, &mut shell) {
        Step::Continue(code) => code,
        Step::Exit(code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
        Step::Return(value) => {
            return ExitCode::from(run_logout(options, status_of(&value), &mut shell));
        }
    };
    let mut pending = String::new();
    let mut gate = HeredocGate::default();
    let mut pending_history_rows = 0;
    loop {
        shell.jobs.reap();
        if pending.is_empty() {
            run_prompt_hooks(PromptEvent::PrePrompt, Vec::new(), &mut shell);
            // After the hooks, so a `preprompt` handler that cds is reported from
            // where it left the shell rather than from where it found it. Only for
            // a fresh command: a continuation line is the same command line still
            // being typed, and the shell cannot have moved between the two.
            report_cwd(shell.vars.interactive());
            set_title(shell.vars.interactive(), &environment_prompt_title());
        }
        *completion.write().expect("completion state poisoned") =
            CompletionState::from_shell(&shell);
        let prompt = MeshPrompt {
            failed: last != 0,
            continuation: !pending.is_empty(),
            custom: shell.prompt.text.clone(),
        };
        match editor.read_line(&prompt) {
            Ok(Signal::HostCommand(command)) if command == "mesh:recall-last-argument" => {
                argument_recall.insert(&mut editor);
            }
            Ok(signal) => {
                let mut rewritten = false;
                // Reedline persists a raw row for every non-empty submitted line,
                // even when history expansion later empties it (a bare `!*` with
                // no arguments), so count the raw submission — not the expanded
                // signal — to keep the row bookkeeping accurate.
                let mut raw_saved = false;
                let signal = match signal {
                    Signal::Success(line) if !line.is_empty() => {
                        raw_saved = true;
                        match expand_history_designators(
                            &line,
                            &pending,
                            argument_recall.previous(),
                        ) {
                            Ok(expanded) => {
                                if expanded != line {
                                    note!("{expanded}");
                                    rewritten = true;
                                }
                                Signal::Success(expanded)
                            }
                            Err(message) => {
                                note!("mesh: {message}");
                                // The line never runs, so drop the raw row
                                // reedline stored for it before re-prompting.
                                if let Some(session) = history_session {
                                    let _ = remove_recent_history_rows(
                                        editor.history_mut(),
                                        Some(session),
                                        1,
                                    );
                                }
                                last = 2;
                                continue;
                            }
                        }
                    }
                    other => other,
                };
                if history_session.is_some() && raw_saved {
                    pending_history_rows += 1;
                }
                let completed_command = completed_command(&signal, &pending, &gate);
                if let Some(session) = history_session
                    && let Err(err) = persist_logical_history(
                        editor.history_mut(),
                        Some(session),
                        &signal,
                        &pending,
                        &gate,
                        pending_history_rows,
                        rewritten,
                    )
                {
                    note!("mesh: could not update history database: {err}");
                }
                match handle_signal(signal, last, &mut shell, &mut pending, &mut gate) {
                    None => continue, // an unfinished `func` body: read the next line
                    Some(Step::Exit(code)) => {
                        run_prompt_hooks(
                            PromptEvent::Exit,
                            vec![Value::Integer(i64::from(code))],
                            &mut shell,
                        );
                        return ExitCode::from(run_logout(options, code, &mut shell));
                    }
                    Some(Step::Continue(code)) => last = code,
                    // Top-level `run_line` reports a stray `return` itself, so one
                    // never reaches here.
                    Some(Step::Return(_)) => unreachable!("top-level return handled in run_line"),
                }
                pending_history_rows = 0;
                if let Some(command) = completed_command {
                    argument_recall.remember(&command);
                }
            }
            Err(err) => {
                note!("mesh: line editor error: {err}");
                return ExitCode::from(run_logout(options, 1, &mut shell));
            }
        }
    }
}

fn interactive_keybindings() -> Keybindings {
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::ALT,
        KeyCode::Char('.'),
        ReedlineEvent::ExecuteHostCommand("mesh:recall-last-argument".to_owned()),
    );
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU.to_owned()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keybindings
}

fn input_highlighter() -> SimpleMatchHighlighter {
    SimpleMatchHighlighter::default().with_neutral_style(nu_ansi_term::Style::new().bold())
}

fn completion_menu() -> ColumnarMenu {
    let plain = nu_ansi_term::Style::default();
    let selected = plain.bold().reverse();
    ColumnarMenu::default()
        .with_name(COMPLETION_MENU)
        .with_text_style(plain)
        .with_selected_text_style(selected)
        .with_description_text_style(plain)
        .with_match_text_style(plain.underline())
        .with_selected_match_text_style(selected.underline())
}

fn completed_command(signal: &Signal, pending: &str, gate: &HeredocGate) -> Option<String> {
    let Signal::Success(line) = signal else {
        return None;
    };
    // Asking the gate first keeps a heredoc body from being re-parsed here as
    // well as in `handle_signal`, which would restore the quadratic cost the
    // gate exists to remove.
    if gate.still_open(line) {
        return None;
    }
    let mut command = String::with_capacity(pending.len() + line.len() + 1);
    command.push_str(pending);
    command.push_str(line);
    command.push('\n');
    (!needs_more_input(&command)).then(|| command.trim_end_matches('\n').to_owned())
}

#[derive(Default)]
struct CompletionState {
    commands: Vec<String>,
    help: HashMap<String, CompletionSpec>,
    cache: CompletionCache,
    variables: Vec<(String, Value)>,
}

impl CompletionState {
    fn from_shell(shell: &Shell) -> Self {
        let mut commands: Vec<String> = builtins::NAMES.iter().map(|name| (*name).into()).collect();
        commands.extend(shell.funcs.names().map(str::to_owned));
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                commands.extend(entries.flatten().filter_map(|entry| {
                    use std::os::unix::fs::PermissionsExt;
                    let metadata = entry.metadata().ok()?;
                    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                        .then(|| entry.file_name().to_string_lossy().into_owned())
                }));
            }
        }
        commands.sort();
        commands.dedup();
        let mut help: HashMap<_, _> = builtins::NAMES
            .iter()
            .filter_map(|name| {
                builtins::help(name).map(|text| ((*name).into(), CompletionSpec::from_help(&text)))
            })
            .collect();
        help.extend(shell.funcs.names().filter_map(|name| {
            shell
                .funcs
                .get(name)
                .map(|def| (name.into(), CompletionSpec::from_help(&def.help(name))))
        }));
        Self {
            commands,
            help,
            cache: CompletionCache::default(),
            variables: shell
                .vars
                .visible()
                .map(|(n, v)| (n.into(), v.clone()))
                .collect(),
        }
    }
}

struct MeshCompleter {
    state: Arc<RwLock<CompletionState>>,
}

impl Completer for MeshCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let line = &line[..pos];
        let start = line.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        let word = &line[start..];
        let state = self.state.read().expect("completion state poisoned");
        let values = if word.starts_with('$') {
            variable_completions(word, &state.variables)
        } else if command_position(&line[..start]) {
            rank_candidates(state.commands.clone(), word)
        } else if let Some(words) = command_segment_words(line) {
            argument_completions(&state, &words, word)
        } else {
            path_completions(word)
        };
        suggestions(values, start, pos)
    }
}

fn suggestions(values: Vec<String>, start: usize, pos: usize) -> Vec<Suggestion> {
    values
        .into_iter()
        .map(|value| Suggestion {
            value,
            span: Span::new(start, pos),
            append_whitespace: false,
            ..Suggestion::default()
        })
        .collect()
}

fn argument_completions(state: &CompletionState, words: &[String], word: &str) -> Vec<String> {
    if let Some((option, prefix)) = word.split_once('=') {
        let context = &words[..words.len().saturating_sub(1)];
        if let Some(hint) = completion_for(state, context).value_hint(option) {
            return value_completions(hint, prefix)
                .into_iter()
                .map(|value| format!("{option}={value}"))
                .collect();
        }
    }

    let completing_word = !word.is_empty();
    let parent = if completing_word {
        &words[..words.len().saturating_sub(1)]
    } else {
        words
    };
    if let Some(option) = parent.last()
        && option.starts_with('-')
    {
        let context = &parent[..parent.len() - 1];
        if let Some(hint) = completion_for(state, context).value_hint(option) {
            return value_completions(hint, word);
        }
    }
    let parent_help = completion_for(state, parent);
    let paths = parent_help.positional_hint().map_or_else(
        || path_completions(word),
        |hint| value_completions(hint, word),
    );
    let mut parent_values = parent_help.matching(word);

    if word.starts_with('-') {
        return parent_values;
    }
    let exact_subcommand = parent_values.iter().any(|value| value == word);
    parent_values.retain(|value| value != word);
    if !parent_values.is_empty() {
        let mut seen: HashSet<_> = parent_values.iter().cloned().collect();
        parent_values.extend(paths.into_iter().filter(|path| seen.insert(path.clone())));
        return parent_values;
    }
    if !paths.is_empty() {
        return paths;
    }
    if completing_word && !exact_subcommand {
        return Vec::new();
    }

    // Once the current word is a complete subcommand, include it in the help
    // request so `git reset<Tab>` asks `git reset --help` for the next word.
    let help_words = if exact_subcommand || !completing_word {
        words
    } else {
        parent
    };
    let mut values = completion_for(state, help_words).matching("");
    if completing_word && exact_subcommand {
        values = values
            .into_iter()
            .map(|value| format!("{word} {value}"))
            .collect();
    }
    values
}

fn value_completions(hint: &ValueHint, prefix: &str) -> Vec<String> {
    match hint {
        ValueHint::File => path_completions_with(prefix, false),
        ValueHint::Directory => path_completions_with(prefix, true),
        ValueHint::Enum(values) => rank_candidates(values.clone(), prefix),
    }
}

fn completion_for(state: &CompletionState, words: &[String]) -> CompletionSpec {
    let Some(command) = words.first() else {
        return CompletionSpec::default();
    };
    state
        .help
        .get(command)
        .cloned()
        .unwrap_or_else(|| state.cache.spec_for(words))
}

fn command_segment_words(line: &str) -> Option<Vec<String>> {
    let mut segment_start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (at, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if quote.is_none() && matches!(character, ';' | '|' | '&' | '{' | '}') {
            segment_start = at + character.len_utf8();
        }
    }
    let words: Vec<String> = line[segment_start..]
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if words.is_empty() { None } else { Some(words) }
}

#[cfg(test)]
fn help_completions(help: &str, prefix: &str) -> Vec<String> {
    CompletionSpec::from_help(help).matching(prefix)
}

fn command_position(before: &str) -> bool {
    let before = before.trim_end();
    before.is_empty() || before.ends_with([';', '|', '&', '{'])
}

fn variable_completions(word: &str, variables: &[(String, Value)]) -> Vec<String> {
    let path = &word[1..];
    let mut parts = path.split('.');
    let root = parts.next().unwrap_or_default();
    let tail: Vec<_> = parts.collect();
    if tail.is_empty() {
        return rank_candidates(
            variables
                .iter()
                .map(|(name, _)| format!("${name}"))
                .collect(),
            word,
        );
    }
    let Some((root_name, root_value)) =
        variables.iter().find(|(name, _)| name == root).or_else(|| {
            smart_case_fallback(root)
                .then(|| {
                    variables
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(root))
                })
                .flatten()
        })
    else {
        return Vec::new();
    };
    let mut resolved = root_name.clone();
    let mut value = root_value;
    for key in &tail[..tail.len() - 1] {
        let Value::Map(entries) = value else {
            return Vec::new();
        };
        let Some((name, next)) = entries.iter().find(|(name, _)| name == key).or_else(|| {
            smart_case_fallback(key)
                .then(|| {
                    entries
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(key))
                })
                .flatten()
        }) else {
            return Vec::new();
        };
        resolved.push('.');
        resolved.push_str(name);
        value = next;
    }
    let Value::Map(entries) = value else {
        return Vec::new();
    };
    rank_candidates(
        entries
            .iter()
            .map(|(key, _)| format!("${resolved}.{key}"))
            .collect(),
        word,
    )
}

fn smart_case_fallback(query: &str) -> bool {
    !query.chars().any(char::is_uppercase)
}

fn path_completions(word: &str) -> Vec<String> {
    path_completions_with(word, false)
}

fn path_completions_with(word: &str, directories_only: bool) -> Vec<String> {
    let word = word.to_owned();
    interruptible_task(Duration::from_millis(200), move || {
        path_completions_sync(&word, directories_only)
    })
    .unwrap_or_default()
}

fn interruptible_task<T: Send + 'static>(
    timeout: Duration,
    task: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(task());
    });
    receiver.recv_timeout(timeout).ok()
}

fn path_completions_sync(word: &str, directories_only: bool) -> Vec<String> {
    let path = std::path::Path::new(word);
    let (dir, prefix) = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            (parent, name.to_string_lossy())
        }
        _ => (std::path::Path::new("."), std::borrow::Cow::Borrowed(word)),
    };
    let display_dir = if dir == std::path::Path::new(".") {
        "".into()
    } else {
        format!("{}/", dir.display())
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            if directories_only && !entry.path().is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            Some((
                name.clone(),
                format!(
                    "{display_dir}{name}{}",
                    if entry.path().is_dir() { "/" } else { "" }
                ),
            ))
        })
        .collect();
    out.sort_by(|left, right| left.0.cmp(&right.0));
    let ranked_names = rank_candidates(
        out.iter().map(|(name, _)| name.clone()).collect(),
        prefix.as_ref(),
    );
    let mut by_name: std::collections::HashMap<_, _> = out.into_iter().collect();
    ranked_names
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect()
}

/// Let the parent job-control shell stop and later foreground mesh before the
/// line editor performs its first terminal read.
fn wait_until_foreground() -> io::Result<()> {
    // A parent shell may itself ignore SIGTTIN. Startup needs the default
    // disposition so the kernel can suspend `mesh &` until the user runs `fg`.
    if unsafe { libc::signal(libc::SIGTTIN, libc::SIG_DFL) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    loop {
        // SAFETY: these calls take no pointers; fd 0 is known to be a terminal
        // because only the interactive path calls this function.
        let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if foreground < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: getpgrp cannot fail and returns mesh's process group.
        let shell_group = unsafe { libc::getpgrp() };
        if foreground == shell_group {
            return Ok(());
        }
        // SAFETY: a zero PID sends SIGTTIN to mesh's process group. With the
        // default disposition above, execution resumes here after `fg`/SIGCONT.
        if unsafe { libc::kill(0, libc::SIGTTIN) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
}

/// Keep terminal-generated signals from stopping or ending mesh itself.
/// Foreground children restore their default dispositions before `exec` and
/// receive these signals after the executor hands them the terminal.
fn ignore_interactive_signals() -> io::Result<()> {
    for signal in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTSTP,
        libc::SIGTTOU,
        libc::SIGTERM,
    ] {
        // SAFETY: signal is one of the valid constants above, and SIG_IGN is a
        // valid disposition. The interactive loop is single-threaded here.
        if unsafe { libc::signal(signal, libc::SIG_IGN) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Handle a reedline signal, buffering input while the parser reports it as
/// incomplete. Extracted from the read loop so the interactive control flow is
/// unit-testable without a terminal.
///
/// `Ctrl-D` on an empty line exits (and abandons any in-progress `func`);
/// `Ctrl-C` cancels the current line/buffer and re-prompts, keeping the status.
fn handle_signal(
    signal: Signal,
    last: u8,
    shell: &mut Shell,
    pending: &mut String,
    gate: &mut HeredocGate,
) -> Option<Step> {
    match signal {
        Signal::Success(line) => {
            pending.push_str(&line);
            pending.push('\n');
            if gate.needs_more_input(pending, &line) {
                return None;
            }
            let text = std::mem::take(pending);
            let command = text.trim_end_matches('\n').to_string();
            // A bare Enter submitted no command: nothing runs, nothing prints.
            // Marking it would give a terminal an empty command block to offer to
            // fold and to badge with a status the user never caused, and the
            // hooks would be answering about a command line that does not exist —
            // the same reason history does not keep a row for it. `run_line`
            // still runs, so the blank line clears top-level control state
            // exactly as it did before.
            if command.trim().is_empty() {
                return Some(run_line(&text, last, false, shell));
            }
            let marks = shell.vars.interactive();
            // Before `C`, since a title is not output: it belongs outside the
            // region a terminal will offer to fold, next to the submission that
            // caused it. The prompt's title comes back at the next prompt, so a
            // command's title lasts exactly as long as the command.
            set_title(marks, &running_title(&command));
            // Both marks sit outside the hooks, so that everything printed
            // because this command was submitted falls inside the region they
            // bracket. A `preexec` hook that writes before `C` is folded into
            // the command line the user typed; a `postexec` hook that writes
            // after `D` lands outside the output a terminal will offer to fold.
            semantic_mark(marks, SemanticMark::OutputStart);
            run_prompt_hooks(
                PromptEvent::PreExec,
                vec![Value::String(command.clone())],
                shell,
            );
            // The clock still starts here: `elapsed` is the command's own, and
            // reporting a hook's time as part of it would make the number
            // depend on what happens to be registered.
            let start = Instant::now();
            let step = run_line(&text, last, false, shell);
            let status = step.status();
            let elapsed = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
            run_prompt_hooks(
                PromptEvent::PostExec,
                vec![
                    Value::String(command),
                    Value::Integer(i64::from(status)),
                    Value::Integer(elapsed),
                ],
                shell,
            );
            // Still the *command's* status. A hook's own outcome is not the
            // command's, and `D` is answering for the command.
            semantic_mark(marks, SemanticMark::CommandDone(status));
            Some(step)
        }
        // Ctrl-D (EOF) exits with the last status, abandoning any in-progress
        // `func` — the buffered lines are dropped as the shell leaves. reedline
        // only emits this on an empty editor line, so a half-typed line is safe.
        Signal::CtrlD => Some(Step::Exit(last)),
        _ => {
            // Ctrl-C: cancel the current line (and any buffered `func` body) and
            // re-prompt, keeping the status.
            //
            // reedline has already written `B`, so without a `D` here the stream
            // leaves the input region open and the terminal reads everything up
            // to the next prompt as more of what the user typed. `D` closes it,
            // and carries *no* status: the abandoned line ran nothing, and
            // reporting `last` would badge the new line with the old command's
            // outcome.
            semantic_mark(shell.vars.interactive(), SemanticMark::CommandAbandoned);
            pending.clear();
            gate.reset();
            Some(Step::Continue(last))
        }
    }
}

/// Piped / non-interactive loop: read commands unbuffered from fd 0 so bytes
/// past a command's newline stay in the pipe/file for a child that inherits
/// stdin. A malformed (non-UTF-8) line is rejected loudly and skipped.
fn run_piped(options: &StartupOptions) -> ExitCode {
    // `ManuallyDrop` keeps us from closing fd 0 when the shell exits.
    let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut shell = Shell::new();
    shell
        .vars
        .set_invocation(options.name.clone(), options.args.clone());
    let (origin, source) = options.origin(false);
    shell.vars.set_origin(origin, source);
    let mut last = match run_startup_files(options, false, 0, &mut shell) {
        Step::Continue(code) => code,
        Step::Exit(code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
        Step::Return(value) => {
            return ExitCode::from(run_logout(options, status_of(&value), &mut shell));
        }
    };
    let mut pending = String::new();
    let mut gate = HeredocGate::default();
    // Discard a buffered input unit if any of its physical lines was invalid
    // UTF-8, while still using the parser to find the unit's end.
    let mut poisoned = false;
    let mut line = Vec::new();

    loop {
        line.clear();
        match read_line(&mut *stdin, &mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(err) => {
                note!("mesh: read error: {err}");
                return ExitCode::from(run_logout(options, 1, &mut shell));
            }
        }

        // Hold a lossy copy alive if we substitute invalid bytes below.
        let lossy;
        let text: &str = match std::str::from_utf8(&line) {
            Ok(text) => text,
            Err(_) => {
                note!("mesh: invalid UTF-8 in input");
                last = 1;
                lossy = String::from_utf8_lossy(&line).into_owned();
                if pending.is_empty() && !needs_more_input(&lossy) {
                    continue;
                }
                poisoned = true;
                &lossy
            }
        };
        pending.push_str(text);
        if gate.needs_more_input(&pending, text) {
            continue;
        }
        let full = std::mem::take(&mut pending);
        if std::mem::take(&mut poisoned) {
            // Discard the definition that contained invalid UTF-8 (error already
            // reported when the bad line was read); do not define or run it.
            continue;
        }
        match run_line(&full, last, false, &mut shell) {
            Step::Exit(code) => {
                return ExitCode::from(run_logout(options, code, &mut shell));
            }
            Step::Continue(code) => last = code,
            Step::Return(_) => unreachable!("top-level return handled in run_line"),
        }
    }
    // Report an incomplete unit at EOF; a poisoned one was already diagnosed.
    if !poisoned && !pending.trim().is_empty() {
        match run_line(&pending, last, false, &mut shell) {
            Step::Exit(code) => {
                return ExitCode::from(run_logout(options, code, &mut shell));
            }
            Step::Continue(code) => last = code,
            Step::Return(_) => unreachable!("top-level return handled in run_line"),
        }
    }
    ExitCode::from(run_logout(options, last, &mut shell))
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

/// The minimal two-glyph prompt: `mesh$` after success and `mesh!` after failure.
/// A continuation prompt fills the width of the current prompt's last line with
/// dots and a trailing space. The full status-dashboard prompt from `DESIGN.md`
/// is a later milestone.
struct MeshPrompt {
    failed: bool,
    continuation: bool,
    custom: Option<String>,
}

impl MeshPrompt {
    fn continuation_indicator(&self) -> String {
        let prompt =
            self.custom
                .as_deref()
                .unwrap_or(if self.failed { "mesh! " } else { "mesh$ " });
        let width = escape_stripped_width(prompt.rsplit('\n').next().unwrap_or_default());

        if width == 0 {
            String::new()
        } else {
            format!("{} ", ".".repeat(width - 1))
        }
    }
}

/// The printed width of `text`, with the escape sequences that draw nothing
/// discounted — the number of columns the continuation dots have to fill.
///
/// Two kinds of sequence reach a prompt, and neither is glyphs:
///
/// **CSI** (`ESC [ … final`), which is what styling emits. Every final byte is
/// skipped, not only SGR's `m`: a clear-to-end-of-line or a cursor save in a
/// prompt draws nothing either, and counting `ESC [ K` as three columns is worse
/// than treating a cursor movement as zero.
///
/// **OSC** (`ESC ] … BEL` or `… ST`), which is a prompt that sets the window
/// title — the classic `PS1` idiom, and what mesh users hand-roll until
/// `DESIGN.md`'s title item lands. Counting the payload is how a two-glyph prompt
/// grows a forty-dot continuation line.
///
/// An unterminated sequence keeps counting from the byte after the `ESC`, which is
/// what the previous SGR-only version did with anything it did not recognize.
/// Nothing better is available: where the sequence was meant to end is exactly
/// what is missing.
///
/// Cursor motion is *not* modeled: `ESC [ 2 C` advances the cursor two columns and
/// this counts it as none. Deliberate, because the number has to agree with the
/// line editor rather than with the terminal — reedline lays the line out with
/// `strip_ansi_escapes::strip(line).width()`, which is zero for every sequence
/// including that one, so dots measured against the real cursor would be aligned
/// to a column reedline does not believe the prompt ends at. The SGR-only rule was
/// wrong here too, by four columns rather than two, in the other direction.
/// Accounting for motion properly — `ESC [ G`, save and restore, what happens at
/// the right margin — is emulating a terminal.
fn escape_stripped_width(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut width = 0;
    let mut visible_start = 0;
    let mut index = 0;

    while index + 1 < bytes.len() {
        if bytes[index] != b'\x1b' {
            index += 1;
            continue;
        }
        // `ESC` and the introducer are ASCII, and every `end` lands just past an
        // ASCII byte, so both slice ends are always char boundaries.
        let end = match bytes[index + 1] {
            b'[' => control_sequence_end(bytes, index + 2),
            b']' => operating_system_command_end(bytes, index + 2),
            _ => None,
        };
        if let Some(end) = end {
            width += text[visible_start..index].width();
            visible_start = end;
            index = end;
        } else {
            index += 1;
        }
    }

    width + text[visible_start..].width()
}

/// One past the end of the CSI sequence whose parameters start at `start`, or
/// `None` when it is unterminated: parameter and intermediate bytes, then a final
/// byte in `0x40..=0x7e`.
fn control_sequence_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    while index < bytes.len() && (0x20..=0x3f).contains(&bytes[index]) {
        index += 1;
    }
    let final_byte = *bytes.get(index)?;
    (0x40..=0x7e).contains(&final_byte).then_some(index + 1)
}

/// One past the end of the OSC whose payload starts at `start`, or `None` when it
/// is unterminated. Both terminators are accepted — `BEL` is what a hand-written
/// prompt uses and `ST` is what mesh's own sequences use — so a prompt carrying
/// either measures the same.
fn operating_system_command_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            0x07 => return Some(index + 1),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some(index + 2),
            _ => index += 1,
        }
    }
    None
}

impl Prompt for MeshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<'_, str> {
        if self.continuation {
            Cow::Owned(self.continuation_indicator())
        } else if let Some(prompt) = &self.custom {
            Cow::Borrowed(prompt)
        } else if self.failed {
            Cow::Borrowed("mesh! ")
        } else {
            Cow::Borrowed("mesh$ ")
        }
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(self.continuation_indicator())
    }
    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("search: ")
    }
    fn get_prompt_color(&self) -> Color {
        Color::Reset
    }
    fn get_prompt_multiline_color(&self) -> nu_ansi_term::Color {
        nu_ansi_term::Color::Default
    }
    fn get_indicator_color(&self) -> Color {
        Color::Reset
    }
    fn get_prompt_right_color(&self) -> Color {
        Color::Reset
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArgumentRecall, CompletionState, HeredocGate, Invocation, MeshPrompt, PromptEvent,
        PromptHook, Shell, StartupOptions, Step, TITLE_LIMIT, TimestampedHistory,
        argument_completions, body_awaits_close, command_position, command_segment_words,
        command_words, completed_command, cwd_url, escape_stripped_width, eval_binary,
        expand_history_designators, expansion_word, func_definition_is_open, handle_signal,
        help_completions, history_designators, history_path_from, input_highlighter,
        interactive_keybindings, interruptible_task, last_argument, needs_more_input, open_history,
        path_completions_sync, persist_logical_history, prepare_history_path, prompt_title,
        run_line, run_prompt_hooks, run_source, running_title, title_sequence, title_text,
        variable_completions,
    };
    use crate::parser;
    use crate::vars::Value;
    use reedline::{
        EditCommand, Highlighter, History, HistoryItem, KeyModifiers, Prompt, PromptEditMode,
        Reedline, ReedlineEvent, SearchDirection, SearchQuery, Signal, SqliteBackedHistory,
    };
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temporary_history_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("mesh-repl-test-{}-{unique}", std::process::id()))
            .join(name)
    }

    #[test]
    fn last_argument_uses_mesh_word_spans() {
        assert_eq!(
            last_argument("puts first \"two words\"").as_deref(),
            Some("\"two words\"")
        );
        assert_eq!(
            last_argument("one ignored | puts '$dir'/'sub dir' >out").as_deref(),
            Some("'$dir'/'sub dir'")
        );
        assert_eq!(
            last_argument("puts old; puts key:value").as_deref(),
            Some("key:value")
        );
        assert_eq!(last_argument("puts"), None);
    }

    #[test]
    fn command_words_returns_command_word_first_then_arguments() {
        assert_eq!(
            command_words("git commit -m \"a message\""),
            Some(vec![
                "git".to_owned(),
                "commit".to_owned(),
                "-m".to_owned(),
                "\"a message\"".to_owned(),
            ])
        );
        // Word list is drawn from the last stage of the last statement.
        assert_eq!(
            command_words("ls old | grep needle"),
            Some(vec!["grep".to_owned(), "needle".to_owned(),])
        );
        assert_eq!(command_words(""), None);
    }

    #[test]
    fn history_designators_expand_against_the_previous_command() {
        let previous = Some("git commit -m msg");
        assert_eq!(
            expand_history_designators("show !^", "", previous).unwrap(),
            "show commit"
        );
        assert_eq!(
            expand_history_designators("show !$", "", previous).unwrap(),
            "show msg"
        );
        assert_eq!(
            expand_history_designators("show !*", "", previous).unwrap(),
            "show commit -m msg"
        );
        // Several designators in one line each resolve independently.
        assert_eq!(
            expand_history_designators("cp !^ !$", "", previous).unwrap(),
            "cp commit msg"
        );
    }

    #[test]
    fn history_designators_stay_literal_when_quoted_or_escaped() {
        let previous = Some("git commit -m msg");
        assert_eq!(
            expand_history_designators("puts '!$'", "", previous).unwrap(),
            "puts '!$'"
        );
        assert_eq!(
            expand_history_designators("puts \"!$\"", "", previous).unwrap(),
            "puts \"!$\""
        );
        assert_eq!(
            expand_history_designators("puts \\!$", "", previous).unwrap(),
            "puts \\!$"
        );
        // A bang that is not a supported designator is left alone.
        assert_eq!(
            expand_history_designators("test 1 != 2", "", previous).unwrap(),
            "test 1 != 2"
        );
        assert_eq!(
            expand_history_designators("puts hi!", "", previous).unwrap(),
            "puts hi!"
        );
    }

    #[test]
    fn history_designators_respect_lexer_quote_rules() {
        let previous = Some("git commit -m msg");
        // mesh single quotes escape `\'`, so the string stays open and the
        // designator inside it is literal (POSIX would close at the apostrophe).
        assert_eq!(
            expand_history_designators("puts 'can\\'t !$'", "", previous).unwrap(),
            "puts 'can\\'t !$'"
        );
        // Raw strings take no escapes; the designator inside stays literal.
        assert_eq!(
            expand_history_designators("puts r'!$'", "", previous).unwrap(),
            "puts r'!$'"
        );
        // A bare `\!` escape keeps the bang literal.
        assert_eq!(
            expand_history_designators("puts \\!*", "", previous).unwrap(),
            "puts \\!*"
        );
    }

    #[test]
    fn history_designators_carry_quote_state_across_continuation_lines() {
        let previous = Some("puts prior arg");
        // The double quote opens on a buffered `func` body line, so a designator
        // on the continuation line is inside the string and stays literal.
        assert_eq!(
            expand_history_designators("!$\"", "func f() {\nputs \"hello\n", previous).unwrap(),
            "!$\""
        );
        // Once that quote closes, a later designator on the same line expands.
        assert_eq!(
            expand_history_designators("!$\" !$", "func f() {\nputs \"hello\n", previous).unwrap(),
            "!$\" arg"
        );
    }

    #[test]
    fn history_designators_star_is_empty_when_there_are_no_arguments() {
        let previous = Some("ls");
        assert_eq!(
            expand_history_designators("puts !*", "", previous).unwrap(),
            "puts "
        );
    }

    #[test]
    fn history_designators_treat_an_argumentless_event_as_empty() {
        // An assignment is a real event with no command words: `!*` is empty,
        // `!^` / `!$` report no arguments — not a missing event.
        assert!(command_words("x = 1").is_none());
        let previous = Some("x = 1");
        assert_eq!(
            expand_history_designators("puts !*", "", previous).unwrap(),
            "puts "
        );
        assert_eq!(
            expand_history_designators("puts !$", "", previous),
            Err("!$: previous command has no arguments".to_owned())
        );
        assert_eq!(
            expand_history_designators("puts !^", "", previous),
            Err("!^: previous command has no arguments".to_owned())
        );
    }

    #[test]
    fn history_designators_error_without_a_usable_event() {
        assert_eq!(
            expand_history_designators("cd !$", "", None),
            Err("!$: no previous command".to_owned())
        );
        assert_eq!(
            expand_history_designators("cd !^", "", Some("ls")),
            Err("!^: previous command has no arguments".to_owned())
        );
        assert_eq!(
            expand_history_designators("cd !$", "", Some("ls")),
            Err("!$: previous command has no arguments".to_owned())
        );
    }

    #[test]
    fn argument_recall_remembers_the_previous_command_line() {
        let mut recall = ArgumentRecall::default();
        assert_eq!(recall.previous(), None);
        recall.remember("git status");
        recall.remember("cargo build --release");
        assert_eq!(recall.previous(), Some("cargo build --release"));
    }

    #[test]
    fn a_blank_submission_keeps_the_previous_command() {
        let mut recall = ArgumentRecall::default();
        recall.remember("mkdir foo");
        // Pressing Enter on an empty prompt is not an event; `!$` still finds foo.
        recall.remember("");
        recall.remember("   ");
        assert_eq!(recall.previous(), Some("mkdir foo"));
    }

    #[test]
    fn repeated_argument_recall_walks_back_and_preserves_user_edits() {
        let mut recall = ArgumentRecall::default();
        recall.remember("puts first");
        recall.remember("puts \"two words\"");
        let mut editor = Reedline::create();
        editor.run_edit_commands(&[EditCommand::InsertString("puts ".to_owned())]);

        recall.insert(&mut editor);
        assert_eq!(editor.current_buffer_contents(), "puts \"two words\"");
        recall.insert(&mut editor);
        assert_eq!(editor.current_buffer_contents(), "puts first");

        editor.run_edit_commands(&[EditCommand::InsertChar('!')]);
        recall.insert(&mut editor);
        assert_eq!(editor.current_buffer_contents(), "puts first!\"two words\"");
    }

    #[test]
    fn repeated_argument_recall_preserves_suffix_after_non_ascii_text() {
        let mut recall = ArgumentRecall::default();
        recall.remember("puts older");
        recall.remember("puts é");
        let mut editor = Reedline::create();
        editor.run_edit_commands(&[
            EditCommand::InsertString("puts suffix".to_owned()),
            EditCommand::MoveToPosition {
                position: 5,
                select: false,
            },
        ]);

        recall.insert(&mut editor);
        assert_eq!(editor.current_buffer_contents(), "puts ésuffix");
        recall.insert(&mut editor);
        assert_eq!(editor.current_buffer_contents(), "puts oldersuffix");
    }

    #[test]
    fn completed_command_assembles_multiline_argument_recall_input() {
        let first = Signal::Success("puts first |".into());
        assert_eq!(completed_command(&first, "", &HeredocGate::default()), None);

        let second = Signal::Success("puts followed-by-words".into());
        assert_eq!(
            completed_command(&second, "puts first |\n", &HeredocGate::default()).as_deref(),
            Some("puts first |\nputs followed-by-words")
        );
        assert_eq!(
            last_argument(
                &completed_command(&second, "puts first |\n", &HeredocGate::default()).unwrap()
            )
            .as_deref(),
            Some("followed-by-words")
        );
    }

    #[test]
    fn startup_options_select_login_and_an_alternate_rc_file() {
        let options = StartupOptions::parse(
            ["--login", "--rcfile", "/tmp/custom.mesh"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert!(options.login);
        assert!(!options.no_rc);
        assert!(options.save_history);
        assert_eq!(options.rc_file, Some(PathBuf::from("/tmp/custom.mesh")));
    }

    #[test]
    fn startup_options_take_a_script_and_stop_parsing_options_there() {
        let options = StartupOptions::parse(
            ["--login", "deploy.mesh", "--norc", "prod"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert!(options.login, "options before the script still apply");
        assert!(!options.no_rc, "--norc after the script belongs to it");
        assert_eq!(
            options.invocation,
            Invocation::Script(PathBuf::from("deploy.mesh"))
        );
        assert_eq!(options.name, "deploy.mesh");
        assert_eq!(options.args, ["--norc", "prod"]);
    }

    #[test]
    fn startup_options_route_dash_c_and_dash_s_operands_to_arguments() {
        let options =
            StartupOptions::parse(["-c", "puts hi", "a", "b"].into_iter().map(str::to_owned))
                .unwrap();
        assert_eq!(
            options.invocation,
            Invocation::Command("puts hi".to_owned())
        );
        assert_eq!(
            options.name, "mesh",
            "a command string is not a script name"
        );
        assert_eq!(options.args, ["a", "b"]);

        let options = StartupOptions::parse(["-s", "a"].into_iter().map(str::to_owned)).unwrap();
        assert_eq!(options.invocation, Invocation::Stdin);
        assert_eq!(options.args, ["a"]);

        assert_eq!(
            StartupOptions::parse(["-c"].into_iter().map(str::to_owned)),
            Err("-c requires a command string".to_owned())
        );
    }

    #[test]
    fn a_double_dash_ends_options_and_a_lone_dash_is_an_operand() {
        let options =
            StartupOptions::parse(["--", "--odd-name", "x"].into_iter().map(str::to_owned))
                .unwrap();
        assert_eq!(
            options.invocation,
            Invocation::Script(PathBuf::from("--odd-name"))
        );
        assert_eq!(options.args, ["x"]);

        let options = StartupOptions::parse(["-"].into_iter().map(str::to_owned)).unwrap();
        assert_eq!(options.invocation, Invocation::Script(PathBuf::from("-")));
    }

    #[test]
    fn startup_options_can_disable_saved_history() {
        let options =
            StartupOptions::parse(["--no-save-history"].into_iter().map(str::to_owned)).unwrap();
        assert!(!options.save_history);
    }

    #[test]
    fn history_uses_xdg_state_home_and_falls_back_for_relative_values() {
        assert_eq!(
            history_path_from(Some("/state".into()), Some("/home/user".into())),
            Some(PathBuf::from("/state/mesh/history.sqlite3"))
        );
        assert_eq!(
            history_path_from(Some("relative".into()), Some("/home/user".into())),
            Some(PathBuf::from(
                "/home/user/.local/state/mesh/history.sqlite3"
            ))
        );
    }

    #[test]
    fn history_path_is_owner_only() {
        let path = temporary_history_path("state/mesh/history.sqlite3");
        prepare_history_path(&path).unwrap();

        let directory_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        let file_mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(directory_mode & 0o777, 0o700);
        assert_eq!(file_mode & 0o777, 0o600);

        fs::remove_dir_all(path.ancestors().nth(3).unwrap()).unwrap();
    }

    #[test]
    fn history_recall_excludes_commands_started_by_newer_peer_sessions() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let peer_session = Reedline::create_history_session_id();
        let mut peer = TimestampedHistory(
            SqliteBackedHistory::with_file(
                path.clone(),
                peer_session,
                Some(SystemTime::now().into()),
            )
            .unwrap(),
        );
        std::thread::sleep(Duration::from_millis(2));
        let current_session = Reedline::create_history_session_id();
        let current = SqliteBackedHistory::with_file(
            path.clone(),
            current_session,
            Some(SystemTime::now().into()),
        )
        .unwrap();

        let mut item = HistoryItem::from_command_line("peer secret");
        item.session_id = peer_session;
        peer.save(item).unwrap();

        let mut recall = ArgumentRecall::default();
        recall.load(&current, current_session);
        assert!(recall.arguments.is_empty());

        drop(current);
        drop(peer);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_recall_reassembles_persisted_multiline_commands() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let saved_session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(
                path.clone(),
                saved_session,
                Some(SystemTime::now().into()),
            )
            .unwrap(),
        );
        for line in ["puts public", "func f() {", "puts secret", "}"] {
            let mut item = HistoryItem::from_command_line(line);
            item.session_id = saved_session;
            saved.save(item).unwrap();
        }
        drop(saved);

        std::thread::sleep(Duration::from_millis(2));
        let current_session = Reedline::create_history_session_id();
        let current = SqliteBackedHistory::with_file(
            path.clone(),
            current_session,
            Some(SystemTime::now().into()),
        )
        .unwrap();
        let mut recall = ArgumentRecall::default();
        recall.load(&current, current_session);

        assert_eq!(recall.arguments, ["public"]);

        drop(current);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_recall_reassembles_interleaved_sessions_independently() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let first_session = Reedline::create_history_session_id();
        let second_session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(
                path.clone(),
                first_session,
                Some(SystemTime::now().into()),
            )
            .unwrap(),
        );
        for (session, line) in [
            (first_session, "func f() {"),
            (second_session, "puts public"),
            (first_session, "puts secret"),
            (first_session, "}"),
        ] {
            let mut item = HistoryItem::from_command_line(line);
            item.session_id = session;
            saved.save(item).unwrap();
        }
        drop(saved);

        let current_session = Reedline::create_history_session_id();
        let current = SqliteBackedHistory::with_file(
            path.clone(),
            current_session,
            Some(SystemTime::now().into()),
        )
        .unwrap();
        let mut recall = ArgumentRecall::default();
        recall.load(&current, current_session);

        assert_eq!(recall.arguments, ["public"]);
        drop(current);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_recall_reloads_persisted_logical_multiline_commands() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let saved_session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(
                path.clone(),
                saved_session,
                Some(SystemTime::now().into()),
            )
            .unwrap(),
        );
        let mut pending = String::new();
        for line in ["func f() {", "puts secret"] {
            let mut item = HistoryItem::from_command_line(line);
            item.session_id = saved_session;
            saved.save(item).unwrap();
            pending.push_str(line);
            pending.push('\n');
        }
        let mut item = HistoryItem::from_command_line("}");
        item.session_id = saved_session;
        saved.save(item).unwrap();
        persist_logical_history(
            &mut saved,
            saved_session,
            &Signal::Success("}".into()),
            &pending,
            &HeredocGate::default(),
            3,
            false,
        )
        .unwrap();
        drop(saved);

        let current_session = Reedline::create_history_session_id();
        let current = SqliteBackedHistory::with_file(
            path.clone(),
            current_session,
            Some(SystemTime::now().into()),
        )
        .unwrap();
        let mut recall = ArgumentRecall::default();
        recall.load(&current, current_session);

        assert!(recall.arguments.is_empty());
        drop(current);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_recall_reloads_multiline_command_arguments() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let saved_session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(
                path.clone(),
                saved_session,
                Some(SystemTime::now().into()),
            )
            .unwrap(),
        );
        for line in ["puts \"first", "followed by last\""] {
            let mut item = HistoryItem::from_command_line(line);
            item.session_id = saved_session;
            saved.save(item).unwrap();
        }
        persist_logical_history(
            &mut saved,
            saved_session,
            &Signal::Success("followed by last\"".into()),
            "puts \"first\n",
            &HeredocGate::default(),
            2,
            false,
        )
        .unwrap();
        drop(saved);

        let current_session = Reedline::create_history_session_id();
        let current = SqliteBackedHistory::with_file(
            path.clone(),
            current_session,
            Some(SystemTime::now().into()),
        )
        .unwrap();
        let mut recall = ArgumentRecall::default();
        recall.load(&current, current_session);

        assert_eq!(recall.arguments, ["\"first\nfollowed by last\""]);
        drop(current);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_recall_preserves_boundary_after_cancel_and_reload() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let saved_session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(
                path.clone(),
                saved_session,
                Some(SystemTime::now().into()),
            )
            .unwrap(),
        );
        let mut item = HistoryItem::from_command_line("func f() {");
        item.session_id = saved_session;
        saved.save(item).unwrap();
        persist_logical_history(
            &mut saved,
            saved_session,
            &Signal::CtrlC,
            "func f() {\n",
            &HeredocGate::default(),
            1,
            false,
        )
        .unwrap();
        let mut item = HistoryItem::from_command_line("puts public");
        item.session_id = saved_session;
        saved.save(item).unwrap();
        drop(saved);

        let current_session = Reedline::create_history_session_id();
        let current = SqliteBackedHistory::with_file(
            path.clone(),
            current_session,
            Some(SystemTime::now().into()),
        )
        .unwrap();
        let mut recall = ArgumentRecall::default();
        recall.load(&current, current_session);

        assert_eq!(recall.arguments, ["public"]);
        drop(current);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn logical_history_counts_saved_submissions_not_pending_lines() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let saved_session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(
                path.clone(),
                saved_session,
                Some(SystemTime::now().into()),
            )
            .unwrap(),
        );
        for line in ["puts public", "func f() {", "}"] {
            let mut item = HistoryItem::from_command_line(line);
            item.session_id = saved_session;
            saved.save(item).unwrap();
        }
        persist_logical_history(
            &mut saved,
            saved_session,
            &Signal::Success("}".into()),
            "func f() {\n\n",
            &HeredocGate::default(),
            2,
            false,
        )
        .unwrap();
        drop(saved);

        let current_session = Reedline::create_history_session_id();
        let current = SqliteBackedHistory::with_file(
            path.clone(),
            current_session,
            Some(SystemTime::now().into()),
        )
        .unwrap();
        let mut recall = ArgumentRecall::default();
        recall.load(&current, current_session);

        assert_eq!(recall.arguments, ["public"]);
        drop(current);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_expansion_replaces_the_raw_single_line_row() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(path.clone(), session, Some(SystemTime::now().into()))
                .unwrap(),
        );
        // reedline stores the raw line the moment the user submits it.
        let mut item = HistoryItem::from_command_line("cd !$");
        item.session_id = session;
        saved.save(item).unwrap();
        // History expansion ran `cd foo`, so the stored row must become that.
        persist_logical_history(
            &mut saved,
            session,
            &Signal::Success("cd foo".into()),
            "",
            &HeredocGate::default(),
            1,
            true,
        )
        .unwrap();

        let commands: Vec<_> = saved
            .search(SearchQuery::everything(SearchDirection::Backward, session))
            .unwrap()
            .into_iter()
            .map(|entry| entry.command_line)
            .collect();
        assert_eq!(commands, ["cd foo"]);
        drop(saved);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_expansion_to_an_empty_line_leaves_no_row() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(path.clone(), session, Some(SystemTime::now().into()))
                .unwrap(),
        );
        // reedline stored the raw ` !* `; expanding it against an argumentless
        // event yields a whitespace-only line that never runs.
        let mut item = HistoryItem::from_command_line(" !* ");
        item.session_id = session;
        saved.save(item).unwrap();
        persist_logical_history(
            &mut saved,
            session,
            &Signal::Success("  ".to_owned()),
            "",
            &HeredocGate::default(),
            1,
            true,
        )
        .unwrap();

        let entries = saved
            .search(SearchQuery::everything(SearchDirection::Backward, session))
            .unwrap();
        assert!(entries.is_empty());
        drop(saved);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn unexpanded_single_line_history_is_left_untouched() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let session = Reedline::create_history_session_id();
        let mut saved = TimestampedHistory(
            SqliteBackedHistory::with_file(path.clone(), session, Some(SystemTime::now().into()))
                .unwrap(),
        );
        let mut item = HistoryItem::from_command_line("ls -l");
        item.session_id = session;
        let id = saved.save(item).unwrap().id;
        // A command with no expansion keeps reedline's original row (and its id).
        persist_logical_history(
            &mut saved,
            session,
            &Signal::Success("ls -l".into()),
            "",
            &HeredocGate::default(),
            1,
            false,
        )
        .unwrap();

        let entries = saved
            .search(SearchQuery::everything(SearchDirection::Backward, session))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command_line, "ls -l");
        assert_eq!(entries[0].id, id);
        drop(saved);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_recall_retains_rows_without_timestamps() {
        let path = temporary_history_path("history.sqlite3");
        prepare_history_path(&path).unwrap();
        let mut legacy = SqliteBackedHistory::with_file(path.clone(), None, None).unwrap();
        legacy
            .save(HistoryItem::from_command_line("legacy command"))
            .unwrap();
        drop(legacy);

        let session = Reedline::create_history_session_id();
        let history = open_history(path.clone(), session, Some(SystemTime::now().into())).unwrap();
        let entries = history
            .search(reedline::SearchQuery::everything(
                reedline::SearchDirection::Backward,
                session,
            ))
            .unwrap();
        assert_eq!(entries[0].command_line, "legacy command");

        drop(history);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_options_reject_a_missing_rc_file_argument() {
        assert_eq!(
            StartupOptions::parse(["--rcfile"].into_iter().map(str::to_owned)),
            Err("--rcfile requires a file path".to_owned())
        );
    }

    #[test]
    fn completion_recognizes_command_positions() {
        assert!(command_position(""));
        assert!(command_position("puts x | "));
        assert!(command_position("false && "));
        assert!(!command_position("puts "));
    }

    #[test]
    fn completion_offers_variables_and_nested_map_keys() {
        let variables = vec![
            ("name".into(), Value::String("mesh".into())),
            (
                "config".into(),
                Value::Map(vec![(
                    "user".into(),
                    Value::Map(vec![("name".into(), Value::String("Ada".into()))]),
                )]),
            ),
        ];
        assert_eq!(variable_completions("$na", &variables), ["$name"]);
        assert_eq!(
            variable_completions("$config.user.n", &variables),
            ["$config.user.name"]
        );
        assert_eq!(variable_completions("$nm", &variables), ["$name"]);
        assert!(variable_completions("$NM", &variables).is_empty());
        assert_eq!(
            variable_completions("$config.user.nm", &variables),
            ["$config.user.name"]
        );
        assert!(variable_completions("$CONFIG.USER.NM", &variables).is_empty());
    }

    #[test]
    fn completion_prefers_exact_case_for_variable_and_map_paths() {
        let variables = vec![
            (
                "Config".into(),
                Value::Map(vec![(
                    "USER".into(),
                    Value::Map(vec![("NAME".into(), Value::String("wrong".into()))]),
                )]),
            ),
            (
                "config".into(),
                Value::Map(vec![
                    (
                        "user".into(),
                        Value::Map(vec![("nickname".into(), Value::String("lower".into()))]),
                    ),
                    (
                        "USER".into(),
                        Value::Map(vec![("name".into(), Value::String("upper".into()))]),
                    ),
                ]),
            ),
        ];

        assert_eq!(
            variable_completions("$config.USER.n", &variables),
            ["$config.USER.name"]
        );
        assert!(variable_completions("$config.user.N", &variables).is_empty());
    }

    #[test]
    fn completion_passes_subcommands_to_help_and_filters_option_prefixes() {
        assert_eq!(
            command_segment_words("echo x | cargo bu"),
            Some(vec!["cargo".into(), "bu".into()])
        );
        assert_eq!(
            command_segment_words("false && cargo --v"),
            Some(vec!["cargo".into(), "--v".into()])
        );
        assert_eq!(
            command_segment_words("puts 'not | a command'; cargo bu"),
            Some(vec!["cargo".into(), "bu".into()])
        );
        assert_eq!(
            help_completions(
                "Commands:\n  soft  reset softly\n  hard  reset hard\n\nOptions:\n  -h, --help  help\n  --quiet=<WHEN> quiet\n",
                "--h"
            ),
            ["--help"]
        );
        assert_eq!(
            help_completions("Commands:\n  soft  reset softly\n  hard  reset hard\n", ""),
            ["hard", "soft"]
        );
        let state = CompletionState {
            help: [(
                "cargo".into(),
                "Commands:\n  build  compile\n  check  analyze\n".into(),
            )]
            .into(),
            ..CompletionState::default()
        };
        assert_eq!(
            argument_completions(&state, &["cargo".into(), "bu".into()], "bu"),
            ["build"]
        );
        assert_eq!(
            argument_completions(&state, &["cargo".into(), "bl".into()], "bl"),
            ["build"]
        );
        let state = CompletionState {
            help: [(
                "tool".into(),
                "Commands:\n  commit  record\n  checkout  switch\n\nOptions:\n  --debug  debug\n"
                    .into(),
            )]
            .into(),
            ..CompletionState::default()
        };
        let completions = argument_completions(&state, &["tool".into(), "co".into()], "co");
        assert_eq!(&completions[..2], ["commit", "checkout"]);
        assert_eq!(
            argument_completions(&state, &["tool".into(), "bu".into()], "bu"),
            Vec::<String>::new()
        );
        assert!(
            argument_completions(
                &state,
                &["cargo".into(), "definitely-missing".into()],
                "definitely-missing"
            )
            .is_empty()
        );
    }

    #[test]
    fn command_help_does_not_hide_path_completions() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("mesh-path-help-{}", std::process::id()));
        let child = dir.join("existing");
        fs::create_dir_all(&child).unwrap();
        let prefix = dir.join("ex").to_string_lossy().into_owned();
        let state = CompletionState {
            help: [("cat".into(), "Options:\n  --number  number lines\n".into())].into(),
            ..CompletionState::default()
        };

        assert_eq!(
            argument_completions(&state, &["cat".into(), prefix.clone()], &prefix),
            [format!("{}/", child.display())]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tab_opens_and_advances_the_completion_menu() {
        assert_eq!(
            interactive_keybindings().find_binding(KeyModifiers::NONE, reedline::KeyCode::Tab),
            Some(ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu(super::COMPLETION_MENU.to_owned()),
                ReedlineEvent::MenuNext,
            ]))
        );
    }

    #[test]
    fn vim_usage_completes_positional_files() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("mesh-vim-help-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cargo_lock = dir.join("Cargo.lock");
        let cargo_toml = dir.join("Cargo.toml");
        fs::write(&cargo_lock, "").unwrap();
        fs::write(&cargo_toml, "").unwrap();
        let prefix = format!("{}/C", dir.display());
        let state = CompletionState {
            help: [(
                "vi".into(),
                "VIM - Vi IMproved 9.2\n\nUsage: vim [arguments] [file ..]       edit specified file(s)\n"
                    .into(),
            )]
            .into(),
            ..CompletionState::default()
        };

        assert_eq!(
            argument_completions(&state, &["vi".into(), prefix.clone()], &prefix),
            [
                cargo_lock.to_string_lossy().into_owned(),
                cargo_toml.to_string_lossy().into_owned()
            ]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn filesystem_completion_uses_exact_case_then_smart_case() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("mesh-case-help-{}", std::process::id()));
        fs::create_dir_all(dir.join("Foo")).unwrap();
        fs::create_dir_all(dir.join("foo")).unwrap();
        fs::create_dir_all(dir.join("football")).unwrap();

        let lowercase = format!("{}/foo", dir.display());
        assert_eq!(
            path_completions_sync(&lowercase, true),
            [
                format!("{}/foo/", dir.display()),
                format!("{}/football/", dir.display()),
                format!("{}/Foo/", dir.display())
            ]
        );
        let uppercase = format!("{}/FOO", dir.display());
        assert!(path_completions_sync(&uppercase, true).is_empty());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn typed_argument_completion_filters_files_directories_and_enums() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("mesh-typed-help-{}", std::process::id()));
        let child = dir.join("folder");
        let file = dir.join("file.txt");
        fs::create_dir_all(&child).unwrap();
        fs::write(&file, "").unwrap();
        let prefix = format!("{}/f", dir.display());
        let state = CompletionState {
            help: [(
                "tool".into(),
                "Options:\n  --file <FILE> input\n  --directory <DIR> root\n  --color <auto|always|never> mode\n"
                    .into(),
            )]
            .into(),
            ..CompletionState::default()
        };

        assert_eq!(
            argument_completions(
                &state,
                &["tool".into(), "--file".into(), prefix.clone()],
                &prefix
            ),
            [
                file.to_string_lossy().into_owned(),
                format!("{}/", child.display())
            ]
        );
        assert_eq!(
            argument_completions(
                &state,
                &["tool".into(), "--directory".into(), prefix.clone()],
                &prefix
            ),
            [format!("{}/", child.display())]
        );
        assert_eq!(
            argument_completions(&state, &["tool".into(), "--color=a".into()], "--color=a"),
            ["--color=auto", "--color=always"]
        );
        assert_eq!(
            argument_completions(&state, &["tool".into(), "--color=nv".into()], "--color=nv"),
            ["--color=never"]
        );
        assert!(
            argument_completions(&state, &["tool".into(), "--color=NV".into()], "--color=NV")
                .is_empty()
        );
        let fuzzy_prefix = format!("{}/ft", dir.display());
        assert_eq!(
            argument_completions(
                &state,
                &["tool".into(), "--file".into(), fuzzy_prefix.clone()],
                &fuzzy_prefix
            ),
            [file.to_string_lossy().into_owned()]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn separated_typed_completion_probes_the_option_context_first() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("mesh-separated-typed-help-{}", std::process::id()));
        let command = dir.join("tool");
        let calls = dir.join("calls");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &command,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$*\" = 'build --help' ]; then\n  echo '  --color <auto|always|never>  mode'\nfi\n",
                calls.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        let command = command.to_string_lossy().into_owned();
        let state = CompletionState::default();

        assert_eq!(
            argument_completions(
                &state,
                &[command, "build".into(), "--color".into(), "a".into()],
                "a"
            ),
            ["auto", "always"]
        );
        assert_eq!(fs::read_to_string(calls).unwrap(), "build --help\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn filesystem_completion_work_is_time_bounded() {
        use std::thread;
        use std::time::{Duration, Instant};

        let started = Instant::now();
        assert_eq!(
            interruptible_task(Duration::from_millis(10), || {
                thread::sleep(Duration::from_secs(1));
                1
            }),
            None
        );
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn custom_prompt_replaces_the_status_glyph_and_can_be_reset() {
        let mut shell = Shell::new();
        assert_eq!(
            run_line("prompt 'ready> '", 0, false, &mut shell),
            Step::Continue(0)
        );
        let prompt = MeshPrompt {
            failed: true,
            continuation: false,
            custom: shell.prompt.text.clone(),
        };
        assert_eq!(
            prompt.render_prompt_indicator(PromptEditMode::Default),
            "ready> "
        );
        assert_eq!(
            run_line("prompt --reset", 0, false, &mut shell),
            Step::Continue(0)
        );
        assert!(shell.prompt.text.is_none());
    }

    #[test]
    fn continuation_prompts_match_the_current_prompt_last_line() {
        let default_prompt = MeshPrompt {
            failed: false,
            continuation: true,
            custom: None,
        };

        assert_eq!(
            default_prompt.render_prompt_indicator(PromptEditMode::Default),
            "..... "
        );
        assert_eq!(default_prompt.render_prompt_multiline_indicator(), "..... ");

        let custom_prompt = MeshPrompt {
            failed: false,
            continuation: true,
            custom: Some("heading\nλ> ".into()),
        };

        assert_eq!(
            custom_prompt.render_prompt_indicator(PromptEditMode::Default),
            ".. "
        );
        assert_eq!(custom_prompt.render_prompt_multiline_indicator(), ".. ");

        let styled_prompt = MeshPrompt {
            failed: false,
            continuation: true,
            custom: Some("\x1b[31mλ> \x1b[0m".into()),
        };

        assert_eq!(
            styled_prompt.render_prompt_indicator(PromptEditMode::Default),
            ".. "
        );
        assert_eq!(styled_prompt.render_prompt_multiline_indicator(), ".. ");

        let styling_only_prompt = MeshPrompt {
            failed: false,
            continuation: true,
            custom: Some("\x1b[0m".into()),
        };

        assert_eq!(
            styling_only_prompt.render_prompt_indicator(PromptEditMode::Default),
            ""
        );
        assert_eq!(styling_only_prompt.render_prompt_multiline_indicator(), "");

        // A prompt that sets the window title on its way past — the `PS1` idiom,
        // and what mesh users reach for until the title item in `DESIGN.md`
        // lands. The dots answer for `λ> `, not for the title.
        let titling_prompt = MeshPrompt {
            failed: false,
            continuation: true,
            custom: Some("\x1b]0;mesh\x07λ> ".into()),
        };

        assert_eq!(
            titling_prompt.render_prompt_indicator(PromptEditMode::Default),
            ".. "
        );
        assert_eq!(titling_prompt.render_prompt_multiline_indicator(), ".. ");
    }

    #[test]
    fn a_titling_prompt_measures_the_same_through_the_input_path() {
        // The unit tests above hand `escape_stripped_width` bytes directly. This
        // one goes through what a user actually types, because the two are not the
        // same reach: mesh's escape set is explicit, so a terminator it cannot
        // spell is a syntax error long before the width scan is consulted, and a
        // scan that handles the bytes would never be asked. Raised in review on
        // #238, when `\a` was that case.
        //
        // All three spellings of a terminator mesh now has: `ST` (`\e\\`, what
        // mesh's own OSC 7 and OSC 133 sequences use), and `BEL` as both `\a` (the
        // idiom, added for exactly this) and `\u{7}` (what spelled it before).
        for prompt_command in [
            r#"prompt "\e]0;mesh\e\\x> ""#,
            r#"prompt "\e]0;mesh\ax> ""#,
            r#"prompt "\e]0;mesh\u{7}x> ""#,
        ] {
            let mut shell = Shell::new();
            assert_eq!(
                run_line(prompt_command, 0, false, &mut shell),
                Step::Continue(0),
                "{prompt_command} did not run"
            );
            let prompt = MeshPrompt {
                failed: false,
                continuation: true,
                custom: shell.prompt.text.clone(),
            };
            assert_eq!(
                prompt.render_prompt_indicator(PromptEditMode::Default),
                ".. ",
                "{prompt_command} measured its title"
            );
        }
    }

    #[test]
    fn the_title_sequence_follows_the_terminal() {
        let osc = |term: &str| title_sequence(Some(OsStr::new(term)), "t");
        // OSC 0 sets the window and icon name together, and is what everything
        // outside a multiplexer takes.
        assert_eq!(osc("xterm-256color").as_deref(), Some("\x1b]0;t\x07"));
        assert_eq!(osc("alacritty").as_deref(), Some("\x1b]0;t\x07"));
        // A variant comes along with its family: kitty names itself after xterm,
        // and `st-256color` and `stterm-256color` are both suckless `st`.
        assert_eq!(osc("xterm-kitty").as_deref(), Some("\x1b]0;t\x07"));
        assert_eq!(osc("foot-extra").as_deref(), Some("\x1b]0;t\x07"));
        assert_eq!(osc("st-256color").as_deref(), Some("\x1b]0;t\x07"));
        assert_eq!(osc("stterm-256color").as_deref(), Some("\x1b]0;t\x07"));
        // Inside screen or tmux the sequence names the *pane*. Sent OSC 0, tmux
        // forwards it to the outer terminal and the pane's name never changes.
        assert_eq!(
            osc("screen.xterm-256color").as_deref(),
            Some("\x1bkt\x1b\\")
        );
        assert_eq!(osc("tmux-256color").as_deref(), Some("\x1bkt\x1b\\"));
    }

    #[test]
    fn a_terminal_with_no_title_is_sent_nothing() {
        let osc = |term: Option<&str>| title_sequence(term.map(OsStr::new), "t");
        // Not caution about a no-op. The Linux console reads `ESC ]` as the start
        // of a palette sequence and abandons it at the first non-hex byte, so the
        // rest of the title prints and the `BEL` beeps; under `dumb` — an Emacs
        // shell buffer — the whole sequence prints.
        assert_eq!(osc(Some("linux")), None);
        assert_eq!(osc(Some("dumb")), None);
        assert_eq!(osc(Some("cons25")), None);
        assert_eq!(osc(Some("vt100")), None);
        // `ansi` and `sun` are the two that fell through this when it was a list of
        // exclusions rather than a list of what works — raised in review on #238.
        // Neither has a title, and both would have printed one.
        assert_eq!(osc(Some("ansi")), None);
        assert_eq!(osc(Some("sun")), None);
        // `st52` is an Atari VT52, not suckless `st` — a family name has to end at
        // a `-`, a `.`, or the end of the string, or the allowlist leaks exactly
        // the terminals it exists to keep out. Raised in review on #238.
        assert_eq!(osc(Some("st52")), None);
        // An unknown terminal is silent rather than assumed to be xterm-like: no
        // title is a thing nobody has to debug, and a printed one is not.
        assert_eq!(osc(Some("wy60")), None);
        assert_eq!(osc(Some("a-terminal-nobody-has-written-yet")), None);
        assert_eq!(osc(Some("")), None);
        assert_eq!(osc(None), None);
    }

    #[test]
    fn a_title_cannot_carry_an_escape_sequence_of_its_own() {
        // Both things mesh titles carry text it did not choose. A file named with
        // an `ESC` — `touch $'\e]0;x\a'`, then `cd` into that directory — would
        // otherwise close mesh's sequence early and open one of its own.
        // The `]0;evil` text survives as text, which is the point: without an
        // `ESC` in front of it, it cannot begin anything.
        assert_eq!(
            title_sequence(Some(OsStr::new("xterm")), "a\x1b]0;evil\x07b").as_deref(),
            Some("\x1b]0;a ]0;evil b\x07")
        );
        // A pasted two-line command keeps its word boundary, which deleting the
        // newline instead of replacing it would lose.
        assert_eq!(title_text("puts one\nputs two"), "puts one puts two");
        // Nor can it end the multiplexer's sequence early: `ST` needs the same
        // `ESC`, so a bare backslash is left as the character it is.
        assert_eq!(
            title_sequence(Some(OsStr::new("screen")), "a\x1b\\b").as_deref(),
            Some("\x1bka \\b\x1b\\")
        );
    }

    #[test]
    fn a_long_title_is_cut_by_the_shell_not_the_title_bar() {
        // A terminal elides the *end*, which is where the identifying part of a
        // long command line would be. 96 characters, then an ellipsis.
        let long = "x".repeat(200);
        let title = title_text(&long);
        assert_eq!(title.chars().count(), TITLE_LIMIT + 1);
        assert!(title.ends_with('…'), "{title}");
        // The cut counts characters, not bytes, so a multi-byte title is not
        // truncated early or split down the middle of one.
        let wide = "日".repeat(200);
        let wide_title = title_text(&wide);
        assert_eq!(wide_title.chars().count(), TITLE_LIMIT + 1);
        assert!(wide_title.starts_with("日日"), "{wide_title}");
        // A title that fits keeps its exact text, ellipsis and all.
        assert_eq!(title_text("puts hi"), "puts hi");
        assert_eq!(
            title_text(&"y".repeat(TITLE_LIMIT)),
            "y".repeat(TITLE_LIMIT)
        );
    }

    #[test]
    fn the_prompt_title_says_who_and_where() {
        let home = PathBuf::from("/home/mikel");
        assert_eq!(
            prompt_title(
                "mikel",
                b"vm",
                &PathBuf::from("/home/mikel/src"),
                Some(&home)
            ),
            "mikel@vm: ~/src"
        );
        // `$HOME` itself, rather than `~/`.
        assert_eq!(
            prompt_title("mikel", b"vm", &home, Some(&home)),
            "mikel@vm: ~"
        );
        // Whole components only: `/home/mikelward` is not `~ward`.
        assert_eq!(
            prompt_title(
                "mikel",
                b"vm",
                &PathBuf::from("/home/mikelward"),
                Some(&home)
            ),
            "mikel@vm: /home/mikelward"
        );
        // A missing piece drops out with its separator rather than leaving `@` or
        // `: ` hanging — a title is read at a glance, and `@vm: ~` reads as a
        // truncation while `vm: ~` reads as a fact.
        assert_eq!(
            prompt_title("", b"vm", &PathBuf::from("/tmp"), None),
            "vm: /tmp"
        );
        assert_eq!(
            prompt_title("mikel", b"", &PathBuf::from("/tmp"), None),
            "mikel: /tmp"
        );
        assert_eq!(prompt_title("", b"", &PathBuf::from("/tmp"), None), "/tmp");
        // `$HOME=/` would make every path `~`-prefixed, so it is left alone.
        assert_eq!(
            prompt_title("", b"", &PathBuf::from("/tmp"), Some(Path::new("/"))),
            "/tmp"
        );
    }

    #[test]
    fn the_running_title_is_the_command() {
        assert_eq!(running_title("  cargo test  "), "cargo test");
        // A multi-line command keeps both lines; `title_text` is what flattens
        // them, so the two responsibilities stay separable.
        assert_eq!(running_title("puts one\nputs two"), "puts one\nputs two");
    }

    #[test]
    fn prompt_width_discounts_an_osc_sequence() {
        // Either terminator: `BEL` is what a hand-written prompt uses, `ST` what
        // mesh's own sequences use, and a prompt carrying either measures the same.
        assert_eq!(escape_stripped_width("\x1b]0;title\x07mesh$ "), 6);
        assert_eq!(escape_stripped_width("\x1b]0;title\x1b\\mesh$ "), 6);
        // A payload holding what would otherwise end a *CSI* is still payload.
        assert_eq!(escape_stripped_width("\x1b]2;a;b[0m\x07>"), 1);
        // And an OSC on its own leaves nothing to fill, rather than 9 columns of it.
        assert_eq!(escape_stripped_width("\x1b]0;title\x07"), 0);
    }

    #[test]
    fn prompt_width_discounts_a_csi_that_is_not_styling() {
        // The old rule accepted only SGR's `m`, so each of these was counted as
        // visible: a clear, a cursor hide, a cursor save, a cursor movement.
        assert_eq!(escape_stripped_width("\x1b[Kmesh$ "), 6);
        assert_eq!(escape_stripped_width("\x1b[?25lmesh$ "), 6);
        assert_eq!(escape_stripped_width("\x1b[smesh$ \x1b[u"), 6);
        // A cursor movement counts as nothing, not as the columns it moves —
        // raised in review on #238. This agrees with how reedline measures the
        // same line for layout; see the note on `escape_stripped_width`.
        assert_eq!(escape_stripped_width("\x1b[2Cmesh$ "), 6);
        // Styling itself is unchanged, including a multi-parameter form.
        assert_eq!(escape_stripped_width("\x1b[1;31mmesh$ \x1b[0m"), 6);
    }

    #[test]
    fn prompt_width_measures_what_is_left_after_the_escapes() {
        // Wide and combining characters still go through the width table rather
        // than being counted per byte or per `char`.
        assert_eq!(escape_stripped_width("\x1b[32m日本\x1b[0m> "), 6);
        // An unterminated sequence keeps its bytes, `ESC` included — where it was
        // meant to end is exactly what is missing, so there is nothing better to do
        // than count on, which is what the SGR-only version did too.
        assert_eq!(escape_stripped_width("\x1b]0;title"), 9);
        assert_eq!(escape_stripped_width("\x1b[1;31"), 6);
        assert_eq!(escape_stripped_width("mesh$ "), 6);
        assert_eq!(escape_stripped_width(""), 0);
    }

    #[test]
    fn prompt_uses_terminal_default_colors() {
        let prompt = MeshPrompt {
            failed: false,
            continuation: false,
            custom: None,
        };

        assert_eq!(prompt.get_prompt_color(), reedline::Color::Reset);
        assert_eq!(
            prompt.get_prompt_multiline_color(),
            nu_ansi_term::Color::Default
        );
        assert_eq!(prompt.get_indicator_color(), reedline::Color::Reset);
        assert_eq!(prompt.get_prompt_right_color(), reedline::Color::Reset);
    }

    #[test]
    fn interactive_input_is_bold_without_a_foreground_color() {
        let highlighted = input_highlighter().highlight("puts hello", 10);

        assert_eq!(highlighted.buffer.len(), 1);
        assert_eq!(highlighted.buffer[0].0, nu_ansi_term::Style::new().bold());
        assert_eq!(highlighted.buffer[0].1, "puts hello");
    }

    #[test]
    fn named_prompt_hooks_replace_in_place_and_run_before_the_prompt() {
        let marker = std::env::temp_dir().join(format!("mesh-prompt-hook-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut shell = Shell::new();
        let script = format!(
            "func first() {{ false }}\nfunc second() {{ touch '{}' }}\nprompt-hook refresh first\nprompt-hook refresh second\n",
            marker.display()
        );
        assert_eq!(run_line(&script, 0, false, &mut shell), Step::Continue(0));
        assert_eq!(
            shell.prompt.hooks,
            vec![PromptHook {
                event: PromptEvent::PrePrompt,
                name: "refresh".into(),
                function: "second".into(),
            }]
        );
        run_prompt_hooks(PromptEvent::PrePrompt, Vec::new(), &mut shell);
        assert!(marker.exists());
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn a_lifecycle_hook_does_not_replace_the_users_status() {
        // The loop already discards a hook's `Step`, keeping the user's `last`.
        // What the hook *runs* has to be discarded the same way, or a `preprompt`
        // that merely prints would report 0 where the user's failed command is
        // still what the prompt and the eventual exit code reflect.
        let mut shell = Shell::new();
        assert_eq!(
            run_line("func h() { puts '' }\n", 0, false, &mut shell),
            Step::Continue(0)
        );
        assert_eq!(
            run_line("prompt-hook preprompt p h\n", 0, false, &mut shell),
            Step::Continue(0)
        );
        assert_eq!(run_line("false\n", 0, false, &mut shell), Step::Continue(1));
        assert_eq!(shell.vars.status(), 1);
        run_prompt_hooks(PromptEvent::PrePrompt, Vec::new(), &mut shell);
        assert_eq!(shell.vars.status(), 1, "a hook replaced the user's status");
    }

    #[test]
    fn input_the_parser_rejects_publishes_its_status() {
        // A parse failure never reaches `run_recorded`, so without publishing it
        // here the shell would carry 2 to the next command while `$sh.status`
        // still reported whatever ran before.
        let mut shell = Shell::new();
        assert_eq!(run_line("true\n", 0, false, &mut shell), Step::Continue(0));
        assert_eq!(
            run_line("nope (\n", 0, false, &mut shell),
            Step::Continue(2)
        );
        assert_eq!(shell.vars.status(), 2);
    }

    #[test]
    fn command_hooks_receive_command_status_and_elapsed_arguments() {
        let mut shell = Shell::new();
        assert_eq!(
            run_line(
                "func before(cmd) { puts $cmd }\nfunc after(cmd, status, elapsed) { puts $cmd $status $elapsed }\nprompt-hook preexec log before\nprompt-hook postexec log after",
                0,
                false,
                &mut shell,
            ),
            Step::Continue(0)
        );
        let mut pending = String::new();
        let mut gate = HeredocGate::default();
        assert_eq!(
            handle_signal(
                Signal::Success("true".into()),
                0,
                &mut shell,
                &mut pending,
                &mut gate
            ),
            Some(Step::Continue(0))
        );
        assert_eq!(shell.prompt.hooks.len(), 2);
    }

    #[test]
    fn a_hook_binds_a_flag_like_command_line_positionally() {
        // A prompt hook receives the command line as a computed positional value.
        // When that line is the `--` terminator or looks like a `--flag`, the hook
        // call must still bind it positionally (flags disabled) so a hook declared
        // `func hook(cmd)` runs, rather than failing with a terminator/unknown-flag
        // or arity error.
        let mut shell = Shell::new();
        run_line("func hook(cmd) { puts $cmd }", 0, false, &mut shell);
        for line in ["--foo", "--", "--tag=v1"] {
            assert_eq!(
                super::call_func(
                    "hook",
                    vec![(Value::String(line.into()), false)],
                    false,
                    &mut shell,
                ),
                Step::Continue(0),
                "hook should bind {line:?} positionally"
            );
        }
        // A command-position call (flags enabled) still reads `--foo` as a flag.
        assert_eq!(
            super::call_func(
                "hook",
                vec![(Value::String("--foo".into()), false)],
                true,
                &mut shell,
            ),
            Step::Continue(2),
        );
    }

    #[test]
    fn compound_input_completeness_comes_from_the_parser() {
        for input in [
            "func f() {\nputs hi\n",
            "if true {\nputs yes\n",
            "for x in [1 2] {\nputs $x\n",
            "func f() {\nif true {\nputs hi\n}\n",
        ] {
            assert!(needs_more_input(input), "expected incomplete: {input:?}");
        }
        for input in [
            "func f() {\nputs hi\n}\n",
            "if true {\nputs yes\n}\n",
            "for x in [1 2] {\nputs $x\n}\n",
            "func f() {\nif true {\nputs hi\n}\n}\n",
        ] {
            assert!(!needs_more_input(input), "expected complete: {input:?}");
        }
        assert!(!needs_more_input("cd /"));
        assert!(!needs_more_input("puts *"));
        assert!(needs_more_input("puts value |"));
        assert!(!needs_more_input("puts 'unterminated"));
    }

    #[test]
    fn malformed_function_bodies_are_buffered_without_swallowing_trailing_braces() {
        assert!(needs_more_input("func f(') {\nputs LEAKED\n"));
        assert!(!needs_more_input("func f(') {\nputs LEAKED\n}\n"));
        assert!(!needs_more_input("func f() {} {\n"));
    }

    #[test]
    fn command_interpolation_accepts_closing_brackets_in_quoted_map_keys() {
        let parser::ParseOutcome::Complete(source) = parser::parse("puts $m[\"a]b\"]").unwrap()
        else {
            panic!("source should be complete");
        };
        let parser::Executable::Pipeline(pipeline) = &source.statements[0].and_or.first else {
            panic!("source should contain a pipeline");
        };
        let parser::CommandItem::Word(word) = &pipeline.stages[0].items[1] else {
            panic!("second command item should be a word");
        };
        let mut shell = Shell::new();
        shell.vars.set_value(
            "m",
            Value::Map(vec![("a]b".into(), Value::String("ok".into()))]),
        );

        assert_eq!(
            crate::expand::expand_values(vec![expansion_word(&word.value)], &shell.vars),
            Ok(vec![Value::String("ok".into())])
        );
    }

    #[test]
    fn ctrl_d_exits_with_the_last_status() {
        let mut shell = Shell::new();
        let mut pending = String::new();
        let mut gate = HeredocGate::default();
        assert_eq!(
            handle_signal(Signal::CtrlD, 7, &mut shell, &mut pending, &mut gate),
            Some(Step::Exit(7))
        );
    }

    #[test]
    fn ctrl_d_exits_even_mid_function_definition() {
        // With a `func` body still buffered, Ctrl-D still exits (abandoning it).
        let mut shell = Shell::new();
        let mut pending = String::from("func f() {\n");
        let mut gate = HeredocGate::default();
        assert_eq!(
            handle_signal(Signal::CtrlD, 4, &mut shell, &mut pending, &mut gate),
            Some(Step::Exit(4))
        );
    }

    #[test]
    fn ctrl_c_re_prompts_keeping_status() {
        let mut shell = Shell::new();
        let mut pending = String::new();
        let mut gate = HeredocGate::default();
        assert_eq!(
            handle_signal(Signal::CtrlC, 7, &mut shell, &mut pending, &mut gate),
            Some(Step::Continue(7))
        );
    }

    #[test]
    fn a_submitted_exit_line_exits() {
        let mut shell = Shell::new();
        let mut pending = String::new();
        let mut gate = HeredocGate::default();
        let signal = Signal::Success("exit 5".to_string());
        assert_eq!(
            handle_signal(signal, 0, &mut shell, &mut pending, &mut gate),
            Some(Step::Exit(5))
        );
    }

    #[test]
    fn a_submitted_blank_line_keeps_the_status() {
        let mut shell = Shell::new();
        assert_eq!(run_line("   ", 3, false, &mut shell), Step::Continue(3));
    }

    #[test]
    fn assignment_then_read() {
        let mut shell = Shell::new();
        assert_eq!(
            run_line("x = hello", 0, false, &mut shell),
            Step::Continue(0)
        );
        assert_eq!(
            shell.vars.get("x"),
            Some(&Value::String("hello".to_string()))
        );
    }

    #[test]
    fn unspaced_assignment() {
        let mut shell = Shell::new();
        assert_eq!(run_line("n=42", 0, false, &mut shell), Step::Continue(0));
        assert_eq!(shell.vars.get("n"), Some(&Value::Integer(42)));
    }

    #[test]
    fn parsed_expressions_preserve_typed_values_through_access_and_modifiers() {
        let mut shell = Shell::new();
        let parser::ParseOutcome::Complete(source) =
            parser::parse("tail = [one two]; xs = [$tail ...$tail]; result = $xs[0]:last").unwrap()
        else {
            panic!("source should be complete");
        };

        assert_eq!(run_source(&source, 0, false, &mut shell), Step::Continue(0));
        assert_eq!(shell.vars.get("result"), Some(&Value::String("two".into())));
    }

    #[test]
    fn parsed_operators_and_recursive_value_bodies_evaluate() {
        let mut shell = Shell::new();
        let parser::ParseOutcome::Complete(source) = parser::parse(
            "answer = if true { if false { 0 } else { 6 * 7 } }; \
             values = for x in [1 2 3] { if true { $x + 1 } }",
        )
        .unwrap() else {
            panic!("source should be complete");
        };

        assert_eq!(run_source(&source, 0, false, &mut shell), Step::Continue(0));
        assert_eq!(shell.vars.get("answer"), Some(&Value::Integer(42)));
        assert_eq!(
            shell.vars.get("values"),
            Some(&Value::List(vec![
                Value::Integer(2),
                Value::Integer(3),
                Value::Integer(4),
            ]))
        );
    }

    #[test]
    fn map_expressions_preserve_typed_values() {
        let mut shell = Shell::new();
        let parser::ParseOutcome::Complete(source) = parser::parse("value = [key: value]").unwrap()
        else {
            panic!("source should be complete");
        };

        assert_eq!(run_source(&source, 0, false, &mut shell), Step::Continue(0));
        assert_eq!(
            shell.vars.get("value"),
            Some(&Value::Map(vec![(
                "key".into(),
                Value::String("value".into())
            )]))
        );
    }

    #[test]
    fn operator_assignments_use_the_ast_evaluator_from_run_line() {
        let mut shell = Shell::new();
        for (source, name, expected) in [
            ("product = 6 * 7", "product", "42"),
            ("quotient = 8 / 2", "quotient", "4"),
            ("equal = 1 == 1", "equal", "true"),
            ("member = 2 in [1 2]", "member", "true"),
        ] {
            assert_eq!(run_line(source, 0, false, &mut shell), Step::Continue(0));
            let expected = match expected {
                "true" => Value::Boolean(true),
                value => Value::Integer(value.parse().unwrap()),
            };
            assert_eq!(shell.vars.get(name), Some(&expected));
        }
    }

    #[test]
    fn for_expression_control_does_not_evaluate_or_collect_the_tail() {
        let mut shell = Shell::new();
        assert_eq!(
            run_line(
                "stopped = for x in [1 2 3] { break; $x }",
                0,
                false,
                &mut shell
            ),
            Step::Continue(0)
        );
        assert_eq!(shell.vars.get("stopped"), Some(&Value::List(Vec::new())));

        assert_eq!(
            run_line(
                "skipped = for x in [1 2 3] { continue; $x }",
                0,
                false,
                &mut shell,
            ),
            Step::Continue(0)
        );
        assert_eq!(shell.vars.get("skipped"), Some(&Value::List(Vec::new())));
    }

    #[test]
    fn checked_division_distinguishes_zero_from_overflow() {
        assert_eq!(
            eval_binary(
                Value::Integer(i64::MIN),
                parser::BinaryOp::Divide,
                Value::Integer(-1),
            ),
            Err("numeric overflow".into())
        );
        assert_eq!(
            eval_binary(
                Value::Integer(1),
                parser::BinaryOp::Divide,
                Value::Integer(0),
            ),
            Err("division by zero".into())
        );
    }

    #[test]
    fn a_multi_line_func_buffers_until_the_brace_closes() {
        let mut shell = Shell::new();
        let mut pending = String::new();
        let mut gate = HeredocGate::default();
        // The opening line leaves the body open — no step yet.
        assert_eq!(
            handle_signal(
                Signal::Success("func greet(who) {".into()),
                0,
                &mut shell,
                &mut pending,
                &mut gate
            ),
            None
        );
        assert_eq!(
            handle_signal(
                Signal::Success("  puts \"hi $who\"".into()),
                0,
                &mut shell,
                &mut pending,
                &mut gate
            ),
            None
        );
        // The closing brace completes and defines the function.
        assert_eq!(
            handle_signal(
                Signal::Success("}".into()),
                0,
                &mut shell,
                &mut pending,
                &mut gate
            ),
            Some(Step::Continue(0))
        );
        assert!(pending.is_empty());
        // Calling it now runs the body.
        assert_eq!(
            run_line("greet world", 0, false, &mut shell),
            Step::Continue(0)
        );
    }

    #[test]
    fn a_non_brace_line_completes_an_invalid_buffered_unit() {
        // The parser alone decides when the buffered unit is no longer
        // incomplete; the reader does not reinterpret its physical lines.
        let mut shell = Shell::new();
        let mut pending = String::new();
        let mut gate = HeredocGate::default();
        assert_eq!(
            handle_signal(
                Signal::Success("func f()".into()),
                0,
                &mut shell,
                &mut pending,
                &mut gate
            ),
            None
        );
        let step = handle_signal(
            Signal::Success("puts after".into()),
            0,
            &mut shell,
            &mut pending,
            &mut gate,
        );
        assert_eq!(step, Some(Step::Continue(2)));
        assert!(pending.is_empty());
        // `f` was never defined.
        assert!(shell.funcs.get("f").is_none());
    }

    #[test]
    fn a_bare_return_at_top_level_is_reported() {
        // Outside a function, `return` is a recoverable error (status 1), not an
        // unwind — `run_line` reports it and continues rather than propagating it.
        let mut shell = Shell::new();
        assert_eq!(run_line("return", 0, false, &mut shell), Step::Continue(1));
    }

    #[test]
    fn a_function_local_does_not_escape_the_call() {
        let mut shell = Shell::new();
        // Define a function that binds a local `x`, then confirm it does not leak.
        assert_eq!(
            run_line("func setx() { x = inside }", 0, false, &mut shell),
            Step::Continue(0)
        );
        assert_eq!(run_line("setx", 0, false, &mut shell), Step::Continue(0));
        assert_eq!(shell.vars.get("x"), None);
    }

    /// Write `value` as a literal, run that text back through the shell, and
    /// return what the reader made of it.
    ///
    /// The writer's own unit tests pin the text it emits, which only proves it
    /// agrees with itself. This is the assertion that matters: the reader is the
    /// ordinary parser, so a literal that reads back as a different value is a
    /// writer bug no amount of expected-string checking would catch.
    fn round_trip(value: &Value) -> Value {
        let literal = value.to_literal().expect("this value has a literal form");
        let mut shell = Shell::new();
        assert_eq!(
            run_line(&format!("x = {literal}"), 0, false, &mut shell),
            Step::Continue(0),
            "the literal {literal} did not run cleanly"
        );
        shell.vars.get("x").expect("x is bound").clone()
    }

    #[test]
    fn a_written_literal_reads_back_as_the_same_value() {
        let cases = vec![
            Value::Integer(0),
            Value::Integer(42),
            Value::Integer(-5),
            Value::Integer(i64::MAX),
            Value::Integer(i64::MIN),
            Value::Boolean(true),
            Value::Boolean(false),
            // Strings whose text is another type's literal: these fail the trip
            // unless the writer quotes unconditionally.
            Value::String("42".into()),
            Value::String("true".into()),
            Value::String("-5".into()),
            Value::String("foo".into()),
            Value::String(String::new()),
            // Text that has to survive the quoting rather than the type rule.
            Value::String("a b".into()),
            Value::String("a$b".into()),
            Value::String("it's".into()),
            Value::String("back\\slash".into()),
            Value::String("new\nline\ttab".into()),
            Value::String("\u{7}bell".into()),
            Value::String("[not, a, list]".into()),
            Value::String("#comment | pipe > redirect".into()),
            Value::List(Vec::new()),
            Value::Map(Vec::new()),
            Value::List(vec![Value::Integer(1), Value::String("a b".into())]),
            Value::Map(vec![("k".into(), Value::Integer(1))]),
            // Awkward keys: a space and a `:` both break an unquoted key.
            Value::Map(vec![
                ("a b".into(), Value::String("x".into())),
                ("with:colon".into(), Value::Boolean(false)),
            ]),
            // Nesting, and the two empty collections nested where their spellings
            // have to stay apart.
            Value::List(vec![
                Value::List(vec![Value::Integer(1)]),
                Value::Map(vec![("k".into(), Value::List(Vec::new()))]),
                Value::Map(Vec::new()),
            ]),
        ];
        for value in cases {
            assert_eq!(
                round_trip(&value),
                value,
                "{value:?} did not survive the round trip"
            );
        }
    }

    /// Both ends of the integer range survive the trip.
    ///
    /// `i64::MIN` is the case that needed the parser fix: the magnitudes are not
    /// symmetric, so `-9223372036854775808` has no positive counterpart, and while
    /// the sign was applied at runtime the literal read `9223372036854775808`
    /// first — too large for an `i64` — and failed with "expected integer". The
    /// writer refused the value meanwhile rather than emit text that would not
    /// read back; now that the parser folds the sign into the literal, it writes.
    #[test]
    fn both_ends_of_the_integer_range_round_trip() {
        for value in [Value::Integer(i64::MIN), Value::Integer(i64::MAX)] {
            assert_eq!(round_trip(&value), value, "{value:?} did not survive");
        }
        // The reachable route to `i64::MIN`, since it cannot be reached by typing
        // a literal until this parses — which is exactly what was fixed.
        let mut shell = Shell::new();
        assert_eq!(
            run_line("x = -9223372036854775807 - 1", 0, false, &mut shell),
            Step::Continue(0)
        );
        assert_eq!(shell.vars.get("x"), Some(&Value::Integer(i64::MIN)));
    }

    #[test]
    fn history_designators_finds_only_bare_bangs() {
        // Bare designators are located by byte offset, left to right.
        assert_eq!(
            history_designators("", "cp !^ !$"),
            vec![(3, '^'), (6, '$')]
        );
        // Quotes, raw strings, escapes, and interpolation keep a bang literal.
        assert!(history_designators("", "puts '!$'").is_empty());
        assert!(history_designators("", "puts \"!$\"").is_empty());
        assert!(history_designators("", "puts r'!$'").is_empty());
        assert!(history_designators("", "puts \\!$").is_empty());
        assert!(history_designators("", "puts ${!$}").is_empty());
        // mesh single quotes escape `\'`, so the string stays open across it.
        assert!(history_designators("", "puts 'can\\'t !$'").is_empty());
        // `!=` / `!~` and a lone bang are not designators.
        assert!(history_designators("", "test 1 != 2 !~ x !").is_empty());
    }

    #[test]
    fn history_designators_treat_delimiters_as_word_boundaries() {
        // A compact body opens a fresh word after `{`, so `r'x\'` is a raw string
        // (no escapes) and the following `!$` is bare and found.
        assert_eq!(
            history_designators("", r"func f(){r'x\' !$}"),
            vec![(15, '$')]
        );
        // Likewise inside a compact call argument list: `(` and `,` are fresh
        // words, so the raw string ends at its quote and `!$` is bare.
        assert_eq!(history_designators("", r"f(r'x\', !$)"), vec![(9, '$')]);
        // A colon is a token boundary too, so the raw prefix in `key:r'…'` is raw
        // and the trailing `!$` is bare.
        assert_eq!(
            history_designators("", r"puts key:r'x\' !$"),
            vec![(15, '$')]
        );
    }

    #[test]
    fn history_designators_skip_comments() {
        // A bare `#` starts a comment; nothing after it on the line expands.
        assert!(history_designators("", "# !$").is_empty());
        assert!(history_designators("", "puts hi # !$").is_empty());
        // A `#` in the middle of a word is literal, so the `!$` still expands.
        assert_eq!(history_designators("", "puts a#!$"), vec![(7, '$')]);
        // A comment on a buffered line does not suppress the next line.
        assert_eq!(history_designators("puts x # note\n", "!$"), vec![(0, '$')]);
        // A `\`-newline continuation keeps token-start eligibility, so a `#` that
        // opens the continued line is still a comment.
        assert!(history_designators("func f() { \\\n", "# !$").is_empty());
        // `!~` / `!=` are operator tokens, so an adjacent `#` still starts a
        // comment and the trailing designator is ignored.
        assert!(history_designators("", "puts a!~# !$").is_empty());
        assert!(history_designators("", "puts a!=# !$").is_empty());
    }

    #[test]
    fn history_designators_skip_heredoc_bodies() {
        // A heredoc body is raw document text — a designator inside it is literal,
        // whether the delimiter is quoted or bare.
        assert!(history_designators("cat << 'EOF'\n", "hello !$").is_empty());
        assert!(history_designators("cat << EOF\n", "hello !$").is_empty());
        // A designator after the body's closing delimiter still expands.
        assert_eq!(
            history_designators("cat << EOF\nbody\nEOF\n", "puts !$"),
            vec![(5, '$')]
        );
        // A designator on the `<<delim` line itself (before the body) expands.
        assert_eq!(history_designators("", "cat !$ << EOF"), vec![(4, '$')]);
        // A composite delimiter (`"EO"F` → `EOF`) is matched as its full text, so
        // the body ends at the real `EOF` and a later designator still expands.
        assert_eq!(
            history_designators("cat << \"EO\"F\nbody\nEOF\n", "puts !$"),
            vec![(5, '$')]
        );
        // A bare designator used as the delimiter itself expands.
        assert_eq!(history_designators("", "cat <<!$"), vec![(6, '$')]);
        // A quoted designator in the delimiter stays literal.
        assert!(history_designators("", "cat <<'!$'").is_empty());
    }

    #[test]
    fn history_designators_carry_state_from_the_pending_prefix() {
        // A double-quoted string opened in a buffered line keeps a designator on
        // the continuation line literal — strings span physical lines.
        assert!(history_designators("func f() {\nputs \"hello\n", "!$\"").is_empty());
        // Once the string closes, a later bare designator is found.
        assert_eq!(
            history_designators("func f() {\nputs \"hello\n", "!$\" !$"),
            vec![(4, '$')]
        );
    }

    /// The body's closing `}` is found through the **real tokenizer**, so a brace
    /// that is not block structure — quoted, raw, escaped, or part of a `${…}`
    /// interpolation — cannot be counted. Getting any of these wrong swallows the
    /// body's `}` and leaves the definition buffering forever, or releases its
    /// lines to the top level early. The bespoke char scanner this replaced needed
    /// a rule per case (word starts, `&` boundaries, escaped newlines, raw
    /// eligibility); the tokenizer answers all of them by construction.
    #[test]
    fn func_definition_is_open_counts_only_block_braces() {
        for closed in [
            r#"func f() { puts "{ ${x} }" }"#,
            r"func f() { puts '{' }",
            r"func f() { puts r'{' }",
            r"func f() { puts \{ }",
            // A raw string whose content ends in a backslash: the `\` is not an
            // escape, so it must not swallow the closing quote and then the `}`.
            // A raw prefix is raw wherever a word starts — after `&`, right after
            // the body's own `{`, or on the next line after a `\`-newline.
            r"func f() { true&r'\' }",
            r"func f(){r'\'}",
            "func f() { true \\\nr'\\' }",
        ] {
            assert!(
                !func_definition_is_open(closed),
                "expected complete: {closed:?}"
            );
        }
        // A real block brace alongside an interpolation still counts as one.
        assert!(func_definition_is_open("func f() { puts ${x}"));
    }

    /// A tokenize failure means two different things, and conflating them hangs the
    /// reader. An **unterminated** quote or heredoc is an open construct that a later
    /// line can still close, so it buffers. A **hard** lexical error — an invalid
    /// escape — is one no later line repairs, so it must dispatch and let the parser
    /// report it. Treating every failure as "still open" buffered forever behind a
    /// diagnostic that never arrived: interactively the continuation prompt could
    /// only be escaped by cancelling, and on piped input every following command was
    /// swallowed into the same buffer.
    #[test]
    fn a_hard_lexical_error_in_a_body_dispatches_but_an_open_construct_buffers() {
        // Hard error with the `}` present: the definition is done, so it dispatches
        // rather than waiting for a repair that can never come.
        for hard in [
            r#"func f() { puts "\z" }"#,
            r#"func f() { puts "\u{ZZ}" }"#,
            "func f() {\n  puts \"\\z\"\n}\n",
        ] {
            assert!(
                !func_definition_is_open(hard),
                "a closed body should dispatch: {hard:?}"
            );
        }

        // The same error with no `}` yet: still open, so the body's later lines stay
        // quarantined instead of leaking to the top level.
        for hard in [
            "func f() { puts \"\\z\"\n",
            "func f() {\nputs \"\\z\"\nputs LEAKED\n",
        ] {
            assert!(
                func_definition_is_open(hard),
                "an unclosed body should stay quarantined: {hard:?}"
            );
        }

        // Open: a string or heredoc still being written spans physical lines, so the
        // body keeps buffering until the construct and then the body close.
        for open in [
            "func f() {\n  puts \"line one\n",
            "func f() {\n  puts 'still going\n",
            "func f() {\n  cat << END\nhello\n",
        ] {
            assert!(
                func_definition_is_open(open),
                "an open construct should buffer: {open:?}"
            );
        }
        // And each of those closes once its construct and body do.
        for closed in [
            "func f() {\n  puts \"line one\nline two\"\n}\n",
            "func f() {\n  cat << END\nhello\nEND\n}\n",
        ] {
            assert!(
                !func_definition_is_open(closed),
                "expected complete: {closed:?}"
            );
        }
    }

    /// The same split stated directly on the helper, since `func_definition_is_open`
    /// reaches it only after the header has opened a body.
    #[test]
    fn body_awaits_close_answers_only_whether_the_brace_arrived() {
        // A hard error sits inside a string and cannot move a brace, so the `}`
        // behind it still closes the body and its absence still leaves it open.
        assert!(!body_awaits_close(r#"{ puts "\z" }"#));
        assert!(body_awaits_close(r#"{ puts "\z""#));
        assert!(body_awaits_close("{ puts \"\\z\"\nputs more\n"));
        // Any number of hard errors, with no recovery budget to run out of: a
        // bounded retry loop silently reverted to "open" past its cap.
        assert!(!body_awaits_close("{ puts \"\\z\"\nputs \"\\q\"\n}"));
        let many = format!("{{ puts \"{}\" }}", "\\z".repeat(200));
        assert!(!body_awaits_close(&many));
        // A **zero-width** diagnostic — `${}` reports an empty span — has nothing to
        // blank, so a span-blanking recovery could not advance past it at all.
        assert!(!body_awaits_close("{ puts ${} }"));
        assert!(body_awaits_close("{ puts ${}"));
        // An unterminated construct is different in kind: the rest of the input is
        // inside it, so a `}` in there is string content, not block structure.
        assert!(body_awaits_close("{ puts \"open\n"));
        assert!(body_awaits_close("{ puts \"open } still open\n"));
        // And the plain cases are unchanged.
        assert!(body_awaits_close("{ puts hi"));
        assert!(!body_awaits_close("{ puts hi }"));
    }

    /// A tokenize failure *after* the body's `}` says nothing about the body: it
    /// already closed. Reading the failure as "still open" made `func f() {} puts "`
    /// buffer forever even though the definition was two tokens done, swallowing
    /// every command after it — the char scanner this replaced got the first-close
    /// answer for free by never failing at all.
    #[test]
    fn a_close_before_a_tokenize_failure_still_settles_the_body() {
        assert!(!body_awaits_close(r#"{} puts ""#));
        assert!(!body_awaits_close("{} cat << END"));
        assert!(!body_awaits_close(r#"{ puts hi } puts ""#));
        // But a failure with no close before it is still the open construct it was.
        assert!(body_awaits_close(r#"{ puts ""#));
        assert!(!func_definition_is_open("func f() {} puts \""));
    }

    #[test]
    fn func_definition_is_open_is_brace_driven() {
        assert!(func_definition_is_open("func f() {"));
        assert!(func_definition_is_open("func f() {\n  puts hi\n"));
        assert!(!func_definition_is_open("func f() { puts hi }"));
        // A malformed header that opens a body still buffers to the matching `}`,
        // so its later body lines cannot leak to the top level (the P1 case).
        assert!(func_definition_is_open("func f(x {\nputs )\nputs LEAKED\n"));
        assert!(!func_definition_is_open(
            "func f(x {\nputs )\nputs LEAKED\n}\n"
        ));
    }

    #[test]
    fn func_definition_is_open_buffers_a_delayed_body_opener() {
        // The grammar's `")" ws? "{"` lets the `{` sit on a later line, so an
        // otherwise-complete header keeps buffering until the body opens/closes.
        assert!(func_definition_is_open("func f()\n"));
        assert!(func_definition_is_open("func f()\n{\n  puts hi\n"));
        assert!(!func_definition_is_open("func f()\n{\n  puts hi\n}\n"));
        // A still-forming signature also keeps reading.
        assert!(func_definition_is_open("func f(a,\n"));
        assert!(func_definition_is_open("func\n"));
        // A malformed header is NOT buffered — it dispatches to a parse error so
        // following commands are not swallowed: non-whitespace after `)`, a
        // signature `)` with no opening `(`/name before it, an invalid name, or a
        // name not followed by `(`.
        assert!(!func_definition_is_open("func f() oops\n"));
        assert!(!func_definition_is_open("func f() ; puts hi\n"));
        assert!(!func_definition_is_open("func f)\n"));
        assert!(!func_definition_is_open("func 1f(\n"));
        assert!(!func_definition_is_open("func f oops\n"));
        // A closed but invalid parameter list is also dispatched immediately —
        // the same validation the parser applies, so no invalid shape buffers.
        assert!(!func_definition_is_open("func f(,)\n"));
        assert!(!func_definition_is_open("func f(a,a)\n"));
        // The flag / optional / rest forms are valid signatures, so a closed one
        // with a delayed body opener keeps buffering rather than dispatching.
        assert!(func_definition_is_open("func f(...xs)\n"));
        assert!(func_definition_is_open("func f(--force)\n"));
        assert!(func_definition_is_open("func f(x = 1)\n"));
        assert!(func_definition_is_open("func f(--tag = latest)\n"));
        // An unclosed but provably-invalid parameter list is dispatched too, while
        // a valid partial list (a name or new-form parameter still forming) keeps
        // buffering.
        assert!(!func_definition_is_open("func f(,\n"));
        assert!(!func_definition_is_open("func f(a,a,\n"));
        assert!(func_definition_is_open("func f(a\n"));
        assert!(func_definition_is_open("func f(a, b\n"));
        assert!(func_definition_is_open("func f(a,\n"));
        // A `...`/`--` prefix with its name attached keeps buffering; a bare prefix
        // finalized by the newline can never gain its name, so it dispatches.
        assert!(!func_definition_is_open("func f(...\n"));
        assert!(func_definition_is_open("func f(...xs\n"));
        assert!(!func_definition_is_open("func f(--\n"));
        assert!(func_definition_is_open("func f(--force\n"));
        assert!(func_definition_is_open("func f(a =\n"));
        assert!(func_definition_is_open("func f(a = 1,\n"));
        // A trailing parameter token whose head cannot start an identifier is
        // impossible, so it dispatches instead of entering continuation mode.
        assert!(!func_definition_is_open("func f(_\n"));
        assert!(!func_definition_is_open("func f(1\n"));
        assert!(!func_definition_is_open("func f(a, _\n"));
        // A still-forming name (before `(`) with a valid letter head keeps reading,
        // including a partial kebab name; an impossible head dispatches.
        assert!(func_definition_is_open("func my-\n"));
        assert!(!func_definition_is_open("func _f\n"));
        assert!(!func_definition_is_open("func 1f\n"));
    }

    #[test]
    fn func_definition_is_open_uses_the_signature_to_find_the_body_opener() {
        // A `{` inside a following command (or hidden by a malformed quoted param)
        // is not the body opener, so a completed header awaiting its body is not
        // kept pending by such a brace.
        assert!(!func_definition_is_open("func f()\nputs '{'\n"));
        assert!(!func_definition_is_open("func f()\nputs '{'\nputs after\n"));
        // A real body opener right after the signature still buffers.
        assert!(func_definition_is_open("func f() {\n"));
        assert!(func_definition_is_open("func f()\n{\n"));
        // A malformed header with a brace in the parameter region still quarantines.
        assert!(func_definition_is_open("func f(x {\nputs LEAK\n"));
        assert!(func_definition_is_open("func f(') {\nputs LEAKED\n"));
    }

    #[test]
    fn func_definition_is_open_finds_the_signature_close_past_a_default_expression() {
        // A default expression may itself contain `)`, `{`/`}`, `[`/`]`, a comma,
        // or a quoted `)` — none of which is the signature's closing `)`. The
        // signature scan honors nesting and the lexer's quote/escape rules, so a
        // closed header with a delayed body opener still buffers.
        assert!(func_definition_is_open("func f(x = (1 + 2))\n"));
        assert!(func_definition_is_open("func f(x = (1 + 2))\n{\n"));
        assert!(func_definition_is_open("func f(x = [a, b])\n{\n"));
        assert!(func_definition_is_open("func f(x = {k: v})\n{\n"));
        assert!(func_definition_is_open("func f(x = \")\")\n{\n"));
        assert!(func_definition_is_open("func f(x = \"a\\\",b\")\n{\n"));
        // The body still opens right after the true signature close.
        assert!(!func_definition_is_open("func f(x = (1 + 2)) { puts $x }"));
        // An unterminated quote in a default keeps the signature open (buffering),
        // not falsely closed at a later `)`.
        assert!(func_definition_is_open("func f(x = \"a)\n"));
    }

    #[test]
    fn func_definition_is_open_dispatches_a_closed_header_with_an_invalid_default() {
        // A closed signature whose default cannot parse (`x = ]`) is a hard error,
        // not an incomplete header: dispatch it immediately so the parser reports
        // the error and the following command is not swallowed into the buffer. A
        // stray close delimiter no longer hides the signature's real `)`.
        assert!(!func_definition_is_open("func f(x = ])\n"));
        assert!(!func_definition_is_open("func f(x = })\n"));
        assert!(!func_definition_is_open("func f(x = 1 +)\n"));
        // A well-formed default is still a valid closed signature awaiting its body.
        assert!(func_definition_is_open("func f(x = 1 + 2)\n"));
    }

    #[test]
    fn func_definition_is_open_skips_comments_when_scanning_a_signature() {
        // A `#` comment runs to the newline, so a `)`/`{`/`,` inside it is not
        // signature structure. The definition keeps buffering for the real `)`.
        assert!(func_definition_is_open("func f(x = 1 # comment )\n"));
        assert!(func_definition_is_open("func f(x = 1 # comment )\n) {\n"));
        assert!(func_definition_is_open("func f(x = 1 # brace {\n) {\n"));
        assert!(func_definition_is_open("func f(\n  a, # a, comment\n  b\n"));
        // The whole definition still closes and defines once its body arrives.
        assert!(!func_definition_is_open(
            "func f(x = 1 # c )\n) { puts $x }"
        ));
        // A `#` mid-word is literal, not a comment, so the `)` still closes.
        assert!(!func_definition_is_open("func f(x = a#b) oops\n"));
        // A delimiter is a word boundary, so a `#` immediately after `{`/`[`/`(`
        // starts a comment — its `}`/`)` are not signature structure.
        assert!(func_definition_is_open("func f(x = if true {# } )\n"));
        assert!(func_definition_is_open("func f(x = [# ]\n"));
        // An operator is a word boundary too, so a `#` after ` + ` is a comment;
        // its `)` is not the signature close, so the header keeps buffering.
        assert!(func_definition_is_open("func f(x = 1 + # note )\n"));
        // But a `#` is only a comment at a *contextual* word boundary. `/#tag`
        // tokenizes as one bare word with a literal `#`, so the following `)` is
        // the real signature close — the definition completes rather than getting
        // lost in a phantom comment that swallows the `)` to EOF.
        assert!(!func_definition_is_open("func f(x = /#tag) { puts $x }"));
        assert!(!func_definition_is_open("func f(x = /#tag) oops\n"));
        // Signature closed at that `)`, so the header now legitimately awaits its
        // body — buffering for the right reason, not because the `)` was hidden.
        assert!(func_definition_is_open("func f(x = /#tag)\n"));
    }

    #[test]
    fn func_definition_is_open_dispatches_a_reserved_or_duplicate_final_name() {
        // A final parameter name the line break finalized is validated in full, so a
        // duplicate or reserved (`env`) name dispatches immediately rather than
        // buffering the following command into the malformed definition. A trailing
        // space finalizes the name the same way.
        assert!(!func_definition_is_open("func f(a, a\n"));
        assert!(!func_definition_is_open("func f(env\n"));
        assert!(!func_definition_is_open("func f(a, a \n"));
        // A valid final name still buffers (it may yet be followed by `,`/`)`), and
        // a bare name the cursor is still extending (no trailing whitespace) too.
        assert!(func_definition_is_open("func f(a\n"));
        assert!(func_definition_is_open("func f(a, b\n"));
        assert!(func_definition_is_open("func f(ab"));
        // A `...`/`--` prefix mid-type (no trailing whitespace) keeps reading — its
        // name may still be typed on the same line — but once a newline finalizes
        // the empty name it can never be completed (the parser skips no whitespace
        // before the name), so it dispatches instead of buffering later commands.
        assert!(func_definition_is_open("func f(..."));
        assert!(func_definition_is_open("func f(--"));
        assert!(!func_definition_is_open("func f(...\n"));
        assert!(!func_definition_is_open("func f(--\n"));
        // A reserved or duplicate name is rejected even when its default is still
        // unfinished — finishing the default can never make the name valid — so it
        // dispatches rather than buffering the next line into the definition. A
        // valid name with an unfinished default still buffers (the default may run
        // onto later lines).
        assert!(!func_definition_is_open("func f(env =\n"));
        assert!(!func_definition_is_open("func f(a, a =\n"));
        assert!(!func_definition_is_open("func f(--env =\n"));
        assert!(func_definition_is_open("func f(a =\n"));
        assert!(func_definition_is_open("func f(a, b =\n"));
    }

    #[test]
    fn func_definition_is_open_dispatches_an_impossible_parameter_ordering() {
        // An unclosed signature whose finalized ordering the parser can never accept
        // dispatches at once rather than buffering the following command: a required
        // positional after an optional, any parameter after a `...rest`, and an
        // optional coexisting with a rest.
        assert!(!func_definition_is_open("func f(a = 1, b\n"));
        assert!(!func_definition_is_open("func f(...xs, a\n"));
        assert!(!func_definition_is_open("func f(a = 1, ...xs\n"));
        assert!(!func_definition_is_open("func f(...xs,\n"));
        // A still-extending final bare name is not yet fixed as required (it may
        // gain `= default`), so `a = 1, b` keeps reading until the token finalizes;
        // flags are order-independent, and valid orderings still buffer.
        assert!(func_definition_is_open("func f(a = 1, b"));
        assert!(func_definition_is_open("func f(a = 1, b = 2\n"));
        assert!(func_definition_is_open("func f(--tag = latest, a\n"));
        assert!(func_definition_is_open("func f(a, ...xs\n"));
        // A newline between a name and its `=` detaches the default (the parser
        // finalizes the name at the break), so the header is irreparable and
        // dispatches; a newline *after* the `=` is a default spanning lines and
        // keeps buffering.
        assert!(!func_definition_is_open("func f(a\n= 1\n"));
        assert!(!func_definition_is_open("func f(--flag\n= 1\n"));
        assert!(func_definition_is_open("func f(a =\n1\n"));
        // A `...name`/`--name` prefix requires the name to abut it, matching the
        // parser: whitespace between the prefix and the name is not a parameter, so
        // the header dispatches. The adjacent forms still buffer.
        assert!(!func_definition_is_open("func f(... xs\n"));
        assert!(!func_definition_is_open("func f(-- force\n"));
        assert!(func_definition_is_open("func f(...xs\n"));
        assert!(func_definition_is_open("func f(--force\n"));
    }

    #[test]
    fn the_reader_and_parser_agree_on_signature_validity() {
        // The reader's still-forming check delegates to the parser, so the two can
        // never disagree about whether a closed signature is valid. For a matrix of
        // parameter lists, a newline-terminated open header dispatches exactly when
        // the parser rejects the same list closed — a growing (unterminated) final
        // token is the only reader-specific exception, so terminate each list.
        for list in [
            "",
            "a",
            "a, b",
            "a = 1",
            "a = 1, b = 2",
            "--force",
            "--tag = latest",
            "...xs",
            "a, ...xs",
            "a, --force, ...xs",
            "env",
            "a, a",
            "a = 1, b",
            "...xs, a",
            "a = 1, ...xs",
            "... xs",
            "-- force",
            "...\nxs",
            "a\n= 1",
            "a b",
            "a,,b",
            ",a",
            "a = ]",
        ] {
            let parser_accepts = matches!(
                crate::parser::parse(&format!("func f({list}) {{}}")),
                Ok(crate::parser::ParseOutcome::Complete(_))
            );
            let reader_buffers = func_definition_is_open(&format!("func f({list}\n"));
            assert_eq!(
                parser_accepts, reader_buffers,
                "disagreement on {list:?}: parser_accepts={parser_accepts} reader_buffers={reader_buffers}"
            );
        }
    }

    #[test]
    fn func_definition_is_open_dispatches_a_mismatched_signature_delimiter() {
        // A default that closes with the wrong delimiter (`[1)`) is malformed, not
        // incomplete: the `)` does not close the `[`, so dispatch immediately
        // rather than buffer the following command. A brace context is exempt — it
        // is handled by the body/quarantine logic, not treated as a bad default.
        assert!(!func_definition_is_open("func f(x = [1)\n"));
        assert!(!func_definition_is_open("func f(x = (1])\n"));
        assert!(!func_definition_is_open("func f(x = [1})\n"));
        // A balanced nested default is still a valid closed signature.
        assert!(func_definition_is_open("func f(x = [1])\n"));
        // A `$( … )` command capture nests like a paren, so its `)` is not the
        // signature close: the header keeps buffering for the real `)` and body.
        assert!(func_definition_is_open("func f(x = $(puts hi))\n"));
        assert!(!func_definition_is_open(
            "func f(x = $(puts hi)) { puts $x }"
        ));
        // A stray brace with no `=` still quarantines to its matching `}`.
        assert!(func_definition_is_open("func f(x {\nputs )\n"));
        // A top-level stray `]`/`}` before any signature `)` is a hard mismatch,
        // not an incomplete header: dispatch immediately rather than skip it and
        // buffer the following command to EOF.
        assert!(!func_definition_is_open("func f(x = ]\nputs after\n"));
        assert!(!func_definition_is_open("func f(x = }\nputs after\n"));
    }

    #[test]
    fn func_definition_is_open_buffers_a_block_bearing_default() {
        // A default that is itself a block-bearing expression (`if`/`match`) has
        // `{ … }` braces that are *not* the function body: while the signature `(`
        // is still open, an inner block's close must not end buffering. Only a
        // provably-malformed header (`func f(x {`, no `=`) quarantines from a brace.
        assert!(func_definition_is_open("func f(x = if true {\n"));
        assert!(func_definition_is_open("func f(x = if true {\n  1\n}\n"));
        assert!(func_definition_is_open(
            "func f(x = if true {\n  1\n} else {\n  2\n}\n"
        ));
        assert!(func_definition_is_open(
            "func f(x = if true {\n  1\n} else {\n  2\n})\n{\n"
        ));
        assert!(!func_definition_is_open(
            "func f(x = if true {\n  1\n} else {\n  2\n}) { puts $x }"
        ));
        // The malformed stray-brace header still quarantines to its matching `}`.
        assert!(func_definition_is_open("func f(x {\n  puts LEAK\n"));
        assert!(!func_definition_is_open("func f(x {\n  puts LEAK\n}\n"));
        // A mismatched closer *inside* a block default (which has an `=`) is
        // malformed and dispatches, unlike the stray-brace quarantine above.
        assert!(!func_definition_is_open("func f(x = if true { 1 ])\n"));
        assert!(!func_definition_is_open("func f(x = { 1 ])\n"));
    }

    #[test]
    fn func_definition_is_open_stops_at_the_first_body_close() {
        // Trailing text that reopens a brace does not keep the definition pending:
        // once the body's matching `}` is found, it is dispatched so the parser
        // reports the trailing-text error rather than swallowing later commands.
        assert!(!func_definition_is_open("func f() {} {\n"));
        assert!(!func_definition_is_open("func f() { puts hi } extra {\n"));
    }

    #[test]
    fn a_cwd_url_leaves_the_unreserved_set_alone() {
        assert_eq!(
            cwd_url(b"host", &PathBuf::from("/home/user")),
            "file://host/home/user"
        );
        // `-`, `.`, `_` and `~` are unreserved, and `/` is the path's structure:
        // encoding any of them is legal but turns a readable URL into noise.
        assert_eq!(
            cwd_url(b"host", &PathBuf::from("/a-b/c.d/e_f/~g")),
            "file://host/a-b/c.d/e_f/~g"
        );
        // An empty host is a valid `file:` URL, and the honest answer when the
        // system will not say what it is called.
        assert_eq!(cwd_url(b"", &PathBuf::from("/")), "file:///");
    }

    #[test]
    fn a_cwd_url_encodes_everything_a_reader_could_misread() {
        // A space would end the URL, `%` would start an escape that is not one,
        // and `#` and `?` would begin a fragment or a query — all of which a
        // directory name is allowed to contain.
        assert_eq!(
            cwd_url(b"host", &PathBuf::from("/tmp/two words")),
            "file://host/tmp/two%20words"
        );
        assert_eq!(
            cwd_url(b"host", &PathBuf::from("/50%/a#b?c")),
            "file://host/50%25/a%23b%3Fc"
        );
        assert_eq!(
            cwd_url(b"host", &PathBuf::from("/tmp/café")),
            "file://host/tmp/caf%C3%A9"
        );
    }

    #[test]
    fn a_cwd_url_encodes_a_path_that_is_not_utf8() {
        // A directory whose name is not text is still a directory to `cd` into,
        // so the bytes are encoded as they are rather than replaced on the way
        // through a `String`.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfe"));
        assert_eq!(cwd_url(b"host", &path), "file://host/tmp/%FF%FE");
    }
}
