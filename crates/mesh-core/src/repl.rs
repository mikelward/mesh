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
use crate::{exec, expand, lexer, parser};

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
    let parser_owned = parser_owned_assignment(text);
    match parser::parse(text) {
        Ok(parser::ParseOutcome::Complete(source)) => {
            let control = text.split_whitespace().any(|word| {
                matches!(
                    word.trim_matches(|c: char| !c.is_ascii_alphabetic()),
                    "break" | "continue"
                )
            });
            let operator_assignment = source.statements.iter().any(|statement| {
                executable_has_operator_assignment(&statement.and_or.first)
                    || statement
                        .and_or
                        .rest
                        .iter()
                        .any(|(_, executable)| executable_has_operator_assignment(executable))
            });
            if source_has_compound(&source) || parser_owned || control || operator_assignment {
                return run_source(&source, last, in_function, shell);
            }
        }
        Ok(parser::ParseOutcome::Incomplete)
            if parser_owned || parser_classifies_compound(text) =>
        {
            eprintln!("mesh: syntax error: unexpected end of input");
            return Step::Continue(2);
        }
        Err(error)
            if parser_owned
                || starts_with_compound_keyword(text)
                || matches!(error.kind, parser::ParseErrorKind::ChainedComparison) =>
        {
            eprintln!("mesh: {error}");
            return Step::Continue(2);
        }
        Ok(parser::ParseOutcome::Incomplete) | Err(_) => {}
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
        if shell.control.is_some() {
            break;
        }
    }
    Step::Continue(status)
}

fn executable_has_operator_assignment(executable: &parser::Executable) -> bool {
    matches!(
        executable,
        parser::Executable::Assignment { value, .. } if expression_has_operator(value)
    )
}

fn expression_has_operator(expression: &parser::Expr) -> bool {
    use parser::Expr;
    match expression {
        Expr::Unary { .. } | Expr::Binary { .. } => true,
        Expr::List(items) => items.iter().any(|item| match item {
            parser::ListItem::Value(value) | parser::ListItem::Spread(value) => {
                expression_has_operator(value)
            }
        }),
        Expr::Map(items) => items.iter().any(|item| match item {
            parser::MapItem::Pair(key, value) => {
                expression_has_operator(key) || expression_has_operator(value)
            }
            parser::MapItem::Spread(value) => expression_has_operator(value),
        }),
        Expr::Group(value) | Expr::Member { value, .. } => expression_has_operator(value),
        Expr::Index { value, index } => {
            expression_has_operator(value) || expression_has_operator(index)
        }
        Expr::Modifier {
            value, arguments, ..
        } => {
            expression_has_operator(value)
                || arguments.as_ref().is_some_and(|arguments| {
                    arguments.iter().any(|argument| match argument {
                        parser::Argument::Positional(value)
                        | parser::Argument::Named(_, value)
                        | parser::Argument::Spread(value) => expression_has_operator(value),
                    })
                })
        }
        Expr::Range { start, end, .. } => {
            start.as_deref().is_some_and(expression_has_operator)
                || end.as_deref().is_some_and(expression_has_operator)
        }
        Expr::Call { callee, arguments } => {
            expression_has_operator(callee)
                || arguments.iter().any(|argument| match argument {
                    parser::Argument::Positional(value)
                    | parser::Argument::Named(_, value)
                    | parser::Argument::Spread(value) => expression_has_operator(value),
                })
        }
        Expr::For { iterable, .. } => expression_has_operator(iterable),
        Expr::Scalar(_)
        | Expr::Variable(_)
        | Expr::BackgroundJob(_)
        | Expr::Capture(_)
        | Expr::If(_)
        | Expr::Lambda { .. } => false,
    }
}

fn source_has_compound(source: &parser::Source) -> bool {
    source.statements.iter().any(|statement| {
        executable_has_compound(&statement.and_or.first)
            || statement
                .and_or
                .rest
                .iter()
                .any(|(_, executable)| executable_has_compound(executable))
    })
}

fn starts_with_compound_keyword(text: &str) -> bool {
    let Ok(tokens) = parser::tokenize(text) else {
        return false;
    };
    matches!(
        tokens.first().map(|token| &token.value),
        Some(parser::TokenKind::Word(word))
            if matches!(word.pieces.as_slice(),
                [parser::WordPiece::Text { text, quote: parser::QuoteMode::Bare }]
                    if matches!(text.as_str(), "func" | "if" | "for"))
    )
}

