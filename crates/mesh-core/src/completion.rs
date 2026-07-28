//! Completion specifications generated from command help output.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher};

const CACHE_VERSION: &str = "mesh-completion-v6";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ValueHint {
    File,
    Directory,
    /// A name from the manual, not from the filesystem: `man PAGE` takes
    /// `git-add`, which is no file in the directory you are standing in.
    ManPage,
    Enum(Vec<String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompletionSpec {
    candidates: Vec<String>,
    values: HashMap<String, ValueHint>,
    positional: Option<ValueHint>,
}

#[derive(Debug)]
enum PendingValues {
    Described {
        options: Vec<String>,
        values: Vec<String>,
    },
    Wrapped {
        options: Vec<String>,
        values: Vec<String>,
    },
}

impl CompletionSpec {
    pub(crate) fn from_help(help: &str) -> Self {
        let mut candidates = Vec::new();
        let mut values = HashMap::new();
        let mut positional = None;
        let mut commands = false;
        let mut entry_column = None;
        let mut current_options = Vec::new();
        let mut pending_values = None;
        for line in help.lines() {
            let trimmed = line.trim();
            let tokens: Vec<_> = trimmed.split_whitespace().collect();
            if trimmed
                .get(.."usage:".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("usage:"))
                && let Some(hint) = positional_hint(&tokens[1..])
            {
                positional = Some(hint);
            }
            let declared: Vec<_> = declaration(trimmed).split_whitespace().collect();
            let options = option_names(&declared);
            if let Some(pending) = &mut pending_values {
                match pending {
                    PendingValues::Described { .. } if trimmed.is_empty() => continue,
                    PendingValues::Described {
                        values: enum_values,
                        ..
                    } => {
                        if let Some(value) = described_enum_value(trimmed) {
                            enum_values.push(value);
                            continue;
                        }
                        finish_pending_values(&mut values, pending_values.take());
                    }
                    PendingValues::Wrapped {
                        values: enum_values,
                        ..
                    } => {
                        let (more, complete) = wrapped_enum_values(trimmed);
                        enum_values.extend(more);
                        if complete {
                            finish_pending_values(&mut values, pending_values.take());
                        }
                        continue;
                    }
                }
            }
            if line == trimmed && trimmed.ends_with(':') {
                current_options.clear();
            }
            if commands_heading(trimmed) {
                commands = true;
                entry_column = None;
                continue;
            }
            // Only the next heading closes the table. Git separates its groups
            // with blank lines and captions them at column 0 ("work on the
            // current change (see also: git help everyday)"), so ending the
            // section at either would stop the table at its first group.
            if trimmed.ends_with(':') {
                commands = false;
            }
            candidates.extend(options.iter().cloned());
            if !options.is_empty() {
                current_options.clone_from(&options);
            }
            if let Some((enum_values, complete)) = inline_possible_values(trimmed) {
                // Cargo puts the description on its own line under the option
                // ("--color <WHEN>" then "Coloring [possible values: auto,
                // always, never]"), so a list on a line that names no option
                // belongs to the last option named.
                let described = if options.is_empty() {
                    &current_options
                } else {
                    &options
                };
                if complete {
                    for option in described {
                        values.insert(option.clone(), ValueHint::Enum(enum_values.clone()));
                    }
                } else if !described.is_empty() {
                    pending_values = Some(PendingValues::Wrapped {
                        options: described.clone(),
                        values: enum_values,
                    });
                }
            } else if let Some(hint) = value_hint(&declared) {
                for option in &options {
                    values.insert(option.clone(), hint.clone());
                }
            }
            if starts_multiline_values(trimmed) && !current_options.is_empty() {
                pending_values = Some(PendingValues::Described {
                    options: current_options.clone(),
                    values: Vec::new(),
                });
            }
            // An entry in the table is indented under it; the caption of a group
            // and the prose that follows the table are not. That indentation is
            // what keeps "concept guides. See 'git help <command>' …" out of the
            // candidates now that a blank line no longer ends the section.
            //
            // The description of an entry wraps to a *deeper* column than the
            // entry itself — `rustup --help` in a narrow terminal breaks
            // "Generate tab-completion scripts for / your shell" — so an entry
            // has to start where the first one did. `command_names` rules out
            // the rest by shape.
            if commands && let Some(indent) = indentation(line) {
                let names = command_names(trimmed);
                if !names.is_empty() && indent <= *entry_column.get_or_insert(indent) {
                    entry_column = Some(indent);
                    candidates.extend(names);
                }
            }
        }
        finish_pending_values(&mut values, pending_values);
        candidates.sort();
        candidates.dedup();
        Self {
            candidates,
            values,
            positional,
        }
    }

    /// A spec the user wrote by hand.
    ///
    /// The generated sources read whatever a program happens to print, so they
    /// are heuristic by nature. A curated spec exists for when that guess was
    /// wrong, which means it has to *say* things rather than have them inferred:
    /// one candidate per line, and a value type spelled out rather than deduced
    /// from a metavar's name.
    ///
    /// ```text
    /// # mesh completions for demo
    /// --verbose
    /// --output file
    /// --color auto|always|never
    /// build
    /// positional dir
    /// ```
    ///
    /// A line naming no candidate is skipped rather than failing the file: a
    /// spec that mostly works beats one the shell refuses to load at a prompt,
    /// where there is nowhere good to report it.
    fn parse_curated(text: &str) -> Option<Self> {
        let mut spec = Self::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (name, rest) = match line.split_once(char::is_whitespace) {
                Some((name, rest)) => (name, rest.trim()),
                None => (line, ""),
            };
            let hint = (!rest.is_empty()).then(|| curated_hint(rest));
            if name == "positional" {
                spec.positional = hint;
                continue;
            }
            spec.candidates.push(name.to_owned());
            if let Some(hint) = hint {
                spec.values.insert(name.to_owned(), hint);
            }
        }
        // An empty file says nothing, and answering with an empty spec would
        // suppress the generated one rather than override it.
        if spec.candidates.is_empty() && spec.positional.is_none() {
            return None;
        }
        spec.candidates.sort();
        spec.candidates.dedup();
        Some(spec)
    }

    pub(crate) fn matching(&self, prefix: &str) -> Vec<String> {
        let candidates = self
            .candidates
            .iter()
            // A leading `-` says which kind of candidate is wanted, so the two
            // kinds do not cross: `-p` must not reach `cherry-pick`, which the
            // fuzzy matcher happily finds by its `-` and its `p`, and which is
            // not a thing you could run with a dash in front of it. An empty
            // prefix has said nothing yet, so it offers both.
            .filter(|candidate| {
                prefix.is_empty() || prefix.starts_with('-') == candidate.starts_with('-')
            })
            .cloned()
            .collect();
        rank_candidates(candidates, prefix)
    }

    pub(crate) fn value_hint(&self, option: &str) -> Option<&ValueHint> {
        self.values.get(option)
    }

    pub(crate) fn positional_hint(&self) -> Option<&ValueHint> {
        self.positional.as_ref()
    }

    fn encode(&self, fingerprint: &str) -> String {
        let mut encoded = format!("{CACHE_VERSION}\n{fingerprint}\n");
        for candidate in &self.candidates {
            encoded.push_str("candidate\t");
            encoded.push_str(candidate);
            encoded.push('\n');
        }
        let mut values: Vec<_> = self.values.iter().collect();
        values.sort_by_key(|(option, _)| *option);
        for (option, hint) in values {
            encoded.push_str("value\t");
            encoded.push_str(option);
            encode_hint(&mut encoded, hint);
        }
        if let Some(hint) = &self.positional {
            encoded.push_str("positional");
            encode_hint(&mut encoded, hint);
        }
        encoded
    }

    fn decode(encoded: &str, fingerprint: &str) -> Option<Self> {
        let mut lines = encoded.lines();
        if lines.next()? != CACHE_VERSION || lines.next()? != fingerprint {
            return None;
        }
        let mut spec = Self::default();
        for line in lines {
            let mut fields = line.split('\t');
            match fields.next()? {
                "candidate" => spec.candidates.push(fields.next()?.to_owned()),
                "value" => {
                    let option = fields.next()?.to_owned();
                    let hint = decode_hint(&mut fields)?;
                    spec.values.insert(option, hint);
                }
                "positional" => spec.positional = Some(decode_hint(&mut fields)?),
                _ => return None,
            }
        }
        Some(spec)
    }
}

fn encode_hint(encoded: &mut String, hint: &ValueHint) {
    match hint {
        ValueHint::File => encoded.push_str("\tfile\n"),
        ValueHint::Directory => encoded.push_str("\tdirectory\n"),
        ValueHint::ManPage => encoded.push_str("\tmanpage\n"),
        ValueHint::Enum(values) => {
            encoded.push_str("\tenum");
            for value in values {
                encoded.push('\t');
                encoded.push_str(value);
            }
            encoded.push('\n');
        }
    }
}

fn decode_hint<'a>(fields: &mut impl Iterator<Item = &'a str>) -> Option<ValueHint> {
    match fields.next()? {
        "file" => Some(ValueHint::File),
        "directory" => Some(ValueHint::Directory),
        "manpage" => Some(ValueHint::ManPage),
        "enum" => Some(ValueHint::Enum(fields.map(str::to_owned).collect())),
        _ => None,
    }
}

