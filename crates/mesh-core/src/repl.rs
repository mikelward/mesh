//! The read / tokenize / dispatch loop.
//!
//! Interactive (TTY) input goes through [`reedline`] for line editing, history,
//! and Ctrl-C/Ctrl-D handling. Piped / non-interactive input keeps the std-only
//! unbuffered fd-0 byte reader, so a spawned child still inherits any bytes that
//! follow its command line and the integration tests need no terminal.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};

use crate::builtins::{self, Builtin};
use crate::expand::{Piece, VarRef, Word};
use crate::funcs::{FuncDef, Funcs};
use crate::vars::{Value, Vars};
use crate::{exec, expand, parser};

/// The mutable shell session threaded through the run loop: variable scopes,
/// defined functions, and the job table.
struct Shell {
    vars: Vars,
    funcs: Funcs,
    jobs: exec::JobTable,
    control: Option<parser::ControlKind>,
    loop_depth: usize,
}

impl Shell {
    fn new() -> Self {
        Self {
            vars: Vars::new(),
            funcs: Funcs::new(),
            jobs: exec::JobTable::new(),
            control: None,
            loop_depth: 0,
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
            name,
            append,
            value,
        } => {
            if name == "env" {
                eprintln!("mesh: env: cannot assign to the reserved name");
                return Step::Continue(1);
            }
            match eval_expr(value, last, in_function, shell) {
                Ok(value) => {
                    let result = if *append {
                        shell.vars.append(name, value)
                    } else {
                        shell.vars.set_value(name, value);
                        Ok(())
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
            }
        }
        Function {
            name,
            parameters,
            body,
        } => {
            if name == "func" || name == "return" || builtins::is_builtin(name) {
                eprintln!("mesh: func: `{name}` is a reserved name and cannot be a function name");
                return Step::Continue(2);
            }
            if let Some(duplicate) = parameters
                .iter()
                .enumerate()
                .find_map(|(index, parameter)| {
                    parameters[..index].contains(parameter).then_some(parameter)
                })
            {
                eprintln!("mesh: func: duplicate parameter `{duplicate}`");
                return Step::Continue(2);
            }
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
        For {
            binding,
            iterable,
            body,
        } => run_ast_for(binding, iterable, body, last, in_function, shell),
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

fn condition_status(
    condition: &parser::Executable,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<u8, Step> {
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
    binding: &str,
    iterable: &parser::Expr,
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    if binding == "env" {
        eprintln!("mesh: for: `env` is a reserved name and cannot be a binding");
        return Step::Continue(2);
    }
    let value = match eval_expr(iterable, last, in_function, shell) {
        Ok(v) => v,
        Err(step) => return step,
    };
    let values = match value {
        Value::List(v) => v,
        value => vec![value],
    };
    let mut status = 0;
    shell.loop_depth += 1;
    for value in values {
        shell.vars.set_value(binding, value);
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
        } else if let Some(value) = rest.strip_prefix('[') {
            let close = value.find(']').expect("parser validated variable access");
            let index = &value[..close];
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
            rest = &value[close + 1..];
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
                    Value::String(_) | Value::Integer(_) | Value::Boolean(_) => {
                        runtime_error("cannot slice a scalar value")
                    }
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
                Value::String(_) | Value::Integer(_) | Value::Boolean(_) => {
                    runtime_error("cannot index a scalar value")
                }
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
            let Some(modifier) = expand::Modifier::from_name(name) else {
                return runtime_error(format!("modifier :{name} is not implemented yet"));
            };
            let value = eval_expr(value, last, in_function, shell)?;
            expand::apply_modifier(value, modifier)
                .map_err(|error| runtime_message(error.to_string()))
        }
        E::If(node) => eval_if_expr(node, last, in_function, shell),
        E::For {
            binding,
            iterable,
            body,
        } => eval_for_expr(binding, iterable, body, last, in_function, shell),
        E::BackgroundJob(pipeline) => match run_ast_pipeline(pipeline, true, last, shell) {
            Step::Continue(code) => Ok(Value::Integer(i64::from(code))),
            step => Err(step),
        },
        E::Capture(source) => capture_source(source, last, in_function, shell),
        E::Range { .. } => runtime_error("range expressions are not implemented yet"),
        E::Call { .. } => runtime_error("call expressions are not implemented yet"),
        E::Lambda { .. } => runtime_error("lambda expressions are not implemented yet"),
    }
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

fn eval_value_body(
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let value_final = body.statements.last().is_some_and(|statement| {
        !statement.background
            && statement.and_or.rest.is_empty()
            && matches!(
                statement.and_or.first,
                parser::Executable::Expression { .. }
                    | parser::Executable::If(_)
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
    binding: &str,
    iterable: &parser::Expr,
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let iterable = eval_expr(iterable, last, in_function, shell)?;
    let values = match iterable {
        Value::List(values) => values,
        value => vec![value],
    };
    let mut results = Vec::new();
    shell.loop_depth += 1;
    for value in values {
        shell.vars.set_value(binding, value);
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
                parser::Executable::For {
                    binding,
                    iterable,
                    body,
                } => {
                    return eval_for_expr(binding, iterable, body, last, in_function, shell);
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
            Value::Integer(_) | Value::Boolean(_) => {
                return Err("right operand of `in` must be a collection or string".into());
            }
        },
        Match | NotMatch => {
            return Err(format!(
                "operator `{}` is not implemented yet",
                if op == Match { "=~" } else { "!~" }
            ));
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
/// positional parameters in a fresh local scope, runs the body, and returns the
/// function's status — an explicit `return`, else the last command's status. A
/// list argument counts as **one** positional (it arrives intact as a list
/// value); an arity mismatch is a recoverable error.
fn call_func(name: &str, args: Vec<Value>, shell: &mut Shell) -> Step {
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
        shell.vars.set_value(param, arg);
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

/// Return whether the parser needs another physical line to complete the input.
fn needs_more_input(text: &str) -> bool {
    matches!(parser::parse(text), Ok(parser::ParseOutcome::Incomplete))
}

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
            Step::Exit(code) => return ExitCode::from(code),
            Step::Continue(code) => last = code,
            Step::Return(_) => unreachable!("top-level return handled in run_line"),
        }
    }
    // Report an incomplete unit at EOF; a poisoned one was already diagnosed.
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
/// `...` while a multi-line input unit is incomplete. The full status-dashboard
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
    use super::{Shell, Step, eval_binary, handle_signal, needs_more_input, run_line, run_source};
    use crate::parser;
    use crate::vars::Value;
    use reedline::Signal;

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
