//! Lexer: split a line into words of pieces, honoring quotes, escapes, and
//! `$` interpolation.
//!
//! Quoting is **Model B** (`DESIGN.md` §"Quoting and escaping"):
//!
//! - Bare words: a backslash escapes the next character; `$name` interpolates.
//! - `"…"` — interpolates (`$name`, `${…}`) and escapes (`\n \t \r \e \\ \" \$`,
//!   `\u{…}`).
//! - `'…'` — escapes but does *not* interpolate (`\n \t \r \e \\ \'`, `\u{…}`);
//!   `$` is always literal.
//! - `r'…'` / `r"…"` — raw: no escapes and no interpolation.
//! - An unknown escape inside a quote is an error.
//!
//! Each word is a list of [`Piece`]s: `Text` (literal or expandable) and `Var`
//! (an interpolation resolved later, in [`crate::expand`], against the variable
//! store). Interpolation and expansion never word-split.
//!
//! Deferred: modifier arguments, `${…}` beyond a name/`.member`, heredocs, and
//! `\`-newline continuation across input lines.

/// A variable reference: `$name`, `${name}`, or `$env.member`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRef {
    pub name: String,
    /// A single `.member` access, e.g. `$env.PATH` → name `env`, member `PATH`.
    pub member: Option<String>,
    pub access: Option<Access>,
    /// Recognized postfix value modifiers, in application order.
    pub modifiers: Vec<Modifier>,
    /// Whether this interpolation appeared inside double quotes.
    pub quoted: bool,
}

/// The initial, argument-free modifier set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Dir,
    Base,
    Ext,
    Exts,
    Stem,
    Bare,
    Len,
    First,
    Last,
    Rest,
    Init,
    Dedup,
    Upper,
    Lower,
}

impl Modifier {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "dir" => Self::Dir,
            "base" => Self::Base,
            "ext" => Self::Ext,
            "exts" => Self::Exts,
            "stem" => Self::Stem,
            "bare" => Self::Bare,
            "len" => Self::Len,
            "first" => Self::First,
            "last" => Self::Last,
            "rest" => Self::Rest,
            "init" => Self::Init,
            "dedup" => Self::Dedup,
            "upper" => Self::Upper,
            "lower" => Self::Lower,
            _ => return None,
        })
    }
}

/// An exact list index or a clamped range slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    Index(i64),
    Slice {
        start: Option<i64>,
        end: Option<i64>,
        inclusive: bool,
    },
}

/// One piece of a word.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    /// Literal or bare text. `expandable` is true for unquoted text (eligible
    /// for tilde/glob expansion), false for quoted or escaped text.
    Text { text: String, expandable: bool },
    /// A `$…` interpolation, resolved against the variable store.
    Var(VarRef),
}

/// A word: adjacent pieces that concatenate into one argument. Quoted empty
/// strings are retained as empty, non-expandable text pieces.
#[derive(Debug, PartialEq, Eq)]
pub struct Word(pub Vec<Piece>);

/// A lexing (syntax) error.
#[derive(Debug, PartialEq, Eq)]
pub enum LexError {
    UnterminatedQuote(char),
    UnknownEscape(char),
    BadUnicodeEscape,
    UnterminatedInterpolation,
    BadInterpolation(String),
    MissingRedirectTarget,
    EmptyPipelineStage,
    UnsupportedRedirect,
    EmptyBackgroundCommand,
    EmptyCommand,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::UnterminatedQuote(q) => write!(f, "syntax error: unterminated {q} quote"),
            LexError::UnknownEscape(c) => write!(f, "syntax error: invalid escape \\{c}"),
            LexError::BadUnicodeEscape => write!(f, "syntax error: invalid \\u{{…}} escape"),
            LexError::UnterminatedInterpolation => {
                write!(f, "syntax error: unterminated ${{…}} interpolation")
            }
            LexError::BadInterpolation(inner) => {
                write!(f, "syntax error: invalid interpolation ${{{inner}}}")
            }
            LexError::MissingRedirectTarget => {
                write!(f, "syntax error: redirection needs a target file")
            }
            LexError::EmptyPipelineStage => {
                write!(f, "syntax error: empty command in a pipeline")
            }
            LexError::UnsupportedRedirect => write!(
                f,
                "syntax error: descriptor redirection (e.g. `2>`, `&>`) is not supported yet"
            ),
            LexError::EmptyBackgroundCommand => {
                write!(f, "syntax error: background operator needs a command")
            }
            LexError::EmptyCommand => write!(f, "syntax error: empty command between separators"),
        }
    }
}

/// A statement separator between commands: `;` (sequence), `&&` (run the next on
/// success), `||` (run the next on failure). Recognized only bare — a quoted or
/// escaped operator is a literal character.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Sep {
    Seq,
    And,
    Or,
}

/// A redirection operator: `>` truncate stdout, `>>` append stdout, `<` stdin.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RedirKind {
    Out,
    Append,
    In,
}

/// A redirection: an operator and the file word it targets.
#[derive(Debug, PartialEq, Eq)]
pub struct Redir {
    pub kind: RedirKind,
    pub target: Word,
}

/// One stage of a pipeline: the command words plus any redirections applied to
/// it. Stages are joined by `|`.
#[derive(Debug, PartialEq, Eq)]
pub struct Stage {
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
}

/// One command in a sequence: the separator connecting it to the previous
/// command (`Seq` for the first), and its pipeline — one or more `|`-joined
/// stages. The connector decides whether the pipeline runs, based on the
/// previous command's status.
#[derive(Debug, PartialEq, Eq)]
pub struct Segment {
    pub sep_before: Sep,
    pub stages: Vec<Stage>,
    pub background: bool,
}

