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
use crate::funcs::{FuncDef, Funcs};
use crate::lexer::{Piece, Redir, RedirKind, Sep, Stage, Word};
use crate::vars::{Value, Vars};
use crate::{exec, expand, lexer};

/// The mutable shell session threaded through the run loop: variable scopes,
/// defined functions, and the job table.
struct Shell {
    vars: Vars,
    funcs: Funcs,
    jobs: exec::JobTable,
}

impl Shell {
    fn new() -> Self {
        Self {
            vars: Vars::new(),
            funcs: Funcs::new(),
            jobs: exec::JobTable::new(),
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
    /// `return` was invoked; unwind the current function with this status. At top
    /// level (no function) `run_line` reports it as a recoverable error instead.
    Return(u8),
}

/// Tokenize and run one line of input against the session. A line is a sequence
/// of commands joined by `;` / `&&` / `||`; each connector decides whether its
/// command runs from the previous command's status. Empty lines are a no-op that
/// keeps the last status.
///
/// A `func name(params) { … }` definition is parsed from raw text, since its body
/// spans lines the per-line lexer would otherwise flatten. `in_function` is true
/// while running a `func` body: there a `return` unwinds; at top level it is a
/// recoverable error.
fn run_line(text: &str, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    if is_func_start(text) {
        return define_func(text, shell);
    }
    let segments = match lexer::split_line(text) {
        Ok(segments) => segments,
        Err(err) => {
            eprintln!("mesh: {err}");
            return Step::Continue(2); // syntax error
        }
    };
    let mut status = last;
    for segment in segments {
        let run_it = match segment.sep_before {
            Sep::Seq => true,
            Sep::And => status == 0, // run after success
            Sep::Or => status != 0,  // run after failure
        };
        if !run_it || segment.stages.is_empty() {
            // Short-circuited commands leave the status unchanged. The empty
            // check is defensive; the lexer no longer emits empty segments.
            continue;
        }
        match run_pipeline(segment.stages, segment.background, status, shell) {
            Step::Exit(code) => return Step::Exit(code),
            Step::Continue(code) => status = code,
            Step::Return(code) => {
                if in_function {
                    // Inside a function, `return` unwinds — abort the line so the
                    // caller (`call_func`) can stop the body.
                    return Step::Return(code);
                }
                // At top level `return` is a recoverable error; the `;` sequence
                // still runs any following command unconditionally.
                eprintln!("mesh: return: not inside a function");
                status = 1;
            }
        }
    }
    Step::Continue(status)
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
    let Stage { mut words, redirs } = stage;
    if redirs.is_empty() {
        if !background {
            return run_command_or_assign(words, last, shell);
        }
        words = match classify(words) {
            Line::Command(words) => words,
            Line::Assign { .. } => {
                eprintln!("mesh: assignments cannot run in the background");
                return Step::Continue(1);
            }
        };
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
        let Stage { words, redirs } = stage;
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
        });
    }
    Step::Continue(exec::run_pipeline(cmds, &mut shell.jobs, background))
}

