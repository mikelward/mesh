//! The variable store.
//!
//! A session-global scope of typed scalar and collection variables, plus a stack of
//! **function-local** scopes pushed for the duration of a `func` call. Reads
//! resolve the innermost local scope, then the global scope — a callee never
//! sees its caller's locals (lexical, not dynamic). Writes land in the innermost
//! scope (a function-local when one is active, else the global). The `$sh`
//! namespace holds the shell's own state: read-only apart from the
//! [`options`](crate::options) settings map, per `DESIGN.md` §"Variables and
//! assignment". The rest of the `$sh.*` surface — the hook maps, `$sh.complete`,
//! `$sh.signal` — is deferred to later tasks.

use std::collections::HashMap;
use std::sync::Arc;

use crate::options::Options;

#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    Integer(i64),
    Boolean(bool),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
    Regex(RegexValue),
    Glob(String),
    /// One of the shell's own streams (`$sh.stdin` and friends), carrying the
    /// descriptor it names. A handle has **no canonical byte form**, so it never
    /// renders to argv or into a string — `DESIGN.md` §"Values and the bytes
    /// boundary" lists it beside a regex for exactly that reason. The descriptor
    /// stays private: the value answers questions (`:tty`) rather than being one.
    Stream(i32),
    /// A **job handle** — `$j` from `j = cmd &`, or `$sh.jobs[2]`. It carries
    /// the job id and nothing else: reading a member resolves it against the
    /// live table, so `$j.state` answers for the job as it is now rather than as
    /// it was when the handle was bound. Like a stream handle it has **no byte
    /// form**, which is what makes `kill $j` a job and `kill 49001` a pid
    /// without either having to guess.
    Job(usize),
    /// An anonymous `func(params) { … }` bound to a name — value-called through
    /// the variable (`$double(5)`). It has no byte form, so unlike every other
    /// value it cannot reach a command argument or an interpolation.
    Function(FuncValue),
    /// A **string carrying display attributes**, from `style("text", fg: blue)`.
    ///
    /// Not a new scalar type, per `DESIGN.md` §"Hooks and the prompt": everywhere
    /// bytes are wanted it behaves exactly as its text — the same argv rule, the
    /// same `+=` (which yields a plain string, since attributes are
    /// rendering-only), the same comparisons and interpolation. Only a renderer
    /// reads the attributes, which today means `puts`/`print` writing to a
    /// color-capable terminal.
    ///
    /// Boxed because it is the largest payload by some way and every `Value`
    /// would otherwise grow to hold it.
    Styled(Box<StyledValue>),
}

/// Equality is **hand-written rather than derived** so that a styled value equals
/// its text: `style(x, fg: red) == x` is true, per `DESIGN.md` §"Hooks and the
/// prompt". Deriving it would make styling a value change every comparison it
/// takes part in, which is exactly what "behaves exactly as its text" forbids.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        if let (Some(left), Some(right)) = (self.as_text(), other.as_text()) {
            return left == right;
        }
        match (self, other) {
            (Value::Integer(left), Value::Integer(right)) => left == right,
            (Value::Boolean(left), Value::Boolean(right)) => left == right,
            (Value::List(left), Value::List(right)) => left == right,
            (Value::Map(left), Value::Map(right)) => left == right,
            (Value::Regex(left), Value::Regex(right)) => left == right,
            (Value::Glob(left), Value::Glob(right)) => left == right,
            (Value::Stream(left), Value::Stream(right)) => left == right,
            (Value::Job(left), Value::Job(right)) => left == right,
            (Value::Function(left), Value::Function(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for Value {}

/// Hashed to agree with [`PartialEq`], which is what `:dedup` and a map key
/// depend on: a styled value and its plain text are one key, not two. The
/// discriminants are written out because a styled value must land in the *string*
/// bucket rather than its own.
impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        if let Some(text) = self.as_text() {
            0u8.hash(state);
            text.hash(state);
            return;
        }
        match self {
            // Covered by `as_text` above.
            Value::String(_) | Value::Styled(_) => unreachable!("a text value hashes as its text"),
            Value::Integer(value) => (1u8, value).hash(state),
            Value::Boolean(value) => (2u8, value).hash(state),
            Value::List(values) => (3u8, values).hash(state),
            Value::Map(entries) => (4u8, entries).hash(state),
            Value::Regex(value) => (5u8, value).hash(state),
            Value::Glob(value) => (6u8, value).hash(state),
            Value::Stream(value) => (7u8, value).hash(state),
            Value::Job(value) => (8u8, value).hash(state),
            Value::Function(value) => (9u8, value).hash(state),
        }
    }
}

/// Text plus the attributes a renderer may apply to it.
///
/// Equality and hashing are **by text alone**, which is what makes a styled value
/// compare as its text: `style(x, fg: red) == x` is true, so styling a prompt
/// segment cannot quietly change a comparison somewhere else.
#[derive(Debug, Clone)]
pub struct StyledValue {
    pub text: String,
    pub style: Style,
}

impl PartialEq for StyledValue {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for StyledValue {}

impl std::hash::Hash for StyledValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

/// The display attributes a renderer may apply. Color and bold are `style`'s MVP
/// set (`DESIGN.md` §"Hooks and the prompt"); `link` is `link()`'s. `None` for a
/// color means "leave it alone" rather than "default", so a nested re-style can add
/// bold without resetting the color.
///
/// Not `Copy`, because the URL is owned — the one attribute that is not a flag.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub bold: bool,
    /// The `OSC 8` target, already percent-encoded and known to carry a scheme.
    pub link: Option<String>,
}

/// What a renderer may emit on the stream it is writing to.
///
/// Two bits rather than one, because the two escapes answer to different things.
/// `NO_COLOR` silences the **palette**, not every sequence — a hyperlink is not
/// color, and dropping it would lose the URL rather than make output plainer. A
/// stream that is not a terminal takes neither.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Decoration {
    /// SGR: color and bold.
    pub color: bool,
    /// `OSC 8` hyperlinks.
    pub links: bool,
}

impl Decoration {
    /// Emit nothing — what a pipe, a redirect and a capture all get.
    pub fn plain() -> Self {
        Self::default()
    }
}

/// The eight ANSI colors and their bright forms — the set every terminal that
/// does color at all agrees on. 256-color and truecolor are deferred: they need a
/// spelling for the value (`fg: 33`, `fg: "#8be9fd"`) and a downgrade rule for
/// terminals that cannot show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl Color {
    /// The color named by a `fg:`/`bg:` argument, or `None` if it names nothing.
    ///
    /// `grey`/`gray` are accepted for bright black: it is what the color is called
    /// everywhere outside a terminal spec, and both spellings are in wide use.
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "white" => Color::White,
            "grey" | "gray" | "bright-black" => Color::BrightBlack,
            "bright-red" => Color::BrightRed,
            "bright-green" => Color::BrightGreen,
            "bright-yellow" => Color::BrightYellow,
            "bright-blue" => Color::BrightBlue,
            "bright-magenta" => Color::BrightMagenta,
            "bright-cyan" => Color::BrightCyan,
            "bright-white" => Color::BrightWhite,
            _ => return None,
        })
    }

    /// Every name `style` accepts, for the diagnostic that lists them.
    pub const NAMES: &'static [&'static str] = &[
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "grey",
        "bright-red",
        "bright-green",
        "bright-yellow",
        "bright-blue",
        "bright-magenta",
        "bright-cyan",
        "bright-white",
    ];

    /// The SGR parameter for this color in the foreground.
    fn foreground_code(self) -> u8 {
        match self {
            Color::Black => 30,
            Color::Red => 31,
            Color::Green => 32,
            Color::Yellow => 33,
            Color::Blue => 34,
            Color::Magenta => 35,
            Color::Cyan => 36,
            Color::White => 37,
            Color::BrightBlack => 90,
            Color::BrightRed => 91,
            Color::BrightGreen => 92,
            Color::BrightYellow => 93,
            Color::BrightBlue => 94,
            Color::BrightMagenta => 95,
            Color::BrightCyan => 96,
            Color::BrightWhite => 97,
        }
    }
}