/// Split `line` into command segments joined by `;` / `&&` / `||`, each a
/// pipeline of `|`-joined stages with optional `>` / `>>` / `<` redirections. A
/// line with no separator is a single segment. A bare `&` ends a pipeline and
/// launches it in the background. Operators are recognized only at the bare
/// (unquoted, unescaped) level.
pub fn split_line(line: &str) -> Result<Vec<Segment>, LexError> {
    let chars: Vec<char> = line.chars().collect();
    let mut segments = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut redirs: Vec<Redir> = Vec::new();
    let mut pending_redir: Option<RedirKind> = None;
    let mut sep_before = Sep::Seq;
    let mut current: Option<Vec<Piece>> = None;
    let mut trailing_separator = None;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            finish_word(&mut current, &mut words, &mut redirs, &mut pending_redir);
            i += 1;
            continue;
        }
        // `\`-newline is line continuation: drop the pair without starting a
        // word. Cross-line continuation is still deferred.
        if c == '\\' && chars.get(i + 1) == Some(&'\n') {
            i += 2;
            continue;
        }
        // Bare operators, checked before the escape/quote handling below so a
        // quoted or `\`-escaped operator is left literal. Order matters: the
        // sequence separators (which include `||`) come before the single-`|`
        // pipe so `||` is not read as two pipes.
        if let Some((sep, len)) = separator_at(&chars, i) {
            finish_word(&mut current, &mut words, &mut redirs, &mut pending_redir);
            if pending_redir.is_some() {
                return Err(LexError::MissingRedirectTarget);
            }
            if stages.is_empty() && words.is_empty() && redirs.is_empty() {
                return Err(LexError::EmptyCommand);
            }
            finish_segment(
                &mut segments,
                sep_before,
                &mut stages,
                &mut words,
                &mut redirs,
                false,
            )?;
            sep_before = sep;
            trailing_separator = Some(sep);
            i += len;
            continue;
        }
        if chars[i] == '&' && chars.get(i + 1) != Some(&'>') {
            finish_word(&mut current, &mut words, &mut redirs, &mut pending_redir);
            if pending_redir.is_some() {
                return Err(LexError::MissingRedirectTarget);
            }
            if stages.is_empty() && words.is_empty() && redirs.is_empty() {
                return Err(LexError::EmptyBackgroundCommand);
            }
            finish_segment(
                &mut segments,
                sep_before,
                &mut stages,
                &mut words,
                &mut redirs,
                true,
            )?;
            sep_before = Sep::Seq;
            trailing_separator = None;
            i += 1;
            continue;
        }
        if chars[i] == '|' {
            finish_word(&mut current, &mut words, &mut redirs, &mut pending_redir);
            if pending_redir.is_some() {
                return Err(LexError::MissingRedirectTarget);
            }
            if words.is_empty() && redirs.is_empty() {
                return Err(LexError::EmptyPipelineStage);
            }
            stages.push(Stage {
                words: std::mem::take(&mut words),
                redirs: std::mem::take(&mut redirs),
            });
            i += 1;
            continue;
        }
        if let Some((kind, len)) = redirect_at(&chars, i) {
            // Deferred descriptor forms are rejected rather than silently
            // reinterpreted: an fd number or `&` attached *before* the operator
            // (`2>`, `&>`), or a `&` attached *after* it (`>&2`, `<&0`, the
            // fd-duplication form) which would otherwise become the target file.
            if is_descriptor_prefix(&current) || chars.get(i + len) == Some(&'&') {
                return Err(LexError::UnsupportedRedirect);
            }
            finish_word(&mut current, &mut words, &mut redirs, &mut pending_redir);
            if pending_redir.is_some() {
                return Err(LexError::MissingRedirectTarget);
            }
            pending_redir = Some(kind);
            i += len;
            continue;
        }
        let at_word_start = current.is_none();
        trailing_separator = None;
        let word = current.get_or_insert_with(Vec::new);
        // A raw string is recognized where a string piece can begin: at a word
        // start, and right after an unescaped `=`. A bare `'…'`/`"…"` already
        // starts a piece there (`--flag='a b'`), so `k=r'v'`, `k='v'`, `k="v"`
        // all yield `k=v`; this also covers the value of a `name=r'…'` binding.
        let raw_eligible = at_word_start || ends_with_bare_equals(word);
        match c {
            '\\' => match chars.get(i + 1) {
                Some(&next) => {
                    push_char(word, next, false);
                    i += 2;
                }
                None => {
                    push_char(word, '\\', false);
                    i += 1;
                }
            },
            '$' => match parse_var(&chars, i + 1)? {
                Some((vref, next)) => {
                    word.push(Piece::Var(vref));
                    i = next;
                }
                // A `$` not starting a valid interpolation is a literal char.
                None => {
                    push_char(word, '$', true);
                    i += 1;
                }
            },
            '\'' => {
                i = lex_escaped(&chars, i + 1, '\'', word)?;
            }
            '"' => {
                i = lex_escaped(&chars, i + 1, '"', word)?;
            }
            'r' if raw_eligible && matches!(chars.get(i + 1), Some('\'') | Some('"')) => {
                let delim = chars[i + 1];
                i = lex_raw(&chars, i + 2, delim, word)?;
            }
            _ => {
                push_char(word, c, true);
                i += 1;
            }
        }
    }

    finish_word(&mut current, &mut words, &mut redirs, &mut pending_redir);
    if pending_redir.is_some() {
        return Err(LexError::MissingRedirectTarget);
    }
    let command_empty = stages.is_empty() && words.is_empty() && redirs.is_empty();
    if command_empty {
        match trailing_separator {
            Some(Sep::Seq) => {} // A single trailing `;` is permitted.
            Some(Sep::And | Sep::Or) => return Err(LexError::EmptyCommand),
            None => {} // Blank line, or a pipeline terminated by `&`.
        }
    } else {
        finish_segment(
            &mut segments,
            sep_before,
            &mut stages,
            &mut words,
            &mut redirs,
            false,
        )?;
    }
    Ok(segments)
}

/// Complete the word being built (if any): it becomes the target of a pending
/// redirection, or otherwise a command word of the current stage.
fn finish_word(
    current: &mut Option<Vec<Piece>>,
    words: &mut Vec<Word>,
    redirs: &mut Vec<Redir>,
    pending_redir: &mut Option<RedirKind>,
) {
    if let Some(pieces) = current.take() {
        let word = Word(pieces);
        match pending_redir.take() {
            Some(kind) => redirs.push(Redir { kind, target: word }),
            None => words.push(word),
        }
    }
}

/// Close off the pipeline accumulated so far and push it as a segment. The final
/// stage is completed from `words`/`redirs`; an empty final stage is an error
/// inside a pipeline (a trailing `|`). Callers filter blank lines and a permitted
/// trailing semicolon before reaching this helper.
fn finish_segment(
    segments: &mut Vec<Segment>,
    sep_before: Sep,
    stages: &mut Vec<Stage>,
    words: &mut Vec<Word>,
    redirs: &mut Vec<Redir>,
    background: bool,
) -> Result<(), LexError> {
    let last_empty = words.is_empty() && redirs.is_empty();
    if !stages.is_empty() {
        if last_empty {
            return Err(LexError::EmptyPipelineStage);
        }
        stages.push(Stage {
            words: std::mem::take(words),
            redirs: std::mem::take(redirs),
        });
    } else if !last_empty {
        stages.push(Stage {
            words: std::mem::take(words),
            redirs: std::mem::take(redirs),
        });
    }
    segments.push(Segment {
        sep_before,
        stages: std::mem::take(stages),
        background,
    });
    Ok(())
}

/// If a bare separator token starts at `at`, return it and its length in chars:
/// `;` (1), `&&` (2), `||` (2). A lone `&` is not a separator yet.
fn separator_at(chars: &[char], at: usize) -> Option<(Sep, usize)> {
    match chars[at] {
        ';' => Some((Sep::Seq, 1)),
        '&' if chars.get(at + 1) == Some(&'&') => Some((Sep::And, 2)),
        '|' if chars.get(at + 1) == Some(&'|') => Some((Sep::Or, 2)),
        _ => None,
    }
}

/// Is the redirection operator at `at` a deferred file-descriptor form — an
/// unspaced fd number (`2>`) or `&` (`&>`) directly before it? True only when
/// the pending word abuts the operator (no space) and is either a bare run of
/// digits (`2>`) or ends in a bare `&` (`&>`, `hello&>`). So a plain argument
/// `2 > f`, a non-fd word `file2>f`, an escaped `\&>`/`\2>` (a literal), and an
/// empty-quote form (`""2>`) are all excluded.
fn is_descriptor_prefix(current: &Option<Vec<Piece>>) -> bool {
    // `&>` / `&>>`: a bare (unescaped) `&` immediately before the operator,
    // whatever precedes it — `echo hello&>f` is still the deferred both-streams
    // form. An escaped `\&` is a literal piece, so it is excluded.
    if let Some(Piece::Text {
        text,
        expandable: true,
    }) = current.as_deref().and_then(<[Piece]>::last)
    {
        if text.ends_with('&') {
            return true;
        }
    }
    // `N>` / `N>>` / `N<`: the pending word must consist of one bare text piece
    // containing only digits. Inspecting the word rather than scanning back to
    // whitespace also makes a preceding operator a boundary, while still
    // excluding empty quotes, escapes, and non-fd words.
    matches!(
        current.as_deref(),
        Some([Piece::Text {
            text,
            expandable: true,
        }]) if !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit())
    )
}