/// Expand each redirection target to exactly one path. Zero or several words is
/// an ambiguous redirect (a glob/list target is not a single file).
fn expand_redirs(redirs: Vec<Redir>, vars: &Vars) -> Result<Vec<(RedirKind, String)>, String> {
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
fn run_command_or_assign(tokens: Vec<Word>, last: u8, shell: &mut Shell) -> Step {
    match classify(tokens) {
        Line::Assign { name, rhs, append } => match if append {
            append_assign(&name, rhs, &mut shell.vars)
        } else {
            assign(&name, rhs, &mut shell.vars)
        } {
            Ok(()) => Step::Continue(0),
            Err(msg) => {
                eprintln!("mesh: {msg}");
                Step::Continue(1)
            }
        },
        Line::Command(tokens) => {
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
            let job_status = match words[0].as_str() {
                "fg" => Some(shell.jobs.foreground(&words[1..])),
                "bg" => Some(shell.jobs.background(&words[1..])),
                "jobs" => Some(shell.jobs.list(&words[1..])),
                _ => None,
            };
            if let Some(code) = job_status {
                return Step::Continue(code);
            }
            // Command resolution: builtins, then user functions, then external.
            match builtins::dispatch(&words, last) {
                Some(Builtin::Exit(code)) => Step::Exit(code),
                Some(Builtin::Status(code)) => Step::Continue(code),
                None => {
                    if shell.funcs.get(&words[0]).is_some() {
                        let name = words[0].clone();
                        let args = words[1..].to_vec();
                        call_func(&name, args, shell)
                    } else {
                        Step::Continue(exec::run(&words, &mut shell.jobs))
                    }
                }
            }
        }
    }
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

/// Call the function `name` with already-expanded `args`. Binds the positional
/// parameters in a fresh local scope, runs the body, and returns the function's
/// status — an explicit `return`, else the last command's status. An arity
/// mismatch is a recoverable error.
fn call_func(name: &str, args: Vec<String>, shell: &mut Shell) -> Step {
    let (params, body) = match shell.funcs.get(name) {
        Some(def) => (def.params.clone(), def.body.clone()),
        None => return Step::Continue(exec::run(&[name.to_string()], &mut shell.jobs)),
    };
    if args.len() != params.len() {
        eprintln!(
            "mesh: {name}: expected {} argument(s), got {}",
            params.len(),
            args.len()
        );
        return Step::Continue(2);
    }

    shell.vars.push_scope();
    for (param, arg) in params.iter().zip(args) {
        shell.vars.set(param, arg);
    }
    let result = run_func_body(&body, shell);
    shell.vars.pop_scope();
    result
}

/// Run a function body, buffering a nested multi-line `func` definition until its
/// braces balance — exactly as the top-level reader does — so a nested definition
/// is stored rather than having only its first line reach `run_line`.
///
/// A function starts fresh, not inheriting the caller's `$?`: an empty body (or a
/// bare `return` before any command) yields status 0 (`DESIGN.md` — "no
/// expression to yield … status 0"), and the first line likewise sees `$?` = 0.
/// `return` ends the body early with its status; `exit` propagates out.
fn run_func_body(body: &str, shell: &mut Shell) -> Step {
    let mut status = 0;
    let mut pending = String::new();
    for line in body.lines() {
        if line_invalidates_awaited_body(&pending, line) {
            // A nested header awaiting its body, invalidated by this line: flush
            // the header (its missing-body error), then reprocess the line below.
            let header = std::mem::take(&mut pending);
            match run_line(&header, status, true, shell) {
                Step::Continue(code) => status = code,
                Step::Return(code) => return Step::Continue(code),
                Step::Exit(code) => return Step::Exit(code),
            }
        }
        pending.push_str(line);
        pending.push('\n');
        if is_func_start(&pending) && lexer::needs_more_input(&pending) {
            continue;
        }
        let full = std::mem::take(&mut pending);
        match run_line(&full, status, true, shell) {
            Step::Continue(code) => status = code,
            Step::Return(code) => return Step::Continue(code),
            Step::Exit(code) => return Step::Exit(code),
        }
    }
    // A truncated nested definition still buffered at the end of the body: run it
    // so its "missing }" error is reported rather than silently swallowed.
    if !pending.trim().is_empty() {
        return match run_line(&pending, status, true, shell) {
            Step::Return(code) => Step::Continue(code),
            other => other,
        };
    }
    Step::Continue(status)
}

/// Does `text` begin a `func` definition? (`func` followed by end-of-input,
/// whitespace, or `(`.)
fn is_func_start(text: &str) -> bool {
    match text.trim_start().strip_prefix("func") {
        Some(rest) => rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace() || c == '('),
        None => false,
    }
}

/// When `pending` holds a `func` header still awaiting its body — `func f()` with
/// the `{` expected on a later line — does the next physical line `next` fail to
/// open that body? If so the header is invalid on its own and `next` is a
/// separate command, so the reader flushes the header (reporting its "missing
/// body" error) and reprocesses `next` fresh rather than swallowing it into the
/// rejected definition. False once a body brace has opened (then `next` continues
/// the body) or when `next` still leaves the header validly incomplete.
fn line_invalidates_awaited_body(pending: &str, next: &str) -> bool {
    if pending.is_empty() || !is_func_start(pending) {
        return false;
    }
    let before = lexer::scan_braces(pending, 0);
    if before.close.is_some() || before.depth != 0 {
        return false; // a body brace is already open or closed — not awaiting one
    }
    let mut combined = pending.to_string();
    combined.push_str(next);
    if lexer::needs_more_input(&combined) {
        return false; // still a valid incomplete header (blank line, forming signature)
    }
    // The header is resolved by `next`. It is a real definition to dispatch only
    // if `next` actually opens this header's body — the first non-whitespace after
    // the signature `)` is `{`. Otherwise `next` is a separate command (even one
    // that is itself a complete `func … { … }`), so the header must be flushed on
    // its own and `next` reprocessed rather than swallowed.
    !body_opens_after_signature(&combined)
}

/// After a `func name(params)` signature, does the body open — i.e. is the first
/// non-whitespace character following the signature's `)` a `{`? v1 parameter
/// lists hold no parentheses, so the first `)` after the first `(` closes the
/// signature.
fn body_opens_after_signature(text: &str) -> bool {
    let rest = text
        .trim_start()
        .strip_prefix("func")
        .unwrap_or("")
        .trim_start();
    let Some(paren) = rest.find('(') else {
        return false;
    };
    let after_open = &rest[paren + 1..];
    let Some(close) = after_open.find(')') else {
        return false;
    };
    after_open[close + 1..].trim_start().starts_with('{')
}

/// Parse and store a `func name(params) { body }` definition.
fn define_func(text: &str, shell: &mut Shell) -> Step {
    match parse_func_def(text) {
        Ok((name, def)) => {
            shell.funcs.define(name, def);
            Step::Continue(0)
        }
        Err(msg) => {
            eprintln!("mesh: {msg}");
            Step::Continue(2)
        }
    }
}

/// Parse `func name(params) { body }` from raw text into a name and definition.
/// v1 accepts required named positionals only.
///
/// The signature's closing `)` is searched for **only before the body's opening
/// `{`**, so a malformed header such as `func f(x { … }` cannot borrow a `)` from
/// a later body line — it is rejected as a missing `)` while the buffered body
/// (already consumed by the brace-driven reader) stays quarantined.
fn parse_func_def(text: &str) -> Result<(String, FuncDef), String> {
    let rest = text
        .trim()
        .strip_prefix("func")
        .ok_or("func: internal error")?
        .trim_start();
    let name_end = rest
        .find(|c: char| c == '(' || c.is_whitespace())
        .ok_or("func: missing parameter list `(...)`")?;
    let name = &rest[..name_end];
    if !lexer::is_ident(name) {
        return Err(format!("func: `{name}` is not a valid function name"));
    }
    // A name resolves as builtin → function → external, and `func`/`return` are
    // intercepted even earlier (the reader reads `func …` as a definition, and
    // `return` is control flow). A function named after any of these would be
    // stored but never reachable, so reject it rather than accept a dead
    // definition.
    if name == "func" || name == "return" || builtins::is_builtin(name) {
        return Err(format!(
            "func: `{name}` is a reserved name and cannot be a function name"
        ));
    }
    let after_open = rest[name_end..]
        .trim_start()
        .strip_prefix('(')
        .ok_or("func: missing parameter list `(...)`")?;
    // Bound the `)` search to the header — everything before the body's `{`.
    let header_end = after_open.find('{').unwrap_or(after_open.len());
    let close = after_open[..header_end]
        .find(')')
        .ok_or("func: missing `)` before the function body")?;
    let params = lexer::parse_params(&after_open[..close])?;
    let body_src = after_open[close + 1..]
        .trim_start()
        .strip_prefix('{')
        .ok_or("func: missing body `{ ... }`")?;
    let (body, after_body) = split_braced_body(body_src)?;
    if !after_body.trim().is_empty() {
        return Err("func: unexpected text after the closing `}`".to_string());
    }
    Ok((
        name.to_string(),
        FuncDef {
            params,
            body: body.to_string(),
        },
    ))
}

/// Given the text right after a body's opening `{`, split off the body (up to
/// the matching `}`) and whatever follows it. Delegates to the lexer's shared
/// [`lexer::scan_braces`] so the boundary honors the same quote/raw/escape rules
/// as execution — the definition scanner and the runtime lexer cannot disagree.
fn split_braced_body(src: &str) -> Result<(&str, &str), String> {
    match lexer::scan_braces(src, 1).close {
        Some(byte) => Ok((&src[..byte], &src[byte + 1..])),
        None => Err("func: missing closing `}`".to_string()),
    }
}

/// A classified line: a variable binding or a command.
enum Line {
    Assign {
        name: String,
        rhs: Vec<Word>,
        append: bool,
    },
    Command(Vec<Word>),
}

/// Classify a non-empty token list. An assignment uses `=` or `+=`, either
/// spaced or unspaced as the whole statement; position separates it from a
/// `k=v` argument after a command word (`git commit --author=me`).
///
/// Deferred: prefix env (`FOO=1 cmd` — use `env FOO=1 cmd`), and `name=value`
/// followed by more words.
fn classify(mut tokens: Vec<Word>) -> Line {
    // Spaced: `name` (`=` | `+=`) value…
    if tokens.len() >= 2 && assignment_operator(&tokens[1]).is_some() {
        if let Some(name) = bare_ident(&tokens[0]) {
            let name = name.to_string();
            let append = assignment_operator(&tokens[1]) == Some(true);
            let rhs = tokens.split_off(2);
            return Line::Assign { name, rhs, append };
        }
    }
    // Unspaced: a single word `name=value` or `name+=value`.
    if tokens.len() == 1 {
        if let Some((name, rhs, append)) = split_unspaced_assignment(&tokens[0]) {
            return Line::Assign {
                name,
                rhs: vec![rhs],
                append,
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
        ] if lexer::is_ident(text) => Some(text),
        _ => None,
    }
}

/// Recognize a bare `=` or `+=` assignment operator.
fn assignment_operator(word: &Word) -> Option<bool> {
    match word.0.as_slice() {
        [
            Piece::Text {
                text,
                expandable: true,
            },
        ] if text == "=" => Some(false),
        [
            Piece::Text {
                text,
                expandable: true,
            },
        ] if text == "+=" => Some(true),
        _ => None,
    }
}

/// Split a single word `name=value…` into the name and a word for the value, if
/// the leading unquoted text is `ident=…`. `value` keeps any later pieces (so
/// `x=$y` binds `x` to the value of `$y`).
fn split_unspaced_assignment(word: &Word) -> Option<(String, Word, bool)> {
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
    let (before, after) = text.split_once('=')?;
    let (name, append) = before
        .strip_suffix('+')
        .map_or((before, false), |name| (name, true));
    if !lexer::is_ident(name) {
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
    Some((name.to_string(), Word(value), append))
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
            access: v.access.clone(),
            quoted: v.quoted,
        }),
    }
}