impl Style {
    /// Does this carry anything to emit? A `style()` call that named nothing
    /// renders as bare text rather than as an empty `ESC [ m`.
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }

    /// `text` wrapped in whatever `decoration` allows — the one place attributes
    /// become bytes.
    ///
    /// The link goes **outermost** so the whole run is clickable including any
    /// color, and each escape is emitted only if its own bit is set: an
    /// `OSC 8` link survives `NO_COLOR` while the SGR does not.
    ///
    /// **Deliberately not wrapped for a multiplexer**, unlike the `OSC 9`
    /// notification. The difference is that `OSC 8` brackets text that has to end up
    /// in the pane, and the `DCS tmux;` envelope forwards its payload to the outer
    /// terminal *instead of* drawing it — so wrapping would lose the link text
    /// entirely. Measured against tmux 3.4: raw, tmux parses the sequence, stores the
    /// hyperlink per cell and re-emits it (`capture-pane -e` shows it back), which is
    /// also what keeps the link alive across a scroll or a resize that tmux repaints
    /// itself; wrapped, nothing reaches the pane at all, with or without
    /// `allow-passthrough`. An older tmux that does not implement `OSC 8` discards it
    /// and the text still lands, which is the right failure.
    pub fn render(&self, text: &str, decoration: Decoration) -> String {
        let sgr = if decoration.color {
            self.sgr()
        } else {
            String::new()
        };
        let link = decoration.links.then_some(self.link.as_deref()).flatten();
        let mut out = String::with_capacity(text.len() + 16);
        if let Some(url) = link {
            out.push_str("\u{1b}]8;;");
            out.push_str(url);
            out.push_str("\u{1b}\\");
        }
        out.push_str(&sgr);
        out.push_str(text);
        if !sgr.is_empty() {
            // `SGR 0` rather than the targeted off-codes: a full reset is what every
            // prompt idiom uses, and it cannot leave an attribute a partial reset
            // missed.
            out.push_str("\u{1b}[0m");
        }
        if link.is_some() {
            // The empty URI is how `OSC 8` says "the link ends here".
            out.push_str("\u{1b}]8;;\u{1b}\\");
        }
        out
    }

    /// The SGR sequence that turns the color attributes on, empty when there are
    /// none. Backgrounds are the foreground codes plus ten, the ANSI offset.
    fn sgr(&self) -> String {
        let mut parameters = Vec::with_capacity(3);
        if self.bold {
            parameters.push(1);
        }
        if let Some(color) = self.foreground {
            parameters.push(color.foreground_code().into());
        }
        if let Some(color) = self.background {
            parameters.push(u16::from(color.foreground_code()) + 10);
        }
        if parameters.is_empty() {
            // Never a bare `ESC [ m`, which some terminals read as a reset.
            return String::new();
        }
        let parameters: Vec<String> = parameters.iter().map(u16::to_string).collect();
        format!("\u{1b}[{}m", parameters.join(";"))
    }
}

/// How long an `OSC 8` target may be. The practical URL ceiling, and well under
/// what terminals accept — past their own limits a terminal drops the whole
/// sequence, taking the link text with it, so a refusal naming the size beats
/// output that silently lost a word.
pub const LINK_LIMIT: usize = 2083;

/// May this byte stand for itself in a URI?
///
/// RFC 3986's own three sets, and nothing else: **unreserved** (`ALPHA`, `DIGIT`,
/// `-._~`), **gen-delims** (`:/?#[]@`) and **sub-delims** (`!$&'()*+,;=`) — the
/// characters that carry a URL's structure — plus `%`, so a target that already
/// spells `%20` keeps it rather than having the escape re-escaped into `%2520`.
///
/// Everything else is encoded, which is not only about control bytes. A **space** is
/// the common case: `file://host/My File` is an ordinary path and an invalid URI, and
/// a terminal is entitled to reject the whole link over it. `"`, `<`, `>`, `\`, `^`,
/// `` ` ``, `{`, `|` and `}` are the same story — printable, and forbidden raw.
fn uri_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            // unreserved
            b'-' | b'.' | b'_' | b'~'
            // gen-delims
            | b':' | b'/' | b'?' | b'#' | b'[' | b']' | b'@'
            // sub-delims
            | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
            // an existing escape
            | b'%'
        )
}

/// A URL made safe to put in an `OSC 8` payload, or the reason it cannot be.
///
/// Two rules:
///
/// - **Anything RFC 3986 does not allow raw is percent-encoded** — see
///   [`uri_safe`]. That covers the sequence's own requirement, since an `ESC` or a
///   `BEL` in the URL would end mesh's sequence and leave the rest on screen (the
///   injection the title payload also guards against), *and* the ordinary case of a
///   space in a path, which a terminal is entitled to reject the whole link over.
///   The delimiters that carry a URL's structure are kept, so `?`, `&`, `=`, `#` and
///   an existing `%20` all survive as written.
/// - **A scheme is required.** A terminal needs an absolute URI to do anything with,
///   so a bare path is a link that silently does not work — worth saying rather than
///   guessing at `file://`, which would also need a hostname to be correct over
///   `ssh`.
pub fn link_url(url: &str) -> Result<String, String> {
    let Some((scheme, _)) = url.split_once(':') else {
        return Err(format!(
            "`{url}` has no scheme; a terminal needs an absolute URL like \
             `https://…` or `file://host/path`"
        ));
    };
    let named = !scheme.is_empty()
        && scheme.starts_with(|first: char| first.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !named {
        return Err(format!(
            "`{url}` does not start with a scheme; a terminal needs an absolute URL \
             like `https://…` or `file://host/path`"
        ));
    }
    let mut encoded = String::with_capacity(url.len());
    for byte in url.bytes() {
        if uri_safe(byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    if encoded.len() > LINK_LIMIT {
        return Err(format!(
            "URL is {} bytes encoded, over the {LINK_LIMIT}-byte limit",
            encoded.len()
        ));
    }
    Ok(encoded)
}

/// Why a value cannot be written as a literal, in the words a diagnostic wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoLiteral {
    Regex,
    Glob,
    Stream,
    Job,
    Function,
}

impl std::fmt::Display for NoLiteral {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            NoLiteral::Regex => "a regex has no literal form",
            NoLiteral::Glob => "a glob has no literal form",
            NoLiteral::Stream => "a stream handle has no literal form",
            NoLiteral::Job => "a job handle has no literal form",
            NoLiteral::Function => "a function has no literal form",
        };
        f.write_str(reason)
    }
}

impl Value {
    /// The plain string a **styled** value behaves as; every other value unchanged.
    ///
    /// `DESIGN.md` §"Hooks and the prompt" makes a styled value a string carrying
    /// display attributes, read only by a renderer. So every boundary that wants
    /// *data* — argv, `+=`, a comparison, an interpolation, an index — calls this
    /// first and then has one fewer case to think about. Keeping the wrapper past
    /// such a boundary is the bug this exists to make hard.
    pub fn plain(self) -> Value {
        match self {
            Value::Styled(styled) => Value::String(styled.text),
            other => other,
        }
    }

    /// The text of a styled value, or `None` for anything else — the borrowing
    /// form of [`plain`](Value::plain), for a match that only needs to route a
    /// styled value to its own `Value::String` arm.
    pub fn styled_text(&self) -> Option<&str> {
        match self {
            Value::Styled(styled) => Some(&styled.text),
            _ => None,
        }
    }

