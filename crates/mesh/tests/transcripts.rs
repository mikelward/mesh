//! Every documentation transcript is **run**, and mesh has to say exactly what
//! the transcript shows it saying.
//!
//! The sibling sweep in `docs.rs` parses examples with `mesh -n` and stops
//! there, which catches an example that stopped being valid mesh but not one
//! that stopped being *true*. Three of those were sitting in the docs at once:
//! a `for` over an unsplit capture (refused since captures became one string),
//! a `check` function built on bash's `return 1` (mesh's `return` carries a
//! value, so the function read as true whatever it was given), and
//! `:match(/re/)`, which is not implemented. Each parses. None works.
//!
//! **What is compared is the diagnostics, not the whole output.** A transcript
//! is written for a reader, so its output lines name the author's home
//! directory, the programs on their `PATH`, a pid, and files that were lying
//! around — none of which a test host has. What *is* reproducible is whether
//! mesh objects: every line the shell writes beginning `mesh:` must be a line
//! the transcript shows, and every such line the transcript shows must appear.
//! An example that quietly stopped working says `mesh: …` where the doc claims
//! a result, and one that documents a diagnostic keeps having to produce it.
//!
//! **A block runs in the session its own file has built.** The docs are written
//! to be typed along with, so a block freely uses a variable or function an
//! earlier block defined. Each block therefore runs with every earlier *run*
//! block of the same file replayed ahead of it, in a fresh directory, which is
//! what a reader following along would have.
//!
//! **A prompt is what makes a block a transcript**, in either markup. The tour
//! writes them as `<pre>` so it can bold what you type; the reference writes
//! some as ```` ```text ```` fences, where the block is a diagnostic and there
//! is nothing to bold. Both are swept — a fence full of `mesh:` lines is
//! exactly the kind of block that drifts unnoticed, and one of them was wrong
//! (a `whence` diagnostic under a `type` command) when this started reading
//! them.
//!
//! **A block that cannot run says so in the file**, as an HTML comment directly
//! above it:
//!
//! ```text
//! <!-- no-run: waits on a background job -->
//! ```
//!
//! That is deliberately visible in the prose rather than hidden in a list here,
//! so the reason travels with the block it excuses, and
//! `the_skip_list_stays_small` keeps the habit from spreading. The six today
//! are one of three kinds: the block never finishes on its own (a `wait` on a
//! 30-second sleep, a `loop` waiting for a deploy), it says something only this
//! host can say (`type rg` reports wherever `rg` is installed), or it needs a
//! terminal — history expansion and a job stopped with Ctrl-Z are interactive
//! by definition, and a script sees `!$` as text.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Transcript {
    line: usize,
    typed: Vec<String>,
    diagnostics: Vec<String>,
    skipped: Option<String>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/mesh")
        .to_path_buf()
}

/// The same set `docs.rs` sweeps, and for the same reason: `DESIGN.md` argues
/// the language mesh is aiming at, so it is neither runnable nor meant to be.
fn documentation_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![root.join("README.md")];
    let mut from_docs: Vec<PathBuf> = fs::read_dir(root.join("docs"))
        .expect("read docs/")
        .map(|entry| entry.expect("docs/ entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .filter(|path| path.file_name().is_some_and(|name| name != "DESIGN.md"))
        .collect();
    from_docs.sort();
    files.extend(from_docs);
    files
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn strip_emphasis(text: &str) -> String {
    text.replace("<strong>", "").replace("</strong>", "")
}

/// A transcript's markup, which decides how a line is read.
///
/// A `<pre>` block is HTML: its examples are bolded and its `<`, `&` and quotes
/// are entities. A fenced block is literal — the text between the fences is
/// exactly what was typed and printed — so running it through the same
/// unescaping would rewrite an `&amp;` a doc means as itself.
#[derive(Clone, Copy, PartialEq)]
enum Markup {
    Html,
    Fenced,
}

/// The typed part of a transcript line: `mesh$ ` opens a unit and `...`
/// continues the one above it. A line with neither prefix is output.
fn typed_line(line: &str, markup: Markup) -> Option<String> {
    let bare = plain(line, markup);
    if let Some(rest) = bare.strip_prefix("mesh$ ") {
        return Some(rest.to_string());
    }
    let rest = bare.strip_prefix("...")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest).to_string())
}