/// Bind `name` to a scalar expansion or a bracketed list literal.
fn assign(name: &str, rhs: Vec<Word>, vars: &mut Vars) -> Result<(), String> {
    // `env` is the environment namespace (`$env.KEY`); a plain `env` binding
    // would be shadowed by that read and so could never be read back. Reject it
    // rather than store an unreachable value.
    if name == "env" {
        return Err(format!("{name}: cannot assign to the reserved name"));
    }
    if let [Word(pieces)] = rhs.as_slice()
        && let [Piece::Var(vref)] = pieces.as_slice()
        && vref.member.is_none()
        && !vref.quoted
    {
        let value = assignment_value(vref, vars)?;
        vars.set_value(name, value);
        return Ok(());
    }
    if let Some(items) = list_literal(rhs.as_slice()) {
        let values = expand::expand(items, vars).map_err(|e| e.to_string())?;
        vars.set_list(name, values);
        return Ok(());
    }
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

/// Preserve list values in an exact variable-to-variable assignment. Command
/// expansion still requires `...`; assignment is a value context instead.
fn assignment_value(vref: &crate::lexer::VarRef, vars: &Vars) -> Result<Value, String> {
    let value = vars
        .get(&vref.name)
        .ok_or_else(|| format!("{}: unbound variable", vref.name))?;
    match (&vref.access, value) {
        (None, value) => Ok(value.clone()),
        (
            Some(crate::lexer::Access::Slice {
                start,
                end,
                inclusive,
            }),
            Value::List(values),
        ) => Ok(Value::List(
            expand::slice(values, *start, *end, *inclusive).to_vec(),
        )),
        _ => scalar_value(vec![Word(vec![Piece::Var(vref.clone())])], vars, &vref.name),
    }
}

/// Apply `+=` without coercion: strings concatenate, while lists append a
/// scalar or extend with another list.
fn append_assign(name: &str, rhs: Vec<Word>, vars: &mut Vars) -> Result<(), String> {
    if name == "env" {
        return Err(format!("{name}: cannot assign to the reserved name"));
    }
    let value = if let Some(items) = list_literal(rhs.as_slice()) {
        Value::List(expand::expand(items, vars).map_err(|e| e.to_string())?)
    } else if let [Word(pieces)] = rhs.as_slice() {
        if let [Piece::Var(vref)] = pieces.as_slice() {
            if vref.member.is_none() && !vref.quoted {
                assignment_value(vref, vars)?
            } else {
                scalar_value(rhs, vars, name)?
            }
        } else {
            scalar_value(rhs, vars, name)?
        }
    } else {
        scalar_value(rhs, vars, name)?
    };
    vars.append(name, value)
}

fn scalar_value(rhs: Vec<Word>, vars: &Vars, name: &str) -> Result<Value, String> {
    let mut args = expand::expand(rhs, vars).map_err(|e| e.to_string())?;
    match args.len() {
        1 => Ok(Value::String(args.pop().unwrap())),
        0 => Err(format!("{name}: assignment needs a value")),
        _ => Err(format!("{name}: append needs one value")),
    }
}

/// Remove bare outer brackets from an assignment RHS. Brackets embedded in a
/// quoted piece remain ordinary text; nested values wait for the M3 parser.
fn list_literal(rhs: &[Word]) -> Option<Vec<Word>> {
    let mut items: Vec<Word> = rhs
        .iter()
        .map(|word| Word(word.0.iter().map(clone_piece).collect()))
        .collect();
    let first = items.first_mut()?;
    let Piece::Text {
        text: first_text,
        expandable: true,
    } = first.0.first_mut()?
    else {
        return None;
    };
    if !first_text.starts_with('[') {
        return None;
    }
    first_text.remove(0);
    if matches!(first.0.first(), Some(Piece::Text { text, expandable: true }) if text.is_empty()) {
        first.0.remove(0);
    }

    let last = items.last_mut()?;
    let Piece::Text {
        text: last_text,
        expandable: true,
    } = last.0.last_mut()?
    else {
        return None;
    };
    if !last_text.ends_with(']') {
        return None;
    }
    last_text.pop();
    if matches!(last.0.last(), Some(Piece::Text { text, expandable: true }) if text.is_empty()) {
        last.0.pop();
    }
    let synthetic = |word: &Word| matches!(word.0.as_slice(), [Piece::Text { text, expandable: true }] if text.is_empty());
    if items
        .first()
        .is_some_and(|word| word.0.is_empty() || synthetic(word))
    {
        items.remove(0);
    }
    if items
        .last()
        .is_some_and(|word| word.0.is_empty() || synthetic(word))
    {
        items.pop();
    }
    Some(items)
}

/// Interactive loop: reedline line editing with an in-memory history. Ctrl-D on
/// an empty line exits (reedline's default — a non-empty line is unaffected);
/// Ctrl-C cancels the current line and returns to the prompt without exiting. A
/// multi-line `func` body is buffered in `pending` until its braces balance.
fn run_interactive() -> ExitCode {
    if let Err(err) = wait_until_foreground() {
        eprintln!("mesh: could not acquire terminal foreground: {err}");
        return ExitCode::from(1);
    }
    if let Err(err) = ignore_interactive_signals() {
        eprintln!("mesh: could not configure interactive signals: {err}");
        return ExitCode::from(1);
    }
    let mut editor = Reedline::create();
    let mut last: u8 = 0;
    let mut shell = Shell::new();
    let mut pending = String::new();
    loop {
        shell.jobs.reap();
        let prompt = MeshPrompt {
            failed: last != 0,
            continuation: !pending.is_empty(),
        };
        match editor.read_line(&prompt) {
            Ok(signal) => match handle_signal(signal, last, &mut shell, &mut pending) {
                None => continue, // an unfinished `func` body: read the next line
                Some(Step::Exit(code)) => return ExitCode::from(code),
                Some(Step::Continue(code)) => last = code,
                // Top-level `run_line` reports a stray `return` itself, so one
                // never reaches here.
                Some(Step::Return(_)) => unreachable!("top-level return handled in run_line"),
            },
            Err(err) => {
                eprintln!("mesh: line editor error: {err}");
                return ExitCode::from(1);
            }
        }
    }
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

/// Handle a reedline signal, buffering the lines of a multi-line `func` body in
/// `pending`. Returns `None` when more input is needed (a `func` body is still
/// open), else `Some(step)`. Extracted from the read loop so the interactive
/// control flow is unit-testable without a terminal.
///
/// `Ctrl-D` on an empty line exits (and abandons any in-progress `func`);
/// `Ctrl-C` cancels the current line/buffer and re-prompts, keeping the status.
fn handle_signal(
    signal: Signal,
    mut last: u8,
    shell: &mut Shell,
    pending: &mut String,
) -> Option<Step> {
    match signal {
        Signal::Success(line) => {
            if line_invalidates_awaited_body(pending, &line) {
                // The awaited body never came: flush the header (its missing-body
                // error) now, then reprocess this line fresh below rather than
                // swallowing it into the rejected definition.
                let header = std::mem::take(pending);
                match run_line(&header, last, false, shell) {
                    Step::Exit(code) => return Some(Step::Exit(code)),
                    Step::Continue(code) => last = code,
                    Step::Return(_) => unreachable!("top-level return handled in run_line"),
                }
            }
            pending.push_str(&line);
            pending.push('\n');
            if is_func_start(pending) && lexer::needs_more_input(pending) {
                return None;
            }
            let text = std::mem::take(pending);
            Some(run_line(&text, last, false, shell))
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
fn run_piped() -> ExitCode {
    // `ManuallyDrop` keeps us from closing fd 0 when the shell exits.
    let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut last: u8 = 0;
    let mut shell = Shell::new();
    let mut pending = String::new();
    // The buffered definition contained invalid UTF-8: keep buffering to its
    // closing brace (so its body can't leak), then discard it whole rather than
    // storing/executing the lossy source.
    let mut poisoned = false;
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

        // Hold a lossy copy alive if we substitute invalid bytes below.
        let lossy;
        let text: &str = match std::str::from_utf8(&line) {
            Ok(text) => text,
            Err(_) => {
                eprintln!("mesh: invalid UTF-8 in input");
                last = 1;
                lossy = String::from_utf8_lossy(&line).into_owned();
                // A malformed line that opens or continues a `func` body must be
                // quarantined: poison it and substitute U+FFFD only so real braces
                // still count, keep buffering to the matching close, then discard
                // the whole definition below — never storing or running lossy
                // source, and never leaking the body to the top level. A standalone
                // malformed line with no open definition is simply skipped.
                let opens_definition = is_func_start(&lossy) && lexer::needs_more_input(&lossy);
                if pending.is_empty() && !opens_definition {
                    continue;
                }
                poisoned = true;
                &lossy
            }
        };
        // A buffered header still awaiting its body, followed by a line that
        // cannot open one: flush the header (its missing-body error) and reprocess
        // this line fresh rather than swallowing it. Skip while poisoning, whose
        // discard path owns the buffer.
        if !poisoned && line_invalidates_awaited_body(&pending, text) {
            let header = std::mem::take(&mut pending);
            match run_line(&header, last, false, &mut shell) {
                Step::Exit(code) => return ExitCode::from(code),
                Step::Continue(code) => last = code,
                Step::Return(_) => unreachable!("top-level return handled in run_line"),
            }
        }
        pending.push_str(text);
        // Keep reading the lines of a multi-line `func` body before running it.
        if is_func_start(&pending) && lexer::needs_more_input(&pending) {
            continue;
        }
        let full = std::mem::take(&mut pending);
        if std::mem::take(&mut poisoned) {
            // Discard the definition that contained invalid UTF-8 (error already
            // reported when the bad line was read); do not define or run it.
            continue;
        }
        match run_line(&full, last, false, &mut shell) {
            Step::Exit(code) => return ExitCode::from(code),
            Step::Continue(code) => last = code,
            Step::Return(_) => unreachable!("top-level return handled in run_line"),
        }
    }
    // A truncated (or poisoned) `func` definition at EOF: a poisoned one is
    // discarded (its error was already reported); otherwise run it so the parse
    // error is reported.
    if !poisoned && !pending.trim().is_empty() {
        match run_line(&pending, last, false, &mut shell) {
            Step::Exit(code) => return ExitCode::from(code),
            Step::Continue(code) => last = code,
            Step::Return(_) => unreachable!("top-level return handled in run_line"),
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

/// The minimal two-glyph prompt: `mesh$` after success, `mesh!` after failure,
/// `...` while a multi-line `func` body is still open. The full status-dashboard
/// prompt from `DESIGN.md` is a later milestone.
struct MeshPrompt {
    failed: bool,
    continuation: bool,
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
            Cow::Borrowed("... ")
        } else if self.failed {
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
    use super::{Shell, Step, handle_signal, run_line};
    use crate::vars::Value;
    use reedline::Signal;

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
        assert_eq!(shell.vars.get("n"), Some(&Value::String("42".to_string())));
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
    fn a_non_brace_line_after_an_awaited_header_is_reprocessed() {
        // `func f()` buffers awaiting its body; the next line is not `{`, so the
        // header is rejected and that line runs as its own command (not swallowed).
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
        // The following line invalidates the awaited body: it runs on its own.
        let step = handle_signal(
            Signal::Success("puts after".into()),
            0,
            &mut shell,
            &mut pending,
        );
        assert_eq!(step, Some(Step::Continue(0)));
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
