//! The read / tokenize / dispatch loop.
//!
//! Interactive (TTY) input goes through [`reedline`] for line editing, history,
//! and Ctrl-C/Ctrl-D handling. Piped / non-interactive input keeps the std-only
//! unbuffered fd-0 byte reader, so a spawned child still inherits any bytes that
//! follow its command line and the integration tests need no terminal.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use reedline::{
    Color, ColumnarMenu, Completer, EditCommand, Emacs, History, HistoryItem, HistoryItemId,
    HistorySessionId, KeyCode, KeyModifiers, Keybindings, MenuBuilder, Prompt, PromptEditMode,
    PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, SearchDirection, SearchQuery,
    Signal, SimpleMatchHighlighter, Span, SqliteBackedHistory, Suggestion,
    default_emacs_keybindings,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::builtins::{self, Builtin};
use crate::completion::{CompletionCache, CompletionSpec, ValueHint, rank_candidates};
use crate::expand::{Piece, VarRef, Word};
use crate::funcs::{FuncDef, Funcs};
use crate::vars::{RegexValue, Value, Vars};
use crate::{exec, expand, parser};

const COMPLETION_MENU: &str = "completion_menu";

/// The mutable shell session threaded through the run loop: variable scopes,
/// defined functions, and the job table.
struct Shell {
    vars: Vars,
    funcs: Funcs,
    jobs: exec::JobTable,
    control: Option<parser::ControlKind>,
    loop_depth: usize,
    prompt: PromptConfig,
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
            loop_depth: 0,
            prompt: PromptConfig::default(),
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
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--mesh-background-redirect") {
        return exec::run_background_redirect(args.collect());
    }
    let options = match StartupOptions::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("mesh: {message}");
            return ExitCode::from(2);
        }
    };
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        run_interactive(&options)
    } else {
        run_piped(&options)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct StartupOptions {
    login: bool,
    no_rc: bool,
    rc_file: Option<PathBuf>,
    save_history: bool,
}

impl Default for StartupOptions {
    fn default() -> Self {
        Self {
            login: false,
            no_rc: false,
            rc_file: None,
            save_history: true,
        }
    }
}

impl StartupOptions {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-l" | "--login" => options.login = true,
                "--norc" => options.no_rc = true,
                "--no-save-history" | "--no-history" => options.save_history = false,
                "--rcfile" => {
                    let path = args
                        .next()
                        .ok_or_else(|| "--rcfile requires a file path".to_owned())?;
                    options.rc_file = Some(path.into());
                }
                _ => return Err(format!("unknown option `{arg}`")),
            }
        }
        Ok(options)
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
    if interactive && !options.no_rc {
        if let Some(path) = options
            .rc_file
            .clone()
            .or_else(|| config_dir().map(|dir| dir.join("rc.mesh")))
        {
            files.push(path);
        }
    }
    files
}

fn run_config_file(path: &Path, last: u8, shell: &mut Shell) -> Step {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Step::Continue(last),
        Err(error) => {
            eprintln!("mesh: {}: {error}", path.display());
            return Step::Continue(1);
        }
    };
    run_line(&text, last, false, shell)
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
    if !options.login {
        return last;
    }
    let Some(path) = config_dir().map(|dir| dir.join("logout.mesh")) else {
        return last;
    };
    let _ = run_config_file(&path, last, shell);
    last
}

/// What to do after handling one input line.
#[derive(Debug, PartialEq)]
enum Step {
    /// A line ran; carry this status as the new "last status".
    Continue(u8),
    /// `exit` was invoked; leave the shell with this status.
    Exit(u8),
    /// `return` was invoked; unwind the current function with this status. At top
    /// level (no function) `run_line` reports it as a recoverable error instead.
    Return(u8),
}