/// If a bare redirection operator starts at `at`, return it and its length:
/// `>>` (2), `>` (1), `<` (1).
fn redirect_at(chars: &[char], at: usize) -> Option<(RedirKind, usize)> {
    match chars[at] {
        '>' if chars.get(at + 1) == Some(&'>') => Some((RedirKind::Append, 2)),
        '>' => Some((RedirKind::Out, 1)),
        '<' => Some((RedirKind::In, 1)),
        _ => None,
    }
}

/// Lex a single command (no separators/pipes/redirects) into its words.
/// Test-only convenience over [`split_line`].
#[cfg(test)]
fn split(line: &str) -> Result<Vec<Word>, LexError> {
    Ok(split_line(line)?
        .into_iter()
        .flat_map(|segment| segment.stages)
        .flat_map(|stage| stage.words)
        .collect())
}

/// Lex a `"…"` or `'…'` string (Model B). `"…"` also interpolates `$…`. `start`
/// is the index just past the opening `quote`; returns the index past the close.
fn lex_escaped(
    chars: &[char],
    start: usize,
    quote: char,
    word: &mut Vec<Piece>,
) -> Result<usize, LexError> {
    let piece_start = word.len();
    let double = quote == '"';
    let mut buf = String::new();
    let mut i = start;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(LexError::UnterminatedQuote(quote));
        };
        if c == quote {
            i += 1;
            break;
        }
        if double && c == '$' {
            if let Some((mut vref, next)) = parse_var(chars, i + 1)? {
                vref.quoted = true;
                push_text(word, &buf, false);
                buf.clear();
                word.push(Piece::Var(vref));
                i = next;
                continue;
            }
            buf.push('$');
            i += 1;
            continue;
        }
        if c == '\\' {
            let Some(&next) = chars.get(i + 1) else {
                return Err(LexError::UnterminatedQuote(quote));
            };
            match next {
                'n' => buf.push('\n'),
                't' => buf.push('\t'),
                'r' => buf.push('\r'),
                'e' => buf.push('\x1b'),
                '\\' => buf.push('\\'),
                'u' => {
                    let (ch, consumed) =
                        parse_unicode_escape(&chars[i + 2..]).ok_or(LexError::BadUnicodeEscape)?;
                    buf.push(ch);
                    i += 2 + consumed;
                    continue;
                }
                q if q == quote => buf.push(q),
                '$' if double => buf.push('$'),
                other => return Err(LexError::UnknownEscape(other)),
            }
            i += 2;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    push_text(word, &buf, false);
    if word.len() == piece_start {
        word.push(Piece::Text {
            text: String::new(),
            expandable: false,
        });
    }
    Ok(i)
}

/// Lex a raw string `r'…'` / `r"…"`: no escapes, no interpolation.
fn lex_raw(
    chars: &[char],
    start: usize,
    delim: char,
    word: &mut Vec<Piece>,
) -> Result<usize, LexError> {
    let mut buf = String::new();
    let mut i = start;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(LexError::UnterminatedQuote(delim));
        };
        if c == delim {
            i += 1;
            break;
        }
        buf.push(c);
        i += 1;
    }
    if buf.is_empty() {
        word.push(Piece::Text {
            text: String::new(),
            expandable: false,
        });
    } else {
        push_text(word, &buf, false);
    }
    Ok(i)
}

/// Parse a `$…` interpolation starting at `at` (the index just past `$`).
/// Returns `Ok(Some((ref, next)))` for a valid interpolation, or `Ok(None)` when
/// `$` is not followed by a variable at all (so the `$` is a literal character,
/// e.g. `$5` or a trailing `$`). A **braced** `${…}` signals interpolation
/// intent, so a missing `}` or a malformed name inside it is a loud `Err` rather
/// than a silent literal — a literal `$` in a string is spelled `\$`.
///
/// Both braced and unbraced forms parse member access and integer indexing.
fn parse_var(chars: &[char], at: usize) -> Result<Option<(VarRef, usize)>, LexError> {
    if chars.get(at) == Some(&'{') {
        let start = at + 1;
        let mut j = start;
        while let Some(&c) = chars.get(j) {
            if c == '}' {
                let inner: String = chars[start..j].iter().collect();
                return match parse_var_ref(&inner) {
                    Some(v) => Ok(Some((v, j + 1))),
                    None => Err(LexError::BadInterpolation(inner)),
                };
            }
            j += 1;
        }
        return Err(LexError::UnterminatedInterpolation);
    }
    let Some((name, mut j)) = read_name(chars, at) else {
        return Ok(None); // `$` not followed by a name → literal `$`
    };
    let mut member = None;
    if chars.get(j) == Some(&'.') {
        if let Some((m, k)) = read_name(chars, j + 1) {
            member = Some(m);
            j = k;
        }
    }
    let (access, mut j) = parse_access(chars, j).unwrap_or((None, j));
    let modifiers = parse_modifiers(chars, &mut j);
    Ok(Some((
        VarRef {
            name,
            member,
            access,
            modifiers,
            quoted: false,
        },
        j,
    )))
}

/// Is `s` a valid kebab identifier? Uses the same rule as [`read_name`] (so an
/// assignment target and a `$name` read agree — e.g. `a--b` is not a name).
pub fn is_ident(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    matches!(read_name(&chars, 0), Some((_, n)) if n == chars.len())
}

/// Parse the inner content of a `${…}` — a `name` with an optional `.member`.
fn parse_var_ref(inner: &str) -> Option<VarRef> {
    let chars: Vec<char> = inner.chars().collect();
    let (name, mut j) = read_name(&chars, 0)?;
    let mut member = None;
    if chars.get(j) == Some(&'.') {
        let (m, k) = read_name(&chars, j + 1)?;
        member = Some(m);
        j = k;
    }
    let (access, next) = parse_access(&chars, j)?;
    j = next;
    let modifiers = parse_modifiers(&chars, &mut j);
    if j != chars.len() {
        return None; // trailing junk
    }
    Some(VarRef {
        name,
        member,
        access,
        modifiers,
        quoted: false,
    })
}

fn parse_modifiers(chars: &[char], at: &mut usize) -> Vec<Modifier> {
    let mut modifiers = Vec::new();
    loop {
        if chars.get(*at) != Some(&':') {
            break;
        }
        let Some((name, next)) = read_name(chars, *at + 1) else {
            break;
        };
        let Some(modifier) = Modifier::from_name(&name) else {
            break;
        };
        modifiers.push(modifier);
        *at = next;
    }
    modifiers
}

