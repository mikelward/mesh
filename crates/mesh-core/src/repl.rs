//! The read / tokenize / dispatch loop.
//!
//! Interactive (TTY) input goes through [`reedline`] for line editing, history,
//! and Ctrl-C/Ctrl-D handling. Piped / non-interactive input keeps the std-only
//! unbuffered fd-0 byte reader, so a spawned child still inherits any bytes that
//! follow its command line and the integration tests need no terminal.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::env;
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
    Color, ColumnarMenu, Completer, EditCommand, Emacs, Highlighter, History, HistoryItem,
    HistoryItemId, HistorySessionId, KeyCode, KeyModifiers, Keybindings, MenuBuilder,
    Osc133Markers, Osc633Markers, Prompt, PromptEditMode, PromptHistorySearch, PromptKind,
    Reedline, ReedlineEvent, ReedlineMenu, SearchDirection, SearchQuery, SemanticPromptMarkers,
    Signal, SimpleMatchHighlighter, Span, SqliteBackedHistory, StyledText, Suggestion,
    default_emacs_keybindings,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::builtins::{self, Builtin, Multiplexer, NOTIFY_LIMIT};
use crate::completion::{CompletionCache, CompletionSpec, ValueHint, man_pages, rank_candidates};
use crate::expand::{Piece, VarRef, Word};
use crate::funcs::{FuncDef, Funcs};
use crate::options::{Opt, Options};
use crate::vars::{self, Decoration, RegexValue, Style, StyledValue, Value, Vars};
use crate::{environ, exec, expand, parser, whence};

const COMPLETION_MENU: &str = "completion_menu";