pub(crate) fn rank_candidates(candidates: Vec<String>, query: &str) -> Vec<String> {
    if query.is_empty() {
        return candidates;
    }
    let (mut ranked, remaining): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|candidate| candidate == query);
    let exact_case = Atom::new(
        query,
        CaseMatching::Respect,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let case_matches = exact_case.match_list(remaining.clone(), &mut matcher);
    let matched: HashSet<_> = case_matches
        .iter()
        .map(|(candidate, _)| candidate.clone())
        .collect();
    ranked.extend(case_matches.into_iter().map(|(candidate, _)| candidate));

    let smart_case = Atom::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );
    ranked.extend(
        smart_case
            .match_list(
                remaining
                    .into_iter()
                    .filter(|candidate| !matched.contains(candidate)),
                &mut matcher,
            )
            .into_iter()
            .map(|(candidate, _)| candidate),
    );
    ranked
}

/// Does this line open a table of subcommands?
///
/// `Commands:` and `Available Commands:` are the common spellings, but git
/// captions its table with a whole sentence — "These are common Git commands
/// used in various situations:" — so a heading counts when any of its words is
/// "commands". Without the colon the line has to be little else, since an
/// ordinary sentence mentioning commands is not a heading.
fn commands_heading(line: &str) -> bool {
    let heading = line.strip_suffix(':');
    let words: Vec<_> = heading.unwrap_or(line).split_whitespace().collect();
    (heading.is_some() || words.len() <= 2)
        && words.iter().any(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            word.eq_ignore_ascii_case("commands") || word.eq_ignore_ascii_case("subcommands")
        })
}

/// How far a line is indented, or `None` for one that is blank or starts at
/// column 0.
fn indentation(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start().len();
    (indent > 0 && !line.trim().is_empty()).then_some(indent)
}

/// The commands one entry of a table introduces, or nothing if the line is not
/// an entry.
///
/// An entry is a name — or several, since cargo writes `build, b` for a
/// subcommand and its alias — set off from its description by two spaces or a
/// tab, or standing alone on the line. Docker stars a command that comes from a
/// plugin (`buildx*  Docker Buildx`); the star is a footnote mark, not part of
/// the name.
///
/// Prose does not have that shape, which is what a section has to be told by:
/// rustup captions its worked examples "Common commands:", and reading
/// "Update Rust toolchains and rustup" as a table entry offers `Update` as a
/// subcommand you could run.
fn command_names(entry: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = entry;
    loop {
        let (token, tail) = rest.split_at(rest.find(char::is_whitespace).unwrap_or(rest.len()));
        let name = token.trim_end_matches(['*', ',']);
        let named = !name.is_empty()
            && !name.starts_with('-')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !named {
            return Vec::new();
        }
        names.push(name.to_owned());
        // Only a trailing comma promises another name; after the last one comes
        // the description, and a single space between them means this was a
        // sentence rather than a table.
        if !token.ends_with(',') {
            let separated =
                tail.trim().is_empty() || tail.starts_with('\t') || tail.starts_with("  ");
            return if separated { names } else { Vec::new() };
        }
        rest = tail.trim_start();
        if rest.is_empty() {
            return names;
        }
    }
}

/// The part of a line that *declares* options, with any description dropped.
///
/// An option entry names its flags first and describes them after a two-space or
/// tab gap — the same boundary `command_names` reads a table entry by. Scanning
/// the description too offers whatever the prose happens to mention: `ls --help`
/// writes "with `-l`, print the author of each file" under `--author` and "with
/// `-lt`: sort by, and show, ctime" under `-c`, so `-lt` became a flag you could
/// complete even though `ls` declares no such option.
///
/// A gap only ends the declaration when a description follows it. Help that
/// aligns its short and long forms in columns puts one *between* them —
/// `-h<TAB><TAB><TAB>--help` — so cutting at the first gap would keep `-h` and
/// throw the alias away. Every gap-separated column that declares an option
/// stays; the first one that does not is where the description begins.
///
/// A line that does not lead with an option is left whole. A usage line spreads
/// its flags across the full width (`usage: git [-v | --version] [-C <path>]`)
/// and separates them by single spaces, so it has no description to cut.
fn declaration(trimmed: &str) -> &str {
    if !declares_option(trimmed) {
        return trimmed;
    }
    let mut kept = 0;
    let mut rest = trimmed;
    loop {
        let Some(gap) = rest.find('\t').into_iter().chain(rest.find("  ")).min() else {
            return trimmed;
        };
        let next = rest[gap..].trim_start();
        if !declares_option(next) {
            return &trimmed[..kept + gap];
        }
        kept += rest.len() - next.len();
        rest = next;
    }
}

/// Whether text begins by naming an option, looking past the bracket a usage
/// line wraps an optional flag in.
fn declares_option(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(|token| token.trim_start_matches(['[', '(']).starts_with('-'))
}

fn option_names(tokens: &[&str]) -> Vec<String> {
    tokens
        .iter()
        // A bar separates alternatives, spaced or not: `[-p | --paginate]` is
        // three tokens but `[--all|--quiet]` is one, and both name two flags.
        .flat_map(|token| token.split('|'))
        .filter_map(|token| {
            // Help text writes an option inside whatever punctuation the
            // sentence or usage line needs — `[-v | --version]`,
            // `'git help -a'`, `(-vv very verbose)`, `-v, --verbose...`, `with
            // -lt:`, ``rustup doc --book``. None of it is part of the name, and
            // leaving it on offers `--version]` as a flag.
            let option =
                token.trim_matches(['[', ']', '(', ')', ',', ';', ':', '.', '\'', '"', '`']);
            (option.starts_with('-') && option.len() > 1).then(|| {
                option
                    .split(['=', '[', '<'])
                    .next()
                    .unwrap_or(option)
                    .to_owned()
            })
        })
        .collect()
}