fn parse_access(chars: &[char], start: usize) -> Option<(Option<Access>, usize)> {
    if chars.get(start) != Some(&'[') {
        return Some((None, start));
    }
    let end = chars[start + 1..].iter().position(|c| *c == ']')? + start + 1;
    let text: String = chars[start + 1..end].iter().collect();
    let access = if let Some((start, end)) = text.split_once("..=") {
        Access::Slice {
            start: parse_bound(start)?,
            end: Some(end.parse().ok()?),
            inclusive: true,
        }
    } else if let Some((start, end)) = text.split_once("..") {
        Access::Slice {
            start: parse_bound(start)?,
            end: parse_bound(end)?,
            inclusive: false,
        }
    } else {
        Access::Index(text.parse().ok()?)
    };
    Some((Some(access), end + 1))
}

fn parse_bound(text: &str) -> Option<Option<i64>> {
    if text.is_empty() {
        Some(None)
    } else {
        Some(Some(text.parse().ok()?))
    }
}

/// Read a kebab identifier at `start`: an alphabetic head, then alphanumeric/`_`,
/// plus interior `-` (a hyphen only when the next char is alphanumeric, so
/// `$a-$b` is `$a` + `-` + `$b` while `$auto-fetch` is one name). The first
/// character must be a letter — a leading `_` is not a name (`_` is reserved as
/// the discard pattern), so `_` / `_x` are not bindable and `$_` is a literal.
fn read_name(chars: &[char], start: usize) -> Option<(String, usize)> {
    let first = *chars.get(start)?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut name = String::from(first);
    let mut i = start + 1;
    while let Some(&c) = chars.get(i) {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
        } else if c == '-' && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphanumeric()) {
            name.push('-');
        } else {
            break;
        }
        i += 1;
    }
    Some((name, i))
}

/// Parse the `{HEX}` body of a `\u` escape (from just past the `u`). Returns the
/// decoded char and how many chars were consumed (including the braces).
fn parse_unicode_escape(rest: &[char]) -> Option<(char, usize)> {
    if rest.first() != Some(&'{') {
        return None;
    }
    let mut j = 1;
    let mut hex = String::new();
    while let Some(&c) = rest.get(j) {
        if c == '}' {
            let code = u32::from_str_radix(&hex, 16).ok()?;
            let ch = char::from_u32(code)?;
            return Some((ch, j + 1));
        }
        hex.push(c);
        j += 1;
    }
    None
}

/// Append `text` to `word`, coalescing with a trailing `Text` piece of the same
/// expandability. Empty text is dropped.
fn push_text(word: &mut Vec<Piece>, text: &str, expandable: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(Piece::Text {
        text: last,
        expandable: e,
    }) = word.last_mut()
    {
        if *e == expandable {
            last.push_str(text);
            return;
        }
    }
    word.push(Piece::Text {
        text: text.to_string(),
        expandable,
    });
}

/// Does `word` end in an unquoted `=`? Used to let a raw-string prefix begin a
/// piece right after `=` (`--flag=r'v'`, and the value of a `name=r'…'` binding),
/// matching where a bare `'…'`/`"…"` already starts one. The `=` is a bare,
/// expandable char, so it lands in a trailing expandable `Text` piece.
fn ends_with_bare_equals(word: &[Piece]) -> bool {
    matches!(
        word.last(),
        Some(Piece::Text { text, expandable: true }) if text.ends_with('=')
    )
}

/// Does the buffered `func` definition in `text` need more input lines?
///
/// Completion is based on the **first** brace that returns the depth to zero, not
/// the final net depth, so trailing text that reopens a brace (`func f() {} {`)
/// does not keep the definition pending; it is dispatched so the parser reports
/// the documented "unexpected text after the closing `}`" error. While a body `{`
/// is open with no matching `}` yet, more input is needed. This stays
/// **brace-driven** once a body has opened, so a malformed header such as
/// `func f(x {` still buffers through to the matching `}` and is quarantined
/// rather than releasing later body lines to the top level.
///
/// Before any body `{` appears, the header may still legitimately be incomplete:
/// the grammar's `")" ws? "{"` lets the opening brace sit on a later line
/// (`ws` includes a newline). So a header that is a valid *incomplete* prefix —
/// still forming its signature, or closed with only whitespace after `)` — keeps
/// buffering ([`header_awaits_body`]); an already-malformed header is dispatched
/// immediately so its parse error is reported without swallowing later commands.
pub fn needs_more_input(text: &str) -> bool {
    match body_open_offset(text) {
        // A body has opened: buffer until its first matching `}`. Only the body
        // (from its `{`) is scanned with quote/brace rules; trailing text after the
        // close (or a reopened brace) still dispatches so the parser reports it.
        Some(open) => scan_braces(&text[open..], 0).close.is_none(),
        // No body has opened yet — the header is still forming, or is malformed.
        None => header_awaits_body(text),
    }
}

/// Byte offset of the body's opening `{`, located via the **signature grammar**
/// rather than a literal brace search, so a `{` that belongs elsewhere — inside a
/// following command (`func f()`⏎`puts '{'`) or hidden by a malformed quoted
/// parameter — is not mistaken for the body opener. The body opens only where a
/// `{` sits right after the signature `func name(params)` (whitespace between `)`
/// and `{`), or — for a malformed header whose `(` never closed (`func f(x {`) —
/// at the first `{` in the parameter region, so that definition still buffers
/// through to its matching `}` and stays quarantined. Returns `None` when no body
/// has opened, i.e. the header is still forming or is malformed without a brace.
fn body_open_offset(text: &str) -> Option<usize> {
    let brace = text.find('{')?;
    match text.find('(') {
        // `(` before the `{`: this is the signature. Find its closing `)`.
        Some(open) if open < brace => match text[open + 1..brace].find(')') {
            // Signature closed before the `{`: the body opens at `{` only if just
            // whitespace sits between `)` and `{` (a real body opener); otherwise
            // the `{` is separate content and no body opens from this header.
            Some(rel) => {
                let close = open + 1 + rel;
                text[close + 1..brace].trim().is_empty().then_some(brace)
            }
            // The signature `(` never closed before the `{`: the `{` is in the
            // parameter region of a malformed header — treat it as the body opener
            // so the definition buffers to its matching `}` and stays quarantined.
            None => Some(brace),
        },
        // `{` before any `(` (or no `(` at all): a body opener only if the header
        // text before it is a valid name prefix (`func f {`); otherwise the `{`
        // belongs to following content (`func`⏎`puts '{'`), not this header.
        _ => {
            let head = text[..brace]
                .trim_start()
                .strip_prefix("func")
                .unwrap_or("")
                .trim();
            (head.is_empty() || is_ident_prefix(head)).then_some(brace)
        }
    }
}