/// The mutable shell session threaded through the run loop: variable scopes,
/// defined functions, and the job table.
struct Shell {
    vars: Vars,
    funcs: Funcs,
    jobs: exec::JobTable,
    control: Option<parser::ControlKind>,
    /// The exit status the innermost **value call** unwound with, when it left
    /// through `return`/`fail`. A value call reads the value channel, so the
    /// status would otherwise be lost — and `f(…):capture` exists precisely to
    /// report every channel, `fail 7`'s `7` included. `None` when the body fell
    /// off its end, where the status is the ordinary "last command" one.
    value_call_status: Option<u8>,
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
    /// Has this session written a window title? The clear on the way out is owed
    /// to any title mesh put there, so this is remembered rather than re-derived
    /// from `$sh.options.osc-title`, which may have been turned off since — see
    /// [`set_title`].
    title_written: bool,
    /// Inside a `precd` / `postcd` handler. A handler may `cd` — the design
    /// allows it — but that move must not dispatch the hooks again, or
    /// `$sh.postcd.track = func(from) { cd $from }` would recurse until the
    /// stack ran out.
    in_cd_hooks: bool,
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
    hooks: Vec<Hook>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookEvent {
    PrePrompt,
    PreExec,
    PostExec,
    PreCd,
    PostCd,
    JobDone,
    Exit,
}

impl HookEvent {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "preprompt" => Some(Self::PrePrompt),
            "preexec" => Some(Self::PreExec),
            "postexec" => Some(Self::PostExec),
            "precd" => Some(Self::PreCd),
            "postcd" => Some(Self::PostCd),
            "jobdone" => Some(Self::JobDone),
            "exit" => Some(Self::Exit),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hook {
    event: HookEvent,
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
            value_call_status: None,
            forked: false,
            loop_depth: 0,
            prompt: PromptConfig::default(),
            result: Value::String(String::new()),
            produced: Produced::Status,
            status_records: 0,
            title_written: false,
            in_cd_hooks: false,
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
    // Before anything can recurse. Turns running off the end of the stack from an
    // abort into a diagnostic; see [`crate::stack`].
    crate::stack::install_fault_reporting();
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
        Invocation::Command(text) if options.no_execute => check_syntax(text, &options),
        Invocation::Command(text) => run_batch(&text.clone(), &options),
        Invocation::Script(path) => match read_script(path) {
            Ok(text) if options.no_execute => check_syntax(&text, &options),
            Ok(text) => run_batch(&text, &options),
            Err(code) => ExitCode::from(code),
        },
        Invocation::Stdin | Invocation::Default if options.no_execute => match read_all_stdin() {
            Ok(text) => check_syntax(&text, &options),
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

/// `-n` — parse the input, report the first thing wrong with it, and run nothing.
///
/// Startup files are **skipped**, deliberately. `env.mesh` is ordinary mesh code,
/// so sourcing it to check an unrelated file would run arbitrary commands — which
/// is the one thing this flag promises not to do. It also means the check answers
/// for the named input alone rather than for the reader's environment.
///
/// Silent on success, so it composes: `mesh -n generated.mesh && source …`.
fn check_syntax(text: &str, options: &StartupOptions) -> ExitCode {
    let mut shell = Shell::new();
    // For the diagnostic's name only — nothing here runs, so the rest of the
    // session state is never consulted.
    let (origin, source) = options.origin(false);
    shell.vars.set_origin(origin, source);
    match parser::parse(text) {
        // A heredoc body is opaque to the parser — it is data, delimited by a
        // line — so `Complete` does not mean its interpolation is well-formed.
        // Without this, `-n` passed a file whose `${bad` rejects it on the way in,
        // *after* every statement before it has run, which is the one thing
        // `mesh -n f && source f` exists to prevent.
        Ok(parser::ParseOutcome::Complete(_)) => match check_heredoc_bodies(text, &shell) {
            Ok(()) => ExitCode::SUCCESS,
            Err(report) => {
                note!("mesh: {report}");
                ExitCode::from(2)
            }
        },
        // A whole input that is still open *is* a syntax error, the same reading
        // `run_line` takes: nothing more is coming.
        Ok(parser::ParseOutcome::Incomplete(error)) => {
            note!("mesh: {}{error}", located(text, error.span.start, &shell));
            ExitCode::from(2)
        }
        Ok(parser::ParseOutcome::IncompleteHeredoc(delimiter)) => {
            note!("mesh: syntax error: heredoc missing its `{delimiter}` delimiter");
            ExitCode::from(2)
        }
        Err(error) => {
            note!("mesh: {}{error}", located(text, error.span.start, &shell));
            ExitCode::from(2)
        }
    }
}

/// Check every interpolated heredoc body in the input, which the parser passed
/// over. Bodies are tokens, so this needs the token stream rather than a walk of
/// the tree, and each one carries the span that locates it.
fn check_heredoc_bodies(text: &str, shell: &Shell) -> Result<(), String> {
    // Already parsed once by the caller, so this cannot fail; the second pass
    // buys the spans without threading tokens back out of `parse`.
    let Ok(tokens) = parser::tokenize(text) else {
        return Ok(());
    };
    for token in tokens {
        let parser::TokenKind::HeredocBody(body) = &token.value else {
            continue;
        };
        // A quoted delimiter takes no interpolation at all, so its body is data
        // and there is nothing to be wrong with.
        if body.raw {
            continue;
        }
        if let Err(message) = interpolate_heredoc(&body.text, None) {
            // Located at the body's first line. The scan reports offsets within
            // the body, which would need threading out to point at the exact
            // character; naming the heredoc is enough to find it.
            return Err(format!(
                "{}{message}",
                located(text, token.span.start, shell)
            ));
        }
    }
    Ok(())
}

/// Read all of stdin, for the checks that take their input whole.
fn read_all_stdin() -> Result<String, u8> {
    let mut text = String::new();
    match io::Read::read_to_string(&mut io::stdin(), &mut text) {
        Ok(_) => Ok(text),
        Err(error) => {
            note!("mesh: stdin: {error}");
            Err(1)
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
    // `-i` makes this an interactive session without changing where the commands
    // come from, so `$sh.interactive` and the `rc.mesh` in the startup set follow
    // the flag while the origin stays `script` / `command` — nothing here is typed
    // at a prompt.
    shell.vars.set_interactive(options.interactive);
    let (origin, source) = options.origin(false);
    shell.vars.set_origin(origin, source);
    let last = match run_startup_files(options, options.interactive, 0, &mut shell) {
        Step::Continue(code) | Step::Error(code) => code,
        Step::Exit(code) => return ExitCode::from(run_logout(options, code, &mut shell)),
        Step::Return(_, code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
    };
    let code = match run_line(text, last, false, &mut shell) {
        Step::Continue(code) | Step::Error(code) | Step::Exit(code) => code,
        Step::Return(..) => unreachable!("top-level return handled in run_line"),
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
    /// `-i` — this is an interactive session whatever stdin is, so `rc.mesh` is
    /// sourced and `$sh.interactive` is true. Orthogonal to where the commands
    /// come from (`DESIGN.md` §Invocation): `mesh -i script.mesh` is a script
    /// *and* interactive, which is what makes a config's interactive half
    /// testable without a pty.
    interactive: bool,
    /// `-n` — parse the input and report what is wrong with it, running nothing.
    /// A syntax check, for a config that *generates* mesh source and today can
    /// only find out whether it is valid by sourcing it into a live shell.
    no_execute: bool,
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
    /// interactive and a script. `typed` is the narrower question of whether a bare
    /// invocation's commands are being **typed at a prompt** or read from a pipe,
    /// which is the one place the invocation alone cannot say. Only the interactive
    /// loop answers it yes; `-i` does not, because a piped `mesh -i` is an
    /// interactive session whose commands still came from stdin.
    fn origin(&self, typed: bool) -> (vars::Origin, String) {
        match &self.invocation {
            Invocation::Script(path) => (vars::Origin::Script, path.to_string_lossy().into_owned()),
            Invocation::Command(_) => (vars::Origin::Command, String::new()),
            Invocation::Stdin => (vars::Origin::Stdin, String::new()),
            Invocation::Default | Invocation::Print(_) => (
                if typed {
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
            interactive: false,
            no_execute: false,
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
  -i                   Interactive session whatever stdin is (sources rc.mesh)
  -n, --no-execute     Check the input for syntax errors, run nothing
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
                "-i" => options.interactive = true,
                "-n" | "--no-execute" => options.no_execute = true,
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
                // `-s` says where the commands come from; it does not end option
                // parsing. Collecting the rest here made `mesh -s -n` swallow the
                // `-n` as an argument and run the input — against this parser's own
                // rule, which stops at the first *operand*, and against bash, which
                // honors both orderings. What follows the first operand is this
                // session's arguments rather than a script name; the operand arm
                // below knows that from the invocation.
                "-s" => options.invocation = Invocation::Stdin,
                // `--` ends option parsing without itself being an operand, so
                // a script whose name looks like an option can still be run.
                "--" => {
                    options.take_first_operand_onward(args);
                    return Ok(options);
                }
                _ if arg.starts_with('-') && arg != "-" => {
                    return Err(format!("unknown option `{arg}`"));
                }
                // The first operand is the script; everything after it is an
                // argument to that script, options included.
                _ => {
                    options.take_first_operand_onward(std::iter::once(arg).chain(args));
                    return Ok(options);
                }
            }
        }
        Ok(options)
    }

    /// Take the operands, which mean different things depending on where the
    /// commands come from. Under `-s` the input is already settled, so every one
    /// of them is an argument to this session; otherwise the first names a script.
    fn take_first_operand_onward(&mut self, operands: impl Iterator<Item = String>) {
        if self.invocation == Invocation::Stdin {
            self.args = operands.collect();
        } else {
            self.take_operands(operands);
        }
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
            return Step::Error(1);
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
/// `gets [VAR]` — read one line from stdin, strip its trailing newline, and bind
/// it to `VAR` (`DESIGN.md` §"Builtins").
///
/// **At end of input the status is `1` and `VAR` is left alone**, which is what
/// terminates `while gets line { … }`. An empty line is a successful read of `""`,
/// not an ending — a blank line in the middle of a file must not stop the loop —
/// so only a zero-byte read ends it.
///
/// The line is read a **byte at a time** rather than through a buffered reader:
/// a buffer would swallow input past the newline, and the bytes after this line
/// belong to whatever runs next — a `gets` in a loop, or an external command
/// sharing the same stdin.
///
/// Reading with no `VAR` consumes the line and reports whether there was one,
/// which is the "skip a line" spelling. The value form `gets()` — the one that
/// yields the line into an expression — is not wired up yet.
fn gets(args: &[String], shell: &mut Shell) -> Step {
    let name = match args {
        [] => None,
        [name] => Some(name.as_str()),
        _ => {
            note!("mesh: gets: takes at most one variable name");
            return Step::Error(2);
        }
    };
    // Checked before a byte is read, so a rejected operand does not also consume
    // the line: an error that swallowed input would leave the caller no way to
    // retry. `env` and `sh` are refused for the same reason every binding path
    // refuses them — resolution always reads those names as the namespaces, so a
    // binding there is one that can never be read back.
    if let Some(name) = name {
        if !parser::valid_name(name) {
            note!("mesh: gets: `{name}` is not a variable name");
            return Step::Error(2);
        }
        if vars::is_reserved_namespace(name) {
            note!("mesh: gets: `{name}` is a reserved namespace");
            return Step::Error(2);
        }
    }
    let mut line = Vec::new();
    // Descriptor 0 directly, not `io::stdin()`: that handle buffers, so it would
    // read past the newline and the bytes it swallowed would never reach whatever
    // runs next — the following `gets`, or an external command sharing this stdin.
    // `ManuallyDrop` because the descriptor is the shell's, not ours to close.
    // The same reasoning, and the same reader, as the interactive input loop.
    //
    // Wrapped so Ctrl-C can cancel it: an interactive shell ignores SIGINT while a
    // foreground *job* holds the terminal, but this blocks in the shell's own
    // process, where there is no job to receive the keystroke. Left ignored, Ctrl-C
    // did nothing here and the next line typed was swallowed as this read's input.
    let (read, interrupted) = exec::interruptible(|| {
        let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
        read_line(&mut *stdin, &mut line, false)
    });
    if interrupted {
        // The status any interrupted foreground command reports, and `var` keeps
        // whatever it held — a cancelled read has read nothing. The newline is
        // what the terminal's own `^C` echo lacks, so the next prompt starts on a
        // line of its own.
        note!("");
        return Step::Continue(130);
    }
    // Status 1 is reserved for end of input, so an I/O failure reports 2 like the
    // operand errors above. Sharing 1 would let a real read error end a
    // `while gets line` as though the input had simply run out.
    if let Err(err) = read {
        note!("mesh: gets: {err}");
        return Step::Error(2);
    }
    // Only a zero-byte read is the end. A file's last line without a trailing
    // newline is still a line, and an empty line is a successful read of `""` —
    // a blank line in the middle of a file must not stop a `while gets line`.
    if line.is_empty() {
        return Step::Continue(1);
    }
    // The line came off the same descriptor a piped session reads its commands
    // from, so it is a line of that input even though the reader never saw it.
    // Without this, `gets x` followed by its data leaves every later diagnostic
    // naming a line too far up the stream.
    //
    // Unless a redirection has fd 0 pointed somewhere else — `gets x < file`, or
    // `gets x << END`, whose body the reader already counted as part of this
    // unit. Reading a file is not reading the session, and counting the heredoc
    // twice is worse than not counting it at all.
    if exec::stdin_is_the_shells() {
        shell.vars.count_stdin_line();
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    // **Strict**, not lossy. `gets` reads data in, so it follows the capture —
    // which refuses a stream that is not UTF-8 — rather than `$env`, whose lossy
    // read renders a table the shell was handed and cannot refuse. Replacing the
    // bad bytes with U+FFFD here would hand back corrupted text and call it
    // success, and the corruption would outlive any chance of noticing it.
    let text = match String::from_utf8(line) {
        Ok(text) => text,
        Err(_) => {
            note!("mesh: gets: line is not valid UTF-8");
            return Step::Error(2);
        }
    };
    if let Some(name) = name {
        shell.vars.set_value(name, Value::String(text));
    }
    Step::Continue(0)
}

fn source_file(args: &[String], last: u8, shell: &mut Shell) -> Step {
    let [path] = args else {
        let message = if args.is_empty() {
            "source: needs a file to run"
        } else {
            "source: takes exactly one file; arguments for a sourced file are not \
             supported yet"
        };
        note!("mesh: {message}");
        return Step::Error(2);
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
        Step::Return(_, code) => Step::Continue(code),
        step => step,
    };
    // `source` is a **status-producing command**, so whatever the file's last
    // statement produced stops here. Without this the file's value carries out
    // through `run_recorded`, and `func f() { source lib.mesh }` returns whatever
    // `lib.mesh` happened to end with — an integer, a list — where every other
    // command yields its status. Set on both paths, since a startup file is run
    // outside `run_recorded` and would otherwise leave the value behind.
    if let Step::Continue(code) | Step::Error(code) = step {
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
            // A startup file that ends on an evaluation error has already reported
            // it, and it is no more a reason to skip the remaining files than a
            // failing command in it is: `rc.mesh` still runs after a typo in
            // `env.mesh`. Only `exit` stops the sequence.
            Step::Continue(code) | Step::Error(code) => last = code,
            flow => return flow,
        }
    }
    Step::Continue(last)
}

fn run_logout(options: &StartupOptions, last: u8, shell: &mut Shell) -> u8 {
    // Anything the exiting command line itself reported. `jobs; exit` reaps the
    // job, prints its `[N] Done`, and then leaves before the loop comes round to
    // drain — so the hook has to run on the way out or never, there being no
    // next prompt to defer it to.
    //
    // Here for the same reason the title is cleared here: every exit path
    // arrives at this function. Draining where the *loop* happens to look meant
    // enumerating the moments a job can be noticed, and that list was wrong four
    // times running — `jobs`, `fg`, a `preprompt` handler, and now `exit`.
    //
    // Reaped first, and not only drained: a job that ended while the shell was
    // waiting for the line that turned out to be `exit` has been noticed by
    // nobody, so there is nothing queued to drain. `exit` is a builtin and forks
    // nothing, so no wait ran on the way past. Without this it leaves with
    // neither its `[N] Done` nor its hook — the shell saw it finish and said
    // nothing at all.
    //
    // Only when interactive, because reaping is what *prints* the notice, and a
    // script that backgrounds something has never been told about it: every
    // report so far comes from the prompt loop, which a script does not have.
    // Reaping here regardless put `[1] Done (0) …` on the stderr of any script
    // that left a job running, which two tests caught and which is not this
    // change's business.
    if shell.vars.owns_terminal() {
        shell.jobs.reap();
    }
    run_jobdone_hooks(shell);
    if options.login
        && let Some(path) = config_dir().map(|dir| dir.join("logout.mesh"))
    {
        let _ = run_config_file(&path, last, shell);
    }
    // Again, because `logout.mesh` is a script like any other and can report a
    // job itself. Cheap when nothing is queued, and it makes the guarantee whole
    // rather than true of every case someone thought to list.
    run_jobdone_hooks(shell);
    // Clear the title on the way out, and last, so mesh has the final word over a
    // `logout.mesh` that writes one. Every exit path arrives here, which is what
    // makes this the place: `exit` would otherwise leave the window named after
    // that command forever, and Ctrl-D would leave it naming the directory of a
    // shell that is gone. An empty title is the reset every terminal understands.
    //
    // Gated on having written one rather than on `$sh.options.osc-title`, so
    // turning the setting off mid-session still cleans up: the debt is from when
    // it was on. `true` rather than the setting for the same reason.
    if shell.title_written {
        set_title(true, "");
    }
    last
}

/// What to do after handling one input line.
#[derive(Debug, PartialEq)]
enum Step {
    /// A line ran; carry this status as the new "last status".
    Continue(u8),
    /// An **evaluation error** — the program was invalid (an unbound name, a
    /// modifier applied to the wrong type, a bad argument), already reported.
    ///
    /// Distinct from `Continue` with a nonzero status, which is a command that ran
    /// and *answered*: `diff` saying 1 for "they differ" is a result the statement
    /// carries on with, while an invalid program is not. Both leave the same status
    /// behind and both let the *next* statement run, so almost everywhere the two
    /// behave alike — but a `$(…)` has to tell them apart, since it yields the
    /// captured bytes for a status and must not for an error. The distinction lives
    /// in the type rather than in a flag beside it so that a site which has to
    /// choose cannot be written without choosing.
    Error(u8),
    /// `exit` was invoked; leave the shell with this status.
    Exit(u8),
    /// `return` (or `fail`) was invoked; unwind the current function carrying its
    /// result value **and**, separately, the exit status to leave behind. At top
    /// level (no function) `run_line` reports it as a recoverable error instead.
    ///
    /// The two travel together rather than one being derived from the other
    /// because they are independent channels: `return 5` yields the *integer* five
    /// with a status of `0`, and `fail 5` yields no value with a status of `5`.
    Return(Value, u8),
}

impl Step {
    /// The exit status this step contributes as the new "last status".
    fn status(&self) -> u8 {
        match self {
            Step::Continue(code) | Step::Error(code) | Step::Exit(code) | Step::Return(_, code) => {
                *code
            }
        }
    }
}

/// The exit status a **returned value** leaves behind.
///
/// Only `false` fails. `false` is mesh's "no result" — what `gets()` yields at
/// EOF and what a failing predicate returns — so it is the one value whose
/// absence of a result is worth reporting as a nonzero status. Every other value
/// is a result, and producing a result is success, so `return 5` carries the
/// integer five with status `0` rather than claiming exit code 5. Naming a
/// specific code is [`fail`](make_fail)'s job.
fn status_of(value: &Value) -> u8 {
    u8::from(matches!(value, Value::Boolean(false)))
}

/// Where a syntax error is, as the `file:line:column: ` prefix a diagnostic wears.
///
/// The name is `$sh.source` where there is a file — a script or a sourced file,
/// which is the case that matters, since locating a syntax error in a long config
/// otherwise means bisecting it. For the file-less origins it is the origin word
/// (`stdin`, `command`, `interactive`), so the line number still has something to
/// hang off and the reader can tell which input is meant.
fn located(text: &str, offset: usize, shell: &Shell) -> String {
    let (line, column) = parser::line_and_column(text, offset);
    let line = line + shell.vars.input_line_offset();
    let source = shell.vars.input_source();
    let name = if source.is_empty() {
        shell.vars.input_origin().to_owned()
    } else {
        source
    };
    format!("{name}:{line}:{column}: ")
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
        Step::Error(2)
    };
    let step = match parser::parse(text) {
        Ok(parser::ParseOutcome::Complete(source)) => run_source(&source, last, in_function, shell),
        // Nothing more is coming — this text *is* the unit — so what a line reader
        // would buffer is a syntax error here, reported where the parser gave up
        // rather than as a bare "unexpected end of input" the reader has to bisect
        // a file to locate.
        Ok(parser::ParseOutcome::Incomplete(error)) => {
            note!("mesh: {}{error}", located(text, error.span.start, shell));
            reject(shell)
        }
        Ok(parser::ParseOutcome::IncompleteHeredoc(delimiter)) => {
            note!("mesh: syntax error: heredoc missing its `{delimiter}` delimiter");
            reject(shell)
        }
        Err(error) => {
            note!("mesh: {}{error}", located(text, error.span.start, shell));
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
    // Whether the statement that just ran was an evaluation error. An error does
    // **not** stop the body — mesh reports it and runs the next statement, which is
    // what `puts "a[$nope]b"` followed by `puts after` has always done — so this
    // only records how the body *ended*, which is what a `$(…)` around it needs in
    // order to tell an invalid program from a command's status.
    let mut errored = false;
    for statement in &source.statements {
        let step = run_statement(statement, status, in_function, shell);
        // A statement a guard skipped **ran nothing**, so it neither carries a
        // status of its own nor says anything about the one before it. Clearing
        // `errored` on one would let `$(puts $nope; puts skipped if false)` lose
        // the error and hand back the empty output as an answer.
        let executed = shell.produced != Produced::Nothing;
        match step {
            Step::Continue(code) => {
                status = code;
                if executed {
                    errored = false;
                }
            }
            Step::Error(code) => {
                status = code;
                errored = true;
            }
            flow => return flow,
        }
        if executed {
            produced = shell.produced;
        }
        if shell.control.is_some() {
            break;
        }
    }
    shell.produced = produced;
    sequenced(status, errored)
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
        return Step::Error(2);
    }
    let mut step = run_recorded(&node.first, background, last, in_function, shell);
    for (op, executable) in &node.rest {
        // An evaluation error chains like any other failure: it left a status, so
        // `h() || fallback` runs the fallback rather than abandoning the list. Only
        // `exit` and `return` are control flow that skips the rest.
        let (Step::Continue(status) | Step::Error(status)) = step else {
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
    if let Step::Continue(code) | Step::Error(code) = step
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
    if let Step::Continue(code) | Step::Error(code) = step
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
        return Step::Error(2);
    }
    match node {
        Pipeline(pipeline) => run_ast_pipeline(pipeline, background, last, in_function, shell),
        Assignment {
            pattern,
            append,
            value,
            global,
        } => {
            let evaluated = eval_operand_of(value, last, in_function, shell);
            match evaluated {
                // A right-hand side that raised `break`/`continue` produced no value to
                // bind — the loop is unwinding, so leave the target as it was rather
                // than overwriting it with the placeholder.
                Ok(_) if shell.control.is_some() => Step::Continue(last),
                Ok(bound) => {
                    let result = if *append {
                        let parser::BindingPattern::Name(name) = pattern else {
                            unreachable!("the parser restricts += to names")
                        };
                        if *global {
                            shell.vars.append_global(name, bound)
                        } else {
                            shell.vars.append(name, bound)
                        }
                    } else {
                        bind_pattern(pattern, &bound, &mut shell.vars, *global)
                    };
                    // A right-hand side that *is* a capture lends the statement its
                    // status, so `if out = $(diff a b)` branches on the diff rather
                    // than on the binding having worked — which is always true here.
                    let captured = capture_status_of(value, shell);
                    result.map_or_else(runtime_message, |_| Step::Continue(captured))
                }
                Err(step) => step,
            }
        }
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
            let evaluated = eval_operand_of(value, last, in_function, shell);
            match evaluated {
                // As for an ordinary assignment: a right-hand side that raised
                // `break`/`continue` produced no value, so leave the variable
                // alone.
                Ok(_) if shell.control.is_some() => Step::Continue(last),
                Ok(evaluated_value) => {
                    let captured = capture_status_of(value, shell);
                    environ::write(key, evaluated_value, *append)
                        .map_or_else(runtime_message, |_| Step::Continue(captured))
                }
                Err(step) => step,
            }
        }
        MemberAssignment {
            target,
            append,
            value,
            global,
        } => {
            let evaluated = eval_operand_of(value, last, in_function, shell);
            match evaluated {
                // As for an ordinary assignment: a right-hand side that raised
                // `break`/`continue` produced no value, so leave the place alone.
                Ok(_) if shell.control.is_some() => Step::Continue(last),
                Ok(evaluated_value) => {
                    let captured = capture_status_of(value, shell);
                    assign_into_member(target, evaluated_value, *append, *global, shell)
                        .map_or_else(runtime_message, |()| Step::Continue(captured))
                }
                Err(step) => step,
            }
        }
        Function {
            name,
            parameters,
            body,
            wrapper,
        } => {
            if matches!(name.as_str(), "func" | "return" | "fail" | "not")
                || builtins::is_builtin(name)
            {
                note!("mesh: func: `{name}` is a reserved name and cannot be a function name");
                return Step::Error(2);
            }
            // Parameter names are already validated (distinct, not `env`) by the
            // parser's `parameters()`.
            shell.funcs.define(
                name.clone(),
                FuncDef {
                    params: parameters.clone(),
                    body: body.clone(),
                    wrapper: *wrapper,
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
        With { bindings, body } => run_with_block(bindings, body, last, in_function, shell),
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
                        return Step::Error(1);
                    }
                    // `return val` unwinds carrying the value, and succeeds unless
                    // the value is `false`; a bare `return` carries the result so
                    // far with the **last status**, so it reads as "stop here, as
                    // if the body ended at this line" (`DESIGN.md`).
                    match value
                        .as_ref()
                        .map(|v| eval_expr(v, last, in_function, shell))
                        .transpose()
                    {
                        Ok(Some(value)) => {
                            let code = status_of(&value);
                            Step::Return(value, code)
                        }
                        Ok(None) => Step::Return(shell.result.clone(), last),
                        Err(step) => step,
                    }
                }
                parser::ControlKind::Fail => {
                    if !in_function && !shell.vars.in_sourced_file() {
                        note!("mesh: fail: not inside a function or sourced file");
                        return Step::Error(1);
                    }
                    match value
                        .as_ref()
                        .map(|v| eval_expr(v, last, in_function, shell))
                        .transpose()
                    {
                        Ok(operand) => make_fail(operand.into_iter().map(|v| (v, true)).collect()),
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
                        Step::Error(1)
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
            // A lone quoted scalar used to run here, the spelling that reached a
            // program whose path needs quoting (`"/opt/my program"`). It is a string
            // literal now: quoting makes a value, and `command -- "…"` is how a path
            // like that is run (`DESIGN.md` §"Bare words and quoted values").
            //
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
        With { .. } => "a `with` block",
        Control { kind, .. } => match kind {
            parser::ControlKind::Return => "`return`",
            parser::ControlKind::Fail => "`fail`",
            parser::ControlKind::Break => "`break`",
            parser::ControlKind::Continue => "`continue`",
        },
    })
}

/// Can this stage hand its words to its own fork to expand?
///
/// Only when it has a value to gain by it, and **only when it has no redirections**.
/// A redirection is the shell's to resolve: it does that for every stage at once,
/// before it forks any of them, which is what keeps `cat < fifo | cmd > fifo` from
/// deadlocking. Deferring the words but not the targets would put the targets
/// *first* — reversing the documented order, so a failing target would stop the
/// words from ever running and `f * > summary` would glob the `summary` its own
/// redirection had just created.
///
/// So a stage with both keeps the old behavior: the parent expands its words, in
/// word order, before its targets. It gives up the isolation the deferral buys,
/// which is why backgrounding one stays refused rather than silently running in the
/// wrong process.
fn can_defer(words: &[parser::Word], redirs: &[Redir]) -> bool {
    redirs.is_empty() && words.iter().any(word_carries_a_value)
}

/// Does this word carry a value — `puts $(pwd)`, `puts "$(pwd)"`, `cmd f()`?
///
/// Asked of a stage that is about to **fork**, since a value is the one thing in a
/// word whose expansion runs code, and running it here would run it in the wrong
/// process. Everything else in a word — a variable, a glob, a tilde — is a pure
/// read, so where it happens cannot be observed.
fn word_carries_a_value(word: &parser::Word) -> bool {
    word.pieces
        .iter()
        .any(|piece| matches!(piece, parser::WordPiece::Value { .. }))
}

/// What a job listing shows for a stage that has not expanded its words yet.
///
/// A value has no text until it is evaluated, and evaluating it to *print* it would
/// be the very thing the deferral avoids — so it shows as `$(…)`, the shape of what
/// was written. The words around it are their own spelling, so `puts $(pwd) &` lists
/// as `puts $(…)`.
fn display_words(words: &[parser::Word]) -> Vec<String> {
    words
        .iter()
        .map(|word| {
            let mut text = String::new();
            for piece in &word.pieces {
                match piece {
                    parser::WordPiece::Text { text: piece, .. } => text.push_str(piece),
                    parser::WordPiece::Variable { name, .. } => text.push_str(name),
                    parser::WordPiece::Value { .. } => text.push_str("$(…)"),
                }
            }
            text
        })
        .collect()
}

/// The words a **deferred** stage is registered and listed under: its display
/// words, with a literal `command` prefix taken off.
///
/// The job is the program, so the table has to name the program — that is what
/// `jobs` shows and what `%prefix` matches, and `wait %sleep` must find
/// `command sleep $(…) &` exactly as it finds `command sleep 0.2 &`. The eager
/// paths get this from [`external_stage`], which strips before the stage is built;
/// a deferred stage is registered *before* its words are expanded, so the same cut
/// has to be made on the spelling.
///
/// Only a literally written prefix is taken: these are unexpanded words, and a
/// first word that is anything but the bare text `command` is left alone — as is a
/// `command` line with no program in it, which is about to report an error rather
/// than run anything.
fn deferred_words(words: &[parser::Word]) -> Vec<String> {
    let shown = display_words(words);
    match shown.split_first() {
        Some((first, rest)) if first == "command" => match command_line(rest) {
            CommandLine::External(program) => program,
            _ => shown,
        },
        _ => shown,
    }
}

/// Does this command hold a value the **parent** has to evaluate, whatever happens
/// next — so that backgrounding it would run the work at the prompt?
///
/// Only when it redirects. A stage with no redirections hands its words to its own
/// fork ([`can_defer`]); one that redirects cannot, because the shell resolves every
/// stage's targets before it forks any of them — in parallel, which is what keeps
/// `cat < fifo | cmd > fifo` from deadlocking. So a value anywhere in a redirecting
/// command is the parent's, and `&` on it is refused rather than silently done in
/// the wrong process.
///
/// A heredoc body is not checked because it does not interpolate a capture at all
/// (`TODO.md`).
fn carries_a_value(items: &[parser::CommandItem]) -> bool {
    let redirects = items
        .iter()
        .any(|item| matches!(item, parser::CommandItem::Redirect { .. }));
    redirects
        && items.iter().any(|item| match item {
            parser::CommandItem::Value(_) => true,
            parser::CommandItem::Word(word) => word_carries_a_value(&word.value),
            parser::CommandItem::Redirect { target, .. } => word_carries_a_value(&target.value),
        })
}

/// Does this one-word body name a **command**?
///
/// Inside braces a bare word is a command, exactly as it is anywhere else a
/// statement is read: `{ pwd }` runs `pwd` rather than yielding the string
/// `"pwd"`. Two bare spellings escape, because neither can name a command — an
/// integer literal and `true`/`false`, which is what keeps `func answer() { 42 }`
/// the integer and `{ false }` the boolean. A **quoted** word is a string literal
/// and never a command (`DESIGN.md` §"Bare words and quoted values"), so it is not
/// one here either.
///
/// This is the whole of the bare-vs-quoted rule for a block tail. Anything that is
/// not a single bare `Text` piece — an interpolation, a computed value — keeps the
/// reading it has elsewhere and is not claimed here.
fn bare_word_names_a_command(word: &parser::Word) -> bool {
    matches!(
        word.pieces.as_slice(),
        [
            parser::WordPiece::Text {
                text,
                quote: parser::QuoteMode::Bare,
            },
        ] if matches!(expand::typed_scalar(text), Value::String(_))
    )
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
        Some(guard) => {
            let value = eval_operand_of(&guard.condition, last, in_function, shell)?;
            if shell.control.is_some() {
                return Ok(false);
            }
            let truth = condition_bool(&value).map_err(runtime_message)?;
            Ok(truth != guard.unless)
        }
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
/// How a body that ran to its end reports itself.
///
/// Every boundary that *sequences* statements — a source, a loop, a function body
/// — keeps going after an evaluation error and answers for how the last statement
/// that executed ended. The two channels only ever separate at `$(…)`, so this is
/// the one place either arm is chosen.
fn sequenced(status: u8, errored: bool) -> Step {
    if errored {
        Step::Error(status)
    } else {
        Step::Continue(status)
    }
}

fn compound_result(step: Step, shell: &mut Shell) -> Step {
    if matches!(step, Step::Continue(_) | Step::Error(_)) && shell.produced == Produced::Nothing {
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
                Ok(value) => match condition_bool(&value) {
                    Ok(true) => {}
                    Ok(false) => {
                        shell.vars.restore_active(snapshot);
                        continue;
                    }
                    Err(message) => {
                        shell.vars.restore_active(snapshot);
                        return runtime_message(message);
                    }
                },
                Err(step) => return step,
            }
        }
        return match &arm.body {
            parser::MatchBody::Block(body) => {
                compound_result(run_source(body, 0, in_function, shell), shell)
            }
            // `=> value` is a value context: a bare word is a string, never a
            // command, so this deliberately skips the command dispatch an
            // expression *statement* would do. Like that statement, it reports the
            // status view of its value.
            parser::MatchBody::Value(expression) => {
                match eval_expr(expression, 0, in_function, shell) {
                    Ok(_) if shell.control.is_some() => {
                        shell.produced = Produced::Nothing;
                        Step::Continue(0)
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
        };
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
        // What this condition asks is whether the value has the requested shape
        // (`docs/REFERENCE.md` §Conditionals); a capture's status is no part of it.
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
        if shell.control.is_some() {
            return Ok(None);
        }
        let truth = condition_bool(&value).map_err(runtime_message)?;
        return Ok(Some(u8::from(!truth)));
    }
    // A command condition **is** a command that ran, so it publishes its status
    // like any other one, and the branch it picks can read it: `if cmd { … } else
    // { … }` sees the real code in the `else`, where bash's `$?` has it too. The
    // publishing normally happens in `run_recorded`, which a condition does not
    // pass through — the condition is not the statement's result, the branch is.
    //
    // Two conditions are exempt, for the same reason: nothing ran. A bool is not a
    // command and has no status to report, so `if $x == 1 { … }` leaves the
    // previous command's standing; and a condition its own trailing guard skipped
    // — `if cmd if false { … }` — never ran the command at all. Both are the rule
    // `docs/REFERENCE.md` §Guards already states, and `Produced::Nothing` is what
    // says which happened.
    let records = shell.status_records;
    shell.produced = Produced::Status;
    match run_executable(condition, false, last, in_function, shell) {
        Step::Continue(_) if shell.control.is_some() => Ok(None),
        Step::Continue(code) => {
            // Beyond "did it run", the same test `run_recorded` applies: a pipeline
            // already recorded its own per-stage breakdown, so leave it rather than
            // flattening it to one entry — `if a | b { puts ...$sh.pipestatus }`
            // still sees both stages.
            if shell.produced != Produced::Nothing
                && (shell.status_records == records || shell.vars.status() != code)
            {
                shell.record_status(code, vec![code]);
            }
            Ok(Some(code))
        }
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
        return Step::Error(2);
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
    let mut errored = false;
    shell.loop_depth += 1;
    for values in values {
        if let Err(message) = bind_iteration(bindings, values, shell) {
            shell.loop_depth -= 1;
            return runtime_message(message);
        }
        match run_source(body, 0, in_function, shell) {
            // An evaluation error says how *this pass* ended, not that the loop is
            // unwinding: the next pass runs exactly as it does after a failing
            // command. Only the classification is carried out, so a `$(for …)`
            // around the loop can still tell an invalid body from a bad status.
            Step::Continue(code) => {
                status = code;
                errored = false;
            }
            Step::Error(code) => {
                status = code;
                errored = true;
            }
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
            Some(parser::ControlKind::Return | parser::ControlKind::Fail) => unreachable!(),
            None => results.push(pass),
        }
    }
    shell.loop_depth -= 1;
    shell.result = Value::List(results);
    shell.produced = Produced::Value;
    sequenced(status, errored)
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
/// Run `body` with `bindings` applied to the environment, then put the environment
/// back the way it was.
///
/// The restore is the whole point, so it runs on **every** way out — a normal
/// finish, a runtime error, `exit`, and `return` / `break` / `continue` — which is
/// why the body's `Step` is held rather than propagated with `?`. Restoring means
/// the *previous* state, so a name that was unset before goes back to unset rather
/// than to an empty string: a child can tell those apart, and this construct exists
/// for what a child sees.
///
/// Bindings are applied left to right, so a later one wins on a repeated name, and
/// each is evaluated against the environment the ones before it left — which is what
/// makes `with PATH=/opt PATH+=/usr/bin { … }` mean what it reads like. The snapshot
/// is taken before any of them run, so the restore is still to the state the whole
/// header found.
fn run_with_block(
    bindings: &[parser::EnvBinding],
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    // Every name in the header is snapshotted **before any right-hand side runs**,
    // not as each binding is reached. A value expression can write the environment
    // itself — `with A=alter() B=inside { … }` where `alter` sets `$env.B` — and a
    // per-binding snapshot would then record what that write left rather than what
    // the header found, so the restore would leak it. Deduplicated, so a name bound
    // twice still goes back to the one state that predates the header.
    let mut saved: Vec<(&str, Option<std::ffi::OsString>)> = Vec::with_capacity(bindings.len());
    for binding in bindings {
        if !saved.iter().any(|(key, _)| *key == binding.key) {
            saved.push((&binding.key, std::env::var_os(&binding.key)));
        }
    }
    let mut failure = None;
    for binding in bindings {
        match eval_operand_of(&binding.value, last, in_function, shell) {
            // A right-hand side that raised `break`/`continue` produced no value, so
            // there is nothing to bind — and the control flow is the answer, as it is
            // for an ordinary assignment.
            Ok(_) if shell.control.is_some() => {
                failure = Some(Step::Continue(last));
                break;
            }
            Ok(value) => {
                if let Err(message) = environ::write(&binding.key, value, binding.append) {
                    failure = Some(runtime_message(message));
                    break;
                }
            }
            Err(step) => {
                failure = Some(step);
                break;
            }
        }
    }
    // Whatever happened, the header's writes are undone before this returns —
    // including the ones a later binding's failure left standing.
    // Seeded at 0 and passed through `compound_result`, as every other compound
    // body is. Seeding with `last` let an empty body — or one whose statements were
    // all guard-skipped — inherit the previous command's status, so
    // `false; with A=x { } || puts fallback` took the fallback over a header that
    // applied and restored cleanly. Raised in review as a P2.
    let step =
        failure.unwrap_or_else(|| compound_result(run_source(body, 0, in_function, shell), shell));
    for (key, previous) in saved {
        match previous {
            // SAFETY: single-threaded execution loop, as `environ::write` relies on.
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
    step
}

fn run_forked_block(body: &parser::Source, in_function: bool, shell: &mut Shell) -> Step {
    // A subshell is a status, never a value: nothing typed survives the process
    // boundary, so whatever the surrounding code had produced is not passed off
    // as this block's own.
    shell.produced = Produced::Status;
    // `owns_terminal`, not `interactive`: putting the child in its own process
    // group is only safe for a shell that took the terminal. A `mesh -i` batch
    // session never did, and a group of its own would be excluded from the
    // `SIGINT` sent to the invocation's — killing the shell and orphaning this.
    let status = exec::fork_and_wait(shell.vars.owns_terminal(), || {
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
            Step::Continue(code) | Step::Error(code) | Step::Exit(code) => code,
            // A `return` that reached the top of a subshell body has no caller
            // left inside it; its value's status is what the child exits with.
            Step::Return(_, code) => code,
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
            Step::Error(1)
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
    let mut errored = false;
    // Everything the last completed pass left behind. A `while` is the one
    // construct whose condition runs *after* its final pass, so the failing test
    // is the newest thing to have run — and the loop reports the *pass*, not the
    // test. Two ways that showed:
    //
    // - `run_recorded` cannot tell the test's status record from the body's when
    //   the codes coincide, so a body ending in `true | sh -c 'exit 1'` under a
    //   now-false condition reported `1 | 1` where the pass was `1 | 0 1`.
    // - the test produces a *status*, which displaces the pass's value, so
    //   `func f() { n = 0; while test $n -lt 1 { n = 1; 7 + 0 } }` answered `0`
    //   where the same loop under a value condition answers `7`.
    //
    // Both are the same mistake, so both are undone the same way: snapshot after
    // each pass, put it back on the way out.
    let mut pass_record = None;
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
            // As in `for`: an evaluation error ends the pass, not the loop.
            Step::Continue(code) => {
                status = code;
                previous = code;
                errored = false;
            }
            Step::Error(code) => {
                status = code;
                previous = code;
                errored = true;
            }
            flow => {
                shell.loop_depth -= 1;
                return flow;
            }
        }
        pass_record = Some((
            shell.vars.status_snapshot(),
            shell.result.clone(),
            shell.produced,
        ));
        match shell.control.take() {
            Some(parser::ControlKind::Break) => break,
            Some(parser::ControlKind::Continue) => continue,
            Some(parser::ControlKind::Return | parser::ControlKind::Fail) => {
                unreachable!("`return` and `fail` unwind as a Step")
            }
            None => {}
        }
    }
    shell.loop_depth -= 1;
    // Only when a pass actually ran: a loop whose condition was false from the
    // start reports its own 0 and produced nothing, which `run_recorded` and
    // `compound_result` answer for between them.
    if let Some((record, result, produced)) = pass_record {
        shell.vars.restore_status(record);
        shell.result = result;
        shell.produced = produced;
    }
    compound_result(sequenced(status, errored), shell)
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
    /// The stage's words, still **parsed**: a value in one is evaluated by the
    /// code that expands that word, in word order — see [`expand_stage`].
    words: Vec<parser::Word>,
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
    /// Still **parsed**, like a stage's words: a value in a target is evaluated by
    /// [`expand_redirs`], which runs after every word of the command.
    target: parser::Word,
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
    in_function: bool,
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
        // A value in a command that does not redirect travels to the stage and is
        // evaluated in its fork, so backgrounding one is ordinary now. One that
        // *does* redirect cannot travel: the shell resolves every stage's targets
        // before it forks, in parallel, which is what keeps `cat < fifo | cmd > fifo`
        // from deadlocking — see `can_defer`. Evaluating it here would run the work
        // in the parent, where `puts $(slow) > out &` hangs the prompt and a mutating
        // call reaches the parent's bindings that `docs/REFERENCE.md` promises the
        // fork keeps. Refused rather than silently done in the wrong process.
        if background && carries_a_value(&command.items) {
            note!(
                "mesh: a value cannot be backgrounded with a redirection yet; \
                 bind it first — `m = $(…)` then `cmd $m > out &`"
            );
            return Step::Error(2);
        }
        let mut words = Vec::new();
        let mut redirs = Vec::new();
        for item in &command.items {
            match item {
                parser::CommandItem::Word(word) => words.push(word.value.clone()),
                // A value argument **is** a word with a value in it, so it becomes
                // one: `puts (1 + 2)` and `puts "$(pwd)"` then travel as the same
                // thing and expand by the same rule. Evaluating it is the job of
                // whoever expands that word, which is what puts it in word order —
                // a call in one argument cannot change what an earlier word read.
                parser::CommandItem::Value(expression) => words.push(parser::Word {
                    pieces: vec![parser::WordPiece::Value {
                        expression: Box::new(expression.clone()),
                        quote: parser::QuoteMode::Bare,
                    }],
                    qualifiers: None,
                }),
                parser::CommandItem::Redirect {
                    kind,
                    fd,
                    target,
                    body,
                } => {
                    // Still parsed, like a command word: a value in a target is
                    // evaluated by `expand_redirs`, which runs after every word — so
                    // `puts $n > "$(g)"` reads the `$n` the reader sees, and
                    // `f * > summary` cannot glob the file it is about to create.
                    let target = target.value.clone();
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
                                return Step::Error(1);
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
    run_pipeline(stages, background, last, in_function, shell)
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
/// Expand a heredoc body, or — with no `vars` — merely check that it *could* be
/// expanded.
///
/// The checking half is what `-n` needs: the body's escapes and references have
/// to be well-formed for the file to be valid, but resolving them would need a
/// session, and an unbound variable is a runtime failure rather than a syntax
/// error. One walk rather than two, so the check cannot drift from the thing it
/// is checking.
fn interpolate_heredoc(text: &str, vars: Option<&Vars>) -> Result<String, String> {
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
                    end + parser::variable_access_prefix(&tail[..word]).map_err(|kind| {
                        format!("heredoc: {}", parser::ParseError { kind, span: 0..0 })
                    })?
                };
                // Checking stops at "the reference is well-formed"; only an
                // expansion resolves it.
                if let Some(vars) = vars {
                    let reference = expansion_variable(&text[i..end], parser::QuoteMode::Double);
                    out.push_str(&expand::resolve(&reference, vars).map_err(|e| e.to_string())?);
                }
                i = end;
                continue;
            }
        }
        out.push(c);
        i += c.len_utf8();
    }
    Ok(out)
}

/// Turn a parsed word into an expansion word, evaluating any **value** piece on
/// the way — `"at $(pwd) now"`.
///
/// Shell-aware because that evaluation has to be: a `$(…)` launches a command,
/// which the expander cannot do. Same division of labor as a value *argument*
/// (`parser::CommandItem::Value`), and the same place in the order — before the
/// word is expanded, and before any redirect target is opened.
fn expansion_word(
    word: &parser::Word,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Word, Step> {
    let mut pieces = Vec::with_capacity(word.pieces.len());
    for piece in &word.pieces {
        pieces.push(match piece {
            parser::WordPiece::Text { text, quote } => Piece::Text {
                text: text.clone(),
                expandable: matches!(quote, parser::QuoteMode::Bare),
            },
            parser::WordPiece::Variable { name, quote } => {
                Piece::Var(expansion_variable(name, *quote))
            }
            parser::WordPiece::Value { expression, quote } => {
                // Through `eval_operand_of`, which puts `shell.result` /
                // `shell.produced` back: a piece of a word is an *operand*, so what
                // it produced must not stand as the enclosing command's result.
                let value = eval_operand_of(&expression.value, last, in_function, shell)?;
                // Control flow is unwinding — a `return` inside the capture. Stop
                // rather than expand a word that was never finished; the statement
                // layer acts on `shell.control`.
                if shell.control.is_some() {
                    return Err(Step::Continue(last));
                }
                // Inside `"…"` the quotes say "make this text", so the same rule
                // `"$xs"` obeys applies here: a scalar renders, a collection is a
                // loud error. Without this a `"${f()}"` whose call returned a list
                // would smuggle the list out through a pair of quotes, and quoting
                // would have stopped meaning "one string".
                match quote {
                    parser::QuoteMode::Double => Piece::Value(interpolated_value(value)?),
                    _ => Piece::Value(value),
                }
            }
        });
    }
    Ok(Word {
        pieces,
        qualifiers: word.qualifiers.clone(),
    })
}

/// What a `${ … }` expression contributes to a **double-quoted** word: its text.
///
/// The rule is [`expand`]'s for `"$x"`, kept in step deliberately — a string or a
/// styled value is its text, an integer and a boolean render, and anything with no
/// single byte form is an error rather than a guessed separator or a lossy shape.
/// A caller wanting the elements spells the join (`:join(" ")`).
fn interpolated_value(value: Value) -> Result<Value, Step> {
    let kind = match value {
        Value::String(_) => return Ok(value),
        // Its *text*, not itself. Returning the styled value unchanged let it stay
        // styled when it was the word's only piece, so `x = "${style(…)}"` kept
        // attributes that `x = "$styled"` and `x = "pre${…}"` both drop — the same
        // leak through a pair of quotes the list arm above exists to close.
        Value::Styled(styled) => return Ok(Value::String(styled.text)),
        Value::Integer(n) => return Ok(Value::String(n.to_string())),
        Value::Boolean(b) => return Ok(Value::String(b.to_string())),
        Value::List(_) => "list",
        Value::Map(_) => "map",
        Value::Regex(_) => "pattern",
        Value::Glob(_) => "glob",
        Value::Stream(_) => "stream handle",
        Value::Job(_) => "job handle",
        Value::Function(_) => "function",
    };
    note!("mesh: a {kind} has no text form to interpolate into a string");
    Err(Step::Continue(1))
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
    if root == "sh" {
        return assign_into_shell(target, &steps, &value, append, global, &shell.vars);
    }
    // Through `Vars::update`, so a failed path leaves no local shadow behind: the
    // write runs on a copy and is installed only once the whole thing succeeds.
    shell.vars.update(&root, global, |root| {
        write_at(root, &steps, value, append, target)
    })
}

/// `$sh.options.KEY = …` — the writable corner of the shell's own namespace.
///
/// `$sh` is not a variable, so none of the machinery above applies: the namespace
/// a read sees is a snapshot built per access, and writing into that would land in
/// a copy nobody keeps. The settings are reached through [`Options`] instead, and
/// everything else in `$sh` is refused *by name*, so the message says which entry
/// and why rather than leaving the user to infer it from a rejected `=`.
///
/// `DESIGN.md` §"Read-only vs. writable within `$sh`" is the list this enforces:
/// the runtime entries are the shell's authoritative state, so config cannot
/// corrupt an invariant; the configuration entries are the user's.
fn assign_into_shell(
    target: &str,
    steps: &[PathStep],
    value: &Value,
    append: bool,
    global: bool,
    vars: &vars::Vars,
) -> Result<(), String> {
    // `global` governs which *scope* a write lands in, and `$sh` has none — it is
    // the session's, whatever function is running. Silently accepting the word
    // would suggest there is another `$sh` somewhere that this one is not.
    if global {
        return Err(
            "`global` cannot apply to `$sh`: it is the session's, not a scope's".to_owned(),
        );
    }
    let (top, rest) = steps.split_first().expect("the parser required one access");
    let (PathStep::Member(entry) | PathStep::Subscript(entry)) = top;
    if entry != "options" {
        return Err(if shell_has_entry(entry, vars) {
            format!("`$sh.{entry}` is read-only; only `$sh.options` may be assigned")
        } else {
            no_shell_entry(entry)
        });
    }
    match rest {
        // `$sh.options = …` wholesale. Refused rather than validated key by key:
        // a map literal that omits a setting would have to mean either "leave it"
        // or "reset it", and neither reading is obviously right. One setting at a
        // time has no such question.
        [] => Err("assign one setting at a time, as `$sh.options.NAME = false`".to_owned()),
        [PathStep::Member(key) | PathStep::Subscript(key)] => {
            // `+=` combines with what is there, which for a boolean is not a
            // meaning `DESIGN.md` gives `+=` — and "or-equals" is not what anyone
            // typing it would expect a setting to do.
            if append {
                return Err(format!("{target}: a setting is set with `=`, not `+=`"));
            }
            vars.options().assign(target, key, value)
        }
        // A setting is a boolean, so there is nothing under one to reach into.
        _ => Err(format!(
            "{target}: a setting is a boolean, with nothing inside it"
        )),
    }
}

/// Does `$sh` have this entry at all? Asked of the live namespace rather than a
/// second list of names, so an entry added later cannot be reported as a typo.
fn shell_has_entry(entry: &str, vars: &vars::Vars) -> bool {
    let Value::Map(entries) = vars.shell_namespace() else {
        unreachable!("$sh resolves to a map");
    };
    entries.iter().any(|(key, _)| key == entry)
}

/// The words a *read* of the same place uses, so a refused write and a failed
/// read do not describe one namespace two different ways.
fn no_shell_entry(entry: &str) -> String {
    format!("$sh: no `{entry}` in this map")
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
    if root == "sh" {
        return Err(unset_shell_error(target, &steps, &shell.vars));
    }
    shell
        .vars
        .update(&root, global, |root| remove_at(root, &steps, target))
}

/// Why nothing in `$sh` can be removed.
///
/// Writable is not the same as removable. `$sh.options` has a **fixed** set of
/// keys — each one is a question the shell asks itself every prompt — so removing
/// one would leave that question with no answer rather than restoring a default.
/// Assigning is the way back, and it is the only way that says which value you
/// meant.
fn unset_shell_error(target: &str, steps: &[PathStep], vars: &vars::Vars) -> String {
    let (top, rest) = steps.split_first().expect("the parser required one access");
    let (PathStep::Member(entry) | PathStep::Subscript(entry)) = top;
    if entry == "options" {
        return if rest.is_empty() {
            "`$sh.options` cannot be removed; it is the settings map itself".to_owned()
        } else {
            format!("{target}: a setting cannot be removed; assign it instead")
        };
    }
    if shell_has_entry(entry, vars) {
        format!("`$sh.{entry}` is read-only")
    } else {
        no_shell_entry(entry)
    }
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
            // A `from_name` miss is dropped rather than reported: the miss does not
            // mean the name is unimplemented, only that `expand` is not where it
            // lives — the regex flags and `:capture` are implemented in
            // `apply_argument_free_modifier`, on the other side of a layer this path
            // cannot reach. Reporting it here mislabeled them. Left as it was, and
            // recorded in TODO.md: the fix is to unify the two paths, not to guess
            // from the name.
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
        E::Scalar(word) => {
            let word = expansion_word(&word.value, last, in_function, shell)?;
            expand::expand_values(vec![word], &shell.vars)
                .map_err(|e| {
                    note!("mesh: {e}");
                    Step::Error(1)
                })
                .map(|mut v| {
                    if v.len() == 1 {
                        v.pop().unwrap()
                    } else {
                        Value::List(v)
                    }
                })
        }
        E::Regex(pattern) => {
            let value = RegexValue::new(pattern.clone());
            compile_regex(&value).map_err(runtime_message)?;
            Ok(Value::Regex(value))
        }
        E::Glob(pattern) => Ok(Value::Glob(pattern.clone())),
        E::Variable(name) => {
            let reference = expansion_variable(&name.value, parser::QuoteMode::Bare);
            expand::resolve_value(&reference, &shell.vars).map_err(runtime_message)
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
        } => {
            let value = eval_expr(expression, last, in_function, shell)?;
            if shell.control.is_some() {
                return Ok(control_placeholder());
            }
            let truth = condition_bool(&value).map_err(runtime_message)?;
            Ok(bool_value(!truth))
        }
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
                    Step::Error(1)
                })
        }
        E::Unary {
            op: U::Spread,
            expression,
        } => eval_expr(expression, last, in_function, shell),
        E::Binary { left, op, right } => {
            let l = eval_expr(left, last, in_function, shell)?;
            // `and` / `or` are boolean operators, so they ask the same question a
            // condition does — and refuse the same values. Short-circuiting has to
            // read the left operand's truth to decide, so the refusal lands here
            // rather than in `eval_binary`, which never sees a short circuit.
            if matches!(op, B::And | B::Or) && shell.control.is_none() {
                let truth = condition_bool(&l).map_err(runtime_message)?;
                if (*op == B::And) != truth {
                    return Ok(bool_value(truth));
                }
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
                Step::Error(1)
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
                        Step::Error(1)
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
                    | Value::Styled(_)
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
                            Step::Error(1)
                        })
                }
                Value::String(_)
                | Value::Styled(_)
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
            match run_ast_pipeline(pipeline, true, last, in_function, shell) {
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
        // A lambda has no `wrapper` marker to carry, so it always parses flags.
        return call_signature_for_value(
            &callee_description(callee),
            params,
            body,
            arguments,
            true,
            last,
            in_function,
            shell,
        );
    };
    let name = word.value.text();
    if name == "style" {
        return eval_style(arguments, last, in_function, shell);
    }
    if name == "link" {
        return eval_link(arguments, last, in_function, shell);
    }
    if name == "glob" {
        return eval_glob(arguments, last, in_function, shell);
    }
    if let Some(filter) = entry_filter(&name) {
        return eval_directory_entries(&name, filter, arguments, last, in_function, shell);
    }
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

/// `style(TEXT, fg: NAME, bg: NAME, bold: BOOL)` — a **styled value**, per
/// `DESIGN.md` §"Hooks and the prompt".
///
/// A value call rather than a command because a structured return value cannot come
/// out of a command position, which yields a status. It sits beside `re()` for the
/// same reason: both are builtins whose arguments are *named*, so neither can go
/// through the argv path that `pwd()` and `puts()` take.
///
/// Styling a styled value **adds to** its attributes rather than replacing them, so
/// `style(style(x, fg: red), bold: true)` is red and bold — a named argument
/// overrides just that attribute. That is what lets a caller take someone else's
/// segment and emphasize it without knowing its color.
///
/// The text argument is taken from any value that has one, through the same
/// rendering `puts` uses minus the styling, so `style($n, fg: red)` on an integer
/// works and `style($j, …)` on a job handle is the same loud error it is everywhere.
fn eval_style(
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let mut text: Option<String> = None;
    let mut style = Style::default();
    let mut inherited = false;
    for argument in arguments {
        match argument {
            parser::Argument::Positional(expression) if text.is_none() => {
                let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                    return Ok(control_placeholder());
                };
                // A styled argument carries its attributes in as the defaults, which
                // is what makes re-styling additive.
                if let Value::Styled(styled) = &value {
                    style = styled.style.clone();
                    inherited = true;
                }
                match builtins::rendered_for_output(&value, Decoration::plain()) {
                    Ok(rendered) => text = Some(rendered),
                    Err(message) => return runtime_error(format!("style(): {message}")),
                }
            }
            parser::Argument::Positional(_) => {
                return runtime_error("style() takes one text argument");
            }
            parser::Argument::Named(name, expression) => {
                let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                    return Ok(control_placeholder());
                };
                match name.as_str() {
                    "fg" | "bg" => {
                        let Value::String(named) = value.plain() else {
                            return runtime_error(format!(
                                "style() `{name}` must be a color name; one of {}",
                                vars::Color::NAMES.join(", ")
                            ));
                        };
                        let Some(color) = vars::Color::named(&named) else {
                            return runtime_error(format!(
                                "style(): `{named}` is not a color name; one of {}",
                                vars::Color::NAMES.join(", ")
                            ));
                        };
                        if name == "fg" {
                            style.foreground = Some(color);
                        } else {
                            style.background = Some(color);
                        }
                    }
                    "bold" => {
                        let Value::Boolean(flag) = value else {
                            return runtime_error("style() `bold` must be a boolean");
                        };
                        style.bold = flag;
                    }
                    _ => {
                        return runtime_error(format!(
                            "style(): no `{name}` attribute; it takes `fg`, `bg` and `bold`"
                        ));
                    }
                }
            }
            parser::Argument::Spread(_) => {
                return runtime_error("style() does not accept spread arguments");
            }
        }
    }
    let Some(text) = text else {
        return runtime_error("style() requires one text argument");
    };
    // A call that named no attribute yields a plain string rather than a styled
    // value with nothing to render — one representation for one meaning, so
    // `style(x) == x` holds by type as well as by value. Re-styling is exempt: the
    // attributes came from the argument and must survive.
    if style.is_plain() && !inherited {
        return Ok(Value::String(text));
    }
    Ok(Value::Styled(Box::new(StyledValue { text, style })))
}

/// `link(text, url)` — a **styled value** whose attribute is an `OSC 8` hyperlink,
/// per `DESIGN.md` §"terminal control".
///
/// A `style` sibling rather than a raw escape, and for the same reason color is: the
/// URL stays *data*, so the shell can measure the visible width from the text and
/// drop the link where it cannot be followed. A raw `\e]8;;…` in a string would be
/// opaque to both.
///
/// Composes with `style` in either order — both build the same value, each setting
/// the attributes it names — so `link(style(x, fg: blue), u)` and
/// `style(link(x, u), fg: blue)` are the same blue clickable `x`.
///
/// Two positional arguments rather than `url:` named, because both are required and
/// the pair reads in the order it renders. It takes the text through the same
/// rendering `style` and `puts` use, so a value with no byte form is the same loud
/// error here as there.
fn eval_link(
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let mut positional: Vec<Value> = Vec::with_capacity(2);
    for argument in arguments {
        match argument {
            parser::Argument::Positional(expression) => {
                let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                    return Ok(control_placeholder());
                };
                if positional.len() == 2 {
                    return runtime_error("link() takes a text and a url argument");
                }
                positional.push(value);
            }
            parser::Argument::Named(name, _) => {
                return runtime_error(format!(
                    "link(): no `{name}` argument; it takes the text and the url positionally"
                ));
            }
            parser::Argument::Spread(_) => {
                return runtime_error("link() does not accept spread arguments");
            }
        }
    }
    let [subject, url] = positional.as_slice() else {
        return runtime_error("link() requires a text and a url argument");
    };
    // The URL is text like any other argument, so it renders the same way — but its
    // *attributes* are meaningless here, and a styled URL almost certainly means the
    // arguments were swapped rather than that the caller wanted escapes in a target.
    let Some(url) = url.as_text() else {
        return runtime_error(format!(
            "link(): the url must be a string, not {}",
            value_kind(url)
        ));
    };
    let url =
        vars::link_url(url).map_err(|message| runtime_message(format!("link(): {message}")))?;
    // An existing link is replaced rather than nested: `OSC 8` has no notion of one
    // link inside another, and the innermost target is the one that was just asked
    // for.
    let mut style = match subject {
        Value::Styled(styled) => styled.style.clone(),
        _ => Style::default(),
    };
    style.link = Some(url);
    let text = match builtins::rendered_for_output(subject, Decoration::plain()) {
        Ok(rendered) => rendered,
        Err(message) => return runtime_error(format!("link(): {message}")),
    };
    Ok(Value::Styled(Box::new(StyledValue { text, style })))
}

/// The file-type filter a directory-entry wrapper is `glob` preset to, or `None`
/// for a name that is not one of them.
fn entry_filter(name: &str) -> Option<expand::Modifier> {
    match name {
        "files" => Some(expand::Modifier::Files),
        "dirs" => Some(expand::Modifier::Dirs),
        _ => None,
    }
}

/// `glob(PATTERN)` — the paths a pattern matches, as a list, per `DESIGN.md`
/// §"Globbing".
///
/// Not a value *constructor* like `re()`: there is no glob value to build. A glob
/// is either a literal you write, which expands where you wrote it, or this call,
/// which expands a pattern the program built at runtime — `ls $p` passes the
/// string `*.jpg` verbatim, and `glob($p)` is how you ask for its matches instead.
///
/// The pattern is a plain string, so it gets no tilde expansion, for the same
/// reason `ls $p` gets none: `~` is a *word* expansion that runs on what you
/// typed, and a value never re-expands. A pattern under the home directory says
/// so with `glob("$env.HOME/…")`, or lets the word form do it — `~/*.txt`.
fn eval_glob(
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let pattern = glob_path_argument("glob", "pattern", arguments, last, in_function, shell)?;
    if shell.control.is_some() {
        return Ok(control_placeholder());
    }
    let Some(pattern) = pattern else {
        return runtime_error("glob() requires one pattern string");
    };
    let paths = expand::glob_paths(&pattern)
        .map_err(|message| runtime_message(format!("glob(): {message}")))?;
    Ok(Value::List(paths.into_iter().map(Value::String).collect()))
}

/// `files(DIR=.)` and `dirs(DIR=.)` — a directory's immediate entries of one type,
/// as a list, per `DESIGN.md` §"Globbing".
///
/// The ergonomic half of the family: `glob` preset to `DIR/*` plus the `type:`
/// filter the name already carries, so the common walk reads as `for d in dirs()`
/// rather than as a pattern plus a modifier. They reuse the `files` / `dirs` words
/// for the same filter the `:files` / `:dirs` modifiers name, so the vocabulary is
/// learned once.
///
/// The default is the working directory rather than a required argument because
/// that is the overwhelming case, and it is spelled `.` — the same directory the
/// bare `*` those entries come from is relative to.
fn eval_directory_entries(
    name: &str,
    filter: expand::Modifier,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let directory = glob_path_argument(name, "directory", arguments, last, in_function, shell)?;
    if shell.control.is_some() {
        return Ok(control_placeholder());
    }
    let directory = directory.unwrap_or_else(|| ".".to_string());
    let paths = expand::directory_entries(&directory, filter)
        .map_err(|message| runtime_message(format!("{name}(): {message}")))?;
    Ok(Value::List(paths.into_iter().map(Value::String).collect()))
}

/// Evaluate the one positional string the glob family takes. `Ok(None)` is "no
/// argument was written" — which is a default for the wrappers and an error for
/// `glob` — or an interrupted call, which the caller tells apart by asking the
/// shell for pending control flow, as every other operand does.
fn glob_path_argument(
    name: &str,
    role: &str,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<String>, Step> {
    let mut path: Option<String> = None;
    for argument in arguments {
        match argument {
            parser::Argument::Positional(expression) if path.is_none() => {
                let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                    return Ok(None);
                };
                // A bare word argument has already globbed by the time it arrives
                // (`dirs(src)` is a word, `dirs(*)` is the list `*` matched), so a
                // list here is a pattern the caller expected to hand over whole.
                let plain = value.plain();
                let Value::String(text) = plain else {
                    return runtime_error(format!(
                        "{name}(): the {role} must be a string, not {}",
                        value_kind(&plain)
                    ));
                };
                path = Some(text);
            }
            parser::Argument::Positional(_) => {
                return runtime_error(format!("{name}() takes one {role} argument"));
            }
            parser::Argument::Named(named, _) => {
                return runtime_error(format!(
                    "{name}(): no `{named}` argument; it takes the {role} positionally"
                ));
            }
            parser::Argument::Spread(_) => {
                return runtime_error(format!("{name}() does not accept spread arguments"));
            }
        }
    }
    Ok(path)
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
        if parser::modifier_requires_arguments(name) {
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
    // A modifier transforms its subject as text, so a styled one is flattened
    // first — the same thing `apply_modifier` does for the argument-free ones, and
    // it has to happen on both paths or `$r:upper` and `$r:split(":")` disagree.
    let value = value.plain();
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
        // `:get(KEY, DEFAULT)` — the total accessor. Both arguments are ordinary
        // values, so no shape is imposed here beyond the count; which key types
        // suit which subject is `expand::get_value`'s to say.
        "get" => {
            let Some([key, default]) = value_arguments(name, arguments, last, in_function, shell)?
            else {
                return Ok(control_placeholder());
            };
            expand::get_value(value, key, default).map_err(runtime_message)
        }
        // The affix family. `:stripstart` / `:stripend` drop a literal affix once
        // if it is there and are a no-op otherwise, and the char-set spellings of
        // `:trimstart` / `:trimend` peel the given characters repeatedly — the
        // whitespace default being the argument-free form handled elsewhere.
        "stripstart" | "stripend" | "trimstart" | "trimend" => {
            let Some(affix) = single_string_argument(name, arguments, last, in_function, shell)?
            else {
                return Ok(control_placeholder());
            };
            if affix.is_empty() {
                return runtime_error(format!("modifier :{name} argument must not be empty"));
            }
            let mut transform = |text: &str| -> Result<String, String> {
                Ok(match name {
                    "stripstart" => text.strip_prefix(&affix).unwrap_or(text).to_string(),
                    "stripend" => text.strip_suffix(&affix).unwrap_or(text).to_string(),
                    "trimstart" => text.trim_start_matches(|c| affix.contains(c)).to_string(),
                    _ => text.trim_end_matches(|c| affix.contains(c)).to_string(),
                })
            };
            expand::map_strings(value, name, &mut transform).map_err(runtime_message)
        }
        // `:replaceall(OLD, NEW)` and its anchored kin. `OLD` is a **match slot**
        // (`DESIGN.md` §"String"): a string matches verbatim, a regex as a pattern
        // — the same no-silent-coercion rule `~` and `:int` follow, so a string
        // full of metacharacters never quietly becomes one.
        "replaceall" | "replacestart" | "replaceend" => {
            let Some([old, new]) = value_arguments(name, arguments, last, in_function, shell)?
            else {
                return Ok(control_placeholder());
            };
            let new = match new.plain() {
                Value::String(new) => new,
                other => {
                    return runtime_error(format!(
                        "modifier :{name} replacement must be a string, got {}",
                        value_kind(&other)
                    ));
                }
            };
            replace_modifier(name, value, old, &new)
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
/// Evaluate exactly `N` positional modifier arguments, imposing no type of their
/// own — a modifier whose arguments are ordinary values (`:get`, the replace
/// family) says what it needs itself, so the shapes stay next to the rule.
///
/// `None` means a control flow effect (a `return` in a lambda default, say) took
/// over mid-evaluation, matching the sibling helpers.
fn value_arguments<const N: usize>(
    name: &str,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Option<[Value; N]>, Step> {
    if arguments.len() != N {
        return runtime_error(format!(
            "modifier :{name} takes exactly {N} arguments, got {}",
            arguments.len()
        ));
    }
    let mut values = Vec::with_capacity(N);
    for argument in arguments {
        let parser::Argument::Positional(expression) = argument else {
            return runtime_error(format!("modifier :{name} takes positional arguments only"));
        };
        let Some(value) = eval_operand(expression, last, in_function, shell)? else {
            return Ok(None);
        };
        values.push(value);
    }
    Ok(Some(
        <[Value; N]>::try_from(values).unwrap_or_else(|_| unreachable!("length checked above")),
    ))
}

/// A per-string modifier step, as [`expand::map_strings`] takes one. Boxed
/// because which one it is depends on whether the pattern arrived as a string or
/// a regex, and the two capture different things.
type StringTransform<'a> = dyn FnMut(&str) -> Result<String, String> + 'a;

/// Apply `:replaceall` / `:replacestart` / `:replaceend` to `subject`.
///
/// The replacement is **literal** — `regex`'s own `$1` expansion is suppressed —
/// because the capture-backreference spelling is still provisional in
/// `DESIGN.md`; taking `$1` now would freeze a syntax the design has not chosen,
/// and the wrong choice is worse than the missing one.
fn replace_modifier(name: &str, subject: Value, old: Value, new: &str) -> Result<Value, Step> {
    let mut transform: Box<StringTransform<'_>> = match old.plain() {
        Value::String(old) => {
            if old.is_empty() {
                return runtime_error(format!("modifier :{name} pattern must not be empty"));
            }
            Box::new(move |text: &str| {
                Ok(match name {
                    "replacestart" => match text.strip_prefix(&old) {
                        Some(rest) => format!("{new}{rest}"),
                        None => text.to_string(),
                    },
                    "replaceend" => match text.strip_suffix(&old) {
                        Some(head) => format!("{head}{new}"),
                        None => text.to_string(),
                    },
                    _ => text.replace(&old, new),
                })
            })
        }
        Value::Regex(regex) => {
            // The same refusal the string arm makes, before anything is compiled.
            // An empty pattern matches at every position, so `:replaceall` would
            // interleave the replacement through the subject and the anchored pair
            // would insert it at an edge — surprising enough to refuse, and the
            // rule must not depend on *which spelling* the caller reached for.
            if regex.pattern.is_empty() {
                return runtime_error(format!("modifier :{name} pattern must not be empty"));
            }
            // The anchored spellings put the edge requirement *into the pattern*
            // and search the **whole subject**, rather than filtering an unanchored
            // pattern's matches or testing truncated slices.
            //
            // Filtering cannot work: the iterator reports **non-overlapping,
            // leftmost-first** matches, so an earlier match consumes the bytes a
            // later trailing one needed — `re("ab|bc")` against `abc` reports only
            // `ab`, and the `bc` that really does end the string is never offered.
            //
            // Slicing cannot work either, for a subtler reason: a look-around
            // assertion reads the bytes *around* the match, so cutting the subject
            // invents context that was never there. `re(r"a\b")` has no match in
            // `ab`, but against the slice `a` the cut end looks like a word
            // boundary and the assertion passes. The subject has to stay whole.
            //
            // `\A` / `\z` are the absolute anchors on purpose: `^` and `$` move to
            // line edges under the `:m` flag, and a subject's edge is not a line's.
            let anchored = |edge: &str| -> Result<regex::Regex, Step> {
                let wrap = |body: &str| {
                    let mut anchored = regex.clone();
                    anchored.pattern = match edge {
                        "replacestart" => format!(r"\A(?:{body})"),
                        _ => format!(r"(?:{body})\z"),
                    };
                    compile_regex(&anchored)
                };
                // Extended mode makes a `#` run to end of line, so a pattern ending
                // in a comment swallows the `)` and the anchor written after it. A
                // newline closes the comment — but whether the pattern is *in*
                // extended mode cannot be read off the flags, since `(?x)` turns it
                // on from inside the pattern. So the plain wrap is tried first and
                // the newline is the fallback, rather than a guess either way:
                // outside extended mode a newline would be one more character the
                // pattern has to match, and short of parsing the pattern the
                // compiler is the only thing that actually knows.
                //
                // The fallback cannot mask a genuinely broken pattern, because a
                // swallowed `)` always leaves the group unclosed — failure here
                // means the wrap broke it, and if both spellings fail the original
                // error is the one reported.
                wrap(&regex.pattern)
                    .or_else(|error| wrap(&format!("{}\n", regex.pattern)).map_err(|_| error))
                    .map_err(runtime_message)
            };
            let compiled = match name {
                "replacestart" | "replaceend" => anchored(name)?,
                _ => compile_regex(&regex).map_err(runtime_message)?,
            };
            Box::new(move |text: &str| {
                Ok(match name {
                    // The match is **the engine's**, found in the whole subject. At
                    // the end that makes it the longest trailing match, since the
                    // engine tries start positions left to right and every candidate
                    // finishes at `\z`; at the start every candidate begins at 0, so
                    // regex's own first-alternative rule decides and `re("a|ab")`
                    // takes `a`. The two edges therefore read differently — but that
                    // difference is the engine's leftmost-first semantics showing
                    // through, the same rule any regex tool follows, and inventing a
                    // longest-match search on top of it is what broke look-around.
                    "replacestart" => match compiled.find(text) {
                        Some(m) => format!("{new}{}", &text[m.end()..]),
                        None => text.to_string(),
                    },
                    "replaceend" => match compiled.find(text) {
                        Some(m) => format!("{}{new}", &text[..m.start()]),
                        None => text.to_string(),
                    },
                    _ => compiled
                        .replace_all(text, regex::NoExpand(new))
                        .into_owned(),
                })
            })
        }
        other => {
            return runtime_error(format!(
                "modifier :{name} pattern must be a string or a regex, got {}",
                value_kind(&other)
            ));
        }
    };
    expand::map_strings(subject, name, &mut *transform).map_err(runtime_message)
}

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
pub(crate) fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "a string",
        // Named as a string, because that is what it behaves as everywhere a
        // diagnostic is talking about: only a renderer sees the difference.
        Value::Styled(_) => "a string",
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
    // Flattened first, like the subject and like the replace family's own
    // arguments: display attributes are rendering-only, so a `style()`d separator
    // or affix is the text it shows. Which bytes a modifier splits or strips on
    // must not depend on how they happen to be colored.
    match value.plain() {
        Value::String(value) => Ok(Some(value)),
        _ => runtime_error(format!("modifier :{name} argument must be a string")),
    }
}

fn runtime_message(message: impl std::fmt::Display) -> Step {
    note!("mesh: {message}");
    Step::Error(1)
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
            // Every built-in value name, not just `re`: `glob("*"):capture` asks
            // what a *call* wrote and returned, and routing it to the command path
            // would report the command-not-found for a `glob` that isn't one.
            (!parser::value_builtin(&name) && shell.funcs.get(&name).is_none()).then_some(name)
        }
        _ => None,
    };
    if let Some(name) = command {
        return capture_command(&name, arguments, last, in_function, shell);
    }

    shell.value_call_status = None;
    let (outcome, out_text, err_text) = capture::with_channels_captured(shell, |shell| {
        eval_call(callee, arguments, last, in_function, shell)
    })?;
    // A `return`/`fail` carries its own status out; a body that fell off its end
    // reports the value's, which is the same rule an expression statement follows.
    let unwound = shell.value_call_status.take();
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
    let status = unwound.unwrap_or_else(|| status_of(&value));
    Ok(channel_record(
        Some(value.clone()),
        out_text,
        err_text,
        status,
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
                    words.extend(capture_argument_words(&value, name)?);
                }
                parser::Argument::Spread(expression) => {
                    let Some(value) = eval_operand(expression, last, in_function, shell)? else {
                        return Ok(None);
                    };
                    match value {
                        Value::List(values) => {
                            for value in &values {
                                words.extend(capture_argument_words(value, name)?);
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

/// The words one captured argument contributes.
///
/// `:capture` reaches a builtin as readily as an external — `run_expanded` resolves
/// either — so `puts`/`print` render their values here exactly as they do in
/// command position. Everything else takes the bytes-only argv rule.
fn capture_argument_words(value: &Value, name: &str) -> Result<Vec<String>, Step> {
    if matches!(name, "puts" | "print") {
        // A capture's stdout is a pipe into the record, so a styled value
        // contributes its text: the record holds data, and escape bytes in it would
        // compare unequal to the text they decorate.
        return match builtins::rendered_for_output(value, Decoration::plain()) {
            Ok(text) => Ok(vec![text]),
            Err(message) => runtime_error(format!("{name}: {message}")),
        };
    }
    argv_words(value, name)
}

/// The argv tokens a value contributes to an external's command line — the same
/// bytes-only rule expansion applies, since an external takes bytes.
fn argv_words(value: &Value, name: &str) -> Result<Vec<String>, Step> {
    match value {
        Value::String(text) => Ok(vec![text.clone()]),
        // An external takes bytes, so the text crosses and the attributes do not.
        Value::Styled(styled) => Ok(vec![styled.text.clone()]),
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

/// The status an assignment takes from its right-hand side: the last recorded
/// status when that side **is** a capture, and `0` otherwise.
///
/// Reading the shell's ordinary status is safe precisely because [`capture_tail`]
/// is syntactic — the only way to reach this is to have just evaluated an
/// expression whose value came from a capture, and running that capture is what
/// recorded the status.
fn capture_status_of(expr: &parser::Expr, shell: &Shell) -> u8 {
    if capture_tail(expr) {
        shell.vars.status()
    } else {
        0
    }
}

/// Can this expression be evaluated without running anything that records a
/// status? Conservative by construction — anything not listed executes as far as
/// this is concerned, so a new expression kind defaults to "assume it runs".
fn runs_nothing(expr: &parser::Expr) -> bool {
    match expr {
        parser::Expr::Variable(_) | parser::Expr::Regex(_) | parser::Expr::Glob(_) => true,
        // A word runs something only if a spliced value does.
        parser::Expr::Scalar(word) => word.value.pieces.iter().all(|piece| match piece {
            parser::WordPiece::Text { .. } | parser::WordPiece::Variable { .. } => true,
            parser::WordPiece::Value { expression, .. } => runs_nothing(&expression.value),
        }),
        parser::Expr::Group(inner) => runs_nothing(inner),
        // Reaching into a value runs nothing of its own, so `($m).sep` is as inert
        // as `$sep` — how a separator is *accessed* must not change whether the
        // capture beside it keeps its status. Both halves are checked, since either
        // can hold something that executes (`$m[$(cmd)]`).
        parser::Expr::Member { value, .. } => runs_nothing(value),
        parser::Expr::Index { value, index } => runs_nothing(value) && runs_nothing(index),
        _ => false,
    }
}

/// Does this expression's value come from a `$(…)` at its **tail**?
///
/// This is what decides whether an assignment reports its right-hand side's
/// capture status or its own `0`, and it is answered from the **syntax** rather
/// than from anything the evaluation left behind — `DESIGN.md`'s standing
/// preference, "readable from the line, never data-dependent". That is what
/// makes the rule leak-proof: an expression that merely *ran* a capture along the
/// way (a call whose body used one, a compound whose body did, a `:capture`
/// record over a command taking one as an argument) is not a capture, so there is
/// no state for it to smuggle out.
///
/// An interpolation reports its **last executing** piece, as bash does:
/// `"$(false)$(true)"` leaves `0`, and `"$(false)suffix"` leaves `1` — trailing
/// text runs nothing, so it cannot displace the capture's status. Only a piece
/// that runs something can, which is why the scan skips text and variables and
/// then answers on whatever it reaches first: a capture hands over its status, and
/// anything else that executes (a call, say) is the thing whose status was
/// recorded last, so the capture's is gone.
fn capture_tail(expr: &parser::Expr) -> bool {
    match expr {
        parser::Expr::Capture(_) => true,
        parser::Expr::Group(inner) => capture_tail(inner),
        // Indexing runs nothing, so a capture reached through one keeps its status
        // — `$(cmd):split(":")[0]` still answers for `cmd`. The index expression
        // itself has to be inert, since `$(cmd):split(":")[$(other)]` would put
        // `other` last.
        parser::Expr::Index { value, index } => runs_nothing(index) && capture_tail(value),
        // A modifier that runs nothing leaves the capture as the last thing that
        // recorded a status, so `$(cmd):upper` still answers for `cmd`. The ones
        // that *do* run something — a higher-order modifier invoking a callable,
        // `:capture` invoking outright — are not transparent, and neither is a
        // modifier whose arguments could execute, since those run after the
        // subject and would be what recorded last.
        parser::Expr::Modifier {
            value,
            name,
            arguments,
        } => {
            let invokes = matches!(name.as_str(), "map" | "filter" | "each" | "capture");
            let inert_arguments = arguments.as_ref().is_none_or(|arguments| {
                arguments.iter().all(|argument| match argument {
                    parser::Argument::Positional(expr) | parser::Argument::Named(_, expr) => {
                        runs_nothing(expr)
                    }
                    parser::Argument::Spread(expr) => runs_nothing(expr),
                })
            });
            !invokes && inert_arguments && capture_tail(value)
        }
        parser::Expr::Scalar(word) => word
            .value
            .pieces
            .iter()
            .rev()
            .find_map(|piece| match piece {
                parser::WordPiece::Value { expression, .. } => {
                    Some(capture_tail(&expression.value))
                }
                // Runs nothing, so it records no status and the scan continues.
                parser::WordPiece::Text { .. } | parser::WordPiece::Variable { .. } => None,
            })
            .unwrap_or(false),
        _ => false,
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
    // A capture that **ran nothing** — `$()`, or a body whose every statement a
    // guard skipped — has no status of its own to hand back. Without this it would
    // borrow whatever ran before it, so `false; x = $()` would report `1`.
    //
    // `produced` is the signal rather than the status-record count, because a
    // skipped statement still passes through `run_recorded` and bumps that count
    // while executing nothing. `run_source` leaves `produced` at `Nothing` exactly
    // when no statement in the body ran, which is the question being asked.
    if shell.produced == Produced::Nothing {
        shell.record_status(0, vec![0]);
    }
    match step {
        // The bytes are the answer whatever the command exited with. A nonzero
        // status is routinely a *result* rather than an error — `diff` says 1 for
        // "they differ" and the diff itself is on stdout, `grep` says 1 for "no
        // match", `timeout` says 124 over whatever was printed first — so
        // discarding the output would throw away the thing that was asked for.
        // Every POSIX shell binds it and reports the status separately. Nothing is
        // stashed for the caller here: running the body recorded the status the
        // ordinary way, and an assignment whose right-hand side is a capture reads
        // it back through `capture_status_of`.
        Step::Continue(_) => Ok(Value::String(output.trim_end_matches('\n').to_string())),
        // An evaluation error is not a status to carry — the program was invalid,
        // and mesh aborts the statement for one wherever it happens (`x = $nope`
        // leaves `x` unbound; `puts "a[$nope]b"` prints nothing). Yielding here
        // would turn `x = $(puts $nope)` into an empty string and let the statement
        // continue, which is the "invalid source" and "the command failed" channels
        // being confused — the one distinction `AGENTS.md` asks to keep.
        //
        // The classification is **within one process**, which is the boundary
        // `DESIGN.md` §"Isolation and subshells" draws: a subshell forks, and "only
        // bytes cross back out". An error inside a forked part of the body — a
        // pipeline stage, an explicit `fork { … }` — therefore arrives as a status,
        // and `x = $(fork { puts $nope })` binds the empty string. That is the
        // subshell contract doing its job rather than a gap in this one: reducing
        // an inner world to bytes and a status is what the isolation is *for*, and
        // a side channel carrying the classification out would undo it.
        Step::Error(code) => Err(Step::Error(code)),
        // `break` / `continue` / `return` / `exit` are not statuses — they are the
        // body unwinding, with no value to yield — so they still propagate.
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
            let guard_value = eval_operand_of(guard, last, in_function, shell)?;
            let passed =
                shell.control.is_some() || condition_bool(&guard_value).map_err(runtime_message)?;
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
        return match &arm.body {
            parser::MatchBody::Value(expression) => eval_expr(expression, 0, in_function, shell),
            // How a statement-context block yields a value is the open
            // value-production question (the same one `func` bodies have), so this
            // keeps the existing block-value behavior rather than settling it.
            parser::MatchBody::Block(body) => eval_value_body(body, 0, in_function, shell),
        };
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
        && !bare_word_names_a_command(&word.value)
    {
        return eval_expr(
            &parser::Expr::Scalar(word.clone()),
            last,
            in_function,
            shell,
        );
    }
    // No capture. A block is not a `$(…)`: its commands stream to wherever stdout
    // goes, in value position exactly as in statement position, and its value is
    // the last thing that *produced* one. That is what `func` has always done, so
    // routing `if` and `match` through the same `eval_body` is what makes the three
    // agree.
    //
    // Capturing here meant the same block text either streamed or was silently
    // eaten depending on whether anyone bound the result — `x = if true { echo hi }`
    // swallowed `hi` and handed back the bytes, while the bare statement printed
    // them — and the whole block was captured, not just its tail. The exit-0 gate
    // that came with it failed silently too: a failing command left the binding
    // unmade, so the error surfaced as an "unbound variable" on a later line.
    // Bytes come from `$(…)`, which is the thing that means "capture".
    eval_body(body, last, in_function, shell)
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
            Some(parser::ControlKind::Return | parser::ControlKind::Fail) => unreachable!(),
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
    // How the last statement that executed ended, as in `run_source`: a body whose
    // value comes from an invalid program is not an answer, and the caller — a
    // `$(f())` above all — has to be able to tell.
    let mut errored = false;
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
                        // A tail that runs answers for the body, clearing an earlier
                        // statement's error the way the next statement does in
                        // `run_source`. One a guard skipped answers for nothing, so
                        // the earlier error still stands.
                        Ok(true) => eval_expr(expression, last, in_function, shell),
                        Ok(false) if errored => Err(Step::Error(last)),
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
        let step = run_statement(statement, last, in_function, shell);
        let executed = shell.produced != Produced::Nothing;
        match step {
            // An evaluation error abandons its own statement, not the body around
            // it, exactly as in `run_source` — `func f() { 1 / 0; return }` still
            // reaches the `return`, carrying the status the failed statement left.
            // Only the classification outlives the statement, so that a body which
            // *ends* invalid says so instead of handing back a plausible value.
            Step::Continue(code) => {
                last = code;
                if executed {
                    errored = false;
                }
            }
            Step::Error(code) => {
                last = code;
                errored = true;
            }
            flow => return Err(flow),
        }
        recorded |= executed;
        if shell.control.is_some() {
            return Ok(Value::String(String::new()));
        }
    }
    if errored {
        return Err(Step::Error(last));
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

/// Is this value true, as a **condition**?
///
/// A condition is a bool or a command, and nothing else: a command branches on
/// its exit status because of *where it is written*, and a value branches on its
/// truth because it is a `Boolean`. There is no third rule, so no value type is
/// coerced into one.
///
/// The coercions this replaces were three different rules wearing one name — an
/// integer read as an exit status (`0` true, the inversion of every other
/// language), a string read for emptiness *and* sniffed against the literal texts
/// `"false"` and `"0"`, a collection read for emptiness. Together they made `if 0`
/// true while `if "0"` was false, and `if $xs:len` fire on the **empty** list and
/// stay quiet on a full one. Refusing is what makes `$xs:len > 0` the thing you
/// write.
fn condition_bool(value: &Value) -> Result<bool, String> {
    match value {
        Value::Boolean(value) => Ok(*value),
        other => Err(format!(
            "{} is not a condition; {}",
            type_phrase(other),
            condition_hint(other)
        )),
    }
}

/// How to turn the value you have into the condition you meant.
fn condition_hint(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) => "compare it (`… > 0`), or use `fail` to report a status",
        Value::String(_) | Value::Styled(_) => "compare it (`… != \"\"`)",
        Value::List(_) | Value::Map(_) => "test its length (`…:len > 0`)",
        _ => "compare it",
    }
}

/// A value's type, with its article, for a diagnostic that reads as a sentence.
fn type_phrase(value: &Value) -> &'static str {
    match value {
        Value::String(_) | Value::Styled(_) => "a string",
        Value::Integer(_) => "an int",
        Value::Boolean(_) => "a bool",
        Value::List(_) => "a list",
        Value::Map(_) => "a map",
        Value::Regex(_) => "a regex",
        Value::Glob(_) => "a glob",
        Value::Stream(_) => "a stream handle",
        Value::Job(_) => "a job handle",
        Value::Function(_) => "a function",
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
                // A styled value orders as its text, the same rule `==` follows.
                _ => match (left.as_text(), right.as_text()) {
                    (Some(left), Some(right)) => left.cmp(right),
                    _ => return Err("comparison requires two integers or two strings".into()),
                },
            };
            bool_value(match op {
                Less => ordering.is_lt(),
                LessEqual => !ordering.is_gt(),
                Greater => ordering.is_gt(),
                GreaterEqual => !ordering.is_lt(),
                _ => unreachable!(),
            })
        }
        // The left operand's truth was settled by the short circuit above, so the
        // result is the right operand's — and it faces the same refusal.
        And | Or => bool_value(condition_bool(&right)?),
        // `in` asks about membership and substrings, both of which read the text —
        // and `Value`'s own equality is by text, so `style(x, fg: red) in $xs`
        // finds a plain `x`. Flattening both sides says that once.
        In => match (left.plain(), right.plain()) {
            (left, Value::List(values)) => bool_value(values.contains(&left)),
            (left, Value::Map(values)) => match left {
                Value::String(key) => {
                    bool_value(values.iter().any(|(candidate, _)| candidate == &key))
                }
                _ => return Err("map key must be a string".into()),
            },
            (left, Value::String(text)) => match left {
                Value::String(needle) => bool_value(text.contains(&needle)),
                _ => return Err("left operand of `in` must be a string".into()),
            },
            (
                _,
                Value::Styled(_)
                | Value::Integer(_)
                | Value::Boolean(_)
                | Value::Regex(_)
                | Value::Glob(_)
                | Value::Stream(_)
                | Value::Job(_)
                | Value::Function(_),
            ) => {
                return Err("right operand of `in` must be a collection or string".into());
            }
        },
        Match | NotMatch => {
            // Matching reads the text, as `==` and `<` do, so a styled subject is
            // matched on its text rather than refused.
            let Some(text) = left.as_text().map(str::to_owned) else {
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
fn run_pipeline(
    mut stages: Vec<Stage>,
    background: bool,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    if stages.len() == 1 {
        run_single(stages.pop().unwrap(), background, last, in_function, shell)
    } else {
        run_multi(stages, background, last, in_function, shell)
    }
}

/// Run a one-stage pipeline. Without redirections this is the full command
/// surface: an assignment or a builtin/function/external command. A redirected
/// in-shell command — builtin or function — runs in the shell with the targets
/// applied to its own descriptors around the call, since there is no child to
/// configure. Backgrounding needs a child, so it goes through the pipeline path,
/// which forks the stage.
fn run_single(
    stage: Stage,
    background: bool,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    let Stage {
        words,
        redirs,
        pipe_stderr: _,
    } = stage;
    if redirs.is_empty() && !background {
        return run_command(&words, last, in_function, shell);
    }
    // A builtin that reads typed arguments keeps them here too, so a handle or a
    // list reaches it through a redirection exactly as it does without one. Styling
    // is the one thing that *does* change: `> file` means this command's stdout is
    // not the terminal, whatever the shell's own stdout is. Backgrounding does not
    // change it — the fork inherits the shell's stdout.
    let decoration = if redirects_stdout(&redirs) {
        Decoration::plain()
    } else {
        stdout_decoration()
    };
    // Backgrounded, this stage forks — so a value in one of its words is expanded
    // *there*, and `puts $(sleep 10) &` spends the ten seconds in the job rather
    // than at the prompt. A foreground redirected command has no fork to defer to:
    // a builtin or function runs in the shell with the targets around the call, so
    // its arguments were always this process's to evaluate.
    if background && can_defer(&words, &redirs) {
        let opened = match expand_redirs(redirs, last, in_function, shell) {
            Ok(redirs) => redirs,
            Err(step) => return step,
        };
        return Step::Continue(run_stages(
            vec![exec::Cmd {
                words: deferred_words(&words),
                redirs: opened,
                pipe_stderr: false,
                in_shell: true,
            }],
            vec![StageBody::Deferred {
                words,
                decoration,
                context: Deferred::Backgrounded,
            }],
            background,
            last,
            in_function,
            shell,
        ));
    }
    // The words expand *before* the targets are opened: `f * > summary` must not
    // see the `summary` the redirection is about to create.
    let argv = match expand_stage(&words, decoration, last, in_function, shell) {
        // A function is resolved by the expansion, since it takes typed arguments in
        // every position. Foreground: the redirection applies to the shell's own
        // descriptors around the in-process call, since there is no child to
        // configure. Background: there *is* a child — the fork the pipeline path
        // makes.
        Ok(Expanded::Function { name, args }) => {
            let opened = match expand_redirs(redirs, last, in_function, shell) {
                Ok(redirs) => redirs,
                Err(step) => return step,
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
                    in_function,
                    shell,
                ));
            }
            return match exec::with_redirections(&opened, || {
                dispatch_function_call(&name, args, shell)
            }) {
                Ok(step) => step,
                Err((path, err)) => {
                    note!("mesh: {path}: {err}");
                    Step::Error(1)
                }
            };
        }
        // `return` is control flow handled on the no-redirection path; with a
        // redirection or in the background it never reaches that handler, so reject
        // it rather than launch an external `return` while the body keeps running.
        Ok(Expanded::Return(_)) => {
            note!("mesh: return: cannot be redirected or backgrounded");
            return Step::Error(2);
        }
        Ok(Expanded::Fail(_)) => {
            note!("mesh: fail: cannot be redirected or backgrounded");
            return Step::Error(2);
        }
        Ok(Expanded::Argv(argv)) => argv,
        Err(step) => return step,
    };
    if argv.is_empty() {
        note!("mesh: redirection with no command is not supported yet");
        return Step::Error(1);
    }
    // A redirected builtin runs in the shell like a redirected function: the
    // targets apply to the shell's own descriptors around the call, so there is
    // nothing to configure on a child.
    //
    // `command NAME …` resolves to the program here rather than in the shell, so
    // a redirected or backgrounded one is that program's own process: the child
    // `&` needs is the program itself, not a shell that goes on to run it.
    let external = external_stage(&argv);
    let builtin = external.is_none();
    let argv = external.unwrap_or(argv);
    let opened = match expand_redirs(redirs, last, in_function, shell) {
        Ok(redirs) => redirs,
        Err(step) => return step,
    };
    if builtin && !background {
        return match exec::with_redirections(&opened, || run_expanded(argv, last, shell)) {
            Ok(step) => step,
            Err((path, err)) => {
                note!("mesh: {path}: {err}");
                Step::Error(1)
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
        in_function,
        shell,
    ))
}

/// Run a multi-stage pipeline (`a | b | c`). Each stage is an external command, a
/// builtin, or a function; the in-shell ones run in a forked stage so all the
/// stages run concurrently.
fn run_multi(
    stages: Vec<Stage>,
    background: bool,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    let mut cmds = Vec::with_capacity(stages.len());
    let mut bodies = Vec::with_capacity(stages.len());
    let count = stages.len();
    for (index, stage) in stages.into_iter().enumerate() {
        let Stage {
            words,
            redirs,
            pipe_stderr,
        } = stage;
        // Only the last stage can be writing to the terminal; every earlier one has
        // stdout on a pipe, so a styled value there renders as plain text.
        let decoration = if index + 1 == count && !redirects_stdout(&redirs) {
            stdout_decoration()
        } else {
            Decoration::plain()
        };
        // A stage carrying a value expands in its **own** fork, not here: a call in
        // one of its arguments belongs to the stage, and this process is not it.
        // Every stage of a pipeline forks, so every one of them can defer.
        if can_defer(&words, &redirs) {
            let opened = match expand_redirs(redirs, last, in_function, shell) {
                Ok(redirs) => redirs,
                Err(step) => return step,
            };
            cmds.push(exec::Cmd {
                words: deferred_words(&words),
                redirs: opened,
                pipe_stderr,
                in_shell: true,
            });
            bodies.push(StageBody::Deferred {
                words,
                decoration,
                context: Deferred::Piped,
            });
            continue;
        }
        // Command words expand before the redirect targets, the order `run_single`
        // uses, so a stage reports the same first failure the unpiped command
        // does — and `f * > summary` cannot glob the file the redirection is
        // about to create.
        let (stage_words, body) = match expand_stage(&words, decoration, last, in_function, shell) {
            // The arguments are typed values, and mesh has no implicit
            // stringification for a list or map, so only the name goes into the
            // words a job listing echoes back.
            Ok(Expanded::Function { name, args }) => (vec![name], StageBody::Function(args)),
            // `return` unwinds the enclosing function; it has no meaning as a
            // pipeline stage, so reject it rather than launch an external `return`.
            Ok(Expanded::Return(_)) => {
                note!("mesh: return: cannot be used in a pipeline");
                return Step::Error(2);
            }
            Ok(Expanded::Fail(_)) => {
                note!("mesh: fail: cannot be used in a pipeline");
                return Step::Error(2);
            }
            Err(step) => return step,
            Ok(Expanded::Argv(argv)) => {
                if argv.is_empty() {
                    note!("mesh: empty command in a pipeline");
                    return Step::Error(1);
                }
                // `command NAME …` is the program `NAME`, so the stage is that
                // program rather than a forked shell that runs it.
                match external_stage(&argv) {
                    Some(program) => (program, StageBody::External),
                    None => (argv, StageBody::Builtin),
                }
            }
        };
        let opened = match expand_redirs(redirs, last, in_function, shell) {
            Ok(redirs) => redirs,
            Err(step) => return step,
        };
        cmds.push(exec::Cmd {
            words: stage_words,
            redirs: opened,
            pipe_stderr,
            in_shell: !matches!(body, StageBody::External),
        });
        bodies.push(body);
    }
    Step::Continue(run_stages(
        cmds,
        bodies,
        background,
        last,
        in_function,
        shell,
    ))
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
    /// A stage whose words are **not expanded yet**, because one of them carries a
    /// value — `puts $(pwd) | cat`, `cmd f() &`.
    ///
    /// Evaluating that in the parent would run the work in the wrong process: a
    /// backgrounded stage would spend its time in the shell before the job was even
    /// registered, and a mutating call would change the parent's bindings where
    /// `docs/REFERENCE.md` promises the fork keeps them. So the stage carries its
    /// words down to its own fork and expands them there — which is also what
    /// decides, that late, whether it is a builtin, a function, or an external to
    /// `exec::exec_stage` into.
    Deferred {
        words: Vec<parser::Word>,
        decoration: Decoration,
        /// Which of the two the stage is, since the words it will come to are
        /// checked against the same rules the eager paths apply — and those two
        /// say different things about the same word.
        context: Deferred,
    },
}

/// Why a stage was deferred, which is also what it says when its words turn out to
/// name something a stage cannot be.
#[derive(Clone, Copy)]
enum Deferred {
    /// One stage of a `|` pipeline.
    Piped,
    /// A single command with `&`.
    Backgrounded,
}

impl Deferred {
    /// What to say when the words come to nothing at all.
    fn empty(self) -> &'static str {
        match self {
            Self::Piped => "empty command in a pipeline",
            Self::Backgrounded => "redirection with no command is not supported yet",
        }
    }

    /// What to say when they come to `return`, which is control flow unwinding a
    /// function rather than a command a stage can run.
    fn returning(self) -> &'static str {
        match self {
            Self::Piped => "return: cannot be used in a pipeline",
            Self::Backgrounded => "return: cannot be redirected or backgrounded",
        }
    }
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
    in_function: bool,
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
            // A deferred stage is *not* treated like a function, for all that what
            // it runs is equally unknowable: almost every one of them is an ordinary
            // command (`puts $(pwd) | cat`), and reaping for those would take a
            // finished job out from under a later `fg` — the very cost this test
            // exists to avoid. Its command word is usually a plain literal, so ask
            // that: `jobs $(…) | …` still refreshes, and nothing else pays for it.
            // A `jobs` spelled some other way sees a stale listing, which is the
            // cheaper way to be wrong.
            StageBody::Deferred { words, .. } => {
                words.first().is_some_and(|word| word.is_bare_text("jobs"))
            }
            StageBody::External => false,
        })
    {
        jobs.reap();
    }
    let outcome = exec::run_pipeline(cmds, &mut jobs, background, &mut |index, cmd, jobs| {
        std::mem::swap(&mut shell.jobs, jobs);
        let status = run_stage_in_shell(&bodies[index], cmd, last, in_function, shell);
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
fn run_stage_in_shell(
    body: &StageBody,
    cmd: &exec::Cmd,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> u8 {
    shell.forked = true;
    let step = match body {
        StageBody::Function(args) => dispatch_function_call(&cmd.words[0], args.clone(), shell),
        // Not `builtins::dispatch`: `jobs`, `fg`, `bg`, and the prompt builtins
        // are dispatched by the shell, and would otherwise fall through to an
        // external lookup and report "command not found".
        StageBody::Builtin => run_expanded(cmd.words.clone(), last, shell),
        StageBody::External => unreachable!("an external stage has no in-shell body"),
        // The words are expanded **here**, in the stage's own process, which is the
        // whole point of deferring them: a `$(…)` or a call in one runs where its
        // effects belong. What they come to decides how the stage ends — as a
        // function call, a builtin, or this process replaced by a program.
        StageBody::Deferred {
            words,
            decoration,
            context,
        } => match expand_stage(words, *decoration, last, in_function, shell) {
            Ok(Expanded::Function { name, args }) => dispatch_function_call(&name, args, shell),
            // `return` unwinds the enclosing function; it is not something a stage
            // can run, and the eager paths refuse it in the same terms.
            Ok(Expanded::Return(_) | Expanded::Fail(_)) => {
                note!("mesh: {}", context.returning());
                Step::Error(2)
            }
            Ok(Expanded::Argv(argv)) => {
                if argv.is_empty() {
                    note!("mesh: {}", context.empty());
                    Step::Error(1)
                } else if let Some(program) = external_stage(&argv) {
                    // Replaces this process, so nothing below runs on success.
                    return exec::exec_stage(&program);
                } else {
                    run_expanded(argv, last, shell)
                }
            }
            Err(step) => step,
        },
    };
    match step {
        Step::Continue(code) | Step::Error(code) | Step::Exit(code) | Step::Return(_, code) => code,
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
/// Does any of these redirections take stdout somewhere else?
///
/// The question a styled value's rendering turns on: a command whose stdout is a
/// file or another descriptor is not writing to a terminal, whatever the shell's
/// own stdout happens to be. Only the descriptor is consulted, not the target, so
/// this is answerable *before* the targets are expanded and opened — which is when
/// it has to be answered, since the words are rendered first.
fn redirects_stdout(redirs: &[Redir]) -> bool {
    redirs.iter().any(|redir| {
        let fd = redir.fd.unwrap_or(match redir.kind {
            exec::RedirKind::In => 0,
            exec::RedirKind::Out | exec::RedirKind::Append => 1,
        });
        fd == 1
    })
}

/// Expand each redirection target, evaluating any value in one on the way.
///
/// Called **after** every command word is expanded, which is the documented order:
/// `f * > summary` must not glob the file the redirection is about to create, and a
/// call in a target must not change what a word written before it reads.
fn expand_redirs(
    redirs: Vec<Redir>,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Vec<exec::Redirection>, Step> {
    // Every "what is wrong with this target" answer is a message the caller used to
    // report; reported here instead, so the caller has one thing to propagate.
    let bad = |message: String| runtime_message(message);
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
                interpolate_heredoc(&body.text, Some(&shell.vars)).map_err(bad)?
            };
            out.push(exec::Redirection {
                fd: libc::STDIN_FILENO,
                kind: exec::RedirKind::In,
                target: exec::RedirTarget::Heredoc(text),
            });
            continue;
        }
        // Each target in turn: a value in one is evaluated as that target is
        // reached, the rule a command word already follows.
        let target = expansion_word(&redir.target, last, in_function, shell)?;
        let mut words =
            expand::expand(vec![target], &shell.vars).map_err(|e| bad(e.to_string()))?;
        if words.len() != 1 {
            return Err(bad(format!(
                "ambiguous redirect: target expanded to {} words",
                words.len()
            )));
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
                    bad(format!(
                        "`>&{word}`: the target of a duplication must be a descriptor"
                    ))
                })?;
                if from < 0 {
                    return Err(bad(format!("`>&{from}`: a descriptor cannot be negative")));
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

/// A single literal word, for a desugaring that needs one. Quoted, so the text a
/// desugaring wrote is the text that arrives — never a glob, never a `~`.
fn one_word(text: &str) -> parser::Word {
    parser::Word {
        pieces: vec![parser::WordPiece::Text {
            text: text.to_owned(),
            quote: parser::QuoteMode::Single,
        }],
        qualifiers: None,
    }
}

/// Run one command with no redirections: classify it as an assignment or a
/// command and act. `last` is the previous status (the default for a bare `exit`
/// or `return`).
/// What a stage's words came to, once the command they name is known.
enum Expanded {
    /// An in-shell **function** and its typed arguments. Resolved here because a
    /// function takes typed arguments in every position, so it has to be settled
    /// before the external argv rule turns a bare list into an error
    /// (`DESIGN.md` §"Arguments do not word-split").
    Function {
        name: String,
        args: Vec<(Value, bool)>,
    },
    /// `return`, with its operand typed the way a function call's arguments are.
    /// Resolved here for the same reason a function is: argv would flatten a list
    /// or map on the way past, and a function's result is exactly where those have
    /// to survive (`DESIGN.md` §"Result and `return`").
    Return(Vec<(Value, bool)>),
    /// `fail`, whose operand is typed the same way `return`'s is.
    Fail(Vec<(Value, bool)>),
    /// argv for a builtin or an external program.
    Argv(Vec<String>),
}

/// Expand a stage's words into the form its command takes.
///
/// **One word at a time, in order**, because a word can carry a *value* — a
/// `$(…)` that launches a command, a call that runs a function — and evaluating
/// one can change what a later word reads. Doing it up front made the change
/// visible to words written *earlier* on the line:
///
/// ```text
/// cmd = /bin/echo
/// func g() { global cmd = /bin/false; return x }
/// $cmd g()            # ran /bin/false, not the /bin/echo that was selected first
/// ```
///
/// Word zero is expanded once and first: it decides how every other word expands,
/// and a value in it (`"$(pick)" arg`) must not run again for each question asked
/// of it. Redirect targets are still expanded after **all** the words, which is
/// the documented order — `f * > summary` must not glob the file the redirection
/// is about to create.
///
/// `decoration` is the caller's answer to "which escapes does this command's own
/// stdout take", which decides what a **styled** value emits. It has to be the
/// caller's because words are expanded *before* a redirection is opened or a pipe
/// is attached, so this function's own view of stdout is the shell's, not the
/// command's.
fn expand_stage(
    words: &[parser::Word],
    decoration: Decoration,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Expanded, Step> {
    let Some((first, rest)) = words.split_first() else {
        return Ok(Expanded::Argv(Vec::new()));
    };
    let head = expansion_word(first, last, in_function, shell)?;
    let mut argv = expand::expand(vec![head], &shell.vars).map_err(runtime_message)?;
    // A word that expanded to several (a glob) or to none names no command; those
    // fall through to the plain argv rule, which reports what is wrong with them.
    let name = (argv.len() == 1).then(|| argv[0].clone());
    // A function's arguments and `return`'s operand take the same typed path: both
    // become *values*, and argv is the boundary that would flatten a list or map on
    // the way. A function can never be named `return` — definition rejects the word
    // — so the two cannot both claim one stage.
    let returning = matches!(name.as_deref(), Some("return" | "fail"));
    let calling = name
        .as_ref()
        .is_some_and(|name| shell.funcs.get(name).is_some());
    if returning || calling {
        let mut args = Vec::new();
        for word in rest {
            let word = expansion_word(word, last, in_function, shell)?;
            args.extend(
                expand::expand_call_values(vec![word], &shell.vars).map_err(runtime_message)?,
            );
        }
        let name = name.expect("a named stage, since one of the two branches claimed it");
        return Ok(match name.as_str() {
            "return" => Expanded::Return(args),
            "fail" => Expanded::Fail(args),
            _ => Expanded::Function { name, args },
        });
    }
    for word in rest {
        let word = expansion_word(word, last, in_function, shell)?;
        argv.extend(stage_argument(&word, name.as_deref(), decoration, shell)?);
    }
    Ok(Expanded::Argv(argv))
}

/// The argv entries one already-evaluated argument word contributes.
///
/// Most words take ordinary expansion. Two builtin families read **values**
/// instead, because what they need is not what the argv boundary produces — and
/// each does so per word, so a word with nothing special in it keeps exactly the
/// text ordinary expansion gives it. Every path that runs a command comes through
/// here, since they expand separately: `puts $xs`, `puts $xs > out` and
/// `puts $xs | cat` have to render the list the same way, just as `kill $j` and
/// `kill $j | cat` have to name the same job.
fn stage_argument(
    word: &Word,
    name: Option<&str>,
    decoration: Decoration,
    shell: &Shell,
) -> Result<Vec<String>, Step> {
    match name {
        Some(name @ ("fg" | "bg" | "wait" | "kill" | "disown")) => {
            if let Some(references) = job_reference_word(word, &shell.vars, name)? {
                return Ok(references);
            }
        }
        Some(name @ ("puts" | "print")) => {
            return output_words(word, &shell.vars, name, decoration);
        }
        _ => {}
    }
    expand::expand(vec![word.clone()], &shell.vars).map_err(runtime_message)
}

/// Run an in-shell function call whose arguments are already expanded: generated
/// `--help` first, then the call itself.
fn dispatch_function_call(name: &str, args: Vec<(Value, bool)>, shell: &mut Shell) -> Step {
    // A `wrapper func` reads no flags at all: `--help` belongs to whatever it
    // forwards to, so answering it here would hide the callee's own help — the
    // whole point of `wrapper func g(...args) { command grep ...$args }` is that
    // `g --help` is grep's help.
    let wrapper = shell.funcs.get(name).is_some_and(|def| def.wrapper);
    // Intercept `--help` only when the signature does not claim it; a function
    // that declares a `--help` flag observes the switch itself (`DESIGN.md`
    // §"Command resolution and help").
    let declares_help = shell.funcs.get(name).is_some_and(|def| def.declares_help());
    if !wrapper && !declares_help && auto_help_requested(&args) {
        let help = shell.funcs.get(name).expect("declared function").help(name);
        return Step::Continue(builtins::print_generated_help(name, &help));
    }
    // The `--` terminator and flag parsing are handled during argument binding in
    // `call_func`. A command-position call parses flags, unless this is a wrapper
    // — then every argument binds positionally, `--flag` and `--` included.
    call_func(name, args, !wrapper, shell)
}

fn run_command(tokens: &[parser::Word], last: u8, in_function: bool, shell: &mut Shell) -> Step {
    // No redirection on this path, so the command's stdout is the shell's.
    //
    // Functions can never share a name with a builtin or the `return`/job control
    // words (definition rejects those), so resolving one in `expand_stage` does not
    // reorder the builtins → functions → external chain.
    match expand_stage(tokens, stdout_decoration(), last, in_function, shell) {
        Ok(Expanded::Function { name, args }) => dispatch_function_call(&name, args, shell),
        Ok(Expanded::Return(args)) => make_return(args, last, shell),
        Ok(Expanded::Fail(args)) => make_fail(args),
        Ok(Expanded::Argv(words)) => run_expanded(words, last, shell),
        Err(step) => step,
    }
}

/// The word(s) one `puts`/`print` argument contributes.
///
/// A word whose values are all plain scalars keeps exactly the text ordinary
/// expansion gives it, so a glob still contributes one word per match and a bare
/// `007` still prints as written. Anything else — a list, a map, a value with no
/// byte form at all, a styled value — is rendered per `DESIGN.md` §"I/O".
fn output_words(
    word: &Word,
    vars: &Vars,
    name: &str,
    decoration: Decoration,
) -> Result<Vec<String>, Step> {
    // A styled value is deliberately *not* a scalar here: its rendering is the
    // whole point, so it must not fall through to ordinary expansion, which would
    // hand over the text with the attributes dropped.
    let scalar = |value: &Value| {
        matches!(
            value,
            Value::String(_) | Value::Integer(_) | Value::Boolean(_)
        )
    };
    match expand::expand_values(vec![word.clone()], vars) {
        Ok(values) if !values.iter().all(scalar) => values
            .iter()
            .map(|value| {
                builtins::rendered_for_output(value, decoration).map_err(|message| {
                    note!("mesh: {name}: {message}");
                    Step::Error(1)
                })
            })
            .collect(),
        // Either every value was a scalar, or the value pass failed; ordinary
        // expansion renders the first and reports the second in the terms the
        // rest of the shell uses.
        _ => expand::expand(vec![word.clone()], vars).map_err(runtime_message),
    }
}

/// The `%id` this word names, if it is a job handle; `None` leaves it to
/// ordinary expansion.
///
/// Only a handle takes this route: everything else keeps exactly the text ordinary
/// expansion produces. That distinction is load-bearing, because a job builtin's
/// options are not just data. Expanding them as values types `-0` as the integer
/// `0` and drops the sign along with it, which turns `kill -0 $pid` — ask whether
/// it is alive — into `kill 0 $pid`, and pid 0 is *the caller's own process group*.
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

/// What a `command …` line comes to, once `command`'s own words are read off the
/// front.
enum CommandLine {
    /// The program to run and its arguments, with the `command` prefix gone.
    External(Vec<String>),
    /// `command --help` — mesh's own help, since no program was named to ask.
    Help,
    /// A leading word that reads as one of `command`'s options and is not one.
    Unknown(String),
    /// A `command` with no program in it at all.
    Nothing,
}

/// Read `command`'s own words off the front of `args`, which is everything after
/// the word `command` itself.
///
/// **Only the leading words are `command`'s.** The first word that is not an
/// option of its own names the program, and everything after that word belongs to
/// the program — which is the whole point of the builtin, so `command ls --help`
/// asks `ls` for its help rather than printing this builtin's.
///
/// The program name is taken verbatim, `command` included: a second `command` is
/// a program of that name to look for, not another prefix to peel. The rule is
/// "the operand is the program", with no exception to remember.
///
/// A **flag-looking** word in front of the program is `command`'s own, and
/// `--help` is the only one it has, so anything else there is a usage error rather
/// than a program name. Two reasons, and either would do: `command -v ls` is a
/// bash reflex that would otherwise report "command not found: -v", which is a
/// true statement about the wrong question; and reading `-v` as a program today is
/// what `command -v` would have to keep meaning tomorrow, when the option it
/// obviously names is built. `command -- -v` still runs a program called `-v`.
fn command_line(args: &[String]) -> CommandLine {
    let program = match args {
        [] => return CommandLine::Nothing,
        [flag, ..] if flag == "--help" => return CommandLine::Help,
        // `command` owns its terminator, because only it knows where its options
        // end: after `--` the next word is the program even if it reads as a flag.
        [terminator, rest @ ..] if terminator == "--" => rest,
        [flag, ..] if flag.starts_with('-') => return CommandLine::Unknown(flag.clone()),
        rest => rest,
    };
    match program {
        [] => CommandLine::Nothing,
        program => CommandLine::External(program.to_vec()),
    }
}

/// The external argv this already-expanded stage runs, if it runs one: an
/// ordinary external's own words, or — for `command NAME …` — the program it
/// names, with the prefix taken off. Stripping it here is what makes the stage
/// *be* the program: one process, not a forked shell that then runs one.
///
/// `None` for anything the shell runs itself — a builtin, or a `command` line with
/// no program in it (`command`, `command --help`), which [`run_expanded`] answers.
///
/// A function is never asked about: every caller resolved one before this, since a
/// function takes typed arguments and these words are already argv.
fn external_stage(argv: &[String]) -> Option<Vec<String>> {
    if argv[0] != "command" {
        return (!builtins::is_builtin(&argv[0])).then(|| argv.to_vec());
    }
    match command_line(&argv[1..]) {
        CommandLine::External(program) => Some(program),
        CommandLine::Help | CommandLine::Unknown(_) | CommandLine::Nothing => None,
    }
}

/// Report a word in front of the program that reads as one of `command`'s options
/// and is not one, and give back the usage status.
///
/// `-v` and `-V` get their own answer because they are not so much unknown as
/// **unbuilt**: they are what a reader coming from another shell types to ask what
/// a name would run, and "not an option" would send them looking for the spelling
/// mesh uses instead of telling them there is not one yet.
fn unknown_command_option(flag: &str) -> Step {
    if flag == "-v" || flag == "-V" {
        note!(
            "mesh: command: {flag}: asking what a name would run is not built yet; \
             `command -- {flag}` runs a program of that name"
        );
    } else {
        note!(
            "mesh: command: {flag}: not an option of `command`; \
             `command -- {flag}` runs a program of that name"
        );
    }
    Step::Error(2)
}

/// Run a command whose words are already expanded: `return`, `command`, generated
/// help, the prompt and job-control builtins, then the builtin → external chain. A
/// function has already been resolved by the caller, which still has its unexpanded
/// words and so can keep its arguments typed.
///
/// `return` reaches here only from the one caller that has nothing but strings —
/// `cmd(args):capture`, which expands into argv before it knows the name. Command
/// position resolves it as [`Expanded::Return`] instead and keeps the operand
/// typed, so a `return $xs` there carries the list rather than its text.
fn run_expanded(mut words: Vec<String>, last: u8, shell: &mut Shell) -> Step {
    if words.is_empty() {
        // A command whose words all expanded away (e.g. a glob with no
        // matches) is an empty-list result — status 0 per `DESIGN.md`.
        return Step::Continue(0);
    }
    // `return` ends the enclosing function (a recoverable error at top
    // level; `run_line` decides which by `in_function`).
    if words[0] == "return" || words[0] == "fail" {
        let args = words[1..]
            .iter()
            .map(|word| (expand::typed_scalar(word), true))
            .collect();
        return if words[0] == "fail" {
            make_fail(args)
        } else {
            make_return(args, last, shell)
        };
    }
    // `command` is read before any of the resolution below, because everything
    // below is what it exists to skip — and because its arguments are another
    // command's: the generic `--help` and `--` handling would read
    // `command ls --help` as a question about `command`.
    if words[0] == "command" {
        return match command_line(&words[1..]) {
            CommandLine::External(program) => Step::Continue(exec::run(&program, &mut shell.jobs)),
            CommandLine::Help => Step::Continue(builtins::print_help("command")),
            CommandLine::Unknown(flag) => unknown_command_option(&flag),
            CommandLine::Nothing => {
                note!("mesh: command: expected a program to run");
                Step::Error(2)
            }
        };
    }
    if builtins::is_builtin(&words[0]) {
        if auto_help_requested_strings(&words[1..]) {
            return Step::Continue(builtins::print_help(&words[0]));
        }
        // `--` ended the search above; for a builtin with no options of its own it
        // now has to be **taken out of the way**, exactly as `call_func` does when
        // binding a function's arguments. Left in, the terminator `DESIGN.md` offers
        // as the escape from auto-help stopped the detection and was then printed:
        // `puts -- --help` wrote `-- --help`.
        //
        // A builtin that *does* read options keeps it, because only that builtin
        // knows where its options end. Removing it here would undo the very thing it
        // was written for — `kill -- -9 %1` would send SIGKILL rather than look for a
        // job named `-9`, and `prompt -- --reset` would reset instead of setting that
        // text.
        if !builtins::reads_options(&words[0])
            && let Some(at) = words.iter().skip(1).position(|word| word == "--")
        {
            words.remove(at + 1);
        }
    }
    match words[0].as_str() {
        "prompt" => return configure_prompt(&words[1..], shell),
        "on" => return configure_hook(&words[1..], shell),
        // `cd` fires the `precd` / `postcd` hooks, which are this shell's, so it
        // cannot go through `builtins::dispatch` either.
        "cd" => return change_directory(&words[1..], shell),
        // `source` runs mesh code in *this* shell, so it belongs here rather than
        // in `builtins::dispatch`, which is handed only words and a status.
        "source" => return source_file(&words[1..], last, shell),
        // `gets` binds a variable in *this* shell, so like `source` it cannot go
        // through `builtins::dispatch`, which sees only words and a status.
        "gets" => return gets(&words[1..], shell),
        // `whence` reads this shell's functions and bindings, which `builtins::dispatch`
        // is handed none of.
        "type" => return Step::Continue(whence::type_of(&words[1..], &shell.funcs, &shell.vars)),
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
            // Whether Ctrl-C abandons the wait or resumes it presupposes a
            // keyboard attached to a terminal this shell took, so it asks the
            // stricter question rather than what kind of session this is.
            let interrupts = shell.vars.owns_terminal();
            Some(shell.jobs.wait(&words[1..], interrupts))
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

/// `cd [DIR]`, with the directory hooks around it: `precd` before the move
/// (still in the old directory, given the resolved destination), `postcd` after
/// (in the new one, given where it came from).
///
/// The hooks fire around **each actual move**, a `cd` inside a function
/// included, which is what makes `precd`'s "old directory" contract hold —
/// deferring to function return would run it somewhere else. A handler that
/// `cd`s itself does not re-dispatch them (`Shell::in_cd_hooks`), and a move
/// that fails owes no `postcd`.
fn change_directory(args: &[String], shell: &mut Shell) -> Step {
    let target = match builtins::cd_target(args) {
        Ok(target) => target,
        Err(code) => return Step::Continue(code),
    };
    // Captured before `precd`, so a handler that wanders cannot become what
    // `$env.OLDPWD` and `postcd` report as where this move started.
    let previous = env::current_dir().ok();
    let hooks = !shell.in_cd_hooks;
    if hooks {
        run_cd_hooks(HookEvent::PreCd, target.path(), shell);
    }
    let status = match builtins::cd_change(&target, previous.as_deref()) {
        Ok(status) => status,
        Err(code) => return Step::Continue(code),
    };
    if hooks && let Some(previous) = previous {
        run_cd_hooks(HookEvent::PostCd, &previous, shell);
    }
    Step::Continue(status)
}

/// Dispatch one directory event with `path` as its single argument, holding the
/// re-entrancy guard for the length of the handlers.
///
/// The path is passed lossily: a hook takes mesh values, and a mesh string is
/// UTF-8. A directory whose name is not valid UTF-8 therefore reaches a handler
/// with replacement characters rather than not at all — the alternative,
/// skipping the event, would hide the move entirely from something like a
/// directory tracker.
fn run_cd_hooks(event: HookEvent, path: &Path, shell: &mut Shell) {
    if !shell.prompt.hooks.iter().any(|hook| hook.event == event) {
        return;
    }
    let argument = Value::String(path.to_string_lossy().into_owned());
    shell.in_cd_hooks = true;
    run_hooks(event, vec![argument], shell);
    shell.in_cd_hooks = false;
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
    // `prompt` owns its terminator because it reads an option: `--` ends the options
    // and everything after it is the prompt text, so `prompt -- --reset` sets that
    // string rather than resetting. A prompt is exactly the kind of value that can
    // start with a dash — one built from `style(…)` escapes, or a literal `-> `.
    if let [terminator, rest @ ..] = args
        && terminator == "--"
    {
        return match rest {
            [text] => {
                shell.prompt.text = Some(text.clone());
                Step::Continue(0)
            }
            _ => {
                note!("mesh: prompt: expected one prompt string after `--`");
                Step::Error(2)
            }
        };
    }
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
            Step::Error(2)
        }
    }
}

fn configure_hook(args: &[String], shell: &mut Shell) -> Step {
    // `on` reads an option, so it owns its terminator: after `--` every word
    // is an operand. That is what lets a hook be *named* `--remove`, which is the
    // whole case the terminator exists for.
    if let [terminator, rest @ ..] = args
        && terminator == "--"
    {
        return register_hook_operands(rest, shell);
    }
    match args {
        [flag, event, name] if flag == "--remove" => {
            let Some(event) = HookEvent::parse(event) else {
                return invalid_hook();
            };
            shell
                .prompt
                .hooks
                .retain(|hook| hook.event != event || hook.name != *name);
            Step::Continue(0)
        }
        _ => register_hook_operands(args, shell),
    }
}

/// The `EVENT NAME FUNCTION` form, with no option reading left to do.
///
/// Shared by the plain path and the one past `--`, so the two cannot drift into
/// disagreeing about what an operand list looks like.
fn register_hook_operands(args: &[String], shell: &mut Shell) -> Step {
    match args {
        [event, name, function] => {
            let Some(event) = HookEvent::parse(event) else {
                return invalid_hook();
            };
            register_hook(event, name, function, shell)
        }
        _ => invalid_hook(),
    }
}

fn invalid_hook() -> Step {
    note!("mesh: on: expected EVENT NAME FUNCTION or --remove EVENT NAME");
    Step::Error(2)
}

fn register_hook(event: HookEvent, name: &str, function: &str, shell: &mut Shell) -> Step {
    if shell.funcs.get(function).is_none() {
        note!("mesh: on: `{function}` is not a function");
        return Step::Error(1);
    }
    if let Some(hook) = shell
        .prompt
        .hooks
        .iter_mut()
        .find(|hook| hook.event == event && hook.name == name)
    {
        hook.function = function.to_string();
    } else {
        shell.prompt.hooks.push(Hook {
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
    /// `E` — the command line as submitted, which lets a terminal label and re-run
    /// the command instead of guessing it back out of the echo. Only `OSC 633`
    /// carries it; `OSC 133` has no such sequence, so this writes nothing there.
    CommandLine(String),
}

/// Which shell-integration dialect a session speaks.
///
/// `OSC 633` is VS Code's superset of `OSC 133`: the same `A`/`B`/`C`/`D`
/// boundaries under a different number, plus `E`, which hands over the command
/// line. VS Code understands plain `133` too — but only from `633;E` does it learn
/// what the command *was*, which is what its re-run and command-label features
/// need; left to `133` it recovers the text by reading back the echo, and gets it
/// wrong whenever the prompt or the line editor is interesting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Integration {
    Osc133,
    Osc633,
}

impl Integration {
    /// The number the dialect's sequences carry.
    fn code(self) -> &'static str {
        match self {
            Integration::Osc133 => "133",
            Integration::Osc633 => "633",
        }
    }
}

/// The dialect for this session, from `$env.TERM_PROGRAM`, held once.
///
/// `vscode` is what VS Code sets, and what its forks set too. One dialect, not
/// both: VS Code parses `133` as well, so sending both would have it count every
/// command twice.
///
/// Read once for the same reason as [`session_term`] — the terminal on the other
/// end does not change mid-session, and a dialect that changed under the marks
/// would close a region in a language it was not opened in. Both are snapshotted
/// at one point in `run_interactive`, after the startup files, so an `rc.mesh` that
/// sets either variable is honored and neither read depends on which sequence
/// happens to be written first.
fn session_integration() -> Integration {
    static DIALECT: OnceLock<Integration> = OnceLock::new();
    *DIALECT.get_or_init(|| match std::env::var("TERM_PROGRAM").as_deref() {
        Ok("vscode") => Integration::Osc633,
        _ => Integration::Osc133,
    })
}

/// The bytes for one mark in one dialect, or `None` when the dialect has no
/// sequence for it.
fn mark_sequence(dialect: Integration, mark: &SemanticMark) -> Option<String> {
    let code = dialect.code();
    Some(match mark {
        SemanticMark::OutputStart => format!("\x1b]{code};C\x1b\\"),
        SemanticMark::CommandDone(status) => format!("\x1b]{code};D;{status}\x1b\\"),
        SemanticMark::CommandAbandoned => format!("\x1b]{code};D\x1b\\"),
        SemanticMark::CommandLine(command) => {
            if dialect == Integration::Osc133 {
                return None;
            }
            format!("\x1b]{code};E;{}\x1b\\", vscode_escaped(command))
        }
    })
}

/// A command line as `OSC 633;E` must carry it.
///
/// The payload is delimited by `;`, so a semicolon in the command would end the
/// sequence early and leave the rest of the line on screen — `sleep 1; puts hi` is
/// an ordinary thing to type. VS Code's escape for this is `\xAB`, hex for the
/// byte, and it wants the same for the control range; the backslash that
/// introduces it has to be escaped too, or `C:\x3b` in an argument would decode as
/// a semicolon that was never typed.
fn vscode_escaped(command: &str) -> String {
    let mut escaped = String::with_capacity(command.len());
    for character in command.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            ';' => escaped.push_str("\\x3b"),
            control if control.is_control() => {
                escaped.push_str(&format!("\\x{:02x}", control as u32));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// Is an interactive decoration on — the session is interactive *and* the
/// setting governing it is set?
///
/// The two tests stay separate questions answered in one place. Interactivity is
/// not the setting's job: `set_interactive` is recorded by the interactive loop
/// rather than derived from `isatty`, so `mesh -s` on a terminal — which reads
/// commands without being a session — stays quiet whatever `$sh.options` says,
/// and so does every piped run the test suite asserts byte-exact output from.
fn decoration(vars: &vars::Vars, option: Opt) -> bool {
    vars.owns_terminal() && vars.options().get(option)
}

/// Write an OSC 133 mark, so a terminal can tell prompt from input from output:
/// jump between commands, fold their output, badge a failure. `DESIGN.md`
/// "terminal control" lists the sequence set; this is the pair with boundaries
/// mesh already has, at `PreExec` and `PostExec`.
///
/// `enabled` is [`decoration`] with [`Opt::ShellIntegration`], decided by the
/// caller and **once per command**: `C` and `D` bracket a region, so a setting
/// changed by the command in between must not close a region that was never
/// opened, or leave one open that was. A mark on stdout that the caller did not
/// ask for is corruption, not decoration.
///
/// Terminated with `ST` rather than `BEL`, matching what reedline emits for `A`
/// and `B`, so one stream does not mix the two spellings.
///
/// Failure to write is ignored: the command's status is the command's, and a
/// decoration that could change it would be worse than a missing decoration.
fn semantic_mark(enabled: bool, mark: SemanticMark) {
    if !enabled {
        return;
    }
    let Some(sequence) = mark_sequence(session_integration(), &mark) else {
        return;
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
/// can chdir to. `enabled` is [`decoration`] with [`Opt::CwdReport`]; failure to
/// write is ignored for the same reason as [`semantic_mark`].
fn report_cwd(enabled: bool) {
    if !enabled {
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
/// glance. `enabled` is [`decoration`] with [`Opt::OscTitle`]; failure to write is
/// ignored for the same reason as [`semantic_mark`].
///
/// **Returns whether a sequence was actually written**, which the caller records
/// on the [`Shell`]. The clear on the way out is owed to any title this session
/// put there — including one written before the setting was turned off, since a
/// shell that stops updating the title still has to stop *owning* it. A terminal
/// that never took one (`$env.TERM` off the allowlist) is owed nothing, which is
/// why this answers about the write rather than about the setting.
fn set_title(enabled: bool, text: &str) -> bool {
    if !enabled {
        return false;
    }
    let Some(sequence) = title_sequence(session_term().as_deref(), text) else {
        return false;
    };
    use std::io::Write as _;
    let mut out = io::stdout();
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
    true
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
/// - **A terminal in [`OSC_TERMS`]** takes OSC 0, which sets the window and the
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
    OSC_TERMS
        .iter()
        .any(|family| names_terminal(term, family))
        .then(|| format!("\x1b]0;{}\x07", title_text(text)))
}

/// The terminal families mesh will send an `OSC` sequence to — a title, or a
/// notification.
///
/// An allowlist because the two ways of being wrong are not equally bad: a
/// terminal missing from here quietly gets neither, while one wrongly assumed to
/// parse `OSC` *prints the payload*, as the Linux console does. A terminal here
/// that does not implement a particular sequence discards it, which is what makes
/// one list serve both. `TODO.md` carries the reasons in full, including why
/// terminfo cannot answer this: `hs`/`tsl`/`fsl` describe a hardware status line,
/// and almost no modern entry declares them.
const OSC_TERMS: &[&str] = &[
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

/// Which escapes the shell's **own** stdout takes, for a command that has not
/// redirected it.
///
/// The `TERM` half of the answer lives here because [`takes_osc`] does; the
/// descriptor half is [`builtins::terminal_decoration`], next to the writing. Split
/// that way so each capability rule stays in one place rather than one per caller.
///
/// `TERM` is read **live** rather than through [`session_term`]'s snapshot, and the
/// difference is not an oversight. The snapshot exists because a *region* must not be
/// opened in one dialect and closed in another — an OSC 133 mark, a title cleared at
/// exit. A styled value spans nothing: each render is self-contained, so reading the
/// current `TERM` is both correct and what `NO_COLOR` already does beside it.
fn stdout_decoration() -> Decoration {
    builtins::terminal_decoration(takes_osc(std::env::var_os("TERM").as_deref()))
}

/// Will this terminal **parse** an `OSC` rather than print it?
///
/// The one question every `OSC` mesh writes has to ask, so it is asked in one place:
/// the notification uses it, and so does an `OSC 8` hyperlink.
///
/// An allowlist rather than a denylist because the two ways of being wrong are not
/// equal. A terminal missing from the list quietly gets no decoration, which nobody
/// has to debug; one wrongly assumed to parse `OSC` **prints the payload** —
/// `TERM=linux` reads `ESC ]` as the start of a palette sequence and abandons it at
/// the first non-hex byte, leaving the rest on screen.
///
/// Multiplexers count: they parse the stream themselves, so a sequence they do not
/// implement is discarded rather than forwarded to be printed.
fn takes_osc(term: Option<&std::ffi::OsStr>) -> bool {
    let Some(term) = term.and_then(std::ffi::OsStr::to_str) else {
        return false;
    };
    names_terminal(term, "screen")
        || names_terminal(term, "tmux")
        || OSC_TERMS.iter().any(|family| names_terminal(term, family))
}

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

/// A string safe to put in a title, via the shared `OSC` payload rule.
fn title_text(text: &str) -> String {
    builtins::osc_payload(text, TITLE_LIMIT)
}

/// How long a command must take before finishing is worth a desktop notification.
///
/// A threshold stands in for the question mesh cannot answer: whether anyone is
/// watching. Terminals report focus (`CSI ?1004 h`), but the line editor owns the
/// input, so those events do not reach the shell — and a command long enough to
/// walk away from is the usable proxy. Ten seconds is long enough that a
/// notification is news and short enough to catch a build.
const NOTIFY_AFTER: Duration = Duration::from_secs(10);

/// The notification for a command that has just finished, or `None` when it does
/// not deserve one.
///
/// Split from the writing so the decision — the threshold, the wording, the
/// terminal's suitability — is testable without a terminal or a ten-second wait.
fn command_notification(
    term: Option<&std::ffi::OsStr>,
    inside: Multiplexer,
    command: &str,
    status: u8,
    elapsed: Duration,
    after: Duration,
) -> Option<String> {
    if elapsed < after {
        return None;
    }
    let outcome = if status == 0 {
        "done".to_owned()
    } else {
        format!("exit {status}")
    };
    let tail = format!(" — {outcome} in {}", duration_words(elapsed));
    // The command is cut to what is left after the parts that must survive, rather
    // than the whole message being cut at the end. Cutting the assembled line drops
    // the outcome and the duration off a long command's notification — exactly the
    // words that make it worth raising. Raised in review on #242.
    //
    // Made safe before it is assembled, too: a control character at the end of the
    // command would otherwise become a space sitting in front of the dash.
    let room = NOTIFY_LIMIT.saturating_sub("mesh: ".len() + tail.chars().count());
    let said = builtins::osc_payload(command, room);
    notification_sequence(term, inside, &format!("mesh: {}{tail}", said.trim()))
}

/// `OSC 9` carrying `text`, for a terminal mesh will send `OSC` to — wrapped for
/// the multiplexer in between, when there is one.
///
/// The same allowlist as the title, and for the same reason: a terminal that
/// mis-parses `OSC` would print this instead. Which terminals *implement*
/// notifications is a different and unaskable question — iTerm2, WezTerm, Ghostty,
/// kitty and ConEmu raise them, xterm and Alacritty parse and discard, and none of
/// them answer — so the list that keeps the payload off the screen is the only gate
/// worth having.
fn notification_sequence(
    term: Option<&std::ffi::OsStr>,
    inside: Multiplexer,
    text: &str,
) -> Option<String> {
    takes_osc(term).then(|| {
        builtins::through_multiplexer(
            &format!("\x1b]9;{}\x07", builtins::osc_payload(text, NOTIFY_LIMIT)),
            inside,
        )
    })
}

/// A duration as a person would say it: `9s`, `1m30s`, `2h5m`.
///
/// Seconds are dropped once there are hours, since "2h5m3s" is more precision than
/// anyone reads off a notification.
fn duration_words(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match (seconds / 3600, (seconds % 3600) / 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m{seconds}s"),
        (hours, minutes, _) => format!("{hours}h{minutes}m"),
    }
}

/// Write a notification, if the command earned one. `enabled` is [`decoration`]
/// with [`Opt::CommandNotify`]; failure to write is ignored, like every other
/// sequence mesh emits automatically.
fn notify_command_done(enabled: bool, command: &str, status: u8, elapsed: Duration) {
    if !enabled {
        return;
    }
    let Some(sequence) = command_notification(
        session_term().as_deref(),
        builtins::multiplexer(),
        command,
        status,
        elapsed,
        NOTIFY_AFTER,
    ) else {
        return;
    };
    use std::io::Write as _;
    let mut out = io::stdout();
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
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

/// Run `jobdone` for every job reported since the last drain, oldest first.
///
/// Called wherever the shell may have noticed one, because the notice and the
/// hook are the same event and must not land at different prompts. A job the
/// user explicitly `wait`ed for never appears here: its status went to the
/// caller, which is the answer the hook exists to give.
fn run_jobdone_hooks(shell: &mut Shell) {
    // Until nothing new is queued, rather than once: a handler is arbitrary mesh
    // code and can report a job itself — it need only run `jobs` — and those
    // arrive *after* the list this call took. One pass would leave them for a
    // later drain, which at shutdown does not come.
    //
    // Bounded because a handler that reports a fresh job on every pass would
    // otherwise keep the shell here for good, and a shell that will not exit is
    // worse than a hook that fires late. The limit is far above any real chain.
    for _ in 0..64 {
        let finished = shell.jobs.take_finished();
        if finished.is_empty() {
            return;
        }
        for job in finished {
            run_hooks(
                HookEvent::JobDone,
                vec![
                    Value::Integer(i64::try_from(job.id).unwrap_or(i64::MAX)),
                    Value::String(job.command),
                    Value::Integer(i64::from(job.status)),
                ],
                shell,
            );
        }
    }
}

fn run_hooks(event: HookEvent, args: Vec<Value>, shell: &mut Shell) {
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

/// Build the [`Step::Return`] for a `return` command word: no argument uses the
/// last status; a single argument is carried as the result, its status a view of
/// that value. A surplus operand is reported and does not unwind (the function
/// keeps running).
///
/// The operand arrives typed the way a function call's arguments do, so a bare
/// `7` is the integer, `true`/`false` are booleans, and a list or map keeps its
/// shape — `return $xs` carries the list. That is the same typing as the implicit
/// last expression, which is the point: the two ways to leave a function must
/// agree about what they carry, or `return` is a narrower channel than falling off
/// the end. Quoting still separates `return 42` from `return "42"`, as everywhere
/// else a value is typed from a word.
fn make_return(args: Vec<(Value, bool)>, last: u8, shell: &Shell) -> Step {
    match <[_; 1]>::try_from(args) {
        Ok([(value, _)]) => {
            let code = status_of(&value);
            Step::Return(value, code)
        }
        // Bare: the result so far, carrying the last status (`DESIGN.md`
        // §"Result and `return`").
        Err(args) if args.is_empty() => Step::Return(shell.result.clone(), last),
        Err(_) => {
            note!("mesh: return: too many arguments");
            Step::Error(1)
        }
    }
}

/// Build the [`Step::Return`] for `fail` — the status channel's counterpart to
/// `return`.
///
/// `fail` leaves the function with a **nonzero** status and no result. Bare it is
/// `1`, the shell's ordinary "something went wrong"; `fail 3` names a specific
/// code. The value it carries is `false`, which is mesh's "no result", so a caller
/// reading the value sees the same absence whether the callee said `fail` or
/// `return false`; the two are told apart by the status.
///
/// `fail 0` is refused rather than silently succeeding: a `fail` that succeeds is
/// always a mistake, and the spelling for "leave with success" is `return true`.
fn make_fail(args: Vec<(Value, bool)>) -> Step {
    let code = match <[_; 1]>::try_from(args) {
        Ok([(value, _)]) => match &value {
            Value::Integer(code) if (1..=255).contains(code) => {
                u8::try_from(*code).expect("checked against the u8 range above")
            }
            Value::Integer(_) => {
                note!("mesh: fail: status must be between 1 and 255");
                return Step::Error(2);
            }
            _ => {
                note!("mesh: fail: status must be an integer");
                return Step::Error(2);
            }
        },
        Err(args) if args.is_empty() => 1,
        Err(_) => {
            note!("mesh: fail: too many arguments");
            return Step::Error(2);
        }
    };
    Step::Return(Value::Boolean(false), code)
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
            Step::Return(_, code) => Step::Continue(code),
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
        Step::Return(_, code) => Step::Continue(code),
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
    let Some((params, body, wrapper)) = shell
        .funcs
        .get(name)
        .map(|def| (def.params.clone(), def.body.clone(), def.wrapper))
    else {
        unreachable!("call_func_for_value is only reached for a declared function");
    };
    // A `wrapper func` parses no flags of its own in either call form, so the
    // value spelling forwards `--flag` verbatim just as command position does.
    call_signature_for_value(
        name,
        &params,
        &body,
        arguments,
        !wrapper,
        last,
        in_function,
        shell,
    )
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
    flags_enabled: bool,
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
    let scanned = evaluate_value_arguments(
        name,
        params,
        arguments,
        flags_enabled,
        last,
        in_function,
        shell,
    );
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
            return Err(Step::Error(1));
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
    shell.value_call_status = match &outcome {
        Err(Step::Return(_, code)) => Some(*code),
        _ => None,
    };
    match outcome {
        // `exit` unwinds regardless: it leaves the shell, not just this call.
        Err(step @ Step::Exit(_)) => Err(step),
        // The diagnostic is already on stderr, so fail quietly with its status.
        _ if escaped => Err(Step::Error(1)),
        Ok(value) => Ok(value),
        // An explicit `return val` yields its value; a runtime step unwinds.
        Err(Step::Return(value, _)) => Ok(value),
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
    let scanned = evaluate_value_arguments(&label, &[], arguments, true, last, in_function, shell);
    // A `break`/`continue` an argument raised belongs to the caller's loop, and is
    // answered before the argument outcome — an out-of-loop one arrives as an error
    // *and* leaves the flag set. Leaving it set stops the enclosing function where
    // the same call through a lambda recovers and runs on.
    if shell.control.is_some() {
        shell.result = caller_result;
        shell.produced = caller_produced;
        if shell.loop_depth == 0 {
            shell.control = None;
            return Err(Step::Error(1));
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
    if parser::modifier_requires_arguments(name) {
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
/// rather than user syntax (hooks) disable it so every value binds
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
        return Err(Step::Error(2));
    }
    if !has_rest && supplied > maximum {
        if maximum > required {
            note!("mesh: {name}: expected at most {maximum} argument(s), got {supplied}");
        } else {
            note!("mesh: {name}: expected {maximum} argument(s), got {supplied}");
        }
        return Err(Step::Error(2));
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
///
/// `flags_enabled` gates the dashed forms exactly as it does in command mode: a
/// `wrapper func` parses no flags of its own, so `g(--color=never)` forwards the
/// token as a positional instead of failing on an option the wrapper never
/// declared. An explicit `key: value` still binds by name — that is the caller
/// naming a parameter, not a flag being passed through.
#[allow(clippy::type_complexity)]
fn evaluate_value_arguments<'p>(
    name: &str,
    params: &'p [parser::Param],
    arguments: &[parser::Argument],
    flags_enabled: bool,
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
                flags_enabled,
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
                            flags_enabled,
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
        return Err(Step::Error(2));
    }
    Ok(())
}

/// Route one value-mode call argument to the right place: a bare `--` ends option
/// parsing (everything after it is positional, even if it looks like a flag), a
/// `--name`/`--name=value` string binds that option, and anything else is a
/// positional. Shared by direct positional arguments and spread elements so both
/// follow the command-mode rules (`DESIGN.md` §"Functions"), including the
/// `flags_enabled` gate a `wrapper func` turns off.
#[allow(clippy::too_many_arguments)]
fn scan_call_value<'p>(
    name: &str,
    params: &'p [parser::Param],
    value: Value,
    bare: bool,
    flags_enabled: bool,
    flags_ended: &mut bool,
    positionals: &mut Vec<Value>,
    switches_on: &mut std::collections::HashSet<&'p str>,
    flag_values: &mut std::collections::HashMap<&'p str, Value>,
) -> Result<(), Step> {
    if flags_enabled
        && !*flags_ended
        && let Value::String(text) = &value
    {
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
        return Err(Step::Error(2));
    };
    match &declared.kind {
        ParamKind::Switch => {
            if inline.is_some() {
                note!("mesh: {name}: flag `--{flag}` is a switch and takes no value");
                return Err(Step::Error(2));
            }
            switches_on.insert(declared.name.as_str());
        }
        ParamKind::Flag(_) => {
            let Some(value) = inline else {
                note!("mesh: {name}: flag `--{flag}` requires a value (write `--{flag}=VALUE`)");
                return Err(Step::Error(2));
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
        return Err(Step::Error(2));
    };
    match &param.kind {
        ParamKind::Switch => {
            let Value::Boolean(on) = value else {
                note!(
                    "mesh: {name}: switch `{key}:` takes a boolean (`{key}: true` or `{key}: false`)"
                );
                return Err(Step::Error(2));
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
            return Err(Step::Error(2));
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
    let evaluated = eval_expr(default, 0, true, shell);
    // A `break`/`continue` the default raised has already been reported as outside
    // a loop; it produced no value, so the binding fails rather than binding the
    // placeholder and running the body. The flag is cleared here because it belongs
    // to no loop — leaving it set would break the *caller's* loop, which is the
    // escape this exists to stop.
    if shell.control.is_some() {
        shell.control = None;
        note!("mesh: {name}: could not evaluate default for `{param}`");
        return Err(Step::Error(2));
    }
    evaluated.map_err(|step| match step {
        exit @ Step::Exit(_) => exit,
        ret @ Step::Return(..) => ret,
        _ => {
            note!("mesh: {name}: could not evaluate default for `{param}`");
            Step::Error(2)
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
    // `wrapper func …` is a function header too, and everything below has to see
    // it as one: without the strip, a malformed `wrapper func f(') {` was
    // dispatched on the spot and the commands in its body ran at top level,
    // where the plain `func` spelling quarantines them through the closing `}`.
    let trimmed = strip_wrapper_marker(text.trim_start());
    let func_header = trimmed.strip_prefix("func").is_some_and(|rest| {
        rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace)
    });
    // A `func` header is judged by the brace scanner rather than the parser, so
    // that a malformed one is dispatched (and diagnosed) instead of buffering.
    let by_braces = || {
        if func_definition_is_open(trimmed) {
            Pending::Other
        } else {
            Pending::Complete
        }
    };
    match parser::parse(text) {
        Ok(parser::ParseOutcome::IncompleteHeredoc(delimiter)) => Pending::Heredoc(delimiter),
        Ok(parser::ParseOutcome::Incomplete(_)) if func_header => by_braces(),
        Ok(parser::ParseOutcome::Incomplete(_)) => Pending::Other,
        Err(_) if func_header => by_braces(),
        Ok(parser::ParseOutcome::Complete(_)) | Err(_) => Pending::Complete,
    }
}

/// Drop a leading `wrapper` marker so the `func` header scanners see the header
/// they know. Only `wrapper` immediately before `func` on the *same line* is the
/// marker — matching the parser's own contextual test — so `wrapper = 1`, a
/// command named `wrapper`, and a `wrapper` on a line of its own are returned
/// untouched and go on reading as ordinary input.
fn strip_wrapper_marker(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("wrapper") else {
        return text;
    };
    let after = rest.trim_start_matches([' ', '\t']);
    if after.len() == rest.len() {
        // No separator: this is a longer word, e.g. `wrappers`.
        return text;
    }
    let is_func = after
        .strip_prefix("func")
        .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '\t']));
    if is_func { after } else { text }
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
            // A value argument is source text like a word, and this reads the line
            // for completion rather than running anything, so its span serves.
            parser::CommandItem::Value(value) => line.get(value.span.clone()).map(str::to_owned),
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
    // Before the editor, so the highlighter can be handed the *same* settings the
    // shell writes to: `$sh.options.bold-input = false` has to reach a reedline
    // that was built once, at startup.
    let mut shell = Shell::new();
    let mut editor = Reedline::create()
        // `A` and `B` — where the prompt starts and where the user's input does.
        // The shell emits `C` and `D` itself, at `PreExec` and `PostExec`; see
        // `semantic_mark`. Both halves have to be present for a terminal to make
        // sense of the stream, and only reedline knows where it drew the prompt —
        // which is also why `$sh.options.shell-integration` is read from inside
        // `PromptMarkers` rather than deciding whether to install them here.
        .with_semantic_markers(Some(Box::new(PromptMarkers {
            options: Arc::clone(shell.vars.options()),
            plain: Osc133Markers,
            vscode: Osc633Markers,
        })))
        // Bracketed paste (`CSI ?2004 h`), per `DESIGN.md` "terminal control":
        // pasted text is *inserted*, not executed line by line. reedline's guard
        // defaults to off, so without this a paste's newlines each arrive as
        // Enter and every line but the last runs before it can be read.
        .use_bracketed_paste(true)
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
        .with_quick_completions(true)
        .with_highlighter(Box::new(input_highlighter(Arc::clone(
            shell.vars.options(),
        ))))
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
    shell
        .vars
        .set_invocation(options.name.clone(), options.args.clone());
    // The only loop that is an interactive session by itself; `-i` makes one out
    // of the others. `mesh -s` on a terminal reads commands without being one,
    // which is why this is recorded rather than derived from `isatty`.
    shell.vars.set_interactive(true);
    // And the only place that may claim the terminal: `wait_until_foreground` and
    // `ignore_interactive_signals` have both succeeded above, so this process
    // really does hold the foreground group and the signal dispositions that go
    // with it. Everything presupposing a keyboard asks this rather than the flag
    // above — see `Vars::owns_terminal`.
    shell.vars.set_owns_terminal();
    // Only this loop drains what `reap` reports, and only here can a `jobdone`
    // hook run, so only here is it worth remembering.
    shell.jobs.collect_finished();
    let (origin, source) = options.origin(true);
    shell.vars.set_origin(origin, source);
    let mut last = match run_startup_files(options, true, 0, &mut shell) {
        Step::Continue(code) | Step::Error(code) => code,
        Step::Exit(code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
        Step::Return(_, code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
    };
    // Now, and deliberately: the environment the session's sequences are chosen
    // from is read *after* the startup files have had their say and before the
    // first prompt draws anything. Leaving it to whichever write happened to come
    // first made the moment an accident, and put the dialect's read before
    // `rc.mesh` — see `session_integration`.
    let _ = session_term();
    let _ = session_integration();
    let mut pending = String::new();
    let mut gate = HeredocGate::default();
    let mut pending_history_rows = 0;
    loop {
        // Where `[N] Done` is printed, so the hook fires exactly when the shell
        // says it noticed — one call per job, in the order they were reported.
        // A job the user explicitly `wait`ed for does not come through here: its
        // status went to the caller, which is the answer the hook exists to give.
        //
        // `take_finished` rather than this reap's own result: `jobs` reaps before
        // it lists, and so does the shell before a pipeline stage that can look
        // at the table, so a job can be reported and removed before this line
        // runs. Draining the table collects those too.
        shell.jobs.reap();
        run_jobdone_hooks(&mut shell);
        if pending.is_empty() {
            run_hooks(HookEvent::PrePrompt, Vec::new(), &mut shell);
            // Again, because a `preprompt` handler can report a job itself — it
            // need only run `jobs`, which reaps before it lists. Without this the
            // notice would be printed above this prompt while its hook waited
            // for the user to submit another line, and the two are supposed to
            // be one event. Cheap when nothing was queued, which is the norm.
            run_jobdone_hooks(&mut shell);
            // After the hooks, so a `preprompt` handler that cds is reported from
            // where it left the shell rather than from where it found it. Only for
            // a fresh command: a continuation line is the same command line still
            // being typed, and the shell cannot have moved between the two.
            report_cwd(decoration(&shell.vars, Opt::CwdReport));
            shell.title_written |= set_title(
                decoration(&shell.vars, Opt::OscTitle),
                &environment_prompt_title(),
            );
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
                        // Before the `exit` hook, not just before leaving. That
                        // hook is where a session tears down what it set up —
                        // `DESIGN.md` gives closing a job-publish file as the
                        // example — so a `jobdone` after it would be writing to
                        // something already closed. `run_logout` drains too, and
                        // still needs to: it is the path every *other* exit
                        // takes, including one from a startup file that never
                        // reaches this arm.
                        //
                        // Reaped first for the reason given there: a job that
                        // ended while this line was being typed has been noticed
                        // by nobody, and `exit` forks nothing that would notice.
                        shell.jobs.reap();
                        run_jobdone_hooks(&mut shell);
                        run_hooks(
                            HookEvent::Exit,
                            vec![Value::Integer(i64::from(code))],
                            &mut shell,
                        );
                        return ExitCode::from(run_logout(options, code, &mut shell));
                    }
                    Some(Step::Continue(code) | Step::Error(code)) => last = code,
                    // Top-level `run_line` reports a stray `return` itself, so one
                    // never reaches here.
                    Some(Step::Return(..)) => unreachable!("top-level return handled in run_line"),
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

/// Draws the line being typed, bold or plain per `$sh.options.bold-input`.
///
/// Both styles are built once and chosen per repaint, so a change reaches the
/// next keystroke rather than the next session: reedline is handed the
/// highlighter when the editor is built, and there is no later chance to swap it.
/// That is what the settings live behind an `Arc` for — see [`crate::options`].
struct InputHighlighter {
    options: Arc<Options>,
    bold: SimpleMatchHighlighter,
    plain: SimpleMatchHighlighter,
}

impl Highlighter for InputHighlighter {
    fn highlight(&self, line: &str, cursor: usize) -> StyledText {
        if self.options.get(Opt::BoldInput) {
            self.bold.highlight(line, cursor)
        } else {
            self.plain.highlight(line, cursor)
        }
    }
}

/// The prompt's own marks — `A` and `B` — in the session's dialect, gated on the
/// same setting as the `C` and `D` [`semantic_mark`] writes.
///
/// Both halves or neither: a terminal that sees `A` and `B` with no `C`/`D` reads
/// everything after the prompt as still being input, which is worse than a stream
/// with no marks in it at all. reedline is handed the markers once, when the
/// editor is built, so the setting is read here rather than by choosing whether to
/// install them.
struct PromptMarkers {
    options: Arc<Options>,
    plain: Osc133Markers,
    vscode: Osc633Markers,
}

impl PromptMarkers {
    /// The dialect's markers, asked for at the moment of drawing rather than held.
    ///
    /// reedline is handed this object once, while the editor is built — which is
    /// *before* the startup files run. Choosing the dialect here instead of there
    /// is what lets an `rc.mesh` that sets `$env.TERM_PROGRAM` be honored, the same
    /// as one that sets `$env.TERM`. Raised in review on #247.
    fn marks(&self) -> &dyn SemanticPromptMarkers {
        match session_integration() {
            Integration::Osc133 => &self.plain,
            Integration::Osc633 => &self.vscode,
        }
    }
}

impl SemanticPromptMarkers for PromptMarkers {
    fn prompt_start(&self, kind: PromptKind) -> Cow<'_, str> {
        if self.options.get(Opt::ShellIntegration) {
            Cow::Owned(self.marks().prompt_start(kind).into_owned())
        } else {
            Cow::Borrowed("")
        }
    }

    fn command_input_start(&self) -> Cow<'_, str> {
        if self.options.get(Opt::ShellIntegration) {
            Cow::Owned(self.marks().command_input_start().into_owned())
        } else {
            Cow::Borrowed("")
        }
    }
}

fn input_highlighter(options: Arc<Options>) -> InputHighlighter {
    InputHighlighter {
        options,
        // Bold and nothing else: weight, not color, so the line stays readable on
        // any theme and carries no syntax claim the shell would have to keep true.
        bold: SimpleMatchHighlighter::default()
            .with_neutral_style(nu_ansi_term::Style::new().bold()),
        plain: SimpleMatchHighlighter::default(),
    }
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
    /// The executables on `$PATH` alone — what `command NAME` can actually find.
    /// Kept beside `commands` rather than subtracted from it, because a builtin's
    /// name is often a program too (`kill`, `pwd`, `printf`), and subtracting
    /// would drop the program along with the builtin.
    programs: Vec<String>,
    help: HashMap<String, CompletionSpec>,
    cache: CompletionCache,
    variables: Vec<(String, Value)>,
    /// Every **name** `whence` can answer about, which is a wider set than
    /// `commands`: the reserved words and the environment are namespaces it
    /// reports on and no command word lives in. Built once here rather than
    /// assembled per keystroke.
    names: Vec<String>,
    /// The manual pages, filled the first time a `PAGE` argument is completed.
    /// The scan walks every `man` section directory — thousands of entries — so
    /// it is not paid by a prompt that never asks for one, and the state is
    /// rebuilt each prompt, so a page installed mid-session still shows up.
    man_pages: OnceLock<Vec<String>>,
}

impl CompletionState {
    fn from_shell(shell: &Shell) -> Self {
        let mut programs: Vec<String> = Vec::new();
        // The **same** search the lookup performs, `execvp`'s fallback included:
        // with `PATH` unset a command is still found and run, so scanning nothing
        // would offer no candidate for a word `whence` reports on and `command`
        // can reach.
        {
            let path = std::env::var_os("PATH").unwrap_or_else(whence::default_path);
            for dir in std::env::split_paths(&path) {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                programs.extend(entries.flatten().filter_map(|entry| {
                    use std::os::unix::fs::PermissionsExt;
                    let metadata = entry.metadata().ok()?;
                    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                        .then(|| entry.file_name().to_string_lossy().into_owned())
                }));
            }
        }
        programs.sort();
        programs.dedup();
        let mut commands: Vec<String> = builtins::names().map(str::to_owned).collect();
        commands.extend(shell.funcs.names().map(str::to_owned));
        commands.extend(programs.iter().cloned());
        commands.sort();
        commands.dedup();
        let mut help: HashMap<_, _> = builtins::names()
            .filter_map(|name| {
                builtins::help(name).map(|text| (name.into(), CompletionSpec::from_help(&text)))
            })
            .collect();
        help.extend(shell.funcs.names().filter_map(|name| {
            shell
                .funcs
                .get(name)
                .map(|def| (name.into(), CompletionSpec::from_help(&def.help(name))))
        }));
        let variables: Vec<(String, Value)> = shell
            .vars
            .visible()
            .map(|(n, v)| (n.into(), v.clone()))
            .collect();
        // Every namespace `whence` reports on, so its argument completion covers
        // what its answers cover. `commands` is only three of them — a reserved
        // word is on no `PATH`, and mesh keeps the environment in a namespace of
        // its own, so both had to be added by name.
        let mut names = commands.clone();
        names.extend(variables.iter().map(|(name, _)| name.clone()));
        names.extend(builtins::SYNTAX_WORDS.iter().map(|word| (*word).to_owned()));
        names.extend(environ::names());
        names.sort();
        names.dedup();
        Self {
            commands,
            programs,
            help,
            cache: CompletionCache::default(),
            variables,
            names,
            man_pages: OnceLock::new(),
        }
    }

    fn man_pages(&self) -> &[String] {
        self.man_pages.get_or_init(man_pages)
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
            segment_completions(&state, &words, word)
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

/// Which resolution a completion is being asked about: the chain a bare name
/// takes, or — behind `command` — the program alone.
///
/// Completion has to answer the same question execution does, or it offers names
/// that will not run and reads the wrong command's flags: with a `func ls` defined,
/// `command ls --<Tab>` must ask the *program* for its options, not the function
/// whose whole reason for existing is that it wraps it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lookup {
    Shell,
    External,
}

/// The completions for a word that is not the first of its command line.
///
/// `command NAME …` runs `NAME`, so it completes as though the prefix were not
/// written: the word after it is a **program name**, and everything after that is
/// that program's own business, asked of that program.
///
/// Which words are `command`'s own is **the same question `command_line` answers**,
/// asked of the words actually entered — every word but the one being typed — so
/// the two cannot drift. A flag in front of the program is `command`'s, so
/// `command --<Tab>` offers `--help`; one after it belongs to the program, so
/// `command cargo --<Tab>` asks cargo. Counting words instead got both ends wrong
/// in turn: first every option prefix went to `command`'s spec, then a *rejected*
/// leading flag counted as the program, so `command -v <Tab>` completed arguments
/// for `-v` — and would have run a `$PATH` file of that name to probe its help —
/// for a line that reports a usage error rather than running anything.
fn segment_completions(state: &CompletionState, words: &[String], word: &str) -> Vec<String> {
    if words[0] != "command" {
        return argument_completions(state, words, word, Lookup::Shell);
    }
    let typed = usize::from(!word.is_empty());
    let entered = words
        .get(1..words.len().saturating_sub(typed))
        .unwrap_or_default();
    let terminated = entered.first().is_some_and(|word| word == "--");
    match command_line(entered) {
        // A program is already named, so the rest of the line is its own.
        CommandLine::External(_) => {
            let rest = if terminated { &words[2..] } else { &words[1..] };
            argument_completions(state, rest, word, Lookup::External)
        }
        // Only `command`'s own words so far, so the program is still to come. A
        // flag here is `command`'s too — unless `--` already ended its options,
        // after which the word is the program however it reads.
        CommandLine::Nothing if word.starts_with('-') && !terminated => {
            argument_completions(state, words, word, Lookup::Shell)
        }
        // The program's *name* completes from `$PATH` alone, since a builtin or a
        // function is exactly what `command` will not run.
        CommandLine::Nothing => rank_candidates(state.programs.clone(), word),
        // The words entered are `command`'s own and have already settled what the
        // line does — print its help, or report the flag — so there is no program
        // to ask and nothing past them to complete.
        CommandLine::Help | CommandLine::Unknown(_) => {
            argument_completions(state, words, word, Lookup::Shell)
        }
    }
}

/// Has a `--` already ended the options in `earlier`? Past one, a flag-looking
/// word is data — a name, for `whence` — which is the whole point of writing it.
fn terminated(earlier: &[String]) -> bool {
    earlier.iter().skip(1).any(|word| word == "--")
}

fn argument_completions(
    state: &CompletionState,
    words: &[String],
    word: &str,
    lookup: Lookup,
) -> Vec<String> {
    if let Some((option, prefix)) = word.split_once('=') {
        let context = &words[..words.len().saturating_sub(1)];
        if let Some(hint) = completion_for(state, context, lookup).value_hint(option) {
            return value_completions(state, hint, prefix)
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
        if let Some(hint) = completion_for(state, context, lookup).value_hint(option) {
            return value_completions(state, hint, word);
        }
    }
    // `whence` takes a **name**, so it completes from every namespace it reports
    // on rather than from the filesystem — commands, syntax words, the visible
    // bindings (asked about without the `$`), and the environment. A word with a
    // `/` in it is a path operand there too, so that one falls through to paths.
    //
    // A `-` prefix is an option and goes to the flag candidates — **unless a `--`
    // came first**, which is exactly what that terminator is for: past it,
    // `whence` reads every word as a name, so a program really called `--tool`
    // has to be completable the same way it is lookupable.
    if words.first().is_some_and(|first| first == "type")
        && !word.contains('/')
        && (!word.starts_with('-') || terminated(&words[..words.len().saturating_sub(1)]))
    {
        return rank_candidates(state.names.clone(), word);
    }
    let parent_help = completion_for(state, parent, lookup);
    let paths = parent_help.positional_hint().map_or_else(
        || path_completions(word),
        |hint| value_completions(state, hint, word),
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
    let mut values = completion_for(state, help_words, lookup).matching("");
    if completing_word && exact_subcommand {
        values = values
            .into_iter()
            .map(|value| format!("{word} {value}"))
            .collect();
    }
    values
}

fn value_completions(state: &CompletionState, hint: &ValueHint, prefix: &str) -> Vec<String> {
    match hint {
        ValueHint::File => path_completions_with(prefix, false),
        ValueHint::Directory => path_completions_with(prefix, true),
        ValueHint::ManPage => rank_candidates(state.man_pages().to_vec(), prefix),
        ValueHint::Enum(values) => rank_candidates(values.clone(), prefix),
    }
}

fn completion_for(state: &CompletionState, words: &[String], lookup: Lookup) -> CompletionSpec {
    let Some(command) = words.first() else {
        return CompletionSpec::default();
    };
    // Behind `command` the shell's own specs are the wrong ones: the builtin and
    // function help is what the name would have meant *without* the prefix, so the
    // program is asked directly — and a name with no program answers with nothing,
    // which is the same nothing running it would find.
    if lookup == Lookup::External {
        return state.cache.spec_for(words);
    }
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
            let marks = decoration(&shell.vars, Opt::ShellIntegration);
            // Before `C`, since a title is not output: it belongs outside the
            // region a terminal will offer to fold, next to the submission that
            // caused it. The prompt's title comes back at the next prompt, so a
            // command's title lasts exactly as long as the command.
            //
            // Still on `interactive()` rather than a setting of its own:
            // `$sh.options.osc-title` is the next change, not this one.
            shell.title_written |= set_title(
                decoration(&shell.vars, Opt::OscTitle),
                &running_title(&command),
            );
            // `E` first, so a terminal knows what is about to run before any of
            // its output arrives — the order VS Code's own integrations use.
            // Nothing is written for `OSC 133`, which has no such sequence.
            semantic_mark(marks, SemanticMark::CommandLine(command.clone()));
            // Both marks sit outside the hooks, so that everything printed
            // because this command was submitted falls inside the region they
            // bracket. A `preexec` hook that writes before `C` is folded into
            // the command line the user typed; a `postexec` hook that writes
            // after `D` lands outside the output a terminal will offer to fold.
            semantic_mark(marks, SemanticMark::OutputStart);
            run_hooks(
                HookEvent::PreExec,
                vec![Value::String(command.clone())],
                shell,
            );
            // The clock still starts here: `elapsed` is the command's own, and
            // reporting a hook's time as part of it would make the number
            // depend on what happens to be registered.
            let start = Instant::now();
            let step = run_line(&text, last, false, shell);
            let status = step.status();
            let took = start.elapsed();
            // Before the hooks, so a slow `postexec` handler cannot delay the news
            // it is not part of — the notification answers for the command, as `D`
            // does.
            notify_command_done(
                decoration(&shell.vars, Opt::CommandNotify),
                &command,
                status,
                took,
            );
            let elapsed = i64::try_from(took.as_millis()).unwrap_or(i64::MAX);
            run_hooks(
                HookEvent::PostExec,
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
            semantic_mark(
                decoration(&shell.vars, Opt::ShellIntegration),
                SemanticMark::CommandAbandoned,
            );
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
    // As in `run_batch`: `-i` decides the session's character, not its input. This
    // loop reads stdin whether or not the flag is set, so `printf … | mesh -i` is
    // an interactive session whose origin is still `stdin`.
    shell.vars.set_interactive(options.interactive);
    let (origin, source) = options.origin(false);
    shell.vars.set_origin(origin, source);
    let mut last = match run_startup_files(options, options.interactive, 0, &mut shell) {
        Step::Continue(code) | Step::Error(code) => code,
        Step::Exit(code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
        Step::Return(_, code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
    };
    // Now, and deliberately: the environment the session's sequences are chosen
    // from is read *after* the startup files have had their say and before the
    // first prompt draws anything. Leaving it to whichever write happened to come
    // first made the moment an accident, and put the dialect's read before
    // `rc.mesh` — see `session_integration`.
    let _ = session_term();
    let _ = session_integration();
    let mut pending = String::new();
    let mut gate = HeredocGate::default();
    // Discard a buffered input unit if any of its physical lines was invalid
    // UTF-8, while still using the parser to find the unit's end.
    let mut poisoned = false;
    let mut line = Vec::new();

    loop {
        line.clear();
        match read_line(&mut *stdin, &mut line, true) {
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
                    // A whole unit on its own, dropped here rather than buffered.
                    // It was still read, so the count has to advance or every
                    // later diagnostic names a line one too high up the file.
                    shell.vars.advance_input_lines(lossy.matches('\n').count());
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
        // Counted before the discard below, since a unit that is thrown away has
        // still been read: skipping it here would number every later diagnostic
        // short by its length.
        let lines = full.matches('\n').count();
        if std::mem::take(&mut poisoned) {
            // Discard the definition that contained invalid UTF-8 (error already
            // reported when the bad line was read); do not define or run it.
            shell.vars.advance_input_lines(lines);
            continue;
        }
        match run_line(&full, last, false, &mut shell) {
            Step::Exit(code) => {
                return ExitCode::from(run_logout(options, code, &mut shell));
            }
            Step::Continue(code) | Step::Error(code) => last = code,
            Step::Return(..) => unreachable!("top-level return handled in run_line"),
        }
        // After the unit runs, so its own diagnostics are numbered from where it
        // started rather than from where the next one will.
        shell.vars.advance_input_lines(lines);
    }
    // Report an incomplete unit at EOF; a poisoned one was already diagnosed.
    if !poisoned && !pending.trim().is_empty() {
        match run_line(&pending, last, false, &mut shell) {
            Step::Exit(code) => {
                return ExitCode::from(run_logout(options, code, &mut shell));
            }
            Step::Continue(code) | Step::Error(code) => last = code,
            Step::Return(..) => unreachable!("top-level return handled in run_line"),
        }
    }
    ExitCode::from(run_logout(options, last, &mut shell))
}

/// Read one line (up to and including the newline) into `out`, one byte at a
/// time so nothing beyond the newline is consumed. Returns the number of bytes
/// read; 0 signals EOF.
/// `retry_on_signal` decides what an `EINTR` means here. The interactive input
/// loop retries — a signal the shell survives is not the end of a line — while
/// `gets` gives up, so a Ctrl-C can cancel a read that would otherwise block
/// until a line or EOF arrived.
fn read_line(
    reader: &mut impl Read,
    out: &mut Vec<u8>,
    retry_on_signal: bool,
) -> io::Result<usize> {
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
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted && retry_on_signal => continue,
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
        ArgumentRecall, CommandLine, CompletionState, HeredocGate, Hook, HookEvent, Integration,
        Invocation, Lookup, MeshPrompt, NOTIFY_LIMIT, PromptMarkers, SemanticMark, Shell,
        StartupOptions, Step, TITLE_LIMIT, TimestampedHistory, argument_completions,
        body_awaits_close, command_line, command_notification, command_position,
        command_segment_words, command_words, completed_command, cwd_url, deferred_words,
        duration_words, escape_stripped_width, eval_binary, expand_history_designators,
        expansion_word, external_stage, func_definition_is_open, handle_signal, help_completions,
        history_designators, history_path_from, input_highlighter, interactive_keybindings,
        interruptible_task, last_argument, mark_sequence, needs_more_input, open_history,
        path_completions_sync, persist_logical_history, prepare_history_path, prompt_title,
        run_hooks, run_line, run_source, running_title, segment_completions, title_sequence,
        title_text, variable_completions, vscode_escaped,
    };
    use crate::builtins::{Multiplexer, through_multiplexer};
    use crate::options::Options;
    use crate::parser;
    use crate::vars::Value;
    use reedline::{
        EditCommand, Highlighter, History, HistoryItem, KeyModifiers, Prompt, PromptEditMode,
        PromptKind, Reedline, ReedlineEvent, SearchDirection, SearchQuery, SemanticPromptMarkers,
        Signal, SqliteBackedHistory,
    };
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
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
        // `key:2`, since a colon before a bare identifier is now a modifier chain.
        assert_eq!(
            last_argument("puts old; puts key:2").as_deref(),
            Some("key:2")
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
    fn dash_s_settles_the_input_without_ending_option_parsing() {
        // Unlike `-c`, `-s` takes no argument of its own, so the options after it
        // are still mesh's. Collecting them as arguments made `mesh -s -n` run the
        // input the flag says not to run.
        for order in [["-s", "-n"], ["-n", "-s"]] {
            let options = StartupOptions::parse(order.into_iter().map(str::to_owned)).unwrap();
            assert!(options.no_execute, "for {order:?}");
            assert_eq!(options.invocation, Invocation::Stdin, "for {order:?}");
            assert!(options.args.is_empty(), "for {order:?}");
        }

        // Operands still end it, and under `-s` they are this session's arguments
        // rather than a script to run — before and after a `--`.
        for form in [
            vec!["-s", "a", "-n"],
            vec!["-s", "--", "a", "-n"],
            vec!["-s", "--", "--odd-name", "-n"],
        ] {
            let options = StartupOptions::parse(form.iter().copied().map(str::to_owned)).unwrap();
            assert!(!options.no_execute, "for {form:?}");
            assert_eq!(options.invocation, Invocation::Stdin, "for {form:?}");
            assert_eq!(options.name, "mesh", "for {form:?}");
            assert_eq!(options.args, form[form.len() - 2..], "for {form:?}");
        }
    }

    #[test]
    fn dash_n_parses_in_both_spellings_and_pairs_with_any_input() {
        for spelling in ["-n", "--no-execute"] {
            let options =
                StartupOptions::parse([spelling, "check.mesh"].into_iter().map(str::to_owned))
                    .unwrap();
            assert!(options.no_execute, "for {spelling}");
            assert_eq!(
                options.invocation,
                Invocation::Script(PathBuf::from("check.mesh"))
            );
        }

        // Like `-i`, it says what to *do* with the input rather than where the
        // input comes from, so it pairs with any of them.
        let options =
            StartupOptions::parse(["-n", "-c", "puts hi"].into_iter().map(str::to_owned)).unwrap();
        assert!(options.no_execute);
        assert_eq!(
            options.invocation,
            Invocation::Command("puts hi".to_owned())
        );

        // And option parsing still stops at the first operand, so a script's own
        // `-n` reaches the script.
        let options =
            StartupOptions::parse(["check.mesh", "-n"].into_iter().map(str::to_owned)).unwrap();
        assert!(!options.no_execute);
        assert_eq!(options.args, ["-n"]);
    }

    #[test]
    fn dash_i_is_orthogonal_to_where_the_commands_come_from() {
        // `-i` says what kind of session this is; the invocation still says where
        // its commands come from. Every pairing is legal, which is the point.
        let options =
            StartupOptions::parse(["-i", "deploy.mesh", "x"].into_iter().map(str::to_owned))
                .unwrap();
        assert!(options.interactive);
        assert_eq!(
            options.invocation,
            Invocation::Script(PathBuf::from("deploy.mesh"))
        );
        assert_eq!(options.args, ["x"]);

        let options =
            StartupOptions::parse(["-i", "-c", "puts hi"].into_iter().map(str::to_owned)).unwrap();
        assert!(options.interactive);
        assert_eq!(
            options.invocation,
            Invocation::Command("puts hi".to_owned())
        );

        let options = StartupOptions::parse(["-i", "-s"].into_iter().map(str::to_owned)).unwrap();
        assert!(options.interactive);
        assert_eq!(options.invocation, Invocation::Stdin);

        // Option parsing still stops at the first operand, so a script's own
        // `-i` reaches the script rather than the shell.
        let options =
            StartupOptions::parse(["deploy.mesh", "-i"].into_iter().map(str::to_owned)).unwrap();
        assert!(!options.interactive);
        assert_eq!(options.args, ["-i"]);
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
            argument_completions(&state, &["cargo".into(), "bu".into()], "bu", Lookup::Shell),
            ["build"]
        );
        assert_eq!(
            argument_completions(&state, &["cargo".into(), "bl".into()], "bl", Lookup::Shell),
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
        let completions =
            argument_completions(&state, &["tool".into(), "co".into()], "co", Lookup::Shell);
        assert_eq!(&completions[..2], ["commit", "checkout"]);
        assert_eq!(
            argument_completions(&state, &["tool".into(), "bu".into()], "bu", Lookup::Shell),
            Vec::<String>::new()
        );
        assert!(
            argument_completions(
                &state,
                &["cargo".into(), "definitely-missing".into()],
                "definitely-missing",
                Lookup::Shell
            )
            .is_empty()
        );
    }

    #[test]
    fn a_command_prefix_completes_as_though_it_were_not_written() {
        // A session with a `puts` builtin, a `cargo` function wrapping the
        // program, and `cargo` / `cat` on `$PATH`.
        let state = CompletionState {
            commands: vec!["cargo".into(), "cat".into(), "puts".into()],
            programs: vec!["cargo".into(), "cat".into()],
            help: [
                (
                    "cargo".into(),
                    "Options:\n  --wrapper  the function's own flag\n".into(),
                ),
                // As the session derives it from the builtin's own help.
                ("command".into(), "Options:\n  --help  Print help\n".into()),
            ]
            .into(),
            ..CompletionState::default()
        };
        // The word after `command` is a program name, whether it has been started
        // or not — and a `--` before it does not change that. `puts` is missing
        // because it is a builtin: `command puts` would find no program.
        assert_eq!(
            segment_completions(&state, &["command".into()], ""),
            ["cargo", "cat"]
        );
        assert_eq!(
            segment_completions(&state, &["command".into(), "car".into()], "car"),
            ["cargo"]
        );
        assert_eq!(
            segment_completions(
                &state,
                &["command".into(), "--".into(), "car".into()],
                "car"
            ),
            ["cargo"]
        );
        // Past the program name it is the *program's* own line. The function's
        // generated help is what the bare name would have offered, and offering it
        // here would describe the wrapper rather than what is about to run.
        assert!(
            !segment_completions(
                &state,
                &["command".into(), "cargo".into(), "--w".into()],
                "--w"
            )
            .iter()
            .any(|value| value == "--wrapper"),
        );
        assert_eq!(
            argument_completions(
                &state,
                &["cargo".into(), "--w".into()],
                "--w",
                Lookup::Shell
            ),
            ["--wrapper"]
        );
        // Which words are `command`'s own is decided by position, not by the shape
        // of the word: a flag **before** the program is `command`'s…
        assert_eq!(
            segment_completions(&state, &["command".into(), "--h".into()], "--h"),
            ["--help"]
        );
        // …and one after it belongs to the program, so it must not fall back to
        // `command`'s own spec and offer `--help` for cargo's line.
        for line in [
            vec!["command".to_owned(), "cargo".into(), "--".into()],
            vec![
                "command".to_owned(),
                "--".into(),
                "cargo".into(),
                "--".into(),
            ],
        ] {
            assert_eq!(
                segment_completions(&state, &line, "--"),
                argument_completions(
                    &state,
                    &["cargo".into(), "--".into()],
                    "--",
                    Lookup::External
                ),
                "{line:?}"
            );
        }
        // A flag `command` *rejects* is not a program either: completing after it
        // as though it were would offer arguments for a line that reports a usage
        // error, and would run a `$PATH` file of that name to ask it for help.
        for entered in ["-v", "-V", "--hepl", "--help"] {
            let line = vec!["command".to_owned(), entered.to_owned()];
            assert_eq!(
                segment_completions(&state, &line, ""),
                argument_completions(&state, &line, "", Lookup::Shell),
                "{entered}"
            );
        }
    }

    #[test]
    fn a_deferred_stage_is_registered_under_the_program_it_will_run() {
        // A deferred stage joins the job table *before* its words are expanded, so
        // the cut the eager paths make in `external_stage` has to be made on the
        // spelling too — or `jobs` names `command` and `%sleep` finds nothing.
        let words = |line: &str| -> Vec<parser::Word> {
            line.split_whitespace()
                .map(|word| parser::Word {
                    pieces: vec![parser::WordPiece::Text {
                        text: word.to_owned(),
                        quote: parser::QuoteMode::Bare,
                    }],
                    qualifiers: None,
                })
                .collect()
        };
        assert_eq!(deferred_words(&words("command sleep 1")), ["sleep", "1"]);
        assert_eq!(deferred_words(&words("command -- sleep 1")), ["sleep", "1"]);
        // Nothing to strip, or nothing that will run: left as written.
        assert_eq!(deferred_words(&words("sleep 1")), ["sleep", "1"]);
        assert_eq!(deferred_words(&words("command")), ["command"]);
        assert_eq!(
            deferred_words(&words("command --help")),
            ["command", "--help"]
        );
        assert_eq!(
            deferred_words(&words("command -v ls")),
            ["command", "-v", "ls"]
        );
    }

    #[test]
    fn command_reads_only_the_words_in_front_of_the_program() {
        let words =
            |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
        // Its own `--help` is the first word after it and nothing else: everything
        // from the program name on belongs to the program.
        assert!(matches!(command_line(&words("--help")), CommandLine::Help));
        assert!(matches!(command_line(&[]), CommandLine::Nothing));
        assert!(matches!(command_line(&words("--")), CommandLine::Nothing));
        let external = |args: &[String]| match command_line(args) {
            CommandLine::External(program) => program,
            _ => panic!("expected a program"),
        };
        assert_eq!(external(&words("ls --help")), words("ls --help"));
        assert_eq!(
            external(&words("grep -- -x file")),
            words("grep -- -x file")
        );
        // `--` ends `command`'s own options and is consumed, so the word after it
        // is the program however it reads.
        assert_eq!(external(&words("-- --help")), words("--help"));
        assert_eq!(external(&words("-- -v ls")), words("-v ls"));
        // The operand is the program, with no second prefix to peel: a `command`
        // there is a program of that name to look for.
        assert_eq!(external(&words("command ls")), words("command ls"));
        // A flag-looking word in front of the program is `command`'s own, and
        // `--help` is the only one it has. Reading `-v` as a program name would
        // report "command not found: -v" for a question mesh understood, and
        // would be the meaning `command -v` had to keep once it is built.
        for line in ["-v ls", "-V ls", "--hepl", "-x"] {
            let flag = words(line)[0].clone();
            assert!(
                matches!(command_line(&words(line)), CommandLine::Unknown(word) if word == flag),
                "{line}"
            );
        }
    }

    #[test]
    fn a_command_stage_is_the_program_rather_than_a_shell_that_runs_it() {
        let words =
            |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
        // The prefix comes off, so the stage forks once — and the program is
        // looked up past the builtin of that name, which is the point.
        assert_eq!(
            external_stage(&words("command ls -l")),
            Some(words("ls -l"))
        );
        assert_eq!(
            external_stage(&words("command puts hi")),
            Some(words("puts hi"))
        );
        // A builtin still runs in the shell, and so does a `command` with no
        // program in it — `run_expanded` has the answer for those.
        assert_eq!(external_stage(&words("puts hi")), None);
        assert_eq!(external_stage(&words("command")), None);
        assert_eq!(external_stage(&words("command --help")), None);
        assert_eq!(external_stage(&words("command -v ls")), None);
        assert_eq!(external_stage(&words("ls -l")), Some(words("ls -l")));
    }

    #[test]
    fn a_page_operand_completes_from_the_manual_not_the_directory() {
        // `man l<Tab>` was offering the files in the current directory, because a
        // command with no subcommands and no typed positional falls through to
        // paths. `man`'s operand is a `PAGE`, so the manual is what it lists.
        let state = CompletionState {
            help: [(
                "man".into(),
                "Usage: man [OPTION...] [SECTION] PAGE...\n\n -w, --path  print location\n".into(),
            )]
            .into(),
            man_pages: vec!["ls".into(), "less".into(), "git-add".into()].into(),
            ..CompletionState::default()
        };

        assert_eq!(
            argument_completions(&state, &["man".into(), "l".into()], "l", Lookup::Shell),
            ["ls", "less"]
        );
        // A flag is still a flag: the operand's type does not swallow the
        // options the same help declares.
        assert_eq!(
            argument_completions(
                &state,
                &["man".into(), "--pa".into()],
                "--pa",
                Lookup::Shell
            ),
            ["--path"]
        );
    }

    #[test]
    fn whence_completes_every_namespace_it_reports_on() {
        // The promise this path makes is that its candidates cover what `whence`
        // answers about. `commands` is only three of those namespaces — a reserved
        // word is on no `PATH`, and the environment is a namespace of its own — so
        // both have to be in the set or the completion contradicts the command.
        let state = CompletionState {
            names: vec!["MESH_WHENCE_ENV".into(), "unless".into(), "xs".into()],
            ..CompletionState::default()
        };
        let whence = |word: &str| {
            argument_completions(&state, &["type".into(), word.into()], word, Lookup::Shell)
        };
        // Ranking is by subsequence, so assert the best match rather than the set:
        // what matters is that each namespace is reachable at all.
        assert_eq!(whence("unl").first().map(String::as_str), Some("unless"));
        assert_eq!(
            whence("MESH_WHENCE_").first().map(String::as_str),
            Some("MESH_WHENCE_ENV")
        );
        assert_eq!(whence("xs").first().map(String::as_str), Some("xs"));
        // A word with a `/` is a path operand for `whence` too, so it falls
        // through to the filesystem rather than being matched against names.
        assert!(!whence("./").iter().any(|value| value == "unless"));
    }

    #[test]
    fn a_terminator_makes_whence_complete_a_flag_looking_name() {
        // `whence -- --tool` looks up a program really called `--tool`, so the
        // completion has to agree: past the terminator every word is a name, and
        // sending a `-` prefix to the flag candidates there would offer `--help`
        // for a word that cannot be a flag any more.
        let state = CompletionState {
            names: vec!["--tool".into(), "unless".into()],
            ..CompletionState::default()
        };
        let complete = |words: &[&str], word: &str| {
            let owned: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
            argument_completions(&state, &owned, word, Lookup::Shell)
        };
        assert_eq!(
            complete(&["type", "--", "--to"], "--to")
                .first()
                .map(String::as_str),
            Some("--tool")
        );
        // Without the terminator a `-` prefix is still an option, so the name is
        // not offered — `whence --to` is a misspelled flag, not a lookup.
        assert!(
            !complete(&["type", "--to"], "--to")
                .iter()
                .any(|value| value == "--tool")
        );
    }

    #[test]
    fn a_builtins_own_flags_complete_from_its_generated_help() {
        // An option prefix is *not* a name, so it goes to the help-derived
        // candidates — which listed only `--help` until the generated `Options:`
        // block started naming a builtin's own flags. Built here the way
        // `CompletionState::from_shell` builds it, so the two cannot drift.
        let spec = |name: &str| {
            (
                name.to_owned(),
                crate::completion::CompletionSpec::from_help(
                    &crate::builtins::help(name).expect("a builtin"),
                ),
            )
        };
        let state = CompletionState {
            help: [spec("type"), spec("prompt")].into(),
            ..CompletionState::default()
        };
        let flags = |command: &str, word: &str| {
            argument_completions(&state, &[command.into(), word.into()], word, Lookup::Shell)
        };
        assert_eq!(flags("type", "-P"), ["-P"]);
        assert_eq!(flags("type", "--q"), ["--quiet"]);
        assert_eq!(flags("prompt", "--r"), ["--reset"]);
        // `--help` is still there, and still last.
        assert!(flags("type", "--").contains(&"--help".to_owned()));
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
            argument_completions(
                &state,
                &["cat".into(), prefix.clone()],
                &prefix,
                Lookup::Shell
            ),
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
            argument_completions(
                &state,
                &["vi".into(), prefix.clone()],
                &prefix,
                Lookup::Shell
            ),
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
                &prefix,
                Lookup::Shell
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
                &prefix,
                Lookup::Shell
            ),
            [format!("{}/", child.display())]
        );
        assert_eq!(
            argument_completions(
                &state,
                &["tool".into(), "--color=a".into()],
                "--color=a",
                Lookup::Shell
            ),
            ["--color=auto", "--color=always"]
        );
        assert_eq!(
            argument_completions(
                &state,
                &["tool".into(), "--color=nv".into()],
                "--color=nv",
                Lookup::Shell
            ),
            ["--color=never"]
        );
        assert!(
            argument_completions(
                &state,
                &["tool".into(), "--color=NV".into()],
                "--color=NV",
                Lookup::Shell
            )
            .is_empty()
        );
        let fuzzy_prefix = format!("{}/ft", dir.display());
        assert_eq!(
            argument_completions(
                &state,
                &["tool".into(), "--file".into(), fuzzy_prefix.clone()],
                &fuzzy_prefix,
                Lookup::Shell
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
        // This asserts on what the probe *found*, so it must not also be asserting
        // that a loaded machine got round to running it within the budget a prompt
        // uses. Generous rather than raised for everyone: the shell still gives up
        // after two seconds, this test just refuses to call that a result.
        crate::completion::set_probe_budget(Duration::from_secs(60));
        let state = CompletionState::default();

        assert_eq!(
            argument_completions(
                &state,
                &[command, "build".into(), "--color".into(), "a".into()],
                "a",
                Lookup::Shell
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
        assert_eq!(title.chars().count(), TITLE_LIMIT);
        assert!(title.ends_with('…'), "{title}");
        // The cut counts characters, not bytes, so a multi-byte title is not
        // truncated early or split down the middle of one.
        let wide = "日".repeat(200);
        let wide_title = title_text(&wide);
        assert_eq!(wide_title.chars().count(), TITLE_LIMIT);
        assert!(wide_title.starts_with("日日"), "{wide_title}");
        // A title that fits keeps its exact text, ellipsis and all.
        assert_eq!(title_text("puts hi"), "puts hi");
        assert_eq!(
            title_text(&"y".repeat(TITLE_LIMIT)),
            "y".repeat(TITLE_LIMIT)
        );
    }

    #[test]
    fn the_marks_carry_the_dialect_the_session_speaks() {
        // The same boundaries under a different number. VS Code parses `133` too,
        // so one dialect is sent, not both — it would count every command twice.
        for (dialect, code) in [(Integration::Osc133, "133"), (Integration::Osc633, "633")] {
            assert_eq!(
                mark_sequence(dialect, &SemanticMark::OutputStart).as_deref(),
                Some(format!("\x1b]{code};C\x1b\\").as_str())
            );
            assert_eq!(
                mark_sequence(dialect, &SemanticMark::CommandDone(3)).as_deref(),
                Some(format!("\x1b]{code};D;3\x1b\\").as_str())
            );
            assert_eq!(
                mark_sequence(dialect, &SemanticMark::CommandAbandoned).as_deref(),
                Some(format!("\x1b]{code};D\x1b\\").as_str())
            );
        }
    }

    #[test]
    fn only_vs_codes_dialect_carries_the_command_line() {
        // `OSC 133` has no sequence for it, so there is nothing to send rather than
        // something to invent.
        assert_eq!(
            mark_sequence(
                Integration::Osc133,
                &SemanticMark::CommandLine("puts hi".to_owned())
            ),
            None
        );
        assert_eq!(
            mark_sequence(
                Integration::Osc633,
                &SemanticMark::CommandLine("puts hi".to_owned())
            )
            .as_deref(),
            Some("\x1b]633;E;puts hi\x1b\\")
        );
    }

    #[test]
    fn a_command_line_cannot_end_the_sequence_carrying_it() {
        // `;` delimits the payload, so an unescaped one would end `E` early and
        // leave the rest of the command on screen — and `sleep 1; puts hi` is an
        // ordinary thing to type.
        assert_eq!(vscode_escaped("sleep 1; puts hi"), "sleep 1\\x3b puts hi");
        // The backslash that introduces the escape needs escaping itself, or a
        // literal `\x3b` in an argument would decode as a semicolon nobody typed.
        assert_eq!(vscode_escaped(r"grep \x3b"), r"grep \\x3b");
        assert_eq!(vscode_escaped(r"C:\tmp"), r"C:\\tmp");
        // Control characters go as hex too: a pasted two-line command carries a
        // newline, and an `ESC` would otherwise start a sequence of its own.
        assert_eq!(vscode_escaped("puts a\nputs b"), "puts a\\x0aputs b");
        assert_eq!(
            vscode_escaped("puts \x1b]0;x\x07"),
            "puts \\x1b]0\\x3bx\\x07"
        );
        // Ordinary text is left alone, including non-ASCII.
        assert_eq!(vscode_escaped("puts café"), "puts café");
    }

    #[test]
    fn a_command_earns_a_notification_by_taking_long_enough() {
        let notification = |seconds| {
            command_notification(
                Some(OsStr::new("xterm")),
                Multiplexer::None,
                "cargo build",
                0,
                Duration::from_secs(seconds),
                Duration::from_secs(10),
            )
        };
        // The threshold is the whole feature: below it a notification is noise for
        // a command the user watched finish.
        assert_eq!(notification(0), None);
        assert_eq!(notification(9), None);
        assert_eq!(
            notification(10).as_deref(),
            Some("\x1b]9;mesh: cargo build — done in 10s\x07"),
            "the boundary belongs to the notification, not to silence"
        );
        assert_eq!(
            notification(42).as_deref(),
            Some("\x1b]9;mesh: cargo build — done in 42s\x07")
        );
    }

    #[test]
    fn a_notification_says_how_it_ended() {
        let notification = |status| {
            command_notification(
                Some(OsStr::new("xterm")),
                Multiplexer::None,
                "  make test  ",
                status,
                Duration::from_secs(75),
                Duration::from_secs(10),
            )
        };
        // The status is the reason to look: a failure that finished while you were
        // away is exactly what a notification is for. Surrounding space goes, since
        // the user's spacing is not news.
        assert_eq!(
            notification(0).as_deref(),
            Some("\x1b]9;mesh: make test — done in 1m15s\x07")
        );
        assert_eq!(
            notification(2).as_deref(),
            Some("\x1b]9;mesh: make test — exit 2 in 1m15s\x07")
        );
        assert_eq!(
            notification(130).as_deref(),
            Some("\x1b]9;mesh: make test — exit 130 in 1m15s\x07")
        );
    }

    #[test]
    fn a_notification_goes_only_where_osc_is_understood() {
        let notification = |term: Option<&str>| {
            command_notification(
                term.map(OsStr::new),
                Multiplexer::None,
                "sleep 30",
                0,
                Duration::from_secs(30),
                Duration::from_secs(10),
            )
        };
        // The same gate as the title, for the same reason: a terminal that
        // mis-parses `OSC` prints the payload instead of raising anything.
        assert!(notification(Some("xterm-256color")).is_some());
        assert!(notification(Some("alacritty")).is_some());
        // Inside a multiplexer it is worth sending: tmux swallows it without
        // `allow-passthrough` and forwards it with, and neither prints it.
        assert!(notification(Some("tmux-256color")).is_some());
        assert!(notification(Some("screen.xterm-256color")).is_some());
        assert_eq!(notification(Some("linux")), None);
        assert_eq!(notification(Some("dumb")), None);
        assert_eq!(notification(Some("st52")), None);
        assert_eq!(notification(None), None);
    }

    #[test]
    fn a_notification_cannot_carry_a_sequence_of_its_own() {
        // The command line is the user's text, so the shared payload rule applies
        // here as it does to the title: an `ESC` in it would otherwise end mesh's
        // sequence and start another.
        let notification = command_notification(
            Some(OsStr::new("xterm")),
            Multiplexer::None,
            "puts \x1b]9;evil\x07",
            0,
            Duration::from_secs(11),
            Duration::from_secs(10),
        );
        assert_eq!(
            notification.as_deref(),
            Some("\x1b]9;mesh: puts  ]9;evil — done in 11s\x07")
        );
    }

    #[test]
    fn a_long_command_keeps_the_part_worth_reading() {
        // Raised in review on #242: the message was assembled and *then* cut to the
        // limit, so a long command line pushed the outcome and the duration off the
        // end — the two things the notification exists to say.
        let command = "cargo build --features ".to_owned() + &"a,".repeat(200);
        let notification = command_notification(
            Some(OsStr::new("xterm")),
            Multiplexer::None,
            &command,
            2,
            Duration::from_secs(95),
            Duration::from_secs(10),
        )
        .expect("a long command still notifies");
        assert!(
            notification.ends_with("— exit 2 in 1m35s\x07"),
            "the outcome was cut off: {notification:?}"
        );
        assert!(
            notification.starts_with("\x1b]9;mesh: cargo build --features a,a,"),
            "the command was not kept: {notification:?}"
        );
        // Still bounded, and the command is where the ellipsis lands.
        assert!(
            notification.chars().count() <= NOTIFY_LIMIT + "\x1b]9;\x07".len(),
            "{} chars",
            notification.chars().count()
        );
        assert!(notification.contains('…'), "{notification:?}");
    }

    #[test]
    fn a_notification_is_wrapped_for_tmux_to_forward() {
        // A multiplexer parses the stream itself, so an `OSC` it does not implement
        // is consumed rather than passed on. tmux forwards a `DCS tmux;` envelope
        // with the payload's `ESC`s doubled — also raised in review on #242, where
        // the raw form was sent and could never arrive.
        let wrapped = command_notification(
            Some(OsStr::new("tmux-256color")),
            Multiplexer::Tmux,
            "make",
            0,
            Duration::from_secs(20),
            Duration::from_secs(10),
        )
        .expect("tmux still notifies");
        assert_eq!(
            wrapped,
            "\x1bPtmux;\x1b\x1b]9;mesh: make — done in 20s\x07\x1b\\"
        );
        // Outside a multiplexer the sequence goes as it is.
        assert_eq!(
            command_notification(
                Some(OsStr::new("tmux-256color")),
                Multiplexer::None,
                "make",
                0,
                Duration::from_secs(20),
                Duration::from_secs(10),
            )
            .as_deref(),
            Some("\x1b]9;mesh: make — done in 20s\x07")
        );
        // Screen is left alone deliberately: its passthrough has quirks mesh cannot
        // test against here, and a wrong envelope prints.
        assert_eq!(
            through_multiplexer("\x1b]9;hi\x07", Multiplexer::Screen),
            "\x1b]9;hi\x07"
        );
    }

    #[test]
    fn durations_read_the_way_people_say_them() {
        let words = |seconds| duration_words(Duration::from_secs(seconds));
        assert_eq!(words(0), "0s");
        assert_eq!(words(59), "59s");
        assert_eq!(words(60), "1m0s");
        assert_eq!(words(90), "1m30s");
        assert_eq!(words(3599), "59m59s");
        // Seconds drop once there are hours: more precision than anyone reads off a
        // notification.
        assert_eq!(words(3600), "1h0m");
        assert_eq!(words(7505), "2h5m");
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
        let options = Arc::new(Options::default());
        let highlighted = input_highlighter(options).highlight("puts hello", 10);

        assert_eq!(highlighted.buffer.len(), 1);
        assert_eq!(highlighted.buffer[0].0, nu_ansi_term::Style::new().bold());
        assert_eq!(highlighted.buffer[0].1, "puts hello");
    }

    /// `A` and `B` go with `C` and `D`. A terminal handed the prompt's marks but
    /// no command marks reads everything after the prompt as still being input,
    /// which is worse than a stream with none — so `shell-integration` has to
    /// silence reedline's half too, not just the shell's.
    #[test]
    fn turning_off_shell_integration_silences_the_prompt_marks_too() {
        let options = Arc::new(Options::default());
        let markers = PromptMarkers {
            options: Arc::clone(&options),
            plain: reedline::Osc133Markers,
            vscode: reedline::Osc633Markers,
        };

        assert_eq!(
            markers.prompt_start(PromptKind::Primary),
            "\x1b]133;A;k=i\x1b\\"
        );
        assert_eq!(markers.command_input_start(), "\x1b]133;B\x1b\\");

        options
            .assign(
                "$sh.options.shell-integration",
                "shell-integration",
                &Value::Boolean(false),
            )
            .expect("shell-integration is a setting");
        assert_eq!(markers.prompt_start(PromptKind::Primary), "");
        assert_eq!(markers.prompt_start(PromptKind::Secondary), "");
        assert_eq!(markers.command_input_start(), "");
    }

    /// The highlighter is built once, when the session starts, so the setting has
    /// to be read at draw time — not baked into the style at construction.
    #[test]
    fn turning_off_bold_input_reaches_the_highlighter_already_built() {
        let options = Arc::new(Options::default());
        let highlighter = input_highlighter(Arc::clone(&options));

        options
            .assign(
                "$sh.options.bold-input",
                "bold-input",
                &Value::Boolean(false),
            )
            .expect("bold-input is a setting");
        let plain = highlighter.highlight("puts hello", 10);
        assert_eq!(plain.buffer.len(), 1);
        assert_eq!(plain.buffer[0].0, nu_ansi_term::Style::default());
        assert_eq!(plain.buffer[0].1, "puts hello");

        // And back: the same highlighter bolds again, so the setting is a live
        // read rather than a one-way latch.
        options
            .assign(
                "$sh.options.bold-input",
                "bold-input",
                &Value::Boolean(true),
            )
            .expect("bold-input is a setting");
        let bold = highlighter.highlight("puts hello", 10);
        assert_eq!(bold.buffer[0].0, nu_ansi_term::Style::new().bold());
    }

    #[test]
    fn named_hooks_replace_in_place_and_run_before_the_prompt() {
        let marker = std::env::temp_dir().join(format!("mesh-on-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut shell = Shell::new();
        let script = format!(
            "func first() {{ false }}\nfunc second() {{ touch '{}' }}\non preprompt refresh first\non preprompt refresh second\n",
            marker.display()
        );
        assert_eq!(run_line(&script, 0, false, &mut shell), Step::Continue(0));
        assert_eq!(
            shell.prompt.hooks,
            vec![Hook {
                event: HookEvent::PrePrompt,
                name: "refresh".into(),
                function: "second".into(),
            }]
        );
        run_hooks(HookEvent::PrePrompt, Vec::new(), &mut shell);
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
            run_line("on preprompt p h\n", 0, false, &mut shell),
            Step::Continue(0)
        );
        assert_eq!(run_line("false\n", 0, false, &mut shell), Step::Continue(1));
        assert_eq!(shell.vars.status(), 1);
        run_hooks(HookEvent::PrePrompt, Vec::new(), &mut shell);
        assert_eq!(shell.vars.status(), 1, "a hook replaced the user's status");
    }

    #[test]
    fn input_the_parser_rejects_publishes_its_status() {
        // A parse failure never reaches `run_recorded`, so without publishing it
        // here the shell would carry 2 to the next command while `$sh.status`
        // still reported whatever ran before.
        let mut shell = Shell::new();
        assert_eq!(run_line("true\n", 0, false, &mut shell), Step::Continue(0));
        // `Step::Error`: a parse failure is an invalid program, so a `$(…)` around
        // one must not take its empty output as an answer. Status unchanged.
        assert_eq!(run_line("nope (\n", 0, false, &mut shell), Step::Error(2));
        assert_eq!(shell.vars.status(), 2);
    }

    #[test]
    fn command_hooks_receive_command_status_and_elapsed_arguments() {
        let mut shell = Shell::new();
        assert_eq!(
            run_line(
                "func before(cmd) { puts $cmd }\nfunc after(cmd, status, elapsed) { puts $cmd $status $elapsed }\non preexec log before\non postexec log after",
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
            // An unknown flag is an invalid call, so it reports `Step::Error` — the
            // status and the recovery are the same, but a `$(…)` around one must
            // not take its empty output as an answer.
            Step::Error(2),
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

        let expansion = expansion_word(&word.value, 0, false, &mut shell).expect("a plain word");
        assert_eq!(
            crate::expand::expand_values(vec![expansion], &shell.vars),
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
        // Invalid source, so `Step::Error` rather than a command's status.
        assert_eq!(step, Some(Step::Error(2)));
        assert!(pending.is_empty());
        // `f` was never defined.
        assert!(shell.funcs.get("f").is_none());
    }

    #[test]
    fn a_bare_return_at_top_level_is_reported() {
        // Outside a function, `return` is a recoverable error (status 1), not an
        // unwind — `run_line` reports it and continues rather than propagating it.
        // `Step::Error` rather than `Continue`: a `return` with nothing to return
        // from is an invalid program, so a `$(…)` around one must not take the
        // empty output as an answer. The status and the recovery are unchanged.
        let mut shell = Shell::new();
        assert_eq!(run_line("return", 0, false, &mut shell), Step::Error(1));
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
