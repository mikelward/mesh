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

    /// The `i64` this word spells, when it is a single **bare** run of text.
    ///
    /// Concatenated text is not enough to go on: `4"2"`, `42""`, and `4\2` all
    /// compose to `42`, but expansion keeps the quoted and escaped pieces and
    /// yields the *string*. Those forms are **already** expressions — the
    /// quoted-literal arm of [`Parser::value_start_in`] claims any word with a
    /// quoted or escaped piece — so this is about not *also* claiming them as
    /// integers. Testing the assembled text would work today only because that arm
    /// runs first, which is an accident of ordering rather than a rule.
    fn bare_integer(&self) -> Option<i64> {
        match self.pieces.as_slice() {
            [
                WordPiece::Text {
                    text,
                    quote: QuoteMode::Bare,
                },
            ] => text.parse().ok(),
            _ => None,
        }
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
    CaptureStart,
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
    /// `>&` — make a descriptor a copy of another (`2>&1`).
    GreaterAmp,
    /// `<&` — the input-side spelling of the same (`0<&3`).
    LessAmp,
    /// `&>` — send both stdout and stderr to one target.
    AmpGreater,
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
    ReservedParameter(String),
    ReservedFunctionName(String),
    DuplicateParameter(String),
    RequiredAfterOptional(String),
    ParameterAfterRest(String),
    OptionalWithRest,
    UnknownEscape(char),
    BadUnicodeEscape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ParseErrorKind::UnexpectedToken => write!(f, "syntax error: unexpected token"),
            ParseErrorKind::UnexpectedEnd => write!(f, "syntax error: unexpected end of input"),
            ParseErrorKind::Unterminated(c) => write!(f, "syntax error: unclosed `{c}`"),
            ParseErrorKind::ChainedComparison => {
                write!(f, "syntax error: comparisons cannot be chained")
            }
            ParseErrorKind::Expected(expected) if expected.starts_with("an empty command") => {
                write!(f, "syntax error: {}", expected.trim_start_matches("an "))
            }
            ParseErrorKind::Expected(expected) if expected.starts_with("a redirection") => {
                write!(f, "syntax error: {}", expected.trim_start_matches("a "))
            }
            ParseErrorKind::Expected(expected) => write!(f, "syntax error: expected {expected}"),
            ParseErrorKind::ReservedParameter(name) => {
                write!(
                    f,
                    "syntax error: `{name}` is reserved and cannot be a parameter"
                )
            }
            ParseErrorKind::ReservedFunctionName(name) => {
                write!(
                    f,
                    "syntax error: `{name}` is a built-in value constructor and cannot be a function name"
                )
            }
            ParseErrorKind::DuplicateParameter(name) => {
                write!(f, "syntax error: duplicate parameter `{name}`")
            }
            ParseErrorKind::RequiredAfterOptional(name) => write!(
                f,
                "syntax error: required parameter `{name}` cannot follow an optional one"
            ),
            ParseErrorKind::ParameterAfterRest(name) => write!(
                f,
                "syntax error: parameter `{name}` cannot follow a `...rest` parameter"
            ),
            ParseErrorKind::OptionalWithRest => write!(
                f,
                "syntax error: an optional positional cannot combine with a `...rest` parameter"
            ),
            ParseErrorKind::UnknownEscape(c) => write!(f, "syntax error: invalid escape \\{c}"),
            ParseErrorKind::BadUnicodeEscape => {
                write!(f, "syntax error: invalid \\u{{…}} escape")
            }
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
        pattern: BindingPattern,
        append: bool,
        value: Expr,
    },
    /// `$env.KEY = value` — a write to the process environment rather than to a
    /// mesh binding. Separate from [`Executable::Assignment`] because `$env` is
    /// a reserved namespace whose entries are bytes, not typed values; general
    /// member assignment (`$m.key = v`) is a later, wider change.
    EnvAssignment {
        key: String,
        append: bool,
        value: Expr,
    },
    Function {
        name: String,
        parameters: Vec<Param>,
        body: Source,
    },
    If(IfExpr),
    Match(MatchExpr),
    For {
        bindings: Vec<BindingPattern>,
        iterable: Expr,
        body: Source,
    },
    /// `while COND { … }` — the condition is tested before each pass, taking the
    /// same forms `if` does: a command's status or a value's truthiness.
    While {
        condition: Box<Executable>,
        body: Source,
    },
    /// `loop { … }` — repeats until a `break`. Clearer than `while true`, and
    /// the one loop whose header cannot end it.
    Loop {
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

/// A single `func`/lambda parameter: its name and how it binds at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
}

/// The four signature roles a parameter can play (`DESIGN.md` §"Functions").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKind {
    /// A required positional (`name`), bound left to right.
    Required,
    /// An optional positional with a default (`name = expr`), omittable from the
    /// right.
    Optional(Expr),
    /// A boolean switch flag (`--name`): `true` iff passed, `false` otherwise.
    Switch,
    /// A valued flag with a default (`--name = expr`).
    Flag(Expr),
    /// A rest parameter (`...name`, last): collects leftover positionals as a list.
    Rest,
}

/// A parameter's ordering role, the only thing the sequencing rules care about
/// (defaults and flag values are irrelevant to ordering). Derived from a full
/// [`ParamKind`] or from a still-forming [`ParameterHead`] so both the executed
/// parse and the continuation check apply the same rules.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OrderClass {
    Required,
    Optional,
    Rest,
    /// A flag or switch — order-independent.
    Independent,
}

impl ParamKind {
    fn order_class(&self) -> OrderClass {
        match self {
            ParamKind::Required => OrderClass::Required,
            ParamKind::Optional(_) => OrderClass::Optional,
            ParamKind::Rest => OrderClass::Rest,
            ParamKind::Switch | ParamKind::Flag(_) => OrderClass::Independent,
        }
    }
}

/// The result of parsing a parameter's *head* — its name and role — before any
/// valued default expression. Splitting the head out lets a name be validated
/// (reserved/duplicate/ordering) before the default is parsed, so a bad name with
/// an unfinished default (`func f(env =`⏎) dispatches instead of buffering.
struct ParameterHead {
    name: String,
    class: OrderClass,
    /// Whether an `=` was consumed, so a default expression still needs parsing.
    has_default: bool,
}

