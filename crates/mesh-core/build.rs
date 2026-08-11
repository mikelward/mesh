//! Stamps the build with the version `mesh --version` and `$sh.version` report.
//!
//! The workspace version in `Cargo.toml` is the placeholder `0.0.0`, because
//! releases are numbered by commit count rather than by a hand-edited field, so
//! `CARGO_PKG_VERSION` says `0.0.0` for every build and tells a user nothing.
//! This derives the number the release workflow tags a commit with, and marks
//! every build that is *not* one of those commits so the two can't be confused:
//!
//! | Stamp | Build |
//! |---|---|
//! | `0.0.888` | A clean checkout of a commit on `main` — what the `v0.0.888` release ships |
//! | `0.0.888+quoting.g1a2b3c4` | A clean checkout elsewhere: the branch says which work, the sha which commit, the number which release it follows |
//! | `0.0.888+quoting.g1a2b3c4.dirty` | The sources carried uncommitted changes |
//! | `0.0.0+unknown` | Built without git, or from a copy outside the repository |
//!
//! Everything after the `+` is semver build metadata, which comparisons ignore,
//! so a stamped build still sorts as the release it followed.
//!
//! `MESH_BUILD_VERSION` overrides all of it, for a build from a source archive
//! that knows its version but has no history to derive it from.
//!
//! **The stamp describes the sources this crate was last compiled from**, not
//! the working tree as it stands now. The `rerun-if-changed` lines cover what
//! changes the answer — a commit, a branch switch, an edit under `crates/` —
//! but editing a file that is not built from (a doc, the `Makefile`) leaves the
//! `dirty` marker off until some other change triggers a rebuild. That is the
//! honest reading of a version stamp: it names the code in the binary.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What a build with no history to read reports.
const UNKNOWN: &str = "0.0.0+unknown";

fn main() {
    println!("cargo::rerun-if-env-changed=MESH_BUILD_VERSION");
    // Naming any dependency turns off cargo's default scan of the package
    // directory, so name the sources back — all of `crates/`, since the stamp
    // describes the whole binary rather than this crate alone.
    println!("cargo::rerun-if-changed=..");
    println!("cargo::rerun-if-changed=../../Cargo.toml");
    println!("cargo::rerun-if-changed=../../Cargo.lock");
    watch_git_head();

    let version = build_version_override().unwrap_or_else(git_version);
    println!("cargo::rustc-env=MESH_VERSION={version}");
}

/// The version `MESH_BUILD_VERSION` asks for, if it asks for one.
///
/// Empty counts as unset, since that is how an unfilled variable arrives from a
/// build system, and deriving is the right answer there. Bytes that are not text
/// are a different thing: something asked for a version this cannot represent,
/// and quietly stamping a git-derived one instead would answer a question nobody
/// asked. That warns, and the derivation carries on — the same "the build must
/// not fail over this, but it must say so" the git calls follow.
fn build_version_override() -> Option<String> {
    let version = match std::env::var("MESH_BUILD_VERSION") {
        // Surrounding space is what a `$(...)` in a build system leaves behind,
        // and is not part of the version anyone means.
        Ok(version) => version.trim().to_owned(),
        Err(std::env::VarError::NotPresent) => return None,
        Err(error) => {
            unusable(&format!("is set but unusable ({error})"));
            return None;
        }
    };
    if version.is_empty() {
        return None;
    }
    // Checked here rather than left to `mesh --version` to report: the stamp is
    // what `$sh.version` answers and what the suite holds to the semver grammar,
    // so a malformed override would ship a version mesh's own tests reject. A
    // newline would be worse than malformed — cargo reads this script's output a
    // line at a time, so one inside the value writes a directive of its own.
    if !is_semver(&version) {
        unusable(&format!("is not a semver version ({version:?})"));
        return None;
    }
    Some(version)
}

/// Report an override that cannot be used, and that the derivation runs instead.
fn unusable(what: &str) {
    println!(
        "cargo::warning=MESH_BUILD_VERSION {what}, so mesh's version is derived from the \
         checkout instead"
    );
}

