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
    Text {
        text: String,
        quote: QuoteMode,
    },
    Variable {
        name: String,
        quote: QuoteMode,
    },
    /// A **value** spliced into the word — `"at $(pwd) now"`, `"${greeting()}"`.
    ///
    /// Evaluating it needs the shell (a `$(…)` launches a command, a call runs
    /// mesh), so it happens where the shell is — [`crate::repl::expansion_word`] —
    /// and rides into expansion as a literal piece, exactly as an interpolated
    /// variable does: never re-split, never re-globbed.
    ///
    /// `quote` is what the piece was written inside, and it decides whether the
    /// value stays a value. Inside `"…"` the quotes say "make this text", so the
    /// result is rendered by the same rule an interpolated `$x` obeys — a list or
    /// a map is the same loud error there as it is for `"$xs"`, rather than a
    /// collection quietly surviving a pair of quotes.
    Value {
        expression: Box<Spanned<Expr>>,
        quote: QuoteMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Word {
    pub pieces: Vec<WordPiece>,
    /// The `(…)` that followed a glob, when this word carried one — `*(d)`. Only
    /// a word whose bare text has glob syntax can take them, so an ordinary word
    /// followed by `(` is still a call.
    pub qualifiers: Option<GlobQualifiers>,
}

/// The options after a glob, narrowing which of its matches survive
/// (`DESIGN.md` §"Globbing"). ANDed: a path has to satisfy every one.
///
/// Syntax only — deciding whether a path qualifies means reading the filesystem,
/// which belongs to expansion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlobQualifiers {
    /// The types accepted. Empty accepts any; more than one is the `file|dir`
    /// alternation. One list because the type dimension is mutually exclusive —
    /// a path has exactly one type, so listing more can only widen.
    pub types: Vec<FileKind>,
    /// `exec: true`, or the `x` shorthand.
    pub exec: Option<bool>,
    /// `empty: true` — a zero-length file or a directory with no entries.
    pub empty: Option<bool>,
}

/// A file's type, spelled as `find -type` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Fifo,
    Socket,
    Block,
    Char,
}

impl FileKind {
    /// The `type:` name and the `find -type` letter that both spell this kind.
    fn spellings(self) -> (&'static str, &'static str) {
        match self {
            FileKind::File => ("file", "f"),
            FileKind::Dir => ("dir", "d"),
            FileKind::Symlink => ("symlink", "l"),
            FileKind::Fifo => ("fifo", "p"),
            FileKind::Socket => ("socket", "s"),
            FileKind::Block => ("block", "b"),
            FileKind::Char => ("char", "c"),
        }
    }

    const ALL: [FileKind; 7] = [
        FileKind::File,
        FileKind::Dir,
        FileKind::Symlink,
        FileKind::Fifo,
        FileKind::Socket,
        FileKind::Block,
        FileKind::Char,
    ];

    /// The kind a `type:` name spells, e.g. `dir`.
    fn from_name(name: &str) -> Option<FileKind> {
        FileKind::ALL
            .into_iter()
            .find(|kind| kind.spellings().0 == name)
    }

    /// The kind a bare shorthand letter spells, e.g. `d`.
    fn from_letter(letter: &str) -> Option<FileKind> {
        FileKind::ALL
            .into_iter()
            .find(|kind| kind.spellings().1 == letter)
    }
}

impl Word {
    /// The word's literal spelling — what a caller that needs a *name* reads.
    ///
    /// A value piece contributes nothing: it has no spelling until it is evaluated.
    /// Every caller pairs this with [`word_is_quoted`], which reports a value piece
    /// as "not a bare literal", so none of them mistakes the remaining text for the
    /// whole word.
    pub fn text(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| match piece {
                WordPiece::Text { text, .. } => text.as_str(),
                WordPiece::Variable { name, .. } => name.as_str(),
                WordPiece::Value { .. } => "",
            })
            .collect()
    }

    /// The `i64` this word spells, when it is a single **bare** run of text and
    /// that text is the integer's own spelling — see [`canonical_integer`].
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
            ] => canonical_integer(text),
            _ => None,
        }
    }

    /// The `bool` this word spells, under exactly [`bare_integer`]'s rule: a single
    /// bare run of text, so `"true"` and `tr\ue` stay strings and reach a program of
    /// that name.
    fn bare_boolean(&self) -> Option<bool> {
        match self.pieces.as_slice() {
            [
                WordPiece::Text {
                    text,
                    quote: QuoteMode::Bare,
                },
            ] => match text.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// The text of a word that is a single **unquoted** piece, as `bare_integer`
    /// requires for the same reason: `:'stem'` and `:\stem` compose to the same
    /// text as `:stem`, and a quoted word must not keep the operator meaning the
    /// bare one has. Modifier names elsewhere go through the same rule.
    fn bare_word(&self) -> Option<&str> {
        match self.pieces.as_slice() {
            [
                WordPiece::Text {
                    text,
                    quote: QuoteMode::Bare,
                },
            ] => Some(text),
            _ => None,
        }
    }

    /// Is this word exactly `expected`, spelled bare? Asked where a *spelling*
    /// settles something before expansion could — a keyword, or which builtin a
    /// stage is about to be.
    pub(crate) fn is_bare_text(&self, expected: &str) -> bool {
        matches!(self.pieces.as_slice(), [WordPiece::Text { text, quote: QuoteMode::Bare }] if text == expected)
    }
}

/// The text of a word that is one literal run and nothing else — `ls`, `'ls -l'`,
/// `"ls -l"` — with how it was written. `None` for anything with a variable, a
/// capture, or more than one piece, since those are not known until run time.
fn single_text(word: &Word) -> Option<(&str, QuoteMode)> {
    match word.pieces.as_slice() {
        [WordPiece::Text { text, quote }] => Some((text, *quote)),
        _ => None,
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
    /// `<<<` — a here-string: the following word *is* the input text.
    HereString,
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
    /// `=>` — separates a `match` arm's pattern from its body.
    FatArrow,
    Operator(String),
}

pub type Token = Spanned<TokenKind>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    UnexpectedToken,
    UnexpectedEnd,
    Unterminated(char),
    /// A heredoc body ran to end of input without its closing delimiter line.
    /// Carries the delimiter so a line-at-a-time reader can wait for exactly that
    /// line instead of re-parsing the body after every line it reads.
    UnterminatedHeredoc(String),
    ChainedComparison,
    /// A range whose start or end is itself a range — `1 .. 2 .. 3`, and the
    /// spellings that reach the same shape through an operand (`1 .. ..3`).
    /// Refused for the reason a chained comparison is: the second operator has no
    /// reading that means anything, so accepting it only defers the complaint to
    /// evaluation, where it arrives as "endpoints must be integers" and names
    /// neither the operator nor the line's real problem.
    ChainedRange,
    Expected(&'static str),
    ReservedParameter(String),
    /// `_ = value` — the discard used as a place. It is a *pattern element*, so it
    /// has a position to discard only inside one.
    DiscardAssignment,
    /// A quoted command after `alias NAME =` — `alias ll = 'ls -l'`. bash needs
    /// the quotes because its alias body is a *string*; mesh's is real syntax,
    /// so they turn the command into one word naming no program.
    QuotedAliasCommand(String),
    /// A `wrapper func` that declares a `--flag`. The marker's whole content is
    /// "parses no flags of its own", so the two cannot both be true: the flag
    /// would be listed in help and offered by completion while every
    /// command-position `--flag` went straight to `...rest` instead.
    WrapperDeclaresFlag(String),
    /// A `/…/` literal the tokenizer had already taken apart — the only way that
    /// happens is a construct the lexer consumes *without emitting a token*, so
    /// in practice a ` #` comment inside the pattern. Its own variant because the
    /// alternative is the silence this literal exists to remove: the scan
    /// declines, the leading `/a` reads as a glob, and the test quietly answers
    /// false. Carries nothing — the span points at the literal, which is the
    /// whole answer.
    RegexLiteralInterrupted,
    /// A value argument with text glued to it — `pre$(x)post`, `f()x`. Its own
    /// variant because the fix a reader needs is a *spelling*, not a rethink: the
    /// message names both, and it keeps this the loud error it was before value
    /// arguments existed rather than three arguments where one was written.
    GluedValueArgument,
    /// An attached `:name` naming no modifier — `ubuntu:latest`, `host:port`. Its
    /// own variant because `:` + identifier is reserved by the grammar rather than
    /// gated on a name list, so this is the diagnostic that replaces the old silent
    /// fallback to literal text. Carries the name so the message can quote it, and
    /// says *unknown* rather than *unimplemented*: a name the vocabulary reserves but
    /// the engine cannot apply yet (`:sort`) parses fine and reports at run time.
    UnknownModifier(String),
    /// A modifier given an **argument list** inside a `$…` interpolation —
    /// `"$env:get(HOME, none)"`. Its own variant because the reader wrote
    /// something that has a working spelling rather than something mesh cannot
    /// do: a `$…` reference is scanned by its characters and stops at the `(`,
    /// so the arguments became literal text and the modifier ran with none. The
    /// braced form `${env:get(HOME, none)}` takes them, sigil-less like the
    /// argument-free `${file:stem}`.
    InterpolatedModifierArguments(String),
    /// Input nested past [`MAX_DEPTH`]. Its own variant because the failure is a
    /// *resource* limit rather than a shape the grammar rejects: the source may be
    /// perfectly well formed, and the honest report is that mesh will not go that
    /// deep, not that the reader wrote something wrong. Without it the recursive
    /// descent runs out of stack and the process aborts, which turns malformed
    /// input into a dead shell.
    TooDeep,
    /// A `(…)` after a glob naming something that is not a qualifier — `*(q)`,
    /// `*(kind: file)`. Carries the spelling so the message can quote it back;
    /// its own variant because the reader wrote a *glob* option and the fix is
    /// another option name, not a different kind of expression.
    UnknownGlobQualifier(String),
    /// A glob qualifier given a value its name does not take — `*(type: blue)`,
    /// `*(exec: maybe)`. Carries the name and what was written.
    BadGlobQualifier(String, String),
    /// Two qualifiers for one dimension — `*(f, d)`, `*(exec: true, exec: false)`.
    /// Carries the dimension. Refused rather than merged: the comma is an **and**,
    /// so a second answer to the same question either contradicts the first or
    /// silently overwrites it, and neither is what the reader wrote.
    DuplicateGlobQualifier(&'static str),
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
            ParseErrorKind::UnterminatedHeredoc(delimiter) => {
                write!(
                    f,
                    "syntax error: heredoc missing its `{delimiter}` delimiter"
                )
            }
            ParseErrorKind::ChainedComparison => {
                write!(f, "syntax error: comparisons cannot be chained")
            }
            ParseErrorKind::ChainedRange => {
                write!(f, "syntax error: ranges cannot be chained")
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
            ParseErrorKind::TooDeep => write!(
                f,
                "syntax error: nested too deeply; mesh parses at most {MAX_DEPTH} levels"
            ),
            ParseErrorKind::UnknownModifier(name) => write!(
                f,
                "syntax error: `:{name}` is not a modifier; quote the whole word to \
                 keep it as text (`\"x:{name}\"`), or brace the name when it comes \
                 from a variable (`\"${{x}}:{name}\"`)"
            ),
            ParseErrorKind::InterpolatedModifierArguments(name) => write!(
                f,
                "syntax error: `:{name}` takes arguments, which a `$…` interpolation \
                 cannot pass; brace it instead (`\"${{x:{name}(…)}}\"`)"
            ),
            ParseErrorKind::UnknownGlobQualifier(text) => write!(
                f,
                "syntax error: `{text}` is not a glob qualifier; the types are \
                 `f`/`file`, `d`/`dir`, `l`/`symlink`, `p`/`fifo`, `s`/`socket`, \
                 `b`/`block`, `c`/`char`, and the tests are `x`/`exec:` and `empty:`"
            ),
            ParseErrorKind::BadGlobQualifier(name, value) => write!(
                f,
                "syntax error: `{value}` is not a value for the glob qualifier `{name}`"
            ),
            ParseErrorKind::DuplicateGlobQualifier(dimension) if *dimension == "type" => write!(
                f,
                "syntax error: a glob takes one type; write `type: file|dir` for either"
            ),
            ParseErrorKind::DuplicateGlobQualifier(dimension) => write!(
                f,
                "syntax error: the glob qualifier `{dimension}` is given twice"
            ),
            ParseErrorKind::RegexLiteralInterrupted => write!(
                f,
                "syntax error: a `/…/` literal cannot contain a comment; attach the \
                 `#` to the pattern, or build it with `re(r\"…\")`"
            ),
            ParseErrorKind::GluedValueArgument => write!(
                f,
                "syntax error: a value argument cannot have text attached; separate it \
                 with a space, or quote the whole word — `\"pre$(…)post\"`"
            ),
            ParseErrorKind::DiscardAssignment => write!(
                f,
                "syntax error: `_` discards a position and binds nothing, so there is \
                 nothing to assign to; name it, or drop the `_ =` to run the value for \
                 its effect"
            ),
            ParseErrorKind::QuotedAliasCommand(text) => write!(
                f,
                "syntax error: an alias takes a command, not a string; write it \
                 unquoted -- `alias NAME = {text}`"
            ),
            ParseErrorKind::WrapperDeclaresFlag(name) => write!(
                f,
                "syntax error: a `wrapper func` parses no flags, so it cannot declare \
                 `--{name}`; drop the marker, or take the flag through `...rest`"
            ),
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
            ParseErrorKind::UnknownEscape(c) => write!(
                f,
                "syntax error: invalid escape \\{c}; for text holding its own backslashes (a sed or awk program, a Windows path) use a raw string, `r'…'`"
            ),
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
    /// More input would finish this: an open delimiter, a trailing operator. The
    /// error that says so rides along rather than being discarded, because
    /// "incomplete" is only an *answer* for a reader that can go on reading. A
    /// whole script or `-c` string has no more input coming, so for those this is
    /// a syntax error — and one that has to be able to say **where**, which is
    /// what the payload is for.
    Incomplete(Box<ParseError>),
    /// Incomplete because a heredoc body is still open, awaiting a line equal to
    /// this delimiter. Distinguished from [`ParseOutcome::Incomplete`] so a
    /// line-at-a-time reader can wait for that one line directly: re-parsing the
    /// buffer after each body line is quadratic in the body's length, and a body
    /// is bulk data rather than syntax.
    IncompleteHeredoc(String),
}

/// One `NAME=value` (or `NAME+=value`) in a `with` header. The same three parts
/// an [`Executable::EnvAssignment`] carries, so both go through `environ::write`
/// and inherit its boundary rules unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvBinding {
    pub key: String,
    pub append: bool,
    pub value: Expr,
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

/// Which entry an `$env` write or removal names.
///
/// The two spellings the environment has as a *place*, kept in one type so a
/// write and a removal cannot disagree about what `$env[…]` means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvKey {
    /// `$env.KEY` — spelled out, and checked by the parser.
    Literal(String),
    /// `$env[…]` — the raw subscript text, resolved to a name at run time by the
    /// same `subscript_key` a read goes through, so `$env[$n] = v` writes exactly
    /// the entry `$env[$n]` reads.
    Computed(String),
}

/// One thing an `unset` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsetTarget {
    /// A whole binding, dropped from the scope.
    Name(Spanned<String>),
    /// A place inside one — `$m.key`, `$xs[0]` — carried as the raw reference text
    /// exactly as [`Executable::MemberAssignment`] carries its target, so both go
    /// through one path parser.
    Member(String),
    /// An environment entry — `unset $env.KEY`, `unset $env[$name]` — removed from
    /// the process environment rather than from a scope. Its own variant for the
    /// reason [`Executable::EnvAssignment`] is: `$env` holds bytes in a table
    /// children inherit, not typed values in a binding.
    Env(EnvKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Executable {
    Pipeline(Pipeline),
    /// `not cmd` in a **condition** — the negation of what the wrapped statement's
    /// status says, so `if not test -f x { … }` branches on the command having
    /// failed. Only [`Parser::condition`] builds it, and only when what follows the
    /// `not` is not a value: before a value the word is the ordinary
    /// [`UnaryOp::Not`] operator, which the expression parser owns.
    ///
    /// The negation is a *reading* of a status, not a second run of it, which is
    /// why it sits outside the wrapped node rather than inside the pipeline: the
    /// command still publishes the code it really exited with, so the branch reads
    /// `130` from a Ctrl-C where bash's `!` has flattened it to `1`.
    Not(Box<Executable>),
    Assignment {
        pattern: BindingPattern,
        append: bool,
        value: Expr,
        /// `global name = …` — bind in the session-global scope rather than the
        /// active one. Assignment is local-by-default inside a function, so this
        /// is how a function writes a global on purpose.
        global: bool,
    },
    /// `unset name …` — drop bindings in the current scope, or with `global`, in
    /// the session-global one. A target may instead name a **place inside** a
    /// binding (`unset $m.key`), which removes that entry rather than the binding.
    Unset {
        targets: Vec<UnsetTarget>,
        global: bool,
    },
    /// `$env.KEY = value`, `$env[$name] = value` — a write to the process
    /// environment rather than to a mesh binding. Separate from
    /// [`Executable::MemberAssignment`] because `$env` is a reserved namespace
    /// whose entries are bytes, not typed values: only strings cross, and the
    /// write reaches the real environment so children inherit it.
    EnvAssignment {
        key: EnvKey,
        append: bool,
        value: Expr,
    },
    /// `$m.key = value`, `$xs[0] = value`, `$m.a[1].b += value` — a write *into* a
    /// bound collection rather than a rebinding of the name. `target` is the raw
    /// reference text, split into a root and accesses by the expansion layer, so
    /// one path parser serves reads and writes alike.
    MemberAssignment {
        target: String,
        append: bool,
        value: Expr,
        /// `global $m.key = …` — write into the session-global binding rather than
        /// the active one. A member write is local-by-default like every other
        /// assignment, so this is how a function reaches a caller's collection
        /// instead of shadowing it.
        global: bool,
    },
    Function {
        name: String,
        parameters: Vec<Param>,
        body: Source,
        /// `wrapper func name(…) { … }` — the function parses no flags of its
        /// own, so every argument reaches its positionals and `...rest`
        /// verbatim. A wrapper cannot validate what it forwards, because it does
        /// not know the callee's grammar; the check is *relocated* to the
        /// wrapped call rather than dropped (`DESIGN.md` §"Functions").
        wrapper: bool,
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
    /// `export A=1 B=2 …` — several environment writes in one statement, each
    /// the [`EnvAssignment`](Executable::EnvAssignment) it would have been alone.
    /// Its own variant rather than a loop at the call site because a statement is
    /// one `Executable`, and applying them left to right is what lets a later one
    /// read what an earlier one wrote.
    EnvAssignments {
        bindings: Vec<EnvBinding>,
    },
    /// `with NAME=value … { … }` — run the body with those environment entries
    /// overridden, restoring what was there on the way out, however the body
    /// leaves: normally, through an error, or through `return` / `break` /
    /// `continue`. The block form of what other shells write as a one-command
    /// prefix (`LC_ALL=C sort file`); the prefix itself is still undecided
    /// (`TODO.md`), and the header spells a binding the same way it would, so
    /// the two cannot drift.
    ///
    /// **Environment**, not shell variables: the point is what a child inherits.
    With {
        bindings: Vec<EnvBinding>,
        body: Source,
    },
    /// `fork { … }` — a subshell. The body runs in a forked child, so the
    /// process state it changes (cwd, environment, umask) and the bindings it
    /// makes are its own, and an `exit` inside it ends the child rather than the
    /// shell. `DESIGN.md` makes isolation explicit in three grades; this is the
    /// strongest, and the only one that costs a process.
    Fork {
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

    /// Is this a declared `--flag` — a switch or a valued flag — rather than a
    /// positional or `...rest`?
    pub fn is_option(&self) -> bool {
        matches!(self, ParamKind::Switch | ParamKind::Flag(_))
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
    /// `fail [STATUS]` — leave the function with a nonzero status and no result.
    /// The status channel's counterpart to `return`, spelled apart from `exit`
    /// because `exit` tears down the whole shell.
    Fail,
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
    /// `NAME=value …` written in front of the command — environment entries in
    /// place for this command alone, restored after it. Per **stage** rather than
    /// per statement, as in every other shell: `FOO=1 a | FOO=2 b` gives each
    /// side its own, and `FOO=1 a && b` leaves `b` alone.
    pub env: Vec<EnvBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandItem {
    Word(Spanned<Word>),
    /// A **value expression** as an argument — `puts (1 + 2)`, `puts $(pwd)`,
    /// `puts style(x, fg: red)`. `DESIGN.md` writes the first two in its own examples
    /// (§"Arithmetic" and §"I/O"); the parser accepts the shapes that have no word
    /// spelling, so a list literal and a range stay the text they already are.
    ///
    /// Kept unevaluated here because running it needs the shell — a `$(…)` launches a
    /// command and a call runs a function — which the parser does not have.
    Value(Spanned<Expr>),
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
    /// `<<< word` — the target is the input text itself rather than a path.
    HereString,
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
    pub body: MatchBody,
}

/// An arm's right-hand side: `=> value` is a **value** expression (a bare word is a
/// string), `=> { … }` is a **block** in ordinary statement context (a bare word runs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchBody {
    Value(Expr),
    Block(Source),
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
    /// A bare modifier reference — `:stem` — denoting "the function that applies
    /// `:stem`". Written where a callable is wanted, so `$paths:map(:stem)` says
    /// what `$paths:map(func(p) { $p:stem })` says (`DESIGN.md`).
    ModifierRef(String),
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
    // A heredoc whose delimiter has not arrived yet is reported by the
    // *tokenizer*, before parsing begins, so it needs the same
    // buffer-rather-than-fail reading an open brace gets from the arm below.
    // Without this the line-at-a-time reader rejects `cat << END` on sight,
    // which is every interactive and piped use of a heredoc.
    let tokens = match tokenize(source) {
        Ok(tokens) => tokens,
        Err(ParseError {
            kind: ParseErrorKind::UnterminatedHeredoc(delimiter),
            ..
        }) => {
            return Ok(ParseOutcome::IncompleteHeredoc(delimiter));
        }
        // An unclosed **quote** is the tokenizer running out of input, not a shape
        // it rejects — the next line can close it, and a script proves it: a
        // `r'…'` holding a two-line sed program parses and runs from a file
        // today. Reported rather than buffered, the same text was a syntax error
        // the moment it arrived a line at a time, so the two readers disagreed
        // about the same source.
        //
        // Buffering does not swallow the diagnostic, which was the reason for
        // returning early here: at true end of input every caller converts an
        // incomplete parse back into the error it carries, because nothing more
        // is coming. Only a reader with more input to offer waits, and it shows
        // the continuation prompt while it does — exactly what an open `{`
        // already gets from the arm below.
        //
        // The quote characters and nothing else. A **bare** `${x` also ends the
        // input, but no later line can complete it: it is scanned by
        // `variable_end`, whose `valid_variable_access` rejects the newline, so
        // buffering it would consume a following `}` into a reference that still
        // cannot parse — and with no `}` at all, swallow every command after it
        // through EOF. Raised in review as a P2. An unclosed `$(` or `${` *inside*
        // a string keeps its old hard error for the same reason: continuing it is
        // a separate question from continuing the string around it.
        Err(ParseError {
            kind: ParseErrorKind::Unterminated(quote @ ('"' | '\'')),
            span,
        }) => {
            return Ok(ParseOutcome::Incomplete(Box::new(ParseError {
                kind: ParseErrorKind::Unterminated(quote),
                span,
            })));
        }
        Err(error) => return Err(error),
    };
    let open_block = tokens
        .iter()
        .fold(0_usize, |depth, token| match token.value {
            TokenKind::LBrace => depth + 1,
            TokenKind::RBrace => depth.saturating_sub(1),
            _ => depth,
        })
        > 0;
    let unclosed = unclosed_opener(&tokens);
    let mut parser = Parser {
        tokens,
        position: 0,
        source,
        source_len: source.len(),
        depth: 0,
        regex_slot: false,
        grouped: 0,
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
            // Point at the delimiter that is still open rather than at the end of
            // the input. "Unexpected end of input" on line 1800 of a config says
            // only that the file ended, which is what made locating one a bisect;
            // the `(` on line 42 is the answer the reader wanted.
            //
            // Only when running out of input is what actually went wrong, though.
            // `open_block` also sends a *real* error here — `x = )` followed by an
            // unmatched `{` — and substituting there would replace the fault on
            // line 1 with a brace on line 2, hiding the thing the reader needs.
            let ran_out = matches!(
                error.kind,
                ParseErrorKind::UnexpectedEnd | ParseErrorKind::Unterminated(_)
            );
            Ok(ParseOutcome::Incomplete(Box::new(
                unclosed
                    .filter(|_| ran_out)
                    .map_or(error, |(delimiter, span)| ParseError {
                        kind: ParseErrorKind::Unterminated(delimiter),
                        span,
                    }),
            )))
        }
        Err(error) => Err(error),
    }
}

/// The innermost delimiter still open at the end of the token stream, and where
/// it was written.
///
/// Innermost because that is the one a reader has to close first, and the one an
/// editor's own matching would have led them to.
///
/// A closer only pops an opener it **matches**. Popping whatever was innermost
/// let a mismatched one throw away a delimiter that is genuinely still open —
/// `$(echo ]` lost the capture's `(` to a stray `]` and fell back to end of
/// input, which is the answer this exists to stop giving. Leaving the stack
/// alone also reads better for `([)`: the `[` really does have to close before
/// the `)` can match, so naming it is the honest answer rather than a guess.
fn unclosed_opener(tokens: &[Token]) -> Option<(char, Span)> {
    let mut open: Vec<(char, Span)> = Vec::new();
    for token in tokens {
        match token.value {
            TokenKind::LParen => open.push(('(', token.span.clone())),
            // `$(` is one token, so its span starts at the `$`. The delimiter
            // left open is the paren, and the report names the paren, so point at
            // it rather than one column to its left.
            TokenKind::CaptureStart => {
                open.push(('(', token.span.start + 1..token.span.end));
            }
            TokenKind::LBracket => open.push(('[', token.span.clone())),
            TokenKind::LBrace => open.push(('{', token.span.clone())),
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                let closes = match token.value {
                    TokenKind::RParen => '(',
                    TokenKind::RBracket => '[',
                    _ => '{',
                };
                if open.last().is_some_and(|(opener, _)| *opener == closes) {
                    open.pop();
                }
            }
            _ => {}
        }
    }
    open.pop()
}

/// Where a byte offset falls in the source, as a 1-based line and column.
///
/// Columns count **characters**, not bytes: the number is for a person locating
/// a spot in their own file, and a byte column would name the wrong place on any
/// line holding a non-ASCII character.
#[must_use]
pub fn line_and_column(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    (
        source[..offset].matches('\n').count() + 1,
        source[start..offset].chars().count() + 1,
    )
}

/// How a still-open `func` parameter list (the text after `(`, before any `)`)
/// relates to the signature grammar — used by the multi-line reader to decide
/// buffer-vs-dispatch without a second copy of the grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrefixStatus {
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
pub(crate) fn params_prefix_status(list: &str) -> PrefixStatus {
    // A tokenize failure is an unterminated quote/raw string — the same thing that
    // makes `parse()` return an error (it propagates `tokenize(..)?`), so it is a
    // hard error the reader dispatches, not an open construct to buffer.
    let Ok(tokens) = tokenize(list) else {
        return PrefixStatus::Malformed;
    };
    Parser {
        tokens,
        position: 0,
        source: list,
        source_len: list.len(),
        depth: 0,
        regex_slot: false,
        grouped: 0,
    }
    .parameters_prefix()
}

/// Fold a leading `-` into the integer literal it applies to, rather than
/// negating that literal at runtime.
///
/// The magnitudes are not symmetric: `i64::MIN` is one further from zero than
/// `i64::MAX`, so `-9223372036854775808` has no positive counterpart to negate.
/// Parsed as a negation it read `9223372036854775808` first, which does not fit
/// an `i64`, so the operand was already a *string* by the time the sign would
/// apply and the negation reported "expected integer" — while `i64::MIN + 1` and
/// `i64::MAX` both worked. Folding the sign into the literal is what languages
/// with two's-complement integers do, Rust included.
///
/// Anything that does not fold keeps the runtime negation, which is what
/// type-checks a string or a variable and reports the overflow.
fn negative_literal(minus: &Span, operand: &Expr) -> Option<Expr> {
    let Expr::Scalar(word) = operand else {
        return None;
    };
    // Only an **attached** sign folds. Spaced, the reader wrote an operation, and
    // folding it built a literal they did not: `- 0` became the text `-0`, which is
    // not `0`'s own spelling, so it typed as a string and `$x + 1` stopped working.
    // `- 007` was worse — it bound the string `-007` where negating a string has to
    // report, as `- abc` does. Adjacency is the whole distinction, and the spans
    // already carry it. Raised in review.
    if minus.end != word.span.start {
        return None;
    }
    // Whether the signed text parses is the whole test — exact on its own, so
    // there is no second shape check to drift out of step with it.
    let text = format!("-{}", word.value.bare_word()?);
    text.parse::<i64>().ok()?;
    Some(Expr::Scalar(Spanned {
        value: Word {
            pieces: vec![WordPiece::Text {
                text,
                quote: QuoteMode::Bare,
            }],
            qualifiers: None,
        },
        // The sign is part of the literal now, so the span covers it.
        span: minus.start..word.span.end,
    }))
}

/// The `i64` `text` spells, when `text` is that integer's **own** spelling.
///
/// `007`, `08`, `+5` and `-0` all parse, but an `i64` carries no record of how it
/// was written, so binding one and rendering it back loses the text: `007` reached
/// a command as `7`. Round-tripping the rendering is the exact test, rather than a
/// scan for leading zeros or signs, so it cannot fall out of step with how an
/// integer prints.
///
/// The **one** place that question is answered. [`Word::bare_integer`] asks it to
/// decide whether a word is a value rather than a command word, and
/// [`crate::expand::typed_scalar`] asks it to decide how an argument binds; the two
/// disagreeing meant a program named `007` on `PATH` was claimed as a value and
/// silently skipped in statement position while binding as the string it is
/// everywhere else. Sharing half a rule is how that happens — raised in review.
pub(crate) fn canonical_integer(text: &str) -> Option<i64> {
    text.parse::<i64>()
        .ok()
        .filter(|number| number.to_string() == text)
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
    /// How many `"$( … )"` the lexer is currently *inside*. A capture in a string is
    /// lexed and parsed where it is found, so `"$(puts "$(…)")"` recurses through
    /// the lexer, not the parser — [`Parser::depth`] alone never sees it. The two
    /// counters share [`MAX_DEPTH`] rather than each getting their own: the frames
    /// sit on one stack, so it is the total that has to be bounded.
    capture_depth: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
            capture_depth: 0,
        }
    }

    fn run(mut self) -> Result<Vec<Token>, ParseError> {
        Ok(self.lex(None)?.0)
    }

    /// Lex the body of a `$( … )` whose `$(` sits just before `self.position`,
    /// stopping at the `)` that closes it. Returns the body's tokens — including
    /// that `)`, which the parser expects — and the offset just past it.
    ///
    /// Bounded because the caller is midway through a **string**: the text after the
    /// capture's `)` is string content, and lexing it as code would report the
    /// string's own closing quote as an unterminated one.
    fn capture_body(mut self) -> Result<(Vec<Token>, usize), ParseError> {
        self.lex(Some(TokenKind::RParen))
    }

    /// The same, for the body of a `${ … }` holding an expression rather than a
    /// plain variable access. Stops at the `}` that closes it.
    fn braced_body(mut self) -> Result<(Vec<Token>, usize), ParseError> {
        self.lex(Some(TokenKind::RBrace))
    }

    /// `close` is the delimiter that ends this lex, or `None` to run to the end of
    /// the source. Nesting is tracked for whichever delimiter that is, so a `)` or
    /// `}` belonging to something inside the body is not mistaken for the end of it.
    fn lex(&mut self, close: Option<TokenKind>) -> Result<(Vec<Token>, usize), ParseError> {
        let body_start = self.position;
        let mut depth = 0_usize;
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
                // A closer at depth zero is the one that ends the body being lexed.
                // Only the matching delimiter counts: a `)` inside a quoted word
                // (`$(puts "a)b")`) is part of that word's token and never reaches
                // here.
                let closes = close.as_ref() == Some(&kind) && depth == 0;
                match (&close, &kind) {
                    (Some(TokenKind::RBrace), TokenKind::LBrace) => depth += 1,
                    (Some(TokenKind::RBrace), TokenKind::RBrace) => {
                        depth = depth.saturating_sub(1);
                    }
                    (_, TokenKind::LParen | TokenKind::CaptureStart) => depth += 1,
                    (_, TokenKind::RParen) => depth = depth.saturating_sub(1),
                    _ => {}
                }
                tokens.push(Spanned {
                    value: kind,
                    span: start..self.position,
                });
                if closes {
                    return Ok((tokens, self.position));
                }
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
                    // Where the quote itself is, kept for the unterminated case:
                    // the word may have started columns earlier, and an escaped
                    // copy of the same character may sit between the two.
                    let quote_at = self.position;
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
                                // `BEL`. Not for the bell: it is the terminator
                                // every shell's title-setting idiom uses, so
                                // `"\e]0;…\a"` is the form a prompt gets copied in
                                // as. `\u{7}` spelled it before and still does.
                                'a' => '\u{7}',
                                // The rest of C's control escapes, so the set has
                                // no arbitrary hole for someone to find one at a
                                // time. `\0` is *not* among them: a NUL cannot
                                // cross `execve` or the environment, both of which
                                // mesh refuses it at, so the escape would only
                                // build values that fail later.
                                'b' => '\u{8}',
                                'f' => '\u{c}',
                                'v' => '\u{b}',
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
                            // `"… $(cmd) …"` — a capture interpolates inside double
                            // quotes, the same as `$name` does. It becomes a value
                            // piece rather than text, so its output crosses whole:
                            // quoted, so never split and never globbed.
                            if self.source[self.position..].starts_with("$(") {
                                let (expression, end) = capture_in_string(
                                    self.source,
                                    self.position,
                                    self.capture_depth + 1,
                                )?;
                                pieces.push(WordPiece::Value {
                                    expression: Box::new(Spanned {
                                        value: expression,
                                        span: self.position..end,
                                    }),
                                    quote: QuoteMode::Double,
                                });
                                self.position = end;
                                continue;
                            }
                            // `"… ${ expr } …"` — a braced body that is not a plain
                            // access is an expression, and needs the shell for the
                            // same reason a capture does, so it rides in the same
                            // value piece rather than through `expand`. A body whose
                            // modifier takes arguments comes here too, still meaning
                            // the binding its head names.
                            if braced_is_access(self.source, self.position) == Some(false) {
                                let (expression, end) = braced_expression_in_string(
                                    self.source,
                                    self.position,
                                    self.capture_depth + 1,
                                )?;
                                // An access whose modifier took arguments is still
                                // the *reference* reading, so its sigil-less head
                                // names the binding rather than the word.
                                let expression = if has_modifier_arguments(&expression) {
                                    head_as_variable(expression)
                                } else {
                                    expression
                                };
                                pieces.push(WordPiece::Value {
                                    expression: Box::new(Spanned {
                                        value: expression,
                                        span: self.position..end,
                                    }),
                                    quote: QuoteMode::Double,
                                });
                                self.position = end;
                                continue;
                            }
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
                            span: quote_at..self.source.len(),
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
                value: TokenKind::Word(Word {
                    pieces,
                    qualifiers: None,
                }),
                span: start..self.position,
            });
        }
        // Ran out of input with the body still open. Reported as its own unterminated
        // open delimiter so it reads like the thing that was left open, rather than
        // as whatever the string's own quote would have been blamed for.
        if let Some(kind) = &close {
            let opener = if *kind == TokenKind::RBrace { '{' } else { '(' };
            // Innermost first, as everywhere else: the body's own opener is the
            // outermost thing still open, so a delimiter opened *inside* it has to
            // be closed before the body can be. Without asking the body's tokens,
            // `"$(x = [1"` blamed the `(` while the unquoted spelling of the same
            // text correctly named the `[`.
            let (delimiter, span) = unclosed_opener(&tokens).unwrap_or((
                opener,
                // The body starts after `$(` or `${`, and the delimiter named is
                // that opener.
                body_start.saturating_sub(1)..self.source.len(),
            ));
            return Err(ParseError {
                kind: ParseErrorKind::Unterminated(delimiter),
                span,
            });
        }
        Ok((tokens, self.position))
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
                // A delimiter is matched against the body's lines as *text*, so a
                // capture in one has nothing to contribute — and evaluating the word
                // would run a command to decide where a heredoc ends. Refused rather
                // than half-honored: `<<"$(x)"` used to be the literal delimiter
                // `$(x)`, which is nobody's intent worth keeping.
                if word
                    .pieces
                    .iter()
                    .any(|piece| matches!(piece, WordPiece::Value { .. }))
                {
                    return Err(ParseError {
                        kind: ParseErrorKind::Expected("a heredoc delimiter without a capture"),
                        span: tokens[index + 1].span.clone(),
                    });
                }
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
                    kind: ParseErrorKind::UnterminatedHeredoc(delimiter.to_owned()),
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
        // `=>` separates a match arm from its body, but like the other value
        // operators it needs a boundary on each side. Without that rule an
        // attached redirection would be swallowed: `puts value=>out` is the word
        // `value=` followed by `>out`, not a fat arrow.
        if let Some(tail) = rest.strip_prefix("=>") {
            let before = self.source[..self.position].chars().next_back();
            let after = tail.chars().next();
            let boundary = |value: Option<char>| {
                value.is_none_or(|c| c.is_whitespace() || ",()[]{};=:".contains(c))
            };
            if boundary(before) && boundary(after) {
                return Some(("=>", TokenKind::FatArrow));
            }
        }
        let choices = [
            ("$(", TokenKind::CaptureStart),
            ("...", TokenKind::Spread),
            ("..=", TokenKind::RangeInclusive),
            ("|&", TokenKind::PipeBoth),
            ("&&", TokenKind::AndAnd),
            ("||", TokenKind::OrOr),
            (">>", TokenKind::Append),
            ("<<<", TokenKind::HereString),
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

/// Parse the `$( … )` starting at `dollar`, from **inside** a double-quoted string.
/// Returns the capture and the offset just past its `)`.
///
/// A `$name` reference ends where its characters stop, so [`variable_end`] can scan
/// for it. A capture body is a whole script, so where it ends is a question only the
/// grammar answers — `"$(puts "a)b")"` closes on the second `)`, not the first. So
/// the body is lexed and parsed here and now, which is also what keeps a syntax
/// error inside it a *parse* error rather than a surprise at run time.
fn capture_in_string(
    source: &str,
    dollar: usize,
    depth: usize,
) -> Result<(Expr, usize), ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError {
            kind: ParseErrorKind::TooDeep,
            span: dollar..dollar + 2,
        });
    }
    let (tokens, end) = Lexer {
        source,
        position: dollar + 2,
        capture_depth: depth,
    }
    .capture_body()?;
    // The inner parse starts where the lexer left off rather than at zero, so a
    // shape that alternates the two — a capture in a string holding a group holding
    // a capture in a string — is bounded by the sum instead of by neither.
    let mut parser = Parser {
        tokens,
        position: 0,
        source,
        source_len: end,
        depth,
        regex_slot: false,
        grouped: 0,
    };
    Ok((Expr::Capture(parser.source(Some(TokenKind::RParen))?), end))
}