    /// The text of any value that **is** text — a string or a styled string.
    ///
    /// The pair that has to answer alike for equality, hashing and ordering, since
    /// a styled value is a string carrying attributes and not a kind of its own.
    /// An integer is deliberately absent: `1 == "1"` is false in mesh and this must
    /// not be the thing that changes it.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            Value::Styled(styled) => Some(&styled.text),
            _ => None,
        }
    }

    /// This value written as the mesh source you would have typed for it.
    ///
    /// The contract is **round-trip, not display**: parsing the result back has to
    /// yield an equal value. That is what forces the quoting to be exact — `42`
    /// and `"42"` are different values, so a string is always quoted even when it
    /// would lex as a bare word, and the empty map is `[:]` rather than the `[]`
    /// that would read back as the empty list.
    ///
    /// Not every value has a literal form, and the ones that do not are exactly
    /// the ones with nothing to write down: `DESIGN.md` §"Isolation and subshells"
    /// draws the line at values whose spelling round-trips. Those answer `Err`
    /// carrying the reason for a diagnostic, never a lossy approximation that
    /// would parse back as something else. The invariant callers get is the useful
    /// one: **every `Ok` round-trips.**
    pub fn to_literal(&self) -> Result<String, NoLiteral> {
        let mut out = String::new();
        self.write_literal(&mut out)?;
        Ok(out)
    }

    fn write_literal(&self, out: &mut String) -> Result<(), NoLiteral> {
        match self {
            Value::String(text) => quote_into(text, out),
            // A styled value writes down as its **text**. `:repr`'s contract is
            // that parsing the result yields an *equal* value, and equality here is
            // by text — a `style(…)` call is not a literal anyway, so there is
            // nothing else to write that would read back.
            Value::Styled(styled) => quote_into(&styled.text, out),
            Value::Integer(number) => out.push_str(&number.to_string()),
            Value::Boolean(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Value::List(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    value.write_literal(out)?;
                }
                out.push(']');
            }
            // `[]` is the empty *list*, so the empty map needs its own spelling or
            // the two collapse into one text (`DESIGN.md:1078`).
            Value::Map(entries) if entries.is_empty() => out.push_str("[:]"),
            Value::Map(entries) => {
                out.push('[');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    // Quoted rather than bare: a key is an arbitrary string, and
                    // one holding a space or a `:` would otherwise not read back.
                    quote_into(key, out);
                    out.push_str(": ");
                    value.write_literal(out)?;
                }
                out.push(']');
            }
            // A regex has a `/…/` spelling, but not one that survives this trip.
            // Its flags ride on `:` modifiers that are not implemented yet, so a
            // case-insensitive regex would come back case-sensitive; and a bare
            // `/…/` in value position is read under the path/glob rule rather than
            // as a regex. Writing it would be silently wrong, so it is refused.
            Value::Regex(_) => return Err(NoLiteral::Regex),
            // Globbing is eager: writing the pattern back would re-glob it in the
            // reader, against the rule that a value never re-globs
            // (`DESIGN.md` §"Globbing").
            Value::Glob(_) => return Err(NoLiteral::Glob),
            Value::Stream(_) => return Err(NoLiteral::Stream),
            // A handle names a job in *this* shell's table, so the id is the one
            // thing about it that could be written and the one thing that means
            // nothing anywhere else. Round-tripping it would hand back a
            // reference to whatever job happened to hold that id.
            Value::Job(_) => return Err(NoLiteral::Job),
            Value::Function(_) => return Err(NoLiteral::Function),
        }
        Ok(())
    }
}

/// Write `text` as a single-quoted mesh string.
///
/// `'…'` rather than `"…"` because it does not interpolate: a `$` in the value
/// stays a `$` in the literal with nothing to escape and no chance of the reader
/// expanding it. The escape set is exactly the one `lex_escaped` decodes for this
/// quote — `\n \t \r \e \\ \'` plus `\u{…}` — so this is its inverse.
fn quote_into(text: &str, out: &mut String) {
    use std::fmt::Write;

    out.push('\'');
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x1b' => out.push_str("\\e"),
            // The rest of the control characters have no escape of their own, and
            // writing one raw would put a real control byte in the text — invisible
            // to read and, on the value channel this exists for, a byte the reader
            // would have to carry verbatim.
            other if other.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", other as u32);
            }
            other => out.push(other),
        }
    }
    out.push('\'');
}

/// A function value: something callable with one thing to do.
///
/// Shared behind an `Arc` because binding or passing one copies the `Value` and a
/// lambda body is a whole parsed `Source`; atomic rather than an `Rc` because the
/// interactive completer holds shell values across a thread boundary.
///
/// Identity, not structure, is what equality means here — two separately written
/// lambdas with the same text are different functions, the answer every language
/// with first-class functions gives — which also keeps `Hash` cheap and
/// consistent with `Eq`. That extends to modifier references: `:stem` written
/// twice is two values, since they are two references rather than one shared one.
#[derive(Debug, Clone)]
pub struct FuncValue(Arc<Callable>);

#[derive(Debug)]
enum Callable {
    /// A written `func(params) { body }`.
    Lambda {
        params: Vec<crate::parser::Param>,
        body: crate::parser::Source,
    },
    /// A bare `:name` reference — the function that applies that modifier to its
    /// one argument. Held as the name rather than a synthesized lambda body: there
    /// is nothing to parse, and applying a modifier is a direct call.
    Modifier(String),
}

impl FuncValue {
    pub fn lambda(params: Vec<crate::parser::Param>, body: crate::parser::Source) -> Self {
        Self(Arc::new(Callable::Lambda { params, body }))
    }

    pub fn modifier(name: String) -> Self {
        Self(Arc::new(Callable::Modifier(name)))
    }

    /// The signature and body, when this is a written lambda.
    pub fn as_lambda(&self) -> Option<(&[crate::parser::Param], &crate::parser::Source)> {
        match &*self.0 {
            Callable::Lambda { params, body } => Some((params, body)),
            Callable::Modifier(_) => None,
        }
    }

    /// The modifier name, when this is a bare reference.
    pub fn modifier_name(&self) -> Option<&str> {
        match &*self.0 {
            Callable::Modifier(name) => Some(name),
            Callable::Lambda { .. } => None,
        }
    }
}

impl PartialEq for FuncValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for FuncValue {}

impl std::hash::Hash for FuncValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegexValue {
    pub pattern: String,
    pub case_insensitive: bool,
    pub multi_line: bool,
    pub dot_matches_new_line: bool,
    pub ignore_whitespace: bool,
}

impl RegexValue {
    pub fn new(pattern: String) -> Self {
        Self {
            pattern,
            case_insensitive: false,
            multi_line: false,
            dot_matches_new_line: false,
            ignore_whitespace: false,
        }
    }
}

type Scope = HashMap<String, Value>;

/// Names the shell owns rather than the user: `$env` is the process
/// environment, `$sh` is the shell's own state. Neither can be bound by an
/// assignment, a function parameter, or a pattern.
///
/// Reserved is about the *name*, not about mutability: `$sh.options.KEY = …`
/// writes into the namespace without rebinding `sh`, which is why the parser's
/// member-assignment place check excludes only `env` (whose own byte-boundary
/// rules apply) rather than consulting this.
pub fn is_reserved_namespace(name: &str) -> bool {
    matches!(name, "env" | "sh")
}

/// The shell-or-script name when nothing named it — bash's `$0` for an
/// interactive shell.
const DEFAULT_SHELL_NAME: &str = "mesh";