fn value_hint(tokens: &[&str]) -> Option<ValueHint> {
    let metavar = tokens.iter().find_map(|token| {
        let token = token.trim_end_matches([',', ';']);
        token
            .split_once('=')
            .map(|(_, value)| value)
            .or_else(|| (token.starts_with(['<', '[', '{'])).then_some(token))
    })?;
    let metavar = metavar.trim_matches(['<', '>', '[', ']', '{', '}']);
    let alternatives: Vec<_> = metavar
        .split(['|', ','])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    // A file/path alternative wins over a directory one: `File` completion
    // already offers directories too, so a mixed `<FILE|DIR>` union stays usable
    // for either kind. `Directory` is reserved for directory-only metavars.
    if alternatives.iter().any(|value| file_metavar(value)) {
        return Some(ValueHint::File);
    }
    if alternatives.iter().any(|value| directory_metavar(value)) {
        return Some(ValueHint::Directory);
    }
    if alternatives.len() > 1 && alternatives.iter().all(|value| enum_literal(value)) {
        return Some(ValueHint::Enum(
            alternatives.into_iter().map(str::to_owned).collect(),
        ));
    }
    None
}

/// The value type of a usage line's *operands*, skipping the metavars that
/// belong to an option.
///
/// `usage: git [-v | --version] [-C <path>] … <command> [<args>]` names a file
/// only inside `[-C <path>]`, which is `-C`'s value — reading it as git's
/// operand had `git a<Tab>` offering the files in the current directory.
fn positional_hint(tokens: &[&str]) -> Option<ValueHint> {
    let mut hints = Vec::new();
    let mut option_group = false;
    for token in tokens {
        if token.starts_with(['[', '(']) {
            option_group = false;
        }
        if token.trim_start_matches(['[', '(']).starts_with('-') {
            option_group = true;
        }
        if !option_group
            && let Some(hint) = value_hint(&[*token]).or_else(|| bare_metavar_hint(token))
        {
            hints.push(hint);
        }
        if token.ends_with([']', ')']) {
            option_group = false;
        }
    }
    if hints.contains(&ValueHint::File) {
        Some(ValueHint::File)
    } else if hints.contains(&ValueHint::Directory) {
        Some(ValueHint::Directory)
    } else if hints.contains(&ValueHint::ManPage) {
        Some(ValueHint::ManPage)
    } else {
        None
    }
}

/// The value type of an unbracketed metavar — the `PAGE...` of
/// `Usage: man [OPTION...] [SECTION] PAGE...`.
///
/// Only a usage line is read this way. An all-caps word elsewhere is prose
/// ("suppress FILE output"), and taking that for a metavar would hang a value
/// type on whichever option the sentence happens to describe.
fn bare_metavar_hint(token: &str) -> Option<ValueHint> {
    let metavar = token.trim_matches(|character: char| !character.is_ascii_alphanumeric());
    let shaped = metavar.chars().all(|character| {
        character.is_ascii_uppercase()
            || character.is_ascii_digit()
            || matches!(character, '-' | '_')
    }) && metavar
        .chars()
        .any(|character| character.is_ascii_uppercase());
    if !shaped {
        return None;
    }
    if file_metavar(metavar) {
        Some(ValueHint::File)
    } else if directory_metavar(metavar) {
        Some(ValueHint::Directory)
    } else if man_page_metavar(metavar) {
        Some(ValueHint::ManPage)
    } else {
        None
    }
}

fn inline_possible_values(line: &str) -> Option<(Vec<String>, bool)> {
    let lowercase = line.to_ascii_lowercase();
    let at = lowercase.find("possible values:")?;
    let remainder = &line[at + "possible values:".len()..];
    let (values, complete) = wrapped_enum_values(remainder);
    // Clap wraps long lists inside `[possible values: …]`, which may spill onto
    // later lines and closes at the `]`. An unbracketed inline list (no opening
    // `[` before the marker) is confined to this line, so treat it as complete
    // rather than swallowing the following options while hunting for a `]`.
    let bracketed = line[..at].contains('[');
    Some((values, complete || !bracketed))
}

fn wrapped_enum_values(line: &str) -> (Vec<String>, bool) {
    let complete = line.contains(']');
    let values = line
        .trim_matches(|character: char| {
            character.is_whitespace() || matches!(character, '[' | ']' | '(' | ')')
        })
        .split(',')
        .map(|value| value.trim().trim_matches(['`', '\'', '"']))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    (values, complete)
}

fn starts_multiline_values(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("possible values for this flag are:")
        || line.contains("possible values for this option are:")
}

fn described_enum_value(line: &str) -> Option<String> {
    let value = line.split_once(':').map_or(line, |(value, _)| value).trim();
    enum_literal(value).then(|| value.to_owned())
}

fn finish_pending_values(hints: &mut HashMap<String, ValueHint>, pending: Option<PendingValues>) {
    let Some((options, values)) = pending
        .map(|pending| match pending {
            PendingValues::Described { options, values }
            | PendingValues::Wrapped { options, values } => (options, values),
        })
        .filter(|(_, values)| !values.is_empty())
    else {
        return;
    };
    for option in options {
        hints.insert(option, ValueHint::Enum(values.clone()));
    }
}

fn directory_metavar(value: &str) -> bool {
    metavar_words(value)
        .any(|word| word.eq_ignore_ascii_case("DIRECTORY") || word.eq_ignore_ascii_case("DIR"))
}

fn file_metavar(value: &str) -> bool {
    metavar_words(value)
        .any(|word| word.eq_ignore_ascii_case("FILE") || word.eq_ignore_ascii_case("PATH"))
}

fn man_page_metavar(value: &str) -> bool {
    metavar_words(value).any(|word| word.eq_ignore_ascii_case("PAGE"))
}

fn metavar_words(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
}

fn enum_literal(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '-' | '_')
    }) && value
        .chars()
        .any(|character| character.is_ascii_lowercase())
}

impl From<&str> for CompletionSpec {
    fn from(help: &str) -> Self {
        Self::from_help(help)
    }
}

#[derive(Debug)]
pub(crate) struct CompletionCache {
    directory: Option<PathBuf>,
    curated: Option<PathBuf>,
    memory: Mutex<HashMap<String, CompletionSpec>>,
}

impl Default for CompletionCache {
    fn default() -> Self {
        Self {
            directory: cache_directory(),
            curated: curated_directory(),
            memory: Mutex::new(HashMap::new()),
        }
    }
}

