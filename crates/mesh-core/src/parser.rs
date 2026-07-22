//! Span-carrying lexer and syntax parser for the M3 language.
//!
//! This module is deliberately independent of expansion and execution.  It is
//! safe for editors and other frontends to use: parsing never reads variables,
//! expands a glob, or starts a process.

use std::ops::Range;

pub type Span = Range<usize>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Word { text: String, quoted: bool },
    Newline,
    Semi,
    Amp,
    AndAnd,
    OrOr,
    Pipe,
    PipeBoth,
    Less,
    Greater,
    Append,
    Heredoc,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    Spread,
    Range,
    RangeInclusive,
    Equal,
    PlusEqual,
    Operator(String),
}

pub type Token = Spanned<TokenKind>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    UnexpectedToken,
    UnexpectedEnd,
    Unterminated(char),
    ChainedComparison,
    Expected(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ParseErrorKind::UnexpectedToken => write!(f, "syntax error: unexpected token"),
            ParseErrorKind::UnexpectedEnd => write!(f, "syntax error: unexpected end of input"),
            ParseErrorKind::Unterminated(c) => write!(f, "syntax error: unclosed `{c}`"),
            ParseErrorKind::ChainedComparison => {
                write!(f, "syntax error: comparisons cannot be chained")
            }
            ParseErrorKind::Expected(expected) => write!(f, "syntax error: expected {expected}"),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Complete(Source),
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub statements: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub and_or: AndOr,
    pub background: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndOr {
    pub first: Executable,
    pub rest: Vec<(AndOrOp, Executable)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndOrOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Executable {
    Pipeline(Pipeline),
    Assignment {
        name: String,
        append: bool,
        value: Expr,
    },
    Function {
        name: String,
        parameters: Vec<String>,
        body: Source,
    },
    If(IfExpr),
    For {
        binding: String,
        iterable: Expr,
        body: Source,
    },
    Control {
        kind: ControlKind,
        value: Option<Expr>,
        guard: Option<Guard>,
    },
    Expression {
        expression: Expr,
        guard: Option<Guard>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Return,
    Break,
    Continue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    pub unless: bool,
    pub condition: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub stages: Vec<Command>,
    pub pipe_stderr: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub items: Vec<CommandItem>,
    pub guard: Option<Guard>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandItem {
    Word(Spanned<String>),
    Redirect {
        kind: RedirectKind,
        target: Spanned<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    Input,
    Output,
    Append,
    Heredoc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfExpr {
    pub condition: Box<Executable>,
    pub then_body: Source,
    pub else_branch: Option<ElseBranch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElseBranch {
    If(Box<IfExpr>),
    Block(Source),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Scalar(Spanned<String>),
    Variable(Spanned<String>),
    List(Vec<ListItem>),
    Map(Vec<MapItem>),
    Unary {
        op: UnaryOp,
        expression: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
    },
    Member {
        value: Box<Expr>,
        name: String,
    },
    Index {
        value: Box<Expr>,
        index: Box<Expr>,
    },
    Modifier {
        value: Box<Expr>,
        name: String,
        arguments: Option<Vec<Argument>>,
    },
    Group(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListItem {
    Value(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapItem {
    Pair(Expr, Expr),
    Spread(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Argument {
    Positional(Expr),
    Named(String, Expr),
    Spread(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Negate,
    Spread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Match,
    NotMatch,
    In,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

/// Produce tokens without performing structural parsing.
pub fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    Lexer::new(source).run()
}

/// Parse a buffered input unit. An open delimiter or trailing operator returns
/// [`ParseOutcome::Incomplete`]; malformed complete input returns an error.
pub fn parse(source: &str) -> Result<ParseOutcome, ParseError> {
    let tokens = match tokenize(source) {
        Ok(tokens) => tokens,
        Err(error) if matches!(error.kind, ParseErrorKind::Unterminated(_)) => {
            return Ok(ParseOutcome::Incomplete);
        }
        Err(error) => return Err(error),
    };
    let mut parser = Parser {
        tokens,
        position: 0,
        source_len: source.len(),
    };
    match parser.source(None) {
        Ok(tree) => Ok(ParseOutcome::Complete(tree)),
        Err(error)
            if matches!(
                error.kind,
                ParseErrorKind::UnexpectedEnd | ParseErrorKind::Unterminated(_)
            ) =>
        {
            Ok(ParseOutcome::Incomplete)
        }
        Err(error) => Err(error),
    }
}

struct Lexer<'a> {
    source: &'a str,
    position: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn run(mut self) -> Result<Vec<Token>, ParseError> {
        let mut tokens = Vec::new();
        while self.position < self.source.len() {
            let start = self.position;
            let c = self.char_at(self.position).expect("position is in bounds");
            if matches!(c, ' ' | '\t' | '\r') {
                self.position += c.len_utf8();
                continue;
            }
            if c == '\\' && self.source[self.position..].starts_with("\\\n") {
                self.position += 2;
                continue;
            }
            if c == '#' {
                while self.position < self.source.len() && self.char_at(self.position) != Some('\n')
                {
                    self.position += self.char_at(self.position).unwrap().len_utf8();
                }
                continue;
            }
            if c == '\n' {
                self.position += 1;
                tokens.push(Spanned {
                    value: TokenKind::Newline,
                    span: start..self.position,
                });
                continue;
            }
            if let Some((text, kind)) = self.punctuation() {
                self.position += text.len();
                tokens.push(Spanned {
                    value: kind,
                    span: start..self.position,
                });
                continue;
            }
            let mut text = String::new();
            let mut quoted = false;
            while self.position < self.source.len() {
                let here = self.char_at(self.position).unwrap();
                if here.is_whitespace()
                    || self.punctuation().is_some()
                    || (here == '#' && text.is_empty())
                {
                    break;
                }
                if here == '\\' {
                    self.position += 1;
                    let Some(next) = self.char_at(self.position) else {
                        text.push('\\');
                        break;
                    };
                    text.push(next);
                    self.position += next.len_utf8();
                    quoted = true;
                    continue;
                }
                let raw =
                    here == 'r' && matches!(self.char_at(self.position + 1), Some('\'' | '"'));
                if matches!(here, '\'' | '"') || raw {
                    let quote = if raw {
                        self.position += 1;
                        self.char_at(self.position).unwrap()
                    } else {
                        here
                    };
                    self.position += 1;
                    quoted = true;
                    let mut closed = false;
                    while self.position < self.source.len() {
                        let inner = self.char_at(self.position).unwrap();
                        self.position += inner.len_utf8();
                        if inner == quote {
                            closed = true;
                            break;
                        }
                        if inner == '\\' && !raw {
                            let Some(escaped) = self.char_at(self.position) else {
                                break;
                            };
                            self.position += escaped.len_utf8();
                            text.push(escaped);
                        } else {
                            text.push(inner);
                        }
                    }
                    if !closed {
                        return Err(ParseError {
                            kind: ParseErrorKind::Unterminated(quote),
                            span: start..self.source.len(),
                        });
                    }
                    continue;
                }
                text.push(here);
                self.position += here.len_utf8();
            }
            if text.is_empty() && !quoted {
                return Err(ParseError {
                    kind: ParseErrorKind::UnexpectedToken,
                    span: start..start + c.len_utf8(),
                });
            }
            tokens.push(Spanned {
                value: TokenKind::Word { text, quoted },
                span: start..self.position,
            });
        }
        Ok(tokens)
    }

    fn char_at(&self, byte: usize) -> Option<char> {
        self.source.get(byte..)?.chars().next()
    }

    fn punctuation(&self) -> Option<(&'static str, TokenKind)> {
        let rest = &self.source[self.position..];
        let choices = [
            ("...", TokenKind::Spread),
            ("..=", TokenKind::RangeInclusive),
            ("|&", TokenKind::PipeBoth),
            ("&&", TokenKind::AndAnd),
            ("||", TokenKind::OrOr),
            (">>", TokenKind::Append),
            ("<<", TokenKind::Heredoc),
            ("+=", TokenKind::PlusEqual),
            ("==", TokenKind::Operator("==".into())),
            ("!=", TokenKind::Operator("!=".into())),
            ("<=", TokenKind::Operator("<=".into())),
            (">=", TokenKind::Operator(">=".into())),
            ("!~", TokenKind::Operator("!~".into())),
            ("..", TokenKind::Range),
        ];
        for (spelling, kind) in choices {
            if rest.starts_with(spelling) {
                return Some((spelling, kind));
            }
        }
        let (spelling, kind) = match rest.chars().next()? {
            ';' => (";", TokenKind::Semi),
            '&' => ("&", TokenKind::Amp),
            '|' => ("|", TokenKind::Pipe),
            '<' => ("<", TokenKind::Less),
            '>' => (">", TokenKind::Greater),
            '(' => ("(", TokenKind::LParen),
            ')' => (")", TokenKind::RParen),
            '[' => ("[", TokenKind::LBracket),
            ']' => ("]", TokenKind::RBracket),
            '{' => ("{", TokenKind::LBrace),
            '}' => ("}", TokenKind::RBrace),
            ',' => (",", TokenKind::Comma),
            ':' => (":", TokenKind::Colon),
            '.' => (".", TokenKind::Dot),
            '=' => ("=", TokenKind::Equal),
            '+' | '-' | '*' | '/' | '%' | '~' => {
                let s = &rest[..rest.chars().next().unwrap().len_utf8()];
                let before = self.source[..self.position].chars().next_back();
                let after = rest[s.len()..].chars().next();
                let boundary = |value: Option<char>| {
                    value.is_none_or(|c| c.is_whitespace() || ",()[]{};".contains(c))
                };
                if !boundary(before) || !boundary(after) {
                    return None;
                }
                return Some((
                    match s {
                        "+" => "+",
                        "-" => "-",
                        "*" => "*",
                        "/" => "/",
                        "%" => "%",
                        _ => "~",
                    },
                    TokenKind::Operator(s.into()),
                ));
            }
            _ => return None,
        };
        Some((spelling, kind))
    }
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    source_len: usize,
}

impl Parser {
    fn source(&mut self, closer: Option<TokenKind>) -> Result<Source, ParseError> {
        let start = self.peek().map_or(self.source_len, |t| t.span.start);
        self.terminators();
        let mut statements = Vec::new();
        while !self.at_end() && !closer.as_ref().is_some_and(|c| self.same(c)) {
            let statement_start = self.peek().unwrap().span.start;
            let and_or = self.and_or()?;
            let background = self.eat(&TokenKind::Amp).is_some();
            let end = self.previous_end();
            statements.push(Statement {
                and_or,
                background,
                span: statement_start..end,
            });
            if background {
                self.terminators();
                continue;
            }
            if self.terminators() == 0
                && !self.at_end()
                && !closer.as_ref().is_some_and(|c| self.same(c))
            {
                return Err(self.error(ParseErrorKind::Expected("a statement separator")));
            }
        }
        if let Some(closer) = closer {
            if self.eat(&closer).is_none() {
                return Err(self.eof(ParseErrorKind::Unterminated(match closer {
                    TokenKind::RBrace => '{',
                    TokenKind::RParen => '(',
                    _ => '[',
                })));
            }
        }
        Ok(Source {
            statements,
            span: start..self.previous_end().max(start),
        })
    }

    fn and_or(&mut self) -> Result<AndOr, ParseError> {
        let first = self.executable()?;
        let mut rest = Vec::new();
        loop {
            let op = if self.eat(&TokenKind::AndAnd).is_some() {
                AndOrOp::And
            } else if self.eat(&TokenKind::OrOr).is_some() {
                AndOrOp::Or
            } else {
                break;
            };
            self.newlines();
            if self.at_end() {
                return Err(self.eof(ParseErrorKind::UnexpectedEnd));
            }
            rest.push((op, self.executable()?));
        }
        Ok(AndOr { first, rest })
    }

    fn executable(&mut self) -> Result<Executable, ParseError> {
        if self.word("func") {
            return self.function();
        }
        if self.word("if") {
            return Ok(Executable::If(self.if_expr()?));
        }
        if self.word("for") {
            return self.for_expr();
        }
        if self.word("return") || self.word("break") || self.word("continue") {
            return self.control();
        }
        if let (Some(name), Some(op)) = (
            self.word_text_at(0),
            self.tokens.get(self.position + 1).map(|t| &t.value),
        ) {
            if matches!(op, TokenKind::Equal | TokenKind::PlusEqual) {
                let name = name.to_owned();
                self.position += 1;
                let append = self.eat(&TokenKind::PlusEqual).is_some();
                if !append {
                    self.expect(&TokenKind::Equal, "`=`")?;
                }
                return Ok(Executable::Assignment {
                    name,
                    append,
                    value: self.expression()?,
                });
            }
        }
        self.pipeline().map(Executable::Pipeline)
    }

    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let mut stages = vec![self.command()?];
        let mut pipe_stderr = Vec::new();
        loop {
            let both = if self.eat(&TokenKind::PipeBoth).is_some() {
                true
            } else if self.eat(&TokenKind::Pipe).is_some() {
                false
            } else {
                break;
            };
            self.newlines();
            if self.at_end() {
                return Err(self.eof(ParseErrorKind::UnexpectedEnd));
            }
            pipe_stderr.push(both);
            stages.push(self.command()?);
        }
        Ok(Pipeline {
            stages,
            pipe_stderr,
        })
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        let mut items = Vec::new();
        while !self.at_command_end() {
            if (self.word("if") || self.word("unless")) && !items.is_empty() {
                break;
            }
            let kind = if self.eat(&TokenKind::Less).is_some() {
                Some(RedirectKind::Input)
            } else if self.eat(&TokenKind::Greater).is_some() {
                Some(RedirectKind::Output)
            } else if self.eat(&TokenKind::Append).is_some() {
                Some(RedirectKind::Append)
            } else if self.eat(&TokenKind::Heredoc).is_some() {
                Some(RedirectKind::Heredoc)
            } else {
                None
            };
            if let Some(kind) = kind {
                let target = self.command_word()?;
                items.push(CommandItem::Redirect { kind, target });
            } else {
                items.push(CommandItem::Word(self.command_word()?));
            }
        }
        if items.is_empty() {
            return Err(self.error(ParseErrorKind::Expected("a command")));
        }
        let guard = self.guard()?;
        Ok(Command { items, guard })
    }

    fn command_word(&mut self) -> Result<Spanned<String>, ParseError> {
        let token = self
            .next()
            .ok_or_else(|| self.eof(ParseErrorKind::UnexpectedEnd))?;
        match token.value {
            TokenKind::Word { text, .. } => Ok(Spanned {
                value: text,
                span: token.span,
            }),
            _ => Err(ParseError {
                kind: ParseErrorKind::Expected("a command word"),
                span: token.span,
            }),
        }
    }

    fn guard(&mut self) -> Result<Option<Guard>, ParseError> {
        let unless = if self.take_word("unless") {
            true
        } else if self.take_word("if") {
            false
        } else {
            return Ok(None);
        };
        Ok(Some(Guard {
            unless,
            condition: self.expression()?,
        }))
    }

    fn function(&mut self) -> Result<Executable, ParseError> {
        self.take_word("func");
        let name = self.name()?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut parameters = Vec::new();
        while !self.same(&TokenKind::RParen) {
            parameters.push(self.name()?);
            if self.eat(&TokenKind::Comma).is_none()
                && !self.same(&TokenKind::RParen)
                && self.peek().is_none()
            {
                return Err(self.eof(ParseErrorKind::Unterminated('(')));
            }
        }
        self.position += 1;
        self.newlines();
        let body = self.block()?;
        Ok(Executable::Function {
            name,
            parameters,
            body,
        })
    }

    fn if_expr(&mut self) -> Result<IfExpr, ParseError> {
        self.take_word("if");
        let condition = Box::new(self.pipeline().map(Executable::Pipeline)?);
        self.newlines();
        let then_body = self.block()?;
        self.newlines();
        let else_branch = if self.take_word("else") {
            self.newlines();
            Some(if self.word("if") {
                ElseBranch::If(Box::new(self.if_expr()?))
            } else {
                ElseBranch::Block(self.block()?)
            })
        } else {
            None
        };
        Ok(IfExpr {
            condition,
            then_body,
            else_branch,
        })
    }

    fn for_expr(&mut self) -> Result<Executable, ParseError> {
        self.take_word("for");
        let binding = self.name()?;
        if !self.take_word("in") {
            return Err(self.error(ParseErrorKind::Expected("`in`")));
        }
        let iterable = self.expression()?;
        self.newlines();
        let body = self.block()?;
        Ok(Executable::For {
            binding,
            iterable,
            body,
        })
    }

    fn control(&mut self) -> Result<Executable, ParseError> {
        let kind = if self.take_word("return") {
            ControlKind::Return
        } else if self.take_word("break") {
            ControlKind::Break
        } else {
            self.take_word("continue");
            ControlKind::Continue
        };
        let value = if self.at_command_end() || self.word("if") || self.word("unless") {
            None
        } else {
            Some(self.expression()?)
        };
        let guard = self.guard()?;
        Ok(Executable::Control { kind, value, guard })
    }

    fn block(&mut self) -> Result<Source, ParseError> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        self.source(Some(TokenKind::RBrace))
    }

    fn expression(&mut self) -> Result<Expr, ParseError> {
        self.binary(1)
    }

    fn binary(&mut self, minimum: u8) -> Result<Expr, ParseError> {
        let mut left = self.prefix()?;
        let mut compared = false;
        loop {
            if minimum <= 5
                && (self.same(&TokenKind::Range) || self.same(&TokenKind::RangeInclusive))
            {
                let inclusive = self.eat(&TokenKind::RangeInclusive).is_some();
                if !inclusive {
                    self.position += 1;
                }
                self.newlines();
                let end = if self.at_expression_end() {
                    None
                } else {
                    Some(Box::new(self.binary(6)?))
                };
                left = Expr::Range {
                    start: Some(Box::new(left)),
                    end,
                    inclusive,
                };
                continue;
            }
            let Some((op, precedence, comparison)) = self.binary_op() else {
                break;
            };
            if precedence < minimum {
                break;
            }
            if comparison && compared {
                return Err(self.error(ParseErrorKind::ChainedComparison));
            }
            self.position += 1;
            self.newlines();
            let right = self.binary(precedence + 1)?;
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
            compared |= comparison;
        }
        Ok(left)
    }

    fn prefix(&mut self) -> Result<Expr, ParseError> {
        if self.take_word("not") {
            return Ok(Expr::Unary {
                op: UnaryOp::Not,
                expression: Box::new(self.prefix()?),
            });
        }
        if self.operator("-") {
            self.position += 1;
            return Ok(Expr::Unary {
                op: UnaryOp::Negate,
                expression: Box::new(self.prefix()?),
            });
        }
        if self.eat(&TokenKind::Spread).is_some() {
            return Ok(Expr::Unary {
                op: UnaryOp::Spread,
                expression: Box::new(self.prefix()?),
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut value = self.primary()?;
        loop {
            if self.eat(&TokenKind::LParen).is_some() {
                value = Expr::Call {
                    callee: Box::new(value),
                    arguments: self.arguments()?,
                };
            } else if self.eat(&TokenKind::Dot).is_some() {
                value = Expr::Member {
                    value: Box::new(value),
                    name: self.name()?,
                };
            } else if self.eat(&TokenKind::LBracket).is_some() {
                let index = self.expression()?;
                self.expect(&TokenKind::RBracket, "`]`")?;
                value = Expr::Index {
                    value: Box::new(value),
                    index: Box::new(index),
                };
            } else if self.same(&TokenKind::Colon)
                && self.word_text_at(1).is_some_and(modifier_name)
            {
                self.position += 1;
                let name = self.name()?;
                let arguments = if self.eat(&TokenKind::LParen).is_some() {
                    Some(self.arguments()?)
                } else {
                    None
                };
                value = Expr::Modifier {
                    value: Box::new(value),
                    name,
                    arguments,
                };
            } else {
                break;
            }
        }
        Ok(value)
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        if self.eat(&TokenKind::LParen).is_some() {
            let value = self.expression()?;
            self.expect(&TokenKind::RParen, "`)`")?;
            return Ok(Expr::Group(Box::new(value)));
        }
        if self.eat(&TokenKind::LBracket).is_some() {
            return self.collection();
        }
        let token = self
            .next()
            .ok_or_else(|| self.eof(ParseErrorKind::UnexpectedEnd))?;
        match token.value {
            TokenKind::Word { text, .. } if text.starts_with('$') => Ok(Expr::Variable(Spanned {
                value: text,
                span: token.span,
            })),
            TokenKind::Word { text, .. } => Ok(Expr::Scalar(Spanned {
                value: text,
                span: token.span,
            })),
            TokenKind::Range | TokenKind::RangeInclusive => {
                self.position -= 1;
                self.range(None)
            }
            _ => Err(ParseError {
                kind: ParseErrorKind::Expected("a value expression"),
                span: token.span,
            }),
        }
    }

    fn collection(&mut self) -> Result<Expr, ParseError> {
        if self.eat(&TokenKind::RBracket).is_some() {
            return Ok(Expr::List(Vec::new()));
        }
        if self.eat(&TokenKind::Colon).is_some() {
            self.expect(&TokenKind::RBracket, "`]`")?;
            return Ok(Expr::Map(Vec::new()));
        }
        let mut values = Vec::new();
        let mut pairs = Vec::new();
        let mut is_map = false;
        loop {
            let spread = self.eat(&TokenKind::Spread).is_some();
            let key = self.expression()?;
            if spread {
                if is_map {
                    pairs.push(MapItem::Spread(key));
                } else {
                    values.push(ListItem::Spread(key));
                }
            } else if self.eat(&TokenKind::Colon).is_some() {
                is_map = true;
                let value = self.expression()?;
                pairs.push(MapItem::Pair(key, value));
            } else if is_map {
                return Err(self.error(ParseErrorKind::Expected("a map pair")));
            } else {
                values.push(ListItem::Value(key));
            }
            self.newlines();
            if self.eat(&TokenKind::RBracket).is_some() {
                break;
            }
            let comma = self.eat(&TokenKind::Comma).is_some();
            self.newlines();
            if self.eat(&TokenKind::RBracket).is_some() {
                if is_map && !comma {
                    return Err(self.error(ParseErrorKind::Expected("`,`")));
                }
                break;
            }
            if is_map && !comma {
                return Err(self.error(ParseErrorKind::Expected("`,`")));
            }
        }
        if is_map {
            for value in values {
                match value {
                    ListItem::Spread(v) => pairs.insert(0, MapItem::Spread(v)),
                    ListItem::Value(_) => {
                        return Err(self.error(ParseErrorKind::Expected("consistent map entries")));
                    }
                }
            }
            Ok(Expr::Map(pairs))
        } else {
            Ok(Expr::List(values))
        }
    }

    fn arguments(&mut self) -> Result<Vec<Argument>, ParseError> {
        let mut result = Vec::new();
        if self.eat(&TokenKind::RParen).is_some() {
            return Ok(result);
        }
        loop {
            if self.eat(&TokenKind::Spread).is_some() {
                result.push(Argument::Spread(self.expression()?));
            } else if self.word_text_at(0).is_some()
                && self
                    .tokens
                    .get(self.position + 1)
                    .is_some_and(|t| matches!(t.value, TokenKind::Colon))
            {
                let name = self.name()?;
                self.position += 1;
                result.push(Argument::Named(name, self.expression()?));
            } else {
                result.push(Argument::Positional(self.expression()?));
            }
            if self.eat(&TokenKind::RParen).is_some() {
                break;
            }
            self.expect(&TokenKind::Comma, "`,`")?;
            if self.eat(&TokenKind::RParen).is_some() {
                break;
            }
        }
        Ok(result)
    }

    fn range(&mut self, start: Option<Expr>) -> Result<Expr, ParseError> {
        let inclusive = self.eat(&TokenKind::RangeInclusive).is_some();
        if !inclusive {
            self.expect(&TokenKind::Range, "a range operator")?;
        }
        let end = if self.at_expression_end() {
            None
        } else {
            Some(Box::new(self.binary(6)?))
        };
        Ok(Expr::Range {
            start: start.map(Box::new),
            end,
            inclusive,
        })
    }

    fn binary_op(&self) -> Option<(BinaryOp, u8, bool)> {
        let token = &self.peek()?.value;
        if matches!(token, TokenKind::Range | TokenKind::RangeInclusive) {
            return None;
        }
        let (op, p, comparison) = match token {
            TokenKind::Word {
                text,
                quoted: false,
            } if text == "or" => (BinaryOp::Or, 1, false),
            TokenKind::Word {
                text,
                quoted: false,
            } if text == "and" => (BinaryOp::And, 2, false),
            TokenKind::Word {
                text,
                quoted: false,
            } if text == "in" => (BinaryOp::In, 4, true),
            TokenKind::Operator(text) => match text.as_str() {
                "==" => (BinaryOp::Equal, 4, true),
                "!=" => (BinaryOp::NotEqual, 4, true),
                "<" => (BinaryOp::Less, 4, true),
                "<=" => (BinaryOp::LessEqual, 4, true),
                ">" => (BinaryOp::Greater, 4, true),
                ">=" => (BinaryOp::GreaterEqual, 4, true),
                "~" => (BinaryOp::Match, 4, true),
                "!~" => (BinaryOp::NotMatch, 4, true),
                "+" => (BinaryOp::Add, 6, false),
                "-" => (BinaryOp::Subtract, 6, false),
                "*" => (BinaryOp::Multiply, 7, false),
                "/" => (BinaryOp::Divide, 7, false),
                "%" => (BinaryOp::Remainder, 7, false),
                _ => return None,
            },
            TokenKind::Less => (BinaryOp::Less, 4, true),
            TokenKind::Greater => (BinaryOp::Greater, 4, true),
            _ => return None,
        };
        Some((op, p, comparison))
    }

    fn name(&mut self) -> Result<String, ParseError> {
        let token = self
            .next()
            .ok_or_else(|| self.eof(ParseErrorKind::UnexpectedEnd))?;
        match token.value {
            TokenKind::Word {
                text,
                quoted: false,
            } if valid_name(&text) => Ok(text),
            _ => Err(ParseError {
                kind: ParseErrorKind::Expected("a name"),
                span: token.span,
            }),
        }
    }
    fn at_command_end(&self) -> bool {
        self.at_end()
            || matches!(
                self.peek().map(|t| &t.value),
                Some(
                    TokenKind::Newline
                        | TokenKind::Semi
                        | TokenKind::Amp
                        | TokenKind::AndAnd
                        | TokenKind::OrOr
                        | TokenKind::Pipe
                        | TokenKind::PipeBoth
                        | TokenKind::LBrace
                        | TokenKind::RBrace
                )
            )
    }
    fn at_expression_end(&self) -> bool {
        self.at_command_end()
            || matches!(
                self.peek().map(|t| &t.value),
                Some(
                    TokenKind::Comma
                        | TokenKind::RParen
                        | TokenKind::RBracket
                        | TokenKind::LBrace
                        | TokenKind::Colon
                )
            )
    }
    fn terminators(&mut self) -> usize {
        let start = self.position;
        while matches!(
            self.peek().map(|t| &t.value),
            Some(TokenKind::Newline | TokenKind::Semi)
        ) {
            self.position += 1;
        }
        self.position - start
    }
    fn newlines(&mut self) {
        while self.eat(&TokenKind::Newline).is_some() {}
    }
    fn word(&self, expected: &str) -> bool {
        matches!(self.peek().map(|t| &t.value), Some(TokenKind::Word { text, quoted: false }) if text == expected)
    }
    fn take_word(&mut self, expected: &str) -> bool {
        if self.word(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    fn word_text_at(&self, offset: usize) -> Option<&str> {
        match &self.tokens.get(self.position + offset)?.value {
            TokenKind::Word {
                text,
                quoted: false,
            } if valid_name(text) => Some(text),
            _ => None,
        }
    }
    fn operator(&self, expected: &str) -> bool {
        matches!(self.peek().map(|t| &t.value), Some(TokenKind::Operator(op)) if op == expected)
    }
    fn same(&self, expected: &TokenKind) -> bool {
        self.peek()
            .is_some_and(|t| std::mem::discriminant(&t.value) == std::mem::discriminant(expected))
    }
    fn eat(&mut self, expected: &TokenKind) -> Option<Token> {
        if self.same(expected) {
            let token = self.tokens[self.position].clone();
            self.position += 1;
            Some(token)
        } else {
            None
        }
    }
    fn expect(
        &mut self,
        expected: &TokenKind,
        description: &'static str,
    ) -> Result<Token, ParseError> {
        self.eat(expected).ok_or_else(|| {
            if self.at_end() {
                self.eof(ParseErrorKind::UnexpectedEnd)
            } else {
                self.error(ParseErrorKind::Expected(description))
            }
        })
    }
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }
    fn next(&mut self) -> Option<Token> {
        let token = self.peek()?.clone();
        self.position += 1;
        Some(token)
    }
    fn at_end(&self) -> bool {
        self.position == self.tokens.len()
    }
    fn previous_end(&self) -> usize {
        self.position
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .map_or(0, |t| t.span.end)
    }
    fn error(&self, kind: ParseErrorKind) -> ParseError {
        ParseError {
            kind,
            span: self
                .peek()
                .map_or(self.source_len..self.source_len, |t| t.span.clone()),
        }
    }
    fn eof(&self, kind: ParseErrorKind) -> ParseError {
        ParseError {
            kind,
            span: self.source_len..self.source_len,
        }
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

fn modifier_name(name: &str) -> bool {
    matches!(
        name,
        "dir"
            | "base"
            | "ext"
            | "exts"
            | "stem"
            | "bare"
            | "len"
            | "first"
            | "last"
            | "rest"
            | "init"
            | "dedup"
            | "upper"
            | "lower"
            | "get"
            | "raw"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(source: &str) -> Source {
        match parse(source).unwrap() {
            ParseOutcome::Complete(tree) => tree,
            ParseOutcome::Incomplete => panic!("unexpected incomplete input"),
        }
    }

    #[test]
    fn tokens_preserve_spans_quotes_and_longest_punctuation() {
        let tokens = tokenize("echo \"a b\"...$xs >>out\n").unwrap();
        assert_eq!(
            tokens[1],
            Spanned {
                value: TokenKind::Word {
                    text: "a b".into(),
                    quoted: true
                },
                span: 5..10
            }
        );
        assert!(matches!(tokens[2].value, TokenKind::Spread));
        assert!(matches!(tokens[4].value, TokenKind::Append));
    }

    #[test]
    fn parses_pipeline_connectors_background_and_redirects() {
        let tree = complete("a <in |& b >out && c &\nd");
        assert_eq!(tree.statements.len(), 2);
        assert!(tree.statements[0].background);
        assert_eq!(tree.statements[0].and_or.rest.len(), 1);
        let Executable::Pipeline(pipeline) = &tree.statements[0].and_or.first else {
            panic!()
        };
        assert_eq!(pipeline.stages.len(), 2);
        assert_eq!(pipeline.pipe_stderr, vec![true]);
    }

    #[test]
    fn parses_functions_blocks_and_control_flow() {
        let tree = complete("func f(x, y) { if test $x { return 1 } else { puts $y } }");
        let Executable::Function {
            parameters, body, ..
        } = &tree.statements[0].and_or.first
        else {
            panic!()
        };
        assert_eq!(parameters, &["x", "y"]);
        assert_eq!(body.statements.len(), 1);
    }

    #[test]
    fn reports_incomplete_delimiters_and_connectors() {
        assert_eq!(parse("x = (1").unwrap(), ParseOutcome::Incomplete);
        assert_eq!(parse("a &&").unwrap(), ParseOutcome::Incomplete);
    }

    #[test]
    fn observes_expression_precedence_and_rejects_chained_comparisons() {
        let tree = complete("x = not $a or $b and $c == 1 + 2 * 3");
        let Executable::Assignment { value, .. } = &tree.statements[0].and_or.first else {
            panic!()
        };
        let Expr::Binary {
            op: BinaryOp::Or,
            right,
            ..
        } = value
        else {
            panic!("or should be the root operator")
        };
        assert!(matches!(
            right.as_ref(),
            Expr::Binary {
                op: BinaryOp::And,
                ..
            }
        ));
        assert!(matches!(
            parse("x = 1 < 2 < 3"),
            Err(ParseError {
                kind: ParseErrorKind::ChainedComparison,
                ..
            })
        ));
    }

    #[test]
    fn parses_lists_maps_ranges_and_postfix_chains() {
        let tree = complete("x = [$xs.a[0]:first 1..=3 ...$more]");
        let Executable::Assignment {
            value: Expr::List(items),
            ..
        } = &tree.statements[0].and_or.first
        else {
            panic!()
        };
        assert_eq!(items.len(), 3);

        let tree = complete("x = [name: value, ...$defaults]");
        let Executable::Assignment {
            value: Expr::Map(items),
            ..
        } = &tree.statements[0].and_or.first
        else {
            panic!()
        };
        assert_eq!(items.len(), 2);
    }
}