/// Parse and run one input unit against the session. `in_function` is true while
/// running a function body: there a `return` unwinds; at top level it is a
/// recoverable error.
fn run_line(text: &str, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    let step = match parser::parse(text) {
        Ok(parser::ParseOutcome::Complete(source)) => run_source(&source, last, in_function, shell),
        Ok(parser::ParseOutcome::Incomplete) => {
            eprintln!("mesh: syntax error: unexpected end of input");
            Step::Continue(2)
        }
        Err(error) => {
            eprintln!("mesh: {error}");
            Step::Continue(2)
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
    for statement in &source.statements {
        match run_statement(statement, status, in_function, shell) {
            Step::Continue(code) => status = code,
            flow => return flow,
        }
        if shell.control.is_some() {
            break;
        }
    }
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
        eprintln!("mesh: background conditional lists are not supported yet");
        return Step::Continue(2);
    }
    let mut step = run_executable(&node.first, background, last, in_function, shell);
    for (op, executable) in &node.rest {
        let Step::Continue(status) = step else {
            return step;
        };
        let run = match op {
            parser::AndOrOp::And => status == 0,
            parser::AndOrOp::Or => status != 0,
        };
        if run {
            step = run_executable(executable, background, status, in_function, shell);
        }
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
    match node {
        Pipeline(pipeline) => run_ast_pipeline(pipeline, background, last, shell),
        Assignment {
            pattern,
            append,
            value,
        } => match eval_expr(value, last, in_function, shell) {
            Ok(value) => {
                let result = if *append {
                    let parser::BindingPattern::Name(name) = pattern else {
                        unreachable!("the parser restricts += to names")
                    };
                    shell.vars.append(name, value)
                } else {
                    bind_pattern(pattern, &value, &mut shell.vars)
                };
                result.map_or_else(
                    |error| {
                        eprintln!("mesh: {error}");
                        Step::Continue(1)
                    },
                    |_| Step::Continue(0),
                )
            }
            Err(step) => step,
        },
        Function {
            name,
            parameters,
            body,
        } => {
            if name == "func" || name == "return" || builtins::is_builtin(name) {
                eprintln!("mesh: func: `{name}` is a reserved name and cannot be a function name");
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
        Control { kind, value, guard } => {
            match guard_allows(guard.as_ref(), last, in_function, shell) {
                Ok(true) => {}
                Ok(false) => return Step::Continue(last),
                Err(step) => return step,
            }
            match kind {
                parser::ControlKind::Return => {
                    if !in_function {
                        eprintln!("mesh: return: not inside a function");
                        return Step::Continue(1);
                    }
                    let code = value
                        .as_ref()
                        .map(|v| eval_expr(v, last, in_function, shell))
                        .transpose();
                    match code {
                        Ok(Some(Value::Integer(code))) => Step::Return(code.rem_euclid(256) as u8),
                        Ok(None) => Step::Return(last),
                        Ok(Some(_)) => {
                            eprintln!("mesh: return: numeric argument required");
                            Step::Continue(2)
                        }
                        Err(step) => step,
                    }
                }
                parser::ControlKind::Break | parser::ControlKind::Continue => {
                    if shell.loop_depth == 0 {
                        eprintln!(
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
                Ok(false) => return Step::Continue(last),
                Err(step) => return step,
            }
            if let parser::Expr::Scalar(word) = expression
                && word.value.pieces.iter().any(|piece| match piece {
                    parser::WordPiece::Text { quote, .. }
                    | parser::WordPiece::Variable { quote, .. } => {
                        !matches!(quote, parser::QuoteMode::Bare)
                    }
                })
            {
                return run_pipeline(
                    vec![Stage {
                        words: vec![expansion_word(&word.value)],
                        redirs: Vec::new(),
                        pipe_stderr: false,
                    }],
                    false,
                    last,
                    shell,
                );
            }
            match eval_expr(expression, last, in_function, shell) {
                Ok(_) => Step::Continue(0),
                Err(step) => step,
            }
        }
    }
}

fn guard_allows(
    guard: Option<&parser::Guard>,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<bool, Step> {
    match guard {
        None => Ok(true),
        Some(guard) => eval_expr(&guard.condition, last, in_function, shell)
            .map(|value| truthy(&value) != guard.unless),
    }
}

fn run_ast_if(node: &parser::IfExpr, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    let code = match condition_status(&node.condition, last, in_function, shell) {
        Ok(code) => code,
        Err(step) => return step,
    };
    if code == 0 {
        run_source(&node.then_body, 0, in_function, shell)
    } else {
        match &node.else_branch {
            Some(parser::ElseBranch::If(next)) => run_ast_if(next, code, in_function, shell),
            Some(parser::ElseBranch::Block(body)) => run_source(body, 0, in_function, shell),
            None => Step::Continue(0),
        }
    }
}

fn run_ast_match(node: &parser::MatchExpr, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    let subject = match eval_expr(&node.value, last, in_function, shell) {
        Ok(value) => value,
        Err(step) => return step,
    };
    for arm in &node.arms {
        let bindings = match match_bindings(&arm.pattern, &subject, last, in_function, shell) {
            Ok(Some(bindings)) => bindings,
            Ok(None) => continue,
            Err(step) => return step,
        };
        let snapshot = shell.vars.active_snapshot();
        commit_bindings(bindings, &mut shell.vars);
        if let Some(guard) = &arm.guard {
            match eval_expr(guard, last, in_function, shell) {
                Ok(value) if truthy(&value) => {}
                Ok(_) => {
                    shell.vars.restore_active(snapshot);
                    continue;
                }
                Err(step) => return step,
            }
        }
        return run_source(&arm.body, 0, in_function, shell);
    }
    Step::Continue(0)
}

fn condition_status(
    condition: &parser::Executable,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<u8, Step> {
    if let parser::Executable::Assignment {
        pattern,
        append: false,
        value,
    } = condition
        && matches!(pattern, parser::BindingPattern::List(_))
    {
        let value = eval_expr(value, last, in_function, shell)?;
        return match pattern_bindings(pattern, &value) {
            Ok(Some(bindings)) => {
                commit_bindings(bindings, &mut shell.vars);
                Ok(0)
            }
            Ok(None) => Ok(1),
            Err(message) => Err(runtime_message(message)),
        };
    }
    if let parser::Executable::Expression {
        expression,
        guard: None,
    } = condition
    {
        return eval_expr(expression, last, in_function, shell)
            .map(|value| if truthy(&value) { 0 } else { 1 });
    }
    match run_executable(condition, false, last, in_function, shell) {
        Step::Continue(code) => Ok(code),
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
        eprintln!("mesh: for: {message}");
        return Step::Continue(2);
    }
    let value = match eval_expr(iterable, last, in_function, shell) {
        Ok(v) => v,
        Err(step) => return step,
    };
    let values = match iteration_values(value, bindings.len()) {
        Ok(values) => values,
        Err(message) => return runtime_message(message),
    };
    let mut status = 0;
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
        match shell.control.take() {
            Some(parser::ControlKind::Break) => break,
            Some(parser::ControlKind::Continue) => continue,
            Some(parser::ControlKind::Return) => unreachable!(),
            None => {}
        }
    }
    shell.loop_depth -= 1;
    Step::Continue(status)
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

fn bind_pattern(
    pattern: &parser::BindingPattern,
    value: &Value,
    vars: &mut Vars,
) -> Result<(), String> {
    let bindings = pattern_bindings(pattern, value)?
        .ok_or_else(|| "value does not match binding pattern".to_string())?;
    validate_bindings(&bindings)?;
    commit_bindings(bindings, vars);
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
        if name == "env" {
            return Err("`env` is a reserved name and cannot be a binding".into());
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
    target: Word,
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
            Ok(false) => return Step::Continue(last),
            Err(step) => return step,
        }
        let mut words = Vec::new();
        let mut redirs = Vec::new();
        for item in &command.items {
            match item {
                parser::CommandItem::Word(word) => words.push(expansion_word(&word.value)),
                parser::CommandItem::Redirect { kind, target, .. } => redirs.push(Redir {
                    kind: match kind {
                        parser::RedirectKind::Input => exec::RedirKind::In,
                        parser::RedirectKind::Output => exec::RedirKind::Out,
                        parser::RedirectKind::Append => exec::RedirKind::Append,
                        parser::RedirectKind::Heredoc => {
                            eprintln!("mesh: heredoc execution is not supported yet");
                            return Step::Continue(1);
                        }
                    },
                    target: expansion_word(&target.value),
                }),
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

fn parse_bound(value: &str) -> Option<i64> {
    (!value.is_empty()).then(|| value.parse().expect("parser validated list bound"))
}

fn eval_expr(
    expr: &parser::Expr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    use parser::{BinaryOp as B, Expr as E, ListItem, MapItem, UnaryOp as U};
    match expr {
        E::Scalar(word) => expand::expand_values(vec![expansion_word(&word.value)], &shell.vars)
            .map_err(|e| {
                eprintln!("mesh: {e}");
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
                eprintln!("mesh: {error}");
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
        } => number(&eval_expr(expression, last, in_function, shell)?)
            .and_then(|n| n.checked_neg().ok_or_else(|| "numeric overflow".into()))
            .map(Value::Integer)
            .map_err(|m| {
                eprintln!("mesh: {m}");
                Step::Continue(1)
            }),
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
            eval_binary(l, *op, r).map_err(|m| {
                eprintln!("mesh: {m}");
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
                        eprintln!("mesh: $env.{name}: not set");
                        Step::Continue(1)
                    });
            }
            match eval_expr(value, last, in_function, shell)? {
                Value::Map(entries) => map_lookup(&entries, name),
                _ => runtime_error(format!("member access .{name} requires a map")),
            }
        }
        E::Index { value, index } => {
            let value = eval_expr(value, last, in_function, shell)?;
            if let E::Range {
                start,
                end,
                inclusive,
            } = index.as_ref()
            {
                let mut bound = |expression: &Option<Box<E>>| -> Result<Option<i64>, Step> {
                    expression
                        .as_ref()
                        .map(|expression| {
                            eval_expr(expression, last, in_function, shell)
                                .and_then(|value| number(&value).map_err(runtime_message))
                        })
                        .transpose()
                };
                return match value {
                    Value::List(values) => Ok(Value::List(
                        expand::slice(&values, bound(start)?, bound(end)?, *inclusive).to_vec(),
                    )),
                    Value::String(_)
                    | Value::Integer(_)
                    | Value::Boolean(_)
                    | Value::Regex(_)
                    | Value::Glob(_) => runtime_error("cannot slice a scalar value"),
                    Value::Map(_) => runtime_error("cannot slice a map value"),
                };
            }
            let index_value = eval_expr(index, last, in_function, shell)?;
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
                            eprintln!("mesh: list index {index} out of range");
                            Step::Continue(1)
                        })
                }
                Value::String(_)
                | Value::Integer(_)
                | Value::Boolean(_)
                | Value::Regex(_)
                | Value::Glob(_) => runtime_error("cannot index a scalar value"),
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
            if arguments.is_some() {
                return runtime_error(format!(
                    "modifier :{name} arguments are not implemented yet"
                ));
            }
            let mut value = eval_expr(value, last, in_function, shell)?;
            if let Value::Regex(regex) = &mut value {
                match name.as_str() {
                    "i" | "ignorecase" => regex.case_insensitive = true,
                    "m" | "multiline" => regex.multi_line = true,
                    "s" | "dotall" => regex.dot_matches_new_line = true,
                    "x" | "extended" => regex.ignore_whitespace = true,
                    _ => {
                        return runtime_error(format!("modifier :{name} is not valid for a regex"));
                    }
                }
                compile_regex(regex).map_err(runtime_message)?;
                return Ok(value);
            }
            let Some(modifier) = expand::Modifier::from_name(name) else {
                return runtime_error(format!("modifier :{name} is not implemented yet"));
            };
            expand::apply_modifier(value, modifier)
                .map_err(|error| runtime_message(error.to_string()))
        }
        E::If(node) => eval_if_expr(node, last, in_function, shell),
        E::Match(node) => eval_match_expr(node, last, in_function, shell),
        E::For {
            bindings,
            iterable,
            body,
        } => eval_for_expr(bindings, iterable, body, last, in_function, shell),
        E::BackgroundJob(pipeline) => match run_ast_pipeline(pipeline, true, last, shell) {
            Step::Continue(code) => Ok(Value::Integer(i64::from(code))),
            step => Err(step),
        },
        E::Capture(source) => capture_source(source, last, in_function, shell),
        E::Range {
            start,
            end,
            inclusive,
        } => {
            let start = range_endpoint(start.as_deref(), 0, last, in_function, shell)?;
            let Some(end) = end.as_deref() else {
                return runtime_error("an open-ended range cannot be used as a value");
            };
            let end = range_endpoint(Some(end), 0, last, in_function, shell)?;
            let stop = if *inclusive {
                end.checked_add(1)
                    .ok_or_else(|| runtime_message("range endpoint overflow"))?
            } else {
                end
            };
            Ok(Value::List((start..stop).map(Value::Integer).collect()))
        }
        E::Call { callee, arguments } => eval_call(callee, arguments, last, in_function, shell),
        E::Lambda { .. } => runtime_error("lambda expressions are not implemented yet"),
    }
}

fn range_endpoint(
    expression: Option<&parser::Expr>,
    default: i64,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<i64, Step> {
    let Some(expression) = expression else {
        return Ok(default);
    };
    match eval_expr(expression, last, in_function, shell)? {
        Value::Integer(value) => Ok(value),
        _ => runtime_error("range endpoints must be integers"),
    }
}

fn eval_call(
    callee: &parser::Expr,
    arguments: &[parser::Argument],
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let parser::Expr::Scalar(word) = callee else {
        return runtime_error("call target is not callable");
    };
    if word.value.text() != "re" {
        return runtime_error(format!(
            "call `{}` is not implemented yet",
            word.value.text()
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

fn runtime_message(message: impl std::fmt::Display) -> Step {
    eprintln!("mesh: {message}");
    Step::Continue(1)
}

fn runtime_error<T>(message: impl std::fmt::Display) -> Result<T, Step> {
    Err(runtime_message(message))
}

fn capture_source(
    source: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return runtime_error(io::Error::last_os_error());
    }
    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if saved < 0 || unsafe { libc::dup2(fds[1], libc::STDOUT_FILENO) } < 0 {
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        return runtime_error(io::Error::last_os_error());
    }
    unsafe { libc::close(fds[1]) };
    let mut reader = unsafe { File::from_raw_fd(fds[0]) };
    let (step, read_result) = std::thread::scope(|scope| {
        let read = scope.spawn(|| {
            let mut output = String::new();
            reader.read_to_string(&mut output).map(|_| output)
        });
        let step = run_source(source, last, in_function, shell);
        let _ = io::stdout().flush();
        unsafe {
            libc::dup2(saved, libc::STDOUT_FILENO);
            libc::close(saved);
        }
        (step, read.join())
    });
    let output = match read_result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return runtime_error(error),
        Err(_) => return runtime_error("capture reader panicked"),
    };
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
    let code = condition_status(&node.condition, last, in_function, shell)?;
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
    let subject = eval_expr(&node.value, last, in_function, shell)?;
    for arm in &node.arms {
        let bindings = match_bindings(&arm.pattern, &subject, last, in_function, shell)?;
        let Some(bindings) = bindings else { continue };
        let snapshot = shell.vars.active_snapshot();
        validate_bindings(&bindings).map_err(runtime_message)?;
        commit_bindings(bindings, &mut shell.vars);
        if let Some(guard) = &arm.guard
            && !truthy(&eval_expr(guard, last, in_function, shell)?)
        {
            shell.vars.restore_active(snapshot);
            continue;
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
    let iterable = eval_expr(iterable, last, in_function, shell)?;
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
    for (index, statement) in body.statements.iter().enumerate() {
        let final_statement = index + 1 == body.statements.len();
        if final_statement && !statement.background && statement.and_or.rest.is_empty() {
            match &statement.and_or.first {
                parser::Executable::Expression {
                    expression,
                    guard: None,
                } => return eval_expr(expression, last, in_function, shell),
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
        if shell.control.is_some() {
            return Ok(Value::String(String::new()));
        }
    }
    Ok(Value::String(String::new()))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.is_empty() && s != "false" && s != "0",
        Value::Integer(value) => *value == 0,
        Value::Boolean(value) => *value,
        Value::List(v) => !v.is_empty(),
        Value::Map(v) => !v.is_empty(),
        Value::Regex(_) | Value::Glob(_) => true,
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
            Value::Integer(_) | Value::Boolean(_) | Value::Regex(_) | Value::Glob(_) => {
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
        run_multi(stages, background, shell)
    }
}

/// Run a one-stage pipeline. Without redirections this is the full command
/// surface: an assignment or a builtin/function/external command. With
/// redirections it is an external command only (a redirected builtin or function
/// is not supported yet).
fn run_single(stage: Stage, background: bool, last: u8, shell: &mut Shell) -> Step {
    let Stage {
        words,
        redirs,
        pipe_stderr: _,
    } = stage;
    if redirs.is_empty() && !background {
        return run_command(words, last, shell);
    }
    let argv = match expand::expand(words, &shell.vars) {
        Ok(argv) => argv,
        Err(err) => {
            eprintln!("mesh: {err}");
            return Step::Continue(1);
        }
    };
    if argv.is_empty() {
        eprintln!("mesh: redirection with no command is not supported yet");
        return Step::Continue(1);
    }
    // `return` is control flow handled on the no-redirection path; with a
    // redirection or in the background it never reaches that handler, so reject
    // it rather than launch an external `return` while the body keeps running.
    if argv[0] == "return" {
        eprintln!("mesh: return: cannot be redirected or backgrounded");
        return Step::Continue(2);
    }
    if builtins::is_builtin(&argv[0]) {
        if background {
            eprintln!(
                "mesh: {}: builtins cannot run in the background yet",
                argv[0]
            );
        } else {
            eprintln!(
                "mesh: {}: redirection of a builtin is not supported yet",
                argv[0]
            );
        }
        return Step::Continue(1);
    }
    if shell.funcs.get(&argv[0]).is_some() {
        eprintln!(
            "mesh: {}: redirection or backgrounding of a function is not supported yet",
            argv[0]
        );
        return Step::Continue(1);
    }
    match expand_redirs(redirs, &shell.vars) {
        Ok(redirs) => Step::Continue(exec::run_pipeline(
            vec![exec::Cmd {
                words: argv,
                redirs,
                pipe_stderr: false,
            }],
            &mut shell.jobs,
            background,
        )),
        Err(err) => {
            eprintln!("mesh: {err}");
            Step::Continue(1)
        }
    }
}

/// Run a multi-stage pipeline (`a | b | c`). Every stage must be an external
/// command; a builtin or function in a pipeline is not supported yet.
fn run_multi(stages: Vec<Stage>, background: bool, shell: &mut Shell) -> Step {
    let mut cmds = Vec::with_capacity(stages.len());
    for stage in stages {
        let Stage {
            words,
            redirs,
            pipe_stderr,
        } = stage;
        let argv = match expand::expand(words, &shell.vars) {
            Ok(argv) => argv,
            Err(err) => {
                eprintln!("mesh: {err}");
                return Step::Continue(1);
            }
        };
        if argv.is_empty() {
            eprintln!("mesh: empty command in a pipeline");
            return Step::Continue(1);
        }
        // `return` unwinds the enclosing function; it has no meaning as a pipeline
        // stage, so reject it rather than launch an external `return`.
        if argv[0] == "return" {
            eprintln!("mesh: return: cannot be used in a pipeline");
            return Step::Continue(2);
        }
        if builtins::is_builtin(&argv[0]) || shell.funcs.get(&argv[0]).is_some() {
            eprintln!(
                "mesh: {}: builtins and functions are not supported in a pipeline yet",
                argv[0]
            );
            return Step::Continue(1);
        }
        let redirs = match expand_redirs(redirs, &shell.vars) {
            Ok(redirs) => redirs,
            Err(err) => {
                eprintln!("mesh: {err}");
                return Step::Continue(1);
            }
        };
        cmds.push(exec::Cmd {
            words: argv,
            redirs,
            pipe_stderr,
        });
    }
    Step::Continue(exec::run_pipeline(cmds, &mut shell.jobs, background))
}

/// Expand each redirection target to exactly one path. Zero or several words is
/// an ambiguous redirect (a glob/list target is not a single file).
fn expand_redirs(
    redirs: Vec<Redir>,
    vars: &Vars,
) -> Result<Vec<(exec::RedirKind, String)>, String> {
    let mut out = Vec::with_capacity(redirs.len());
    for redir in redirs {
        let mut paths = expand::expand(vec![redir.target], vars).map_err(|e| e.to_string())?;
        if paths.len() != 1 {
            return Err(format!(
                "ambiguous redirect: target expanded to {} words",
                paths.len()
            ));
        }
        out.push((redir.kind, paths.pop().unwrap()));
    }
    Ok(out)
}

/// Run one command with no redirections: classify it as an assignment or a
/// command and act. `last` is the previous status (the default for a bare `exit`
/// or `return`).
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
        let args = match expand::expand_values(arg_words, &shell.vars) {
            Ok(args) => args,
            Err(err) => {
                eprintln!("mesh: {err}");
                return Step::Continue(1);
            }
        };
        // Intercept `--help` only when the signature does not claim it; a function
        // that declares a `--help` flag observes the switch itself (`DESIGN.md`
        // §"Command resolution and help").
        let declares_help = shell.funcs.get(&name).unwrap().declares_help();
        if !declares_help && auto_help_requested(&args) {
            let help = shell.funcs.get(&name).unwrap().help(&name);
            return Step::Continue(builtins::print_generated_help(&name, &help));
        }
        // The `--` terminator and flag parsing are handled during argument
        // binding in `call_func`.
        return call_func(&name, args, shell);
    }
    let words = match expand::expand(tokens, &shell.vars) {
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
    // `return` ends the enclosing function (a recoverable error at top
    // level; `run_line` decides which by `in_function`).
    if words[0] == "return" {
        return make_return(&words[1..], last);
    }
    if builtins::is_builtin(&words[0]) && auto_help_requested_strings(&words[1..]) {
        return Step::Continue(builtins::print_help(&words[0]));
    }
    match words[0].as_str() {
        "prompt" => return configure_prompt(&words[1..], shell),
        "prompt-hook" => return configure_prompt_hook(&words[1..], shell),
        _ => {}
    }
    let job_status = match words[0].as_str() {
        "fg" => Some(shell.jobs.foreground(&words[1..])),
        "bg" => Some(shell.jobs.background(&words[1..])),
        "jobs" => Some(shell.jobs.list(&words[1..])),
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

fn auto_help_requested(args: &[Value]) -> bool {
    args.iter()
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
            println!("{}", shell.prompt.text.as_deref().unwrap_or("mesh$ "));
            Step::Continue(0)
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
            eprintln!("mesh: prompt: expected one prompt string or --reset");
            Step::Continue(2)
        }
    }
}

fn configure_prompt_hook(args: &[String], shell: &mut Shell) -> Step {
    let invalid = || {
        eprintln!("mesh: prompt-hook: expected [EVENT] NAME FUNCTION or --remove [EVENT] NAME");
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
        eprintln!("mesh: prompt-hook: `{function}` is not a function");
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

fn run_prompt_hooks(event: PromptEvent, args: Vec<Value>, shell: &mut Shell) {
    let hooks: Vec<String> = shell
        .prompt
        .hooks
        .iter()
        .filter(|hook| hook.event == event)
        .map(|hook| hook.function.clone())
        .collect();
    for function in hooks {
        let _ = call_func(&function, args.clone(), shell);
    }
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

/// Build the [`Step::Return`] for a `return` command: no argument uses the last
/// status; a numeric argument is masked to 0–255. A surplus or non-numeric
/// operand is reported and does not unwind (the function keeps running).
fn make_return(args: &[String], last: u8) -> Step {
    match args {
        [] => Step::Return(last),
        [n] => match n.parse::<i64>() {
            Ok(code) => Step::Return(code.rem_euclid(256) as u8),
            Err(_) => {
                eprintln!("mesh: return: {n}: numeric argument required");
                Step::Continue(2)
            }
        },
        _ => {
            eprintln!("mesh: return: too many arguments");
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
fn call_func(name: &str, args: Vec<Value>, shell: &mut Shell) -> Step {
    let (params, body) = match shell.funcs.get(name) {
        Some(def) => (def.params.clone(), def.body.clone()),
        None => return Step::Continue(exec::run(&[name.to_string()], &mut shell.jobs)),
    };

    shell.vars.push_scope();
    if let Err(code) = bind_arguments(name, &params, args, shell) {
        shell.vars.pop_scope();
        return Step::Continue(code);
    }
    let caller_loop_depth = std::mem::replace(&mut shell.loop_depth, 0);
    let executed = run_source(&body, 0, true, shell);
    shell.loop_depth = caller_loop_depth;
    if matches!(
        shell.control,
        Some(parser::ControlKind::Break | parser::ControlKind::Continue)
    ) {
        shell.control = None;
    }
    let result = match executed {
        Step::Return(code) => Step::Continue(code),
        other => other,
    };
    shell.vars.pop_scope();
    result
}

/// Match `args` against `params` and bind each parameter in the current (already
/// pushed) scope. Positionals bind left to right, `--flags` in any order, and a
/// `...rest` collects the leftovers; a bare `--` ends flag parsing. Returns the
/// exit status to report on a bad argument count, an unknown/misused flag, or a
/// default that fails to evaluate.
fn bind_arguments(
    name: &str,
    params: &[parser::Param],
    args: Vec<Value>,
    shell: &mut Shell,
) -> Result<(), u8> {
    use parser::ParamKind;

    // Positionals in declaration order (`None` default = required); the lone rest.
    let mut positionals: Vec<(&str, Option<&parser::Expr>)> = Vec::new();
    let mut rest_name: Option<&str> = None;
    for param in params {
        match &param.kind {
            ParamKind::Required => positionals.push((param.name.as_str(), None)),
            ParamKind::Optional(default) => positionals.push((param.name.as_str(), Some(default))),
            ParamKind::Rest => rest_name = Some(param.name.as_str()),
            ParamKind::Switch | ParamKind::Flag(_) => {}
        }
    }

    // Scan the call-site arguments, separating positionals from flags. Only a
    // `Value::String` beginning with `--` is a flag candidate; everything else
    // (and everything after a bare `--`) is a positional.
    let mut positional_values: Vec<Value> = Vec::new();
    let mut switches_on: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut flag_values: std::collections::HashMap<&str, Value> = std::collections::HashMap::new();
    let mut flags_ended = false;
    for arg in args {
        if !flags_ended && let Value::String(text) = &arg {
            if text == "--" {
                flags_ended = true;
                continue;
            }
            if let Some(body) = text.strip_prefix("--")
                && !body.is_empty()
            {
                let (flag, inline) = match body.split_once('=') {
                    Some((flag, value)) => (flag, Some(value.to_owned())),
                    None => (body, None),
                };
                let declared = params.iter().find(|param| {
                    param.name == flag
                        && matches!(param.kind, ParamKind::Switch | ParamKind::Flag(_))
                });
                let Some(declared) = declared else {
                    eprintln!("mesh: {name}: unknown flag `--{flag}`");
                    return Err(2);
                };
                match &declared.kind {
                    ParamKind::Switch => {
                        if inline.is_some() {
                            eprintln!(
                                "mesh: {name}: flag `--{flag}` is a switch and takes no value"
                            );
                            return Err(2);
                        }
                        switches_on.insert(declared.name.as_str());
                    }
                    ParamKind::Flag(_) => {
                        let Some(value) = inline else {
                            eprintln!(
                                "mesh: {name}: flag `--{flag}` requires a value (write `--{flag}=VALUE`)"
                            );
                            return Err(2);
                        };
                        // Last occurrence wins for a valued flag.
                        flag_values.insert(declared.name.as_str(), Value::String(value));
                    }
                    _ => unreachable!("only flags are collected here"),
                }
                continue;
            }
        }
        positional_values.push(arg);
    }

    // Arity: every required positional must be filled; without a rest, surplus
    // positionals are an error.
    let required = positionals.iter().filter(|(_, d)| d.is_none()).count();
    let maximum = positionals.len();
    let supplied = positional_values.len();
    if supplied < required {
        if rest_name.is_some() || maximum > required {
            eprintln!("mesh: {name}: expected at least {required} argument(s), got {supplied}");
        } else {
            eprintln!("mesh: {name}: expected {required} argument(s), got {supplied}");
        }
        return Err(2);
    }
    if rest_name.is_none() && supplied > maximum {
        if maximum > required {
            eprintln!("mesh: {name}: expected at most {maximum} argument(s), got {supplied}");
        } else {
            eprintln!("mesh: {name}: expected {maximum} argument(s), got {supplied}");
        }
        return Err(2);
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

/// Evaluate a parameter's default expression in the function's fresh scope,
/// reporting a recoverable error if it fails.
fn evaluate_default(
    name: &str,
    param: &str,
    default: &parser::Expr,
    shell: &mut Shell,
) -> Result<Value, u8> {
    eval_expr(default, 0, true, shell).map_err(|_| {
        eprintln!("mesh: {name}: could not evaluate default for `{param}`");
        2
    })
}

/// Return whether the parser needs another physical line to complete the input.
fn needs_more_input(text: &str) -> bool {
    let trimmed = text.trim_start();
    let func_header = trimmed.strip_prefix("func").is_some_and(|rest| {
        rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace)
    });
    match parser::parse(text) {
        Ok(parser::ParseOutcome::Incomplete) if func_header => crate::lexer::needs_more_input(text),
        Ok(parser::ParseOutcome::Incomplete) => true,
        Err(_) if func_header => crate::lexer::needs_more_input(text),
        Ok(parser::ParseOutcome::Complete(_)) | Err(_) => false,
    }
}

#[derive(Default)]
struct ArgumentRecall {
    arguments: Vec<String>,
    inserted: Option<(usize, String, usize)>,
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
        if let Some(argument) = last_argument(line) {
            self.arguments.push(argument);
        }
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
    saved_submissions: usize,
) -> reedline::Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let completed = completed_command(signal, pending);
    if completed.is_none() && !matches!(signal, Signal::CtrlC | Signal::CtrlD) {
        return Ok(());
    }

    let entries = history.search(SearchQuery::everything(SearchDirection::Backward, session))?;
    let mut remaining = saved_submissions;
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

    if let Some(command) = completed {
        let mut item = HistoryItem::from_command_line(command);
        item.session_id = session;
        history.save(item)?;
    }
    Ok(())
}

fn last_argument(line: &str) -> Option<String> {
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
    let words: Vec<_> = pipeline
        .stages
        .last()?
        .items
        .iter()
        .filter_map(|item| match item {
            parser::CommandItem::Word(word) => Some(word),
            parser::CommandItem::Redirect { .. } => None,
        })
        .collect();
    let argument = words.get(1..)?.last()?;
    line.get(argument.span.clone()).map(str::to_owned)
}

fn run_interactive(options: &StartupOptions) -> ExitCode {
    if let Err(err) = wait_until_foreground() {
        eprintln!("mesh: could not acquire terminal foreground: {err}");
        return ExitCode::from(1);
    }
    if let Err(err) = ignore_interactive_signals() {
        eprintln!("mesh: could not configure interactive signals: {err}");
        return ExitCode::from(1);
    }
    let completion = Arc::new(RwLock::new(CompletionState::default()));
    let keybindings = interactive_keybindings();
    let completion_menu = completion_menu();
    let mut editor = Reedline::create()
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
            Err(err) => eprintln!("mesh: could not open history database: {err}"),
        }
    }
    let mut shell = Shell::new();
    let mut last = match run_startup_files(options, true, 0, &mut shell) {
        Step::Continue(code) => code,
        Step::Exit(code) | Step::Return(code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
    };
    let mut pending = String::new();
    let mut pending_history_rows = 0;
    loop {
        shell.jobs.reap();
        if pending.is_empty() {
            run_prompt_hooks(PromptEvent::PrePrompt, Vec::new(), &mut shell);
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
                if history_session.is_some()
                    && matches!(&signal, Signal::Success(line) if !line.is_empty())
                {
                    pending_history_rows += 1;
                }
                let completed_command = completed_command(&signal, &pending);
                if let Some(session) = history_session
                    && let Err(err) = persist_logical_history(
                        editor.history_mut(),
                        Some(session),
                        &signal,
                        &pending,
                        pending_history_rows,
                    )
                {
                    eprintln!("mesh: could not update history database: {err}");
                }
                match handle_signal(signal, last, &mut shell, &mut pending) {
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
                eprintln!("mesh: line editor error: {err}");
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

fn completed_command(signal: &Signal, pending: &str) -> Option<String> {
    let Signal::Success(line) = signal else {
        return None;
    };
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
) -> Option<Step> {
    match signal {
        Signal::Success(line) => {
            pending.push_str(&line);
            pending.push('\n');
            if needs_more_input(pending) {
                return None;
            }
            let text = std::mem::take(pending);
            let command = text.trim_end_matches('\n').to_string();
            run_prompt_hooks(
                PromptEvent::PreExec,
                vec![Value::String(command.clone())],
                shell,
            );
            let start = Instant::now();
            let step = run_line(&text, last, false, shell);
            let status = match step {
                Step::Continue(code) | Step::Exit(code) | Step::Return(code) => code,
            };
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
            Some(step)
        }
        // Ctrl-D (EOF) exits with the last status, abandoning any in-progress
        // `func` — the buffered lines are dropped as the shell leaves. reedline
        // only emits this on an empty editor line, so a half-typed line is safe.
        Signal::CtrlD => Some(Step::Exit(last)),
        _ => {
            // Ctrl-C: cancel the current line (and any buffered `func` body) and
            // re-prompt, keeping the status.
            pending.clear();
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
    let mut last = match run_startup_files(options, false, 0, &mut shell) {
        Step::Continue(code) => code,
        Step::Exit(code) | Step::Return(code) => {
            return ExitCode::from(run_logout(options, code, &mut shell));
        }
    };
    let mut pending = String::new();
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
                eprintln!("mesh: read error: {err}");
                return ExitCode::from(run_logout(options, 1, &mut shell));
            }
        }

        // Hold a lossy copy alive if we substitute invalid bytes below.
        let lossy;
        let text: &str = match std::str::from_utf8(&line) {
            Ok(text) => text,
            Err(_) => {
                eprintln!("mesh: invalid UTF-8 in input");
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
        if needs_more_input(&pending) {
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
        let width = sgr_stripped_width(prompt.rsplit('\n').next().unwrap_or_default());

        if width == 0 {
            String::new()
        } else {
            format!("{} ", ".".repeat(width - 1))
        }
    }
}

fn sgr_stripped_width(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut width = 0;
    let mut visible_start = 0;
    let mut index = 0;

    while index + 1 < bytes.len() {
        if bytes[index] != b'\x1b' || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }

        let mut end = index + 2;
        while end < bytes.len() && (0x30..=0x3f).contains(&bytes[end]) {
            end += 1;
        }
        while end < bytes.len() && (0x20..=0x2f).contains(&bytes[end]) {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'm' {
            width += text[visible_start..index].width();
            visible_start = end + 1;
            index = end + 1;
        } else {
            index += 1;
        }
    }

    width + text[visible_start..].width()
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
        ArgumentRecall, CompletionState, MeshPrompt, PromptEvent, PromptHook, Shell,
        StartupOptions, Step, TimestampedHistory, argument_completions, command_position,
        command_segment_words, completed_command, eval_binary, expansion_word, handle_signal,
        help_completions, history_path_from, input_highlighter, interactive_keybindings,
        interruptible_task, last_argument, needs_more_input, open_history, path_completions_sync,
        persist_logical_history, prepare_history_path, run_line, run_prompt_hooks, run_source,
        variable_completions,
    };
    use crate::parser;
    use crate::vars::Value;
    use reedline::{
        EditCommand, Highlighter, History, HistoryItem, KeyModifiers, Prompt, PromptEditMode,
        Reedline, ReedlineEvent, Signal, SqliteBackedHistory,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
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
        assert_eq!(completed_command(&first, ""), None);

        let second = Signal::Success("puts followed-by-words".into());
        assert_eq!(
            completed_command(&second, "puts first |\n").as_deref(),
            Some("puts first |\nputs followed-by-words")
        );
        assert_eq!(
            last_argument(&completed_command(&second, "puts first |\n").unwrap()).as_deref(),
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
            3,
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
            2,
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
        persist_logical_history(&mut saved, saved_session, &Signal::CtrlC, "func f() {\n", 1)
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
            2,
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
        assert_eq!(
            handle_signal(Signal::Success("true".into()), 0, &mut shell, &mut pending,),
            Some(Step::Continue(0))
        );
        assert_eq!(shell.prompt.hooks.len(), 2);
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
        assert_eq!(
            handle_signal(Signal::CtrlD, 7, &mut shell, &mut pending),
            Some(Step::Exit(7))
        );
    }

    #[test]
    fn ctrl_d_exits_even_mid_function_definition() {
        // With a `func` body still buffered, Ctrl-D still exits (abandoning it).
        let mut shell = Shell::new();
        let mut pending = String::from("func f() {\n");
        assert_eq!(
            handle_signal(Signal::CtrlD, 4, &mut shell, &mut pending),
            Some(Step::Exit(4))
        );
    }

    #[test]
    fn ctrl_c_re_prompts_keeping_status() {
        let mut shell = Shell::new();
        let mut pending = String::new();
        assert_eq!(
            handle_signal(Signal::CtrlC, 7, &mut shell, &mut pending),
            Some(Step::Continue(7))
        );
    }

    #[test]
    fn a_submitted_exit_line_exits() {
        let mut shell = Shell::new();
        let mut pending = String::new();
        let signal = Signal::Success("exit 5".to_string());
        assert_eq!(
            handle_signal(signal, 0, &mut shell, &mut pending),
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
        // The opening line leaves the body open — no step yet.
        assert_eq!(
            handle_signal(
                Signal::Success("func greet(who) {".into()),
                0,
                &mut shell,
                &mut pending
            ),
            None
        );
        assert_eq!(
            handle_signal(
                Signal::Success("  puts \"hi $who\"".into()),
                0,
                &mut shell,
                &mut pending
            ),
            None
        );
        // The closing brace completes and defines the function.
        assert_eq!(
            handle_signal(Signal::Success("}".into()), 0, &mut shell, &mut pending),
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
        assert_eq!(
            handle_signal(
                Signal::Success("func f()".into()),
                0,
                &mut shell,
                &mut pending
            ),
            None
        );
        let step = handle_signal(
            Signal::Success("puts after".into()),
            0,
            &mut shell,
            &mut pending,
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
}