fn plain(line: &str, markup: Markup) -> String {
    match markup {
        Markup::Html => unescape(&strip_emphasis(line)),
        Markup::Fenced => line.to_string(),
    }
}

/// The reason on a `<!-- no-run: … -->` comment, if the block carries one.
fn skip_reason(line: &str) -> Option<String> {
    let inner = line.trim().strip_prefix("<!--")?.strip_suffix("-->")?;
    let reason = inner.trim().strip_prefix("no-run:")?;
    Some(reason.trim().to_string())
}

/// Where a block opens, and in which markup — or `None` if this line opens no
/// block at all.
fn block_opener(line: &str) -> Option<Markup> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("<pre>") {
        return Some(Markup::Html);
    }
    // A fence's label says what it holds, and a transcript is labeled `text`
    // for the parse sweep's benefit — it is output, not source. The `mesh$`
    // test below is what actually decides, so an unlabeled fence full of
    // prompts counts too.
    trimmed.starts_with("```").then_some(Markup::Fenced)
}

fn block_closer(line: &str, markup: Markup) -> bool {
    let trimmed = line.trim_start();
    match markup {
        Markup::Html => trimmed.starts_with("</pre>"),
        Markup::Fenced => trimmed.starts_with("```"),
    }
}

fn transcripts_in(text: &str) -> Vec<Transcript> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let Some(markup) = block_opener(lines[index]) else {
            index += 1;
            continue;
        };

        let start = index + 2;
        // The marker sits directly above the block, past any blank line.
        let skipped = lines[..index]
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| skip_reason(line));

        let mut body = Vec::new();
        index += 1;
        while index < lines.len() && !block_closer(lines[index], markup) {
            body.push(lines[index]);
            index += 1;
        }
        index += 1;

        // A block is a transcript when it shows a prompt. Everything else — a
        // fenced snippet of config, a `<pre>` setting bash against mesh — has
        // no session of its own and belongs to the parse sweep instead.
        if !body
            .iter()
            .any(|line| plain(line, markup).starts_with("mesh$ "))
        {
            continue;
        }

        let mut typed = Vec::new();
        let mut diagnostics = Vec::new();
        for line in &body {
            match typed_line(line, markup) {
                Some(source) => typed.push(source),
                None => {
                    let output = plain(line, markup);
                    if output.starts_with("mesh:") {
                        diagnostics.push(output);
                    }
                }
            }
        }

        // `<Tab>` stands for a keypress, so the block is describing completion
        // and was never source. The parse sweep skips it for the same reason,
        // and it needs no marker in the prose: nothing about it is runnable.
        if typed.iter().any(|line| line.contains("<Tab>")) {
            continue;
        }

        found.push(Transcript {
            line: start,
            typed,
            diagnostics,
            skipped,
        });
    }
    found
}

/// The system temp directory, resolved once and absolute.
///
/// Absolute matters twice over. This process joins its scratch directories onto
/// it, and it is handed to the child as `TMPDIR` — where a relative one would
/// be read from the child's *own* directory, which is the scratch directory
/// rather than wherever the test was started. A heredoc is the visible casualty:
/// it needs a temp file, and `cat << END` fails with `<<: No such file or
/// directory` under `TMPDIR=relative-tmp`.
fn temp_root() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        // `temp_dir` hands back an *empty path* for `TMPDIR=`, which every step
        // below would then fail on. An empty setting means the same thing as no
        // setting, so it takes the platform default.
        let root = match std::env::temp_dir() {
            path if path.as_os_str().is_empty() => PathBuf::from("/tmp"),
            path => path,
        };
        // Created first, because `canonicalize` needs the directory to exist —
        // and a `TMPDIR` naming a relative directory that is not there yet is
        // exactly the case where falling back to the unresolved path would
        // reintroduce what this function exists to prevent.
        fs::create_dir_all(&root).expect("create the temp root");
        root.canonicalize().expect("resolve the temp root")
    })
}

