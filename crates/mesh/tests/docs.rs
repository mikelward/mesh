//! Every mesh example in the documentation is parsed with `mesh -n`.
//!
//! The docs drift the way any prose does: a construct is renamed, a limitation
//! is lifted and the workaround that stood in for it is left behind. Prose
//! cannot be checked, but the *examples* can — `-n` parses input and runs
//! nothing, so an example that stopped being valid mesh fails here instead of
//! in a reader's terminal.
//!
//! This checks **syntax only**, and has a sibling that goes further:
//! `transcripts.rs` *runs* every `<pre>` transcript and compares the
//! diagnostics mesh produces against the ones the doc shows. The two divide
//! along what each can cover — every example here parses, including the fenced
//! snippets that are fragments of a config and have no session to run in, while
//! only a prompted transcript says what its own output should be.
//!
//! **A fenced block is checked unless it says it is not mesh.** Unlabeled
//! counts as mesh, so the default is checked and a block that holds something
//! else — output, a transcript, a deliberate diagnostic — carries an honest
//! label (`text`, `sh`, `rust`) and is skipped by it. Opting *in* with
//! ```` ```mesh ```` was the earlier rule and it failed open: a block nobody
//! labeled was silently unchecked, and a missing example looks exactly like an
//! absent one. Three separate shapes went unswept that way before this
//! inverted.
//!
//! Three example spellings are recognized, matching what the docs use:
//!
//!   - fenced blocks, whole, per the rule above;
//!   - `<pre>` transcripts, where `mesh$ ` opens a unit and the `...` lines
//!     under it continue the same one, so a multi-line `if` is checked as the
//!     one construct it is rather than as fragments that cannot parse alone;
//!   - `<pre>` blocks with no prompt, where a `<strong>` span is the example —
//!     `INTRO.md` sets bash against mesh in one block and bolds the mesh half.
//!
//! `<strong>` is stripped wherever it falls rather than required per line,
//! because the docs bold both ways: per line, and once around a whole
//! multi-line construct.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A single example, with where it came from so a failure names the line to fix.
struct Example {
    file: PathBuf,
    line: usize,
    source: String,
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/mesh; the docs live two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/mesh")
        .to_path_buf()
}

/// The design documents are the ones the sweep skips.
///
/// They argue the language mesh is *aiming at*, so they deliberately write
/// syntax the parser does not accept yet (glob qualifiers, `fork`, heredocs) and
/// quote diagnostics as examples of what must fail. Parsing them against today's
/// parser would report the design's own open front as drift, and the only way to
/// stay green would be to stop writing down unbuilt syntax — exactly what the
/// files are for. Every other doc, including the forward-looking `INTRO.md` and
/// `PROMPT.md`, is swept.
///
/// `TYPES.md` qualifies on the same ground rather than by being adjacent to it:
/// it proposes end states for the type system, so its examples are spelled in
/// syntax no package has built (`status func` / `int func` declarations, say).
///
/// This is a **named list, not a pattern**, so a new file under `docs/` is swept
/// by default and joining the exemption takes an edit here.
fn is_design_document(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "DESIGN.md" || name == "TYPES.md")
}