fn parser_classifies_compound(text: &str) -> bool {
    let Ok(tokens) = parser::tokenize(text) else {
        return false;
    };
    let bare_keyword = |token: &parser::Token, expected: &[&str]| {
        matches!(&token.value,
            parser::TokenKind::Word(word)
                if matches!(word.pieces.as_slice(),
                    [parser::WordPiece::Text { text, quote: parser::QuoteMode::Bare }]
                        if expected.contains(&text.as_str())))
    };
    tokens
        .first()
        .is_some_and(|token| bare_keyword(token, &["func", "if", "for"]))
        || matches!(tokens.as_slice(), [_, equal, keyword, ..]
            if matches!(equal.value, parser::TokenKind::Equal)
                && bare_keyword(keyword, &["if", "for"]))
}

fn executable_has_compound(executable: &parser::Executable) -> bool {
    matches!(
        executable,
        parser::Executable::Function { .. }
            | parser::Executable::If(_)
            | parser::Executable::For { .. }
            | parser::Executable::Assignment {
                value: parser::Expr::If(_) | parser::Expr::For { .. },
                ..
            }
    )
}

fn parser_owned_assignment(text: &str) -> bool {
    let trimmed = text.trim_start();
    let name_len = trimmed
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_alphanumeric())
        .last()
        .map_or(0, |(index, c)| index + c.len_utf8());
    let assignment = name_len > 0
        && trimmed[..name_len]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && matches!(
            trimmed[name_len..].trim_start().as_bytes(),
            [b'=', ..] | [b'+', b'=', ..]
        );
    let rhs = trimmed.split_once('=').map_or("", |(_, rhs)| rhs);
    assignment && (rhs.contains(" + ") || rhs.trim_end().ends_with(" +"))
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
        } => match eval_expr(value, last, in_function, shell) {
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
            if !guard_allows(guard.as_ref(), last, in_function, shell) {
                return Step::Continue(last);
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
                        Ok(Some(Value::String(s))) => make_return(&[s], last),
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
                        Step::Continue(1)
                    } else {
                        shell.control = Some(*kind);
                        Step::Continue(0)
                    }
                }
            }
        }
        Expression { expression, guard } => {
            if !guard_allows(guard.as_ref(), last, in_function, shell) {
                return Step::Continue(last);
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
) -> bool {
    guard.is_none_or(
        |guard| match eval_expr(&guard.condition, last, in_function, shell) {
            Ok(value) => truthy(&value) != guard.unless,
            Err(_) => false,
        },
    )
}

fn run_ast_if(node: &parser::IfExpr, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    let condition = run_executable(&node.condition, false, last, in_function, shell);
    let Step::Continue(code) = condition else {
        return condition;
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

fn run_ast_for(
    binding: &str,
    iterable: &parser::Expr,
    body: &parser::Source,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
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

fn run_ast_pipeline(
    node: &parser::Pipeline,
    background: bool,
    last: u8,
    shell: &mut Shell,
) -> Step {
    let mut stages = Vec::with_capacity(node.stages.len());
    for command in &node.stages {
        if !guard_allows(command.guard.as_ref(), last, false, shell) {
            return Step::Continue(last);
        }
        let mut words = Vec::new();
        let mut redirs = Vec::new();
        for item in &command.items {
            match item {
                parser::CommandItem::Word(word) => words.push(expansion_word(&word.value)),
                parser::CommandItem::Redirect { kind, target, .. } => redirs.push(Redir {
                    kind: match kind {
                        parser::RedirectKind::Input => RedirKind::In,
                        parser::RedirectKind::Output => RedirKind::Out,
                        parser::RedirectKind::Append => RedirKind::Append,
                        parser::RedirectKind::Heredoc => {
                            eprintln!("mesh: heredoc execution is not supported yet");
                            return Step::Continue(1);
                        }
                    },
                    target: expansion_word(&target.value),
                }),
            }
        }
        stages.push(Stage { words, redirs });
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
                parser::WordPiece::Variable { name, quote } => Piece::Var(crate::lexer::VarRef {
                    name: name.strip_prefix('$').unwrap_or(name).to_string(),
                    member: None,
                    access: None,
                    modifiers: Vec::new(),
                    quoted: !matches!(quote, parser::QuoteMode::Bare),
                }),
            })
            .collect(),
    )
}

fn eval_expr(
    expr: &parser::Expr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    use parser::{BinaryOp as B, Expr as E, ListItem, UnaryOp as U};
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
        E::Variable(name) => shell
            .vars
            .get(name.value.strip_prefix('$').unwrap_or(&name.value))
            .cloned()
            .ok_or_else(|| {
                eprintln!("mesh: {}: unbound variable", name.value);
                Step::Continue(1)
            }),
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
            .map(|n| Value::String(n.to_string()))
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
            runtime_error(format!("member access .{name} is not implemented yet"))
        }
        E::Index { value, index } => {
            let value = eval_expr(value, last, in_function, shell)?;
            let index_value = eval_expr(index, last, in_function, shell)?;
            let index = number(&index_value).map_err(runtime_message)?;
            match value {
                Value::List(values) => {
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
                Value::String(_) => runtime_error("cannot index a string value"),
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
            let Some(modifier) = crate::lexer::Modifier::from_name(name) else {
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
            Step::Continue(code) => Ok(Value::String(code.to_string())),
            step => Err(step),
        },
        E::Capture(source) => match run_source(source, last, in_function, shell) {
            Step::Continue(code) => Ok(Value::String(code.to_string())),
            step => Err(step),
        },
        E::Map(_) => runtime_error("map expressions are not implemented yet"),
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

fn eval_if_expr(
    node: &parser::IfExpr,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Result<Value, Step> {
    let condition = run_executable(&node.condition, false, last, in_function, shell);
    let Step::Continue(code) = condition else {
        return Err(condition);
    };
    if code == 0 {
        eval_body(&node.then_body, 0, in_function, shell)
    } else {
        match &node.else_branch {
            Some(parser::ElseBranch::If(next)) => eval_if_expr(next, code, in_function, shell),
            Some(parser::ElseBranch::Block(body)) => eval_body(body, 0, in_function, shell),
            None => Ok(Value::String(String::new())),
        }
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
        Value::List(v) => !v.is_empty(),
    }
}
fn bool_value(value: bool) -> Value {
    Value::String(value.to_string())
}
fn number(value: &Value) -> Result<i64, String> {
    match value {
        Value::String(s) => s.parse().map_err(|_| format!("{s}: expected number")),
        _ => Err("expected number".into()),
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
        Add => Value::String(
            number(&left)?
                .checked_add(number(&right)?)
                .ok_or("numeric overflow")?
                .to_string(),
        ),
        Subtract => Value::String(
            number(&left)?
                .checked_sub(number(&right)?)
                .ok_or("numeric overflow")?
                .to_string(),
        ),
        Multiply => Value::String(
            number(&left)?
                .checked_mul(number(&right)?)
                .ok_or("numeric overflow")?
                .to_string(),
        ),
        Divide => Value::String(checked_div(number(&left)?, number(&right)?)?.to_string()),
        Remainder => Value::String(
            number(&left)?
                .checked_rem(number(&right)?)
                .ok_or("division by zero")?
                .to_string(),
        ),
        Less => bool_value(number(&left)? < number(&right)?),
        LessEqual => bool_value(number(&left)? <= number(&right)?),
        Greater => bool_value(number(&left)? > number(&right)?),
        GreaterEqual => bool_value(number(&left)? >= number(&right)?),
        And => bool_value(truthy(&left) && truthy(&right)),
        Or => bool_value(truthy(&left) || truthy(&right)),
        In => match right {
            Value::List(values) => bool_value(values.contains(&left)),
            Value::String(text) => match left {
                Value::String(needle) => bool_value(text.contains(&needle)),
                _ => return Err("left operand of `in` must be a string".into()),
            },
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
    }
}

/// Expand just the command word to its name, if it resolves to a single string —
/// used to look up an in-shell function before the arguments are expanded. A word
/// that expands to zero or several words (an empty glob, a multi-match glob, a
/// bare list) is not a function name, so this returns `None` and the byte-string
/// path takes over.
fn command_name(tokens: &[Word], vars: &Vars) -> Option<String> {
    let first = tokens.first()?;
    let cloned = Word(first.0.iter().map(clone_piece).collect());
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
    let executed = run_source(&body, 0, true, shell);
    let result = match executed {
        Step::Return(code) => Step::Continue(code),
        other => other,
    };
    shell.vars.pop_scope();
    result
}

/// Return whether the parser needs another physical line to complete the input.
fn needs_more_input(text: &str) -> bool {
    parser_classifies_compound(text)
        && matches!(parser::parse(text), Ok(parser::ParseOutcome::Incomplete))
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
            modifiers: v.modifiers.clone(),
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
    if let Some(value) = list_value(rhs.as_slice(), vars) {
        vars.set_value(name, value?);
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

/// Preserve typed values in an exact variable-reference assignment. Command
/// expansion still requires `...`; assignment is a value context instead.
fn assignment_value(vref: &crate::lexer::VarRef, vars: &Vars) -> Result<Value, String> {
    expand::resolve_value(vref, vars).map_err(|error| error.to_string())
}

/// Apply `+=` without coercion: strings concatenate, while lists append a
/// scalar or extend with another list.
fn append_assign(name: &str, rhs: Vec<Word>, vars: &mut Vars) -> Result<(), String> {
    if name == "env" {
        return Err(format!("{name}: cannot assign to the reserved name"));
    }
    let value = if let Some(value) = list_value(rhs.as_slice(), vars) {
        value?
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

/// Remove bare outer brackets from a list expression. Brackets embedded in a
/// quoted piece remain ordinary text.
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

/// Evaluate a bracketed list expression, preserving nested lists and flattening
/// an explicit spread by exactly one level.
fn list_value(rhs: &[Word], vars: &Vars) -> Option<Result<Value, String>> {
    let items = list_literal(rhs)?;
    let mut values = Vec::new();
    let mut index = 0;
    while index < items.len() {
        if starts_list(&items[index]) {
            let start = index;
            let mut depth = 0_i32;
            while index < items.len() {
                depth += bracket_delta(&items[index]);
                index += 1;
                if depth == 0 {
                    break;
                }
            }
            if depth != 0 {
                return Some(Err("list: unmatched `[`".into()));
            }
            match list_value(&items[start..index], vars) {
                Some(Ok(value)) => values.push(value),
                Some(Err(error)) => return Some(Err(error)),
                None => return Some(Err("list: invalid nested list".into())),
            }
        } else {
            let word = Word(items[index].0.iter().map(clone_piece).collect());
            match expand::expand_values(vec![word], vars) {
                Ok(expanded) => values.extend(expanded),
                Err(error) => return Some(Err(error.to_string())),
            }
            index += 1;
        }
    }
    Some(Ok(Value::List(values)))
}

fn starts_list(word: &Word) -> bool {
    matches!(word.0.first(), Some(Piece::Text { text, expandable: true }) if text.starts_with('['))
}

fn bracket_delta(word: &Word) -> i32 {
    // Interior brackets belong to scalar text (often a glob), not list syntax.
    let opens = match word.0.first() {
        Some(Piece::Text {
            text,
            expandable: true,
        }) => text.chars().take_while(|ch| *ch == '[').count(),
        _ => 0,
    };
    let closes = match word.0.last() {
        Some(Piece::Text {
            text,
            expandable: true,
        }) => text.chars().rev().take_while(|ch| *ch == ']').count(),
        _ => 0,
    };
    opens as i32 - closes as i32
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
        assert!(!needs_more_input("puts value |"));
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
        assert_eq!(shell.vars.get("n"), Some(&Value::String("42".to_string())));
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
        assert_eq!(shell.vars.get("answer"), Some(&Value::String("42".into())));
        assert_eq!(
            shell.vars.get("values"),
            Some(&Value::List(vec![
                Value::String("2".into()),
                Value::String("3".into()),
                Value::String("4".into()),
            ]))
        );
    }

    #[test]
    fn parsed_but_unimplemented_expressions_return_runtime_errors() {
        let mut shell = Shell::new();
        let parser::ParseOutcome::Complete(source) = parser::parse("value = [key: value]").unwrap()
        else {
            panic!("source should be complete");
        };

        assert_eq!(run_source(&source, 0, false, &mut shell), Step::Continue(1));
        assert_eq!(shell.vars.get("value"), None);
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
            assert_eq!(shell.vars.get(name), Some(&Value::String(expected.into())));
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
                Value::String(i64::MIN.to_string()),
                parser::BinaryOp::Divide,
                Value::String("-1".into()),
            ),
            Err("numeric overflow".into())
        );
        assert_eq!(
            eval_binary(
                Value::String("1".into()),
                parser::BinaryOp::Divide,
                Value::String("0".into()),
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