/// With no body `{` seen yet, is `text` a valid *incomplete* `func` header still
/// awaiting its `{`? True only while the header could still become a well-formed
/// `func name(params)` — the name so far is a valid identifier (or empty), and
/// once the signature's `)` is present it is preceded by a proper `name(` and
/// followed by only whitespace. Anything already impossible (a bad name, a
/// missing `(`, non-whitespace after `)`) returns false, so a malformed header is
/// dispatched immediately — its parse error reported — rather than buffering and
/// swallowing the commands that follow it.
fn header_awaits_body(text: &str) -> bool {
    let rest = text.trim().strip_prefix("func").unwrap_or("").trim_start();
    let Some(paren) = rest.find('(') else {
        // No `(` yet: still forming the name. Keep reading while what we have is a
        // valid identifier *prefix* (or just `func`); an impossible head (`_`, a
        // digit) or an embedded space can never become a name, so dispatch it.
        let name = rest.trim_end();
        return name.is_empty() || is_ident_prefix(name);
    };
    // The name is everything before the `(` and must be a valid identifier.
    if !is_ident(rest[..paren].trim()) {
        return false;
    }
    let after_open = &rest[paren + 1..];
    match after_open.find(')') {
        // Params still forming: keep reading only while the partial list could
        // still be completed into a valid one (`func f(,`, `func f(...`, `func
        // f(a=` can never be repaired, so they dispatch immediately).
        None => params_prefix_ok(after_open),
        // Signature closed: the parameter list must actually parse, and only
        // whitespace may sit between `)` and the `{`. Reusing `parse_params` means
        // any signature the parser will reject (`(,)`, `(...xs)`, `(a,a)`) is
        // dispatched immediately rather than buffering the commands after it.
        Some(close) => {
            parse_params(&after_open[..close]).is_ok() && after_open[close + 1..].trim().is_empty()
        }
    }
}

/// Is `s` a valid *prefix* of a kebab identifier — an ASCII-letter head followed
/// by identifier-body characters (alphanumeric, `_`, or `-`)? Used to decide
/// whether a still-forming name or parameter token could yet become a valid name;
/// an impossible head (`_`, a digit) or a stray character is rejected at once.
fn is_ident_prefix(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Could the partial (still-unclosed) parameter list `list` still be completed
/// into a valid one by appending more input? Applies the same structural rules as
/// [`parse_params`], but treats the final token as an in-progress prefix (a bare
/// name may still be growing), so only *provably* unrepairable content fails: a
/// leading or doubled comma, a rest/flag/default token (`...`, `-…`, `…=…`), a
/// finalized name that is invalid/`env`/duplicate, or a trailing token whose head
/// already cannot start an identifier. A trailing comma is repairable.
fn params_prefix_ok(list: &str) -> bool {
    let chars: Vec<char> = list.chars().collect();
    let mut names: Vec<String> = Vec::new();
    let mut have_name = false;
    let mut pending_comma = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ',' {
            if !have_name || pending_comma {
                return false; // a comma with no name before it can never be valid
            }
            pending_comma = true;
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] != ',' && !chars[i].is_whitespace() {
            i += 1;
        }
        let tok: String = chars[start..i].iter().collect();
        // Forms that can never be a valid positional, even as a prefix.
        if tok.starts_with("...") || tok.starts_with('-') || tok.contains('=') {
            return false;
        }
        // A delimiter after the token finalizes it; the trailing token may still be
        // growing. A finalized token must be a valid, non-`env`, non-duplicate
        // name; the trailing one need only be a valid identifier *prefix* — an
        // impossible head (`_`, a digit) can never become a name, so reject it now
        // rather than entering continuation mode.
        let finalized = i < chars.len();
        if finalized {
            if !is_ident(&tok) || tok == "env" || names.iter().any(|n| n == &tok) {
                return false;
            }
            names.push(tok);
        } else if !is_ident_prefix(&tok) {
            return false;
        }
        have_name = true;
        pending_comma = false;
    }
    true
}

/// Parse a parameter list: names separated by commas and/or whitespace. A comma
/// is a real separator, not ignorable filler — it must sit between two names, so
/// a leading, trailing, or doubled comma (`,x`, `x,`, `x,,y`) is a loud error
/// rather than a silently dropped empty. v1 also rejects the deferred flag /
/// optional / rest forms with a clear message.
pub(crate) fn parse_params(list: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = list.chars().collect();
    let mut params: Vec<String> = Vec::new();
    // A comma needs a name on each side: it is only valid once at least one name
    // has been read, and never immediately after another comma.
    let mut have_name = false;
    let mut pending_comma = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ',' {
            if !have_name || pending_comma {
                return Err("func: missing parameter name before `,`".to_string());
            }
            pending_comma = true;
            i += 1;
            continue;
        }
        // Read a name token: a run up to the next comma or whitespace.
        let start = i;
        while i < chars.len() && chars[i] != ',' && !chars[i].is_whitespace() {
            i += 1;
        }
        let tok: String = chars[start..i].iter().collect();
        if tok.starts_with("...") {
            return Err("func: rest parameters (`...name`) are not supported yet".to_string());
        }
        if tok.starts_with('-') {
            return Err("func: flag parameters (`--name`) are not supported yet".to_string());
        }
        if tok.contains('=') {
            return Err("func: optional/default parameters are not supported yet".to_string());
        }
        if !is_ident(&tok) {
            return Err(format!("func: `{tok}` is not a valid parameter name"));
        }
        // `env` is the environment namespace (`$env.KEY`), so a parameter named
        // `env` would bind but never read back — reject it, as assignment does.
        if tok == "env" {
            return Err("func: `env` is a reserved name and cannot be a parameter".to_string());
        }
        // A repeated name would silently overwrite the earlier positional in the
        // local scope, making one argument unreachable; diagnose it here.
        if params.iter().any(|p| p == &tok) {
            return Err(format!("func: duplicate parameter `{tok}`"));
        }
        params.push(tok);
        have_name = true;
        pending_comma = false;
    }
    if pending_comma {
        return Err("func: missing parameter name after `,`".to_string());
    }
    Ok(params)
}

/// The result of a bare-level brace scan (see [`scan_braces`]).
pub struct BraceScan {
    /// Byte offset of the `}` that first returned the depth to 0, if one was
    /// reached — used to split a `func` body from whatever follows its `}`.
    pub close: Option<usize>,
    /// Net `{` minus `}` at the bare (unquoted) level over the whole input.
    pub depth: i32,
}