impl CompletionCache {
    /// A cache with no curated directory behind it, which is what a test of the
    /// generated path wants: the user's own `~/.local/share` must not decide
    /// what it sees. Outside a test the only constructor is `Default`, which
    /// reads both directories from the environment.
    #[cfg(test)]
    pub(crate) fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            curated: None,
            memory: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_curated(curated: Option<PathBuf>, directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            curated,
            memory: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn spec_for(&self, words: &[String]) -> CompletionSpec {
        let Some((command, args)) = words.split_first() else {
            return CompletionSpec::default();
        };
        // A curated spec is the user's own answer, so it is read before anything
        // is resolved or run — it holds for a command that is not on `PATH` at
        // all, and it is read afresh every time rather than cached, so editing
        // one takes effect at the next Tab.
        if let Some(spec) = self.curated_spec(words) {
            return spec;
        }
        let Some(executable) = resolve_command(command) else {
            return CompletionSpec::default();
        };
        let Ok(metadata) = fs::metadata(&executable) else {
            return CompletionSpec::default();
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        let fingerprint = format!(
            "{}\0{modified}\0{}\0{}",
            executable.display(),
            metadata.len(),
            args.join("\0")
        );
        let cache_name = cache_name(&executable, args);

        if let Some(spec) = self
            .memory
            .lock()
            .expect("completion cache poisoned")
            .get(&fingerprint)
            .cloned()
        {
            return spec;
        }
        if let Some(spec) = self.read(&cache_name, &fingerprint) {
            self.remember(fingerprint, spec.clone());
            return spec;
        }

        let mut probe = vec![executable.to_string_lossy().into_owned()];
        probe.extend_from_slice(args);
        let spec = CompletionSpec::from_help(&command_help(&probe));
        self.write(&cache_name, &fingerprint, &spec);
        self.remember(fingerprint, spec.clone());
        spec
    }

    /// The curated spec for these words, if the user wrote one.
    ///
    /// Named the way the manual names the same thing — `git`, `git-commit` — so
    /// a spec for a subcommand sits beside the one for its command instead of
    /// needing a directory per command. A name with a separator in it is refused
    /// rather than joined, so a command word can never reach outside the
    /// directory.
    fn curated_spec(&self, words: &[String]) -> Option<CompletionSpec> {
        let directory = self.curated.as_ref()?;
        if words
            .iter()
            .any(|word| word.is_empty() || word.contains('/') || word == "." || word == "..")
        {
            return None;
        }
        let path = directory.join(words.join("-"));
        CompletionSpec::parse_curated(&fs::read_to_string(path).ok()?)
    }

    fn remember(&self, fingerprint: String, spec: CompletionSpec) {
        self.memory
            .lock()
            .expect("completion cache poisoned")
            .insert(fingerprint, spec);
    }

    fn read(&self, name: &str, fingerprint: &str) -> Option<CompletionSpec> {
        let path = self.directory.as_ref()?.join(name);
        CompletionSpec::decode(&fs::read_to_string(path).ok()?, fingerprint)
    }

    fn write(&self, name: &str, fingerprint: &str, spec: &CompletionSpec) {
        let Some(directory) = &self.directory else {
            return;
        };
        if fs::create_dir_all(directory).is_err() {
            return;
        }
        let path = directory.join(name);
        let temporary = directory.join(format!(".{name}-{}", std::process::id()));
        if fs::write(&temporary, spec.encode(fingerprint)).is_ok() {
            let _ = fs::rename(&temporary, path);
        }
        let _ = fs::remove_file(temporary);
    }
}

/// The value type a curated line spells out.
///
/// The four names are the value types a spec can hold; anything else is read as
/// a list of literal values, so `auto|always|never` and a single fixed word both
/// mean what they look like.
fn curated_hint(text: &str) -> ValueHint {
    match text {
        "file" => ValueHint::File,
        "dir" | "directory" => ValueHint::Directory,
        "page" | "manpage" => ValueHint::ManPage,
        _ => ValueHint::Enum(
            text.split('|')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
    }
}

/// Where a hand-written spec goes: data rather than cache, since nothing
/// regenerates it and losing it loses the user's work.
fn curated_directory() -> Option<PathBuf> {
    if let Some(path) = env::var_os("XDG_DATA_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("mesh/completions"));
    }
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(path).join(".local/share/mesh/completions"))
}

fn cache_directory() -> Option<PathBuf> {
    if let Some(path) = env::var_os("XDG_CACHE_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("mesh/completions"));
    }
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(path).join(".cache/mesh/completions"))
}

fn cache_name(executable: &Path, args: &[String]) -> String {
    let mut hasher = DefaultHasher::new();
    executable.hash(&mut hasher);
    args.hash(&mut hasher);
    format!("{:016x}.spec", hasher.finish())
}

fn resolve_command(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path
            .is_file()
            .then(|| fs::canonicalize(path).unwrap_or_else(|_| path.into()));
    }
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(command))
            .find(|candidate| candidate.is_file())
            .map(|candidate| fs::canonicalize(&candidate).unwrap_or(candidate))
    })
}

/// Every manual page installed on this system, named the way `man` takes it:
/// `ls`, `git-add`, `CPAN::Meta`.
///
/// The scan reads directories only — no `man`, `manpath`, or `apropos` is run,
/// so a `PAGE` argument completes without executing anything and without a
/// `mandb` index having been built.
pub(crate) fn man_pages() -> Vec<String> {
    let mut roots = man_roots();
    roots.sort();
    roots.dedup();
    let mut pages: Vec<String> = roots.iter().flat_map(|root| pages_in(root)).collect();
    pages.sort();
    pages.dedup();
    pages
}

fn man_roots() -> Vec<PathBuf> {
    let manpath = env::var_os("MANPATH").unwrap_or_default();
    let configured: Vec<PathBuf> = env::split_paths(&manpath)
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    // A `MANPATH` with no empty entry replaces the search path outright, which
    // is `man`'s own rule; an empty entry — a leading, trailing, or doubled `:`
    // — asks for the derived path to be spliced in, and so does no `MANPATH`.
    let replaces = !configured.is_empty()
        && env::split_paths(&manpath).all(|path| !path.as_os_str().is_empty());
    if replaces {
        return configured;
    }
    let mut roots = configured;
    roots.extend(derived_man_roots());
    roots
}

/// Where `man` looks when `MANPATH` does not say: the manual tree beside each
/// `$PATH` directory — `/opt/pkg/bin` is documented in `/opt/pkg/share/man` —
/// plus the system trees, which stay in the list for a `$PATH` that misses them.
fn derived_man_roots() -> Vec<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    let mut roots: Vec<PathBuf> = env::split_paths(&path)
        .filter_map(|directory| directory.parent().map(Path::to_path_buf))
        .flat_map(|prefix| [prefix.join("share/man"), prefix.join("man")])
        .collect();
    roots.extend(
        ["/usr/share/man", "/usr/local/share/man", "/usr/local/man"]
            .into_iter()
            .map(PathBuf::from),
    );
    roots
}

