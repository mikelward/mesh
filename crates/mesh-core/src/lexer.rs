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
//! Deferred: `:` value modifiers, `${…}` beyond a name/`.member`, heredocs, and
//! `\`-newline continuation across input lines.

/// A variable reference: `$name`, `${name}`, or `$env.member`.
#[derive(Debug, PartialEq, Eq)]
pub struct VarRef {
    pub name: String,
    /// A single `.member` access, e.g. `$env.PATH` → name `env`, member `PATH`.
    pub member: Option<String>,
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

/// A word: adjacent pieces that concatenate into one argument. An empty piece
/// list is a genuine empty argument (e.g. from `""`).
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
}

/// Split `line` into command segments joined by `;` / `&&` / `||`, each a
/// pipeline of `|`-joined stages with optional `>` / `>>` / `<` redirections. A
/// line with no separator is a single segment. Operators are recognized only at
/// the bare (unquoted, unescaped) level; a lone `&` (background) is not one yet
/// and stays a literal character.
pub fn split_line(line: &str) -> Result<Vec<Segment>, LexError> {
    let chars: Vec<char> = line.chars().collect();
    let mut segments = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut redirs: Vec<Redir> = Vec::new();
    let mut pending_redir: Option<RedirKind> = None;
    let mut sep_before = Sep::Seq;
    let mut current: Option<Vec<Piece>> = None;
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
            finish_segment(
                &mut segments,
                sep_before,
                &mut stages,
                &mut words,
                &mut redirs,
            )?;
            sep_before = sep;
            i += len;
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
            if is_descriptor_prefix(&current, &chars, i) || chars.get(i + len) == Some(&'&') {
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
            '$' => match parse_var(&chars, i + 1, true)? {
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
    finish_segment(
        &mut segments,
        sep_before,
        &mut stages,
        &mut words,
        &mut redirs,
    )?;
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
/// stage is completed from `words`/`redirs`; an empty final stage is a no-op for
/// a single-stage segment (a blank line, `;;`) but an error inside a pipeline
/// (a trailing `|`).
fn finish_segment(
    segments: &mut Vec<Segment>,
    sep_before: Sep,
    stages: &mut Vec<Stage>,
    words: &mut Vec<Word>,
    redirs: &mut Vec<Redir>,
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
fn is_descriptor_prefix(current: &Option<Vec<Piece>>, chars: &[char], at: usize) -> bool {
    if at == 0 || chars[at - 1].is_whitespace() {
        return false;
    }
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
    // `N>` / `N>>` / `N<`: the word abutting the operator is a bare run of ASCII
    // digits (an fd number). Scan the raw chars back to the previous space, so an
    // empty quote (`""2>`), an escape (`\2>`), or a non-fd word (`file2>`) — each
    // of which puts a non-digit char in the run — is excluded.
    let mut j = at;
    while j > 0 && !chars[j - 1].is_whitespace() {
        if !chars[j - 1].is_ascii_digit() {
            return false;
        }
        j -= 1;
    }
    j < at // at least one digit abutted the operator
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
            // Inside a string an unbraced `$name.member` is `$name` + literal
            // `.member`; only `${…}` parses member access here.
            if let Some((vref, next)) = parse_var(chars, i + 1, false)? {
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
    push_text(word, &buf, false);
    Ok(i)
}

/// Parse a `$…` interpolation starting at `at` (the index just past `$`).
/// Returns `Ok(Some((ref, next)))` for a valid interpolation, or `Ok(None)` when
/// `$` is not followed by a variable at all (so the `$` is a literal character,
/// e.g. `$5` or a trailing `$`). A **braced** `${…}` signals interpolation
/// intent, so a missing `}` or a malformed name inside it is a loud `Err` rather
/// than a silent literal — a literal `$` in a string is spelled `\$`.
///
/// `member_after_name` controls whether a `.member` after an unbraced `$name` is
/// consumed as member access. It is **true** outside strings (`$m.key` is access)
/// and **false** inside `"…"`, where an unbraced `$name.member` is `$name` plus
/// the literal `.member` (per `DESIGN.md` — use `${…}` for access in a string).
/// The braced `${…}` form always parses member access.
fn parse_var(
    chars: &[char],
    at: usize,
    member_after_name: bool,
) -> Result<Option<(VarRef, usize)>, LexError> {
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
    if member_after_name && chars.get(j) == Some(&'.') {
        if let Some((m, k)) = read_name(chars, j + 1) {
            member = Some(m);
            j = k;
        }
    }
    Ok(Some((VarRef { name, member }, j)))
}

/// Is `s` a valid kebab identifier? Uses the same rule as [`read_name`] (so an
/// assignment target and a `$name` read agree — e.g. `a--b` is not a name).
pub fn is_ident(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    matches!(read_name(&chars, 0), Some((_, n)) if n == chars.len())
}

/// Does `text` leave an unclosed `{` at the bare (unquoted) level? The read loop
/// uses this to keep buffering the lines of a multi-line `func … { … }` body.
/// Quoted regions (`'…'`, `"…"`, `r'…'`, `r"…"`) and `\`-escapes are skipped, so
/// a brace inside a string does not count.
pub fn needs_more_input(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2;
                continue;
            }
            'r' if matches!(chars.get(i + 1), Some('\'') | Some('"')) => {
                i = skip_quoted(&chars, i + 2, chars[i + 1], false);
                continue;
            }
            q @ ('\'' | '"') => {
                i = skip_quoted(&chars, i + 1, q, true);
                continue;
            }
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth > 0
}

/// Skip from `start` to just past the closing `quote`. When `escapes`, a `\`
/// escapes the next character (as in `"…"` / `'…'`); a raw string skips none.
/// Returns the text length if the quote is unterminated in `text` so far.
fn skip_quoted(chars: &[char], start: usize, quote: char, escapes: bool) -> usize {
    let mut i = start;
    while i < chars.len() {
        if escapes && chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    i
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
    if j != chars.len() {
        return None; // trailing junk
    }
    Some(VarRef { name, member })
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
        LexError, Piece, Redir, RedirKind, Segment, Sep, Stage, VarRef, Word, split, split_line,
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
    fn a_lone_amp_is_not_a_separator() {
        // `&` (background) is not an operator yet — it stays a literal character.
        assert_eq!(split_line("a&b").unwrap(), [seg(Sep::Seq, &["a&b"])]);
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
    fn an_empty_pipeline_stage_is_an_error() {
        assert_eq!(split_line("| cat"), Err(LexError::EmptyPipelineStage));
        assert_eq!(split_line("ls |"), Err(LexError::EmptyPipelineStage));
        assert_eq!(split_line("ls | | wc"), Err(LexError::EmptyPipelineStage));
    }

    #[test]
    fn an_empty_segment_is_kept() {
        // A trailing `;` leaves an empty final segment (no stages); the runner
        // treats it as a no-op. Structurally it is still present.
        let segs = split_line("a ;").unwrap();
        assert_eq!(segs.len(), 2);
        assert!(segs[1].stages.is_empty());
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
                member: Some("PATH".into())
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
        assert_eq!(words(r#""a$x""#), [Word(vec![lit("a"), var("x")])]);
        assert_eq!(words(r"'a$x'"), [Word(vec![lit("a$x")])]);
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
        assert_eq!(words(r#"""r'x'"#), [Word(vec![exp("r"), lit("x")])]);
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert_eq!(split("'oops"), Err(LexError::UnterminatedQuote('\'')));
        assert_eq!(split(r"r'oops"), Err(LexError::UnterminatedQuote('\'')));
    }
}