/// Count bare-level `{` / `}` in `text`, honoring the lexer's own quote, raw,
/// escape, and interpolation rules so the multi-line `func` reader
/// ([`needs_more_input`]) and the body extractor (`repl::split_braced_body`)
/// cannot disagree about where a body ends.
///
/// Unlike [`split_line`], it never fails: a line that lexes cleanly at the quote
/// level but is unsupported higher up (a bare `2>`, a target-less `>`) does not
/// change brace nesting, so a still-open `func` body keeps buffering instead of
/// being released into the top-level loop. An unterminated quote ends the scan at
/// the line boundary (the rest of the input is inside that string); the depth
/// returned still reflects the open brace, so buffering continues and the syntax
/// error surfaces when the completed definition is finally parsed.
///
/// Counting starts from `start_depth` (0 for a whole-line check, 1 for a body
/// whose opening `{` has already been consumed).
pub fn scan_braces(text: &str, start_depth: i32) -> BraceScan {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut depth = start_depth;
    let mut close = None;
    // Raw-string eligibility, tracked exactly as `split_line`: a raw prefix
    // `r'`/`r"` is recognized only at a word start or right after a bare `=`.
    let mut word_start = true;
    let mut after_equals = false;
    let mut k = 0;
    while k < chars.len() {
        let (byte, c) = chars[k];
        let raw_eligible = word_start || after_equals;
        // Default: an ordinary bare-word char. Boundary arms below re-enable
        // raw eligibility exactly where the lexer would start a fresh word.
        word_start = false;
        after_equals = false;
        match c {
            _ if c.is_whitespace() => {
                word_start = true;
                k += 1;
            }
            // A backslash escapes the next char, so `\{` / `\}` are literal. A
            // `\`-newline is a line boundary, though: a function body runs line by
            // line, so the next word starts fresh there (a following raw prefix is
            // raw), exactly as an unescaped newline would reset it.
            '\\' => {
                if chars.get(k + 1).map(|&(_, c)| c) == Some('\n') {
                    word_start = true;
                }
                k += 2;
            }
            '\'' | '"' => match skip_quote(&chars, k + 1, c, true) {
                Some(next) => k = next,
                None => return BraceScan { close, depth },
            },
            'r' if raw_eligible
                && matches!(chars.get(k + 1).map(|&(_, c)| c), Some('\'') | Some('"')) =>
            {
                match skip_quote(&chars, k + 2, chars[k + 1].1, false) {
                    Some(next) => k = next,
                    None => return BraceScan { close, depth },
                }
            }
            // A bare `${…}` interpolation: its braces belong to the interpolation,
            // not to block structure, so skip to its close (as `parse_var` does)
            // without counting them. An unterminated `${` is a line-local error, so
            // it ends at the line boundary and leaves no dangling `{`.
            '$' if chars.get(k + 1).map(|&(_, c)| c) == Some('{') => {
                k = skip_interpolation(&chars, k + 2);
            }
            // A bare `{`/`}` is a block delimiter and a word boundary: the body's
            // first word begins right after the opening `{` (so a `func f(){r'…'}`
            // raw prefix is raw), and a fresh word follows the closing `}`.
            '{' => {
                depth += 1;
                word_start = true;
                k += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 && close.is_none() {
                    close = Some(byte);
                }
                word_start = true;
                k += 1;
            }
            // Operators the lexer starts a fresh word after (so a following
            // `r'…'` is raw): `;`, `|`/`||`, `<`, `>`/`>>`.
            ';' | '|' | '<' | '>' => {
                word_start = true;
                k += 1;
            }
            // Both `&&` (a separator) and a lone `&` (the background operator)
            // start a fresh word after them, exactly as `split_line` does — so a
            // raw prefix that immediately follows (`true&r'…'`) is recognized as
            // raw here too, and the scan cannot mis-read it as a plain quote and
            // swallow the block's closing `}`.
            '&' => {
                word_start = true;
                if chars.get(k + 1).map(|&(_, c)| c) == Some('&') {
                    k += 2;
                } else {
                    k += 1;
                }
            }
            // A bare `=` lets a raw prefix begin the value (`k=r'v'`).
            '=' => {
                after_equals = true;
                k += 1;
            }
            _ => k += 1,
        }
    }
    BraceScan { close, depth }
}

/// Skip a bare `${…}` interpolation from just past the `{`. Its braces do not
/// count as block structure. Stops at the closing `}` or a line break.
fn skip_interpolation(chars: &[(usize, char)], start: usize) -> usize {
    let mut k = start;
    while k < chars.len() {
        match chars[k].1 {
            '}' => return k + 1,
            '\n' => return k,
            _ => k += 1,
        }
    }
    k
}