fn documentation_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![root.join("README.md")];
    let mut from_docs: Vec<PathBuf> = std::fs::read_dir(root.join("docs"))
        .expect("read docs/")
        .map(|entry| entry.expect("docs/ entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .filter(|path| !is_design_document(path))
        .collect();
    from_docs.sort();
    files.extend(from_docs);
    files
}

/// The handful of entities the docs escape inside `<pre>`. Not a general HTML
/// unescape: these are the ones that appear, and an unknown entity should stay
/// visible as itself rather than be silently rewritten into something that
/// parses.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// The label on an opening fence, or `None` if this is not one.
fn fence_label(line: &str) -> Option<&str> {
    Some(line.trim_start().strip_prefix("```")?.trim())
}

/// Whether a fence holds mesh to check.
///
/// **Unlabeled counts as mesh**, deliberately. Opting *in* by writing ```` ```mesh ````
/// means a block someone forgets to label is silently unchecked, and there is
/// nothing to notice — a missing example looks exactly like an absent one. This
/// way the default is checked and a block that is *not* mesh has to say so, so
/// forgetting is loud (CI fails to parse it) instead of quiet.
///
/// Everything with an honest label of its own — `text` for output, transcripts
/// and deliberate diagnostics, `sh`, `rust` — is skipped by that label.
fn is_mesh_fence(label: &str) -> bool {
    label.is_empty() || label == "mesh"
}

/// Drop `<strong>` markup wherever it sits.
///
/// The docs bold their examples in two ways: one wrapper per line, and one
/// wrapper spanning a whole multi-line construct. Both mean "this is the part
/// you type", so the tags are removed rather than required in any position — a
/// rule that reads the same either way, where matching per line silently
/// dropped every span that crossed one.
fn strip_emphasis(text: &str) -> String {
    text.replace("<strong>", "").replace("</strong>", "")
}

/// The default prompt a transcript opens each command with.
const PROMPT: &str = "mesh$ ";

/// The continuation indicator drawn under it: one dot per printable column of
/// the prompt, less its trailing space, then a space of its own — what
/// `mesh-core`'s `continuation_indicator` renders, so the dots line up under the
/// prompt and the docs show what the shell does.
const CONTINUATION: &str = ".....";

/// The typed part of a transcript line: `mesh$ ` opens a unit and `.....`
/// continues the one above it. A line with neither prefix is output.
fn transcript_line(line: &str) -> Option<(bool, String)> {
    if let Some(rest) = line.strip_prefix(PROMPT) {
        return Some((true, unescape(&strip_emphasis(rest))));
    }
    // Only the indicator and its single separating space come off, so the
    // continuation keeps the indentation it is written with and a failure shows
    // the construct as the doc has it.
    let rest = line.strip_prefix(CONTINUATION)?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    Some((false, unescape(&strip_emphasis(rest))))
}

/// Pull the `<strong>` spans out of a `<pre>` that has no prompts.
///
/// `INTRO.md` sets a bash snippet against its mesh equivalent inside one block
/// and bolds only the mesh half, so the emphasis — not the block — is what
/// marks the source. A span may start mid-line (`mesh:  <strong>…`) and may
/// cross lines, and each one is its own example.
fn emphasized_spans(body: &[&str], first_line: usize) -> Vec<(usize, String)> {
    let joined = body.join("\n");
    let mut spans = Vec::new();
    let mut rest = joined.as_str();
    let mut consumed = 0usize;

    while let Some(open) = rest.find("<strong>") {
        let after_open = open + "<strong>".len();
        let Some(close) = rest[after_open..].find("</strong>") else {
            break;
        };
        let inner = &rest[after_open..after_open + close];
        let line = first_line + joined[..consumed + open].matches('\n').count();
        spans.push((line, unescape(inner)));
        let advance = after_open + close + "</strong>".len();
        consumed += advance;
        rest = &rest[advance..];
    }
    spans
}

fn examples_in(text: &str, path: &Path) -> Vec<Example> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut index = 0;
    let at = |line: usize, source: String| Example {
        file: path.to_path_buf(),
        line,
        source,
    };

    while index < lines.len() {
        let line = lines[index];

        if let Some(label) = fence_label(line) {
            let checked = is_mesh_fence(label);
            let start = index + 2;
            let mut body = Vec::new();
            index += 1;
            while index < lines.len() && !lines[index].trim_start().starts_with("```") {
                body.push(lines[index]);
                index += 1;
            }
            if checked {
                found.push(at(start, body.join("\n")));
            }
            index += 1;
            continue;
        }

        if line.trim_start().starts_with("<pre>") {
            let start = index + 2;
            let mut body = Vec::new();
            index += 1;
            while index < lines.len() && !lines[index].trim_start().starts_with("</pre>") {
                body.push(lines[index]);
                index += 1;
            }
            index += 1;

            if body.iter().any(|line| line.starts_with(PROMPT)) {
                let mut cursor = 0;
                while cursor < body.len() {
                    match transcript_line(body[cursor]) {
                        Some((true, first)) => {
                            let at_line = start + cursor;
                            let mut unit = vec![first];
                            cursor += 1;
                            while let Some((false, continuation)) =
                                body.get(cursor).and_then(|l| transcript_line(l))
                            {
                                unit.push(continuation);
                                cursor += 1;
                            }
                            found.push(at(at_line, unit.join("\n")));
                        }
                        _ => cursor += 1,
                    }
                }
            } else {
                for (line, source) in emphasized_spans(&body, start) {
                    found.push(at(line, source));
                }
            }
            continue;
        }

        index += 1;
    }
    found
}