/// A directory holding the handful of files the transcripts name, so a `cat` or
/// a glob has something to find. It stands in for "a directory you were working
/// in", not for any one example's fixture — what is asserted never depends on
/// the contents, only on mesh not objecting.
///
/// The path is stable across a file's blocks, and has to be: a diagnostic
/// naming the script would otherwise differ from block to block, and the
/// replayed setup would never match itself.
fn scratch_directory(tag: &str) -> PathBuf {
    let dir = temp_root().join(format!("mesh_transcripts_{}_{tag}", std::process::id()));
    remove_directory(&dir);
    fs::create_dir_all(&dir).expect("create the transcript scratch directory");
    // A relative `TMPDIR` leaves `temp_dir()` relative, and the child runs
    // *in* this directory — so a relative script path would be looked for
    // beneath itself, and `session.mesh` would be missing rather than run.
    // Canonicalizing needs the directory to exist, which is why it is here and
    // not on the line above.
    let dir = dir
        .canonicalize()
        .expect("resolve the transcript scratch directory");
    for name in ["words.txt", "notes.txt", "todo.txt"] {
        fs::write(dir.join(name), "alpha\nbeta\ngamma\n").expect("write a transcript fixture file");
    }
    for name in ["docs", "src", "tests"] {
        fs::create_dir_all(dir.join(name)).expect("create a transcript fixture directory");
    }
    // The one file whose *contents* a transcript depends on: `source lib.mesh`
    // is shown defining a `greet` that the next line calls.
    fs::write(
        dir.join("lib.mesh"),
        "func greet(who) { puts \"hi, $who\" }\n",
    )
    .expect("write the sourced transcript fixture");
    dir
}

/// Remove a directory, treating "it was not there" as the ordinary case and
/// anything else as a failure.
///
/// Swallowing this would be quiet in the worst way: `create_dir_all` succeeds
/// against a directory that is still standing, so a failed removal leaves the
/// *previous* block's files in place and every later block runs against stale
/// fixture contents, with nothing anywhere saying why.
fn remove_directory(dir: &Path) {
    if let Err(error) = fs::remove_dir_all(dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        panic!(
            "remove the transcript scratch directory {}: {error}",
            dir.display()
        );
    }
}

/// A home directory of the sweep's own, standing in for the reader's.
///
/// Two things hang off it, and both are about the session belonging to nobody
/// in particular. `$XDG_CONFIG_HOME` is `.config` beneath it and is **empty**,
/// so a developer's own `env.mesh` — which mesh reads on every invocation, not
/// just interactive ones — cannot print a diagnostic or set a variable into a
/// transcript. And `$HOME` is this, so `puts $env.HOME` has an answer even when
/// the invoking environment has none: `env -u HOME cargo test` otherwise fails
/// the sweep where mesh itself is fine.
///
/// Absolute, for the same reason the scratch directory is — the child runs
/// elsewhere, and a relative path would name a different place from there.
fn isolated_home() -> &'static Path {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = temp_root().join(format!("mesh_transcripts_{}_home", std::process::id()));
        remove_directory(&dir);
        fs::create_dir_all(dir.join(".config")).expect("create the transcript home");
        dir.canonicalize().expect("resolve the transcript home")
    })
}

fn isolated_config_home() -> PathBuf {
    isolated_home().join(".config")
}

