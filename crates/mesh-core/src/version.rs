//! The version the running binary reports, handed in at startup.
//!
//! `mesh --version` and `$sh.version` both answer with this. It is *not* derived
//! here: the binary crate's `build.rs` reads it off the checkout and passes it to
//! [`crate::run`], because the stamp changes with every commit and whichever
//! crate bakes it in is recompiled on every commit — which should be the thin
//! binary rather than this one.
//!
//! A `&'static str` through one setter rather than a field threaded to both
//! readers: the two are far apart (the startup-option parser and the `$sh` map,
//! which hangs off [`crate::vars::Vars`]), and threading it would put a version
//! parameter on `Shell::new` and its fifty-odd call sites for a value that is
//! constant for the life of the process.

use std::sync::OnceLock;

/// What a reader sees when nothing set one.
///
/// Only reachable from this crate's own unit tests, which call into the option
/// parser and the `$sh` map directly rather than through [`crate::run`] — the
/// binary cannot skip setting it, since [`crate::run`] takes it as an argument.
/// A distinct stamp from `build.rs`'s `0.0.0+unknown`, which means "built with no
/// history to read": that is a real build a user can hold, and this never is.
const UNSET: &str = "0.0.0+unset";

static VERSION: OnceLock<&'static str> = OnceLock::new();

/// Record the version the binary was stamped with. Called once, by [`crate::run`].
pub(crate) fn set(version: &'static str) {
    let installed = VERSION.get_or_init(|| version);
    // Two `run` calls in one process hand over the same compiled-in string, so
    // the only way these differ is a caller that invented one. `OnceLock` cannot
    // be reset, so the first stands — but it is said rather than swallowed,
    // since `--version` would otherwise disagree with what was just passed.
    if *installed != version {
        note!("mesh: version is already {installed}, ignoring {version}");
    }
}

/// The version to report.
pub(crate) fn get() -> &'static str {
    VERSION.get().copied().unwrap_or(UNSET)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset is a version, not a panic or an empty string: a `$sh.version` read
    /// from a unit test has to answer something, and the suite holds every
    /// version it sees to the semver grammar.
    #[test]
    fn an_unset_version_still_reads_as_semver() {
        let reported = get();
        let (core, metadata) = reported.split_once('+').expect("a build-metadata tail");
        assert_eq!(core.split('.').count(), 3, "{reported:?}");
        assert!(core.split('.').all(|part| part == "0"), "{reported:?}");
        assert!(
            metadata
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "{reported:?}"
        );
    }

    /// The setter is idempotent for the value the binary actually passes, so a
    /// second frontend calling `run` in-process does not trip the mismatch path.
    #[test]
    fn setting_the_same_version_twice_agrees() {
        set(UNSET);
        set(UNSET);
        assert_eq!(get(), UNSET);
    }
}