/// `<Tab>` and friends stand for a keypress, not for source. An example holding
/// one is describing completion and was never valid mesh to begin with.
fn is_placeholder(source: &str) -> bool {
    source.contains("<Tab>")
}

#[test]
fn every_documented_example_parses() {
    let root = repo_root();
    let scratch = std::env::temp_dir().join(format!("mesh_docs_{}", std::process::id()));
    // Usually nothing to remove, so the miss is the normal case rather than a
    // failure; `create_dir_all` on the next line is what actually has to
    // succeed. A leftover directory cannot skew the run either — every example
    // is written before it is read, and none is read from a previous one.
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    let mut checked = 0usize;
    let mut failures = Vec::new();

    for file in documentation_files(&root) {
        let text = std::fs::read_to_string(&file).expect("read a documentation file");
        for example in examples_in(&text, &file) {
            if example.source.trim().is_empty() || is_placeholder(&example.source) {
                continue;
            }
            let script = scratch.join(format!("example_{checked}.mesh"));
            std::fs::write(&script, format!("{}\n", example.source)).expect("write example");
            let output = Command::new(env!("CARGO_BIN_EXE_mesh"))
                .arg("-n")
                .arg(&script)
                .stdin(Stdio::null())
                .output()
                .expect("run mesh -n");
            checked += 1;
            if !output.status.success() {
                let shown = example.file.strip_prefix(&root).unwrap_or(&example.file);
                failures.push(format!(
                    "{}:{}\n    {}\n  {}",
                    shown.display(),
                    example.line,
                    example.source.replace('\n', "\n    "),
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
    }

    // Cleanup runs before the assertions so a failing sweep still tidies up,
    // but its result is reported *after* them: a temp-directory hiccup must not
    // be what a reader sees when an example actually stopped parsing. It is
    // still reported rather than dropped, so a scratch directory that cannot be
    // removed fails the test on its own.
    let cleaned = std::fs::remove_dir_all(&scratch);

    // A silently empty sweep would pass while checking nothing, which is the
    // failure mode a test like this is most likely to rot into: one rename in
    // the docs' markup and the extractor matches no examples at all. The floor
    // is well under the count today and only has to catch that.
    //
    // It is deliberately coarse, and does not stand in for the `extraction`
    // tests below: losing one *shape* of markup costs too few examples to trip
    // a floor, which is how a transcript-wide `<strong>` went unswept while
    // this stayed green. The floor catches collapse; those catch a shape.
    assert!(
        checked > 200,
        "only {checked} documentation examples found — the extractor has probably \
         stopped matching the docs' markup"
    );

    assert!(
        failures.is_empty(),
        "{} of {checked} documentation examples do not parse:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );

    cleaned.unwrap_or_else(|error| {
        panic!(
            "remove the scratch directory {}: {error}",
            scratch.display()
        )
    });
}

/// The extractor's own tests, on markup samples rather than on the live docs.
///
/// The sweep above cannot cover this: markup it fails to recognize yields *no*
/// example, so a shape it silently drops looks exactly like a shape that is not
/// there. The count floor bounds how much can vanish at once but not whether
/// any one construct is reaching the parser, which is how the transcript-wide
/// `<strong>` in `TOUR.md` went unchecked while the sweep stayed green.
#[cfg(test)]
mod extraction {
    use super::*;

    fn sources(text: &str) -> Vec<String> {
        examples_in(text, Path::new("sample.md"))
            .into_iter()
            .map(|example| example.source)
            .collect()
    }

    #[test]
    fn a_transcript_bolded_line_by_line_is_one_example_per_prompt() {
        let text = "<pre>\nmesh$ <strong>puts one</strong>\none\nmesh$ <strong>puts two</strong>\ntwo\n</pre>\n";
        assert_eq!(sources(text), vec!["puts one", "puts two"]);
    }

    /// The shape that was being dropped: one `<strong>` opening on the prompt
    /// line and closing after the last continuation.
    #[test]
    fn a_transcript_bolded_as_one_span_keeps_its_continuations() {
        let text = "<pre>\nmesh$ <strong>result = match $x {\n.....   _ =&gt; []\n..... }</strong>\nmesh$ <strong>puts $result</strong>\n</pre>\n";
        assert_eq!(
            sources(text),
            vec!["result = match $x {\n  _ => []\n}", "puts $result"]
        );
    }

    /// The indicator the docs write has to be the one the shell draws, or every
    /// transcript teaches a prompt nobody sees: one dot per printable column of
    /// `mesh$ `, less its trailing space.
    #[test]
    fn the_continuation_indicator_matches_the_prompt_width() {
        assert_eq!(CONTINUATION.len() + 1, PROMPT.len());
        assert!(CONTINUATION.chars().all(|c| c == '.'));
    }

    /// A block with no prompt at all bolds only the mesh half of a bash/mesh
    /// contrast, so the emphasis is what marks the source — including when a
    /// span starts mid-line or crosses lines.
    #[test]
    fn a_prompt_less_block_takes_each_emphasized_span() {
        let text = "<pre>\n# bash\nname=$(basename \"$f\")\n\n# mesh\nhere:  <strong>name = $f:base</strong>\n<strong>match $f {\n  _ =&gt; {}\n}</strong>\n</pre>\n";
        assert_eq!(
            sources(text),
            vec!["name = $f:base", "match $f {\n  _ => {}\n}"]
        );
    }

    #[test]
    fn a_fenced_block_is_taken_whole() {
        let text = "```mesh\nx = 1\nputs $x\n```\n";
        assert_eq!(sources(text), vec!["x = 1\nputs $x"]);
    }

    /// The default has to be *checked*, so a block nobody labeled is swept
    /// rather than skipped. Opting in was how three separate shapes went
    /// silently unchecked.
    #[test]
    fn an_unlabeled_fence_is_checked_like_mesh() {
        let text = "```\nx = 1\n```\n";
        assert_eq!(sources(text), vec!["x = 1"]);
    }

    /// A block that is not mesh — output, a transcript, a deliberate
    /// diagnostic — says so with its own label and is skipped by it.
    #[test]
    fn a_labeled_non_mesh_fence_is_skipped() {
        for label in ["text", "sh", "rust", "console"] {
            let text = format!("```{label}\nnot mesh at all (((\n```\n");
            assert!(
                sources(&text).is_empty(),
                "a ```{label} block should not be swept"
            );
        }
    }

    /// Output lines inside a transcript are not source and must not be swept in.
    #[test]
    fn transcript_output_is_not_taken_as_source() {
        let text = "<pre>\nmesh$ <strong>puts hi</strong>\nhi\nnot a command\n</pre>\n";
        assert_eq!(sources(text), vec!["puts hi"]);
    }

    #[test]
    fn a_reported_line_points_at_the_example() {
        let text = "intro\n\n<pre>\nmesh$ <strong>puts hi</strong>\n</pre>\n";
        let found = examples_in(text, Path::new("sample.md"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 4);
    }

    /// The design documents are skipped and their neighbors in `docs/` are not,
    /// so the exclusion stays two files wide rather than quietly swallowing the
    /// directory.
    #[test]
    fn only_the_design_documents_are_excluded() {
        let files = documentation_files(&repo_root());
        assert!(!files.iter().any(|path| is_design_document(path)));
        for excluded in ["DESIGN.md", "TYPES.md"] {
            assert!(
                is_design_document(Path::new(excluded)),
                "{excluded} should be exempt from the sweep"
            );
        }
        assert!(
            !is_design_document(Path::new("REFERENCE.md")),
            "the exemption must stay a named list rather than a pattern"
        );
        for expected in ["README.md", "INTRO.md", "TOUR.md", "REFERENCE.md"] {
            assert!(
                files
                    .iter()
                    .any(|path| path.file_name().is_some_and(|name| name == expected)),
                "{expected} should still be swept"
            );
        }
    }

    /// Every file the sweeps read rides the CODE lane, and root markdown that
    /// nothing reads rides the docs lane.
    ///
    /// The policy in `.github/lanes.conf` decides whether CI runs this suite
    /// at all, and its failure mode is the quiet one: classify and gate derive
    /// the same wrong `docs` verdict, the sweeps skip, and a `docs:` pull
    /// request merges green over an example that stopped working — leaving the
    /// next code pull request red on prose it never touched. Nothing outside
    /// this file knows which files the sweeps read, so the assertion belongs
    /// beside the function that enumerates them.
    #[test]
    fn every_swept_document_is_code() {
        let root = repo_root();
        for file in documentation_files(&root) {
            let path = file
                .strip_prefix(&root)
                .expect("a swept file lives under the repo root")
                .to_str()
                .expect("a documentation path is UTF-8");
            assert_eq!(
                lane(path),
                "code",
                "{path} is read by this suite, so a docs-only skip would skip the read"
            );
        }
        // `include_str!` in `mesh-core`'s builtin table makes this one a
        // compile input too, and it is swept by name above — asserted
        // separately so the reason survives a change to what is swept.
        assert_eq!(lane("docs/REFERENCE.md"), "code");
        // The design documents are exempt from the sweep but not from the
        // tree rule, and a file that does not exist yet is code before anyone
        // remembers to add it.
        for path in ["docs/DESIGN.md", "docs/TYPES.md", "docs/NEW.md"] {
            assert_eq!(lane(path), "code", "{path}");
        }
        // The other direction, which is what keeps the lane worth having:
        // root markdown nothing reads still skips the suite.
        for path in ["AGENTS.md", "TODO.md", "ROADMAP.md", "DEVELOPMENT.md"] {
            assert_eq!(lane(path), "docs", "{path}");
        }
        // And nothing that builds is ever documentation.
        for path in ["crates/mesh/tests/docs.rs", "crates/mesh-core/src/lib.rs"] {
            assert_eq!(lane(path), "code", "{path}");
        }
    }

    /// The lane `.github/lanes.conf` gives a repo-relative path: ordered
    /// rules, first match wins, no rule means code — the engine's own
    /// contract (mikelward/lanes).
    fn lane(path: &str) -> &'static str {
        let policy = std::fs::read_to_string(repo_root().join(".github/lanes.conf"))
            .expect("read .github/lanes.conf");
        for line in policy.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((verdict, pattern)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            if verdict != "code" && verdict != "docs" {
                continue;
            }
            if glob_matches(pattern.trim(), path) {
                return if verdict == "docs" { "docs" } else { "code" };
            }
        }
        "code"
    }

    /// Whether a policy pattern matches a path, for the pattern shapes this
    /// policy uses.
    ///
    /// The engine matches with Node's `path.matchesGlob`; this understands the
    /// three shapes `.github/lanes.conf` actually writes and **panics** on any
    /// other. Returning `false` for a shape it did not understand would report
    /// every path as code and make every assertion above vacuously true, which
    /// is exactly the false pass this test exists to prevent.
    fn glob_matches(pattern: &str, path: &str) -> bool {
        if let Some(prefix) = pattern.strip_suffix("/**") {
            // `**` crosses `/`; `docs/**` matches at any depth below `docs/`
            // and does not match `docs` itself.
            return path.starts_with(&format!("{prefix}/"));
        }
        if let Some(extension) = pattern.strip_prefix("*.") {
            // `*` does not cross `/`, so this reaches the root and no further.
            return !path.contains('/') && path.ends_with(&format!(".{extension}"));
        }
        assert!(
            !pattern.contains('*'),
            "the lane assertions do not understand the pattern {pattern:?} — \
             teach glob_matches its shape rather than letting it match nothing"
        );
        pattern == path
    }
}