/// Whether this is a semver version: `MAJOR.MINOR.PATCH`, then an optional
/// `-prerelease`, then an optional `+build`.
///
/// Build metadata is split off first, since it may itself contain the `-` that
/// opens a prerelease. Numbers — the three core ones, and any prerelease
/// identifier that is all digits — are canonical decimal, so `01` is not one.
fn is_semver(version: &str) -> bool {
    let (core, metadata) = match version.split_once('+') {
        Some((core, metadata)) => (core, Some(metadata)),
        None => (version, None),
    };
    let (release, prerelease) = match core.split_once('-') {
        Some((release, prerelease)) => (release, Some(prerelease)),
        None => (core, None),
    };

    let numbers: Vec<&str> = release.split('.').collect();
    if numbers.len() != 3 || !numbers.iter().all(|number| is_number(number)) {
        return false;
    }
    let identifiers = |tail: Option<&str>, numeric_rule: bool| {
        tail.is_none_or(|tail| {
            tail.split('.').all(|identifier| {
                !identifier.is_empty()
                    && identifier
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    && (!numeric_rule
                        || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                        || is_number(identifier))
            })
        })
    };
    identifiers(prerelease, true) && identifiers(metadata, false)
}

/// Whether this is a semver numeric identifier: digits, and no leading zero.
fn is_number(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
}

/// Run a git command that a working repository always answers.
///
/// Git refusing one of these is not "there is no history here" — it is a
/// repository git can see and won't read: dubious ownership under a different
/// user, an unreadable index, a corrupt object. Falling back to [`UNKNOWN`]
/// silently would report that as indistinguishable from building an unpacked
/// source archive, and leave a fixable setup problem looking like the version
/// stamp not being supported. The build still succeeds — a compiler must not
/// need git — but it says so.
fn git(args: &[&str]) -> Option<String> {
    decode(args, run(args, &[])?)
}

/// Run such a command for output that is not text.
///
/// A path in `git status --porcelain` is whatever bytes the filesystem holds,
/// and with `core.quotePath=false` git prints them as they are — so a tree with
/// one Latin-1 filename in it would fail to decode, and a version stamp is no
/// place to lose the whole answer over that. The question asked of `status` is
/// only whether it said anything at all, which bytes answer as well as text.
fn git_bytes(args: &[&str]) -> Option<Vec<u8>> {
    run(args, &[])
}

/// Run a git command whose answer can be "no", which git spells as exit 1.
///
/// Is HEAD on a branch, is there an `origin/main`, is this commit on it, do two
/// commits share a base — git answers all of those by exiting 1, so *that*
/// status is a result rather than a fault. Every other refusal still goes
/// through [`warn`], which is the difference between "no" and "git could not
/// tell me": exit 1 from `merge-base` means no merge base, while 128 means it
/// could not read the objects to look, and the two must not become one `None`.
fn git_yes_no(args: &[&str]) -> Option<String> {
    decode(args, run(args, &[1])?)
}

/// Read git's output as text, reporting bytes that are not.
///
/// Every caller but [`git_bytes`] wants a sha, a count, a ref or a tag, all of
/// which are text — so bytes that are not are a fault worth naming rather than
/// one more silent road to [`UNKNOWN`].
fn decode(args: &[&str], stdout: Vec<u8>) -> Option<String> {
    match String::from_utf8(stdout) {
        Ok(text) => Some(text.trim().to_owned()),
        Err(error) => {
            warn(args, &error.to_string());
            None
        }
    }
}

/// The shared body: run git, and report any failure that is not on `no`.
fn run(args: &[&str], no: &[i32]) -> Option<Vec<u8>> {
    let output = match git_output(args) {
        Ok(output) => output,
        // There being no git to run is the source-archive case, and is
        // expected. Any other launch failure — a `git` on `PATH` that cannot be
        // executed, say — is a fault, and reads as "no history" without this.
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn(args, &error.to_string());
            }
            return None;
        }
    };
    if output.status.success() {
        return Some(output.stdout);
    }
    if output.status.code().is_some_and(|code| no.contains(&code)) {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Sources sitting outside any repository are the other expected case, and
    // this is the message that reports it — read in English, per `git_output`.
    if !stderr.contains("not a git repository") {
        warn(args, &stderr);
    }
    None
}