/// A binding pattern shared by assignments, conditional bindings, loops, and
/// list-shaped match arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingPattern {
    Name(String),
    Ignore,
    Rest(String),
    List(Vec<BindingPattern>),
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
        /// The descriptor named by a `N>`-style prefix, if any. `None` means the
        /// default for the direction: stdin for `<`, stdout for `>` / `>>`.
        fd: Option<u32>,
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
    /// `N>&M` — the target names a descriptor, not a path. Output side, so a
    /// missing `N` defaults to stdout.
    DuplicateOut,
    /// `N<&M` — the input-side spelling, defaulting to stdin. Kept distinct from
    /// [`RedirectKind::DuplicateOut`] precisely so that default survives.
    DuplicateIn,
    /// `&> file` — stdout and stderr to one target, the shorthand for
    /// `> file 2>&1`.
    Both,
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
pub struct MatchExpr {
    pub value: Box<Expr>,
    pub arms: Vec<MatchArm>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub guard: Option<Expr>,
    pub body: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchPattern {
    Binding(BindingPattern),
    Wildcard,
    Value(Expr),
    Alternation(Vec<MatchPattern>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Scalar(Spanned<Word>),
    /// A context-sensitive `/.../` literal in a match operand.
    Regex(String),
    /// A bare whole-string glob in a match operand.
    Glob(String),
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
    Capture(Source),
    If(Box<IfExpr>),
    Match(Box<MatchExpr>),
    For {
        bindings: Vec<BindingPattern>,
        iterable: Box<Expr>,
        body: Source,
    },
    Lambda {
        parameters: Vec<Param>,
        body: Source,
    },
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
    let tokens = tokenize(source)?;
    let open_block = tokens
        .iter()
        .fold(0_usize, |depth, token| match token.value {
            TokenKind::LBrace => depth + 1,
            TokenKind::RBrace => depth.saturating_sub(1),
            _ => depth,
        })
        > 0;
    let mut parser = Parser {
        tokens,
        position: 0,
        source_len: source.len(),
    };
    match parser.source(None) {
        Ok(tree) => Ok(ParseOutcome::Complete(tree)),
        Err(error)
            if open_block
                || matches!(
                    error.kind,
                    ParseErrorKind::UnexpectedEnd | ParseErrorKind::Unterminated(_)
                ) =>
        {
            Ok(ParseOutcome::Incomplete)
        }
        Err(error) => Err(error),
    }
}

/// How a still-open `func` parameter list (the text after `(`, before any `)`)
/// relates to the signature grammar — used by the multi-line reader to decide
/// buffer-vs-dispatch without a second copy of the grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixStatus {
    /// A complete, valid parameter list so far (the `)` may still follow).
    Complete,
    /// A valid list that simply ran out of input mid-parameter (a default not yet
    /// typed, a trailing comma) — keep reading.
    Incomplete,
    /// A shape the parser can never accept (bad/`env`/duplicate name, a bad
    /// ordering, a detached default) — dispatch so the error is reported now.
    Malformed,
}

/// Classify a still-open parameter list `list` with the **real** signature
/// grammar (the same [`Parser::parameter`] and ordering rules an executed
/// definition uses), so the reader never needs to re-implement that grammar.
///
/// Caller contract: `list` is the text just past the signature's `(`, with the
/// closing `)` not yet present, and any final token the user may still be typing
/// already trimmed (so the parser does not finalize a growing name early).
pub fn params_prefix_status(list: &str) -> PrefixStatus {
    // A tokenize failure is an unterminated quote/raw string — the same thing that
    // makes `parse()` return an error (it propagates `tokenize(..)?`), so it is a
    // hard error the reader dispatches, not an open construct to buffer.
    let Ok(tokens) = tokenize(list) else {
        return PrefixStatus::Malformed;
    };
    Parser {
        tokens,
        position: 0,
        source_len: list.len(),
    }
    .parameters_prefix()
}

/// A parse error that only means "the input ran out" — the reader buffers on it
/// rather than dispatching, matching how [`parse`] maps these to `Incomplete`.
fn is_incomplete(kind: &ParseErrorKind) -> bool {
    matches!(
        kind,
        ParseErrorKind::UnexpectedEnd | ParseErrorKind::Unterminated(_)
    )
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
                let raw = here == 'r'
                    && pieces.is_empty()
                    && matches!(self.char_at(self.position + 1), Some('\'' | '"'));
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
                            let escape_start = self.position;
                            self.position += 1;
                            let Some(escaped) = self.char_at(self.position) else {
                                break;
                            };
                            let decoded = match escaped {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                'e' => '\u{1b}',
                                '\\' => '\\',
                                '\'' if quote == '\'' => '\'',
                                '"' if quote == '"' => '"',
                                '$' if quote == '"' => '$',
                                'u' => {
                                    let (value, end) = decode_unicode_escape(
                                        self.source,
                                        self.position + escaped.len_utf8(),
                                    )
                                    .ok_or_else(|| ParseError {
                                        kind: ParseErrorKind::BadUnicodeEscape,
                                        span: escape_start..self.position + 1,
                                    })?;
                                    self.position = end;
                                    push_text(&mut pieces, &value.to_string(), mode);
                                    continue;
                                }
                                other => {
                                    return Err(ParseError {
                                        kind: ParseErrorKind::UnknownEscape(other),
                                        span: escape_start..self.position + other.len_utf8(),
                                    });
                                }
                            };
                            self.position += escaped.len_utf8();
                            push_text(&mut pieces, &decoded.to_string(), mode);
                        } else if inner == '$' && mode == QuoteMode::Double {
                            let end = variable_end(self.source, self.position)?;
                            if end == self.position + 1 {
                                push_text(&mut pieces, "$", QuoteMode::Double);
                            } else {
                                push_variable(
                                    &mut pieces,
                                    &self.source[self.position..end],
                                    QuoteMode::Double,
                                );
                            }
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
                    let end = variable_end(self.source, self.position)?;
                    if end == self.position + 1 {
                        push_text(&mut pieces, "$", QuoteMode::Bare);
                    } else {
                        push_variable(
                            &mut pieces,
                            &self.source[self.position..end],
                            QuoteMode::Bare,
                        );
                    }
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
            ("$(", TokenKind::CaptureStart),
            ("...", TokenKind::Spread),
            ("..=", TokenKind::RangeInclusive),
            ("|&", TokenKind::PipeBoth),
            ("&&", TokenKind::AndAnd),
            ("||", TokenKind::OrOr),
            (">>", TokenKind::Append),
            ("<<", TokenKind::Heredoc),
            (">&", TokenKind::GreaterAmp),
            ("<&", TokenKind::LessAmp),
            ("&>", TokenKind::AmpGreater),
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
                    value.is_none_or(|c| c.is_whitespace() || ",()[]{};=:".contains(c))
                };
                // A prefix minus belongs to the expression grammar even when it
                // is attached to its operand (`-$n`). Binary operators retain
                // their whitespace/delimiter boundary requirement.
                let attached_prefix_operand = s == "-"
                    && after.is_some_and(|c| c == '$' || c.is_ascii_digit() || "'\"([".contains(c));
                if !boundary(before) || (!boundary(after) && !attached_prefix_operand) {
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

fn variable_end(source: &str, start: usize) -> Result<usize, ParseError> {
    let rest = &source[start..];
    if let Some(braced) = rest.strip_prefix("${") {
        let Some(close) = braced.find('}') else {
            return Err(ParseError {
                kind: ParseErrorKind::Unterminated('}'),
                span: start..source.len(),
            });
        };
        if !valid_variable_access(&braced[..close]) {
            return Err(ParseError {
                kind: ParseErrorKind::Expected("a variable name or access"),
                span: start + 2..start + 2 + close,
            });
        }
        return Ok(start + 3 + close);
    }
    let mut end = start + 1;
    let mut chars = source[end..].char_indices().peekable();
    let Some((offset, head)) = chars.next() else {
        return Ok(end);
    };
    if !head.is_alphabetic() {
        return Ok(end);
    }
    end = start + 1 + offset + head.len_utf8();
    while let Some((offset, c)) = chars.next() {
        if c == '_' || c.is_alphanumeric() {
            end = start + 1 + offset + c.len_utf8();
        } else if c == '-'
            && chars
                .peek()
                .is_some_and(|(_, next)| *next == '_' || next.is_alphanumeric())
        {
            end = start + 1 + offset + 1;
        } else {
            break;
        }
    }
    Ok(variable_suffix_end(source, start, end))
}

fn variable_suffix_end(source: &str, start: usize, mut end: usize) -> usize {
    loop {
        let rest = &source[end..];
        let candidate = if let Some(member) = rest.strip_prefix('.') {
            let length = member
                .char_indices()
                .take_while(|(_, ch)| *ch == '_' || *ch == '-' || ch.is_alphanumeric())
                .map(|(offset, ch)| offset + ch.len_utf8())
                .last()
                .unwrap_or(0);
            (length > 0).then_some(end + 1 + length)
        } else if rest.starts_with('[') {
            subscript_end(rest).map(|length| end + length)
        } else {
            None
        };
        let Some(candidate) = candidate else { break };
        if !valid_variable_access(&source[start + 1..candidate]) {
            break;
        }
        end = candidate;
    }
    end
}

pub(crate) fn subscript_end(rest: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in rest.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if ch == '\\' && quote.is_some() {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && ch == ']' {
            return Some(offset + 1);
        }
    }
    None
}

fn valid_variable_access(value: &str) -> bool {
    let name_end = value.find(['.', '[', ':']).unwrap_or(value.len());
    if !valid_name(&value[..name_end]) {
        return false;
    }
    let mut rest = &value[name_end..];
    while !rest.is_empty() {
        if let Some(member) = rest.strip_prefix('.') {
            let end = member.find(['.', '[', ':']).unwrap_or(member.len());
            if !valid_name(&member[..end]) {
                return false;
            }
            rest = &member[end..];
        } else if rest.starts_with('[') {
            let Some(close) = subscript_end(rest) else {
                return false;
            };
            if !valid_variable_subscript(&rest[1..close - 1]) {
                return false;
            }
            rest = &rest[close..];
        } else if let Some(modifier) = rest.strip_prefix(':') {
            let end = modifier.find(':').unwrap_or(modifier.len());
            if !modifier_name(&modifier[..end]) {
                return false;
            }
            rest = &modifier[end..];
        } else {
            return false;
        }
    }
    true
}

fn valid_variable_subscript(value: &str) -> bool {
    let signed_integer = |text: &str| {
        let digits = text.strip_prefix('-').unwrap_or(text);
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    };
    if let Some((start, end)) = value.split_once("..=") {
        (start.is_empty() || signed_integer(start)) && signed_integer(end)
    } else if let Some((start, end)) = value.split_once("..") {
        (start.is_empty() || signed_integer(start)) && (end.is_empty() || signed_integer(end))
    } else {
        signed_integer(value)
            || valid_name(value)
            || value.strip_prefix('$').is_some_and(valid_name)
            || quoted_subscript(value)
    }
}

fn quoted_subscript(value: &str) -> bool {
    let Some(quote) = value.chars().next().filter(|ch| matches!(ch, '\'' | '"')) else {
        return false;
    };
    let Some(inner) = value
        .strip_prefix(quote)
        .and_then(|v| v.strip_suffix(quote))
    else {
        return false;
    };
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return false;
        }
    }
    !escaped
}

