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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteMode {
    Bare,
    Double,
    Single,
    Raw,
    Escaped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPiece {
    Text { text: String, quote: QuoteMode },
    Variable { name: String, quote: QuoteMode },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub pieces: Vec<WordPiece>,
}

impl Word {
    pub fn text(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| match piece {
                WordPiece::Text { text, .. } => text.as_str(),
                WordPiece::Variable { name, .. } => name.as_str(),
            })
            .collect()
    }

    fn is_bare_text(&self, expected: &str) -> bool {
        matches!(self.pieces.as_slice(), [WordPiece::Text { text, quote: QuoteMode::Bare }] if text == expected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeredocBody {
    pub text: String,
    pub raw: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Word(Word),
    HeredocBody(HeredocBody),
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
    Word(Spanned<Word>),
    Redirect {
        kind: RedirectKind,
        target: Spanned<Word>,
        body: Option<Spanned<HeredocBody>>,
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
    Scalar(Spanned<Word>),
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
    BackgroundJob(Pipeline),
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
        let mut line_start = 0;
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
                self.consume_heredocs(&mut tokens, line_start, start)?;
                tokens.push(Spanned {
                    value: TokenKind::Newline,
                    span: start..start + 1,
                });
                line_start = tokens.len();
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
            let mut pieces = Vec::new();
            while self.position < self.source.len() {
                let here = self.char_at(self.position).unwrap();
                if here.is_whitespace()
                    || self.punctuation().is_some()
                    || (here == '#' && pieces.is_empty())
                {
                    break;
                }
                if here == '\\' {
                    self.position += 1;
                    let Some(next) = self.char_at(self.position) else {
                        push_text(&mut pieces, "\\", QuoteMode::Escaped);
                        break;
                    };
                    push_text(&mut pieces, &next.to_string(), QuoteMode::Escaped);
                    self.position += next.len_utf8();
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
                    let mode = if raw {
                        QuoteMode::Raw
                    } else if quote == '\'' {
                        QuoteMode::Single
                    } else {
                        QuoteMode::Double
                    };
                    let mut closed = false;
                    let piece_count = pieces.len();
                    while self.position < self.source.len() {
                        let inner = self.char_at(self.position).unwrap();
                        if inner == quote {
                            self.position += inner.len_utf8();
                            closed = true;
                            break;
                        }
                        if inner == '\\' && !raw {
                            self.position += 1;
                            let Some(escaped) = self.char_at(self.position) else {
                                break;
                            };
                            self.position += escaped.len_utf8();
                            push_text(&mut pieces, &escaped.to_string(), QuoteMode::Escaped);
                        } else if inner == '$' && mode == QuoteMode::Double {
                            let end = variable_end(self.source, self.position);
                            push_variable(
                                &mut pieces,
                                &self.source[self.position..end],
                                QuoteMode::Double,
                            );
                            self.position = end;
                        } else {
                            push_text(&mut pieces, &inner.to_string(), mode);
                            self.position += inner.len_utf8();
                        }
                    }
                    if !closed {
                        return Err(ParseError {
                            kind: ParseErrorKind::Unterminated(quote),
                            span: start..self.source.len(),
                        });
                    }
                    if pieces.len() == piece_count {
                        push_text(&mut pieces, "", mode);
                    }
                    continue;
                }
                if here == '$' {
                    let end = variable_end(self.source, self.position);
                    push_variable(
                        &mut pieces,
                        &self.source[self.position..end],
                        QuoteMode::Bare,
                    );
                    self.position = end;
                } else {
                    push_text(&mut pieces, &here.to_string(), QuoteMode::Bare);
                    self.position += here.len_utf8();
                }
            }
            if pieces.is_empty() {
                return Err(ParseError {
                    kind: ParseErrorKind::UnexpectedToken,
                    span: start..start + c.len_utf8(),
                });
            }
            tokens.push(Spanned {
                value: TokenKind::Word(Word { pieces }),
                span: start..self.position,
            });
        }
        Ok(tokens)
    }

    fn consume_heredocs(
        &mut self,
        tokens: &mut Vec<Token>,
        line_start: usize,
        command_newline: usize,
    ) -> Result<(), ParseError> {
        let mut requests = Vec::new();
        for index in line_start..tokens.len() {
            if matches!(tokens[index].value, TokenKind::Heredoc) {
                let Some(Token {
                    value: TokenKind::Word(word),
                    ..
                }) = tokens.get(index + 1)
                else {
                    return Err(ParseError {
                        kind: ParseErrorKind::Expected("a heredoc delimiter"),
                        span: tokens[index].span.clone(),
                    });
                };
                requests.push((index + 1, word.text(), word_is_quoted(word)));
            }
        }
        if requests.is_empty() {
            return Ok(());
        }

        let mut scan = command_newline + 1;
        for (inserted, (delimiter_index, delimiter, raw)) in requests.into_iter().enumerate() {
            let body_start = scan;
            let mut closing = None;
            while scan <= self.source.len() {
                let line_end = self.source[scan..]
                    .find('\n')
                    .map_or(self.source.len(), |offset| scan + offset);
                let line = self.source[scan..line_end]
                    .strip_suffix('\r')
                    .unwrap_or(&self.source[scan..line_end]);
                if line == delimiter {
                    closing = Some((
                        scan,
                        if line_end < self.source.len() {
                            line_end + 1
                        } else {
                            line_end
                        },
                    ));
                    break;
                }
                if line_end == self.source.len() {
                    break;
                }
                scan = line_end + 1;
            }
            let Some((closing_start, closing_end)) = closing else {
                return Err(ParseError {
                    kind: ParseErrorKind::Unterminated('<'),
                    span: body_start..self.source.len(),
                });
            };
            tokens.insert(
                delimiter_index + 1 + inserted,
                Spanned {
                    value: TokenKind::HeredocBody(HeredocBody {
                        text: self.source[body_start..closing_start].to_owned(),
                        raw,
                    }),
                    span: body_start..closing_start,
                },
            );
            scan = closing_end;
        }
        self.position = scan;
        Ok(())
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

fn push_text(pieces: &mut Vec<WordPiece>, text: &str, quote: QuoteMode) {
    if let Some(WordPiece::Text {
        text: previous,
        quote: previous_quote,
    }) = pieces.last_mut()
        && *previous_quote == quote
    {
        previous.push_str(text);
    } else {
        pieces.push(WordPiece::Text {
            text: text.to_owned(),
            quote,
        });
    }
}

fn push_variable(pieces: &mut Vec<WordPiece>, variable: &str, quote: QuoteMode) {
    pieces.push(WordPiece::Variable {
        name: variable.to_owned(),
        quote,
    });
}

fn variable_end(source: &str, start: usize) -> usize {
    let rest = &source[start..];
    if let Some(braced) = rest.strip_prefix("${") {
        return braced.find('}').map_or(start + 1, |end| start + 3 + end);
    }
    let mut end = start + 1;
    for c in source[end..].chars() {
        if c == '_' || c.is_alphanumeric() {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn word_is_quoted(word: &Word) -> bool {
    word.pieces.iter().any(|piece| match piece {
        WordPiece::Text { quote, .. } | WordPiece::Variable { quote, .. } => {
            *quote != QuoteMode::Bare
        }
    })
}

fn token_word_pieces(kind: &TokenKind) -> Option<Vec<WordPiece>> {
    if let TokenKind::Word(word) = kind {
        return Some(word.pieces.clone());
    }
    let spelling = match kind {
        TokenKind::Dot => ".",
        TokenKind::Colon => ":",
        TokenKind::LBracket => "[",
        TokenKind::RBracket => "]",
        TokenKind::LParen => "(",
        TokenKind::RParen => ")",
        TokenKind::Comma => ",",
        TokenKind::Spread => "...",
        TokenKind::Range => "..",
        TokenKind::RangeInclusive => "..=",
        TokenKind::Equal => "=",
        TokenKind::PlusEqual => "+=",
        TokenKind::Operator(operator) => operator,
        _ => return None,
    };
    Some(vec![WordPiece::Text {
        text: spelling.to_owned(),
        quote: QuoteMode::Bare,
    }])
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
                let value = if !append && !self.value_start() && self.amp_before_terminator() {
                    let pipeline = self.pipeline()?;
                    self.expect(&TokenKind::Amp, "`&`")?;
                    Expr::BackgroundJob(pipeline)
                } else {
                    self.expression()?
                };
                return Ok(Executable::Assignment {
                    name,
                    append,
                    value,
                });
            }
        }
        if self.value_start() {
            let expression = self.expression()?;
            let guard = self.guard()?;
            return Ok(Executable::Expression { expression, guard });
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
            if (self.word("if") || self.word("unless")) && !items.is_empty() && self.viable_guard()
            {
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
                let body = if kind == RedirectKind::Heredoc {
                    let token = self
                        .next()
                        .ok_or_else(|| self.eof(ParseErrorKind::UnexpectedEnd))?;
                    match token.value {
                        TokenKind::HeredocBody(body) => Some(Spanned {
                            value: body,
                            span: token.span,
                        }),
                        _ => {
                            return Err(ParseError {
                                kind: ParseErrorKind::Expected("a heredoc body"),
                                span: token.span,
                            });
                        }
                    }
                } else {
                    None
                };
                items.push(CommandItem::Redirect { kind, target, body });
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

    fn command_word(&mut self) -> Result<Spanned<Word>, ParseError> {
        let first = self
            .next()
            .ok_or_else(|| self.eof(ParseErrorKind::UnexpectedEnd))?;
        let start = first.span.start;
        let mut end = first.span.end;
        let mut pieces = token_word_pieces(&first.value).ok_or_else(|| ParseError {
            kind: ParseErrorKind::Expected("a command word"),
            span: first.span.clone(),
        })?;
        while self.peek().is_some_and(|token| token.span.start == end) {
            let Some(next_pieces) = self
                .peek()
                .and_then(|token| token_word_pieces(&token.value))
            else {
                break;
            };
            end = self.peek().unwrap().span.end;
            self.position += 1;
            pieces.extend(next_pieces);
        }
        Ok(Spanned {
            value: Word { pieces },
            span: start..end,
        })
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
        let condition = Box::new(self.condition()?);
        self.newlines();
        let then_body = self.block()?;
        let before_else_trivia = self.position;
        self.newlines();
        let else_branch = if self.take_word("else") {
            self.newlines();
            Some(if self.word("if") {
                ElseBranch::If(Box::new(self.if_expr()?))
            } else {
                ElseBranch::Block(self.block()?)
            })
        } else {
            self.position = before_else_trivia;
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

    fn condition(&mut self) -> Result<Executable, ParseError> {
        if self.word_text_at(0).is_some()
            && self
                .tokens
                .get(self.position + 1)
                .is_some_and(|token| matches!(token.value, TokenKind::Equal))
        {
            let name = self.name()?;
            self.position += 1;
            return Ok(Executable::Assignment {
                name,
                append: false,
                value: self.expression()?,
            });
        }
        if self.value_start() {
            return Ok(Executable::Expression {
                expression: self.expression()?,
                guard: None,
            });
        }
        self.pipeline().map(Executable::Pipeline)
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
                self.newlines();
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
                self.newlines();
                let index = self.expression()?;
                self.newlines();
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
            self.newlines();
            let value = self.expression()?;
            self.newlines();
            self.expect(&TokenKind::RParen, "`)`")?;
            return Ok(Expr::Group(Box::new(value)));
        }
        if self.eat(&TokenKind::LBracket).is_some() {
            self.newlines();
            return self.collection();
        }
        let token = self
            .next()
            .ok_or_else(|| self.eof(ParseErrorKind::UnexpectedEnd))?;
        match token.value {
            TokenKind::Word(word) => {
                if let [
                    WordPiece::Variable {
                        name,
                        quote: QuoteMode::Bare,
                    },
                ] = word.pieces.as_slice()
                {
                    Ok(Expr::Variable(Spanned {
                        value: name.clone(),
                        span: token.span,
                    }))
                } else {
                    Ok(Expr::Scalar(Spanned {
                        value: word,
                        span: token.span,
                    }))
                }
            }
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
            let mut prefix = Vec::new();
            for value in values {
                match value {
                    ListItem::Spread(v) => prefix.push(MapItem::Spread(v)),
                    ListItem::Value(_) => {
                        return Err(self.error(ParseErrorKind::Expected("consistent map entries")));
                    }
                }
            }
            prefix.extend(pairs);
            Ok(Expr::Map(prefix))
        } else {
            Ok(Expr::List(values))
        }
    }

    fn arguments(&mut self) -> Result<Vec<Argument>, ParseError> {
        let mut result = Vec::new();
        self.newlines();
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
            self.newlines();
            if self.eat(&TokenKind::RParen).is_some() {
                break;
            }
            self.expect(&TokenKind::Comma, "`,`")?;
            self.newlines();
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
            TokenKind::Word(word) if word.is_bare_text("or") => (BinaryOp::Or, 1, false),
            TokenKind::Word(word) if word.is_bare_text("and") => (BinaryOp::And, 2, false),
            TokenKind::Word(word) if word.is_bare_text("in") => (BinaryOp::In, 4, true),
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
            TokenKind::Word(word) if valid_name(&word.text()) && !word_is_quoted(&word) => {
                Ok(word.text())
            }
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
    fn value_start(&self) -> bool {
        match self.peek().map(|token| &token.value) {
            Some(
                TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::Range
                | TokenKind::RangeInclusive,
            ) => true,
            Some(TokenKind::Word(word)) => {
                matches!(
                    word.pieces.as_slice(),
                    [WordPiece::Variable {
                        quote: QuoteMode::Bare,
                        ..
                    }]
                ) || self.tokens.get(self.position + 1).is_some_and(|next| {
                    matches!(next.value, TokenKind::LParen)
                        && next.span.start == self.peek().unwrap().span.end
                })
            }
            _ => false,
        }
    }
    fn amp_before_terminator(&self) -> bool {
        for token in &self.tokens[self.position..] {
            match token.value {
                TokenKind::Amp => return true,
                TokenKind::Newline | TokenKind::Semi | TokenKind::RBrace => return false,
                _ => {}
            }
        }
        false
    }
    fn viable_guard(&mut self) -> bool {
        let saved = self.position;
        self.position += 1;
        let viable = self.expression().is_ok() && self.at_command_end();
        self.position = saved;
        viable
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
        matches!(self.peek().map(|t| &t.value), Some(TokenKind::Word(word)) if word.is_bare_text(expected))
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
            TokenKind::Word(word) if valid_name(&word.text()) && !word_is_quoted(word) => {
                match word.pieces.as_slice() {
                    [
                        WordPiece::Text {
                            text,
                            quote: QuoteMode::Bare,
                        },
                    ] => Some(text),
                    _ => None,
                }
            }
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
                value: TokenKind::Word(Word {
                    pieces: vec![WordPiece::Text {
                        text: "a b".into(),
                        quote: QuoteMode::Double,
                    }],
                }),
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

    #[test]
    fn retains_quote_boundaries_and_interpolation_modes() {
        let tokens = tokenize("\"pre\"$x'$y'").unwrap();
        let TokenKind::Word(word) = &tokens[0].value else {
            panic!()
        };
        assert_eq!(
            word.pieces,
            vec![
                WordPiece::Text {
                    text: "pre".into(),
                    quote: QuoteMode::Double
                },
                WordPiece::Variable {
                    name: "$x".into(),
                    quote: QuoteMode::Bare
                },
                WordPiece::Text {
                    text: "$y".into(),
                    quote: QuoteMode::Single
                },
            ]
        );
    }

    #[test]
    fn assembles_adjacent_punctuation_into_command_words() {
        let tree = complete("echo file.txt ./tool key:value xs[0]");
        let Executable::Pipeline(pipeline) = &tree.statements[0].and_or.first else {
            panic!()
        };
        let words: Vec<_> = pipeline.stages[0]
            .items
            .iter()
            .map(|item| match item {
                CommandItem::Word(word) => word.value.text(),
                CommandItem::Redirect { .. } => panic!(),
            })
            .collect();
        assert_eq!(words, ["echo", "file.txt", "./tool", "key:value", "xs[0]"]);
    }

    #[test]
    fn consumes_heredoc_body_without_parsing_it_as_statements() {
        let tree = complete("cat <<EOF\nhello $name\nEOF\n");
        assert_eq!(tree.statements.len(), 1);
        let Executable::Pipeline(pipeline) = &tree.statements[0].and_or.first else {
            panic!()
        };
        let CommandItem::Redirect {
            body: Some(body), ..
        } = &pipeline.stages[0].items[1]
        else {
            panic!()
        };
        assert_eq!(body.value.text, "hello $name\n");
        assert!(!body.value.raw);
    }

    #[test]
    fn parses_value_conditions_and_value_shaped_statements() {
        let tree = complete("if $x == 1 { puts yes }\n$x\nfoo()\n[one two]");
        let Executable::If(condition) = &tree.statements[0].and_or.first else {
            panic!()
        };
        assert!(matches!(
            condition.condition.as_ref(),
            Executable::Expression { .. }
        ));
        assert!(
            tree.statements[1..]
                .iter()
                .all(|statement| matches!(statement.and_or.first, Executable::Expression { .. }))
        );
    }

    #[test]
    fn keeps_background_pipeline_inside_assignment() {
        let tree = complete("j = make -j8 &");
        let Executable::Assignment {
            value: Expr::BackgroundJob(pipeline),
            ..
        } = &tree.statements[0].and_or.first
        else {
            panic!()
        };
        assert_eq!(pipeline.stages[0].items.len(), 2);
        assert!(!tree.statements[0].background);
    }

    #[test]
    fn leaves_non_viable_guard_keywords_as_arguments() {
        let tree = complete("echo if\necho unless");
        for statement in tree.statements {
            let Executable::Pipeline(pipeline) = statement.and_or.first else {
                panic!()
            };
            assert_eq!(pipeline.stages[0].items.len(), 2);
            assert!(pipeline.stages[0].guard.is_none());
        }
    }

    #[test]
    fn skips_newlines_inside_expression_delimiters() {
        let tree = complete("x = (\n1 +\n2\n)");
        assert!(matches!(
            tree.statements[0].and_or.first,
            Executable::Assignment { .. }
        ));
    }

    #[test]
    fn preserves_map_spread_source_order() {
        let tree = complete("x = [...$a, ...$b, key: value]");
        let Executable::Assignment {
            value: Expr::Map(items),
            ..
        } = &tree.statements[0].and_or.first
        else {
            panic!()
        };
        let names: Vec<_> = items
            .iter()
            .take(2)
            .map(|item| match item {
                MapItem::Spread(Expr::Variable(name)) => name.value.as_str(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(names, ["$a", "$b"]);
    }
}