/// Say on the build's output that a git call failed and what it cost.
///
/// What is lost depends on the call — the whole version for most of them, only
/// the commit count for `rev-list` — so this says the version falls back rather
/// than naming a stamp it may not produce. A cargo warning is one line, so a
/// multi-line refusal is folded into one.
fn warn(args: &[&str], reason: &str) {
    println!(
        "cargo::warning=`git {}` failed, so mesh's version falls back instead of \
         describing this checkout: {}",
        args.join(" "),
        reason.trim().replace('\n', "; ")
    );
}

fn git_output(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        // Git translates its messages, and `run` reads one.
        .env("LC_ALL", "C")
        .args(args)
        .output()
}

/// Rebuild when the commit changes.
///
/// `logs/HEAD` is the one that catches a commit whose ref is packed, where the
/// loose ref file does not exist to be watched. Only existing paths are named:
/// cargo treats a missing one as "changed" and would rerun this script, and so
/// recompile the crate, on every single build.
///
/// Tags and `origin/main` are deliberately not watched, for the same reason:
/// both move on every push to `main`, so a routine `git fetch` would rebuild the
/// crate, and neither changes the answer for a commit already built. A tag says
/// what the count already said, and `origin/main` moving forward leaves both the
/// merge base and "is HEAD on `main`" where they were. The one case it costs is
/// committing to `main` locally and pushing, where the stamp keeps its
/// pre-push metadata until something else triggers a rebuild — and committing to
/// `main` is not how this repository works.
fn watch_git_head() {
    let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    let git_dir = PathBuf::from(git_dir);
    let mut paths = vec![git_dir.join("HEAD"), git_dir.join("logs/HEAD")];
    if let Some(head_ref) = git_yes_no(&["symbolic-ref", "--quiet", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &head_ref])
    {
        paths.push(PathBuf::from(path));
    }
    if let Some(common_dir) = git(&["rev-parse", "--git-common-dir"]) {
        paths.push(PathBuf::from(common_dir).join("packed-refs"));
    }
    for path in paths.iter().filter(|path| path.exists()) {
        println!("cargo::rerun-if-changed={}", path.display());
    }
}

/// The version this checkout describes, or [`UNKNOWN`].
fn git_version() -> String {
    if !is_this_workspace() {
        return UNKNOWN.to_owned();
    }
    let (Some(sha), Some(status)) = (
        git(&["rev-parse", "--short=7", "HEAD"]),
        git_bytes(&["status", "--porcelain"]),
    ) else {
        return UNKNOWN.to_owned();
    };
    let dirty = !status.is_empty();

    // A release tag on this exact commit is the release itself, and it holds
    // even in a shallow clone, where no count is trustworthy. Every tag on the
    // commit is offered to `release_tag` rather than `git describe`'s single
    // pick: describe prefers an annotated tag, so a stray annotated `v0.0.1junk`
    // beside the real lightweight `v0.0.1` would be the one it answered with,
    // and the release would be missed for a tag nobody meant anything by.
    // Listing them is another call a working repository answers: no tags is a
    // successful empty listing, so a refusal is a fault and is reported.
    if !dirty
        && let Some(tags) = git(&["tag", "--points-at", "HEAD"])
        && let Some(release) = tags.lines().find_map(|tag| release_tag(tag.trim()))
    {
        return release;
    }

    // Asked of `show-ref` rather than `rev-parse --verify --quiet`, which exits
    // 1 with nothing to say both for a ref that is not there and for one whose
    // object cannot be read — and the second is a broken repository rather than
    // an answer. `show-ref` exits 1 silently only for the former, and reports
    // `bad ref` for the latter, which lands in `warn` where it belongs.
    let main = [
        ("refs/remotes/origin/main", "origin/main"),
        ("refs/heads/main", "main"),
    ]
    .into_iter()
    .find(|(reference, _)| git_yes_no(&["show-ref", "--verify", "--quiet", reference]).is_some())
    .map(|(_, name)| name);
    let on_main = main
        .is_some_and(|name| git_yes_no(&["merge-base", "--is-ancestor", "HEAD", name]).is_some());
    // Off `main`, count to the merge base instead of to HEAD: a branch's own
    // commits would push the number past the newest release and read as one
    // from the future. Counting the base says which release the work sits on.
    let counted = main
        .and_then(|name| git_yes_no(&["merge-base", "HEAD", name]))
        .unwrap_or_else(|| "HEAD".to_owned());
    let count = if git(&["rev-parse", "--is-shallow-repository"]).as_deref() == Some("true") {
        // A shallow clone sees part of the history, so its count is a smaller
        // number than the release with no sign that it is wrong.
        None
    } else {
        // Counting is the other call a working repository always answers, so a
        // refusal here — a missing or corrupt object somewhere back in the
        // history — is reported rather than quietly becoming a `0.0.0` base.
        git(&["rev-list", "--count", &counted])
    };

    if let Some(count) = &count
        && on_main
        && !dirty
    {
        return format!("0.0.{count}");
    }

    // What this build *is*, in the order a person asks it: which work, which
    // commit, whether it was modified.
    let mut metadata: Vec<String> = branch_identifier().into_iter().collect();
    metadata.push(format!("g{sha}"));
    if dirty {
        metadata.push("dirty".to_owned());
    }
    format!(
        "0.0.{}+{}",
        count.unwrap_or_else(|| "0".to_owned()),
        metadata.join(".")
    )
}