/// Variable bindings: one session-global scope plus a stack of function-local
/// scopes (one per active `func` call), alongside the read-only `$sh` namespace.
pub struct Vars {
    global: Scope,
    locals: Vec<Scope>,
    shell: Vec<(String, Value)>,
    status: u8,
    stages: Vec<u8>,
    interactive: bool,
    /// Whether this process is the interactive REPL driving a terminal it has
    /// **taken** — foreground group acquired, terminal signals configured.
    ///
    /// A stricter question than `interactive`, and a separate one since `-i`:
    /// that flag says what *kind* of session this is, while everything that
    /// presupposes a keyboard and a controlling terminal has to ask this instead.
    /// `mesh -i script.mesh` is an interactive session that never took a
    /// terminal, so it must not put a `fork` child in its own process group —
    /// that group is excluded from a `SIGINT` sent to the invocation's, which
    /// kills the shell and orphans the child.
    owns_terminal: bool,
    /// Captured once rather than read per access, so they stay the *shell's* —
    /// bash's `$$` / `$PPID`, which do not change inside a subshell. A forked
    /// pipeline stage inherits this copy, so `$sh.pid` there still names the
    /// session rather than the short-lived stage.
    pid: u32,
    ppid: i32,
    /// The effective user owns the shell's filesystem and process privileges.
    /// Capture it with the process ids so a forked stage reports the session's
    /// identity rather than consulting its own process state.
    uid: libc::uid_t,
    /// The live job table as `$sh.jobs` reports it. A snapshot rather than a
    /// live borrow, because expansion is handed only the variable store — the
    /// shell refreshes it from the real table on the same funnel that publishes
    /// `$sh.status`, so a read never sees a stale one.
    jobs: Vec<(usize, Value)>,
    /// The stack of inputs being evaluated, innermost last, reported as
    /// `$sh.origin` and `$sh.source`. A stack because `source` nests: a file that
    /// sources another must see the *inner* one while it runs and its own again
    /// afterwards, which is what `${BASH_SOURCE[0]}` is used for.
    ///
    /// `DESIGN.md` leaves the nesting question open and asks for the two axes to
    /// stay orthogonal: this reports the **innermost** frame, so "where am I" has
    /// one answer, and interactivity stays separately in `interactive`.
    inputs: Vec<Input>,
    /// The `$sh.options` settings, shared rather than owned: the line editor
    /// holds a reader too, so a write here reaches the next keystroke. See
    /// [`crate::options`].
    options: Arc<Options>,
}

/// One level of input: where it came from, and its path when it has one.
#[derive(Debug, Clone)]
struct Input {
    origin: Origin,
    /// The file's path, empty for the origins that are not files.
    source: String,
    /// How many lines of *this* input came before the unit being run, so a
    /// diagnostic can name the line in the whole input rather than in the
    /// fragment. Per-frame rather than per-shell because a sourced file is its
    /// own input: without that, `puts ok` followed by `source bad.mesh` reported
    /// the sourced file's line 1 as line 2, having carried stdin's count into it.
    line_offset: usize,
}

/// Where the source being evaluated came from — `DESIGN.md`'s input **origin**,
/// deliberately not the same question as interactivity (`mesh -i script.mesh` is
/// both interactive and a script).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A script path operand.
    Script,
    /// A file pulled in by `source`, including the startup files.
    Sourced,
    /// `-c TEXT`.
    Command,
    /// `-s`, or piped input.
    Stdin,
    /// Typed at a prompt.
    Interactive,
}

impl Origin {
    /// The word `$sh.origin` reports.
    fn word(self) -> &'static str {
        match self {
            Origin::Script => "script",
            Origin::Sourced => "sourced",
            Origin::Command => "command",
            Origin::Stdin => "stdin",
            Origin::Interactive => "interactive",
        }
    }
}

impl Default for Vars {
    fn default() -> Self {
        Self {
            global: Scope::new(),
            locals: Vec::new(),
            shell: invocation_entries(DEFAULT_SHELL_NAME.to_owned(), Vec::new()),
            status: 0,
            stages: vec![0],
            interactive: false,
            owns_terminal: false,
            pid: std::process::id(),
            // SAFETY: `getppid` takes no arguments and cannot fail.
            ppid: unsafe { libc::getppid() },
            // SAFETY: `geteuid` takes no arguments and cannot fail.
            uid: unsafe { libc::geteuid() },
            jobs: Vec::new(),
            // A shell with nothing said about it reads commands from stdin, which
            // is what `Invocation::Default` falls back to.
            inputs: vec![Input {
                line_offset: 0,
                origin: Origin::Stdin,
                source: String::new(),
            }],
            options: Arc::new(Options::default()),
        }
    }
}

/// The terminal's column count, or `0` when there is no terminal to ask.
///
/// Asked of stdout first, then stderr, then stdin: the width that matters is the
/// terminal the user is *looking at*, and a redirected stdout answers `ENOTTY`
/// rather than the terminal behind it. `mesh script.mesh | less` is the case the
/// fallback exists for — stdout is a pipe, and a script sizing a progress line or
/// a table it writes to stderr still needs the real width.
///
/// `0` rather than a conventional 80: a width of zero is not a width, so it says
/// "there is no terminal" without a caller having to know which number was made
/// up. `if $sh.width == 0` is the test, and a prompt that wants a default can say
/// so itself.
fn terminal_width() -> u16 {
    for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO, libc::STDIN_FILENO] {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: `TIOCGWINSZ` writes a `winsize` through the pointer, which is
        // what is passed; a bad descriptor or a non-terminal is a return of -1
        // rather than a write.
        if unsafe { libc::ioctl(fd, mesh_platform::TIOCGWINSZ, &raw mut size) } == 0
            && size.ws_col > 0
        {
            return size.ws_col;
        }
    }
    0
}