/// For a `${…}` at `start`: whether its body is a plain variable access, or
/// `None` when this is not a braced reference (or is unterminated, which
/// [`variable_end`] reports).
fn braced_is_access(source: &str, start: usize) -> Option<bool> {
    let braced = source[start..].strip_prefix("${")?;
    let close = braced.find('}')?;
    Some(valid_variable_access(&braced[..close]))
}

/// Does this chain end in a modifier that was **given arguments**?
///
/// The question is asked of the parsed body rather than of its text. A scan for
/// the closing `)` has to re-derive the lexer's idea of what counts as text —
/// escapes, raw strings, comments, token heads, nested interpolations — and every
/// place the two disagreed produced a body read one way by the scan and another by
/// the lexer. The parse already applied all of those rules, so it is the thing to
/// ask.
fn has_modifier_arguments(expression: &Expr) -> bool {
    match expression {
        Expr::Modifier {
            value, arguments, ..
        } => arguments.is_some() || has_modifier_arguments(value),
        Expr::Member { value, .. } | Expr::Index { value, .. } => has_modifier_arguments(value),
        _ => false,
    }
}

/// Read the head of an access-shaped body as the **binding** it names.
///
/// `${xs:join(" ")}` is the reference form, so its sigil-less `xs` is the variable
/// — but the expression parser that carries the arguments reads a bare word as the
/// *word* `xs`, which is how `:join` came to report "requires a list" the moment a
/// modifier grew an argument. The chain is walked to its root because the
/// modifiers, members, and indices wrap it.
fn head_as_variable(expression: Expr) -> Expr {
    match expression {
        // A single bare run of text, by [`Word::bare_integer`]'s rule: a quoted or
        // escaped head spells a string, not a name, and must stay one.
        //
        // It becomes the same **variable piece** the sigil spelling produces, rather
        // than an `Expr::Variable`, so a head that carries its own members and
        // indices (`m.k`, `xs[0]`) resolves exactly as `${$m.k:…}` does — `expand`
        // reads the whole path out of the one name.
        Expr::Scalar(word) => {
            // Every piece bare text, joined: the lexer splits `m.k` into `m`, `.`,
            // `k`, so one piece is not the test — but a quoted or escaped piece
            // anywhere means the head spells a *string*, and those keep their
            // reading.
            let bare = word
                .value
                .pieces
                .iter()
                .try_fold(String::new(), |mut text, piece| match piece {
                    WordPiece::Text {
                        text: part,
                        quote: QuoteMode::Bare,
                    } => {
                        text.push_str(part);
                        Some(text)
                    }
                    _ => None,
                });
            match bare {
                Some(name) if valid_variable_access(&name) => Expr::Scalar(Spanned {
                    value: Word {
                        pieces: vec![WordPiece::Variable {
                            name,
                            quote: QuoteMode::Bare,
                        }],
                        qualifiers: word.value.qualifiers.clone(),
                    },
                    span: word.span,
                }),
                _ => Expr::Scalar(word),
            }
        }
        Expr::Modifier {
            value,
            name,
            arguments,
        } => Expr::Modifier {
            value: Box::new(head_as_variable(*value)),
            name,
            arguments,
        },
        Expr::Member { value, name } => Expr::Member {
            value: Box::new(head_as_variable(*value)),
            name,
        },
        Expr::Index { value, index } => Expr::Index {
            value: Box::new(head_as_variable(*value)),
            index,
        },
        other => other,
    }
}

/// Parse the `${ … }` starting at `dollar` as an **expression**, for a body that
/// is not a plain variable access — `"${greeting()}"`, `"${$n + 1}"`.
///
/// `DESIGN.md` §"Variables and assignment" puts general expressions in `${…}`;
/// [`valid_variable_access`] covers the cheap majority (a name, member, index, or
/// modifier chain) which [`crate::expand`] resolves with only `&Vars`, and this
/// covers the rest, which needs the shell and so rides in as a
/// [`WordPiece::Value`] exactly as a `$(…)` capture does.
fn braced_expression_in_string(
    source: &str,
    dollar: usize,
    depth: usize,
) -> Result<(Expr, usize), ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError {
            kind: ParseErrorKind::TooDeep,
            span: dollar..dollar + 2,
        });
    }
    let (tokens, end) = Lexer {
        source,
        position: dollar + 2,
        capture_depth: depth,
    }
    .braced_body()?;
    let mut parser = Parser {
        tokens,
        position: 0,
        source,
        source_len: end,
        depth,
        regex_slot: false,
        // A newline inside the braces is layout, not a terminator — the body is
        // one expression, so it wraps the way a `( … )` group does, mid-expression
        // included. Without this the trailing `Newline` sat where the `}` was
        // expected and a body that merely broke across lines was a syntax error,
        // while `$( … )` and a bare group both took it.
        grouped: 1,
    };
    parser.newlines();
    let expression = parser.expression()?;
    parser.newlines();
    // The lexer stopped *past* the `}`, so the parser must find it where the body
    // ends — anything else means the body held more than one expression, and
    // reporting that here beats letting the trailing text vanish.
    parser.expect(&TokenKind::RBrace, "`}`")?;
    Ok((expression, end))
}