/// The version a release tag names, if the tag is one.
///
/// Releases here are `v0.0.<commit count>` and nothing else, so a tag of any
/// other shape — `v1-test`, or a `v1.2.3` from some experiment — is not evidence
/// of a release and must not override the derived version. Git's own tag
/// matching is glob-only and cannot say "digits", so every tag on the commit is
/// offered here and the shape is decided in one place.
///
/// A count is a `git rev-list --count`, which never pads, so `v0.0.001` is not a
/// tag the release workflow can have written — and `0.0.001` is not a semver
/// version either, since a numeric identifier cannot carry a leading zero.
fn release_tag(tag: &str) -> Option<String> {
    let version = tag.strip_prefix('v')?;
    let count = version.strip_prefix("0.0.")?;
    let canonical = count == "0" || !count.starts_with('0');
    (!count.is_empty() && canonical && count.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| version.to_owned())
}

/// The branch this build came off, as a semver build-metadata identifier.
///
/// The last segment only: branches here are `<agent>/<topic>` or `<user>/<topic>`,
/// and the prefix says who is working rather than what the build is. Anything
/// semver does not allow in an identifier becomes a hyphen, since a branch name
/// admits `_`, `.` and more. A detached HEAD has no branch and reports none —
/// the commit that follows already identifies it.
fn branch_identifier() -> Option<String> {
    let branch = git_yes_no(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let topic = branch.rsplit('/').next().unwrap_or(&branch);
    let identifier: String = topic
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    let identifier = identifier.trim_matches('-').to_owned();
    (!identifier.is_empty()).then_some(identifier)
}

/// Whether the enclosing repository is mesh's own.
///
/// Sources unpacked from an archive — what `cargo install mesh` builds — inherit
/// whatever repository they happen to sit under, and reporting *its* commit and
/// dirty state would be a confident lie. A repository whose `crates/mesh-core`
/// is the directory being built is this one.
fn is_this_workspace() -> bool {
    // Through the diagnosing helper: this is the *first* git call, so a
    // repository git can see and won't read is refused here, before either call
    // in `git_version` runs. Silence here would put the warning below a door
    // that a dubious-ownership refusal has already closed.
    let Some(toplevel) = git(&["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    match (
        Path::new(&toplevel).join("crates/mesh-core").canonicalize(),
        Path::new(&manifest_dir).canonicalize(),
    ) {
        (Ok(from_repo), Ok(building)) => from_repo == building,
        // A repository with no `crates/mesh-core` of its own to resolve is
        // precisely the case this guards: some other repository that these
        // sources were unpacked underneath.
        _ => false,
    }
}