/// The `$sh` entries that describe how mesh was invoked. Only these exist today;
/// the rest of the `$sh.*` surface in `DESIGN.md` is deferred.
fn invocation_entries(name: String, args: Vec<String>) -> Vec<(String, Value)> {
    vec![
        ("name".to_owned(), Value::String(name)),
        (
            "args".to_owned(),
            Value::List(args.into_iter().map(Value::String).collect()),
        ),
    ]
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record how mesh was invoked: `$sh.name` is the script or shell name and
    /// `$sh.args` the positional arguments, as a real list.
    pub fn set_invocation(&mut self, name: String, args: Vec<String>) {
        self.shell = invocation_entries(name, args);
    }

    /// Record the status the next command sees as `$sh.status`, and the
    /// per-stage breakdown `$sh.pipestatus` reports. Kept as one call because
    /// the two must agree: `$sh.pipestatus` always describes the run that
    /// produced the current `$sh.status`.
    pub(crate) fn set_status(&mut self, status: u8, stages: Vec<u8>) {
        self.status = status;
        self.stages = stages;
    }

    /// Record that this session is interactive, which `$sh.interactive` reports.
    /// Set by the shell rather than derived from `isatty`: the question is which
    /// loop is running, not what fd 0 happens to be — `mesh -s` on a terminal
    /// reads commands without being an interactive session.
    pub fn set_interactive(&mut self, interactive: bool) {
        self.interactive = interactive;
    }

    /// Record that this process has taken the terminal: the interactive loop
    /// calls it once, after `wait_until_foreground` and `ignore_interactive_signals`
    /// have both succeeded. Nothing else may — the flag is a claim about what this
    /// process actually did, not about what kind of session it is.
    pub fn set_owns_terminal(&mut self) {
        self.owns_terminal = true;
    }

    /// Whether this process owns terminal job control, as recorded above.
    pub(crate) fn owns_terminal(&self) -> bool {
        self.owns_terminal
    }

    /// The live `$sh.options` settings. Handed out as the shared `Arc` rather
    /// than a copy, because the line editor keeps its own reader — see
    /// [`crate::options`].
    pub fn options(&self) -> &Arc<Options> {
        &self.options
    }

    /// Replace the outermost input — the one the invocation itself established.
    pub fn set_origin(&mut self, origin: Origin, source: String) {
        self.inputs = vec![Input {
            origin,
            source,
            line_offset: 0,
        }];
    }

    /// Enter a nested input for the duration of a `source`, so `$sh.origin` reads
    /// `sourced` and `$sh.source` names *that* file while it runs.
    pub(crate) fn push_input(&mut self, origin: Origin, source: String) {
        self.inputs.push(Input {
            origin,
            source,
            line_offset: 0,
        });
    }

    /// Leave it again. The outermost frame is never popped: something is always
    /// being evaluated, so there is always an origin to report.
    pub(crate) fn pop_input(&mut self) {
        if self.inputs.len() > 1 {
            self.inputs.pop();
        }
    }

    /// Publish the live jobs, keyed by job id in registration order.
    /// The record a job handle resolves to, or `None` once the job has left the
    /// table. Reads the same published snapshot `$sh.jobs` does, so a handle and
    /// the table can never disagree about a job.
    pub(crate) fn job_record(&self, id: usize) -> Option<Value> {
        self.jobs
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, record)| record.clone())
    }

    pub(crate) fn set_jobs(&mut self, jobs: Vec<(usize, Value)>) {
        self.jobs = jobs;
    }

    /// The currently published `$sh.status`.
    pub(crate) fn status(&self) -> u8 {
        self.status
    }

    /// Take a copy of both runtime entries, to put back with
    /// [`Vars::restore_status`] after running something that is the shell's own
    /// bookkeeping rather than the user's command.
    pub(crate) fn status_snapshot(&self) -> (u8, Vec<u8>) {
        (self.status, self.stages.clone())
    }

    pub(crate) fn restore_status(&mut self, (status, stages): (u8, Vec<u8>)) {
        self.status = status;
        self.stages = stages;
    }

    /// The innermost input's file path, empty for the origins that have none —
    /// what `$sh.source` reports, read directly for a diagnostic that has to name
    /// where it is.
    pub(crate) fn input_source(&self) -> String {
        self.input().source.clone()
    }

    /// How many lines of the innermost input came before the unit being run.
    pub(crate) fn input_line_offset(&self) -> usize {
        self.input().line_offset
    }

    /// Advance the innermost input's line count by a unit just consumed. Only the
    /// piped reader calls it: every other caller hands its input over whole.
    pub(crate) fn advance_input_lines(&mut self, lines: usize) {
        if let Some(input) = self.inputs.last_mut() {
            input.line_offset += lines;
        }
    }

    /// Count a line that `gets` took straight off descriptor 0.
    ///
    /// It belongs to the **outermost** input, which is the one being read from
    /// that descriptor — a `gets` inside a sourced file still consumes from the
    /// stream the outer session is reading. And only when that input *is* stdin:
    /// under `mesh script.mesh` the data `gets` reads has nothing to do with the
    /// script's own lines, so counting it would push every later diagnostic down
    /// the file by the size of the data.
    pub(crate) fn count_stdin_line(&mut self) {
        if let Some(outermost) = self.inputs.first_mut()
            && outermost.origin == Origin::Stdin
        {
            outermost.line_offset += 1;
        }
    }

    /// The innermost input's origin word, as `$sh.origin` reports it.
    pub(crate) fn input_origin(&self) -> &'static str {
        self.input().origin.word()
    }

    /// Is the innermost input a **sourced file**? That is the one top level a
    /// `return` may leave, because it is the only one with an invoker to return to
    /// — the `source` command, whose status becomes the returned value's.
    pub(crate) fn in_sourced_file(&self) -> bool {
        self.input().origin == Origin::Sourced
    }

    fn input(&self) -> &Input {
        self.inputs
            .last()
            .expect("the outermost input is never popped")
    }

    /// The `$sh` namespace as an ordered map, so member access, indexing, and
    /// modifiers all work through the usual map and list paths.
    ///
    /// A **snapshot**, not the namespace itself: the runtime entries are built
    /// here rather than stored in `shell`, so recording a status after every
    /// command is two field writes instead of a keyed update of the map. That is
    /// also why a write to `$sh.options` cannot go through this value — it would
    /// land in a copy that is discarded — and takes the [`Options`] path instead.
    pub(crate) fn shell_namespace(&self) -> Value {
        let mut entries = vec![
            ("status".to_owned(), Value::Integer(i64::from(self.status))),
            (
                "pipestatus".to_owned(),
                Value::List(
                    self.stages
                        .iter()
                        .map(|code| Value::Integer(i64::from(*code)))
                        .collect(),
                ),
            ),
            ("pid".to_owned(), Value::Integer(i64::from(self.pid))),
            ("ppid".to_owned(), Value::Integer(i64::from(self.ppid))),
            ("uid".to_owned(), Value::Integer(i64::from(self.uid))),
            (
                "version".to_owned(),
                Value::String(env!("CARGO_PKG_VERSION").to_owned()),
            ),
            ("interactive".to_owned(), Value::Boolean(self.interactive)),
            // Read here, on each access, rather than cached and refreshed on
            // `SIGWINCH`. `TIOCGWINSZ` is what the kernel answers from, and it is
            // current the instant the window changes — `SIGWINCH` is only the
            // notification that it did. A cache would need the signal to stay
            // honest; a live read cannot be stale, and costs one syscall against
            // the fork per prompt that `tput cols` was.
            (
                "width".to_owned(),
                Value::Integer(i64::from(terminal_width())),
            ),
            // Handles rather than integers: `DESIGN.md` puts a stream handle in
            // the same row as a regex — no byte form — so `puts $sh.stdin` is a
            // loud error and `:tty` is the way to ask about one.
            ("stdin".to_owned(), Value::Stream(0)),
            ("stdout".to_owned(), Value::Stream(1)),
            ("stderr".to_owned(), Value::Stream(2)),
            // Handles, not the records themselves. `DESIGN.md` calls
            // `$sh.jobs[2]` a handle, and it can only behave like one — live,
            // and usable as a job reference after being stored — if that is what
            // indexing yields. The records stay private, behind `job_record`,
            // which is what a handle reads through.
            (
                "jobs".to_owned(),
                Value::Map(
                    self.jobs
                        .iter()
                        .map(|(id, _)| (id.to_string(), Value::Job(*id)))
                        .collect(),
                ),
            ),
            // The innermost input: what is being evaluated *now*, which is the
            // question a sourced file asks about itself.
            (
                "origin".to_owned(),
                Value::String(self.input().origin.word().to_owned()),
            ),
            (
                "source".to_owned(),
                Value::String(self.input().source.clone()),
            ),
            // The one writable entry. Read like any other map, but assigned
            // through `Options` rather than into this snapshot.
            ("options".to_owned(), Value::Map(self.options.entries())),
        ];
        entries.extend(self.shell.iter().cloned());
        Value::Map(entries)
    }

    /// Enter a fresh function-local scope; balanced by [`pop_scope`].
    pub fn push_scope(&mut self) {
        self.locals.push(Scope::new());
    }

    /// Leave the innermost function-local scope, discarding its bindings.
    pub fn pop_scope(&mut self) {
        self.locals.pop();
    }

    /// The scope writes land in: the innermost function-local if one is active,
    /// else the global scope.
    fn active_mut(&mut self) -> &mut Scope {
        if self.locals.is_empty() {
            &mut self.global
        } else {
            self.locals.last_mut().unwrap()
        }
    }

    pub(crate) fn active_snapshot(&self) -> Scope {
        self.locals
            .last()
            .cloned()
            .unwrap_or_else(|| self.global.clone())
    }

    pub(crate) fn restore_active(&mut self, snapshot: Scope) {
        if let Some(scope) = self.locals.last_mut() {
            *scope = snapshot;
        } else {
            self.global = snapshot;
        }
    }

    /// Does the active scope already hold `name`? (Only the innermost local when
    /// one is active, else the global — never an outer scope.)
    fn active_has(&self, name: &str) -> bool {
        if let Some(scope) = self.locals.last() {
            scope.contains_key(name)
        } else {
            self.global.contains_key(name)
        }
    }

    /// Bind `name` to `value`, creating or replacing it in the active scope.
    #[cfg(test)]
    pub fn set(&mut self, name: &str, value: String) {
        self.active_mut()
            .insert(name.to_string(), Value::String(value));
    }

    /// Bind an already typed value without converting lists to strings.
    pub fn set_value(&mut self, name: &str, value: Value) {
        self.active_mut().insert(name.to_string(), value);
    }

    /// Bind in the **session-global** scope, whatever scope is active — what
    /// `global name = value` says explicitly, since a plain assignment inside a
    /// function is local by default (`DESIGN.md` §"Scope — two levels").
    pub fn set_value_global(&mut self, name: &str, value: Value) {
        self.global.insert(name.to_string(), value);
    }

    /// Remove `name` from the active scope, reporting whether it was bound
    /// there. Inside a function this drops the local only: a global it was
    /// shadowing becomes visible again, because plain `unset` never reaches
    /// through to a global — the same rule that makes assignment local.
    pub fn unset(&mut self, name: &str) -> bool {
        self.active_mut().remove(name).is_some()
    }

    /// Remove `name` from the session-global scope — `global unset name`,
    /// symmetric with `global name = value`.
    pub fn unset_global(&mut self, name: &str) -> bool {
        self.global.remove(name).is_some()
    }

    /// Is `name` bound in any visible scope? A read errors only when it is
    /// bound in none, so `unset` uses this to tell "removed" from "never there".
    pub fn is_bound(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Is the binding a read would find the **function-local** one, rather than a
    /// global it shadows? `false` at top level, where there is no local scope for
    /// a binding to be in, and for a name bound nowhere.
    ///
    /// Asked by `what`, which reports which scope a name lives in; every other
    /// caller wants [`get`](Vars::get), whose whole job is to make the two the
    /// same question.
    pub(crate) fn is_local(&self, name: &str) -> bool {
        self.locals
            .last()
            .is_some_and(|scope| scope.contains_key(name))
    }

    /// Read `name`: the innermost function-local binding, else the global one.
    /// Returns `None` if unbound — the caller turns that into a loud error, per
    /// the no-null / fail-loud rule.
    pub fn get(&self, name: &str) -> Option<&Value> {
        if let Some(scope) = self.locals.last()
            && let Some(value) = scope.get(name)
        {
            return Some(value);
        }
        self.global.get(name)
    }

    /// Iterate over bindings visible in the current scope, with locals taking
    /// precedence over globals of the same name.
    pub(crate) fn visible(&self) -> impl Iterator<Item = (&str, &Value)> {
        let local = self.locals.last();
        local
            .into_iter()
            .flat_map(|scope| scope.iter().map(|(name, value)| (name.as_str(), value)))
            .chain(self.global.iter().filter_map(move |(name, value)| {
                (!local.is_some_and(|scope| scope.contains_key(name)))
                    .then_some((name.as_str(), value))
            }))
    }

    /// Append `value` according to the current string/list value rules.
    ///
    /// Append is an assignment, and assignment is **local-by-default**: inside a
    /// function it must create or modify a local, never reach out and mutate an
    /// outer (global) binding (`DESIGN.md` §"Scope — two levels"). So if the
    /// active scope does not already hold `name`, the visible value (resolved
    /// outward: local → global) is copied into the active scope first, then
    /// appended there — leaving any shadowed global untouched. At top level the
    /// active scope *is* the global, so this stays an in-place append there.
    /// `global name += value`: append in the session-global scope. Unlike
    /// [`Vars::append`] there is no seeding step — the global scope *is* the
    /// target, so an unbound name is simply an error rather than something to
    /// copy inward.
    pub fn append_global(&mut self, name: &str, value: Value) -> Result<(), String> {
        self.update(name, true, |current| append_into(current, value, name))
    }

    pub fn append(&mut self, name: &str, value: Value) -> Result<(), String> {
        self.update(name, false, |current| append_into(current, value, name))
    }

    /// A **mutable place** for `name` in the active scope, for a write that reads
    /// the old value first (`+=`, or a member assignment reaching into it).
    ///
    /// Copies an outer binding inward before handing it over, the local-by-default
    /// rule assignment already follows: `$m.key = v` inside a function shadows the
    /// global rather than reaching through to it, exactly as `$m = …` would. An
    /// unbound name is an error — there is nothing to modify.
    /// Apply `write` to the place `name` names, in the global scope when `global`
    /// or the active one otherwise.
    ///
    /// A **failed write leaves no trace**. Where the name is only bound further
    /// out, the write runs on a copy and the local shadow is installed only once it
    /// succeeds — otherwise a statement that errored would still have changed scope
    /// state, and a later `global $m.… = …` would be written past by a stale local
    /// nobody asked for. Where the binding is already in the target scope there is
    /// nothing to shadow, so it is modified in place.
    ///
    /// `global` never seeds: that scope *is* the target, so an unbound name is an
    /// error rather than something to copy inward.
    pub fn update<T>(
        &mut self,
        name: &str,
        global: bool,
        write: impl FnOnce(&mut Value) -> Result<T, String>,
    ) -> Result<T, String> {
        if global {
            let place = self
                .global
                .get_mut(name)
                .ok_or_else(|| format!("{name}: unbound variable"))?;
            return write(place);
        }
        if self.active_has(name) {
            let place = self.active_mut().get_mut(name).expect("checked just above");
            return write(place);
        }
        let mut copy = self
            .get(name)
            .cloned()
            .ok_or_else(|| format!("{name}: unbound variable"))?;
        let produced = write(&mut copy)?;
        self.active_mut().insert(name.to_string(), copy);
        Ok(produced)
    }
}

