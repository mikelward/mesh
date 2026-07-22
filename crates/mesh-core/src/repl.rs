//! Parser-driven read/evaluate loop for the mesh language.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};

use crate::builtins::{self, Builtin};
use crate::funcs::{FuncDef, Funcs};
use crate::lexer::{self, Piece, RedirKind, Word};
use crate::parser::{
    self, AndOrOp, BinaryOp, CommandItem, ControlKind, ElseBranch, Executable, Expr, ListItem,
    QuoteMode, RedirectKind, Source, UnaryOp, WordPiece,
};
use crate::vars::{Value, Vars};
use crate::{exec, expand};

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

#[derive(Debug, PartialEq)]
enum Step {
    Continue(u8),
    Exit(u8),
    Return(u8),
    Break,
    ContinueLoop,
}

pub fn run() -> ExitCode {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        run_interactive()
    } else {
        run_piped()
    }
}

fn run_line(text: &str, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    match parser::parse(text) {
        Ok(parser::ParseOutcome::Complete(source)) => run_source(&source, last, in_function, shell),
        Ok(parser::ParseOutcome::Incomplete) => {
            eprintln!("mesh: syntax error: unexpected end of input");
            Step::Continue(2)
        }
        Err(error) => {
            eprintln!("mesh: {error}");
            Step::Continue(2)
        }
    }
}

fn run_source(source: &Source, mut status: u8, in_function: bool, shell: &mut Shell) -> Step {
    for statement in &source.statements {
        let mut step = run_executable(
            &statement.and_or.first,
            statement.background,
            status,
            in_function,
            shell,
        );
        for (op, executable) in &statement.and_or.rest {
            let code = match step {
                Step::Continue(code) => code,
                _ => return step,
            };
            if matches!(op, AndOrOp::And) == (code == 0) {
                step = run_executable(executable, statement.background, code, in_function, shell);
            }
        }
        match step {
            Step::Continue(code) => status = code,
            other => return other,
        }
    }
    Step::Continue(status)
}