/// Skip a quoted string from just past the opening `quote`. With `escapes`, a
/// backslash escapes the next char (matching `"…"`/`'…'`); without it (raw
/// `r'…'`/`r"…"`) no escape applies. An unterminated quote ends at the next line
/// break, so a brace after it is not miscounted. Returns the index past the
/// close (or the line boundary), or `None` at end of input with no close.
fn skip_quote(chars: &[(usize, char)], start: usize, quote: char, escapes: bool) -> Option<usize> {
    let mut k = start;
    while k < chars.len() {
        let c = chars[k].1;
        if c == '\n' {
            return Some(k); // unterminated quote ends at the line boundary
        }
        if escapes && c == '\\' {
            // A backslash escapes the next char, but never across a line break.
            if chars.get(k + 1).map(|&(_, c)| c) == Some('\n') {
                return Some(k + 1);
            }
            k += 2;
            continue;
        }
        if c == quote {
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

fn push_char(word: &mut Vec<Piece>, c: char, expandable: bool) {
    if let Some(Piece::Text {
        text: last,
        expandable: e,
    }) = word.last_mut()
    {
        if *e == expandable {
            last.push(c);
            return;
        }
    }
    word.push(Piece::Text {
        text: c.to_string(),
        expandable,
    });
}

#[cfg(test)]
mod tests {
    use super::{
        Access, LexError, Piece, Redir, RedirKind, Segment, Sep, Stage, VarRef, Word, split,
        split_line,
    };

    fn exp(text: &str) -> Piece {
        Piece::Text {
            text: text.to_string(),
            expandable: true,
        }
    }
    fn lit(text: &str) -> Piece {
        Piece::Text {
            text: text.to_string(),
            expandable: false,
        }
    }
    fn var(name: &str) -> Piece {
        Piece::Var(VarRef {
            name: name.to_string(),
            member: None,
            access: None,
            modifiers: Vec::new(),
            quoted: false,
        })
    }
    fn quoted_var(name: &str) -> Piece {
        Piece::Var(VarRef {
            name: name.to_string(),
            member: None,
            access: None,
            modifiers: Vec::new(),
            quoted: true,
        })
    }

    fn words(line: &str) -> Vec<Word> {
        split(line).expect("lex")
    }

    fn stage(ws: &[&str]) -> Stage {
        Stage {
            words: ws.iter().map(|w| Word(vec![exp(w)])).collect(),
            redirs: Vec::new(),
        }
    }

    fn seg(sep_before: Sep, ws: &[&str]) -> Segment {
        Segment {
            sep_before,
            stages: vec![stage(ws)],
            background: false,
        }
    }

    #[test]
    fn a_plain_line_is_one_segment() {
        assert_eq!(split_line("ls -l").unwrap(), [seg(Sep::Seq, &["ls", "-l"])]);
    }

    #[test]
    fn separators_split_into_segments() {
        assert_eq!(
            split_line("a; b && c || d").unwrap(),
            [
                seg(Sep::Seq, &["a"]),
                seg(Sep::Seq, &["b"]),
                seg(Sep::And, &["c"]),
                seg(Sep::Or, &["d"]),
            ]
        );
    }

    #[test]
    fn background_operator_ends_a_segment() {
        let mut first = seg(Sep::Seq, &["sleep", "1"]);
        first.background = true;
        assert_eq!(
            split_line("sleep 1 & echo ready").unwrap(),
            [first, seg(Sep::Seq, &["echo", "ready"]),]
        );
        assert_eq!(split_line("&"), Err(LexError::EmptyBackgroundCommand));
    }

    #[test]
    fn separators_need_no_surrounding_space() {
        assert_eq!(
            split_line("a&&b||c").unwrap(),
            [
                seg(Sep::Seq, &["a"]),
                seg(Sep::And, &["b"]),
                seg(Sep::Or, &["c"]),
            ]
        );
    }

    #[test]
    fn a_quoted_or_escaped_separator_is_literal() {
        // Neither the quoted `;` nor the escaped `&&` splits the line — one
        // segment of one word each (the pieces are literal, not expandable).
        let quoted = split_line("'a;b'").unwrap();
        assert_eq!(quoted.len(), 1);
        assert_eq!(quoted[0].stages[0].words, [Word(vec![lit("a;b")])]);
        let escaped = split_line(r"a\&\&b").unwrap();
        assert_eq!(escaped.len(), 1);
        assert_eq!(
            escaped[0].stages[0].words,
            [Word(vec![exp("a"), lit("&&"), exp("b")])]
        );
    }

    #[test]
    fn a_single_pipe_splits_a_segment_into_stages() {
        let segs = split_line("a | b|c").unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(
            segs[0].stages,
            [stage(&["a"]), stage(&["b"]), stage(&["c"])]
        );
    }

    #[test]
    fn redirections_attach_to_their_stage() {
        // `sort < in > out` — one stage, two redirections, targets peeled off the
        // command words.
        let segs = split_line("sort < in >> out").unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].stages.len(), 1);
        let s = &segs[0].stages[0];
        assert_eq!(s.words, [Word(vec![exp("sort")])]);
        assert_eq!(
            s.redirs,
            [
                Redir {
                    kind: RedirKind::In,
                    target: Word(vec![exp("in")]),
                },
                Redir {
                    kind: RedirKind::Append,
                    target: Word(vec![exp("out")]),
                },
            ]
        );
    }

    #[test]
    fn a_redirect_without_a_target_is_an_error() {
        assert_eq!(split_line("cat >"), Err(LexError::MissingRedirectTarget));
        assert_eq!(
            split_line("cat > | wc"),
            Err(LexError::MissingRedirectTarget)
        );
    }

    #[test]
    fn descriptor_prefix_stops_at_an_operator() {
        for line in ["true;2>f", "true&&2>f", "false||2>f", "echo x|2>f"] {
            assert_eq!(
                split_line(line),
                Err(LexError::UnsupportedRedirect),
                "{line:?}"
            );
        }
    }

    #[test]
    fn an_empty_pipeline_stage_is_an_error() {
        assert_eq!(split_line("| cat"), Err(LexError::EmptyPipelineStage));
        assert_eq!(split_line("ls |"), Err(LexError::EmptyPipelineStage));
        assert_eq!(split_line("ls | | wc"), Err(LexError::EmptyPipelineStage));
    }

    #[test]
    fn a_single_trailing_semicolon_is_permitted() {
        let segs = split_line("a ;").unwrap();
        assert_eq!(segs, [seg(Sep::Seq, &["a"])]);
    }

    #[test]
    fn empty_command_positions_are_errors() {
        for line in ["; a", "&& a", "|| a", "a ;; b", "a ; && b", "a &&", "a ||"] {
            assert_eq!(split_line(line), Err(LexError::EmptyCommand), "{line:?}");
        }
        assert!(split_line("").unwrap().is_empty());
    }

    #[test]
    fn splits_bare_words_on_whitespace() {
        assert_eq!(
            words("ls -l"),
            [Word(vec![exp("ls")]), Word(vec![exp("-l")])]
        );
    }

    #[test]
    fn backslash_escapes_a_space_into_one_word() {
        assert_eq!(words(r"a\ b"), [Word(vec![exp("a"), lit(" "), exp("b")])]);
    }

    #[test]
    fn bare_dollar_name_is_a_var() {
        assert_eq!(words("$x"), [Word(vec![var("x")])]);
        assert_eq!(words("pre$x"), [Word(vec![exp("pre"), var("x")])]);
    }

    #[test]
    fn braced_and_member_vars() {
        assert_eq!(words("${x}post"), [Word(vec![var("x"), exp("post")])]);
        assert_eq!(
            words("$env.PATH"),
            [Word(vec![Piece::Var(VarRef {
                name: "env".into(),
                member: Some("PATH".into()),
                access: None,
                modifiers: Vec::new(),
                quoted: false,
            })])]
        );
    }

    #[test]
    fn hyphen_is_interior_only_in_var_names() {
        // `$a-$b` is `$a`, a literal `-`, then `$b`.
        assert_eq!(words("$a-$b"), [Word(vec![var("a"), exp("-"), var("b")])]);
        // but a hyphen between name chars stays in the name.
        assert_eq!(words("$auto-fetch"), [Word(vec![var("auto-fetch")])]);
    }

    #[test]
    fn double_quotes_interpolate_single_quotes_do_not() {
        assert_eq!(words(r#""a$x""#), [Word(vec![lit("a"), quoted_var("x")])]);
        assert_eq!(words(r"'a$x'"), [Word(vec![lit("a$x")])]);
    }

    #[test]
    fn braced_and_unbraced_quoted_access_are_equivalent() {
        let member = || {
            Piece::Var(VarRef {
                name: "map".into(),
                member: Some("field".into()),
                access: None,
                modifiers: Vec::new(),
                quoted: true,
            })
        };
        assert_eq!(words(r#""$map.field""#), [Word(vec![member()])]);
        assert_eq!(words(r#""${map.field}""#), [Word(vec![member()])]);

        let index = || {
            Piece::Var(VarRef {
                name: "items".into(),
                member: None,
                access: Some(Access::Index(-1)),
                modifiers: Vec::new(),
                quoted: true,
            })
        };
        assert_eq!(words(r#""$items[-1]""#), [Word(vec![index()])]);
        assert_eq!(words(r#""${items[-1]}""#), [Word(vec![index()])]);
    }

    #[test]
    fn parses_half_open_and_inclusive_slices() {
        let slice = |start, end, inclusive| {
            Piece::Var(VarRef {
                name: "items".into(),
                member: None,
                access: Some(Access::Slice {
                    start,
                    end,
                    inclusive,
                }),
                modifiers: Vec::new(),
                quoted: false,
            })
        };
        assert_eq!(
            words("$items[1..3]"),
            [Word(vec![slice(Some(1), Some(3), false)])]
        );
        assert_eq!(
            words("${items[..=2]}"),
            [Word(vec![slice(None, Some(2), true)])]
        );
        assert_eq!(
            words("$items[-2..]"),
            [Word(vec![slice(Some(-2), None, false)])]
        );
    }

    #[test]
    fn single_quotes_escape_but_do_not_interpolate() {
        assert_eq!(words(r"'can\'t'"), [Word(vec![lit("can't")])]);
        assert_eq!(words(r"'a\nb'"), [Word(vec![lit("a\nb")])]);
        assert_eq!(words(r"'$x'"), [Word(vec![lit("$x")])]);
    }

    #[test]
    fn single_quote_unknown_escape_is_an_error() {
        assert_eq!(split(r"'\d'"), Err(LexError::UnknownEscape('d')));
    }

    #[test]
    fn double_quote_escapes_and_errors() {
        assert_eq!(words(r#""a\nb""#), [Word(vec![lit("a\nb")])]);
        assert_eq!(words(r#""\$5""#), [Word(vec![lit("$5")])]);
        assert_eq!(split(r#""\z""#), Err(LexError::UnknownEscape('z')));
        assert_eq!(split(r#""\uZ""#), Err(LexError::BadUnicodeEscape));
    }

    #[test]
    fn raw_strings_take_no_escapes_or_interpolation() {
        assert_eq!(words(r"r'\d+\.txt'"), [Word(vec![lit(r"\d+\.txt")])]);
        assert_eq!(words(r"r'$x'"), [Word(vec![lit("$x")])]);
    }

    #[test]
    fn raw_prefix_only_at_word_start() {
        assert_eq!(words(r"ptr'x'"), [Word(vec![exp("ptr"), lit("x")])]);
    }

    #[test]
    fn empty_quote_does_not_reset_word_start() {
        assert_eq!(
            words(r#"""r'x'"#),
            [Word(vec![lit(""), exp("r"), lit("x")])]
        );
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert_eq!(split("'oops"), Err(LexError::UnterminatedQuote('\'')));
        assert_eq!(split(r"r'oops"), Err(LexError::UnterminatedQuote('\'')));
    }

    use super::{needs_more_input, scan_braces};

    #[test]
    fn scan_braces_reports_balance_and_first_close() {
        assert_eq!(scan_braces("a { b } c", 0).depth, 0);
        let scan = scan_braces("func f() { puts hi }", 0);
        assert_eq!(scan.depth, 0);
        assert_eq!(scan.close, Some("func f() { puts hi ".len()));
        assert_eq!(scan_braces("func f() {", 0).depth, 1);
        assert_eq!(scan_braces("{ { }", 0).depth, 1);
    }

    #[test]
    fn scan_braces_ignores_quoted_raw_escaped_and_interpolated_braces() {
        assert_eq!(scan_braces(r#"puts "{ ${x} }""#, 0).depth, 0);
        assert_eq!(scan_braces(r"puts '{'", 0).depth, 0);
        assert_eq!(scan_braces(r"puts r'{'", 0).depth, 0);
        assert_eq!(scan_braces(r"puts \{", 0).depth, 0);
        // A real block brace alongside an interpolation still counts.
        assert_eq!(scan_braces("func f() { puts ${x}", 0).depth, 1);
    }

    #[test]
    fn scan_braces_treats_a_bare_ampersand_as_a_word_boundary() {
        // The no-space background form (`true&r'…'`) starts a fresh word after
        // `&`, so the following raw string is raw here (as in `split_line`) and
        // its trailing backslash does not escape the quote and swallow the `}`.
        assert_eq!(scan_braces(r"func f() { true&r'\' }", 0).depth, 0);
        assert_eq!(scan_braces(r"func f() { true&r'\' }", 0).close, Some(21));
    }

    #[test]
    fn needs_more_input_is_brace_driven() {
        assert!(needs_more_input("func f() {"));
        assert!(needs_more_input("func f() {\n  puts hi\n"));
        assert!(!needs_more_input("func f() { puts hi }"));
        // A malformed header that opens a body still buffers to the matching `}`,
        // so its later body lines cannot leak to the top level (the P1 case).
        assert!(needs_more_input("func f(x {\nputs )\nputs LEAKED\n"));
        assert!(!needs_more_input("func f(x {\nputs )\nputs LEAKED\n}\n"));
    }

    #[test]
    fn needs_more_input_buffers_a_delayed_body_opener() {
        // The grammar's `")" ws? "{"` lets the `{` sit on a later line, so an
        // otherwise-complete header keeps buffering until the body opens/closes.
        assert!(needs_more_input("func f()\n"));
        assert!(needs_more_input("func f()\n{\n  puts hi\n"));
        assert!(!needs_more_input("func f()\n{\n  puts hi\n}\n"));
        // A still-forming signature also keeps reading.
        assert!(needs_more_input("func f(a,\n"));
        assert!(needs_more_input("func\n"));
        // A malformed header is NOT buffered — it dispatches to a parse error so
        // following commands are not swallowed: non-whitespace after `)`, a
        // signature `)` with no opening `(`/name before it, an invalid name, or a
        // name not followed by `(`.
        assert!(!needs_more_input("func f() oops\n"));
        assert!(!needs_more_input("func f() ; puts hi\n"));
        assert!(!needs_more_input("func f)\n"));
        assert!(!needs_more_input("func 1f(\n"));
        assert!(!needs_more_input("func f oops\n"));
        // A closed but invalid parameter list is also dispatched immediately —
        // the same validation the parser applies, so no invalid shape buffers.
        assert!(!needs_more_input("func f(,)\n"));
        assert!(!needs_more_input("func f(...xs)\n"));
        assert!(!needs_more_input("func f(a,a)\n"));
        // An unclosed but provably-invalid parameter list is dispatched too, while
        // a valid partial list (a name still forming) keeps buffering.
        assert!(!needs_more_input("func f(,\n"));
        assert!(!needs_more_input("func f(...\n"));
        assert!(!needs_more_input("func f(a=\n"));
        assert!(!needs_more_input("func f(a,a,\n"));
        assert!(needs_more_input("func f(a\n"));
        assert!(needs_more_input("func f(a, b\n"));
        assert!(needs_more_input("func f(a,\n"));
        // A trailing parameter token whose head cannot start an identifier is
        // impossible, so it dispatches instead of entering continuation mode.
        assert!(!needs_more_input("func f(_\n"));
        assert!(!needs_more_input("func f(1\n"));
        assert!(!needs_more_input("func f(a, _\n"));
        // A still-forming name (before `(`) with a valid letter head keeps reading,
        // including a partial kebab name; an impossible head dispatches.
        assert!(needs_more_input("func my-\n"));
        assert!(!needs_more_input("func _f\n"));
        assert!(!needs_more_input("func 1f\n"));
    }

    #[test]
    fn needs_more_input_uses_the_signature_to_find_the_body_opener() {
        // A `{` inside a following command (or hidden by a malformed quoted param)
        // is not the body opener, so a completed header awaiting its body is not
        // kept pending by such a brace.
        assert!(!needs_more_input("func f()\nputs '{'\n"));
        assert!(!needs_more_input("func f()\nputs '{'\nputs after\n"));
        // A real body opener right after the signature still buffers.
        assert!(needs_more_input("func f() {\n"));
        assert!(needs_more_input("func f()\n{\n"));
        // A malformed header with a brace in the parameter region still quarantines.
        assert!(needs_more_input("func f(x {\nputs LEAK\n"));
        assert!(needs_more_input("func f(') {\nputs LEAKED\n"));
    }

    #[test]
    fn needs_more_input_stops_at_the_first_body_close() {
        // Trailing text that reopens a brace does not keep the definition pending:
        // once the body's matching `}` is found, it is dispatched so the parser
        // reports the trailing-text error rather than swallowing later commands.
        assert!(!needs_more_input("func f() {} {\n"));
        assert!(!needs_more_input("func f() { puts hi } extra {\n"));
    }

    #[test]
    fn scan_braces_treats_a_body_brace_as_a_word_boundary() {
        // The body's first word begins right after `{`, so a raw prefix there is
        // raw and its trailing backslash does not escape the quote and swallow `}`.
        assert_eq!(scan_braces(r"func f(){r'\'}", 0).depth, 0);
        assert_eq!(
            scan_braces(r"func f(){r'\'}", 0).close,
            Some(r"func f(){r'\'".len())
        );
    }

    #[test]
    fn scan_braces_resets_word_start_after_an_escaped_newline() {
        // A `\`-newline is a line boundary: the raw prefix on the next physical
        // line is raw, so its trailing backslash does not escape the quote and
        // swallow the block's `}` (mirrors how the body is run line by line).
        assert_eq!(scan_braces("func f() { true \\\nr'\\' }", 0).depth, 0);
    }
}
