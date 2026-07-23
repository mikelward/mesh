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

const CACHE_VERSION: &str = "mesh-completion-v3";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ValueHint {
    File,
    Directory,
    Enum(Vec<String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompletionSpec {
    candidates: Vec<String>,
    values: HashMap<String, ValueHint>,
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
        let mut commands = false;
        let mut current_options = Vec::new();
        let mut pending_values = None;
        for line in help.lines() {
            let trimmed = line.trim();
            let tokens: Vec<_> = trimmed.split_whitespace().collect();
            let options = option_names(&tokens);
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
            let heading = trimmed.trim_end_matches(':').to_ascii_lowercase();
            if line == trimmed && trimmed.ends_with(':') {
                current_options.clear();
            }
            if matches!(
                heading.as_str(),
                "commands" | "subcommands" | "available commands"
            ) {
                commands = true;
                continue;
            }
            if trimmed.ends_with(':') || trimmed.is_empty() {
                commands = false;
            }
            candidates.extend(options.iter().cloned());
            if !options.is_empty() {
                current_options.clone_from(&options);
            }
            if let Some((enum_values, complete)) = inline_possible_values(trimmed) {
                if complete {
                    for option in &options {
                        values.insert(option.clone(), ValueHint::Enum(enum_values.clone()));
                    }
                } else if !options.is_empty() {
                    pending_values = Some(PendingValues::Wrapped {
                        options: options.clone(),
                        values: enum_values,
                    });
                }
            } else if let Some(hint) = value_hint(&tokens) {
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
            if commands
                && let Some(command) = trimmed.split_whitespace().next()
                && command
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                candidates.push(command.to_owned());
            }
        }
        finish_pending_values(&mut values, pending_values);
        candidates.sort();
        candidates.dedup();
        Self { candidates, values }
    }

    pub(crate) fn matching(&self, prefix: &str) -> Vec<String> {
        let candidates = self
            .candidates
            .iter()
            .filter(|candidate| {
                prefix.is_empty() || prefix.starts_with('-') || !candidate.starts_with('-')
            })
            .cloned()
            .collect();
        rank_candidates(candidates, prefix)
    }

    pub(crate) fn value_hint(&self, option: &str) -> Option<&ValueHint> {
        self.values.get(option)
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
            match hint {
                ValueHint::File => encoded.push_str("\tfile\n"),
                ValueHint::Directory => encoded.push_str("\tdirectory\n"),
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
                    let hint = match fields.next()? {
                        "file" => ValueHint::File,
                        "directory" => ValueHint::Directory,
                        "enum" => ValueHint::Enum(fields.map(str::to_owned).collect()),
                        _ => return None,
                    };
                    spec.values.insert(option, hint);
                }
                _ => return None,
            }
        }
        Some(spec)
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

fn option_names(tokens: &[&str]) -> Vec<String> {
    tokens
        .iter()
        .filter_map(|token| {
            let option = token.trim_end_matches([',', ';']);
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
    memory: Mutex<HashMap<String, CompletionSpec>>,
}

impl Default for CompletionCache {
    fn default() -> Self {
        Self::new(cache_directory())
    }
}

impl CompletionCache {
    pub(crate) fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            memory: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn spec_for(&self, words: &[String]) -> CompletionSpec {
        let Some((command, args)) = words.split_first() else {
            return CompletionSpec::default();
        };
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
        CompletionCache, CompletionSpec, ValueHint, cache_name, command_help, rank_candidates,
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
        assert_eq!(cache.spec_for(&words).matching("--"), ["--cached"]);
        assert_eq!(cache.spec_for(&words).matching("--"), ["--cached"]);
        assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);

        let fresh = CompletionCache::new(Some(cache_dir));
        assert_eq!(fresh.spec_for(&words).matching("--"), ["--cached"]);
        assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn modification_time_invalidates_cache() {
        let root = fresh_temp_dir("mesh-invalidate");
        let command = root.join("helper");
        helper(&command, "echo '  --first  first'");
        let words = vec![command.to_string_lossy().into_owned()];
        let cache = CompletionCache::new(Some(root.join("cache")));
        assert_eq!(cache.spec_for(&words).matching("--"), ["--first"]);

        thread::sleep(Duration::from_millis(10));
        helper(&command, "echo '  --second  second'");
        assert_eq!(cache.spec_for(&words).matching("--"), ["--second"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_disk_cache_is_replaced_by_a_fresh_probe() {
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
        let command = fs::canonicalize(command).unwrap();
        let entry = cache_dir.join(cache_name(&command, &[]));
        fs::write(entry, "not a completion cache").unwrap();

        let spec = CompletionCache::new(Some(cache_dir)).spec_for(&words);
        assert_eq!(spec.matching("--"), ["--fresh"]);
        assert_eq!(fs::read_to_string(count).unwrap().lines().count(), 2);
        fs::remove_dir_all(root).unwrap();
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