fn run_executable(
    node: &Executable,
    background: bool,
    last: u8,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    match node {
        Executable::Pipeline(pipeline) => run_pipeline(pipeline, background, last, shell),
        Executable::Assignment {
            name,
            append,
            value,
        } => match eval_expr(value, shell, in_function) {
            Ok(value) => {
                let result = if *append {
                    shell.vars.append(name, value)
                } else {
                    shell.vars.set_value(name, value);
                    Ok(())
                };
                match result {
                    Ok(()) => Step::Continue(0),
                    Err(error) => runtime_error(error),
                }
            }
            Err(error) => runtime_error(error),
        },
        Executable::Function {
            name,
            parameters,
            body,
        } => {
            if builtins::is_builtin(name)
                || matches!(name.as_str(), "func" | "return" | "break" | "continue")
            {
                return runtime_error(format!(
                    "func: `{name}` is a reserved name and cannot be a function name"
                ));
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
        Executable::If(expression) => run_if(expression, last, in_function, shell),
        Executable::For {
            binding,
            iterable,
            body,
        } => run_for(binding, iterable, body, in_function, shell),
        Executable::Control { kind, value, guard } => {
            if !guard_allows(guard.as_ref(), shell, in_function) {
                return Step::Continue(last);
            }
            match kind {
                ControlKind::Return if !in_function => {
                    runtime_error("return: not inside a function")
                }
                ControlKind::Return => match value
                    .as_ref()
                    .map(|v| eval_expr(v, shell, in_function))
                    .transpose()
                {
                    Ok(None) => Step::Return(last),
                    Ok(Some(value)) => {
                        value_status(value, "return").map_or_else(runtime_error, Step::Return)
                    }
                    Err(e) => runtime_error(e),
                },
                ControlKind::Break => Step::Break,
                ControlKind::Continue => Step::ContinueLoop,
            }
        }
        Executable::Expression { expression, guard } => {
            if !guard_allows(guard.as_ref(), shell, in_function) {
                return Step::Continue(last);
            }
            match eval_expr(expression, shell, in_function) {
                Ok(value) => Step::Continue(if truthy(&value) { 0 } else { 1 }),
                Err(e) => runtime_error(e),
            }
        }
    }
}

fn runtime_error(message: impl std::fmt::Display) -> Step {
    eprintln!("mesh: {message}");
    Step::Continue(1)
}

fn guard_allows(guard: Option<&parser::Guard>, shell: &mut Shell, in_function: bool) -> bool {
    guard.is_none_or(|guard| {
        eval_expr(&guard.condition, shell, in_function).is_ok_and(|v| truthy(&v) != guard.unless)
    })
}

fn run_if(value: &parser::IfExpr, last: u8, in_function: bool, shell: &mut Shell) -> Step {
    let condition = run_executable(&value.condition, false, last, in_function, shell);
    let take = matches!(condition, Step::Continue(0));
    if !matches!(condition, Step::Continue(_)) {
        return condition;
    }
    if take {
        run_source(&value.then_body, 0, in_function, shell)
    } else {
        match &value.else_branch {
            Some(ElseBranch::If(next)) => run_if(next, last, in_function, shell),
            Some(ElseBranch::Block(body)) => run_source(body, 0, in_function, shell),
            None => Step::Continue(0),
        }
    }
}

fn run_for(
    binding: &str,
    iterable: &Expr,
    body: &Source,
    in_function: bool,
    shell: &mut Shell,
) -> Step {
    let values = match eval_expr(iterable, shell, in_function) {
        Ok(Value::List(v)) => v,
        Ok(Value::String(v)) => vec![Value::String(v)],
        Err(e) => return runtime_error(e),
    };
    let mut status = 0;
    for value in values {
        shell.vars.set_value(binding, value);
        match run_source(body, status, in_function, shell) {
            Step::Continue(code) => status = code,
            Step::ContinueLoop => continue,
            Step::Break => break,
            other => return other,
        }
    }
    Step::Continue(status)
}

fn run_pipeline(
    pipeline: &parser::Pipeline,
    background: bool,
    last: u8,
    shell: &mut Shell,
) -> Step {
    if pipeline.stages.len() == 1 {
        return run_command(&pipeline.stages[0], background, last, shell);
    }
    let mut commands = Vec::new();
    for command in &pipeline.stages {
        match prepare_command(command, shell) {
            Ok((argv, redirs)) if !argv.is_empty() => commands.push(exec::Cmd {
                words: argv,
                redirs,
            }),
            Ok(_) => return runtime_error("empty command in a pipeline"),
            Err(e) => return runtime_error(e),
        }
    }
    Step::Continue(exec::run_pipeline(commands, &mut shell.jobs, background))
}

fn run_command(command: &parser::Command, background: bool, last: u8, shell: &mut Shell) -> Step {
    if !guard_allows(command.guard.as_ref(), shell, false) {
        return Step::Continue(last);
    }
    if !background
        && command
            .items
            .iter()
            .all(|item| matches!(item, CommandItem::Word(_)))
    {
        let words: Result<Vec<_>, _> = command
            .items
            .iter()
            .map(|item| match item {
                CommandItem::Word(word) => adapt_word(&word.value),
                _ => unreachable!(),
            })
            .collect();
        if let Ok(words) = words
            && let Some(first) = words.first()
            && let Ok(mut name) = expand::expand(vec![clone_word(first)], &shell.vars)
            && name.len() == 1
            && let Some(def) = shell.funcs.get(&name[0]).cloned()
        {
            let args = match expand::expand_values(words.into_iter().skip(1).collect(), &shell.vars)
            {
                Ok(values) => values,
                Err(error) => return runtime_error(error),
            };
            return call_func(&name.remove(0), &def, args, shell);
        }
    }
    let (argv, redirs) = match prepare_command(command, shell) {
        Ok(v) => v,
        Err(e) => return runtime_error(e),
    };
    if argv.is_empty() {
        return Step::Continue(0);
    }
    if !redirs.is_empty() || background {
        if builtins::is_builtin(&argv[0]) || shell.funcs.get(&argv[0]).is_some() {
            return runtime_error(format!(
                "{}: builtins and functions cannot be redirected or backgrounded yet",
                argv[0]
            ));
        }
        return Step::Continue(exec::run_pipeline(
            vec![exec::Cmd {
                words: argv,
                redirs,
            }],
            &mut shell.jobs,
            background,
        ));
    }
    let job = match argv[0].as_str() {
        "fg" => Some(shell.jobs.foreground(&argv[1..])),
        "bg" => Some(shell.jobs.background(&argv[1..])),
        "jobs" => Some(shell.jobs.list(&argv[1..])),
        _ => None,
    };
    if let Some(code) = job {
        return Step::Continue(code);
    }
    match builtins::dispatch(&argv, last) {
        Some(Builtin::Exit(c)) => Step::Exit(c),
        Some(Builtin::Status(c)) => Step::Continue(c),
        None => Step::Continue(exec::run(&argv, &mut shell.jobs)),
    }
}

fn clone_word(word: &Word) -> Word {
    Word(
        word.0
            .iter()
            .map(|piece| match piece {
                Piece::Text { text, expandable } => Piece::Text {
                    text: text.clone(),
                    expandable: *expandable,
                },
                Piece::Var(reference) => Piece::Var(reference.clone()),
            })
            .collect(),
    )
}

type PreparedCommand = (Vec<String>, Vec<(RedirKind, String)>);

fn prepare_command(command: &parser::Command, shell: &Shell) -> Result<PreparedCommand, String> {
    let mut words = Vec::new();
    let mut redirs = Vec::new();
    for item in &command.items {
        match item {
            CommandItem::Word(word) => words.push(adapt_word(&word.value)?),
            CommandItem::Redirect { kind, target, body } => {
                if body.is_some() || *kind == RedirectKind::Heredoc {
                    return Err("heredoc execution is not implemented yet".into());
                }
                let mut path = expand::expand(vec![adapt_word(&target.value)?], &shell.vars)
                    .map_err(|e| e.to_string())?;
                if path.len() != 1 {
                    return Err(format!(
                        "ambiguous redirect: target expanded to {} words",
                        path.len()
                    ));
                }
                redirs.push((
                    match kind {
                        RedirectKind::Input => RedirKind::In,
                        RedirectKind::Output => RedirKind::Out,
                        RedirectKind::Append => RedirKind::Append,
                        RedirectKind::Heredoc => unreachable!(),
                    },
                    path.pop().unwrap(),
                ));
            }
        }
    }
    Ok((
        expand::expand(words, &shell.vars).map_err(|e| e.to_string())?,
        redirs,
    ))
}

fn adapt_word(word: &parser::Word) -> Result<Word, String> {
    let mut pieces = Vec::new();
    let mut at = 0;
    while at < word.pieces.len() {
        match &word.pieces[at] {
            WordPiece::Text { text, quote } => pieces.push(Piece::Text {
                text: text.clone(),
                expandable: *quote == QuoteMode::Bare,
            }),
            WordPiece::Variable { name, quote } => {
                let mut spelling = name.clone();
                while let Some(WordPiece::Text {
                    text,
                    quote: QuoteMode::Bare,
                }) = word.pieces.get(at + 1)
                    && matches!(text.chars().next(), Some('.' | '[' | ':'))
                {
                    spelling.push_str(text);
                    at += 1;
                }
                pieces.push(Piece::Var(
                    lexer::parser_var_ref(&spelling, *quote == QuoteMode::Double)
                        .ok_or_else(|| format!("{name}: invalid variable reference"))?,
                ));
            }
        }
        at += 1;
    }
    Ok(Word(pieces))
}

fn call_func(name: &str, def: &FuncDef, args: Vec<Value>, shell: &mut Shell) -> Step {
    if args.len() != def.params.len() {
        return runtime_error(format!(
            "{name}: expected {} argument(s), got {}",
            def.params.len(),
            args.len()
        ));
    }
    shell.vars.push_scope();
    for (param, arg) in def.params.iter().zip(args) {
        shell.vars.set_value(param, arg);
    }
    let result = match run_source(&def.body, 0, true, shell) {
        Step::Return(code) => Step::Continue(code),
        other => other,
    };
    shell.vars.pop_scope();
    result
}

fn eval_expr(expr: &Expr, shell: &mut Shell, in_function: bool) -> Result<Value, String> {
    match expr {
        Expr::Scalar(word) => {
            let mut values = expand::expand_values(vec![adapt_word(&word.value)?], &shell.vars)
                .map_err(|e| e.to_string())?;
            if values.len() == 1 {
                Ok(values.pop().unwrap())
            } else {
                Ok(Value::List(values))
            }
        }
        Expr::Variable(name) => {
            let reference = lexer::parser_var_ref(&name.value, false)
                .ok_or_else(|| format!("{}: invalid variable reference", name.value))?;
            expand::resolve_value(&reference, &shell.vars).map_err(|e| e.to_string())
        }
        Expr::List(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    ListItem::Value(v) => out.push(eval_expr(v, shell, in_function)?),
                    ListItem::Spread(v) => match eval_expr(v, shell, in_function)? {
                        Value::List(v) => out.extend(v),
                        _ => return Err("spread value is not a list".into()),
                    },
                }
            }
            Ok(Value::List(out))
        }
        Expr::Group(v) => eval_expr(v, shell, in_function),
        Expr::Unary { op, expression } => {
            let value = eval_expr(expression, shell, in_function)?;
            match op {
                UnaryOp::Not => Ok(boolean(!truthy(&value))),
                UnaryOp::Negate => Ok(Value::String((-integer(&value)?).to_string())),
                UnaryOp::Spread => Err("spread is only valid in a list or argument list".into()),
            }
        }
        Expr::Binary { left, op, right } => eval_binary(
            eval_expr(left, shell, in_function)?,
            *op,
            eval_expr(right, shell, in_function)?,
        ),
        Expr::Index { value, index } => {
            let values = eval_expr(value, shell, in_function)?;
            let at = integer(&eval_expr(index, shell, in_function)?)?;
            match values {
                Value::List(v) => {
                    let i = if at < 0 { v.len() as i64 + at } else { at };
                    v.get(usize::try_from(i).map_err(|_| "list index out of range")?)
                        .cloned()
                        .ok_or_else(|| "list index out of range".into())
                }
                _ => Err("cannot index a string value".into()),
            }
        }
        Expr::Member { value, name } => {
            if let Expr::Variable(root) = value.as_ref() {
                if root.value == "$env" {
                    return std::env::var(name)
                        .map(Value::String)
                        .map_err(|_| format!("$env.{name}: not set"));
                }
            }
            Err(format!(
                "member access .{name} is not implemented for this value"
            ))
        }
        Expr::Modifier { .. } => {
            Err("expression modifiers with the new parser are not implemented yet".into())
        }
        Expr::If(value) => match run_if(value, 0, in_function, shell) {
            Step::Continue(code) => Ok(Value::String(code.to_string())),
            _ => Err("control flow cannot be used as a value here".into()),
        },
        Expr::For {
            binding,
            iterable,
            body,
        } => match run_for(binding, iterable, body, in_function, shell) {
            Step::Continue(code) => Ok(Value::String(code.to_string())),
            _ => Err("control flow cannot be used as a value here".into()),
        },
        Expr::Range { .. } => Err("range values are not implemented yet".into()),
        Expr::Map(_) => Err("map values are not implemented yet".into()),
        Expr::Call { .. } => Err("expression calls are not implemented yet".into()),
        Expr::BackgroundJob(_) => Err("background job values are not implemented yet".into()),
        Expr::Capture(_) => Err("command capture is not implemented yet".into()),
        Expr::Lambda { .. } => Err("lambda values are not implemented yet".into()),
    }
}