/// The pages under one manual tree. Only its own `man<section>` directories are
/// read: a translated tree (`.../man/fr/man1`) names the same pages again.
fn pages_in(root: &Path) -> Vec<String> {
    let Ok(sections) = fs::read_dir(root) else {
        return Vec::new();
    };
    sections
        .flatten()
        .filter(|section| section.file_name().as_encoded_bytes().starts_with(b"man"))
        .filter_map(|section| fs::read_dir(section.path()).ok())
        .flat_map(|pages| {
            pages
                .flatten()
                .filter_map(|page| page_name(&page.file_name().to_string_lossy()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The name a page file answers to: `ls.1.gz` is `ls`, `git-add.1` is `git-add`,
/// `CPAN::Meta.3pm.gz` is `CPAN::Meta`. A file with no section suffix is not a
/// page, so it contributes no name.
fn page_name(file: &str) -> Option<String> {
    const COMPRESSED: [&str; 6] = ["gz", "bz2", "xz", "zst", "lzma", "Z"];
    let stem = match file.rsplit_once('.') {
        Some((stem, suffix)) if COMPRESSED.contains(&suffix) => stem,
        _ => file,
    };
    let (name, section) = stem.rsplit_once('.')?;
    let sectioned = section.starts_with(|character: char| {
        character.is_ascii_digit() || matches!(character, 'n' | 'l')
    });
    (!name.is_empty() && sectioned).then(|| name.to_owned())
}

pub(crate) fn command_help(words: &[String]) -> String {
    let Some((command, args)) = words.split_first() else {
        return String::new();
    };
    let mut process = Command::new(command);
    process
        .args(args)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: only async-signal-safe process-group and signal calls run between
    // fork and exec. A help child must not inherit the shell's ignored signals.
    unsafe {
        process.pre_exec(|| {
            if libc::setpgid(0, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            for signal in [
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTSTP,
                libc::SIGTTOU,
                libc::SIGTERM,
            ] {
                if libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let Ok(mut child) = process.spawn() else {
        return String::new();
    };
    let child_group = child.id() as libc::pid_t;
    let shell_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
    if shell_group >= 0 {
        unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, child_group) };
    }
    let stdout = pipe_reader(child.stdout.take());
    let stderr = pipe_reader(child.stderr.take());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                unsafe { libc::kill(-child_group, libc::SIGKILL) };
                let _ = child.wait();
                break;
            }
            Err(_) => break,
        }
    }
    unsafe {
        libc::kill(-child_group, libc::SIGKILL);
        if shell_group >= 0 {
            libc::tcsetpgrp(libc::STDIN_FILENO, shell_group);
        }
    }
    let mut text = String::from_utf8_lossy(&join_reader(stdout)).into_owned();
    text.push_str(&String::from_utf8_lossy(&join_reader(stderr)));
    text
}

fn pipe_reader<T: Read + Send + 'static>(pipe: Option<T>) -> Option<thread::JoinHandle<Vec<u8>>> {
    pipe.map(|output| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = output.take(1024 * 1024).read_to_end(&mut bytes);
            bytes
        })
    })
}

fn join_reader(reader: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionCache, CompletionSpec, ValueHint, cache_name, command_help, pages_in,
        rank_candidates, resolve_command,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};

    fn helper(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fresh_temp_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        if let Err(error) = fs::remove_dir_all(&root)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("failed to clean temporary directory: {error}");
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Run a completion-cache scenario that spawns a helper subprocess, retrying
    /// from clean state when a probe transiently produces nothing.
    ///
    /// These tests assert on a probe's output and on how many times the helper
    /// ran, but spawning and `exec`ing a just-written script can occasionally
    /// yield an empty probe under heavy parallel test load — orthogonal to what
    /// the test checks. The scenario returns `Err` for exactly that transient
    /// case (so the attempt is discarded and retried); a genuine assertion failure
    /// still panics inside the closure and fails the test immediately.
    fn retrying(mut scenario: impl FnMut() -> Result<(), String>) {
        let mut last = String::new();
        for _ in 0..8 {
            match scenario() {
                Ok(()) => return,
                Err(reason) => last = reason,
            }
        }
        panic!("completion-cache scenario never completed a probe: {last}");
    }

    #[test]
    fn parses_typed_specs() {
        let spec = CompletionSpec::from_help(
            "Commands:\n  build  compile\n\nOptions:\n  -h, --help  help\n  -o, --output <FILE>  output file\n  --directory <DIR>  working directory\n  --color <WHEN>  color [possible values: auto, always, never]\n  --format=<json|text>  output format\n  --config <KEY=VALUE|PATH>  config override or file\n  --placeholder <LEFT|RIGHT>  metasyntactic union\n",
        );
        assert_eq!(spec.matching("--h")[0], "--help");
        assert_eq!(spec.matching("b"), ["build"]);
        assert_eq!(spec.value_hint("-o"), Some(&ValueHint::File));
        assert_eq!(spec.value_hint("--output"), Some(&ValueHint::File));
        assert_eq!(spec.value_hint("--directory"), Some(&ValueHint::Directory));
        assert_eq!(
            spec.value_hint("--color"),
            Some(&ValueHint::Enum(vec![
                "auto".into(),
                "always".into(),
                "never".into()
            ]))
        );
        assert_eq!(
            spec.value_hint("--format"),
            Some(&ValueHint::Enum(vec!["json".into(), "text".into()]))
        );
        assert_eq!(spec.value_hint("--config"), Some(&ValueHint::File));
        assert_eq!(spec.value_hint("--placeholder"), None);
        assert_eq!(
            CompletionSpec::decode(&spec.encode("test"), "test"),
            Some(spec)
        );
    }

    #[test]
    fn parses_vim_usage_file_positionals() {
        let spec = CompletionSpec::from_help(
            "VIM - Vi IMproved 9.2\n\
             \n\
             Usage: vim [arguments] [file ..]       edit specified file(s)\n\
                or: vim [arguments] -               read text from stdin\n\
                or: vim [arguments] -t tag          edit file where tag is defined\n\
                or: vim [arguments] -q [errorfile]  edit file with first error\n\
             \n\
             Arguments:\n\
                --                 Only file names after this\n\
                -v                 Vi mode (like \"vi\")\n\
                -T <terminal>      Set terminal type to <terminal>\n\
                --not-a-term       Skip warning for input/output not being a terminal\n\
                -u <vimrc>         Use <vimrc> instead of any .vimrc\n\
                +<lnum>            Start at line <lnum>\n",
        );

        assert_eq!(spec.positional_hint(), Some(&ValueHint::File));
        assert_eq!(spec.matching("-T"), ["-T"]);
        assert_eq!(spec.matching("--not"), ["--not-a-term"]);
        assert_eq!(spec.matching("-u"), ["-u"]);
        assert_eq!(
            CompletionSpec::decode(&spec.encode("vim"), "vim"),
            Some(spec)
        );
    }

    #[test]
    fn parses_git_style_command_tables() {
        let spec = CompletionSpec::from_help(
            "usage: git [-v | --version] [-h | --help] [-C <path>]\n\
             \x20          [--git-dir=<path>] <command> [<args>]\n\
             \n\
             These are common Git commands used in various situations:\n\
             \n\
             start a working area (see also: git help tutorial)\n\
             \x20  clone     Clone a repository into a new directory\n\
             \x20  init      Create an empty Git repository\n\
             \n\
             work on the current change (see also: git help everyday)\n\
             \x20  add       Add file contents to the index\n\
             \x20  restore   Restore working tree files\n\
             \n\
             'git help -a' and 'git help -g' list available subcommands and some\n\
             concept guides. See 'git help <command>' or 'git help <concept>'\n",
        );

        // A blank line separates the groups, so the table has to survive one.
        assert_eq!(spec.matching("a")[0], "add");
        assert_eq!(spec.matching("res"), ["restore"]);
        assert!(spec.matching("").iter().any(|value| value == "init"));
        // The group captions and the closing prose sit at column 0, which is
        // what keeps them out of the table.
        for prose in ["start", "work", "concept"] {
            assert!(
                !spec.matching("").iter().any(|value| value == prose),
                "{prose} was taken for a subcommand"
            );
        }
        // Both ends of a usage line's brackets come off, so the flag inside is
        // offered under its own name.
        assert_eq!(spec.matching("--v"), ["--version"]);
        assert_eq!(spec.matching("--git"), ["--git-dir"]);
    }

    #[test]
    fn bar_separated_alternatives_name_each_option() {
        // Git spaces its alternatives out (`[-v | --version]`), but mesh's own
        // builtin help writes them in one token. Both name two flags, not one
        // flag spelled `--all|--quiet`.
        let spec = CompletionSpec::from_help("Usage: whence [--all|--quiet] NAME ...\n");

        assert_eq!(spec.matching("--a"), ["--all"]);
        assert_eq!(spec.matching("--q"), ["--quiet"]);
        // `NAME` is an operand of no known type, so completion falls back to
        // paths rather than claiming one.
        assert_eq!(spec.positional_hint(), None);
    }

    #[test]
    fn command_headings_need_more_than_the_word() {
        // A colon makes a caption a heading; without one it has to be a heading
        // and nothing more, or every sentence about commands opens a table.
        let spec = CompletionSpec::from_help(
            "Run 'tool help' to list the commands\n  notacommand  prose under prose\n\nCommands\n  real  a command\n",
        );

        assert_eq!(spec.matching(""), ["real"]);
    }

    #[test]
    fn man_usage_positionals_are_manual_pages() {
        let spec = CompletionSpec::from_help(
            "Usage: man [OPTION...] [SECTION] PAGE...\n\
             \n\
             \x20-C, --config-file=FILE     use this user configuration file\n\
             \x20-w, --path                 print physical location of man page(s)\n",
        );

        // `PAGE` is the operand, and it names the manual rather than the
        // directory you happen to be standing in.
        assert_eq!(spec.positional_hint(), Some(&ValueHint::ManPage));
        assert_eq!(spec.value_hint("--config-file"), Some(&ValueHint::File));
        assert_eq!(
            CompletionSpec::decode(&spec.encode("man"), "man"),
            Some(spec)
        );
    }

    #[test]
    fn unbracketed_usage_operands_are_read_as_metavars() {
        // A usage line's bare operand is a metavar; the same word in a
        // description is prose, and must not type the option it describes.
        let spec = CompletionSpec::from_help(
            "Usage: tool [OPTION]... FILE...\n\nOptions:\n  --quiet  suppress FILE output\n",
        );

        assert_eq!(spec.positional_hint(), Some(&ValueHint::File));
        assert_eq!(spec.value_hint("--quiet"), None);
    }

    #[test]
    fn a_curated_spec_says_its_candidates_and_their_types() {
        let spec = CompletionSpec::parse_curated(concat!(
            "# mesh completions for demo\n",
            "\n",
            "--verbose\n",
            "--output file          # where to write\n",
            "--into dir\n",
            "--color auto|always|never\n",
            "--page page\n",
            "build\n",
            "positional dir\n",
        ))
        .expect("a spec");

        assert_eq!(
            spec.matching("-"),
            ["--color", "--into", "--output", "--page", "--verbose"]
        );
        assert_eq!(spec.matching("b"), ["build"]);
        assert_eq!(spec.value_hint("--output"), Some(&ValueHint::File));
        assert_eq!(spec.value_hint("--into"), Some(&ValueHint::Directory));
        assert_eq!(spec.value_hint("--page"), Some(&ValueHint::ManPage));
        assert_eq!(
            spec.value_hint("--color"),
            Some(&ValueHint::Enum(vec![
                "auto".into(),
                "always".into(),
                "never".into()
            ]))
        );
        assert_eq!(spec.value_hint("--verbose"), None);
        assert_eq!(spec.positional_hint(), Some(&ValueHint::Directory));
    }

    #[test]
    fn a_curated_file_that_says_nothing_does_not_override() {
        // Empty or comments-only must fall through to the generated spec rather
        // than answer with nothing, which would look like "this command has no
        // completions" instead of "no one curated it".
        assert!(CompletionSpec::parse_curated("").is_none());
        assert!(CompletionSpec::parse_curated("# just a note\n\n").is_none());
        // A positional alone is a real answer, even with no candidates.
        assert!(CompletionSpec::parse_curated("positional file\n").is_some());
    }

    #[test]
    fn a_curated_spec_outranks_the_probe_and_needs_no_executable() {
        let root = fresh_temp_dir("mesh-curated");
        let curated = root.join("curated");
        fs::create_dir_all(&curated).unwrap();
        let command = root.join("helper");
        helper(&command, "echo '  --probed  from the probe'");

        // Named for the command as typed, so the curated spec answers even
        // though the words name a path the probe would otherwise run.
        fs::write(curated.join("nothing-on-path"), "--curated\n").unwrap();
        let cache = CompletionCache::with_curated(Some(curated), Some(root.join("cache")));

        let spec = cache.spec_for(&["nothing-on-path".to_owned()]);
        assert_eq!(spec.matching("-"), ["--curated"]);

        // A command with no curated file still reaches the probe.
        let probed = cache.spec_for(&[command.to_string_lossy().into_owned()]);
        assert_eq!(probed.matching("-"), ["--probed"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_curated_subcommand_spec_sits_beside_its_command() {
        let root = fresh_temp_dir("mesh-curated-sub");
        let curated = root.join("curated");
        fs::create_dir_all(&curated).unwrap();
        fs::write(curated.join("demo"), "build\n").unwrap();
        fs::write(curated.join("demo-build"), "--release\n").unwrap();
        let cache = CompletionCache::with_curated(Some(curated), None);

        assert_eq!(cache.spec_for(&["demo".to_owned()]).matching(""), ["build"]);
        assert_eq!(
            cache
                .spec_for(&["demo".to_owned(), "build".to_owned()])
                .matching("-"),
            ["--release"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_curated_lookup_cannot_leave_its_directory() {
        let root = fresh_temp_dir("mesh-curated-escape");
        let curated = root.join("curated");
        fs::create_dir_all(&curated).unwrap();
        fs::write(root.join("outside"), "--reached\n").unwrap();
        let cache = CompletionCache::with_curated(Some(curated), None);

        // A command word is a file name here, never a path: joining one with a
        // separator in it would read a spec from wherever it pointed.
        for word in ["../outside", "..", ".", "/etc/passwd"] {
            assert!(
                cache.spec_for(&[word.to_owned()]).matching("").is_empty(),
                "{word} reached a spec"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn columns_of_aligned_options_all_belong_to_the_declaration() {
        // Help that lines its long forms up in a column separates them from the
        // short form by the same gap that ends a declaration. Cutting at the
        // first one would offer `-h` and lose `--help` with it.
        let spec = CompletionSpec::from_help(concat!(
            "Options:\n",
            "\t-h\t\t\t--help\n",
            "\t-V\t\t\t--version\n",
            "\t-f progfile\t\t--file=progfile\tread the program from there\n",
        ));

        assert_eq!(
            spec.matching("-"),
            ["--file", "--help", "--version", "-V", "-f", "-h"]
        );
        // The description still ends the declaration once a column stops
        // declaring options — `read the program from there` types nothing.
        assert_eq!(spec.value_hint("--file"), None);
    }

    #[test]
    fn a_dashed_prefix_offers_options_and_nothing_else() {
        let spec = CompletionSpec::from_help(concat!(
            "Commands:\n",
            "   cherry-pick   Apply changes from commits\n",
            "   push          Update remote refs\n",
            "\n",
            "Options:\n",
            "  --pretty <FMT>   Pretty-print\n",
            "  -q, --quiet      Be quiet\n",
        ));

        // Nucleo matches noncontiguously, so `-p` finds the `-` and the `p` of
        // `cherry-pick` — a subcommand you could never run dash-first.
        assert_eq!(spec.matching("-p"), ["--pretty"]);
        // A bare prefix is the mirror image: subcommands only.
        assert_eq!(spec.matching("p"), ["push", "cherry-pick"]);
        // Nothing typed yet says nothing about which kind is wanted.
        assert_eq!(
            spec.matching(""),
            ["--pretty", "--quiet", "-q", "cherry-pick", "push"]
        );
    }

    #[test]
    fn an_entrys_description_declares_nothing() {
        let spec = CompletionSpec::from_help(concat!(
            "usage: prog [-p | --paginate] <command>\n",
            "\n",
            "Options:\n",
            "  -v, --verbose      louder, unlike -q\n",
            "      --out=<FILE>    write there; see --help for -1 style output\n",
            "  -c                  sort by ctime, and show <FILE> mtime\n",
        ));

        // The declaration on each entry, and the usage line's flags — a usage
        // line spreads its options across the whole width, so it is read whole.
        assert_eq!(
            spec.matching("-"),
            ["--out", "--paginate", "--verbose", "-c", "-p", "-v"]
        );
        // A value type comes from the declaration too: `--out` keeps the one it
        // declares, and `-c` gains nothing from the metavar in its prose.
        assert_eq!(spec.value_hint("--out"), Some(&ValueHint::File));
        assert_eq!(spec.value_hint("-c"), None);
    }

    /// Specs parsed from **real** `--help` output, captured verbatim under
    /// `tests/help/` — see the README there for the commands and versions, and
    /// for how to re-capture. Hand-written help exercises one parsing rule at a
    /// time; these say what the parser does with what commands actually print.
    mod captured {
        use super::super::{CompletionSpec, ValueHint};

        const GIT: &str = include_str!("../tests/help/git.txt");
        const CARGO: &str = include_str!("../tests/help/cargo.txt");
        const DOCKER: &str = include_str!("../tests/help/docker.txt");
        const LS: &str = include_str!("../tests/help/ls.txt");
        const RUSTUP: &str = include_str!("../tests/help/rustup.txt");

        #[test]
        fn git_offers_subcommands_not_the_prose_around_them() {
            let spec = CompletionSpec::from_help(GIT);

            // The bug: `git a<Tab>` completed filenames, because git captions
            // its table with a sentence rather than `Commands:`.
            assert_eq!(spec.matching("a")[0], "add");
            let commands = spec.matching("");
            for command in ["clone", "init", "restore", "switch", "rebase", "tag"] {
                assert!(commands.contains(&command.to_owned()), "missing {command}");
            }
            // Group captions and the closing paragraph are not commands.
            for prose in ["start", "work", "examine", "grow", "collaborate", "concept"] {
                assert!(
                    !commands.contains(&prose.to_owned()),
                    "{prose} is not a command"
                );
            }
            // Flags come off the usage line with their brackets removed.
            // Ranking is fuzzy, so assert the best match: `--git` is a
            // subsequence of `--paginate` too.
            assert_eq!(spec.matching("--git")[0], "--git-dir");
            assert_eq!(spec.matching("--vers")[0], "--version");
            // `[-C <path>]` is `-C`'s value, not git's operand: reading it as
            // one is what put the current directory's files in front of the
            // subcommands.
            assert_eq!(spec.positional_hint(), None);
            assert_eq!(spec.value_hint("--git-dir"), Some(&ValueHint::File));
        }

        #[test]
        fn cargo_offers_aliases_and_values_described_on_the_next_line() {
            let spec = CompletionSpec::from_help(CARGO);

            assert_eq!(spec.matching("bui")[0], "build");
            let commands = spec.matching("");
            // `build, b` names two ways to run the same subcommand, and both
            // are things you can type.
            for command in ["build", "b", "check", "c", "clean", "publish"] {
                assert!(commands.contains(&command.to_owned()), "missing {command}");
            }
            // `...  See all commands with --list` ends the table without
            // naming a command.
            assert!(!commands.contains(&"...".to_owned()));
            // Cargo puts the description under the option, so the value list
            // arrives on a line that names no option.
            assert_eq!(
                spec.value_hint("--color"),
                Some(&ValueHint::Enum(vec![
                    "auto".into(),
                    "always".into(),
                    "never".into()
                ]))
            );
            assert_eq!(spec.value_hint("-C"), Some(&ValueHint::Directory));
            assert_eq!(spec.value_hint("--config"), Some(&ValueHint::File));
            assert_eq!(spec.positional_hint(), None);
        }

        #[test]
        fn docker_offers_every_one_of_its_command_tables() {
            let spec = CompletionSpec::from_help(DOCKER);
            let commands = spec.matching("");

            // Four tables — Common, Management, Swarm, and plain `Commands:` —
            // each of which has to open a section of its own.
            for command in ["run", "network", "swarm", "attach"] {
                assert!(commands.contains(&command.to_owned()), "missing {command}");
            }
            // `buildx*` is starred as a plugin; the star is a footnote mark.
            assert!(commands.contains(&"buildx".to_owned()));
            assert!(!commands.iter().any(|command| command.contains('*')));
            // "Run 'docker COMMAND --help' …" closes the output at column 0.
            assert!(!commands.contains(&"Run".to_owned()));
            assert_eq!(spec.matching("--tlsc"), ["--tlscacert", "--tlscert"]);
        }

        #[test]
        fn rustup_offers_neither_wrapped_descriptions_nor_worked_examples() {
            // Captured at 50 columns on purpose: rustup wraps its descriptions
            // there, and it heads its worked examples "Common commands:" — a
            // section whose lines are indented and mention commands without
            // being a table.
            let spec = CompletionSpec::from_help(RUSTUP);
            let commands = spec.matching("");

            for command in ["install", "toolchain", "completions", "self", "which"] {
                assert!(commands.contains(&command.to_owned()), "missing {command}");
            }
            // A flag quoted in prose (``rustup doc --book``) keeps neither its
            // backticks nor the rest of the sentence's punctuation.
            assert_eq!(spec.matching("--b")[0], "--book");
            // `completions  Generate tab-completion scripts for / your shell`
            // continues at a deeper column, and "Update Rust toolchains and
            // rustup" is a sentence, not `name  description`.
            for prose in [
                "your",
                "Update",
                "Install",
                "toolchains",
                "or",
                "active",
                "command",
                "the",
            ] {
                assert!(
                    !commands.contains(&prose.to_owned()),
                    "{prose} is not a command"
                );
            }
        }

        #[test]
        fn ls_takes_files_and_has_no_subcommands() {
            let spec = CompletionSpec::from_help(LS);

            assert_eq!(spec.positional_hint(), Some(&ValueHint::File));
            assert_eq!(spec.matching("--col")[0], "--color");
            // Paragraphs of prose about SIZE, TIME_STYLE and exit status must
            // not turn into subcommands — with no `Commands:` heading anywhere,
            // nothing but options is on offer.
            assert!(spec.matching("a").is_empty());
            assert!(spec.matching("").iter().all(|value| value.starts_with('-')));
        }

        #[test]
        fn ls_offers_no_options_its_descriptions_merely_mention() {
            let spec = CompletionSpec::from_help(LS);
            let options = spec.matching("-");

            // `ls` describes `-c` as "with -lt: sort by, and show, ctime" and
            // `--author` as "with -l, print the author of each file". Reading the
            // description as more declarations offered `-lt`, which `ls` has no
            // such option for.
            assert!(
                !options.contains(&"-lt".to_owned()),
                "-lt is prose, not a declared option"
            );
            // The flags those sentences point at are still on offer, because
            // `ls` declares them on their own entries.
            for declared in ["-c", "-l", "--author"] {
                assert!(options.contains(&declared.to_owned()), "missing {declared}");
            }
        }
    }

    #[test]
    fn names_pages_by_section_and_compression_suffix() {
        let root = fresh_temp_dir("mesh-man");
        for (section, files) in [
            ("man1", ["ls.1.gz", "git-add.1", "notes.txt"].as_slice()),
            ("man3", ["CPAN::Meta.3pm.gz"].as_slice()),
            // Not a section directory, so its pages are not this tree's.
            ("notman", ["hidden.1"].as_slice()),
        ] {
            fs::create_dir_all(root.join(section)).unwrap();
            for file in files {
                fs::write(root.join(section).join(file), "").unwrap();
            }
        }

        let mut pages = pages_in(&root);
        pages.sort();
        assert_eq!(pages, ["CPAN::Meta", "git-add", "ls"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fuzzy_ranking_prefers_exact_then_exact_case_then_folded_case() {
        assert_eq!(
            rank_candidates(
                vec!["Foo".into(), "football".into(), "foo".into(), "FOOD".into()],
                "foo"
            ),
            ["foo", "football", "Foo", "FOOD"]
        );
        assert_eq!(
            rank_candidates(vec!["Pictures".into(), "pictures".into()], "pic"),
            ["pictures", "Pictures"]
        );
    }

    #[test]
    fn uppercase_fuzzy_queries_are_case_sensitive() {
        assert_eq!(
            rank_candidates(vec!["USER".into(), "User".into(), "user".into()], "USER"),
            ["USER"]
        );
        assert_eq!(
            rank_candidates(
                vec!["checkout".into(), "cherry-pick".into(), "check".into()],
                "CP"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parses_multiline_possible_value_sections() {
        let spec = CompletionSpec::from_help(
            "Options:\n  --color=WHEN\n      Controls when colors are used.\n\n      Color output supports several modes.\n\n      The possible values for this flag are:\n\n      never: Colors will never be used.\n      auto: Colors are used when writing to a terminal.\n      always: Colors will always be used.\n      ansi: ANSI color escapes are always used.\n\n  --type=TYPE\n      Select a file type.\n      The possible values for this option are:\n\n      rust: Rust source files.\n      python: Python source files.\n",
        );

        assert_eq!(
            spec.value_hint("--color"),
            Some(&ValueHint::Enum(vec![
                "never".into(),
                "auto".into(),
                "always".into(),
                "ansi".into()
            ]))
        );
        assert_eq!(
            spec.value_hint("--type"),
            Some(&ValueHint::Enum(vec!["rust".into(), "python".into()]))
        );
    }

    #[test]
    fn does_not_treat_profile_metavars_as_files() {
        let spec = CompletionSpec::from_help(
            "Options:\n  --profile <PROFILE-NAME>  Build artifacts with the specified profile\n",
        );

        assert_eq!(spec.value_hint("--profile"), None);
    }

    #[test]
    fn parses_wrapped_inline_possible_values() {
        let spec = CompletionSpec::from_help(
            "Options:\n  --message-format <FMT>  Error format [possible values: human, short, json,\n      json-diagnostic-short, json-diagnostic-rendered-ansi,\n      json-render-diagnostics]\n",
        );

        assert_eq!(
            spec.value_hint("--message-format"),
            Some(&ValueHint::Enum(vec![
                "human".into(),
                "short".into(),
                "json".into(),
                "json-diagnostic-short".into(),
                "json-diagnostic-rendered-ansi".into(),
                "json-render-diagnostics".into()
            ]))
        );
    }

    #[test]
    fn mixed_file_and_directory_unions_prefer_files() {
        let spec = CompletionSpec::from_help(
            "Options:\n  --path <FILE|DIR>  file or directory\n  --dir <DIR>  directory only\n",
        );

        // `File` completion offers directories too, so a mixed union stays usable
        // for either kind; a directory-only metavar keeps `Directory`.
        assert_eq!(spec.value_hint("--path"), Some(&ValueHint::File));
        assert_eq!(spec.value_hint("--dir"), Some(&ValueHint::Directory));
    }

    #[test]
    fn compound_directory_metavars_are_recognized() {
        let spec = CompletionSpec::from_help("Options:\n  --out <OUTPUT_DIR>  output directory\n");

        assert_eq!(spec.value_hint("--out"), Some(&ValueHint::Directory));
    }

    #[test]
    fn unbracketed_possible_values_do_not_swallow_later_options() {
        let spec = CompletionSpec::from_help(
            "Options:\n  --format FORMAT  possible values: json, text\n  --output <FILE>  destination\n",
        );

        assert_eq!(
            spec.value_hint("--format"),
            Some(&ValueHint::Enum(vec!["json".into(), "text".into()]))
        );
        // The unbracketed list is confined to its line: the following option
        // stays a candidate instead of being consumed into the enum.
        assert_eq!(spec.matching("--o")[0], "--output");
        assert_eq!(spec.value_hint("--output"), Some(&ValueHint::File));
    }

    #[test]
    fn rejects_specs_generated_with_older_parser_semantics() {
        let encoded = "mesh-completion-v2\ntest\ncandidate\t--profile\nvalue\t--profile\tfile\n";

        assert_eq!(CompletionSpec::decode(encoded, "test"), None);
    }

    #[test]
    fn memory_and_disk_cache_avoid_repeated_probes() {
        retrying(|| {
            let root = fresh_temp_dir("mesh-cache");
            let command = root.join("helper");
            let count = root.join("count");
            let cache_dir = root.join("cache");
            helper(
                &command,
                &format!("echo x >> '{}'\necho '  --cached  cached'", count.display()),
            );
            let words = vec![command.to_string_lossy().into_owned()];

            let cache = CompletionCache::new(Some(cache_dir.clone()));
            // Only the first `spec_for` probes; the rest are served from the memory
            // and disk caches. An empty first probe is a transient spawn hiccup —
            // discard the attempt and retry from clean state.
            let first = cache.spec_for(&words).matching("--");
            if first.is_empty() {
                let _ = fs::remove_dir_all(&root);
                return Err("first probe produced no completions".into());
            }
            assert_eq!(first, ["--cached"]);
            assert_eq!(cache.spec_for(&words).matching("--"), ["--cached"]);
            assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);

            let fresh = CompletionCache::new(Some(cache_dir));
            assert_eq!(fresh.spec_for(&words).matching("--"), ["--cached"]);
            assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);
            fs::remove_dir_all(root).unwrap();
            Ok(())
        });
    }

    #[test]
    fn modification_time_invalidates_cache() {
        retrying(|| {
            let root = fresh_temp_dir("mesh-invalidate");
            let command = root.join("helper");
            helper(&command, "echo '  --first  first'");
            let words = vec![command.to_string_lossy().into_owned()];
            let cache = CompletionCache::new(Some(root.join("cache")));
            let first = cache.spec_for(&words).matching("--");
            if first.is_empty() {
                let _ = fs::remove_dir_all(&root);
                return Err("first probe produced no completions".into());
            }
            assert_eq!(first, ["--first"]);

            thread::sleep(Duration::from_millis(10));
            helper(&command, "echo '  --second  second'");
            // A newer mtime invalidates the cache, so this re-probes. An empty
            // result is a transient spawn hiccup; a stale `--first` would be a real
            // invalidation bug and still fails the assertion below.
            let second = cache.spec_for(&words).matching("--");
            if second.is_empty() {
                let _ = fs::remove_dir_all(&root);
                return Err("second probe produced no completions".into());
            }
            assert_eq!(second, ["--second"]);
            fs::remove_dir_all(root).unwrap();
            Ok(())
        });
    }

    #[test]
    fn corrupt_disk_cache_is_replaced_by_a_fresh_probe() {
        retrying(|| {
            let root = fresh_temp_dir("mesh-corrupt");
            let command = root.join("helper");
            let count = root.join("count");
            let cache_dir = root.join("cache");
            helper(
                &command,
                &format!("echo x >> '{}'\necho '  --fresh  fresh'", count.display()),
            );
            let words = vec![command.to_string_lossy().into_owned()];
            CompletionCache::new(Some(cache_dir.clone())).spec_for(&words);
            // Name the entry the way `spec_for` does — through `resolve_command`,
            // which canonicalizes. Hashing the raw path instead corrupts a file
            // nothing reads whenever the temporary directory is reached through a
            // symlink (macOS `/var` → `/private/var`), and the second probe then
            // reads its own valid cache rather than re-probing.
            let resolved = resolve_command(&command.to_string_lossy())
                .expect("helper must resolve the way spec_for resolves it");
            let entry = cache_dir.join(cache_name(&resolved, &[]));
            fs::write(entry, "not a completion cache").unwrap();

            let spec = CompletionCache::new(Some(cache_dir)).spec_for(&words);
            let matches = spec.matching("--");
            let probes = fs::read_to_string(&count)
                .map(|text| text.lines().count())
                .unwrap_or(0);
            // Both probes must have run: the initial population and the fresh
            // re-probe after the corrupt entry. An empty result or a shortfall is a
            // transient spawn hiccup — discard and retry.
            if matches.is_empty() || probes < 2 {
                let _ = fs::remove_dir_all(&root);
                return Err(format!(
                    "transient probe hiccup: matches={matches:?} probes={probes}"
                ));
            }
            assert_eq!(matches, ["--fresh"]);
            assert_eq!(probes, 2);
            fs::remove_dir_all(root).unwrap();
            Ok(())
        });
    }

    #[test]
    fn captures_stdout_and_stderr_and_passes_subcommands() {
        let root = fresh_temp_dir("mesh-help");
        let command = root.join("helper");
        let args = root.join("args");
        helper(
            &command,
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\necho --stdout\necho --stderr >&2",
                args.display()
            ),
        );
        let output = command_help(&[command.to_string_lossy().into_owned(), "subcommand".into()]);
        assert!(output.contains("--stdout"));
        assert!(output.contains("--stderr"));
        assert_eq!(fs::read_to_string(args).unwrap(), "subcommand\n--help\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn times_out_nonterminating_help() {
        let root = fresh_temp_dir("mesh-timeout");
        let command = root.join("helper");
        helper(&command, "sleep 10");
        let started = Instant::now();
        assert!(command_help(&[command.to_string_lossy().into_owned()]).is_empty());
        assert!(started.elapsed() < Duration::from_secs(4));
        fs::remove_dir_all(root).unwrap();
    }
}