/// Combine `value` into an existing place, the `+=` rule. Free-standing so a
/// member assignment (`$m.key += v`) and a whole-variable one go through the same
/// table rather than two that can drift; `name` labels the target in errors.
pub fn append_into(current: &mut Value, value: Value, name: &str) -> Result<(), String> {
    // Appending is a text operation, so attributes do not survive it on either
    // side — `DESIGN.md` §"Hooks and the prompt" says `+=` on a styled value
    // yields a plain string. Flattening here rather than in an arm keeps the
    // table below one case shorter.
    if current.styled_text().is_some() {
        *current = std::mem::replace(current, Value::Boolean(false)).plain();
    }
    let value = value.plain();
    match (current, value) {
        (Value::String(left), Value::String(right)) => left.push_str(&right),
        (Value::Integer(left), Value::Integer(right)) => {
            *left = left
                .checked_add(right)
                .ok_or_else(|| format!("{name}: numeric overflow"))?;
        }
        (Value::List(left), Value::List(mut right)) => left.append(&mut right),
        (Value::List(left), right) => left.push(right),
        (Value::Map(left), Value::Map(right)) => {
            for (key, value) in right {
                if let Some((_, old)) = left.iter_mut().find(|(old, _)| old == &key) {
                    *old = value;
                } else {
                    left.push((key, value));
                }
            }
        }
        (Value::String(_), Value::List(_) | Value::Map(_)) => {
            return Err(format!("{name}: cannot append a list to a string"));
        }
        (Value::String(_), _) => {
            return Err(format!("{name}: can only append a string to a string"));
        }
        (Value::Integer(_), _) => {
            return Err(format!("{name}: can only add an integer to an integer"));
        }
        (Value::Boolean(_), _) => return Err(format!("{name}: cannot append to a boolean")),
        (Value::Map(_), _) => return Err(format!("{name}: can only merge a map into a map")),
        (Value::Regex(_), _) => return Err(format!("{name}: cannot append to a regex")),
        (Value::Glob(_), _) => return Err(format!("{name}: cannot append to a glob")),
        (Value::Stream(_), _) => {
            return Err(format!("{name}: cannot append to a stream handle"));
        }
        (Value::Job(_), _) => {
            return Err(format!("{name}: cannot append to a job handle"));
        }
        (Value::Function(_), _) => {
            return Err(format!("{name}: cannot append to a function value"));
        }
        // Flattened above, so this is the exhaustiveness the compiler wants rather
        // than a reachable case. Answering as the string arm does keeps it honest
        // if that ever changes.
        (Value::Styled(_), _) => {
            return Err(format!("{name}: can only append a string to a string"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Color, Decoration, FuncValue, LINK_LIMIT, NoLiteral, RegexValue, Style, StyledValue, Value,
        Vars, link_url,
    };

    #[test]
    fn a_scalar_writes_the_literal_you_would_have_typed() {
        assert_eq!(Value::Integer(42).to_literal().unwrap(), "42");
        assert_eq!(Value::Integer(-5).to_literal().unwrap(), "-5");
        assert_eq!(Value::Boolean(true).to_literal().unwrap(), "true");
        assert_eq!(Value::Boolean(false).to_literal().unwrap(), "false");
    }

    #[test]
    fn a_string_is_always_quoted_so_it_cannot_read_back_as_another_type() {
        // The case the exactness rule exists for: unquoted, these three would come
        // back as an integer, a boolean, and a bare word.
        assert_eq!(Value::String("42".into()).to_literal().unwrap(), "'42'");
        assert_eq!(Value::String("true".into()).to_literal().unwrap(), "'true'");
        assert_eq!(Value::String("foo".into()).to_literal().unwrap(), "'foo'");
        assert_eq!(Value::String(String::new()).to_literal().unwrap(), "''");
    }

    #[test]
    fn quoting_escapes_exactly_what_the_lexer_decodes() {
        // `'…'` does not interpolate, so a `$` needs no escape — that is the reason
        // for choosing it over `"…"`.
        assert_eq!(Value::String("a$b".into()).to_literal().unwrap(), "'a$b'");
        assert_eq!(
            Value::String("a'b\\c".into()).to_literal().unwrap(),
            r"'a\'b\\c'"
        );
        assert_eq!(
            Value::String("\n\t\r\x1b".into()).to_literal().unwrap(),
            r"'\n\t\r\e'"
        );
        // A control character with no escape of its own falls back to `\u{…}`
        // rather than going onto the wire raw.
        assert_eq!(
            Value::String("\u{7}".into()).to_literal().unwrap(),
            r"'\u{7}'"
        );
    }

    #[test]
    fn a_list_and_a_map_keep_their_two_empty_spellings_apart() {
        // `[]` and `[:]` are the whole reason the writer cannot treat an empty
        // collection generically.
        assert_eq!(Value::List(Vec::new()).to_literal().unwrap(), "[]");
        assert_eq!(Value::Map(Vec::new()).to_literal().unwrap(), "[:]");
    }

    #[test]
    fn a_collection_writes_its_elements_and_nests() {
        let list = Value::List(vec![
            Value::Integer(1),
            Value::String("a b".into()),
            Value::Boolean(true),
        ]);
        assert_eq!(list.to_literal().unwrap(), "[1, 'a b', true]");

        let map = Value::Map(vec![
            ("k".into(), Value::Integer(1)),
            ("a b".into(), Value::List(vec![Value::String("x".into())])),
        ]);
        assert_eq!(map.to_literal().unwrap(), "['k': 1, 'a b': ['x']]");
    }

    #[test]
    fn a_value_with_no_literal_form_is_refused_by_name() {
        assert_eq!(Value::Stream(1).to_literal(), Err(NoLiteral::Stream));
        assert_eq!(
            Value::Glob("*.txt".into()).to_literal(),
            Err(NoLiteral::Glob)
        );
        assert_eq!(
            Value::Regex(RegexValue::new("ab".into())).to_literal(),
            Err(NoLiteral::Regex)
        );
        assert_eq!(
            Value::Function(FuncValue::modifier("stem".into())).to_literal(),
            Err(NoLiteral::Function)
        );
        // Both ends of the range write now that the parser folds the sign into a
        // literal; `i64::MIN` was refused until it did.
        assert_eq!(
            Value::Integer(i64::MIN).to_literal().unwrap(),
            "-9223372036854775808"
        );
        assert_eq!(
            Value::Integer(i64::MAX).to_literal().unwrap(),
            "9223372036854775807"
        );
        assert_eq!(
            NoLiteral::Stream.to_string(),
            "a stream handle has no literal form"
        );
    }

    #[test]
    fn a_nested_value_with_no_literal_form_refuses_the_whole_write() {
        // The refusal has to travel outward: the caller holds the list and cannot
        // see the handle buried in it.
        let list = Value::List(vec![Value::Integer(1), Value::Stream(2)]);
        assert_eq!(list.to_literal(), Err(NoLiteral::Stream));

        let map = Value::Map(vec![("k".into(), Value::Glob("*.rs".into()))]);
        assert_eq!(map.to_literal(), Err(NoLiteral::Glob));
    }

    #[test]
    fn a_local_shadows_the_global_and_is_dropped_on_pop() {
        let mut vars = Vars::new();
        vars.set("x", "global".into());
        vars.push_scope();
        vars.set("x", "local".into());
        assert_eq!(vars.get("x"), Some(&Value::String("local".into())));
        vars.pop_scope();
        assert_eq!(vars.get("x"), Some(&Value::String("global".into())));
    }

    #[test]
    fn a_read_falls_through_to_the_global() {
        let mut vars = Vars::new();
        vars.set("g", "seen".into());
        vars.push_scope();
        // A name not bound locally resolves against the global scope.
        assert_eq!(vars.get("g"), Some(&Value::String("seen".into())));
    }

    #[test]
    fn a_callee_does_not_see_a_callers_local() {
        // Two nested scopes: only the innermost local plus the global are visible,
        // so a name bound in an outer (caller) scope is invisible to the callee.
        let mut vars = Vars::new();
        vars.push_scope();
        vars.set("caller-only", "x".into());
        vars.push_scope();
        assert_eq!(vars.get("caller-only"), None);
    }

    #[test]
    fn interactive_is_reported_by_the_shell_not_inferred() {
        // The `true` case needs the interactive loop, which needs a terminal, so
        // the flag-to-namespace plumbing is checked here and the CLI tests cover
        // every path that must report `false`.
        let mut vars = Vars::new();
        let read = |vars: &Vars| match vars.shell_namespace() {
            Value::Map(entries) => entries
                .iter()
                .find(|(key, _)| key == "interactive")
                .map(|(_, value)| value.clone())
                .expect("$sh.interactive"),
            other => panic!("$sh should be a map, got {other:?}"),
        };
        assert_eq!(read(&vars), Value::Boolean(false));
        vars.set_interactive(true);
        assert_eq!(read(&vars), Value::Boolean(true));
    }

    #[test]
    fn append_mutates_the_binding_in_place() {
        let mut vars = Vars::new();
        vars.set("s", "a".into());
        vars.append("s", Value::String("b".into())).unwrap();
        assert_eq!(vars.get("s"), Some(&Value::String("ab".into())));
    }

    #[test]
    fn append_in_a_function_shadows_rather_than_clobbers_a_global() {
        // `+=` on a global-only name inside a function must create a local from
        // the visible value, not mutate the global (local-by-default assignment).
        let mut vars = Vars::new();
        vars.set("g", "before".into());
        vars.push_scope();
        vars.append("g", Value::String("after".into())).unwrap();
        assert_eq!(vars.get("g"), Some(&Value::String("beforeafter".into())));
        vars.pop_scope();
        assert_eq!(vars.get("g"), Some(&Value::String("before".into())));
    }
    /// A styled value with `text` and no attributes but `style`.
    fn styled(text: &str, style: &Style) -> Value {
        Value::Styled(Box::new(StyledValue {
            text: text.to_owned(),
            style: style.clone(),
        }))
    }

    #[test]
    fn a_styled_value_equals_and_hashes_as_its_text() {
        use std::collections::HashSet;

        let red = Style {
            foreground: Some(Color::Red),
            ..Style::default()
        };
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        // Styling must not change what a value *is*, or `==` and `:dedup` would
        // answer differently depending on how a segment was decorated.
        assert_eq!(styled("x", &red), Value::String("x".into()));
        assert_eq!(styled("x", &red), styled("x", &bold));
        assert_ne!(styled("x", &red), Value::String("y".into()));
        // `1 == "1"` stays false: only the two *text* kinds are unified.
        assert_ne!(styled("1", &red), Value::Integer(1));

        let mut set = HashSet::new();
        set.insert(Value::String("x".into()));
        assert!(
            !set.insert(styled("x", &red)),
            "hashing must agree with `eq`"
        );
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn a_styled_value_writes_down_as_its_text() {
        let red = Style {
            foreground: Some(Color::Red),
            ..Style::default()
        };
        // `:repr` round-trips by equality, and equality is by text — so the text is
        // the honest thing to write, quoted like any other string.
        assert_eq!(styled("x", &red).to_literal().as_deref(), Ok("'x'"));
    }

    #[test]
    fn appending_to_a_styled_value_yields_a_plain_string() {
        // Attributes are rendering-only, so `+=` drops them on either side.
        let red = Style {
            foreground: Some(Color::Red),
            ..Style::default()
        };
        let mut vars = Vars::new();
        vars.set_value("s", styled("a", &red));
        vars.append("s", styled("b", &red)).unwrap();
        assert_eq!(vars.get("s"), Some(&Value::String("ab".into())));
        assert!(vars.get("s").unwrap().styled_text().is_none());
    }

    /// Everything this stream would take, for a render test that is about the
    /// bytes rather than about the capability rules.
    const EVERYTHING: Decoration = Decoration {
        color: true,
        links: true,
    };

    #[test]
    fn sgr_carries_bold_then_foreground_then_background() {
        // One sequence with the parameters in a fixed order, not three sequences:
        // the order is what a byte-for-byte test can pin.
        let style = Style {
            foreground: Some(Color::Blue),
            background: Some(Color::White),
            bold: true,
            link: None,
        };
        assert_eq!(style.render("x", EVERYTHING), "\u{1b}[1;34;47mx\u{1b}[0m");
        // A bright color is the 90/100 range, not 30/40 with a modifier.
        let bright = Style {
            foreground: Some(Color::BrightBlack),
            ..Style::default()
        };
        assert_eq!(bright.render("x", EVERYTHING), "\u{1b}[90mx\u{1b}[0m");
        // Nothing named means nothing emitted — never a bare `ESC [ m`, which
        // some terminals read as a reset.
        assert!(Style::default().is_plain());
        assert_eq!(Style::default().render("x", EVERYTHING), "x");
    }

    #[test]
    fn a_link_wraps_outside_the_color_and_each_drops_on_its_own() {
        let style = Style {
            foreground: Some(Color::Blue),
            link: Some("https://x.test/".to_owned()),
            ..Style::default()
        };
        // The link is outermost, so the whole colored run is clickable, and it
        // closes with the empty URI that means "the link ends here".
        assert_eq!(
            style.render("x", EVERYTHING),
            "\u{1b}]8;;https://x.test/\u{1b}\\\u{1b}[34mx\u{1b}[0m\u{1b}]8;;\u{1b}\\"
        );
        // `NO_COLOR` leaves the link: it is not color, and dropping it would lose
        // the URL rather than make the output plainer.
        assert_eq!(
            style.render(
                "x",
                Decoration {
                    color: false,
                    links: true
                }
            ),
            "\u{1b}]8;;https://x.test/\u{1b}\\x\u{1b}]8;;\u{1b}\\"
        );
        // A terminal that would print an `OSC` gets the color and no link.
        assert_eq!(
            style.render(
                "x",
                Decoration {
                    color: true,
                    links: false
                }
            ),
            "\u{1b}[34mx\u{1b}[0m"
        );
        assert_eq!(style.render("x", Decoration::plain()), "x");
    }

    #[test]
    fn a_link_url_is_encoded_where_it_would_break_the_sequence() {
        // A URL's delimiters are left alone, so the structure the caller wrote
        // survives — `?`, `&`, `=`, `#`, and an existing `%20` that must not become
        // `%2520`.
        assert_eq!(
            link_url("https://x.test/a?b=c&d=e#f%20g").as_deref(),
            Ok("https://x.test/a?b=c&d=e#f%20g")
        );
        // A **space** is the ordinary case, not an exotic one: a path with one is
        // an invalid URI, and a terminal may reject the whole link over it.
        assert_eq!(
            link_url("file://host/My File.txt").as_deref(),
            Ok("file://host/My%20File.txt")
        );
        // The rest of RFC 3986's forbidden printables, which are just as raw-illegal
        // as a space despite looking harmless.
        assert_eq!(
            link_url("https://x.test/a\"b<c>d\\e^f`g{h|i}j").as_deref(),
            Ok("https://x.test/a%22b%3Cc%3Ed%5Ce%5Ef%60g%7Bh%7Ci%7Dj")
        );
        // Sub-delims stay: they are legal raw and some APIs depend on them.
        assert_eq!(
            link_url("https://x.test/a!$&'()*+,;=b").as_deref(),
            Ok("https://x.test/a!$&'()*+,;=b")
        );
        // An `ESC` or `BEL` would end mesh's own sequence and leave the rest on
        // screen. This is the title payload's injection guard in URL form.
        assert_eq!(
            link_url("https://x/\u{1b}]0;pwned\u{7}").as_deref(),
            Ok("https://x/%1B]0;pwned%07")
        );
        // Non-ASCII is encoded per the `OSC 8` specification.
        assert_eq!(link_url("https://x/é").as_deref(), Ok("https://x/%C3%A9"));
        assert_eq!(
            link_url("file://host/path").as_deref(),
            Ok("file://host/path")
        );
        assert_eq!(
            link_url("mailto:a@b.test").as_deref(),
            Ok("mailto:a@b.test")
        );
    }

    #[test]
    fn a_link_url_needs_a_scheme_and_a_bounded_length() {
        // A terminal needs an absolute URI, so a bare path is a link that silently
        // does nothing — said rather than guessed at `file://`, which would need a
        // hostname to be right over `ssh`.
        assert!(link_url("/etc/passwd").is_err());
        assert!(link_url("x.test/a").is_err());
        assert!(link_url("12://x").is_err(), "a scheme starts with a letter");
        assert!(link_url(":no-scheme").is_err());
        assert!(link_url("").is_err());
        // Over the limit a terminal drops the whole sequence, taking the link text
        // with it, so the size is named rather than silently truncated.
        let long = format!("https://x.test/{}", "a".repeat(LINK_LIMIT));
        let message = link_url(&long).expect_err("over the limit");
        assert!(message.contains(&LINK_LIMIT.to_string()), "{message}");
        // Encoding counts against the limit, not the written length: a URL of
        // non-ASCII triples on the way out.
        let wide = format!("https://x.test/{}", "é".repeat(LINK_LIMIT / 2));
        assert!(link_url(&wide).is_err());
    }

    #[test]
    fn color_names_cover_both_grey_spellings_and_reject_the_rest() {
        assert_eq!(Color::named("grey"), Some(Color::BrightBlack));
        assert_eq!(Color::named("gray"), Some(Color::BrightBlack));
        assert_eq!(Color::named("bright-black"), Some(Color::BrightBlack));
        assert_eq!(Color::named("bright-cyan"), Some(Color::BrightCyan));
        assert_eq!(Color::named("chartreuse"), None);
        assert_eq!(Color::named("Red"), None, "names are lowercase");
        assert_eq!(Color::named(""), None);
        // Every advertised name resolves, so the diagnostic cannot list a name that
        // then fails.
        for name in Color::NAMES {
            assert!(Color::named(name).is_some(), "{name}");
        }
    }
}