fn eval_binary(left: Value, op: BinaryOp, right: Value) -> Result<Value, String> {
    use BinaryOp::*;
    Ok(match op {
        Or => boolean(truthy(&left) || truthy(&right)),
        And => boolean(truthy(&left) && truthy(&right)),
        Equal => boolean(left == right),
        NotEqual => boolean(left != right),
        Less | LessEqual | Greater | GreaterEqual => {
            let (a, b) = (integer(&left)?, integer(&right)?);
            boolean(match op {
                Less => a < b,
                LessEqual => a <= b,
                Greater => a > b,
                GreaterEqual => a >= b,
                _ => unreachable!(),
            })
        }
        Add => match (left, right) {
            (Value::String(a), Value::String(b)) => Value::String(a + &b),
            (Value::List(mut a), Value::List(b)) => {
                a.extend(b);
                Value::List(a)
            }
            _ => return Err("`+` needs two strings or two lists".into()),
        },
        Subtract | Multiply | Divide | Remainder => {
            let (a, b) = (integer(&left)?, integer(&right)?);
            if b == 0 && matches!(op, Divide | Remainder) {
                return Err("division by zero".into());
            }
            Value::String(
                match op {
                    Subtract => a - b,
                    Multiply => a * b,
                    Divide => a / b,
                    Remainder => a % b,
                    _ => unreachable!(),
                }
                .to_string(),
            )
        }
        Match | NotMatch | In => {
            return Err("match and membership operators are not implemented yet".into());
        }
    })
}
fn integer(v: &Value) -> Result<i64, String> {
    match v {
        Value::String(s) => s.parse().map_err(|_| format!("{s}: expected an integer")),
        _ => Err("expected an integer".into()),
    }
}
fn boolean(v: bool) -> Value {
    Value::String(v.to_string())
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.is_empty() && s != "false" && s != "0",
        Value::List(v) => !v.is_empty(),
    }
}
fn value_status(v: Value, what: &str) -> Result<u8, String> {
    match v {
        Value::String(s) => s
            .parse::<i64>()
            .map(|n| n as u8)
            .map_err(|_| format!("{what}: {s}: numeric argument required")),
        _ => Err(format!("{what}: numeric argument required")),
    }
}