fn decode_unicode_escape(source: &str, start: usize) -> Option<(char, usize)> {
    let rest = source.get(start..)?;
    let hex = rest.strip_prefix('{')?;
    let close = hex.find('}')?;
    if close == 0 || close > 6 || !hex[..close].chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(&hex[..close], 16).ok()?;
    Some((char::from_u32(value)?, start + close + 2))
}

fn word_is_quoted(word: &Word) -> bool {
    word.pieces.iter().any(|piece| match piece {
        WordPiece::Text { quote, .. } | WordPiece::Variable { quote, .. } => {
            *quote != QuoteMode::Bare
        }
    })
}

fn merge_command_variable_access(pieces: Vec<WordPiece>) -> Vec<WordPiece> {
    let mut coalesced = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match piece {
            WordPiece::Text { text, quote } => push_text(&mut coalesced, &text, quote),
            piece => coalesced.push(piece),
        }
    }
    let mut output = Vec::with_capacity(coalesced.len());
    for piece in coalesced {
        if let WordPiece::Text { text, quote } = &piece
            && let Some(WordPiece::Variable {
                name,
                quote: variable_quote,
            }) = output.last_mut()
            && *variable_quote == *quote
            && !name.ends_with('}')
        {
            let length = variable_access_prefix(text);
            if length > 0 {
                name.push_str(&text[..length]);
                if length < text.len() {
                    output.push(WordPiece::Text {
                        text: text[length..].to_string(),
                        quote: *quote,
                    });
                }
                continue;
            }
        }
        output.push(piece);
    }
    output
}

fn variable_access_prefix(text: &str) -> usize {
    let mut consumed = 0;
    loop {
        let rest = &text[consumed..];
        if let Some(value) = rest.strip_prefix('.') {
            let length = value
                .find(['.', '[', ':', '!', '/', ' '])
                .unwrap_or(value.len());
            if length == 0 || !valid_name(&value[..length]) {
                break;
            }
            consumed += length + 1;
        } else if rest.starts_with('[') {
            let Some(close) = subscript_end(rest) else {
                break;
            };
            if !valid_variable_subscript(&rest[1..close - 1]) {
                break;
            }
            consumed += close;
        } else if let Some(value) = rest.strip_prefix(':') {
            let length = value
                .find(['.', '[', ':', '!', '/', ' '])
                .unwrap_or(value.len());
            if length == 0 || !modifier_name(&value[..length]) {
                break;
            }
            consumed += length + 1;
        } else {
            break;
        }
    }
    consumed
}

fn match_operand(expression: Expr) -> Expr {
    match expression {
        Expr::Modifier {
            value,
            name,
            arguments,
        } => Expr::Modifier {
            value: Box::new(match_operand(*value)),
            name,
            arguments,
        },
        Expr::Scalar(word)
            if word.value.pieces.iter().all(|piece| {
                matches!(
                    piece,
                    WordPiece::Text {
                        quote: QuoteMode::Bare | QuoteMode::Escaped,
                        ..
                    }
                )
            }) =>
        {
            let pieces = &word.value.pieces;
            let text = word.value.text();
            let clean_regex = text.starts_with('/')
                && text.ends_with('/')
                && text.len() >= 2
                && pieces
                    .iter()
                    .enumerate()
                    .all(|(piece_index, piece)| match piece {
                        WordPiece::Text {
                            text,
                            quote: QuoteMode::Bare,
                        } => text.char_indices().all(|(byte, c)| {
                            c != '/'
                                || (piece_index == 0 && byte == 0)
                                || (piece_index + 1 == pieces.len() && byte + 1 == text.len())
                        }),
                        WordPiece::Text {
                            quote: QuoteMode::Escaped,
                            ..
                        } => true,
                        _ => false,
                    });
            if clean_regex {
                let mut source = String::new();
                for piece in pieces {
                    let WordPiece::Text { text, quote } = piece else {
                        unreachable!()
                    };
                    if *quote == QuoteMode::Escaped && text != "/" {
                        source.push('\\');
                    }
                    source.push_str(text);
                }
                Expr::Regex(source[1..source.len() - 1].to_owned())
            } else if pieces.iter().all(|piece| {
                matches!(
                    piece,
                    WordPiece::Text {
                        quote: QuoteMode::Bare,
                        ..
                    }
                )
            }) {
                Expr::Glob(text)
            } else {
                Expr::Scalar(word)
            }
        }
        other => other,
    }
}

fn match_pattern_operand(expression: Expr) -> Expr {
    let original = expression.clone();
    match match_operand(expression) {
        Expr::Glob(pattern) if !pattern.contains(['*', '?', '[', ']', '{', '}']) => original,
        pattern => pattern,
    }
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

/// Names that cannot be user functions because they are built-in **value**
/// constructors, reachable only through the `name(...)` call form.
const RESERVED_FUNCTION_NAMES: &[&str] = &["re"];

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    source_len: usize,
}

