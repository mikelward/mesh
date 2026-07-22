//! Completion specifications generated from command help output.

use std::collections::HashMap;
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

const CACHE_VERSION: &str = "mesh-completion-v1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompletionSpec {
    candidates: Vec<String>,
}

impl CompletionSpec {
    pub(crate) fn from_help(help: &str) -> Self {
        let mut candidates = Vec::new();
        let mut commands = false;
        for line in help.lines() {
            let trimmed = line.trim();
            let heading = trimmed.trim_end_matches(':').to_ascii_lowercase();
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
            for token in trimmed.split_whitespace() {
                let option = token.trim_end_matches([',', ';']);
                if option.starts_with('-') && option.len() > 1 {
                    let option = option.split(['=', '[', '<']).next().unwrap_or(option);
                    candidates.push(option.to_owned());
                }
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
        candidates.sort();
        candidates.dedup();
        Self { candidates }
    }

    pub(crate) fn matching(&self, prefix: &str) -> Vec<String> {
        self.candidates
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn encode(&self, fingerprint: &str) -> String {
        let mut encoded = format!("{CACHE_VERSION}\n{fingerprint}\n");
        for candidate in &self.candidates {
            encoded.push_str(candidate);
            encoded.push('\n');
        }
        encoded
    }

    fn decode(encoded: &str, fingerprint: &str) -> Option<Self> {
        let mut lines = encoded.lines();
        (lines.next()? == CACHE_VERSION && lines.next()? == fingerprint).then(|| Self {
            candidates: lines
                .filter(|line| !line.is_empty() && !line.contains(['\r', '\n']))
                .map(str::to_owned)
                .collect(),
        })
    }
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
    use super::{CompletionCache, CompletionSpec, command_help};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    fn helper(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn parses_typed_specs() {
        let spec = CompletionSpec::from_help(
            "Commands:\n  build  compile\n\nOptions:\n  -h, --help  help\n",
        );
        assert_eq!(spec.matching("--"), ["--help"]);
        assert_eq!(spec.matching("b"), ["build"]);
    }

    #[test]
    fn memory_and_disk_cache_avoid_repeated_probes() {
        let root = std::env::temp_dir().join(format!("mesh-cache-{}", std::process::id()));
        let command = root.join("helper");
        let count = root.join("count");
        let cache_dir = root.join("cache");
        fs::create_dir_all(&root).unwrap();
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
        let root = std::env::temp_dir().join(format!("mesh-invalidate-{}", std::process::id()));
        let command = root.join("helper");
        fs::create_dir_all(&root).unwrap();
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
        let root = std::env::temp_dir().join(format!("mesh-corrupt-{}", std::process::id()));
        let command = root.join("helper");
        let count = root.join("count");
        let cache_dir = root.join("cache");
        fs::create_dir_all(&root).unwrap();
        helper(
            &command,
            &format!("echo x >> '{}'\necho '  --fresh  fresh'", count.display()),
        );
        let words = vec![command.to_string_lossy().into_owned()];
        CompletionCache::new(Some(cache_dir.clone())).spec_for(&words);
        let entry = fs::read_dir(&cache_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(entry, "not a completion cache").unwrap();

        let spec = CompletionCache::new(Some(cache_dir)).spec_for(&words);
        assert_eq!(spec.matching("--"), ["--fresh"]);
        assert_eq!(fs::read_to_string(count).unwrap().lines().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn captures_stdout_and_stderr_and_passes_subcommands() {
        let root = std::env::temp_dir().join(format!("mesh-help-{}", std::process::id()));
        let command = root.join("helper");
        let args = root.join("args");
        fs::create_dir_all(&root).unwrap();
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
        let root = std::env::temp_dir().join(format!("mesh-timeout-{}", std::process::id()));
        let command = root.join("helper");
        fs::create_dir_all(&root).unwrap();
        helper(&command, "sleep 10");
        let started = Instant::now();
        assert!(command_help(&[command.to_string_lossy().into_owned()]).is_empty());
        assert!(started.elapsed() < Duration::from_secs(4));
        fs::remove_dir_all(root).unwrap();
    }
}