fn parse_incomplete(text: &str) -> bool {
    matches!(parser::parse(text), Ok(parser::ParseOutcome::Incomplete))
}

fn run_interactive() -> ExitCode {
    if let Err(e) = wait_until_foreground().and_then(|_| ignore_interactive_signals()) {
        eprintln!("mesh: terminal setup: {e}");
        return ExitCode::from(1);
    }
    let mut editor = Reedline::create();
    let mut last = 0;
    let mut shell = Shell::new();
    let mut pending = String::new();
    loop {
        shell.jobs.reap();
        let prompt = MeshPrompt {
            failed: last != 0,
            continuation: !pending.is_empty(),
        };
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                pending.push_str(&line);
                pending.push('\n');
                if parse_incomplete(&pending) {
                    continue;
                }
                match run_line(&std::mem::take(&mut pending), last, false, &mut shell) {
                    Step::Exit(c) => return ExitCode::from(c),
                    Step::Continue(c) => last = c,
                    _ => last = 1,
                }
            }
            Ok(Signal::CtrlD) => return ExitCode::from(last),
            Ok(_) => pending.clear(),
            Err(e) => {
                eprintln!("mesh: line editor error: {e}");
                return ExitCode::from(1);
            }
        }
    }
}
fn run_piped() -> ExitCode {
    let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let (mut last, mut shell, mut pending, mut line) = (0, Shell::new(), String::new(), Vec::new());
    loop {
        line.clear();
        match read_line(&mut *stdin, &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("mesh: read error: {e}");
                return ExitCode::from(1);
            }
        }
        let text = match std::str::from_utf8(&line) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("mesh: invalid UTF-8 in input");
                last = 1;
                continue;
            }
        };
        pending.push_str(text);
        if parse_incomplete(&pending) {
            continue;
        }
        match run_line(&std::mem::take(&mut pending), last, false, &mut shell) {
            Step::Exit(c) => return ExitCode::from(c),
            Step::Continue(c) => last = c,
            _ => last = 1,
        }
    }
    if !pending.trim().is_empty() {
        match run_line(&pending, last, false, &mut shell) {
            Step::Exit(c) | Step::Continue(c) => last = c,
            _ => last = 1,
        }
    }
    ExitCode::from(last)
}
fn read_line(reader: &mut impl Read, out: &mut Vec<u8>) -> io::Result<usize> {
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                out.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(out.len())
}