impl Parser {
    fn source(&mut self, closer: Option<TokenKind>) -> Result<Source, ParseError> {
        let start = self.peek().map_or(self.source_len, |t| t.span.start);
        self.newlines();
        if self.same(&TokenKind::Semi) {
            return Err(self.error(ParseErrorKind::Expected("an empty command")));
        }
        if self.same(&TokenKind::Amp) {
            return Err(self.error(ParseErrorKind::Expected(
                "a background operator needs a command",
            )));
        }
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
            if self.same(&TokenKind::Semi)
                && self
                    .tokens
                    .get(self.position + 1)
                    .is_some_and(|token| matches!(token.value, TokenKind::Semi))
            {
                return Err(self.error(ParseErrorKind::Expected("an empty command")));
            }
            if self.terminators() == 0
                && !self.at_end()
                && !closer.as_ref().is_some_and(|c| self.same(c))
            {
                return Err(self.error(ParseErrorKind::Expected("a statement separator")));
            }
        }
        if let Some(closer) = closer
            && self.eat(&closer).is_none()
        {
            return Err(self.eof(ParseErrorKind::Unterminated(match closer {
                TokenKind::RBrace => '{',
                TokenKind::RParen => '(',
                _ => '[',
            })));
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
        if self.word("func")
            && !self
                .tokens
                .get(self.position + 1)
                .is_some_and(|token| matches!(token.value, TokenKind::LParen))
        {
            return self.function();
        }
        if self.word("if") {
            return Ok(Executable::If(self.if_expr()?));
        }
        if self.word("match") {
            return Ok(Executable::Match(self.match_expr()?));
        }
        if self.word("for") {
            return self.for_expr();
        }
        if self.word("while") {
            return self.while_expr();
        }
        if self.word("loop") {
            return self.loop_expr();
        }
        if self.word("return") || self.word("break") || self.word("continue") {
            return self.control();
        }
        let assignment_start = self.position;
        if let Some(key) = self.env_target() {
            if matches!(
                self.peek().map(|token| &token.value),
                Some(TokenKind::Equal | TokenKind::PlusEqual)
            ) {
                let append = self.eat(&TokenKind::PlusEqual).is_some();
                if !append {
                    self.expect(&TokenKind::Equal, "`=`")?;
                }
                let value = self.expression()?;
                return Ok(Executable::EnvAssignment { key, append, value });
            }
            self.position = assignment_start;
        }
        if self.word_text_at(0).is_some() || self.same(&TokenKind::LBracket) {
            if let Ok(pattern) = self.binding_pattern()
                && matches!(
                    self.peek().map(|token| &token.value),
                    Some(TokenKind::Equal | TokenKind::PlusEqual)
                )
            {
                let append = self.eat(&TokenKind::PlusEqual).is_some();
                if !append {
                    self.expect(&TokenKind::Equal, "`=`")?;
                }
                if append && !matches!(pattern, BindingPattern::Name(_)) {
                    return Err(self.error(ParseErrorKind::Expected("a name before `+=`")));
                }
                let value = if !append && !self.value_start() && self.amp_before_terminator() {
                    let pipeline = self.pipeline()?;
                    self.expect(&TokenKind::Amp, "`&`")?;
                    Expr::BackgroundJob(pipeline)
                } else {
                    self.expression()?
                };
                return Ok(Executable::Assignment {
                    pattern,
                    append,
                    value,
                });
            }
            self.position = assignment_start;
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
            } else if self.eat(&TokenKind::GreaterAmp).is_some() {
                // `>&` is two operators wearing one spelling, told apart by the
                // target: `>&2` names a descriptor and duplicates, `>& file`
                // names a path and takes both streams there. Decided on the
                // token as written rather than after expansion, so the meaning
                // of a line never depends on a variable's contents.
                Some(if self.at_descriptor_word() {
                    RedirectKind::DuplicateOut
                } else {
                    RedirectKind::Both
                })
            } else if self.eat(&TokenKind::LessAmp).is_some() {
                Some(RedirectKind::DuplicateIn)
            } else if self.eat(&TokenKind::AmpGreater).is_some() {
                Some(RedirectKind::Both)
            } else {
                None
            };
            if let Some(kind) = kind {
                let operator_start = self.tokens[self.position - 1].span.start;
                // `2> file`: a bare run of digits *abutting* the operator names
                // the descriptor and is not an argument. Spacing decides —
                // `echo 2 > f` writes "2" to f, exactly as in bash.
                let fd = match items.last() {
                    Some(CommandItem::Word(word))
                        if word.span.end == operator_start
                            && matches!(word.value.pieces.as_slice(),
                                [WordPiece::Text { text, quote: QuoteMode::Bare }]
                                    if text.chars().all(|c| c.is_ascii_digit())) =>
                    {
                        let WordPiece::Text { text, .. } = &word.value.pieces[0] else {
                            unreachable!("matched a bare text piece")
                        };
                        let descriptor = text.parse::<u32>().map_err(|_| {
                            self.error(ParseErrorKind::Expected("a descriptor mesh can redirect"))
                        })?;
                        if descriptor > 2 {
                            return Err(self.error(ParseErrorKind::Expected(
                                "a descriptor of 0, 1, or 2; higher descriptors are not supported yet",
                            )));
                        }
                        items.pop();
                        Some(descriptor)
                    }
                    _ => None,
                };
                if fd.is_some() && kind == RedirectKind::Heredoc {
                    return Err(self.error(ParseErrorKind::Expected(
                        "a heredoc on a descriptor other than stdin",
                    )));
                }
                if fd.is_some() && kind == RedirectKind::Both {
                    return Err(self.error(ParseErrorKind::Expected(
                        "`&>` to name no descriptor; it always means stdout and stderr",
                    )));
                }
                if self.at_command_end() {
                    return Err(self.error(ParseErrorKind::Expected(
                        "a redirection needs a target file",
                    )));
                }
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
                items.push(CommandItem::Redirect {
                    kind,
                    fd,
                    target,
                    body,
                });
            } else {
                items.push(CommandItem::Word(self.command_word()?));
            }
        }
        if items.is_empty() {
            return Err(self.error(ParseErrorKind::Expected("an empty command in a pipeline")));
        }
        let guard = self.guard()?;
        Ok(Command { items, guard })
    }

    fn command_word(&mut self) -> Result<Spanned<Word>, ParseError> {
        let first = self.next().ok_or_else(|| {
            self.eof(ParseErrorKind::Expected(
                "a redirection needs a target file",
            ))
        })?;
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
            value: Word {
                pieces: merge_command_variable_access(pieces),
            },
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
        // `re(...)` is a built-in value constructor. A function of that name would
        // be reachable as a command (`re x`) but never as a value call, since
        // `re(x)` always builds a regex — so reserve the name rather than ship a
        // function that behaves differently depending on how it is called.
        if RESERVED_FUNCTION_NAMES.contains(&name.as_str()) {
            return Err(self.error(ParseErrorKind::ReservedFunctionName(name)));
        }
        let parameters = self.parameters()?;
        self.newlines();
        let body = self.block()?;
        Ok(Executable::Function {
            name,
            parameters,
            body,
        })
    }

    fn parameters(&mut self) -> Result<Vec<Param>, ParseError> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut parameters: Vec<Param> = Vec::new();
        let mut seen_optional = false;
        let mut seen_rest = false;
        self.newlines();
        while !self.same(&TokenKind::RParen) {
            let param = self.parameter()?;
            let names = parameters.iter().map(|p| p.name.as_str());
            self.check_param_order(
                &param.name,
                param.kind.order_class(),
                names,
                &mut seen_optional,
                &mut seen_rest,
            )?;
            parameters.push(param);
            let comma = self.eat(&TokenKind::Comma).is_some();
            self.newlines();
            if comma && self.same(&TokenKind::RParen) {
                return Err(self.error(ParseErrorKind::Expected("a name")));
            }
            if !comma && !self.same(&TokenKind::RParen) {
                if self.peek().is_none() {
                    return Err(self.eof(ParseErrorKind::Unterminated('(')));
                }
                return Err(self.error(ParseErrorKind::Expected("`,` or `)`")));
            }
        }
        self.position += 1;
        // An optional positional and a `...rest` cannot usefully coexist: the rest
        // would swallow anything meant for the optional (`DESIGN.md` §"Functions").
        if seen_optional && seen_rest {
            return Err(self.error(ParseErrorKind::OptionalWithRest));
        }
        Ok(parameters)
    }

    /// Parse a parameter's head — name and role, plus whether a `=` default marker
    /// was consumed — without parsing the default expression itself. `...name`
    /// (rest), `--name`[` =`] (switch or valued flag), or `name`[` =`] (required or
    /// optional positional).
    fn parameter_head(&mut self) -> Result<ParameterHead, ParseError> {
        if let Some(spread) = self.eat(&TokenKind::Spread) {
            // The documented `...name` grammar requires the name to abut `...`; a
            // space or newline between them is not a rest parameter. At EOF (no
            // next token yet) defer to `name()` so a truncated buffer stays
            // Incomplete rather than becoming a hard error.
            if let Some(next) = self.peek()
                && next.span.start != spread.span.end
            {
                return Err(self.error(ParseErrorKind::Expected("a name immediately after `...`")));
            }
            return Ok(ParameterHead {
                name: self.name()?,
                class: OrderClass::Rest,
                has_default: false,
            });
        }
        if let Some(name) = self.flag_name_at(0) {
            self.position += 1;
            let has_default = self.eat(&TokenKind::Equal).is_some();
            if has_default {
                self.newlines();
            }
            return Ok(ParameterHead {
                name,
                class: OrderClass::Independent,
                has_default,
            });
        }
        let name = self.name()?;
        let has_default = self.eat(&TokenKind::Equal).is_some();
        if has_default {
            self.newlines();
        }
        Ok(ParameterHead {
            name,
            class: if has_default {
                OrderClass::Optional
            } else {
                OrderClass::Required
            },
            has_default,
        })
    }

    /// Parse one full parameter, including any valued default expression.
    fn parameter(&mut self) -> Result<Param, ParseError> {
        let head = self.parameter_head()?;
        let kind = match head.class {
            OrderClass::Rest => ParamKind::Rest,
            OrderClass::Required => ParamKind::Required,
            OrderClass::Optional => ParamKind::Optional(self.expression()?),
            OrderClass::Independent if head.has_default => ParamKind::Flag(self.expression()?),
            OrderClass::Independent => ParamKind::Switch,
        };
        Ok(Param {
            name: head.name,
            kind,
        })
    }

    /// Enforce the signature's per-parameter rules — reserved (`env`, `sh`), duplicate,
    /// and ordering (nothing after `...rest`; no required after an optional) —
    /// updating the running `seen_optional`/`seen_rest` flags. Shared by the full
    /// [`Parser::parameters`] and the lenient [`Parser::parameters_prefix`] so both
    /// apply exactly the same grammar.
    fn check_param_order<'a>(
        &self,
        name: &str,
        class: OrderClass,
        existing: impl Iterator<Item = &'a str>,
        seen_optional: &mut bool,
        seen_rest: &mut bool,
    ) -> Result<(), ParseError> {
        if crate::vars::is_reserved_namespace(name) {
            return Err(self.error(ParseErrorKind::ReservedParameter(name.to_owned())));
        }
        if existing.into_iter().any(|existing| existing == name) {
            return Err(self.error(ParseErrorKind::DuplicateParameter(name.to_owned())));
        }
        if *seen_rest {
            return Err(self.error(ParseErrorKind::ParameterAfterRest(name.to_owned())));
        }
        match class {
            OrderClass::Rest => *seen_rest = true,
            OrderClass::Optional => *seen_optional = true,
            OrderClass::Required if *seen_optional => {
                return Err(self.error(ParseErrorKind::RequiredAfterOptional(name.to_owned())));
            }
            _ => {}
        }
        Ok(())
    }

    /// Classify a still-open parameter list (tokens with no closing `)`), using
    /// the real [`Parser::parameter`] and [`Parser::check_param_order`]. Running
    /// out of input mid-parameter or after a comma is [`PrefixStatus::Incomplete`]
    /// (keep reading); a hard grammar violation is [`PrefixStatus::Malformed`]
    /// (dispatch); a clean boundary is [`PrefixStatus::Complete`].
    fn parameters_prefix(&mut self) -> PrefixStatus {
        let mut names: Vec<String> = Vec::new();
        let mut seen_optional = false;
        let mut seen_rest = false;
        loop {
            self.newlines();
            if self.at_end() {
                break; // empty list, or a repairable trailing comma
            }
            // Parse the head (name + role) first. Ran out mid-head (a `...`/`--`
            // still awaiting its name) is a repairable prefix; anything else there
            // is a hard error.
            let head = match self.parameter_head() {
                Ok(head) => head,
                Err(error) if is_incomplete(&error.kind) => return PrefixStatus::Incomplete,
                Err(_) => return PrefixStatus::Malformed,
            };
            // Validate the name *before* the default: a reserved/duplicate/out-of-
            // order name can never be repaired by finishing the default, so it must
            // dispatch even when the default is unfinished (`func f(env =`⏎).
            if self
                .check_param_order(
                    &head.name,
                    head.class,
                    names.iter().map(String::as_str),
                    &mut seen_optional,
                    &mut seen_rest,
                )
                .is_err()
            {
                return PrefixStatus::Malformed;
            }
            names.push(head.name);
            if head.has_default {
                // A default not yet closed keeps buffering; a malformed one (`x = ]`)
                // dispatches, exactly as the parser judges it.
                match self.expression() {
                    Ok(_) => {}
                    Err(error) if is_incomplete(&error.kind) => return PrefixStatus::Incomplete,
                    Err(_) => return PrefixStatus::Malformed,
                }
            }
            self.newlines();
            if self.at_end() {
                break;
            }
            if self.eat(&TokenKind::Comma).is_none() {
                return PrefixStatus::Malformed; // e.g. `a b` — two names, no comma
            }
            if seen_rest {
                // A comma after the rest introduces a parameter that can never be
                // valid (`...xs, …` / the `...xs,)` close is rejected too).
                return PrefixStatus::Malformed;
            }
        }
        if seen_optional && seen_rest {
            return PrefixStatus::Malformed;
        }
        PrefixStatus::Complete
    }

    /// If the token at `offset` is a bare `--name` word with a valid flag name,
    /// return that name without the leading dashes.
    fn flag_name_at(&self, offset: usize) -> Option<String> {
        let token = self.tokens.get(self.position + offset)?;
        let TokenKind::Word(word) = &token.value else {
            return None;
        };
        if word_is_quoted(word) {
            return None;
        }
        let text = word.text();
        let name = text.strip_prefix("--")?;
        valid_name(name).then(|| name.to_owned())
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
        let bindings = self.for_bindings()?;
        if !self.take_word("in") {
            return Err(self.error(ParseErrorKind::Expected("`in`")));
        }
        let iterable = self.expression()?;
        self.newlines();
        let body = self.block()?;
        Ok(Executable::For {
            bindings,
            iterable,
            body,
        })
    }

    fn while_expr(&mut self) -> Result<Executable, ParseError> {
        self.take_word("while");
        let condition = Box::new(self.condition()?);
        self.newlines();
        let body = self.block()?;
        Ok(Executable::While { condition, body })
    }

    fn loop_expr(&mut self) -> Result<Executable, ParseError> {
        self.take_word("loop");
        self.newlines();
        let body = self.block()?;
        Ok(Executable::Loop { body })
    }

    fn match_expr(&mut self) -> Result<MatchExpr, ParseError> {
        self.take_word("match");
        let value = Box::new(self.expression()?);
        self.newlines();
        self.expect(&TokenKind::LBrace, "`{`")?;
        self.newlines();
        let mut arms = Vec::new();
        while !self.same(&TokenKind::RBrace) {
            let mut patterns = vec![self.match_pattern()?];
            while self.eat(&TokenKind::Pipe).is_some() {
                self.newlines();
                patterns.push(self.match_pattern()?);
            }
            let pattern = if patterns.len() == 1 {
                patterns.pop().unwrap()
            } else {
                MatchPattern::Alternation(patterns)
            };
            let guard = if self.take_word("if") {
                Some(self.expression()?)
            } else {
                None
            };
            self.newlines();
            let body = self.block()?;
            arms.push(MatchArm {
                pattern,
                guard,
                body,
            });
            self.newlines();
            if self.at_end() {
                return Err(self.eof(ParseErrorKind::Unterminated('{')));
            }
        }
        self.position += 1;
        Ok(MatchExpr { value, arms })
    }

    fn match_pattern(&mut self) -> Result<MatchPattern, ParseError> {
        if self.same(&TokenKind::LBracket) {
            Ok(MatchPattern::Binding(self.binding_pattern()?))
        } else if self.word("_") {
            self.position += 1;
            Ok(MatchPattern::Wildcard)
        } else if self.operator("*") {
            self.position += 1;
            Ok(MatchPattern::Value(Expr::Glob("*".into())))
        } else {
            Ok(MatchPattern::Value(match_pattern_operand(
                self.expression()?,
            )))
        }
    }

    fn for_bindings(&mut self) -> Result<Vec<BindingPattern>, ParseError> {
        let first = self.binding_pattern()?;
        let mut bindings = vec![first];
        if self.eat(&TokenKind::Comma).is_some() {
            bindings.push(self.binding_pattern()?);
        }
        Ok(bindings)
    }

    /// Is the next token a bare run of digits — the shape a descriptor takes?
    fn at_descriptor_word(&self) -> bool {
        self.peek().is_some_and(|token| match &token.value {
            TokenKind::Word(word) => matches!(
                word.pieces.as_slice(),
                [WordPiece::Text { text, quote: QuoteMode::Bare }]
                    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
            ),
            _ => false,
        })
    }

    /// A bare `$env.KEY` in assignment position, consumed on a match.
    ///
    /// Only a plain member is a place you can assign: `$env` alone names the
    /// whole namespace, and an index, slice, or modifier (`$env.PATH:dedup`)
    /// describes a derived value rather than a variable, so none of them is an
    /// assignment target. Those fall through and parse as ordinary expressions,
    /// which is where their real error message comes from.
    ///
    /// The key is checked with the same rule reads use, so anything spellable as
    /// `$env.KEY` is also assignable — including a kebab name like
    /// `$env.MY-VAR`, which the environment permits and mesh can read.
    fn env_target(&mut self) -> Option<String> {
        let TokenKind::Word(word) = &self.peek()?.value else {
            return None;
        };
        let [
            WordPiece::Variable {
                name,
                quote: QuoteMode::Bare,
            },
        ] = word.pieces.as_slice()
        else {
            return None;
        };
        let key = name.strip_prefix("$env.").or_else(|| {
            name.strip_prefix("${env.")
                .and_then(|k| k.strip_suffix('}'))
        })?;
        if !crate::lexer::is_name(key) {
            return None;
        }
        let key = key.to_owned();
        self.next();
        Some(key)
    }

    fn binding_pattern(&mut self) -> Result<BindingPattern, ParseError> {
        if self.eat(&TokenKind::LBracket).is_some() {
            self.newlines();
            let mut elements = Vec::new();
            let mut rest_seen = false;
            while !self.same(&TokenKind::RBracket) {
                let element = if self.eat(&TokenKind::Spread).is_some() {
                    if rest_seen {
                        return Err(
                            self.error(ParseErrorKind::Expected("only one `...rest` binding"))
                        );
                    }
                    rest_seen = true;
                    BindingPattern::Rest(self.name()?)
                } else if self.take_word("_") {
                    BindingPattern::Ignore
                } else {
                    BindingPattern::Name(self.name()?)
                };
                elements.push(element);
                self.eat(&TokenKind::Comma);
                self.newlines();
                if self.at_end() {
                    return Err(self.eof(ParseErrorKind::Unterminated('[')));
                }
            }
            self.position += 1;
            return Ok(BindingPattern::List(elements));
        }
        if self.take_word("_") {
            Ok(BindingPattern::Ignore)
        } else {
            Ok(BindingPattern::Name(self.name()?))
        }
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
        self.or_expression()
    }

    fn or_expression(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.and_expression()?;
        while self.take_word("or") {
            self.newlines();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinaryOp::Or,
                right: Box::new(self.and_expression()?),
            };
        }
        Ok(left)
    }

    fn and_expression(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.not_expression()?;
        while self.take_word("and") {
            self.newlines();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinaryOp::And,
                right: Box::new(self.not_expression()?),
            };
        }
        Ok(left)
    }

    fn not_expression(&mut self) -> Result<Expr, ParseError> {
        if self.take_word("not") {
            return Ok(Expr::Unary {
                op: UnaryOp::Not,
                expression: Box::new(self.not_expression()?),
            });
        }
        self.binary(4)
    }

    fn condition(&mut self) -> Result<Executable, ParseError> {
        let start = self.position;
        if self.word_text_at(0).is_some() || self.same(&TokenKind::LBracket) {
            if let Ok(pattern) = self.binding_pattern()
                && self.eat(&TokenKind::Equal).is_some()
            {
                return Ok(Executable::Assignment {
                    pattern,
                    append: false,
                    value: self.expression()?,
                });
            }
            self.position = start;
        }
        if self.condition_value_start() {
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
            let mut right = self.binary(precedence + 1)?;
            if matches!(op, BinaryOp::Match | BinaryOp::NotMatch) {
                right = match_operand(right);
            }
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
            } else if self.same(&TokenKind::LBracket)
                && self
                    .peek()
                    .is_some_and(|token| token.span.start == self.previous_end())
            {
                self.position += 1;
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
        if self.eat(&TokenKind::CaptureStart).is_some() {
            self.newlines();
            return Ok(Expr::Capture(self.source(Some(TokenKind::RParen))?));
        }
        if self.word("if") {
            return Ok(Expr::If(Box::new(self.if_expr()?)));
        }
        if self.word("match") {
            return Ok(Expr::Match(Box::new(self.match_expr()?)));
        }
        if self.word("for") {
            self.take_word("for");
            let bindings = self.for_bindings()?;
            if !self.take_word("in") {
                return Err(self.error(ParseErrorKind::Expected("`in`")));
            }
            let iterable = self.expression()?;
            self.newlines();
            let body = self.block()?;
            return Ok(Expr::For {
                bindings,
                iterable: Box::new(iterable),
                body,
            });
        }
        if self.word("func")
            && self
                .tokens
                .get(self.position + 1)
                .is_some_and(|token| matches!(token.value, TokenKind::LParen))
        {
            self.take_word("func");
            let parameters = self.parameters()?;
            self.newlines();
            let body = self.block()?;
            return Ok(Expr::Lambda { parameters, body });
        }
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
                    let start = token.span.start;
                    let mut end = token.span.end;
                    let mut pieces = word.pieces;
                    let mut brackets = 0usize;
                    while self.peek().is_some_and(|next| next.span.start == end) {
                        match self.peek().map(|next| &next.value) {
                            Some(TokenKind::LBracket) => brackets += 1,
                            Some(TokenKind::RBracket) if brackets > 0 => brackets -= 1,
                            Some(TokenKind::RBracket)
                                if self.tokens.get(self.position + 1).is_some_and(|next| {
                                    next.span.start == self.peek().unwrap().span.end
                                        && matches!(next.value, TokenKind::Word(_))
                                }) => {}
                            Some(
                                TokenKind::RBracket
                                | TokenKind::RParen
                                | TokenKind::RBrace
                                | TokenKind::Comma
                                | TokenKind::Colon
                                | TokenKind::Semi
                                | TokenKind::Amp
                                | TokenKind::AndAnd
                                | TokenKind::OrOr
                                | TokenKind::Pipe
                                | TokenKind::PipeBoth
                                | TokenKind::Less
                                | TokenKind::Greater
                                | TokenKind::Append
                                | TokenKind::Heredoc
                                | TokenKind::Range
                                | TokenKind::RangeInclusive
                                | TokenKind::Operator(_),
                            ) => break,
                            _ => {}
                        }
                        let Some(next_pieces) =
                            self.peek().and_then(|next| token_word_pieces(&next.value))
                        else {
                            break;
                        };
                        end = self.peek().unwrap().span.end;
                        self.position += 1;
                        pieces.extend(next_pieces);
                    }
                    Ok(Expr::Scalar(Spanned {
                        value: Word { pieces },
                        span: start..end,
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
                        | TokenKind::RParen
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
    fn value_start(&mut self) -> bool {
        self.value_start_in(false)
    }

    /// [`value_start`] as a **condition** reads it, where a *spaced* `<` / `>`
    /// after a value-like operand is a comparison rather than a redirection.
    ///
    /// `if $i < 3` otherwise parses as the command `$i` with its stdin redirected
    /// from a file named `3`, which is why `<=`, `>=`, and `!=` worked in a
    /// condition while `<` and `>` did not. Statement position keeps the redirect
    /// reading, so `$editor > log` still runs the command named by `$editor` and
    /// redirects it.
    fn condition_value_start(&mut self) -> bool {
        self.value_start_in(true)
    }

    /// Is the next token a `<` or `>` with whitespace on both sides — the shape
    /// a comparison takes and an attached redirect (`cmd >out`) does not?
    fn spaced_comparison(&self) -> bool {
        let Some(left) = self.peek() else {
            return false;
        };
        self.tokens
            .get(self.position + 1)
            .zip(self.tokens.get(self.position + 2))
            .is_some_and(|(operator, right)| {
                matches!(operator.value, TokenKind::Less | TokenKind::Greater)
                    && left.span.end < operator.span.start
                    && operator.span.end < right.span.start
            })
    }

    fn value_start_in(&mut self, in_condition: bool) -> bool {
        match self.peek().map(|token| &token.value) {
            Some(
                TokenKind::CaptureStart
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::Range
                | TokenKind::RangeInclusive,
            ) => true,
            Some(TokenKind::Word(word)) => {
                let variable = matches!(
                    word.pieces.as_slice(),
                    [WordPiece::Variable {
                        quote: QuoteMode::Bare,
                        ..
                    }]
                );
                let quoted = word_is_quoted(word);
                let attached_call = self.tokens.get(self.position + 1).is_some_and(|next| {
                    matches!(next.value, TokenKind::LParen)
                        && next.span.start == self.peek().unwrap().span.end
                });
                let followed_by_operator = self
                    .tokens
                    .get(self.position + 1)
                    .zip(self.tokens.get(self.position + 2))
                    .is_some_and(|(operator, right)| {
                        value_operator(&operator.value) && operator.span.end < right.span.start
                    });
                let numeric = word.text().parse::<i64>().is_ok();
                // `>>` is only ever a redirect, so it stays excluded even in a
                // condition; `<` and `>` are the two that also spell comparisons.
                let redirect_next = matches!(
                    self.tokens.get(self.position + 1).map(|token| &token.value),
                    Some(TokenKind::Less | TokenKind::Greater | TokenKind::Append)
                );
                let comparison_next = in_condition && self.spaced_comparison();
                (variable && (!redirect_next || comparison_next))
                    || (quoted && (!redirect_next || comparison_next) && self.viable_expression())
                    || attached_call
                    || (numeric && followed_by_operator)
                    // A lone numeric literal is a **value**, so a block can yield one:
                    // `func answer() { 42 }` is 42 rather than "command not found: 42".
                    // Narrow on purpose — the whole statement must be that literal — so
                    // `42 foo` and `42 > file` stay the commands they were, and a
                    // numeral is never a plausible command name anyway (`./42` still
                    // runs a file called that).
                    || (numeric
                        && (!redirect_next || comparison_next)
                        && self.lone_integer_literal())
            }
            _ => false,
        }
    }
    /// Is the rest of this statement exactly one integer literal?
    ///
    /// The token being peeked is not enough to tell. A word can be assembled from
    /// several adjacent tokens, and only the *first* is in hand here: `3.5` peeks
    /// as `3`, which parses as an integer while the word does not. So the check
    /// parses the statement and looks at what came out, rather than trusting the
    /// lookahead — and asks [`Word::bare_integer`], since even the assembled text is
    /// not enough: `4"2"` spells `42` while expanding to the *string*.
    fn lone_integer_literal(&mut self) -> bool {
        let saved = self.position;
        let lone = matches!(
            self.expression(),
            Ok(Expr::Scalar(word)) if word.value.bare_integer().is_some()
        ) && self.at_command_end()
            // `at_command_end` counts a pipe, but an expression cannot *be* a
            // pipeline stage: classifying `42 | cat` as one would leave the `|`
            // unconsumed and turn a command that runs today into a syntax error.
            // A numeral heading a pipeline stays the command it was.
            && !matches!(
                self.peek().map(|token| &token.value),
                Some(TokenKind::Pipe | TokenKind::PipeBoth)
            );
        self.position = saved;
        lone
    }

    fn viable_expression(&mut self) -> bool {
        let saved = self.position;
        let viable = self.expression().is_ok() && self.at_command_end();
        self.position = saved;
        viable
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

fn value_operator(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Operator(_) | TokenKind::Range | TokenKind::RangeInclusive
    )
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_alphabetic() {
        return false;
    }
    let mut previous_hyphen = false;
    for c in chars {
        if c == '-' {
            if previous_hyphen {
                return false;
            }
            previous_hyphen = true;
        } else if c == '_' || c.is_alphanumeric() {
            previous_hyphen = false;
        } else {
            return false;
        }
    }
    !previous_hyphen
}

fn modifier_name(name: &str) -> bool {
    MODIFIER_NAMES.contains(&name)
}

const MODIFIER_NAMES: &[&str] = &[
    "add",
    "ancestors",
    "atime",
    "bare",
    "base",
    "capture",
    "captures",
    "ctime",
    "d",
    "dedup",
    "dir",
    "dirs",
    "dotall",
    "each",
    "epoch",
    "exec",
    "exists",
    "ext",
    "extended",
    "exts",
    "f",
    "files",
    "filter",
    "first",
    "format",
    "get",
    "groups",
    "h",
    "has",
    "i",
    "ignorecase",
    "init",
    "int",
    "iso",
    "join",
    "keys",
    "l",
    "last",
    "len",
    "lines",
    "links",
    "lower",
    "m",
    "map",
    "match",
    "matches",
    "mod",
    "ms",
    "mtime",
    "multiline",
    "nulls",
    "num",
    "old",
    "parents",
    "quotemeta",
    "raw",
    "real",
    "remove",
    "replace",
    "replaceall",
    "replaceend",
    "replacestart",
    "rest",
    "s",
    "same",
    "secs",
    "sort",
    "split",
    "stem",
    "stripend",
    "stripstart",
    "tabs",
    "trimend",
    "trimstart",
    "tty",
    "type",
    "upper",
    "values",
    "words",
    "x",
];

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
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["x", "y"]);
        assert!(parameters.iter().all(|p| p.kind == ParamKind::Required));
        assert_eq!(body.statements.len(), 1);
        for invalid in [
            "func f(x,) {}",
            "func f(x y) {}",
            "func f(x, x) {}",
            "func f(env) {}",
            // A required positional cannot follow an optional one.
            "func f(a = 1, b) {}",
            // Nothing may follow a rest parameter, and it cannot pair with an optional.
            "func f(...xs, a) {}",
            "func f(a = 1, ...xs) {}",
            // The rest name must abut `...` (the documented `...name` grammar): a
            // space or newline between them is not a rest parameter.
            "func f(... xs) {}",
            "func f(...\nxs) {}",
        ] {
            assert!(parse(invalid).is_err(), "accepted {invalid:?}");
        }
        // The adjacent forms remain valid.
        assert!(matches!(
            parse("func f(...xs) {}"),
            Ok(ParseOutcome::Complete(_))
        ));
    }

    #[test]
    fn parses_flag_optional_and_rest_parameters() {
        let tree = complete(
            "func deploy(env0, --region = us-west, --force, --tag = latest, ...hosts) {\n  puts hi\n}",
        );
        let Executable::Function { parameters, .. } = &tree.statements[0].and_or.first else {
            panic!()
        };
        assert_eq!(parameters.len(), 5);
        assert_eq!(parameters[0].name, "env0");
        assert_eq!(parameters[0].kind, ParamKind::Required);
        assert_eq!(parameters[1].name, "region");
        assert!(matches!(parameters[1].kind, ParamKind::Flag(_)));
        assert_eq!(parameters[2].name, "force");
        assert_eq!(parameters[2].kind, ParamKind::Switch);
        assert_eq!(parameters[3].name, "tag");
        assert!(matches!(parameters[3].kind, ParamKind::Flag(_)));
        assert_eq!(parameters[4].name, "hosts");
        assert_eq!(parameters[4].kind, ParamKind::Rest);
    }

    #[test]
    fn reports_incomplete_delimiters_and_connectors() {
        assert_eq!(parse("x = (1").unwrap(), ParseOutcome::Incomplete);
        assert_eq!(parse("a &&").unwrap(), ParseOutcome::Incomplete);
        assert_eq!(
            parse("func f(x {\nputs )").unwrap(),
            ParseOutcome::Incomplete
        );
        assert!(parse("func f(x {\nputs )\n}").is_err());
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
    fn dispatches_literal_led_expressions_as_values() {
        let tree = complete("if 1 == 1 { puts yes }\n'final value'");
        let Executable::If(condition) = &tree.statements[0].and_or.first else {
            panic!()
        };
        assert!(matches!(
            condition.condition.as_ref(),
            Executable::Expression {
                expression: Expr::Binary {
                    op: BinaryOp::Equal,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            tree.statements[1].and_or.first,
            Executable::Expression {
                expression: Expr::Scalar(_),
                ..
            }
        ));
    }

    #[test]
    fn keeps_quoted_executables_in_command_position() {
        for source in [r#""my tool" arg"#, r#"'echo' > out"#] {
            let tree = complete(source);
            assert!(matches!(
                tree.statements[0].and_or.first,
                Executable::Pipeline(_)
            ));
        }
    }

    #[test]
    fn tokenizes_attached_negation_as_an_operator() {
        for source in ["x = -$n", "x=-$n", "x = f(value:-$n)"] {
            let tree = complete(source);
            let Executable::Assignment { value, .. } = &tree.statements[0].and_or.first else {
                panic!()
            };
            let negated = match value {
                Expr::Unary {
                    op: UnaryOp::Negate,
                    ..
                } => true,
                Expr::Call { arguments, .. } => matches!(
                    arguments.as_slice(),
                    [Argument::Named(
                        _,
                        Expr::Unary {
                            op: UnaryOp::Negate,
                            ..
                        }
                    )]
                ),
                _ => false,
            };
            assert!(negated, "negation was not parsed in {source}");
        }
    }

    #[test]
    fn raw_prefix_requires_a_valid_word_position() {
        let tokens = tokenize("car'pet' x=r'raw'").unwrap();
        let TokenKind::Word(first) = &tokens[0].value else {
            panic!()
        };
        assert_eq!(first.text(), "carpet");
        assert!(matches!(
            first.pieces.as_slice(),
            [
                WordPiece::Text { text, quote: QuoteMode::Bare },
                WordPiece::Text { quote: QuoteMode::Single, .. }
            ] if text == "car"
        ));
        assert!(matches!(
            &tokens[3].value,
            TokenKind::Word(Word { pieces })
                if matches!(pieces.as_slice(), [WordPiece::Text { quote: QuoteMode::Raw, .. }])
        ));
    }

    #[test]
    fn invalid_unbraced_variable_heads_remain_literal() {
        for source in ["$5", "$_name"] {
            let tokens = tokenize(source).unwrap();
            let TokenKind::Word(word) = &tokens[0].value else {
                panic!()
            };
            assert!(matches!(
                word.pieces.as_slice(),
                [WordPiece::Text { text, quote: QuoteMode::Bare }] if text == source
            ));
        }
    }

    #[test]
    fn validates_braced_variable_access() {
        assert!(tokenize("${user.name}").is_ok());
        assert!(tokenize("${items[0]}").is_ok());
        assert!(tokenize("${items[1..]}").is_ok());
        assert!(tokenize("${items[-1]:stem}").is_ok());
        assert!(matches!(
            tokenize("${bad name}"),
            Err(ParseError {
                kind: ParseErrorKind::Expected(_),
                ..
            })
        ));
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

    #[test]
    fn decodes_quoted_escapes_and_rejects_unknown_ones() {
        let tokens = tokenize(r#""a\nb\u{21}""#).unwrap();
        let TokenKind::Word(word) = &tokens[0].value else {
            panic!()
        };
        assert_eq!(word.text(), "a\nb!");
        assert!(matches!(
            tokenize(r"'\d'"),
            Err(ParseError {
                kind: ParseErrorKind::UnknownEscape('d'),
                ..
            })
        ));

        let bare = tokenize(r"a\nb").unwrap();
        let TokenKind::Word(word) = &bare[0].value else {
            panic!()
        };
        assert_eq!(word.text(), "anb");
    }

    #[test]
    fn accepts_kebab_case_names_and_variables() {
        let tree = complete("last-cmd-time = $last-cmd-time\nfunc auto-fetch() { return }");
        let Executable::Assignment { pattern, value, .. } = &tree.statements[0].and_or.first else {
            panic!()
        };
        assert_eq!(pattern, &BindingPattern::Name("last-cmd-time".into()));
        assert!(matches!(value, Expr::Variable(variable) if variable.value == "$last-cmd-time"));
        assert!(matches!(
            &tree.statements[1].and_or.first,
            Executable::Function { name, .. } if name == "auto-fetch"
        ));
    }

    #[test]
    fn parses_list_binding_patterns() {
        let tree = complete("[first ...middle last] = [a b c d]");
        assert!(matches!(
            &tree.statements[0].and_or.first,
            Executable::Assignment {
                pattern: BindingPattern::List(patterns),
                ..
            } if patterns.len() == 3
        ));
    }

    #[test]
    fn parses_command_substitution_as_a_capture() {
        let tree = complete("x = $(echo hi):lines");
        let Executable::Assignment { value, .. } = &tree.statements[0].and_or.first else {
            panic!()
        };
        let Expr::Modifier { value, name, .. } = value else {
            panic!()
        };
        assert_eq!(name, "lines");
        assert!(matches!(value.as_ref(), Expr::Capture(source) if source.statements.len() == 1));
    }

    #[test]
    fn parses_compound_expressions_in_value_position() {
        let tree = complete(
            "greeting = if $french { bonjour } else { hi }\nmapper = func(x) { $x }\nitems = for x in $xs { $x }",
        );
        assert!(matches!(
            &tree.statements[0].and_or.first,
            Executable::Assignment {
                value: Expr::If(_),
                ..
            }
        ));
        assert!(matches!(
            &tree.statements[1].and_or.first,
            Executable::Assignment {
                value: Expr::Lambda { .. },
                ..
            }
        ));
        assert!(matches!(
            &tree.statements[2].and_or.first,
            Executable::Assignment {
                value: Expr::For { .. },
                ..
            }
        ));
    }

    #[test]
    fn not_wraps_the_complete_comparison() {
        let tree = complete("x = not $a == b");
        let Executable::Assignment {
            value:
                Expr::Unary {
                    op: UnaryOp::Not,
                    expression,
                },
            ..
        } = &tree.statements[0].and_or.first
        else {
            panic!()
        };
        assert!(matches!(
            expression.as_ref(),
            Expr::Binary {
                op: BinaryOp::Equal,
                ..
            }
        ));
    }

    #[test]
    fn recognizes_the_documented_modifier_vocabulary() {
        for modifier in MODIFIER_NAMES {
            let source = format!("x = $value:{modifier}");
            let tree = complete(&source);
            assert!(matches!(
                &tree.statements[0].and_or.first,
                Executable::Assignment { value: Expr::Modifier { name, .. }, .. } if name == modifier
            ));
        }
    }
}