pub(crate) fn variable_end(source: &str, start: usize) -> Result<usize, ParseError> {
    let rest = &source[start..];
    if let Some(braced) = rest.strip_prefix("${") {
        let Some(close) = braced.find('}') else {
            return Err(ParseError {
                kind: ParseErrorKind::Unterminated('}'),
                // The `{` this is missing the mate for, not the `$` before it.
                span: start + 1..source.len(),
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
    if !head.is_alphabetic()
        && (head != '_'
            || !chars
                .peek()
                .is_some_and(|(_, next)| *next == '_' || next.is_alphanumeric()))
    {
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

pub(crate) fn decode_unicode_escape(source: &str, start: usize) -> Option<(char, usize)> {
    let rest = source.get(start..)?;
    let hex = rest.strip_prefix('{')?;
    let close = hex.find('}')?;
    if close == 0 || close > 6 || !hex[..close].chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(&hex[..close], 16).ok()?;
    Some((char::from_u32(value)?, start + close + 2))
}

/// Is this word something other than a plain bare literal?
///
/// Asked by every decision that needs the word to *be* its spelling — a flag name,
/// a function name, a heredoc delimiter, a command reading. A **value** piece
/// answers yes for the same reason a quote does: the word is not the text it looks
/// like, so `--flag$(x)` is not the flag `flag` and `$(x)y` is not a name.
fn word_is_quoted(word: &Word) -> bool {
    word.pieces.iter().any(|piece| match piece {
        WordPiece::Text { quote, .. } | WordPiece::Variable { quote, .. } => {
            *quote != QuoteMode::Bare
        }
        WordPiece::Value { .. } => true,
    })
}

/// The spelling a token contributes when it is glued to the name in front of it,
/// or `None` when it ends the name instead.
///
/// Deliberately the **same** table a bare command word is assembled from
/// ([`token_word_pieces`]), so a candidate definition name covers every spelling
/// that would have been one word in command position — `a.b`, `a:b`, `a[0]`.
/// Anything the runtime check would reject has to arrive here whole, or it is
/// rejected by the *parser* instead and takes the rest of its file down with it,
/// which is the failure mode reading the name unjudged exists to remove.
///
/// Every piece, `=` included: whether `=` ends a name is not a fact about names
/// but about the **form** being read, so it belongs to the caller —
/// [`name_piece`], which knows which one it is.
fn glued_name_text(kind: &TokenKind) -> Option<String> {
    if let TokenKind::Word(word) = kind {
        return (!word_is_quoted(word)).then(|| word.text());
    }
    let text: String = token_word_pieces(kind)?
        .iter()
        .map(|piece| match piece {
            WordPiece::Text { text, .. } => text.as_str(),
            _ => "",
        })
        .collect();
    Some(text)
}

/// The spelling a token contributes to a definition name, for the form being read.
///
/// `alias` is the only form with a separator: `alias NAME = COMMAND` is told from
/// a command by that `=`, so there it has to end the name however tightly it is
/// written, or `alias co=vcs` would read as defining `co=vcs` with no `=` after it
/// and stop being a definition. A `func` has `(` for that job and no use for `=`,
/// so there it is an ordinary piece of a bare word — `func a=b()` reaches the
/// runtime check and costs only its own definition, where excluding it put the
/// whole-file parse error back.
///
/// `+=` is a separator for neither, so it is always ordinary.
fn name_piece(kind: &TokenKind, alias: bool) -> Option<String> {
    if alias && matches!(kind, TokenKind::Equal) {
        return None;
    }
    glued_name_text(kind)
}

fn merge_command_variable_access(pieces: Vec<WordPiece>) -> Result<Vec<WordPiece>, ParseErrorKind> {
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
            let length = variable_access_prefix(text)?;
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
    Ok(output)
}

pub(crate) fn variable_access_prefix(text: &str) -> Result<usize, ParseErrorKind> {
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
            // Stop at the first character that cannot be *in* a modifier name, then
            // ask whether the name read is one — rather than scanning to a fixed list
            // of delimiters. Every modifier name is alphanumeric, so anything else
            // ends it. A delimiter list silently loses the whole chain to whatever it
            // forgets: `]`, `)` and `,` were all absent, so `"[$x:upper]"` scanned
            // `upper]`, matched no modifier, and reverted to the literal text
            // `[ab:upper]` with no error — while `"$x:upper."` worked, because `.`
            // happened to be listed.
            let length = name_prefix(value);
            // **Shape, not membership.** What follows the colon is claimed when it is
            // an identifier; whether that identifier names a modifier decides only
            // whether it *works*, never whether it was claimed. Asking the name list
            // instead made the reading depend on the list's contents, so a string
            // holding `$h:nope` was the text `host:nope` until the day `:nope` was
            // implemented and it silently became a chain — introducing a modifier
            // would break scripts that never mentioned it. Every other spelling
            // already reserves the shape (`$h:nope` and `"${h:nope}"` are both
            // `:nope` is not a modifier); this was the one that did not.
            //
            // A leading digit is not an identifier, which is what keeps `"$h:2"`,
            // `"$h:/path"`, `"$h:$port"` and a bare `"$h:"` reading as the text they
            // always were: punctuation and numerals were never in the claim.
            if length == 0 {
                break;
            }
            if !modifier_name(&value[..length]) {
                return Err(ParseErrorKind::UnknownModifier(value[..length].to_string()));
            }
            // An abutting `(` after a modifier that **can take one** is an argument
            // list, and this scan has nowhere to put it: it stops at the character,
            // so the arguments stayed behind as literal text while the modifier ran
            // with none. That is silent and wrong in the same breath —
            // `"$env:get(HOME, none)"` answered the whole environment and then failed
            // on being a list. Reported rather than supported here because the
            // expression form already takes them, so what the reader needs is the
            // other spelling.
            //
            // Gated on arity, because after an argument-free modifier a `(` is
            // ordinary text and always was: `"$x:upper(foo)"` is `AB(foo)`, and the
            // braced form the message points at would reject it.
            if modifier_accepts_arguments(&value[..length]) && value[length..].starts_with('(') {
                return Err(ParseErrorKind::InterpolatedModifierArguments(
                    value[..length].to_string(),
                ));
            }
            consumed += length + 1;
        } else {
            break;
        }
    }
    Ok(consumed)
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
                // The delimiters have to be **written** as delimiters. `\/a/` and
                // `/a\/` come out as the text `/a/`, which begins and ends with a
                // slash without either one being one — and outside a match slot
                // that is the ordinary string `/a/`, so reading it as a pattern
                // here would make identical text mean two different things. An
                // escape *inside* is untouched: `/a\/b/` is still a regex whose
                // pattern contains an escaped slash.
                && bare_delimiters(pieces)
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

/// A **regex** match slot: a clean `/…/` literal becomes a regex, and everything
/// else is left exactly as written.
///
/// Narrower than [`match_operand`], which also turns a bare word into a glob.
/// The replace family's slot is only ever "a string matches verbatim, a regex
/// matches as a pattern" (`DESIGN.md` §"String") — there is no glob reading for
/// it to have — so converting one would make `:replaceall(a, X)` fail on a word
/// that should have matched itself.
fn regex_slot_operand(expression: Expr) -> Expr {
    let original = expression.clone();
    let converted = match_operand(expression);
    if became_regex(&converted) {
        converted
    } else {
        original
    }
}

/// Did [`match_operand`] turn this into a regex? Asked through any modifiers,
/// because a **flagged** literal converts to the regex wrapped in its flag chain
/// (`/a/:i` is `:i` applied to a regex, not a regex). Looking only at the top
/// would restore the original word and leave `:i` to be applied to a string.
fn became_regex(expression: &Expr) -> bool {
    match expression {
        Expr::Regex(_) => true,
        // Only through a **regex flag**. Any other modifier means the chain was
        // never a flagged literal but an ordinary string expression that happens
        // to start with a slash word: `/a/:upper` is the string `/A/` everywhere
        // else, and reading it as a regex here would both change its meaning and
        // fail, since `:upper` is not a flag.
        Expr::Modifier {
            value,
            name,
            arguments: None,
        } => regex_flag(name) && became_regex(value),
        _ => false,
    }
}

/// The argument-free modifiers that are regex **flags** rather than transforms.
/// Kept beside [`became_regex`], the only place that needs to tell them apart —
/// applying them is the engine's, and it names the same set.
fn regex_flag(name: &str) -> bool {
    matches!(
        name,
        "i" | "ignorecase" | "m" | "multiline" | "s" | "dotall" | "x" | "extended"
    )
}

/// Are this word's outermost slashes bare, rather than escaped? Asked of the
/// **pieces**, since the reconstructed text cannot tell `\/` from `/` — that
/// difference is exactly what a quote mode records.
fn bare_delimiters(pieces: &[WordPiece]) -> bool {
    let bare_edge = |piece: Option<&WordPiece>, opening: bool| {
        matches!(
            piece,
            Some(WordPiece::Text {
                text,
                quote: QuoteMode::Bare,
            }) if if opening { text.starts_with('/') } else { text.ends_with('/') }
        )
    };
    bare_edge(pieces.first(), true) && bare_edge(pieces.last(), false)
}

/// Does this character end a bare word, so that a `/` before it is the word's
/// last character? Whitespace, the closers and separators, and the `:` a regex
/// flag chain hangs off.
fn ends_a_word(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            ')' | ']' | '}' | ',' | ';' | '&' | '|' | '<' | '>' | '=' | ':'
        )
}

fn match_pattern_operand(expression: Expr) -> Expr {
    let original = expression.clone();
    match match_operand(expression) {
        Expr::Glob(pattern) if !pattern.contains(['*', '?', '[', ']', '{', '}']) => original,
        pattern => pattern,
    }
}

/// Does this word carry glob syntax in **bare** text? The test that decides
/// whether an attached `(…)` is a qualifier list or a call, so `*(d)` qualifies a
/// glob while `style(x)` and `"*"(d)` stay calls — a quoted star is not a pattern.
fn word_globs(word: &Word) -> bool {
    word.pieces.iter().any(|piece| {
        matches!(
            piece,
            WordPiece::Text {
                text,
                quote: QuoteMode::Bare,
            } if text.contains(['*', '?', '['])
        )
    })
}

/// Does this word's text begin with `/`? The test that tells `../x` from a range,
/// a leading `/` being a spelling no operand has.
fn word_starts_with_slash(word: &Word) -> bool {
    matches!(
        word.pieces.first(),
        Some(WordPiece::Text { text, .. }) if text.starts_with('/')
    )
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

/// Names that cannot be user functions because they are built-in **values**,
/// reachable only through the `name(...)` call form: the `re` / `style` / `link`
/// constructors, and the `glob` family, which expands rather than constructs but
/// answers with a value all the same.
pub const RESERVED_FUNCTION_NAMES: &[&str] = &["re", "style", "link", "glob", "files", "dirs"];

/// The rest parameter an `alias` desugars to. Not a name a user can collide
/// with: it only ever appears inside the generated body, which nothing else can
/// see.
const ALIAS_REST: &str = "args";

/// Does this name call a built-in **value** rather than a command? Asked wherever
/// a call has to be told apart from a command that merely shares the call spelling
/// — `f(…):capture` is one, since a command's record has no `.value` to fill in.
pub fn value_builtin(name: &str) -> bool {
    RESERVED_FUNCTION_NAMES.contains(&name)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    position: usize,
    /// The text the tokens were lexed from. Spans index into it, so a construct
    /// that has to be read as it was *written* rather than as it tokenized —
    /// see [`Parser::regex_literal`] — can take itself back out of the source.
    source: &'a str,
    source_len: usize,
    /// Is the operand about to be parsed a **regex slot** — the right-hand side
    /// of `~` / `!~`, a `match` arm, or the pattern a replace takes?
    ///
    /// Consulted once and cleared, by the first [`Parser::primary`] to run under
    /// it, so it describes where an operand *starts* and never leaks into a
    /// sub-expression further in.
    regex_slot: bool,
    /// How many nested constructs deep [`Parser::primary`] currently is, so that
    /// input can be *refused* before the recursive descent runs out of stack.
    depth: usize,
    /// How many enclosing `( … )` groups or `${ … }` bodies the parse is inside.
    ///
    /// Those hold exactly **one expression**, so a newline in them cannot be
    /// separating anything and is layout — see [`Parser::wraps`]. Cleared by
    /// [`Parser::source`], since a block or a capture written inside a group is
    /// back to statements, where a newline separates again.
    grouped: usize,
}

/// How deep a nesting the parser accepts before reporting [`ParseErrorKind::TooDeep`].
///
/// Far past anything written by hand — input nested 100 deep is generated, or
/// pathological, or a paste that went wrong — and comfortably under what the
/// stack holds. The shapes cost different amounts per level; measured on a debug
/// build a chain of `$( … )` captures is the most expensive, overflowing the
/// usual 8 MiB at 253 levels, so this leaves room to spare on the stack a shell
/// actually starts with.
///
/// It cannot leave room on *every* stack, which is the limitation to know about:
/// under `ulimit -s 1024` that same shape overflows at 30, below this limit, and
/// the check never gets to fire. That case is why [`crate::stack`] exists — the
/// fault is reported rather than aborted — but the honest summary is that the
/// limit is the fix for the common stack and the handler is the net for the rest.
const MAX_DEPTH: usize = 100;

impl Parser<'_> {
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
        // Contextual too: `alias` leads a definition only in the shape
        // `alias NAME = …`, so a command called `alias` and a variable of that
        // name are both still reachable.
        if self.word("alias") && self.alias_definition_follows() {
            return self.alias_def();
        }
        // Contextual, like `fork`: `wrapper` leads a definition only where `func`
        // follows it, so a command or variable of that name is still reachable as
        // `wrapper`, `wrapper --flag`, or `wrapper = 1`.
        if self.word("wrapper") && self.word_at(1, "func") {
            self.take_word("wrapper");
            return self.function(true);
        }
        if self.word("func")
            && !self
                .tokens
                .get(self.position + 1)
                .is_some_and(|token| matches!(token.value, TokenKind::LParen))
        {
            return self.function(false);
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
        // Contextual, like `global` and `unset`: `fork` leads a statement only
        // where a subshell can follow, so a command of that name is still
        // reachable as `fork`, `fork --flag`, or `fork somewhere`.
        if self.word("fork") && self.block_follows(1) {
            return self.fork_expr();
        }
        // Contextual for the same reason `fork` is: `with` leads a statement only
        // where a `NAME=` binding follows it, so `with`, `with --help` and
        // `with somewhere` are still reachable as commands.
        if self.word("with") && self.env_binding_follows(1) {
            return self.with_expr();
        }
        if self.word("return") || self.word("fail") || self.word("break") || self.word("continue") {
            return self.control();
        }
        // `global` and `unset` lead a statement, but only where one can follow —
        // `global = 5` still binds a variable of that name, the way any other
        // lowercase word may be one. Contextual because neither is reserved in
        // `DESIGN.md`; only `env` and `sh` are.
        if (self.word("global") || self.word("unset")) && !self.assignment_follows(1) {
            return self.scoped();
        }
        if self.word("export") && !self.assignment_follows(1) {
            return self.export();
        }
        // `not cmd` on its own line negates the command's status, by the same rule and
        // the same discriminator a condition uses — one meaning for the word in both
        // positions. A statement has no branch to carry the answer, so the negated
        // code *is* the result: `run_recorded` publishes it, and its own invariant
        // keeps `$sh.pipestatus` describing the run that produced it.
        //
        // Ahead of `assignment_start`, so the probes below reset to the operand rather
        // than to the `not` — rewinding past it would hand the line back to the
        // expression parser with the negation still on it.
        let negations = self.command_negations(false);
        let negate = |executable| {
            if negations % 2 == 1 {
                Executable::Not(Box::new(executable))
            } else {
                executable
            }
        };
        // `NAME=value cmd` is an environment prefix, not an assignment, and only
        // what follows the bindings tells them apart. Asked before the assignment
        // paths below, since the first one would otherwise claim `FOO=bar` and
        // leave `cmd` looking like a second statement. `command` re-reads the run
        // when it gets there, which is what binds it to a **stage** rather than to
        // the whole pipeline.
        //
        // After the negation, so `not FOO=bar cmd` negates the prefixed command
        // rather than leaving the `not` behind for the expression parser.
        if self.env_prefix_follows()? {
            return Ok(negate(Executable::Pipeline(self.pipeline()?)));
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
                return Ok(negate(Executable::EnvAssignment { key, append, value }));
            }
            self.position = assignment_start;
        }
        if let Some(target) = self.member_target() {
            if matches!(
                self.peek().map(|token| &token.value),
                Some(TokenKind::Equal | TokenKind::PlusEqual)
            ) {
                let append = self.eat(&TokenKind::PlusEqual).is_some();
                if !append {
                    self.expect(&TokenKind::Equal, "`=`")?;
                }
                let value = self.expression()?;
                return Ok(negate(Executable::MemberAssignment {
                    target,
                    append,
                    value,
                    global: false,
                }));
            }
            self.position = assignment_start;
        }
        // `_ = …`. The probe below cannot see it — `word_text_at` answers only for a
        // *name*, and `_` deliberately is not one — so without this it falls past
        // every assignment path to `command not found: _`, which says nothing about
        // the rule it broke.
        if self.word("_") && self.assignment_follows(1) {
            return Err(self.error(ParseErrorKind::DiscardAssignment));
        }
        if !self.word("not") && (self.word_text_at(0).is_some() || self.same(&TokenKind::LBracket))
        {
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
                return Ok(negate(Executable::Assignment {
                    global: false,
                    pattern,
                    append,
                    value,
                }));
            }
            self.position = assignment_start;
        }
        // Unreachable with a negation: `command_negations` gives the run back when a
        // value follows, so `negations` is zero here and `negate` is the identity.
        if self.value_start() {
            let expression = self.expression()?;
            let guard = self.guard()?;
            return Ok(negate(Executable::Expression { expression, guard }));
        }
        self.pipeline().map(Executable::Pipeline).map(negate)
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
        // `NAME=value …` in front of a command is an environment prefix, as in
        // every other shell. Read here rather than at statement level so it binds
        // to a **stage**: `FOO=1 a | FOO=2 b` gives each side its own, and
        // `FOO=1 a && b` leaves `b` alone.
        //
        // A binding only counts as a prefix when a command follows it. Without
        // that, `x=1` on its own is the assignment it has always been, and the
        // two are told apart by looking rather than by guessing — the bindings
        // are parsed and then given back if nothing follows them.
        let env = self.env_prefix()?;
        let mut items = Vec::new();
        while !self.at_command_end() {
            // An attached `:modifier` outranks the guard keyword too, so
            // `puts if:upper` is an argument rather than `puts` guarded by `:upper`.
            if (self.word("if") || self.word("unless"))
                && !self.carries_attached_modifier()
                && !items.is_empty()
                && self.viable_guard()
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
            } else if self.eat(&TokenKind::HereString).is_some() {
                Some(RedirectKind::HereString)
            } else if self.eat(&TokenKind::GreaterAmp).is_some() {
                // `>&` is two operators wearing one spelling, told apart by the
                // target: `>&2` names a descriptor and duplicates, `>& file`
                // names a path and takes both streams there.
                //
                // The choice is made on the token as written, never after
                // expansion, so a line's meaning cannot depend on a variable's
                // contents. That leaves a computed target genuinely ambiguous —
                // `>&$fd` reads as "duplicate onto $fd" but would silently create
                // a file named `2` — so it is refused rather than guessed. Both
                // meanings have an unambiguous spelling to say instead.
                //
                // Only the *bare* `>&` is ambiguous. An explicit source
                // descriptor (`1>&…`) can only mean duplication, so a computed
                // target is fine there — which is what the message below points
                // at.
                let operator_start = self.tokens[self.position - 1].span.start;
                if self.descriptor_prefix(&items, operator_start).is_some()
                    || self.at_descriptor_word()
                {
                    Some(RedirectKind::DuplicateOut)
                } else if self.at_literal_word() {
                    Some(RedirectKind::Both)
                } else {
                    return Err(self.error(ParseErrorKind::Expected(
                        "a written-out target: `1>&$fd` to duplicate, or `&> $file` for both streams",
                    )));
                }
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
                let fd = match self.descriptor_prefix(&items, operator_start) {
                    Some(text) => {
                        let descriptor = text.parse::<u32>().map_err(|_| {
                            self.error(ParseErrorKind::Expected("a descriptor mesh can redirect"))
                        })?;
                        // Bounded by what a descriptor can be rather than by
                        // what mesh happens to support: the kernel's own limit
                        // is the process's open-file cap, and a number past
                        // `c_int` could not name one at all.
                        if libc::c_int::try_from(descriptor).is_err() {
                            return Err(self.error(ParseErrorKind::Expected(
                                "a descriptor small enough to name one",
                            )));
                        }
                        items.pop();
                        Some(descriptor)
                    }
                    None => None,
                };
                // Phrased as "a redirection …" so the `Expected` rendering reads
                // as a refusal rather than "expected a heredoc".
                if fd.is_some() && kind == RedirectKind::Heredoc {
                    return Err(self.error(ParseErrorKind::Expected(
                        "a redirection with `<<` to feed stdin; it cannot name another descriptor",
                    )));
                }
                if fd.is_some() && kind == RedirectKind::HereString {
                    return Err(self.error(ParseErrorKind::Expected(
                        "a redirection with `<<<` to feed stdin; it cannot name another descriptor",
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
            } else if items
                .iter()
                .any(|item| matches!(item, CommandItem::Word(_)))
                && self.value_argument_starts()
                // A glob's attached `(` opens its qualifiers, not an argument
                // list, so `ls *.rs(f)` must not take the attached-call route
                // that `style(x)` takes. The word branch below reads both.
                && !self.qualified_glob_at(0)
            {
                let start = self.peek().map_or(0, |token| token.span.start);
                // Parsed **above comparison precedence**, so a following `<` / `>` is
                // left for the redirect parser above rather than swallowed as a
                // comparison: `puts (1 + 2) > out` has to write a file, not print
                // `true`. Arithmetic (6) and the postfix forms bind tighter than 4 and
                // are unaffected, and a comparison that really is wanted says so with
                // its own parens — `puts (1 < 2)` reaches a fresh `expression` inside
                // the group and prints `true`. `&&`, `||` and `|` sit below 4 too, so
                // they keep their connector readings for free.
                // A **redirect target** counts as well as a word: `>out$(x)` reaches
                // here with `Redirect` as the last item, so checking only `Word` let the
                // glued spelling through and passed the capture as a separate argument.
                let glued_before = match items.last() {
                    Some(CommandItem::Word(word)) => word.span.end == start,
                    Some(CommandItem::Redirect { target, .. }) => target.span.end == start,
                    _ => false,
                };
                let expression = self.binary(Self::COMPARISON_PRECEDENCE + 1)?;
                let end = self.tokens[self.position - 1].span.end;
                // A value argument is a whole argument, so text touching either side of
                // it is **refused** rather than silently becoming a separate one:
                // `pre$(x)post` would otherwise hand over three arguments where the
                // reader wrote one, and quietly. Gluing a value into a *bare* word is
                // its own change — until then this stays the syntax error it was before
                // value arguments existed, and the message points at `"pre$(…)post"`,
                // the quoted spelling that does interpolate.
                // Only a token that could *continue* an argument counts: a newline or a
                // `;` sits flush against the expression too, and neither is glued text.
                let glued_after = self.peek().is_some_and(|token| {
                    token.span.start == end
                        && matches!(
                            token.value,
                            TokenKind::Word(_) | TokenKind::CaptureStart | TokenKind::LParen
                        )
                });
                if glued_before || glued_after {
                    return Err(ParseError {
                        kind: ParseErrorKind::GluedValueArgument,
                        span: start..end,
                    });
                }
                items.push(CommandItem::Value(Spanned {
                    value: expression,
                    span: start..end,
                }));
            } else {
                let mut word = self.command_word()?;
                // Read after the word rather than before it: `[ab]*(f)` and `*(f)`
                // start on tokens that are not words at all, so which run is a glob
                // is only settled once `command_word` has assembled it.
                if word_globs(&word.value)
                    && let Some(qualifiers) = self.glob_qualifiers(word.span.end)?
                {
                    word.value.qualifiers = Some(qualifiers);
                    word.span.end = self.tokens[self.position - 1].span.end;
                }
                items.push(CommandItem::Word(word));
            }
        }
        if items.is_empty() {
            return Err(self.error(ParseErrorKind::Expected("an empty command in a pipeline")));
        }
        let guard = self.guard()?;
        Ok(Command { items, guard, env })
    }

    /// Does a **value** start here, in an argument position?
    ///
    /// Only ever asked once a `CommandItem::Word` is in hand, since a value is an
    /// *argument*: a command whose first item is a redirection has a non-empty item
    /// list and still no command word, so testing emptiness let `>out f()` take the
    /// call as word zero and run whatever it returned — a syntax error before.
    ///
    /// Deliberately narrower than [`value_start_in`], which weighs a command reading
    /// against a value one for a whole *statement*. Here the command word is already
    /// in hand, so the only question is whether this token can be part of an argument
    /// word at all — and the four shapes below cannot, which is exactly why every one
    /// of them is a syntax error today. Accepting them cannot change what any working
    /// script means.
    ///
    /// `[` and `..` are pointedly absent. Both *are* value syntax elsewhere, but in an
    /// argument they are already text with a job: `[` opens a glob character class
    /// (`ls gt/[ab]*`), and `1..3` is the literal word. Reading either as a value here
    /// would break working scripts, so a list literal or a range in an argument stays
    /// a separate decision.
    fn value_argument_starts(&mut self) -> bool {
        // `$( … )` and `( … )` have no word spelling at all.
        if matches!(
            self.peek().map(|token| &token.value),
            Some(TokenKind::CaptureStart | TokenKind::LParen)
        ) {
            return true;
        }
        // An **attached** call: a command word stops in front of `(`, so `style(x)`,
        // `$f(1)` and `pwd()` are calls rather than words. The spacing is the whole
        // signal, which is why it is asked of the tokens rather than of a parsed tree.
        if let (Some(word), Some(next)) = (self.peek(), self.tokens.get(self.position + 1))
            && matches!(word.value, TokenKind::Word(_))
            && matches!(next.value, TokenKind::LParen)
            && next.span.start == word.span.end
        {
            return true;
        }
        // An attached modifier chain is a value, not a word, so `puts $env:get(EDITOR,
        // vim)` and `puts abc:upper` both read in command position exactly as they do
        // on the right of an `=`. A command word stops in front of a `(`, so without
        // this an argument list would arrive glued on as a separate argument.
        //
        // One `:name` is enough — a trailing `(` is *not* required. Requiring it split
        // a chain by whether its last step happened to take arguments, so
        // `puts abc:stripend("c")` was `ab` while `puts abc:upper` was the literal text
        // `abc:upper`. Only a *literal* subject was ever affected: `$x:upper` carries
        // its chain on the `VarRef` and expansion applies it (`expand.rs:863`).
        //
        // Asked of the tokens, like the attached-call check above, because the signal
        // is the shape of the run rather than anything a parsed word records.
        // A leading `...` is deliberately *not* skipped here: `CommandItem::Value`
        // has no spread variant, so routing `...$x:split(":")` through the
        // expression parser would build a `UnaryOp::Spread` nothing consumes and
        // pass one list where the reader asked for its elements. Left as the syntax
        // error it already was until the spread reaches the value-argument path.
        if matches!(
            self.peek().map(|token| &token.value),
            Some(TokenKind::Word(_))
        ) {
            // **Every** step of the run has to abut the one before it. In command
            // position spacing is what separates one argument from the next, so a
            // gap anywhere ends the chain: `puts $x :upper(lo)` is the word `$x`
            // and a separate `:upper(lo)` modifier-reference call, and
            // `puts $x:upper (1)` is a chain and a separate `(1)`. Merging across
            // either gap would take an argument the reader gave to `puts`.
            let abuts = |this: &Self, offset: usize| {
                this.tokens
                    .get(this.position + offset)
                    .zip(this.tokens.get(this.position + offset - 1))
                    .is_some_and(|(token, previous)| token.span.start == previous.span.end)
            };
            if matches!(
                self.tokens.get(self.position + 1).map(|t| &t.value),
                Some(TokenKind::Colon)
            ) && abuts(self, 1)
                && self.word_text_at(2).is_some()
                && abuts(self, 2)
            {
                return true;
            }
        }
        // An attached modifier-reference call — `:exists("Cargo.toml")` — the same
        // form `value_start_in` admits at the start of a statement. A bare `:name`
        // stays a word, so `puts :stem` keeps its reading.
        self.same(&TokenKind::Colon)
            && self.modifier_ref_name().is_some()
            && self
                .tokens
                .get(self.position + 1)
                .zip(self.tokens.get(self.position + 2))
                .is_some_and(|(name, next)| {
                    matches!(next.value, TokenKind::LParen) && name.span.end == next.span.start
                })
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
        let pieces = merge_command_variable_access(pieces).map_err(|kind| ParseError {
            kind,
            // The merge works on piece text, which carries no offsets of its own, so
            // the word is the narrowest span honestly available.
            span: start..end,
        })?;
        Ok(Spanned {
            value: Word {
                pieces,
                qualifiers: None,
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

    /// Is this `alias NAME = …`? The shape that claims the word, checked before
    /// committing so `alias`, `alias --help`, and `alias = 1` keep their ordinary
    /// readings. bash's unspaced `alias NAME=VALUE` is *not* matched here: it
    /// tokenizes as one word, and [`alias_def`](Self::alias_def) reports it with
    /// the spelling mesh wants rather than letting it fall through to a
    /// command-not-found.
    fn alias_definition_follows(&self) -> bool {
        let mut index = self.position + 1;
        let Some(first) = self.tokens.get(index) else {
            return false;
        };
        // Wide enough to claim every *attempt* at a definition, which is a
        // different question from whether the name is any good — that one belongs
        // to [`definition_name`](Self::definition_name), and it has to be reached
        // to be asked. So a **quoted** word is claimed here and rejected there:
        // `alias "foo" = …` is unmistakably an alias being defined, and answering
        // `expected a name` says what is wrong with it, where declining leaves the
        // far less useful `command not found: alias`.
        if !matches!(first.value, TokenKind::Word(_)) && name_piece(&first.value, true).is_none() {
            return false;
        }
        // The name may be a run the lexer split — `a.b` is `a` `.` `b` — and the
        // definition has to claim those too, or a bad name would report the far
        // less useful `command not found: alias` instead of what is wrong with it.
        //
        // A quoted piece is claimed here for the same reason the first one is:
        // `alias ."foo" = …` is a definition being attempted, and stopping the scan
        // at the quote loses the `=` that says so.
        let mut end = first.span.end;
        index += 1;
        while let Some(next) = self.tokens.get(index)
            && next.span.start == end
            && (matches!(next.value, TokenKind::Word(_)) || name_piece(&next.value, true).is_some())
        {
            end = next.span.end;
            index += 1;
        }
        matches!(
            self.tokens.get(index).map(|t| &t.value),
            Some(TokenKind::Equal)
        )
    }

    /// `alias NAME = COMMAND [ARG …]` — the terse forwarding wrapper.
    ///
    /// Sugar over [`wrapper func`](Self::function), not a mechanism of its own:
    /// it builds exactly the definition you would have written by hand,
    ///
    /// ```text
    /// alias co = vcs checkout
    /// wrapper func co(...args) { vcs checkout ...$args }
    /// ```
    ///
    /// so an alias resolves, scopes and takes arguments like any function, and
    /// there is no second resolution stage and no parse-time textual expansion —
    /// the things `DESIGN.md` dropped when it dropped the alias *mechanism*.
    ///
    /// **A self-naming alias reaches the program, not itself.** `alias grep =
    /// grep --color=auto` is the commonest alias there is, and desugared
    /// literally it would recurse forever, so a first word equal to the alias's
    /// own name is emitted as `command grep` — the same escape `func ls() {
    /// command ls --color=auto }` uses, and the same no-self-expansion rule bash
    /// applies to aliases.
    fn alias_def(&mut self) -> Result<Executable, ParseError> {
        let start = self.peek().map(|t| t.span.start).unwrap_or_default();
        self.take_word("alias");
        // An alias desugars to a `wrapper func`, so it is named by the same rules
        // and reports them the same way: read here, judged where the definition
        // runs.
        let name = self.definition_name(true)?;
        self.expect(&TokenKind::Equal, "`=`")?;
        self.newlines();
        let mut command = self.command()?;
        if command.items.is_empty() {
            return Err(self.error(ParseErrorKind::Expected("a command after `alias NAME =`")));
        }
        if command.guard.is_some() {
            return Err(self.error(ParseErrorKind::Expected(
                "a plain command after `alias NAME =`; a guard belongs in a `wrapper func` body",
            )));
        }
        // `alias ll = 'ls -l'` is the bash reflex, and here the quotes make one
        // word: it would look for a program whose name contains a space. Caught
        // by hand rather than left to `command not found`, which reports the odd
        // name without saying the quotes are what did it.
        if let Some(CommandItem::Word(first)) = command.items.first()
            && let Some((text, quote)) = single_text(&first.value)
            && quote != QuoteMode::Bare
            && text.contains(char::is_whitespace)
        {
            return Err(self.error(ParseErrorKind::QuotedAliasCommand(text.to_owned())));
        }
        let end = self.peek().map(|t| t.span.start).unwrap_or(start);
        let span = start..end;

        // A first word naming the alias itself means the program, not a call
        // back into this definition. Quoting is not part of the question: a
        // quoted command head still resolves functions, so `alias true = "true"`
        // would recurse to the stack limit without this.
        if let Some(CommandItem::Word(first)) = command.items.first()
            && single_text(&first.value).is_some_and(|(text, _)| text == name)
        {
            command.items.insert(
                0,
                CommandItem::Word(Spanned {
                    value: Word {
                        pieces: vec![WordPiece::Text {
                            text: "command".to_owned(),
                            quote: QuoteMode::Bare,
                        }],
                        qualifiers: None,
                    },
                    span: span.clone(),
                }),
            );
        }

        // `...$args`, the forwarded rest — built rather than parsed, since there
        // is no source text for it. A command-position spread is a *word* whose
        // pieces are the bare `...` and the variable, which is the shape
        // `expand::spread_var` recognizes; a `UnaryOp::Spread` expression is the
        // value-position form and reaches argv as an unspreadable list.
        command.items.push(CommandItem::Word(Spanned {
            value: Word {
                pieces: vec![
                    WordPiece::Text {
                        text: "...".to_owned(),
                        quote: QuoteMode::Bare,
                    },
                    // Stored with the `$`, as the lexer stores it: the raw name
                    // is what the job list renders a backgrounded call from.
                    WordPiece::Variable {
                        name: format!("${ALIAS_REST}"),
                        quote: QuoteMode::Bare,
                    },
                ],
                qualifiers: None,
            },
            span: span.clone(),
        }));

        Ok(Executable::Function {
            name,
            parameters: vec![Param {
                name: ALIAS_REST.to_owned(),
                kind: ParamKind::Rest,
            }],
            body: Source {
                statements: vec![Statement {
                    and_or: AndOr {
                        first: Executable::Pipeline(Pipeline {
                            stages: vec![command],
                            pipe_stderr: Vec::new(),
                        }),
                        rest: Vec::new(),
                    },
                    background: false,
                    span: span.clone(),
                }],
                span,
            },
            wrapper: true,
        })
    }

    fn function(&mut self, wrapper: bool) -> Result<Executable, ParseError> {
        self.take_word("func");
        // Taken as written and judged when the definition runs — see
        // [`definition_name`](Self::definition_name). The rules themselves (no
        // reserved word, no built-in value call, no dotted name) live at the one
        // runtime check in `repl.rs`.
        let name = self.definition_name(false)?;
        let parameters = self.parameters()?;
        // "Parses no flags of its own" is the whole content of the marker, so a
        // declared flag contradicts it: help would list the flag and completion
        // offer it while every command-position `--flag` went to `...rest`.
        if wrapper && let Some(flag) = parameters.iter().find(|p| p.kind.is_option()) {
            return Err(self.error(ParseErrorKind::WrapperDeclaresFlag(flag.name.clone())));
        }
        self.newlines();
        let body = self.block()?;
        Ok(Executable::Function {
            name,
            parameters,
            body,
            wrapper,
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
                // Counted, because an `else if` chain recurses *here* and nowhere
                // the other counters can see: `then_body` above has already given
                // its level back by the time this runs, so a chain of them would
                // otherwise descend at a constant depth of zero.
                ElseBranch::If(Box::new(self.deeper(Self::if_expr)?))
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

    fn fork_expr(&mut self) -> Result<Executable, ParseError> {
        self.take_word("fork");
        self.newlines();
        let body = self.block()?;
        Ok(Executable::Fork { body })
    }

    /// `with NAME=value NAME2=value2 … { … }`.
    ///
    /// Each binding is written **unspaced**, as the one-command prefix form in
    /// other shells is and as mesh's own would be. That is what lets a header
    /// hold several of them without the reader having to guess where one value
    /// ends: in `with FOO=a b { … }` the `b` cannot be part of `FOO`, so it is
    /// reported rather than absorbed.
    fn with_expr(&mut self) -> Result<Executable, ParseError> {
        self.take_word("with");
        let mut bindings = Vec::new();
        while let Some(binding) = self.env_binding()? {
            bindings.push(binding);
        }
        if bindings.is_empty() {
            return Err(self.error(ParseErrorKind::Expected("a `NAME=value` after `with`")));
        }
        self.newlines();
        let body = self.block()?;
        Ok(Executable::With { bindings, body })
    }

    /// One unspaced `NAME=value` / `NAME+=value`, or `None` where the header ends.
    fn env_binding(&mut self) -> Result<Option<EnvBinding>, ParseError> {
        if !self.env_binding_follows(0) {
            return Ok(None);
        }
        // `env_binding_follows` asks only about the *shape* — a word with an
        // attached `=` — because that is what tells the statement from a command of
        // the same name. Whether the word is a usable name is this function's
        // question, and both answers are reachable from ordinary input: `with "A"=x`
        // is quoted, so it is not the text it looks like, and `with 1=x` is not a
        // name at all. Both are reported; treating the shape check as a guarantee
        // here crashed the shell on them.
        let key = self
            .word_text_at(0)
            .map(str::to_owned)
            .filter(|key| valid_name(key));
        let Some(key) = key else {
            return Err(self.error(ParseErrorKind::Expected(
                "a valid NAME before `=` in a `with` header",
            )));
        };
        self.position += 1;
        let append = self.eat(&TokenKind::PlusEqual).is_some();
        if !append {
            self.expect(&TokenKind::Equal, "`=`")?;
        }
        // `FOO= cmd` is the empty value, as it is in every other shell: the
        // operator is there, so the name is being set, and what follows is the
        // next binding or the block rather than this one's value. Told apart by
        // spacing, like every other attachment rule here.
        let value = if self.peek().is_some_and(|token| {
            token.span.start == self.previous_end() && !matches!(token.value, TokenKind::LBrace)
        }) {
            self.expression()?
        } else {
            Expr::Scalar(Spanned {
                value: Word {
                    pieces: vec![WordPiece::Text {
                        text: String::new(),
                        quote: QuoteMode::Double,
                    }],
                    qualifiers: None,
                },
                span: self.previous_end()..self.previous_end(),
            })
        };
        Ok(Some(EnvBinding { key, append, value }))
    }

    /// The `NAME=value …` run in front of a command, or empty if there is none.
    ///
    /// Speculative: the bindings are parsed and then **given back** unless a
    /// command follows them, because `x=1` alone is an assignment and `x=1 cmd`
    /// is a prefix, and only what comes after tells them apart. Rewinding is what
    /// keeps that decision from being a guess about where a value ends — the
    /// value has actually been parsed by the time the question is asked.
    ///
    /// Note which namespace this writes. A prefix sets the **environment**, since
    /// its whole purpose is what the child inherits, while `x=1` alone binds a
    /// shell variable a child never sees. The same spelling, two namespaces —
    /// deliberate, and the reason `TODO.md` carries the question of whether a
    /// bare `FOO=bar` should mean the environment too.
    /// Is a `NAME=value …` run here followed by a command?
    ///
    /// Non-consuming: the bindings are parsed to find where they end — the only
    /// honest way to know, since a value can be a call or a capture — and the
    /// position is always given back. `command` parses them again for real.
    fn env_prefix_follows(&mut self) -> Result<bool, ParseError> {
        if !self.env_binding_follows(0) {
            return Ok(false);
        }
        let start = self.position;
        let mut sound = true;
        while self.env_binding_follows(0) {
            // A malformed binding is not this question's business — say "not a
            // prefix" and let the assignment path produce the diagnostic it
            // always did, rather than reporting from inside a lookahead.
            if self.env_binding().is_err() {
                sound = false;
                break;
            }
        }
        let prefix = sound && !self.at_command_end();
        self.position = start;
        Ok(prefix)
    }

    fn env_prefix(&mut self) -> Result<Vec<EnvBinding>, ParseError> {
        if !self.env_binding_follows(0) {
            return Ok(Vec::new());
        }
        let start = self.position;
        let mut bindings = Vec::new();
        while let Some(binding) = self.env_binding()? {
            bindings.push(binding);
        }
        // A command has to follow, on this line. At the end of a statement the
        // run was an assignment after all, so hand the tokens back untouched.
        if self.at_command_end() {
            self.position = start;
            return Ok(Vec::new());
        }
        Ok(bindings)
    }

    /// Is the run at `offset` an **unspaced** `NAME=` / `NAME+=`? The signal a
    /// `with` header is made of, and what tells the statement from a command of
    /// the same name — `with --help` and `with somewhere` stay commands.
    fn env_binding_follows(&self, offset: usize) -> bool {
        let Some(name) = self.tokens.get(self.position + offset) else {
            return false;
        };
        if !matches!(name.value, TokenKind::Word(_)) {
            return false;
        }
        self.tokens
            .get(self.position + offset + 1)
            .is_some_and(|operator| {
                matches!(operator.value, TokenKind::Equal | TokenKind::PlusEqual)
                    && operator.span.start == name.span.end
            })
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
            self.expect(&TokenKind::FatArrow, "`=>`")?;
            self.newlines();
            // `=> { … }` is a block in statement context; anything else is a value
            // expression, where a bare word is a string.
            let body = if self.same(&TokenKind::LBrace) {
                MatchBody::Block(self.block()?)
            } else {
                MatchBody::Value(self.expression()?)
            };
            arms.push(MatchArm {
                pattern,
                guard,
                body,
            });
            if self.at_end() {
                return Err(self.eof(ParseErrorKind::Unterminated('{')));
            }
            // Arms are terminator-separated: a newline or `;`, never a comma. The
            // separator is required, so `a => 1 b => 2` does not parse.
            if self.terminators() == 0 && !self.same(&TokenKind::RBrace) {
                return Err(self.error(ParseErrorKind::Expected("a newline or `;`")));
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
            self.regex_slot = true;
            let pattern = self.expression();
            self.regex_slot = false;
            Ok(MatchPattern::Value(match_pattern_operand(pattern?)))
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
    /// The bare run of digits abutting the operator, if the last item is one —
    /// the `2` in `2> file`. Shared so the `>&` reading and the descriptor prefix
    /// itself agree on what counts.
    fn descriptor_prefix<'a>(
        &self,
        items: &'a [CommandItem],
        operator_start: usize,
    ) -> Option<&'a str> {
        match items.last() {
            Some(CommandItem::Word(word)) if word.span.end == operator_start => {
                match word.value.pieces.as_slice() {
                    [
                        WordPiece::Text {
                            text,
                            quote: QuoteMode::Bare,
                        },
                    ] if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) => Some(text),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Is the next token spelled out in full — text only, with nothing that has
    /// to be expanded to know what it says?
    ///
    /// Quoting counts as literal: `>& "2"` names a file, because the quotes are
    /// the user saying so.
    fn at_literal_word(&self) -> bool {
        self.peek().is_some_and(|token| match &token.value {
            TokenKind::Word(word) => {
                !word.pieces.is_empty()
                    && word
                        .pieces
                        .iter()
                        .all(|piece| matches!(piece, WordPiece::Text { .. }))
            }
            _ => false,
        })
    }

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

    /// A bare `$env.KEY` or `$env[…]` in assignment position, consumed on a match.
    ///
    /// **Exactly one access.** `$env` alone names the whole namespace, and a
    /// second access or a modifier (`$env.PATH[0]`, `$env.PATH:dedup`) describes a
    /// derived value rather than a place, so none of them is a target. Those fall
    /// through and parse as ordinary expressions, which is where their real error
    /// message comes from.
    ///
    /// A **member** is checked here with the same rule reads use, so anything
    /// spellable as `$env.KEY` is also assignable — including a kebab name like
    /// `$env.MY-VAR`, which the environment permits and mesh can read. A
    /// **subscript** names the entry its text resolves to, which is only known
    /// once it is evaluated, so the text rides along and `environ` does the
    /// checking. That is what gives the computed read `$env[$name]` a writing
    /// twin.
    ///
    /// A subscript holding `..` is refused, because that is how
    /// `expansion_variable` tells a slice from a key: a slice names a copy of a
    /// run of entries, not a place. It costs a literal key that contains `..`,
    /// which the read side cannot spell either.
    fn env_target(&mut self) -> Option<EnvKey> {
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
        let rest = name
            .strip_prefix("${")
            .and_then(|value| value.strip_suffix('}'))
            .or_else(|| name.strip_prefix('$'))?
            .strip_prefix("env")?;
        let key = if let Some(member) = rest.strip_prefix('.') {
            if !valid_name(member) {
                return None;
            }
            EnvKey::Literal(member.to_owned())
        } else if rest.starts_with('[') && subscript_end(rest) == Some(rest.len()) {
            let subscript = &rest[1..rest.len() - 1];
            if subscript.contains("..") {
                return None;
            }
            EnvKey::Computed(subscript.to_owned())
        } else {
            return None;
        };
        self.next();
        Some(key)
    }

    /// A `$name.member` / `$name[index]` **place** for an assignment, handed on as
    /// the raw reference text for the expansion layer to split.
    ///
    /// Requires at least one access: a place is what `$m.key` names, while a bare
    /// `$m` on the left of `=` is not how mesh spells a rebinding (`m = …` is).
    /// Refuses a trailing modifier, since `$xs:dedup` describes a derived value and
    /// not somewhere to store one.
    ///
    /// `$env` is excluded, keeping the byte-boundary rules `env_target` above
    /// applies to it. `$sh` is **not**: `$sh.options.KEY = …` is a real place, and
    /// which of the namespace's entries may be written is a runtime question — the
    /// answer differs per key — so it belongs where the write happens rather than
    /// in the grammar. Refusing `$sh` here made every `$sh.x = …` a syntax error
    /// about the `=`, which named neither the entry nor why it was refused.
    fn member_target(&mut self) -> Option<String> {
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
        let inner = name
            .strip_prefix("${")
            .and_then(|value| value.strip_suffix('}'))
            .or_else(|| name.strip_prefix('$'))?;
        let root_end = inner.find(['.', '[', ':'])?;
        let root = &inner[..root_end];
        if !valid_name(root) || root == "env" {
            return None;
        }
        // Walk the accesses structurally rather than scanning for a `:`: a colon
        // *inside* a subscript belongs to the key (`$m["a:b"]`, which reads fine),
        // so brackets are skipped whole and only a `:` between accesses is the
        // modifier that disqualifies a place.
        let mut rest = &inner[root_end..];
        while !rest.is_empty() {
            if rest.starts_with('[') {
                rest = &rest[subscript_end(rest)?..];
                continue;
            }
            // A `:` here is a modifier, and a modifier names a derived value rather
            // than a place; anything else is not a shape this recognizes.
            let member = rest.strip_prefix('.')?;
            let end = member.find(['.', '[', ':']).unwrap_or(member.len());
            rest = &member[end..];
        }
        let target = name.clone();
        self.next();
        Some(target)
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
        } else if self.take_word("fail") {
            ControlKind::Fail
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

    /// Counted like [`Parser::primary`], and separately from it, because a
    /// statement-position `if` never descends through `primary` — it is read by
    /// the statement path directly, so guarding the expression side alone left
    /// `if true { if true { … } }` aborting at a few thousand levels. An
    /// expression-position `if` passes both counters and so is held to half the
    /// depth, which is the right way round: its frames are the larger pair.
    fn block(&mut self) -> Result<Source, ParseError> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error(ParseErrorKind::TooDeep));
        }
        let body = self.source(Some(TokenKind::RBrace));
        self.depth -= 1;
        body
    }

    fn expression(&mut self) -> Result<Expr, ParseError> {
        self.or_expression()
    }

    /// Step over a newline inside `( … )` or `${ … }` when what follows it
    /// continues the expression.
    ///
    /// Those constructs hold one expression, so a newline in them separates
    /// nothing and is layout — `(1` / `+ 2)` is `(1 + 2)`, which is what
    /// `docs/REFERENCE.md` has always said and what a group already did for a
    /// newline right after the `(`, right before the `)`, or straight after an
    /// operator. Only the operator-*leading* line was left out, and it is the one
    /// spelling a wrapped sum is usually written in.
    ///
    /// **Speculative**: the newlines go back unless `continues` finds what it was
    /// looking for. Consuming them unconditionally would let an unclosed group eat
    /// the lines after it and report far from the `(`, where `(1` followed by an
    /// ordinary statement should still say `expected `)`` right there.
    fn wraps(&mut self, continues: impl FnOnce(&Self) -> bool) -> bool {
        if self.grouped == 0 || !self.same(&TokenKind::Newline) {
            return false;
        }
        let newline = self.position;
        self.newlines();
        if continues(self) {
            return true;
        }
        self.position = newline;
        false
    }

    /// Would an operator here extend an expression being parsed at `minimum`?
    ///
    /// Precedence is part of the question: a weaker operator belongs to an outer
    /// call, which asks again and takes the newline itself, so answering "no" here
    /// hands it over intact rather than eating the layout on its behalf.
    fn continues_binary(&self, minimum: u8) -> bool {
        if minimum <= 5 && (self.same(&TokenKind::Range) || self.same(&TokenKind::RangeInclusive)) {
            return true;
        }
        self.binary_op()
            .is_some_and(|(_, precedence, _)| precedence >= minimum)
    }

    fn or_expression(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.and_expression()?;
        loop {
            self.wraps(|parser| parser.word("or"));
            if !self.take_word("or") {
                break;
            }
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
        loop {
            self.wraps(|parser| parser.word("and"));
            if !self.take_word("and") {
                break;
            }
            self.newlines();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinaryOp::And,
                right: Box::new(self.not_expression()?),
            };
        }
        Ok(left)
    }

    /// Steps over the run of `not`s in a loop and folds it to its **parity** rather
    /// than recursing once per word and stacking one node each.
    ///
    /// `not` yields a bool from the operand's truthiness, so every `not` past the
    /// second only flips a bool that is already there: `not not not $x` is `not $x`,
    /// and any even run is the `not not $x` that coerces without inverting. Two nodes
    /// carry both readings, so the fold is the same tree to any observer.
    ///
    /// Depth is the reason. A word of `not` costs a parse frame, an eval frame, and a
    /// `Drop` frame, so a generated or pasted line of thousands of them aborted the
    /// shell — by signal, before it could report anything. It survived only while a
    /// lookahead concluded such lines were *commands* and never built the chain;
    /// reserving the word took that away, and folding replaces it with something that
    /// does not depend on where the line sits.
    fn not_expression(&mut self) -> Result<Expr, ParseError> {
        let mut negations = 0_usize;
        // `not:upper` is a chain on the text `not`, not a negation of `:upper`.
        while !self.carries_attached_modifier() && self.take_word("not") {
            negations += 1;
        }
        let mut expression = self.binary(4)?;
        for _ in 0..negations.min(2 - negations % 2) {
            expression = Expr::Unary {
                op: UnaryOp::Not,
                expression: Box::new(expression),
            };
        }
        Ok(expression)
    }

    /// A leading `not` run that negates a **command's** status, counted and consumed;
    /// `0` when the word is the value operator instead, with the position rewound.
    ///
    /// `not` is reserved, so it never names a command — but it only ever started a
    /// **value**, which left "this command failed" with no spelling at all: the
    /// command had to run as a statement, `$sh.status` be read into a name, and the
    /// `if` test that. A `not` whose operand is not a value negates the operand's
    /// *status* instead, so `not test -f x` reads as it does in every other shell.
    ///
    /// Which reading applies is decided by the same [`value_start_in`] the position
    /// already uses, asked one token later: `not $ready` and `not have(x)` are values
    /// and rewind to the expression parser, `not ls` is a command. Nothing that
    /// parsed as a value before parses as a command now, since the discriminator is
    /// the one that was already there.
    ///
    /// The run is counted here rather than folded by the caller because `not not cmd`
    /// has to cancel *before* the operand is read: only the whole run says whether a
    /// value follows it.
    ///
    /// [`value_start_in`]: Parser::value_start_in
    fn command_negations(&mut self, in_condition: bool) -> usize {
        let start = self.position;
        // `not:upper` is a chain on the text `not`, as in `not_expression`.
        let mut negations = 0_usize;
        while !self.carries_attached_modifier() && self.take_word("not") {
            negations += 1;
        }
        if negations == 0 {
            return 0;
        }
        // A **list-pattern binding** is asked for first, because `value_start_in`
        // answers for the `[` alone and a list literal really is a value: without
        // this, `not [head ...tail] = $xs` rewinds and the negation is lost, while
        // the un-negated binding is a condition that works. The `=` is what tells
        // the two apart, so the trial parse looks for it rather than for the
        // bracket.
        //
        // Otherwise a command needs a **word** to name it — or a leading `...`, the
        // forwarded-rest command, whose head supplies the word at expansion time.
        // A value operand belongs to the expression parser. Either way the whole run
        // is given back, so the negations are folded in one place rather than two —
        // and `not = 5` stays the syntax error it already was rather than becoming
        // `command not found: =`.
        //
        // `value_start_in` is what keeps the two positions apart for a spread, so
        // this needs no `in_condition` test of its own: in a statement it declines
        // the `...` and the negation lands on the command, while in a condition it
        // claims it and the run rewinds to the `a list is not a condition` refusal
        // that reading still gets. Raised in review — the word-only test read
        // `not ...$xs` as a value negation and never invoked the list.
        let negates = if self.same(&TokenKind::LBracket) {
            self.binding_assignment_follows()
        } else {
            matches!(
                self.peek().map(|token| &token.value),
                Some(TokenKind::Word(_) | TokenKind::Spread)
            ) && !self.value_start_in(in_condition)
        };
        if !negates {
            self.position = start;
            return 0;
        }
        negations
    }

    /// Does a binding pattern followed by `=` / `+=` start here? A trial parse,
    /// rewound either way — only a full parse of the pattern says where the operator
    /// after it sits, since a list pattern nests and `...rest` spans.
    fn binding_assignment_follows(&mut self) -> bool {
        let start = self.position;
        let follows = self.binding_pattern().is_ok()
            && matches!(
                self.peek().map(|token| &token.value),
                Some(TokenKind::Equal | TokenKind::PlusEqual)
            );
        self.position = start;
        follows
    }

    /// A condition, with any leading `not` run folded in — see [`command_negations`].
    ///
    /// [`command_negations`]: Parser::command_negations
    fn condition(&mut self) -> Result<Executable, ParseError> {
        let negations = self.command_negations(true);
        let condition = self.bare_condition()?;
        Ok(if negations % 2 == 1 {
            Executable::Not(Box::new(condition))
        } else {
            condition
        })
    }

    /// The condition proper: an assignment, a value, or a command.
    fn bare_condition(&mut self) -> Result<Executable, ParseError> {
        let start = self.position;
        // A condition is a command position too, so `if FOO=x cmd { … }` is the
        // prefixed command it reads as. Asked before the assignment path below for
        // the same reason `executable` does: that path claims `FOO=x` and leaves
        // `cmd` where the `{` belongs, which reported a syntax error on a form that
        // parsed fine one line up. Raised in review as a P1.
        if self.env_prefix_follows()? {
            return self.pipeline().map(Executable::Pipeline);
        }
        // `not` never opens a binding, since it is reserved — see `value_start`. Left
        // out, `if not = 5` would bind a variable whose name can never be spoken in
        // command position again, the way `func = 5` and `return = 6` already refuse to.
        // As in statement position: `if _ = f() { … }` is the discard used as a
        // place, and reaches no probe below.
        if self.word("_") && self.assignment_follows(1) {
            return Err(self.error(ParseErrorKind::DiscardAssignment));
        }
        if !self.word("not") && (self.word_text_at(0).is_some() || self.same(&TokenKind::LBracket))
        {
            if let Ok(pattern) = self.binding_pattern()
                && self.eat(&TokenKind::Equal).is_some()
            {
                return Ok(Executable::Assignment {
                    global: false,
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

    /// The precedence of the comparison operators in [`Parser::binary_op`]'s table —
    /// `==`, `!=`, `<`, `<=`, `>`, `>=`, `~`, `!~`.
    ///
    /// Named because a command **argument** is parsed just above it, which is what keeps
    /// a following `<` / `>` a redirection instead of a comparison.
    const COMPARISON_PRECEDENCE: u8 = 4;

    fn binary(&mut self, minimum: u8) -> Result<Expr, ParseError> {
        let mut left = self.prefix()?;
        let mut compared = false;
        // A leading `..` is read whole by `primary`, so the first operand can
        // already *be* a range without the loop having built one. Seeding from it
        // is what makes `..1 .. 2` answer like `1 .. 2 .. 3` instead of slipping
        // past a counter that only watches the loop.
        let mut ranged = matches!(left, Expr::Range { .. });
        loop {
            self.wraps(|parser| parser.continues_binary(minimum));
            if minimum <= 5
                && (self.same(&TokenKind::Range) || self.same(&TokenKind::RangeInclusive))
            {
                // Reported before the operator is consumed, so the span covers the
                // `..` that cannot be there rather than whatever follows it.
                if ranged {
                    return Err(self.error(ParseErrorKind::ChainedRange));
                }
                let inclusive = self.eat(&TokenKind::RangeInclusive).is_some();
                if !inclusive {
                    self.position += 1;
                }
                self.newlines();
                let end = if self.at_expression_end() {
                    None
                } else {
                    Some(Box::new(self.range_endpoint()?))
                };
                left = Expr::Range {
                    start: Some(Box::new(left)),
                    end,
                    inclusive,
                };
                ranged = true;
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
            self.regex_slot = matches!(op, BinaryOp::Match | BinaryOp::NotMatch);
            let mut right = self.binary(precedence + 1)?;
            self.regex_slot = false;
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

    /// Counted around the *recursive* calls rather than around the whole function,
    /// which matters: `prefix` is on the path to every operand, so counting on
    /// entry would spend a level on each one and halve what the limit means for
    /// ordinary nesting. A prefix chain is the only thing here that recurses, so
    /// it is the only thing that costs.
    fn prefix(&mut self) -> Result<Expr, ParseError> {
        if self.operator("-") {
            let minus = self.peek().expect("the operator just peeked").span.clone();
            self.position += 1;
            let operand = self.deeper(Self::prefix)?;
            if let Some(literal) = negative_literal(&minus, &operand) {
                return Ok(literal);
            }
            return Ok(Expr::Unary {
                op: UnaryOp::Negate,
                expression: Box::new(operand),
            });
        }
        if self.eat(&TokenKind::Spread).is_some() {
            return Ok(Expr::Unary {
                op: UnaryOp::Spread,
                expression: Box::new(self.deeper(Self::prefix)?),
            });
        }
        self.postfix()
    }

    /// Run `step` one level deeper, reporting [`ParseErrorKind::TooDeep`] rather
    /// than descending past [`MAX_DEPTH`].
    ///
    /// The depth is left incremented when the limit is hit, on purpose: the error
    /// unwinds the whole descent, so nothing below will parse anyway, and putting
    /// the count back would only let a sibling construct start another run just as
    /// deep.
    fn deeper<T>(&mut self, step: fn(&mut Self) -> Result<T, ParseError>) -> Result<T, ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error(ParseErrorKind::TooDeep));
        }
        let parsed = step(self);
        self.depth -= 1;
        parsed
    }

    /// An **operand**, so [`Parser::grouped`] does not apply inside it.
    ///
    /// This is the one place the group's "a newline is layout" rule is handed off.
    /// Everything a wrapped expression can descend into is reached from here — the
    /// `[ … ]` and `{ … }` a `primary` opens, the `( … )` argument lists this loop
    /// reads — and every one of them holds *several* things, so a newline in one
    /// separates rather than wraps. Clearing at the single point they share beats
    /// clearing at each, which is how `([1 ⏎ + 2])` came to be the one-element
    /// `[3]` while the identical `[1 ⏎ + 2]` was a syntax error. Raised in review
    /// as a P2, against the list; call arguments had it too.
    ///
    /// A `( … )` written *inside* one of them turns it back on, since that arm
    /// sets the count itself — `[1, (2 ⏎ + 3)]` is two elements, the second
    /// wrapped. Restored on the way out, so the operator loop that called this
    /// still sees the group it is in.
    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let grouped = std::mem::take(&mut self.grouped);
        let value = self.postfix_operand();
        self.grouped = grouped;
        value
    }

    fn postfix_operand(&mut self) -> Result<Expr, ParseError> {
        let mut value = self.primary()?;
        // A glob's attached `(…)` is its qualifier list, so it is read before the
        // call loop below ever sees the `(` — `*(d)` narrows the glob rather than
        // calling its matches. Once, since qualifiers do not chain.
        if let Expr::Scalar(word) = &mut value
            && word_globs(&word.value)
            && let Some(qualifiers) = self.glob_qualifiers(word.span.end)?
        {
            word.value.qualifiers = Some(qualifiers);
            word.span.end = self.tokens[self.position - 1].span.end;
        }
        loop {
            // A `(` after a **modifier chain** has to abut it, so command position
            // keeps `puts $x:upper (1)` a chain plus a separate `(1)` argument rather
            // than calling the chain's result. Narrow to `Expr::Modifier` on purpose:
            // a modifier yields a string, list or bool and is never callable, so
            // nothing legal is refused, while `y = f (1)` keeps its spacing freedom.
            if self.same(&TokenKind::LParen)
                && (!matches!(value, Expr::Modifier { .. })
                    || self
                        .peek()
                        .is_some_and(|token| token.span.start == self.previous_end()))
            {
                self.position += 1;
                self.newlines();
                value = Expr::Call {
                    callee: Box::new(value),
                    // Counted, for the same reason the `else if` arm is: `primary`
                    // has already given its level back by the time the trailer loop
                    // runs, so `f(f(f(…)))` would otherwise nest at a constant depth
                    // of zero. Counted here rather than around the whole loop so
                    // that a trailer which does *not* descend — `a.b`, or a chain of
                    // indexes — stays free, and ordinary nesting keeps its full
                    // budget.
                    arguments: self.deeper(Self::arguments)?,
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
                let index = self.deeper(Self::expression)?;
                self.newlines();
                self.expect(&TokenKind::RBracket, "`]`")?;
                value = Expr::Index {
                    value: Box::new(value),
                    index: Box::new(index),
                };
            } else if self.same(&TokenKind::Colon)
                && self.word_text_at(1).is_some()
                // Both halves have to **abut**, which is what keeps a map literal's
                // `key: value` out of the chain: `[host: upper]` is a map, not the
                // string `HOST`. Without it any map whose value word happened to name
                // a modifier was silently read as a chain on the key — `[host: len]`
                // gave `4`, `[host: keys]` an error — and the wrongness scaled with
                // the vocabulary rather than being one reserved word.
                && self
                    .peek()
                    .is_some_and(|colon| colon.span.start == self.previous_end())
                && self
                    .tokens
                    .get(self.position + 1)
                    .zip(self.peek())
                    .is_some_and(|(name, colon)| name.span.start == colon.span.end)
            {
                self.position += 1;
                let start = self.previous_end();
                let name = self.name()?;
                // `:` + identifier is reserved by the grammar, so a name the
                // vocabulary does not hold is an error rather than falling back to
                // literal text. A name it *does* hold but the engine cannot apply yet
                // (`:sort`) parses fine and reports at run time, which is why this
                // asks `modifier_name` rather than whether it can be applied.
                if !modifier_name(&name) {
                    return Err(ParseError {
                        kind: ParseErrorKind::UnknownModifier(name),
                        span: start..self.previous_end(),
                    });
                }
                // The `(` has to **abut** the name, exactly as it must for an attached
                // call and an index. Command position separates arguments by spacing,
                // so `puts $x:upper (1)` is the chain plus a separate `(1)`; that used
                // to be enforced only by `value_argument_starts` declining to claim an
                // argument-free chain, which stopped being available once it claims
                // them. Enforcing it here keeps the rule where the chain is read.
                let arguments = if self.same(&TokenKind::LParen)
                    && self
                        .peek()
                        .is_some_and(|token| token.span.start == self.previous_end())
                {
                    self.position += 1;
                    // The first argument of the replace family is a **regex match
                    // slot** (`DESIGN.md` §"String"), so a bare `/…/` there reads as
                    // a pattern rather than an absolute path — the same conversion
                    // the `~` right-hand side and a `match` arm get. Only the first:
                    // the replacement is an ordinary value slot, where `/…/` is the
                    // literal string it looks like. Setting the slot here is enough
                    // to say "only the first", since the first operand to be parsed
                    // takes the flag with it.
                    let regex_slot = matches!(
                        name.as_str(),
                        "replaceall" | "replacestart" | "replaceend" | "match" | "matches"
                    );
                    self.regex_slot = regex_slot;
                    let mut arguments = self.deeper(Self::arguments)?;
                    self.regex_slot = false;
                    if regex_slot && !arguments.is_empty() {
                        if let Argument::Positional(first) = arguments.remove(0) {
                            arguments.insert(0, Argument::Positional(regex_slot_operand(first)));
                        } else {
                            return Err(
                                self.error(ParseErrorKind::Expected("a positional pattern"))
                            );
                        }
                    }
                    Some(arguments)
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

    /// Is the token run at `offset` a glob with an **attached** `(`?
    ///
    /// The two signals a qualifier list needs, and both are about the tokens
    /// rather than a parsed tree: the word has to carry bare glob syntax, which
    /// is what keeps `style(x)` and `pwd()` ordinary calls, and the `(` has to
    /// abut it, which is what keeps `ls * (1 + 2)` a glob and a separate value.
    fn qualified_glob_at(&self, offset: usize) -> bool {
        let Some(word) = self.tokens.get(self.position + offset) else {
            return false;
        };
        let TokenKind::Word(text) = &word.value else {
            return false;
        };
        word_globs(text)
            && self
                .tokens
                .get(self.position + offset + 1)
                .is_some_and(|next| {
                    matches!(next.value, TokenKind::LParen) && next.span.start == word.span.end
                })
    }

    /// Read the `(…)` qualifiers sitting at the cursor, if a glob just ended here.
    ///
    /// `end` is where that glob's last token stopped: the `(` counts only when it
    /// abuts, since spacing is what separates an argument from the next one.
    fn glob_qualifiers(&mut self, end: usize) -> Result<Option<GlobQualifiers>, ParseError> {
        if !self.peek().is_some_and(|token| {
            matches!(token.value, TokenKind::LParen) && token.span.start == end
        }) {
            return Ok(None);
        }
        self.position += 1;
        let mut qualifiers = GlobQualifiers::default();
        loop {
            self.newlines();
            if self.eat(&TokenKind::RParen).is_some() {
                break;
            }
            self.qualifier(&mut qualifiers)?;
            self.newlines();
            if self.eat(&TokenKind::Comma).is_none() {
                self.expect(&TokenKind::RParen, "`,` or `)`")?;
                break;
            }
        }
        Ok(Some(qualifiers))
    }

    /// One qualifier: a bare `find -type` letter, or `name: value`.
    ///
    /// Each **dimension** may be answered once. The comma is an `and`, so a second
    /// answer to the same question is either a contradiction (`exec: true,
    /// exec: false`) or a silent overwrite, and a second *type* is neither: a path
    /// has exactly one, so `*(f, d)` can only have meant the `file|dir` alternation
    /// and saying so is better than quietly reading it as one.
    fn qualifier(&mut self, into: &mut GlobQualifiers) -> Result<(), ParseError> {
        let Some(name) = self.word_text_at(0) else {
            return Err(self.error(ParseErrorKind::Expected("a glob qualifier")));
        };
        let name = name.to_owned();
        let named = matches!(
            self.tokens.get(self.position + 1).map(|t| &t.value),
            Some(TokenKind::Colon)
        );
        let taken = |dimension, already: bool, span: Span| {
            already.then_some(ParseError {
                kind: ParseErrorKind::DuplicateGlobQualifier(dimension),
                span,
            })
        };
        if !named {
            let span = self.peek().map_or(0..0, |token| token.span.clone());
            self.position += 1;
            // The letters are shorthands for the two dimensions that have one:
            // a type, and the `exec` test. Anything else is a name the reader
            // expected to mean something, so say so rather than ignore it.
            if let Some(kind) = FileKind::from_letter(&name) {
                if let Some(error) = taken("type", !into.types.is_empty(), span) {
                    return Err(error);
                }
                into.types.push(kind);
                return Ok(());
            }
            if name == "x" {
                if let Some(error) = taken("exec", into.exec.is_some(), span) {
                    return Err(error);
                }
                into.exec = Some(true);
                return Ok(());
            }
            return Err(ParseError {
                kind: ParseErrorKind::UnknownGlobQualifier(name),
                span: self.tokens[self.position - 1].span.clone(),
            });
        }
        let name_span = self.peek().map_or(0..0, |token| token.span.clone());
        self.position += 2;
        match name.as_str() {
            // Alternation is the type dimension's only spelling for "either", `|`
            // rather than a second `type:` entry, so it is read here rather than
            // by letting the qualifier appear twice.
            "type" => {
                if let Some(error) = taken("type", !into.types.is_empty(), name_span) {
                    return Err(error);
                }
                loop {
                    let span = self.peek().map_or(0..0, |token| token.span.clone());
                    let value = self
                        .word_text_at(0)
                        .ok_or_else(|| self.error(ParseErrorKind::Expected("a file type")))?
                        .to_owned();
                    let kind = FileKind::from_name(&value).ok_or(ParseError {
                        kind: ParseErrorKind::BadGlobQualifier("type".into(), value),
                        span,
                    })?;
                    into.types.push(kind);
                    self.position += 1;
                    if self.eat(&TokenKind::Pipe).is_none() {
                        return Ok(());
                    }
                }
            }
            "exec" => {
                if let Some(error) = taken("exec", into.exec.is_some(), name_span) {
                    return Err(error);
                }
                into.exec = Some(self.qualifier_bool(&name)?);
            }
            "empty" => {
                if let Some(error) = taken("empty", into.empty.is_some(), name_span) {
                    return Err(error);
                }
                into.empty = Some(self.qualifier_bool(&name)?);
            }
            _ => {
                return Err(ParseError {
                    kind: ParseErrorKind::UnknownGlobQualifier(name),
                    span: self.tokens[self.position - 2].span.clone(),
                });
            }
        }
        Ok(())
    }

    /// The `true` / `false` a boolean qualifier takes.
    fn qualifier_bool(&mut self, name: &str) -> Result<bool, ParseError> {
        let span = self.peek().map_or(0..0, |token| token.span.clone());
        let value = self
            .word_text_at(0)
            .ok_or_else(|| self.error(ParseErrorKind::Expected("`true` or `false`")))?
            .to_owned();
        self.position += 1;
        match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(ParseError {
                kind: ParseErrorKind::BadGlobQualifier(name.to_owned(), value),
                span,
            }),
        }
    }

    /// Grow a word rightwards through every **adjacent** token that has a word
    /// spelling, so a run the lexer split on punctuation (`./x`, `a.b`, `x[0]`)
    /// comes back as the one word it looks like. `pieces` is what the leading
    /// token contributed and `span` is where it sat.
    ///
    /// Ends by folding an access or modifier chain into the reference it follows,
    /// exactly as [`command_word`](Self::command_word) does. Doing it in only one of
    /// the two made the same string mean two things by where it sat: `puts
    /// "$x:upper"` printed `AB` while `y = "$x:upper"` bound the literal
    /// `ab:upper`, and the value-position reading was silent. §Modifiers states the
    /// rule without qualifying by position, so the divergence was a bug against the
    /// documented behavior rather than a choice.
    ///
    /// [`command_word`]: Parser::command_word
    fn word_run(&mut self, pieces: Vec<WordPiece>, span: Range<usize>) -> Result<Expr, ParseError> {
        let start = span.start;
        let mut end = span.end;
        let mut pieces = pieces;
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
            let Some(next_pieces) = self.peek().and_then(|next| token_word_pieces(&next.value))
            else {
                break;
            };
            end = self.peek().unwrap().span.end;
            self.position += 1;
            pieces.extend(next_pieces);
        }
        let pieces = merge_command_variable_access(pieces).map_err(|kind| ParseError {
            kind,
            // The merge works on piece text, which carries no offsets of its own, so
            // the word is the narrowest span honestly available — as in `command_word`.
            span: start..end,
        })?;
        Ok(Expr::Scalar(Spanned {
            value: Word {
                pieces,
                qualifiers: None,
            },
            span: start..end,
        }))
    }

    /// Most nested constructs descend through here — a parenthesized group, a list
    /// or map literal, a capture, and the block of an expression-position `if` /
    /// `match` / `for` — so one counter on this call covers all of those. The ones
    /// it does not see have counters of their own: [`Parser::block`] for a
    /// statement-position `if`, [`Parser::prefix`] for a chain of `-` or `...`,
    /// and the `else if` arm of [`Parser::if_expr`].
    ///
    /// Counted rather than measured against the real stack: a limit that depended
    /// on how much stack happened to be left would accept a script one day and
    /// refuse it the next.
    fn primary(&mut self) -> Result<Expr, ParseError> {
        self.deeper(Self::primary_inner)
    }

    /// A `/…/` literal in a **regex slot**, read from the source rather than
    /// reassembled from tokens.
    ///
    /// A regex literal is not a token: it is an ordinary word that
    /// [`match_operand`] recognizes afterwards by its shape, so it ends wherever
    /// a word ends. Every character a regex is *made of* ends one — `[`, `(`,
    /// `{`, `|`, `,`, `:` — so `/[A-Za-z]/` was never one word to recognize, and
    /// the tokens it did produce parsed as an index and a division: "expected a
    /// value expression", pointing at neither. In an `if` condition it did not
    /// even report, since a condition that fails to parse as a value is re-read
    /// as a command, and `if $x ~ /[A-Za-z]/` quietly ran `$x`.
    ///
    /// The slots are the reason this can be decided at all. A leading `/` is far
    /// more often a path than a pattern, so the lexer cannot know — but the
    /// right-hand side of `~`, a `match` arm, and a replace's pattern are three
    /// places where the shape is a pattern or nothing, so the parser can ask.
    /// Command position never reaches here, and `ls /usr/bin` stays a path.
    ///
    /// **The closing `/` must end the word.** That is what keeps `$x ~ /usr/bin`
    /// the glob it is today: the scan finds a closing `/` before `bin`, sees a
    /// word character after it, and declines — leaving the existing reading,
    /// which refuses any literal holding an interior slash, to answer as before.
    /// A regex that needs one spells it `\/`, as it always has.
    fn regex_literal(&mut self) -> Result<Option<Expr>, ParseError> {
        let Some(start) = self.peek().map(|token| token.span.start) else {
            return Ok(None);
        };
        // Bounded by `source_len`, not by the end of the string. A nested parse —
        // a `${…}` body, a capture — is handed the *whole* source with its own
        // end recorded separately, so an unbounded scan would run past the body
        // and take a slash from the text around it as the closer.
        let Some(rest) = self.source.get(start..self.source_len) else {
            return Ok(None);
        };
        let mut characters = rest.char_indices();
        if characters.next().is_none_or(|(_, first)| first != '/') {
            return Ok(None);
        }
        let mut pattern = String::new();
        let mut close = None;
        while let Some((offset, character)) = characters.next() {
            match character {
                // A literal is one line. Scanning past a newline would swallow
                // whole statements looking for a slash that was never a closer.
                '\n' => return Ok(None),
                '\\' => {
                    let Some((_, escaped)) = characters.next() else {
                        return Ok(None);
                    };
                    match escaped {
                        // A **line continuation**, which the lexer has already
                        // resolved by joining the lines — so keeping the pair
                        // would put a backslash and a newline in the pattern
                        // that the reader never wrote, and `/a\⏎b/` would match
                        // a newline instead of `ab`.
                        '\n' => {}
                        // `\/` is how a literal spells a slash, and the regex
                        // engine has no such escape — it is this grammar's, so
                        // it is spent here.
                        '/' => pattern.push('/'),
                        // Every other escape is the engine's and travels whole.
                        other => {
                            pattern.push('\\');
                            pattern.push(other);
                        }
                    }
                }
                '/' => {
                    close = Some(start + offset);
                    break;
                }
                _ => pattern.push(character),
            }
        }
        let Some(close) = close else {
            return Ok(None);
        };
        // The closer has to end the word, or this is a path with slashes in it
        // rather than a literal with one at each end.
        if self.source[close + 1..self.source_len]
            .chars()
            .next()
            .is_some_and(|after| !ends_a_word(after))
        {
            return Ok(None);
        }
        // Give back every token the literal covers. A token that straddles its
        // end is not one this can consume — nothing spells that today, and
        // guessing at half a token is worse than declining.
        let saved = self.position;
        while self.peek().is_some_and(|token| token.span.start <= close) {
            self.position += 1;
        }
        if self.previous_end() != close + 1 {
            self.position = saved;
            // The literal is well formed in the source and the tokens do not
            // cover it, which means the lexer consumed part of it without
            // emitting anything — a comment. Reported rather than declined: a
            // decline leaves the leading `/a` to read as a glob and the test to
            // answer false, which is the silence this whole reading exists to
            // remove.
            return Err(ParseError {
                kind: ParseErrorKind::RegexLiteralInterrupted,
                span: start..close + 1,
            });
        }
        // A chain hanging off the closer keeps this a literal only if every link
        // is a regex **flag**. `/a/:upper` is the ordinary string `/A/`
        // everywhere else, and reading it as a regex here would both change what
        // it means and fail, since `:upper` is not a flag — the rule
        // [`became_regex`] already applies to a literal that tokenized as one
        // word, asked here of one that did not.
        let mut ahead = 0;
        while matches!(
            self.tokens.get(self.position + ahead).map(|t| &t.value),
            Some(TokenKind::Colon)
        ) {
            match self.word_text_at(ahead + 1) {
                Some(name) if regex_flag(name) => ahead += 2,
                _ => {
                    self.position = saved;
                    return Ok(None);
                }
            }
        }
        Ok(Some(Expr::Regex(pattern)))
    }

    fn primary_inner(&mut self) -> Result<Expr, ParseError> {
        // Before anything else, and only where a slot asked for it: a `/…/`
        // literal is read whole here or not at all, since every reading below
        // would take it apart. Cleared as it is read, so it describes the start
        // of this operand and not of a sub-expression inside it.
        if std::mem::take(&mut self.regex_slot)
            && let Some(regex) = self.regex_literal()?
        {
            return Ok(regex);
        }
        if self.eat(&TokenKind::CaptureStart).is_some() {
            self.newlines();
            return Ok(Expr::Capture(self.source(Some(TokenKind::RParen))?));
        }
        // An attached `:modifier` outranks all three: these arms return before the
        // postfix loop, so without the guard the chain is never reached.
        let keyword = !self.carries_attached_modifier();
        if keyword && self.word("if") {
            return Ok(Expr::If(Box::new(self.if_expr()?)));
        }
        if keyword && self.word("match") {
            return Ok(Expr::Match(Box::new(self.match_expr()?)));
        }
        if keyword && self.word("for") {
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
        // A leading `:name` is a modifier *reference*. A postfix `:name` never
        // reaches here — it is consumed by the modifier loop after a value — so the
        // only `:` that starts an expression is this one.
        if self.same(&TokenKind::Colon)
            && let Some(name) = self.modifier_ref_name()
        {
            self.position += 2;
            return Ok(Expr::ModifierRef(name));
        }
        if self.eat(&TokenKind::LParen).is_some() {
            self.newlines();
            // One expression, so every newline in here is layout — including one
            // that lands mid-expression, which is the case `newlines` on its own
            // could not reach. See [`Parser::wraps`].
            self.grouped += 1;
            let value = self.expression();
            self.grouped -= 1;
            let value = value?;
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
                    self.word_run(word.pieces, token.span)
                }
            }
            // A word can begin with punctuation the expression grammar otherwise
            // owns, and the leading `.` of a relative path is the case that bites:
            // `./x`, `.*` and `.` are all one word to the lexer's caller, but the
            // `.` arrives as its own `Dot`. Command position already stitches the
            // run back together (`token_word_pieces`); an operand slot has to do
            // the same or `x = ./foo` is a syntax error while `puts ./foo` works.
            // Member access never lands here — a postfix `.` is consumed after a
            // value, so the only `.` that can start an expression is a path's.
            TokenKind::Dot => self.word_run(
                vec![WordPiece::Text {
                    text: ".".into(),
                    quote: QuoteMode::Bare,
                }],
                token.span,
            ),
            // `..` is the range token, and stays one everywhere a range can be
            // written — `..3`, `1..`, a bare `..`. A `/` attached to it cannot
            // continue a range, though, since no operand starts with one, so
            // `../x` is unambiguously the parent-directory path.
            TokenKind::Range
                if self.peek().is_some_and(|next| {
                    next.span.start == token.span.end
                        && matches!(&next.value, TokenKind::Word(word)
                            if word_starts_with_slash(word))
                }) =>
            {
                self.word_run(
                    vec![WordPiece::Text {
                        text: "..".into(),
                        quote: QuoteMode::Bare,
                    }],
                    token.span,
                )
            }
            TokenKind::Range | TokenKind::RangeInclusive => {
                self.position -= 1;
                self.range(None)
            }
            // A lone `*` is the glob, not multiplication. The lexer can only tell
            // them apart by spacing, so it hands both spellings over as the same
            // operator token; reaching `primary` settles it, since a binary `*` is
            // consumed by `binary` before its right operand is parsed and never
            // arrives here. Yielding the bare word lets the usual expansion glob it,
            // so `for f in *` and `x = *` mean what `DESIGN.md` §"Loops" says.
            TokenKind::Operator(op) if op == "*" => Ok(Expr::Scalar(Spanned {
                value: Word {
                    pieces: vec![WordPiece::Text {
                        text: op,
                        quote: QuoteMode::Bare,
                    }],
                    qualifiers: None,
                },
                span: token.span,
            })),
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
        // `[:]` is the empty map, but only when the `:` is immediately closed: a list
        // whose first element is a modifier reference also opens with a colon
        // (`[:stem]`), so matching on the `:` alone would swallow it.
        if self.same(&TokenKind::Colon)
            && matches!(
                self.tokens.get(self.position + 1).map(|t| &t.value),
                Some(TokenKind::RBracket)
            )
        {
            self.position += 2;
            return Ok(Expr::Map(Vec::new()));
        }
        let mut values = Vec::new();
        let mut pairs = Vec::new();
        let mut is_map = false;
        loop {
            let spread = self.eat(&TokenKind::Spread).is_some();
            // A bare identifier with an attached `:` is a **map key**, settled before
            // descending. The key is otherwise parsed by `expression`, whose postfix
            // loop claims the colon first, so `[host:upper]` built the string `HOST`
            // and `[host:upper, port:22]` was a hard "consistent map entries" error —
            // silently, and only for values that happened to name a modifier.
            //
            // Nothing that really wants a chain here is a bare word, so every spelling
            // that means one is untouched: `["abc":upper]`, `[$x:upper]`,
            // `[(host:upper)]`.
            let key = if !spread && self.bare_map_key() {
                let token = self.next().expect("`bare_map_key` peeked one");
                let TokenKind::Word(word) = token.value else {
                    unreachable!("`bare_map_key` checked the token kind")
                };
                self.word_run(word.pieces, token.span)?
            } else {
                self.expression()?
            };
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

    /// An endpoint, refused if it is itself a range.
    ///
    /// The operand tier is `binary(6)`, above range, so the *loop* can never hand
    /// one back — but `primary` reads a leading `..` as a whole range, which is
    /// how `1 .. ..3` reaches the shape `1 .. 2 .. 3` is refused for. Checked
    /// here so both spellings answer the same way, rather than one at parse time
    /// and the other at evaluation.
    fn range_endpoint(&mut self) -> Result<Expr, ParseError> {
        let start = self.position;
        let end = self.binary(6)?;
        if matches!(end, Expr::Range { .. }) {
            return Err(ParseError {
                kind: ParseErrorKind::ChainedRange,
                span: self.tokens[start].span.start..self.previous_end(),
            });
        }
        Ok(end)
    }

    fn range(&mut self, start: Option<Expr>) -> Result<Expr, ParseError> {
        let inclusive = self.eat(&TokenKind::RangeInclusive).is_some();
        if !inclusive {
            self.expect(&TokenKind::Range, "a range operator")?;
        }
        let end = if self.at_expression_end() {
            None
        } else {
            Some(Box::new(self.range_endpoint()?))
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

    /// The name a `func` or an `alias` is defining, read **without judging it**.
    ///
    /// Unlike [`name`](Self::name), which is the general "a name goes here" reader,
    /// this accepts any bare word and hands the text on for the definition to
    /// reject when it runs. That is what keeps one bad name from costing the whole
    /// file it was generated into: every rule about what a definition may be called
    /// now reports the same way, at the same moment, and takes only its own
    /// definition down.
    ///
    /// It has to read the shapes no name may have, which means gluing back what the
    /// lexer split. `a.b` arrives as `a` `.` `b`, so adjacent word and dot tokens
    /// with nothing between them are one candidate; anything else ends it, and
    /// `func f()` still stops at the `(` because `(` is neither.
    fn definition_name(&mut self, alias: bool) -> Result<String, ParseError> {
        let token = self
            .next()
            .ok_or_else(|| self.eof(ParseErrorKind::UnexpectedEnd))?;
        // A quoted word is a string, not a name — there is no call spelling for
        // one — so this stays the general "expected a name" rather than becoming a
        // rule about which names are allowed.
        if matches!(&token.value, TokenKind::Word(word) if word_is_quoted(word)) {
            return Err(ParseError {
                kind: ParseErrorKind::Expected("a name"),
                span: token.span,
            });
        }
        // The **first** piece goes through the same table as the glued ones, so a
        // candidate that opens with lexer-split punctuation — `.foo`, which starts
        // on a `Dot` — is read whole rather than rejected here. Requiring a `Word`
        // to start would put the parse error back for exactly the names that most
        // need the runtime one, and take the rest of the file with it. What is left
        // over answers `None` and is still `expected a name`: `(`, `=`, a redirect.
        let Some(mut name) = name_piece(&token.value, alias) else {
            return Err(ParseError {
                kind: ParseErrorKind::Expected("a name"),
                span: token.span,
            });
        };
        let mut end = token.span.end;
        while let Some(next) = self
            .tokens
            .get(self.position)
            .filter(|t| t.span.start == end)
        {
            // A quoted piece glued on is the same rule as a quoted first piece — a
            // string is not a name — and has to be *claimed* to be reported, so
            // `alias ."foo" = …` answers `expected a name` rather than stopping the
            // name early and failing on the `=` that is not where it now looks.
            if matches!(&next.value, TokenKind::Word(word) if word_is_quoted(word)) {
                return Err(ParseError {
                    kind: ParseErrorKind::Expected("a name"),
                    span: next.span.clone(),
                });
            }
            let Some(text) = name_piece(&next.value, alias) else {
                break;
            };
            end = next.span.end;
            name.push_str(&text);
            self.position += 1;
        }
        Ok(name)
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

    /// Is the token at `index` a `<` or `>` with whitespace on both sides — the shape
    /// a comparison takes and an attached redirect (`cmd >out`) does not?
    ///
    /// Takes an index rather than peeking, because every caller reaches it after
    /// parsing the left operand and so stands *on* the operator: an operand can be a
    /// list literal or carry postfix modifiers, and only a parse of the whole thing
    /// says where the operator is.
    fn spaced_operator_at(&self, index: usize) -> bool {
        let Some(operator) = self.tokens.get(index) else {
            return false;
        };
        if !matches!(operator.value, TokenKind::Less | TokenKind::Greater) {
            return false;
        }
        let Some(left) = index
            .checked_sub(1)
            .and_then(|before| self.tokens.get(before))
        else {
            return false;
        };
        self.tokens.get(index + 1).is_some_and(|right| {
            left.span.end < operator.span.start && operator.span.end < right.span.start
        })
    }

    /// Does a value start here?
    ///
    /// Three questions in order, and the order is the rule.
    ///
    /// 1. Is this something a command word cannot be spelled with — `[`, `(`, `$(`,
    ///    `..`, an attached `:name(`, an attached call, or the reserved `not`? Then
    ///    there is no command reading to weigh it against.
    /// 2. Does the *command* reading claim the text, because a redirect or a spaced
    ///    argument follows the command word? Then the line is a command.
    /// 3. Otherwise **parse the statement and look at what came out**.
    ///
    /// Step 3 is what keeps this from being a list of operand shapes. The predecessor
    /// enumerated them — variable, quoted word, numeral, signed numeral, attached call
    /// — and each shape carried its own hand-rolled lookahead for finding the operator
    /// after it. Every one of those lookaheads was a place to be wrong, and each was:
    /// a modifier chain moved the operator (`if $x:len > 5`), so did arithmetic
    /// (`if $x + 1 > 1`), a sign put the operand a token later (`if -1 < 0`), and a
    /// modifier taking arguments moved it past a `(` no token scan could follow
    /// (`if 1:repr:split("x"):len > 0`). The parser already knows where every operand
    /// ends, so asking it answers for shapes nobody enumerated.
    ///
    /// `in_condition` survives for step 2 alone: a *spaced* `<` / `>` after a command
    /// word is a comparison in a condition and a redirection in a statement.
    fn value_start_in(&mut self, in_condition: bool) -> bool {
        // `not` is a **reserved word**, so a leading one always negates a value and
        // never names a command. `DESIGN.md` writes the idiom that way, and the other
        // two positions already read it so: a postfix guard (`puts x if not $b`) and
        // an assignment's right-hand side both parse an expression directly.
        //
        // Reserving it is what makes this one line. Keeping a command literally named
        // `not` reachable meant asking whether the *operand* was value-shaped, whether
        // a redirect followed the completed operand, and whether the negation was the
        // whole statement — three tests, one of them a trial parse, that existed only
        // for a command nobody writes. `./not` and `"not" arg` still reach one.
        if self.word("not") {
            return true;
        }
        // A modifier reference *call* — `:exists("Cargo.toml")` — starts a value, so
        // a condition or statement beginning with one reaches the expression parser
        // rather than the command parser. Restricted to the attached call form,
        // which nothing else can spell: a bare `:name` stays a command word, so
        // `puts :stem` and `$host:$port` keep the readings they have.
        if self.same(&TokenKind::Colon)
            && self.modifier_ref_name().is_some()
            && self
                .tokens
                .get(self.position + 1)
                .zip(self.tokens.get(self.position + 2))
                .is_some_and(|(name, next)| {
                    matches!(next.value, TokenKind::LParen) && name.span.end == next.span.start
                })
        {
            return true;
        }
        // None of these can name a command in any spelling — `[` always opens a list
        // literal, so there is no `[1 2]` command the way a shell that lacks list
        // values would have one — so they stay values and a following `<` / `>` stays
        // the comparison it reads as. Only a word operand has the second reading a
        // redirect needs.
        if matches!(
            self.peek().map(|token| &token.value),
            Some(
                TokenKind::CaptureStart
                    | TokenKind::LParen
                    | TokenKind::LBracket
                    | TokenKind::Range
                    | TokenKind::RangeInclusive
            )
        ) {
            return true;
        }
        // An **attached** call is the one word-initial shape with no command reading:
        // a command word stops in front of `(`, so `answer()` and `$f()` are calls.
        // The spacing matters and the tree does not record it, which is why this is
        // asked here rather than of the parsed expression — `puts (1 + 2)` is `puts`
        // with an argument, and the parser's `postfix` would happily call it.
        if let (Some(word), Some(next)) = (self.peek(), self.tokens.get(self.position + 1))
            && matches!(word.value, TokenKind::Word(_))
            && matches!(next.value, TokenKind::LParen)
            && next.span.start == word.span.end
        {
            return true;
        }
        // A leading `...` is the forwarded-rest **command**, not a value. In command
        // position a spread is a *word* whose pieces are the bare `...` and the
        // variable — the shape `expand::spread_var` explodes into argv — which is
        // why `...$xs | cat` already runs the list. Only the bare statement went the
        // other way: the expression parser claimed it, built a `UnaryOp::Spread`,
        // and the statement discarded the value, so `...$xs` alone ran nothing,
        // printed nothing and reported `0`. One position disagreeing with the other
        // is the whole bug; this makes both take the command reading.
        //
        // Conditions keep theirs. `if ...$xs` is the loud `a list is not a
        // condition` settled in 03c22a9, and reopening that is a separate question
        // from what a statement does.
        if !in_condition && self.same(&TokenKind::Spread) {
            return false;
        }
        if self.command_reading_claims_statement(in_condition) {
            return false;
        }
        self.parsed_value_claims_statement()
    }

    /// Does the **command** reading claim this statement outright, before any parse?
    ///
    /// Two shapes only command position has, both measured from the end of the command
    /// word rather than from the token after the one starting it — an operand can span
    /// several tokens, and in `$x:len > out.txt` the next token is the `:` of the
    /// modifier, so a one-token lookahead saw no redirect and let the expression parser
    /// swallow the `>` as a comparison:
    ///
    /// - a **redirect** operator (`if $editor > log`). In a *condition* a spaced
    ///   `<` / `>` is a comparison instead — `if $xs:len > 5` — and that reading is
    ///   left alone; `>>` is only ever a redirect and is never spelled spaced.
    /// - a **spaced postfix**, which is the next *argument*: `$cmd :len` runs
    ///   `printf :len` rather than taking the length of the word `printf`.
    fn command_reading_claims_statement(&self, in_condition: bool) -> bool {
        let Some(end) = self.command_word_end() else {
            return false;
        };
        let redirect = matches!(
            self.tokens.get(end).map(|token| &token.value),
            Some(TokenKind::Less | TokenKind::Greater | TokenKind::Append)
        ) && !(in_condition && self.spaced_operator_at(end));
        redirect || self.unattached_postfix_at(end)
    }

    /// Parse the statement as a value and ask whether the value **wins**.
    ///
    /// Two conditions, and both are about the parse rather than about the tokens that
    /// went into it: the expression that came out has to outrank the command reading of
    /// the same text, and it has to account for the whole statement.
    fn parsed_value_claims_statement(&mut self) -> bool {
        let saved = self.position;
        let claims = match self.expression() {
            Ok(expression) => {
                let one_word = self.is_one_command_word(saved, self.position);
                outranks_a_command(&expression) && self.value_spans_statement(&expression, one_word)
            }
            // Text that is not an expression at all is a command — including a partial
            // one, which the command parser reports as incomplete so the reader asks
            // for more instead of erroring.
            //
            // A **chaining** error is the exception, and the one kind of failure that
            // can still claim the statement. It is not "this was never a value": the
            // parse got an operand, an operator, a second operand, and only then a
            // second operator of a tier that takes one. Handing that back to command
            // position reads the operators as arguments, so `$x .. 2 .. 3` *runs* `$x`
            // with `.. 2 .. 3` after it, and the diagnostic the value parse already
            // had is thrown away.
            Err(error)
                if matches!(
                    error.kind,
                    ParseErrorKind::ChainedRange | ParseErrorKind::ChainedComparison
                ) =>
            {
                self.chaining_error_claims_statement(saved)
            }
            Err(_) => false,
        };
        self.position = saved;
        claims
    }

    /// Does a chaining error claim the statement, or does the command reading still win?
    ///
    /// The same two questions a *successful* probe answers, asked of a parse that has no
    /// tree to ask them of. Skipping them was a real regression: `puts .. 2 .. 3` and
    /// `echo == x == y` are command lines whose arguments merely look like operators,
    /// and `${cmd}==a==b` is one unbroken word naming a program. All three became syntax
    /// errors when any chaining failure claimed the statement outright.
    ///
    /// The run is measured to where the parse *stopped* — the second operator — since
    /// that is as much text as the value reading ever accounted for.
    fn chaining_error_claims_statement(&mut self, start: usize) -> bool {
        if self.is_one_command_word(start, self.position) {
            return false;
        }
        self.position = start;
        // Only the leading operand, which is all `outranks_a_command` reads: a bare word
        // keeps the command reading, while an integer, boolean, quoted word, variable,
        // list, group, or capture has no command spelling to keep.
        matches!(self.prefix(), Ok(operand) if outranks_a_command(&operand))
    }

    /// Is the token run the value parse just consumed spellable as **one command word**?
    ///
    /// A command word is an unbroken run of adjacent tokens, so this is a question about
    /// spans rather than about the tree — and the tree cannot answer it. `${cmd}-1` and
    /// `$a - 1` are the same `Expr::Binary` over the same `Expr::Variable`; the first is
    /// one word naming a program and the second is arithmetic, and *only* the whitespace
    /// says which. The same holds for `${cmd}[0]` against `$xs[0 + 0]`, and for
    /// `${cmd}..bak` against `$a .. $b`.
    ///
    /// A `(` is the one thing an unbroken run still cannot be, since command position has
    /// no call syntax — which is what keeps `$x:split("-") || puts x` a value even though
    /// nothing in it is spaced.
    fn is_one_command_word(&self, start: usize, end: usize) -> bool {
        let run = &self.tokens[start..end.min(self.tokens.len())];
        run.windows(2)
            .all(|pair| pair[0].span.end == pair[1].span.start)
            // A `(` is the one thing an unbroken run still cannot be, and a newline is
            // the one whitespace that does *not* break the run by span: a `\n` is a
            // token of its own, one character wide, so it abuts its neighbors on both
            // sides. `$a ==` continued on the next line would otherwise measure as one
            // word.
            && !run.iter().any(|token| {
                matches!(token.value, TokenKind::LParen | TokenKind::Newline)
            })
    }
    /// The token index just past an operand that a **command word** can be: a word —
    /// optionally behind a leading `-`, since `-1` is a name a shell will try to run —
    /// plus any *attached, argument-free* `:modifier` suffixes. `None` when the operand
    /// cannot be a command word at all — anything that is not a word to start with —
    /// because then the line has no command reading and no redirect to find. A nested
    /// expression needs no special rejection: it begins with `[` or `(`, which the scan
    /// stops in front of and which is not a redirect operator, so `$xs[0 + 0] > 0` and
    /// `$p:pad(3) > 0` fall out as values on their own.
    ///
    /// A token scan rather than a parse, deliberately. It answers where the *command*
    /// reading of the text stops, which is a question the expression parser cannot be
    /// asked: the expression grammar nests whole expressions inside a subscript or a
    /// call, so a parse-based version reached past the arithmetic in `$x + 1 > 1` and
    /// past the computed index in `$xs[0 + 0] > 0`, turning value statements into
    /// commands that truncated a file. A command word cannot contain either.
    fn command_word_end(&self) -> Option<usize> {
        // A leading `-` is part of the command word: `-1` is a name a shell will try to
        // run, so `-1 < 0` redirects in statement position the way every other command
        // word does. Spacing is not the test here, even though it is the test for
        // reading the same `-` as a *sign*, because both spellings run a command called
        // `-` today and `- 3 > out` should not redirect differently from `-3 > out`.
        let mut start = self.position;
        if matches!(self.peek().map(|token| &token.value), Some(TokenKind::Operator(text)) if text == "-")
            && matches!(
                self.tokens.get(start + 1).map(|token| &token.value),
                Some(TokenKind::Word(_))
            )
        {
            start += 1;
        }
        let word = self.tokens.get(start)?;
        if !matches!(word.value, TokenKind::Word(_)) {
            return None;
        }
        let mut end = word.span.end;
        let mut index = start + 1;
        while let (Some(colon), Some(name)) = (self.tokens.get(index), self.tokens.get(index + 1)) {
            let attached = colon.span.start == end && name.span.start == colon.span.end;
            if !matches!(colon.value, TokenKind::Colon)
                || !matches!(name.value, TokenKind::Word(_))
                || !attached
            {
                break;
            }
            end = name.span.end;
            index += 2;
        }
        Some(index)
    }

    /// Is the token at `index` a postfix opener that is **not attached** to what comes
    /// before it — a spaced `:mod`, `(`, `[`, or `.`?
    ///
    /// The expression parser accepts a spaced modifier: `y = $x :len` is 4. Command
    /// position reads the same spacing as the next *argument*, which is why
    /// `puts $x :len` prints `abcd :len`. So after a command word an unattached postfix
    /// means the line is a command with an argument — `$cmd :len` runs `printf :len` —
    /// and the value probe must not swallow it. An *attached* one is part of the value
    /// and still may: `$xs[0 + 0] > 0` is a comparison.
    fn unattached_postfix_at(&self, index: usize) -> bool {
        let Some(token) = self.tokens.get(index) else {
            return false;
        };
        if !matches!(
            token.value,
            TokenKind::Colon | TokenKind::LParen | TokenKind::LBracket | TokenKind::Dot
        ) {
            return false;
        }
        index
            .checked_sub(1)
            .and_then(|before| self.tokens.get(before))
            .is_some_and(|previous| previous.span.end < token.span.start)
    }

    /// Does the statement end here, for a statement that *is* a value?
    ///
    /// [`at_command_end`](Self::at_command_end) counts a pipe, but an expression
    /// cannot *be* a pipeline stage: classifying `42 | cat` or `not $b | cat` as one
    /// would leave the `|` unconsumed and turn a command that runs today into a
    /// syntax error. A value heading a pipeline stays the command it was.
    fn at_value_statement_end(&self) -> bool {
        self.at_command_end()
            && !matches!(
                self.peek().map(|token| &token.value),
                Some(TokenKind::Pipe | TokenKind::PipeBoth)
            )
    }

    /// Is the next token one that makes this statement part of a **command list** —
    /// `&&`, `||`, or a backgrounding `&`?
    ///
    /// [`at_command_end`](Self::at_command_end) counts all three, because they do end a
    /// *command*. They do not end a value that has a command reading: `$cmd || puts
    /// failed` is the shell idiom it looks like, and reading `$cmd` as the string
    /// skipped running the command entirely — no output, no side effects, and the
    /// fallback branch decided by the string's truthiness instead of the exit status.
    fn at_command_list_operator(&self) -> bool {
        matches!(
            self.peek().map(|token| &token.value),
            Some(TokenKind::AndAnd | TokenKind::OrOr | TokenKind::Amp)
        )
    }

    /// Did the expression just parsed account for the **whole** statement?
    ///
    /// Asked with the cursor left where that parse stopped. An expression that merely
    /// *starts* the statement is not enough: `$editor file` and `$p:base arg` are
    /// command invocations, and claiming them left the trailing words unconsumed and
    /// reported `expected a statement separator` for lines that should run.
    fn value_spans_statement(&mut self, expression: &Expr, one_word: bool) -> bool {
        (self.at_value_statement_end()
                // A bare variable has a command reading, so shell-list syntax after it
                // — an argument, a pipe, a redirect, `&&`, `||`, `&` — picks the
                // command, and the variable alone is the value.
                && !(defers_to_a_command_list(expression, one_word)
                    && self.at_command_list_operator()))
                // An assignment operator counts as the end of it. What follows is a
                // *place* expression, which the expression side owns whether or not
                // the place is a legal one, and that is what keeps
                // `$env.PATH[0] = x` and `$xs:dedup = 9` the syntax errors about
                // places they are meant to be, rather than attempts to run a command
                // named by the value.
                || matches!(
                    self.peek().map(|token| &token.value),
                    Some(TokenKind::Equal | TokenKind::PlusEqual)
                )
                // So does a postfix guard, which is part of the value statement rather
                // than something following it: `$x if $b` is the guarded value it
                // already was, not a command named by `$x`. Checked with the same pair
                // — the keyword, and a guard that parses — that the command parser uses
                // to tell a guard from an argument called `if`.
                || ((self.word("if") || self.word("unless")) && self.viable_guard())
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
    /// Does a block open `offset` ahead, across any newlines in between? A
    /// contextual keyword has to ask this the way its own parser will read it:
    /// `fork_expr` consumes newlines before the `{`, so a lookahead at the
    /// immediate token alone made `fork\n{ … }` a command and then a syntax
    /// error, while `loop` — an unconditional keyword needing no lookahead —
    /// accepted the same shape.
    fn block_follows(&self, offset: usize) -> bool {
        self.tokens
            .get(self.position + offset..)
            .and_then(|rest| {
                rest.iter()
                    .find(|token| !matches!(token.value, TokenKind::Newline))
            })
            .is_some_and(|token| matches!(token.value, TokenKind::LBrace))
    }
    /// Is the token `offset` ahead an assignment operator? Used to tell a
    /// statement keyword from a variable that happens to share its name.
    fn assignment_follows(&self, offset: usize) -> bool {
        matches!(
            self.tokens.get(self.position + offset).map(|t| &t.value),
            Some(TokenKind::Equal | TokenKind::PlusEqual)
        )
    }

    /// `global name = …`, `global name += …`, `global unset …`, or `unset …`.
    fn scoped(&mut self) -> Result<Executable, ParseError> {
        let global = self.take_word("global");
        // `unset` is contextual *here* too, not only at the start of a statement:
        // in `global unset = 9` the assignment operator says `unset` is the name
        // being bound, so consuming it as the operation would deny the global
        // scope a variable the local scope is allowed to have.
        if self.word("unset") && !self.assignment_follows(1) {
            self.take_word("unset");
            return self.unset(global);
        }
        if !global {
            unreachable!("only `global` or `unset` reaches here, and `unset` was taken");
        }
        // `global $m.key = …` writes *into* the global binding rather than rebinding
        // the name — the escape hatch a local-by-default member write needs, so a
        // function can modify a caller's collection instead of shadowing it.
        let member_start = self.position;
        if let Some(target) = self.member_target() {
            if self.assignment_follows(0) {
                let append = self.eat(&TokenKind::PlusEqual).is_some();
                if !append {
                    self.expect(&TokenKind::Equal, "`=`")?;
                }
                let value = self.expression()?;
                return Ok(Executable::MemberAssignment {
                    target,
                    append,
                    value,
                    global: true,
                });
            }
            self.position = member_start;
        }
        // `global` on its own governs an assignment; anything else is a mistake
        // worth naming, since `global f` reads like a call but cannot be one.
        let pattern = self.binding_pattern()?;
        if !self.assignment_follows(0) {
            return Err(self.error(ParseErrorKind::Expected(
                "`=` or `unset` after `global`; it governs an assignment, not a command",
            )));
        }
        // `global _ = …`. Refused for the same reason as the bare spelling, and it
        // reached here rather than being caught alongside it because this path asks
        // `binding_pattern` directly — which answers `Ignore`, and an `Ignore`
        // assignment then bound nothing and said nothing. A whole-pattern discard is
        // the degenerate case; `global [_ x] = …` is still a discarded *position*.
        if matches!(pattern, BindingPattern::Ignore) {
            return Err(self.error(ParseErrorKind::DiscardAssignment));
        }
        let append = self.eat(&TokenKind::PlusEqual).is_some();
        if !append {
            self.expect(&TokenKind::Equal, "`=`")?;
        }
        if append && !matches!(pattern, BindingPattern::Name(_)) {
            return Err(self.error(ParseErrorKind::Expected("a name before `+=`")));
        }
        let value = self.expression()?;
        Ok(Executable::Assignment {
            pattern,
            append,
            value,
            global: true,
        })
    }

    /// `export NAME = value` / `export NAME += value`.
    ///
    /// Desugars to the `$env.NAME` write, which already carries the boundary
    /// rules: only byte-strings cross, path-type names `:`-join, and an embedded
    /// NUL is refused. `export` exists because it is the ingrained spelling, not
    /// because it means anything else.
    fn export(&mut self) -> Result<Executable, ParseError> {
        self.take_word("export");
        // `export A=1 B=2` — the unspaced run `with` and the command prefix take,
        // so a name list is written the same way everywhere it appears. The spaced
        // `export NAME = value` below stays the single-binding spelling: with
        // spaces there is no way to see where one value ends and the next name
        // begins, which is the same reason a prefix requires them unspaced.
        if self.env_binding_follows(0) {
            let mut bindings = Vec::new();
            while let Some(binding) = self.env_binding()? {
                bindings.push(binding);
            }
            return Ok(Executable::EnvAssignments { bindings });
        }
        let Some(key) = self.word_text_at(0).map(str::to_owned) else {
            return Err(self.error(ParseErrorKind::Expected("a NAME after `export`")));
        };
        if !valid_name(&key) {
            return Err(self.error(ParseErrorKind::Expected("a valid name after `export`")));
        }
        self.position += 1;
        if !self.assignment_follows(0) {
            // Bare `export NAME` is bash's "mark this variable exported", which
            // has no meaning here: mesh keeps shell bindings and the environment
            // in separate namespaces, so there is nothing to mark. Name the
            // spelling that does what the user meant.
            return Err(self.error(ParseErrorKind::Expected(
                "a value: `export NAME = …`. mesh keeps shell variables and the environment \
                 separate, so to copy one in write `export NAME = $NAME`",
            )));
        }
        let append = self.eat(&TokenKind::PlusEqual).is_some();
        if !append {
            self.expect(&TokenKind::Equal, "`=`")?;
        }
        let value = self.expression()?;
        Ok(Executable::EnvAssignment {
            key: EnvKey::Literal(key),
            append,
            value,
        })
    }

    fn unset(&mut self, global: bool) -> Result<Executable, ParseError> {
        let mut targets = Vec::new();
        loop {
            if let Some(text) = self.word_text_at(0) {
                let name = text.to_owned();
                let span = self.tokens[self.position].span.clone();
                self.position += 1;
                targets.push(UnsetTarget::Name(Spanned { value: name, span }));
            } else if let Some(key) = self.env_target() {
                // Ahead of `member_target`, which excludes `$env` for the same
                // reason the assignment side does: the environment's entries are
                // bytes in the process, so removing one is `environ`'s job rather
                // than a walk into a bound collection.
                targets.push(UnsetTarget::Env(key));
            } else if let Some(target) = self.member_target() {
                targets.push(UnsetTarget::Member(target));
            } else {
                break;
            }
        }
        if targets.is_empty() {
            return Err(self.error(ParseErrorKind::Expected("a name or place to unset")));
        }
        Ok(Executable::Unset { targets, global })
    }

    fn word(&self, expected: &str) -> bool {
        matches!(self.peek().map(|t| &t.value), Some(TokenKind::Word(word)) if word.is_bare_text(expected))
    }
    /// Is the token `offset` ahead the bare word `expected`? The lookahead a
    /// two-word lead-in needs (`wrapper func`), where [`word`](Self::word) only
    /// sees the cursor.
    fn word_at(&self, offset: usize, expected: &str) -> bool {
        matches!(
            self.tokens.get(self.position + offset).map(|t| &t.value),
            Some(TokenKind::Word(word)) if word.is_bare_text(expected)
        )
    }
    /// Is the cursor on a bare identifier with an **attached** `:` — a map key?
    ///
    /// Narrow on purpose: only a bare word qualifies, so a quoted, expanded or
    /// parenthesized subject keeps its modifier chain inside a `[…]` literal. The
    /// colon must abut the name, which is the same signal a chain uses, so
    /// `[host : x]` is unaffected either way — it was already a pair.
    fn bare_map_key(&self) -> bool {
        self.word_text_at(0).is_some()
            && self
                .tokens
                .get(self.position + 1)
                .zip(self.peek())
                .is_some_and(|(colon, word)| {
                    matches!(colon.value, TokenKind::Colon) && colon.span.start == word.span.end
                })
    }
    /// Does the word at the cursor carry an **attached** `:modifier`?
    ///
    /// A keyword is claimed only as a *bare* word, so `if:upper` is a chain on the
    /// text `if` rather than the start of a conditional. Without this the keyword
    /// arms run first and never reach the postfix loop, which made `if:upper`,
    /// `match:upper` and `for:upper` syntax errors and — worse, because it is silent
    /// — `not:upper` the value `false`, since `not` took the negation and left
    /// `:upper` to fold away.
    ///
    /// Spacing is the signal, exactly as it is wherever else a chain is recognized:
    /// `if :upper` keeps the keyword and reads `:upper` as a modifier reference.
    fn carries_attached_modifier(&self) -> bool {
        let (Some(word), Some(colon)) = (self.peek(), self.tokens.get(self.position + 1)) else {
            return false;
        };
        matches!(word.value, TokenKind::Word(_))
            && matches!(colon.value, TokenKind::Colon)
            && colon.span.start == word.span.end
            && self
                .tokens
                .get(self.position + 2)
                .is_some_and(|name| name.span.start == colon.span.end)
            && self.word_text_at(2).is_some_and(modifier_name)
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

/// The **leftmost leaf** of an expression — the operand the command reading would take
/// as its command word.
///
/// Every classification below is about this one operand, because that is the only part
/// of an expression a command line can also be. `ls / extra` parses as a division and
/// `exit -1` as a subtraction, and either would outrank a command if the *shape* of the
/// tree decided; what says they are command lines is that they lead with a bare word.
/// Prefixes, infixes, and postfixes all wrap the same leading operand, so following
/// them down is the whole rule.
fn leading_operand(expression: &Expr) -> &Expr {
    match expression {
        Expr::Binary { left: inner, .. }
        | Expr::Unary {
            expression: inner, ..
        }
        | Expr::Modifier { value: inner, .. }
        | Expr::Index { value: inner, .. }
        | Expr::Member { value: inner, .. }
        | Expr::Call { callee: inner, .. }
        | Expr::Range {
            start: Some(inner), ..
        } => leading_operand(inner),
        _ => expression,
    }
}

/// Does this expression **outrank** the command reading of the same text?
///
/// The one shape a command is spelled with is a bare word, so an expression leading
/// with one leaves the command reading standing: `puts a` is the command, `ls / extra`
/// is not a division, and the `true` in `if true` runs a program. Everything else — a
/// list, a capture, a group, a lambda, a variable — has no command spelling at all, so
/// leading with it is already the answer.
///
/// Two words escape. An **integer literal** is never a command name, which is what lets
/// a block yield one (`func answer() { 42 }` is 42, not "command not found: 42"; `./42`
/// still runs a file called that) and what makes `1 < 2` a comparison wherever it is
/// read. A **quoted** word is a string literal rather than a command, so it takes the
/// value reading here and the evaluator does not run it either; a path needing quotes
/// goes through `command -- "…"` (`DESIGN.md` §"Bare words and quoted values").
///
/// **`true` / `false`** escape for the integer's reason: read as a value they are the
/// boolean, so `if true` is a literal rather than a fork+exec of `/usr/bin/true`, and
/// `func no() { false }` is the boolean instead of that program's status. They differ
/// from a numeral in that a program of each name really does exist, so the bare word
/// stops reaching it — `./true` and `command -- true` still do, exactly as `./42` does
/// (`DESIGN.md` §"Bare words and quoted values").
fn outranks_a_command(expression: &Expr) -> bool {
    match leading_operand(expression) {
        Expr::Scalar(word) => {
            word.value.bare_integer().is_some()
                || word.value.bare_boolean().is_some()
                || word_is_quoted(&word.value)
        }
        _ => true,
    }
}

/// Does a following `&&` / `||` / `&` make this a **command list** rather than a value?
///
/// Only when the value *is* a command word: an unbroken run of tokens led by a variable.
/// That is the one thing which both is a value and names a command — `$cmd || puts
/// failed` is the shell idiom it looks like, and reading `$cmd` as the string skipped
/// running the command entirely: no output, no side effects, and the fallback branch
/// decided by the string's truthiness instead of the exit status. `$p:base ||
/// puts failed` and `${cmd}.exe || puts failed` are the same idiom with suffixes the
/// command word keeps.
///
/// Two conditions, and `one_word` — from
/// [`is_one_command_word`](Parser::is_one_command_word) — is the load-bearing one.
/// Asking instead whether a *variable led* the expression handed the command reading to
/// text that has none, and the connector then picked a reading that could not work:
///
/// ```text
/// $a == $b && puts eq        # ran the command `5`, while `1 == 2 || puts no` compared
/// $x ~ /b/ && puts matched   # ran the command `abc`
/// $a + 1 && puts sum         # ran the command `1`
/// $x:split("-") || puts x    # syntax error: a command word stops in front of `(`
/// ```
///
/// Narrowing it by *shape* instead does not work, and the shape of a command word's
/// suffix is the thing to get wrong here: `${cmd}.exe`, `${cmd}[0]`, `${cmd}..bak`, and
/// `${cmd}-1` are all one word naming a program, and the tree calls them a member
/// access, an index, a range, and a subtraction. Each is indistinguishable from the
/// spaced expression of the same shape — `$a - 1` really is arithmetic — so whitespace
/// is the only thing that separates them, and it lives in the spans.
///
/// The leading operand still has to be a **variable**: that is what has both readings.
/// A numeral has none to lose, so `1..3 || puts x` is the range it looks like, and
/// `42 &` stays the refused backgrounded expression rather than a program called `42`.
fn defers_to_a_command_list(expression: &Expr, one_word: bool) -> bool {
    one_word && matches!(leading_operand(expression), Expr::Variable(_))
}

/// Is the whole of `name` a name?
///
/// The rule the tokenizer's own `$name` scan applies
/// ([`variable_end`](variable_end)) — an alphabetic or `_` head, then alphanumerics, `_`,
/// and *interior* `-` — asked of a complete string instead of a prefix of one. Every
/// caller that validates a name it did not scan itself (an `$env.KEY` target,
/// `export NAME`, a `${…}` place's root) goes through here, so a name a read accepts
/// is a name a write accepts. The previous whole-string check was anchored to the
/// compatibility lexer's ASCII-only scan instead, which is how `café = 5` bound a
/// variable while `export CAFÉ = x` was refused as an invalid name.
/// The byte length of the identifier `text` starts with, or `0` when it starts with
/// something that is not one.
///
/// The same rule [`valid_name`] applies to a whole string, asked of a prefix — an
/// alphabetic or `_` head, then alphanumerics, `_`, and *interior* `-`. Scanning by
/// anything narrower splits an identifier in half and hands the halves different
/// meanings: an alphanumeric-only scan read `upper_case` as the modifier `:upper`
/// plus the text `_case`, so `"$h:upper_case"` was `HOST_case` where the unquoted and
/// braced spellings both reported an unknown modifier. A name is claimed whole or not
/// at all.
fn name_prefix(text: &str) -> usize {
    let mut chars = text.char_indices().peekable();
    let Some((_, head)) = chars.next() else {
        return 0;
    };
    if !head.is_alphabetic()
        && (head != '_'
            || !chars
                .peek()
                .is_some_and(|(_, next)| *next == '_' || next.is_alphanumeric()))
    {
        return 0;
    }
    let mut end = head.len_utf8();
    // A `-` only continues the name when something continues *it*, so a trailing one
    // is punctuation rather than part of the identifier — the rule `valid_name`
    // spells as "no name ends in a hyphen".
    while let Some((offset, c)) = chars.peek().copied() {
        if c == '_' || c.is_alphanumeric() {
            end = offset + c.len_utf8();
            chars.next();
        } else if c == '-'
            && text[offset + 1..]
                .chars()
                .next()
                .is_some_and(|next| next == '_' || next.is_alphanumeric())
        {
            chars.next();
        } else {
            break;
        }
    }
    end
}

pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_alphabetic()
        && (first != '_'
            || !chars
                .clone()
                .next()
                .is_some_and(|c| c == '_' || c.is_alphanumeric()))
    {
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

impl Parser<'_> {
    /// The name of a `:name` modifier reference starting at the current `:`, when
    /// the next token really is a modifier name written tight against it.
    ///
    /// The adjacency check is what keeps `[a: 1]` and `f(key: value)` out of this:
    /// there the colon follows a key and is separated from what comes next, while a
    /// reference is written `:stem` with nothing between.
    fn modifier_ref_name(&self) -> Option<String> {
        let colon = self.peek()?;
        let next = self.tokens.get(self.position + 1)?;
        if colon.span.end != next.span.start {
            return None;
        }
        let TokenKind::Word(word) = &next.value else {
            return None;
        };
        let name = word.bare_word()?;
        modifier_name(name).then(|| name.to_string())
    }
}

fn modifier_name(name: &str) -> bool {
    MODIFIER_NAMES.contains(&name)
}

/// Is `name` a modifier that **requires** a parenthesized argument list? Used to
/// give a clearer error when such a modifier is written bare (`:split` with no
/// arguments) rather than the generic "not implemented yet".
///
/// `:trimstart` / `:trimend` are deliberately absent: their argument (a char set)
/// is optional, so the bare spelling is the whitespace form rather than a mistake.
pub(crate) fn modifier_requires_arguments(name: &str) -> bool {
    matches!(
        name,
        "join"
            | "split"
            | "map"
            | "filter"
            | "each"
            | "get"
            | "stripstart"
            | "stripend"
            | "replaceall"
            | "replacestart"
            | "replaceend"
    )
}

/// Can `name` take an argument list at all? The superset that adds the two whose
/// argument is *optional*, since an abutting `(` after either is still the argument
/// form — `"$x:trimstart(abc)"` asks for the char set, not for literal parentheses.
fn modifier_accepts_arguments(name: &str) -> bool {
    modifier_requires_arguments(name) || matches!(name, "trimstart" | "trimend")
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
    "ls",
    "m",
    "map",
    "match",
    "matches",
    "mod",
    "ms",
    "mtime",
    "multiline",
    "ns",
    "nulls",
    "num",
    "old",
    "parents",
    "quotemeta",
    "raw",
    "read",
    "real",
    "remove",
    "replace",
    "replaceall",
    "replaceend",
    "replacestart",
    "repr",
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
    "ts",
    "tty",
    "type",
    "upper",
    "url",
    "values",
    "words",
    "write",
    "ws",
    "x",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(source: &str) -> Source {
        match parse(source).unwrap() {
            ParseOutcome::Complete(tree) => tree,
            ParseOutcome::Incomplete(_) | ParseOutcome::IncompleteHeredoc(_) => {
                panic!("unexpected incomplete input")
            }
        }
    }

    /// A newline is the one whitespace that does not break a token run by span: it is
    /// a token of its own, one character wide, so it abuts its neighbors on both sides
    /// and `$a==` continued on the next line measures as adjacent. A command word
    /// cannot span lines, so [`Parser::is_one_command_word`] rejects the run outright.
    ///
    /// A unit test because there is nothing to observe from the shell: the reader
    /// splits these statements before the predicate is consulted, so the rule is about
    /// the predicate keeping its contract rather than about a reading that moves.
    #[test]
    fn a_run_spanning_a_newline_is_no_command_word() {
        for (source, one_word) in [
            ("$a==$a", true),
            ("$a==\n$a", false),
            ("$a:len", true),
            ("$a:split(\"-\")", false),
            ("$a == $a", false),
        ] {
            let tokens = tokenize(source).unwrap();
            let parser = Parser {
                source,
                source_len: source.len(),
                position: 0,
                tokens,
                depth: 0,
                regex_slot: false,
                grouped: 0,
            };
            // The whole source, which is what the value parse would consume here.
            let end = parser.tokens.len();
            assert_eq!(parser.is_one_command_word(0, end), one_word, "{source:?}");
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
                    qualifiers: None,
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
    fn parses_the_wrapper_marker_only_before_func() {
        // Contextual, like `fork`: the word leads a definition only where `func`
        // follows it, so an ordinary command or assignment of that name is
        // untouched.
        let tree = complete("wrapper func g(...xs) { puts hi }");
        let Executable::Function { wrapper, name, .. } = &tree.statements[0].and_or.first else {
            panic!("expected a function definition")
        };
        assert!(wrapper);
        assert_eq!(name, "g");

        let plain = complete("func g(...xs) { puts hi }");
        let Executable::Function { wrapper, .. } = &plain.statements[0].and_or.first else {
            panic!("expected a function definition")
        };
        assert!(!wrapper);

        // A bare `wrapper` is still a command word, not a malformed definition.
        let command = complete("wrapper --flag x");
        assert!(matches!(
            command.statements[0].and_or.first,
            Executable::Pipeline(_)
        ));
    }

    /// The word pieces of a single-command body, span-free so a synthesized body
    /// can be compared against a parsed one.
    fn body_words(body: &Source) -> Vec<Vec<WordPiece>> {
        let Executable::Pipeline(pipeline) = &body.statements[0].and_or.first else {
            panic!("expected a pipeline")
        };
        pipeline.stages[0]
            .items
            .iter()
            .map(|item| match item {
                CommandItem::Word(word) => word.value.pieces.clone(),
                other => panic!("expected a word, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn an_alias_desugars_to_a_wrapper_func() {
        // The claim the whole feature rests on: `alias` builds the definition you
        // would have written by hand, so it is checked against exactly that tree
        // rather than against its own shape.
        let sugar = complete("alias co = vcs checkout");
        let Executable::Function {
            name,
            parameters,
            body,
            wrapper,
        } = &sugar.statements[0].and_or.first
        else {
            panic!("expected a function definition")
        };
        assert!(wrapper);
        assert_eq!(name, "co");
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters[0].name, ALIAS_REST);
        assert_eq!(parameters[0].kind, ParamKind::Rest);

        let written = complete("wrapper func co(...args) { vcs checkout ...$args }");
        let Executable::Function { body: expected, .. } = &written.statements[0].and_or.first
        else {
            panic!("expected a function definition")
        };
        // Compared by word pieces rather than whole nodes: the spans differ by
        // construction, since the desugared body has no source text of its own.
        assert_eq!(body_words(body), body_words(expected));

        // `alias` leads a definition only in the shape that claims it.
        let assignment = complete("alias = 1");
        assert!(matches!(
            assignment.statements[0].and_or.first,
            Executable::Assignment { .. }
        ));
        let command = complete("alias --help");
        assert!(matches!(
            command.statements[0].and_or.first,
            Executable::Pipeline(_)
        ));
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
        // The payload is what a reader with no more input reports, so each case
        // is checked for *where* it says the trouble is, not only that it is
        // incomplete. An open delimiter names the delimiter and points at it.
        let incomplete = |source: &str| match parse(source).unwrap() {
            ParseOutcome::Incomplete(error) => *error,
            other => panic!("expected incomplete input, got {other:?}"),
        };

        let open_paren = incomplete("x = (1");
        assert_eq!(open_paren.kind, ParseErrorKind::Unterminated('('));
        assert_eq!(open_paren.span.start, 4);

        // A trailing connector has no delimiter to point at, so it keeps the
        // parser's own "ran out of input" rather than inventing a location.
        assert_eq!(incomplete("a &&").kind, ParseErrorKind::UnexpectedEnd);

        // A real fault reached before the input ran out keeps the parser's own
        // error: the parameter list wanted `,` or `)` and met `{`, which is more
        // use than "unclosed `(`" and is where the reader has to look. Only an
        // input that genuinely ends mid-construct gets re-aimed at its opener,
        // which is what stops a stray `{` later in a file from displacing the
        // fault above it.
        let nested = incomplete("func f(x {\nputs )");
        assert_eq!(nested.kind, ParseErrorKind::Expected("`,` or `)`"));

        // And the innermost opener is still what an end-of-input case names: `{`
        // has to be closed before the `(` around it.
        let both_open = incomplete("func f() {\n  x = (1");
        assert_eq!(both_open.kind, ParseErrorKind::Unterminated('('));
        assert_eq!(both_open.span.start, 17);

        assert!(parse("func f(x {\nputs )\n}").is_err());
    }

    #[test]
    fn line_and_column_are_one_based_and_counted_in_characters() {
        let source = "puts one\nputs two\nx = )\n";
        assert_eq!(line_and_column(source, 0), (1, 1));
        assert_eq!(line_and_column(source, 9), (2, 1));
        // The `)` on the third line.
        assert_eq!(line_and_column(source, 22), (3, 5));

        // Columns count characters: a byte column would name the wrong place on
        // any line holding a multi-byte one.
        let wide = "puts \"日本\"\nx = )\n";
        assert_eq!(line_and_column(wide, wide.find(')').unwrap()), (2, 5));

        // An offset at or past the end is the end, not a panic — `UnexpectedEnd`
        // carries exactly that.
        assert_eq!(line_and_column("abc", 3), (1, 4));
        assert_eq!(line_and_column("abc", 99), (1, 4));
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
    fn a_range_cannot_be_chained() {
        // Every spelling that puts a range where an endpoint belongs, including the
        // two that reach it through an operand rather than through the loop:
        // `1 .. ..3` builds its end in `primary`, and `..1 .. 2` arrives with the
        // *first* operand already a range.
        for source in [
            "x = 1 .. 2 .. 3",
            "x = 1 ..= 2 ..= 3",
            "x = 1..2..3..4",
            "x = 1.. ..3",
            "x = .. ..3",
            "x = ..1..2",
        ] {
            assert!(
                matches!(
                    parse(source),
                    Err(ParseError {
                        kind: ParseErrorKind::ChainedRange,
                        ..
                    })
                ),
                "{source} should be refused as a chained range"
            );
        }
        // Grouping is the way to say it, as it is for a comparison, and every
        // one-operator spelling is untouched — including the open-ended ones, whose
        // missing endpoint must not read as a chain.
        for source in [
            "x = (1 .. 2) .. 3",
            "x = 1..2",
            "x = 1..=2",
            "x = 1..",
            "x = ..3",
            "x = ..",
            "x = [1..2, 3..4]",
        ] {
            assert!(parse(source).is_ok(), "{source} should still parse");
        }
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
    fn a_capture_inside_double_quotes_becomes_a_value_piece() {
        let tokens = tokenize("\"at $(pwd) now\"").unwrap();
        let TokenKind::Word(word) = &tokens[0].value else {
            panic!()
        };
        let [
            WordPiece::Text {
                text: before,
                quote: QuoteMode::Double,
            },
            WordPiece::Value {
                expression: capture,
                ..
            },
            WordPiece::Text {
                text: after,
                quote: QuoteMode::Double,
            },
        ] = word.pieces.as_slice()
        else {
            panic!("{:?}", word.pieces)
        };
        assert_eq!((before.as_str(), after.as_str()), ("at ", " now"));
        // Parsed here and now, so a syntax error inside is a *parse* error — see
        // `capture_in_string`.
        let Expr::Capture(body) = &capture.value else {
            panic!("{:?}", capture.value)
        };
        assert_eq!(body.statements.len(), 1);
        // The span covers `$(pwd)`, the text the piece stands in for.
        assert_eq!(capture.span, 4..10);
    }

    #[test]
    fn a_capture_inside_double_quotes_ends_where_the_grammar_says() {
        // Not at the first `)`: that one is inside a word of the body. Scanning
        // characters would close the capture there and leave `b")"` as string text.
        let tokens = tokenize("\"[$(puts \"a)b\")]\"").unwrap();
        let TokenKind::Word(word) = &tokens[0].value else {
            panic!()
        };
        assert!(
            matches!(
                word.pieces.as_slice(),
                [
                    WordPiece::Text { text: open, .. },
                    WordPiece::Value { .. },
                    WordPiece::Text { text: close, .. },
                ] if open == "[" && close == "]"
            ),
            "{:?}",
            word.pieces
        );

        // Nesting closes innermost-first, the same as outside a string.
        assert!(tokenize("\"$(puts \"$(pwd)\")\"").is_ok());

        // A capture that never closes is an unterminated `(`, not the string's quote.
        assert!(matches!(
            tokenize("puts \"$(pwd"),
            Err(ParseError {
                kind: ParseErrorKind::Unterminated('('),
                ..
            })
        ));
        // With the string's quote still there, the body claims it first — a quote is
        // ordinary inside a capture (`$(puts "x")`), so the lexer cannot know this one
        // was meant to close the outer string. Still a syntax error, still names an
        // unclosed delimiter.
        assert!(matches!(
            tokenize("puts \"$(pwd\""),
            Err(ParseError {
                kind: ParseErrorKind::Unterminated('"'),
                ..
            })
        ));
    }

    #[test]
    fn assembles_adjacent_punctuation_into_command_words() {
        // `key:2` rather than `key:value`: a colon followed by a bare *identifier* is
        // a reserved modifier chain, so only a non-identifier keeps the old
        // punctuation-glues-into-a-word reading. `key:/path` and `a:$b` do too.
        let tree = complete("echo file.txt ./tool key:2 xs[0]");
        let Executable::Pipeline(pipeline) = &tree.statements[0].and_or.first else {
            panic!()
        };
        let words: Vec<_> = pipeline.stages[0]
            .items
            .iter()
            .map(|item| match item {
                CommandItem::Word(word) => word.value.text(),
                CommandItem::Redirect { .. } | CommandItem::Value(_) => panic!(),
            })
            .collect();
        assert_eq!(words, ["echo", "file.txt", "./tool", "key:2", "xs[0]"]);
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
            TokenKind::Word(Word { pieces, .. })
                if matches!(pieces.as_slice(), [WordPiece::Text { quote: QuoteMode::Raw, .. }])
        ));
    }

    #[test]
    fn invalid_unbraced_variable_heads_remain_literal() {
        for source in ["$5", "$_"] {
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
    fn an_access_with_modifier_arguments_is_recognised_from_the_parse() {
        // Asked of the parsed body, not its text: the parse has already applied the
        // lexer's rules about escapes, raw strings, comments, and nested
        // interpolations, which a scan over the source would have to re-derive.
        let arguments = |source: &str| {
            let tokens = tokenize(source).expect("lexes");
            tokens.iter().any(|token| match &token.value {
                TokenKind::Word(word) => word.pieces.iter().any(|piece| match piece {
                    WordPiece::Value { expression, .. } => {
                        has_modifier_arguments(&expression.value)
                    }
                    _ => false,
                }),
                _ => false,
            })
        };
        assert!(arguments("\"${xs:join(\" \")}\""));
        assert!(arguments("\"${m.k:join(\" \"):upper}\""));
        // An argument-free chain is the cheap path and never reaches this.
        assert!(!arguments("\"${$xs:len}\""));
        assert!(!arguments("\"${$n + 1}\""));
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

        // The control escapes, in both quote kinds, and `\u{…}` still spells the
        // same bytes — each is a second spelling of one that already worked, so a
        // script written either way behaves the same.
        for (source, decoded) in [
            (r#""\a""#, "\u{7}"),
            (r"'\a'", "\u{7}"),
            (r#""\u{7}""#, "\u{7}"),
            (r#""\b""#, "\u{8}"),
            (r"'\b'", "\u{8}"),
            (r#""\f""#, "\u{c}"),
            (r#""\v""#, "\u{b}"),
            (r#""\a\b\f\v""#, "\u{7}\u{8}\u{c}\u{b}"),
        ] {
            let tokens = tokenize(source).unwrap();
            let TokenKind::Word(word) = &tokens[0].value else {
                panic!("{source} did not lex to a word")
            };
            assert_eq!(word.text(), decoded, "{source}");
        }

        // `\0` stays out, so it is still a loud error rather than a NUL that
        // `execve` and the environment would refuse further down.
        assert!(matches!(
            tokenize(r#""\0""#),
            Err(ParseError {
                kind: ParseErrorKind::UnknownEscape('0'),
                ..
            })
        ));
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

    /// One name rule, checked whole. The whole-string check callers use to validate
    /// a name they did not scan themselves — an `$env.KEY` target, `export NAME`, a
    /// `${…}` place's root — must accept exactly what a `$name` read accepts, or a
    /// name can be read and not written. It used to be anchored to the
    /// compatibility lexer's ASCII-only scan instead, so `café = 5` bound a variable
    /// that `export CAFÉ = x` then refused as an invalid name.
    #[test]
    fn a_name_a_read_accepts_is_a_name_a_write_accepts() {
        for name in [
            "x", "MY-VAR", "PATH", "a_b", "a1-b2", "_private", "café", "CAFÉ",
        ] {
            assert!(valid_name(name), "{name} should be a name");
        }
        // Interior-only hyphens, an alphabetic or underscore head, and nothing else.
        for name in [
            "", "_", "_-x", "-x", "1x", "x-", "a--b", "a.b", "PATH[0]", "x:dedup",
        ] {
            assert!(!valid_name(name), "{name} should not be a name");
        }
        // And the read side agrees: each of these is one whole variable token.
        for name in ["x", "MY-VAR", "a1-b2", "_private", "café"] {
            let source = format!("${name}");
            assert_eq!(
                variable_end(&source, 0).unwrap(),
                source.len(),
                "${name} did not read as one name"
            );
        }
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

    /// Spacing is all the lexer has to tell the glob `*` from the multiplication
    /// `*`, so both spellings arrive as one operator token and the grammar decides:
    /// an operand slot takes the glob, the slot after a left operand takes the
    /// operator. A unit test because the two readings are indistinguishable from
    /// the shell once the glob has expanded.
    #[test]
    fn a_lone_star_is_a_glob_in_an_operand_slot_and_multiplication_after_a_value() {
        let tree = complete("xs = *\nn = 4 * 3\nitems = for f in * { $f }");
        let Executable::Assignment {
            value: Expr::Scalar(word),
            ..
        } = &tree.statements[0].and_or.first
        else {
            panic!("a lone `*` should be the bare glob word")
        };
        assert_eq!(
            word.value.pieces,
            vec![WordPiece::Text {
                text: "*".into(),
                quote: QuoteMode::Bare,
            }]
        );
        assert!(matches!(
            &tree.statements[1].and_or.first,
            Executable::Assignment {
                value: Expr::Binary {
                    op: BinaryOp::Multiply,
                    ..
                },
                ..
            }
        ));
        let Executable::Assignment {
            value: Expr::For { iterable, .. },
            ..
        } = &tree.statements[2].and_or.first
        else {
            panic!()
        };
        assert!(matches!(iterable.as_ref(), Expr::Scalar(_)));
    }

    /// The qualifier list is a fixed grammar rather than a call's arguments, so the
    /// shorthands, the long `type:` names, the `|` alternation and the boolean tests
    /// all land in one structure. A unit test because the shell shows only the paths
    /// that survived, which several different qualifier sets can agree on.
    #[test]
    fn a_glob_reads_its_attached_parentheses_as_qualifiers() {
        let tree = complete("xs = *(f, x)\nys = *(type: file|dir, empty: false)\nzs = f(x)");
        let Executable::Assignment {
            value: Expr::Scalar(word),
            ..
        } = &tree.statements[0].and_or.first
        else {
            panic!()
        };
        assert_eq!(
            word.value.qualifiers,
            Some(GlobQualifiers {
                types: vec![FileKind::File],
                exec: Some(true),
                empty: None,
            })
        );
        let Executable::Assignment {
            value: Expr::Scalar(word),
            ..
        } = &tree.statements[1].and_or.first
        else {
            panic!()
        };
        assert_eq!(
            word.value.qualifiers,
            Some(GlobQualifiers {
                types: vec![FileKind::File, FileKind::Dir],
                exec: None,
                empty: Some(false),
            })
        );
        // No glob syntax in the word, so the parentheses stay a call's.
        assert!(matches!(
            &tree.statements[2].and_or.first,
            Executable::Assignment {
                value: Expr::Call { .. },
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

    /// `$sh.x = …` is a **place**, so the grammar hands it on and the runtime
    /// decides which entries may be written. `$env` keeps its own path, where the
    /// byte-boundary rules live.
    #[test]
    fn the_shell_namespace_is_an_assignment_place_and_the_environment_is_not() {
        let tree = complete("$sh.options.bold-input = false");
        let Executable::MemberAssignment { target, append, .. } = &tree.statements[0].and_or.first
        else {
            panic!("expected a member assignment, got {:?}", tree.statements[0]);
        };
        assert_eq!(target, "$sh.options.bold-input");
        assert!(!append);

        // Even a runtime entry, which the runtime then refuses by name — the
        // grammar does not know which is which, and a syntax error here could not
        // say why.
        assert!(matches!(
            complete("$sh.status = 3").statements[0].and_or.first,
            Executable::MemberAssignment { .. }
        ));

        // `$env.KEY` is still its own assignment, not a member write.
        assert!(matches!(
            complete("$env.HOME = /tmp").statements[0].and_or.first,
            Executable::EnvAssignment { .. }
        ));
    }

    /// Both `$env` places, and the shapes that are not one. A computed key is
    /// carried as its raw subscript text, because which entry it names is a
    /// run-time question — the same one the matching *read* asks.
    #[test]
    fn an_env_place_is_a_member_or_one_subscript() {
        for (source, expected) in [
            ("$env.HOME = /tmp", EnvKey::Literal("HOME".into())),
            ("${env.MY-VAR} = x", EnvKey::Literal("MY-VAR".into())),
            ("export HOME = /tmp", EnvKey::Literal("HOME".into())),
            ("$env[$n] = x", EnvKey::Computed("$n".into())),
            ("$env[\"HOME\"] = x", EnvKey::Computed("\"HOME\"".into())),
            ("${env[$n]} += x", EnvKey::Computed("$n".into())),
        ] {
            let Executable::EnvAssignment { key, .. } =
                &complete(source).statements[0].and_or.first
            else {
                panic!("expected an environment assignment for {source}");
            };
            assert_eq!(key, &expected, "{source}");
        }

        // The same two shapes name an entry to remove, so a write and a removal
        // cannot disagree about what `$env[…]` means.
        for source in ["unset $env.HOME", "unset $env[$n]"] {
            let Executable::Unset { targets, .. } = &complete(source).statements[0].and_or.first
            else {
                panic!("expected an unset for {source}");
            };
            assert!(
                matches!(targets.as_slice(), [UnsetTarget::Env(_)]),
                "{source}"
            );
        }

        // Not places: a second access, a modifier, and a slice — each describes a
        // derived value, and the environment has nothing inside an entry to reach
        // into. They fall through to the expression parser, whose own error is the
        // one worth reporting.
        for source in [
            "$env.PATH[0] = /x",
            "$env.PATH:dedup = x",
            "$env[$n][0] = x",
            "$env[0..2] = x",
            "$env = x",
        ] {
            let parsed_as_env = matches!(parse(source), Ok(ParseOutcome::Complete(tree))
                if matches!(tree.statements[0].and_or.first, Executable::EnvAssignment { .. }));
            assert!(
                !parsed_as_env,
                "{source} parsed as an environment assignment"
            );
        }

        // And they are no more places for a removal than for a write — `unset`
        // asks the same `env_target`, so there is one answer for both.
        for source in ["unset $env", "unset $env.PATH[0]", "unset $env[0..2]"] {
            assert!(parse(source).is_err(), "{source} parsed as an unset");
        }
    }

    /// One word, two readings, and the operand decides which: `not` before a value is
    /// the [`UnaryOp::Not`] operator the expression parser owns, and `not` before
    /// anything else negates a command's status. Asserted on the tree because the two
    /// agree on the *answer* for a bool — `if not false` is true either way — so a
    /// behavioral test alone would not notice the command reading swallowing values.
    #[test]
    fn not_negates_a_command_only_when_no_value_follows() {
        // A condition and a statement are checked against the *same* operand list,
        // because one rule serving both positions is the claim being made.
        let negation = |source: &str| match &complete(source).statements[0].and_or.first {
            Executable::If(node) => (*node.condition).clone(),
            other => other.clone(),
        };

        for operand in [
            "ls",
            "test -f y",
            "ls | cat",
            "out = $(ls)",
            // A list-pattern binding: `value_start_in` answers for the `[` alone, so
            // without asking for the `=` behind it the negation was rewound and lost.
            "[head ...tail] = $xs",
        ] {
            for source in [format!("if {operand} {{ x }}"), operand.to_owned()] {
                let negated = source.replacen(operand, &format!("not {operand}"), 1);
                assert!(
                    !matches!(negation(&source), Executable::Not(_)),
                    "{source} negated without a `not`"
                );
                assert!(
                    matches!(negation(&negated), Executable::Not(_)),
                    "{negated} did not negate a command"
                );
            }
        }

        // A command needs a **word** to name it. Without one there is nothing whose
        // status could be negated, so the line is left to fail as the value it is
        // rather than becoming `command not found: =`.
        for source in ["if not = 5 { x }", "not = 5"] {
            assert!(parse(source).is_err(), "{source} parsed");
        }

        // A value operand keeps the expression reading, negations and all — and an
        // even run cancels rather than wrapping twice.
        for operand in ["$b", "false", "have(y)", "not ls"] {
            for source in [
                format!("if not {operand} {{ x }}"),
                format!("not {operand}"),
            ] {
                assert!(
                    !matches!(negation(&source), Executable::Not(_)),
                    "{source} was read as a negated command"
                );
            }
        }

        // Where the two positions already disagreed, the negation inherits the
        // disagreement rather than papering over it: a spaced `>` is a comparison in
        // a condition and a redirection in a statement, so `not $n > 2` negates a
        // comparison in one and a redirected command in the other — exactly what each
        // position does without the `not`.
        assert!(!matches!(
            negation("if not $n > 2 { x }"),
            Executable::Not(_)
        ));
        assert!(matches!(negation("not $n > 2"), Executable::Not(_)));
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