fn wait_until_foreground() -> io::Result<()> {
    if unsafe { libc::signal(libc::SIGTTIN, libc::SIG_DFL) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    loop {
        let fg = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if fg < 0 {
            return Err(io::Error::last_os_error());
        }
        let own = unsafe { libc::getpgrp() };
        if fg == own {
            return Ok(());
        }
        if unsafe { libc::kill(0, libc::SIGTTIN) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
}
fn ignore_interactive_signals() -> io::Result<()> {
    for signal in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTSTP,
        libc::SIGTTOU,
        libc::SIGTERM,
    ] {
        if unsafe { libc::signal(signal, libc::SIG_IGN) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

struct MeshPrompt {
    failed: bool,
    continuation: bool,
}
impl Prompt for MeshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(if self.continuation {
            "..."
        } else if self.failed {
            "mesh!"
        } else {
            "mesh$"
        })
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(" ")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(" ")
    }
    fn render_prompt_history_search_indicator(&self, _: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_ast_drives_nested_functions() {
        let mut shell = Shell::new();
        assert_eq!(
            run_line(
                "func outer(x) { if true { puts $x } }\nouter yes\n",
                0,
                false,
                &mut shell
            ),
            Step::Continue(0)
        );
    }
    #[test]
    fn parser_errors_are_authoritative() {
        let mut shell = Shell::new();
        assert_eq!(
            run_line("x = 1 < 2 < 3", 0, false, &mut shell),
            Step::Continue(2)
        );
    }
    #[test]
    fn parser_completeness_is_the_only_buffer_rule() {
        assert!(parse_incomplete("func f() {\n"));
        assert!(!parse_incomplete("puts '{'\n"));
    }
}