/// The environment variable holding the scratch directory, which the anchor
/// `cd` reads.
///
/// The path goes through the **environment**, never into the session text. A
/// formatted `cd "{path}"` re-parses whatever the host's `TMPDIR` happens to
/// contain, so a `$` in it interpolates and a quote or backslash changes where
/// the string ends — a `TMPDIR` of `/tmp/mesh$tmp` fails every transcript with
/// `tmp: unbound variable`. `$env.NAME` is one value by the language's own
/// guarantee, whatever bytes are in it.
const ANCHOR: &str = "MESH_TRANSCRIPT_HOME";

/// How long one session gets. Generous — the whole sweep runs in under a
/// second — and there only to keep a block that waits on something from
/// hanging CI instead of failing it. A block that needs longer needs a marker.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Run a session and return every line mesh itself reported, or `None` if it
/// had to be killed.
fn diagnostics_of(session: &str, dir: &Path) -> Option<Vec<String>> {
    let script = dir.join("session.mesh");
    fs::write(&script, format!("{session}\n")).expect("write the session script");

    // stderr goes to a **file**, not a pipe. A transcript may leave a detached
    // descendant holding the inherited descriptor — `sleep 60 &` outlives the
    // shell that launched it — and reading a pipe waits for every writer to
    // close, so the drain would hang after the deadline had already passed and
    // let go. A file is readable the moment the child is gone, whoever else
    // still holds it open.
    let log = dir.join("session.stderr");
    let sink = fs::File::create(&log).expect("create the transcript's diagnostic log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .arg(&script)
        .current_dir(dir)
        .env("HOME", isolated_home())
        .env("XDG_CONFIG_HOME", isolated_config_home())
        .env("TMPDIR", temp_root())
        .env(ANCHOR, dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(sink))
        .spawn()
        .expect("run a documentation transcript");

    // `output()` would wait forever, and a documentation sweep that hangs is
    // worse than one that fails: nobody reads a job that never finishes.
    let started = std::time::Instant::now();
    loop {
        match child
            .try_wait()
            .expect("wait on a documentation transcript")
        {
            Some(_) => break,
            None if started.elapsed() >= DEADLINE => {
                // A kill that finds the child already gone is the ordinary
                // race, not a failure — it exited between the `try_wait` above
                // and here, and the `wait` below still reaps it. Anything else
                // means the child is loose, which is exactly what must not be
                // reported as a plain documentation timeout.
                if let Err(error) = child.kill()
                    && error.kind() != std::io::ErrorKind::InvalidInput
                {
                    panic!("kill a documentation transcript that ran past {DEADLINE:?}: {error}");
                }
                child
                    .wait()
                    .expect("reap a documentation transcript after killing it");
                return None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }

    let stderr = fs::read_to_string(&log).expect("read a transcript's diagnostics");
    Some(
        stderr
            .lines()
            .filter(|line| line.starts_with("mesh:"))
            .map(str::to_string)
            .collect(),
    )
}

struct Sweep {
    ran: usize,
    skipped: Vec<String>,
    failures: Vec<String>,
}

/// The sweep runs once for the whole file. Every test below reads the same
/// result, so the shell is not invoked three times over, and the three cannot
/// race each other for the scratch directory.
fn sweep() -> &'static Sweep {
    static SWEEP: std::sync::OnceLock<Sweep> = std::sync::OnceLock::new();
    SWEEP.get_or_init(run_sweep)
}

fn run_sweep() -> Sweep {
    let root = repo_root();
    let mut ran = 0usize;
    let mut skipped = Vec::new();
    let mut failures = Vec::new();

    for file in documentation_files(&root) {
        let text = fs::read_to_string(&file).expect("read a documentation file");
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string();

        // The session grows a block at a time, so the run that checks block N
        // also produces the setup diagnostics block N+1 has to subtract. That
        // makes it one shell invocation per block rather than two.
        let mut session: Vec<String> = Vec::new();
        let mut setup_diagnostics: Vec<String> = Vec::new();

        for transcript in transcripts_in(&text) {
            if transcript.typed.is_empty() {
                continue;
            }
            if let Some(reason) = transcript.skipped {
                skipped.push(format!("{shown}:{} — {reason}", transcript.line));
                continue;
            }

            let dir = scratch_directory(&shown.replace('/', "_"));
            // Every block starts where a reader would start it. Blocks
            // demonstrate `cd`, so without this each one would begin wherever
            // the one before it wandered to, and a `source lib.mesh` three
            // sections later would look for the file in the wrong directory.
            // A `cd` reports nothing, so anchoring cannot change what is
            // compared — and it has to go in the session itself, not just in
            // front of the block being checked, or the replayed setup drifts
            // even when the block under test does not.
            session.push(format!("cd $env.{ANCHOR}"));
            session.extend(transcript.typed.iter().cloned());
            let produced = diagnostics_of(&session.join("\n"), &dir);
            remove_directory(&dir);
            ran += 1;

            let Some(produced) = produced else {
                failures.push(format!(
                    "{shown}:{}\n  the session did not finish within {DEADLINE:?}; if this block \
                     waits on something, mark it `<!-- no-run: … -->`\n  typed:\n    {}",
                    transcript.line,
                    transcript.typed.join("\n    ")
                ));
                // The session is left as it was: a block that never returned
                // cannot be replayed as setup for the ones after it. Its
                // anchor goes back too, which is the `+ 1`.
                for _ in 0..transcript.typed.len() + 1 {
                    session.pop();
                }
                continue;
            };

            // The setup is replayed verbatim, so its diagnostics come back
            // verbatim and in front. If they ever do not, the session is not
            // deterministic and subtracting a count would hide it.
            let new = if produced.starts_with(setup_diagnostics.as_slice()) {
                produced[setup_diagnostics.len()..].to_vec()
            } else {
                failures.push(format!(
                    "{shown}:{}\n  the replayed session changed what it reported:\n    before: {:?}\n    now:    {:?}",
                    transcript.line, setup_diagnostics, produced
                ));
                setup_diagnostics = produced;
                continue;
            };

            if new != transcript.diagnostics {
                failures.push(format!(
                    "{shown}:{}\n  documented: {:?}\n  produced:   {:?}\n  typed:\n    {}",
                    transcript.line,
                    transcript.diagnostics,
                    new,
                    transcript.typed.join("\n    ")
                ));
            }
            setup_diagnostics = produced;
        }
    }

    // The fixture home outlives every session rather than one of them, so it is
    // taken down here, once they have all finished. A scratch directory is
    // removed as its block ends; this one would otherwise be left behind by
    // *successful* runs, one per test process, accumulating in the temp root.
    remove_directory(isolated_home());

    Sweep {
        ran,
        skipped,
        failures,
    }
}

#[test]
fn every_transcript_says_what_the_docs_say_it_says() {
    let sweep = sweep();

    assert!(
        sweep.failures.is_empty(),
        "{} of {} documentation transcripts do not run as written:\n\n{}",
        sweep.failures.len(),
        sweep.ran,
        sweep.failures.join("\n\n")
    );
}

/// A sweep that silently stops finding transcripts passes while checking
/// nothing, which is the way a test like this rots. The floor is well under
/// the count today and only has to catch the collapse.
#[test]
fn the_sweep_finds_the_transcripts() {
    let sweep = sweep();
    assert!(
        sweep.ran > 50,
        "only {} transcripts ran — the extractor has probably stopped matching the docs' markup",
        sweep.ran
    );
}

/// `no-run` is an escape hatch, and an escape hatch nobody counts becomes the
/// default. Every skip is listed on failure so the reasons can be read at once
/// rather than hunted for.
#[test]
fn the_skip_list_stays_small() {
    let sweep = sweep();
    assert!(
        sweep.skipped.len() <= 12,
        "{} transcripts are marked no-run; the sweep is supposed to run them:\n  {}",
        sweep.skipped.len(),
        sweep.skipped.join("\n  ")
    );
}
