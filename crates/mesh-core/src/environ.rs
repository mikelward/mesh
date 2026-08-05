//! The `$env` namespace — the boundary between mesh's typed values and the
//! process environment.
//!
//! The environment is a flat `KEY=bytes` table, so only byte-strings cross it.
//! The one exception is the **path-type** variables (`PATH` and friends), which
//! are lists in mesh and are `:`-joined on the way out and split on the way in —
//! a defined serialization for known names, not a general "lists become strings"
//! rule. See `DESIGN.md` §"Variables and assignment".

use std::env;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::vars::Value;

/// Environment names carried as `:`-delimited lists. A fixed built-in set for
/// now; `DESIGN.md` adds `export --list NAME` to opt other names in.
const PATH_VARS: [&str; 6] = [
    "PATH",
    "MANPATH",
    "CDPATH",
    "INFOPATH",
    "LD_LIBRARY_PATH",
    "PYTHONPATH",
];

/// The separator for path-type variables. Fixed, per `DESIGN.md`.
const PATH_SEPARATOR: char = ':';

pub(crate) fn is_path_var(key: &str) -> bool {
    PATH_VARS.contains(&key)
}

/// Read `$env.KEY`, or `None` when it is unset — a strict read, so the caller
/// reports the absence rather than substituting an empty string.
pub(crate) fn read(key: &str) -> Option<Value> {
    Some(typed(key, &env::var_os(key)?))
}

/// Every entry's **name**, for the completion tables. Names only, so nothing is
/// typed on the way past — unlike [`snapshot`], which builds a value per entry
/// and would be pure waste when only the keys are wanted.
///
/// A name that is not valid UTF-8 is skipped rather than lossily spelled: the
/// lossy form names nothing, so completing it would insert a word that reads
/// back as a different variable — or as some *other* entry that really is
/// spelled that way.
pub(crate) fn names() -> impl Iterator<Item = String> {
    env::vars_os().filter_map(|(name, _)| name.into_string().ok())
}

/// The mesh value an entry's raw bytes denote: a list for a path-type name,
/// a string otherwise. Shared by [`read`] and [`snapshot`] so a name cannot be
/// typed one way when asked for by name and another when listed.
fn typed(key: &str, raw: &std::ffi::OsStr) -> Value {
    let raw = raw.to_string_lossy();
    if is_path_var(key) {
        Value::List(split_path(&raw))
    } else {
        Value::String(raw.into_owned())
    }
}

/// The whole environment as an ordered map, sorted by name so the order is stable
/// rather than inherited from `environ`'s arbitrary one. Each entry is typed as
/// [`read`] types it, so a path-type name is a **list** here too — which is what
/// keeps `$env:get(PATH, …)` and `$env.PATH` the same value, and what makes those
/// entries print as indented blocks under the ordinary nesting rule — `puts $env`
/// needs no rule of its own. The sort is what makes that print stable, as well as
/// ordering `$env:keys`, `$env:get(NAME, …)` and `$env.NAME`.
///
/// This is what a bare `$env` denotes, which is what gives `$env:get(EDITOR,
/// vim)` — the `${EDITOR:-vim}` of `DESIGN.md` §"Variables and assignment" — an
/// ordinary map to work on rather than a special case in the resolver.
pub(crate) fn snapshot() -> Value {
    entries_of(env::vars_os())
}

/// The listing, built from `(name, value)` pairs rather than by re-querying each
/// name. Re-querying loses an entry whose **name** is not valid UTF-8: the lossy
/// spelling names nothing, so the entry vanishes — or, worse, names some *other*
/// variable that really is spelled that way, and the listing shows its value
/// under the wrong key. The pair is already in hand, so nothing has to be asked
/// for twice. Split out so a test can hand it names the process does not have.
fn entries_of(vars: impl Iterator<Item = (OsString, OsString)>) -> Value {
    let mut entries: Vec<(String, Value)> = vars
        .map(|(key, value)| {
            let key = key.to_string_lossy().into_owned();
            let value = typed(&key, &value);
            (key, value)
        })
        .collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    Value::Map(entries)
}

/// Split a path-type value **exactly**: every empty component is kept, leading,
/// interior, and trailing alike. `PATH=/usr/bin:` means "…and the cwd", so
/// dropping the empty would change its meaning, and a split→join round trip has
/// to be byte-faithful.
fn split_path(raw: &str) -> Vec<Value> {
    raw.split(PATH_SEPARATOR)
        .map(|part| Value::String(part.to_owned()))
        .collect()
}

/// Names the process environment cannot hold, reported rather than hit.
///
/// `set_var` and `remove_var` **panic** on an empty key, a `=` in one, or a NUL —
/// they are `EINVAL` at the syscall, and `std` turns that into an abort. A
/// literal `$env.KEY` could never reach them, since the parser proved it a
/// `valid_name` first; a computed `$env[$name]` is the first key a *user* supplies
/// that is only known at run time, so this is where it gets checked.
///
/// It checks only what actually panics, deliberately, rather than re-applying
/// `valid_name`: `$env:keys` answers with every name the process really has, so a
/// round trip over them (`for k in $env:keys { $env[$k] = … }`) must not fail on a
/// name that mesh's own grammar could not spell.
fn check_key(key: &str) -> Result<(), String> {
    let problem = if key.is_empty() {
        "be empty"
    } else if key.contains('=') {
        "contain `=`"
    } else if key.contains('\0') {
        "contain a NUL byte"
    } else {
        return Ok(());
    };
    // Debug-quoted, so an empty name is visible and a NUL is spelled rather than
    // written into the diagnostic as itself.
    Err(format!(
        "$env[{key:?}]: an environment name cannot {problem}"
    ))
}

/// Write `$env.KEY`, appending to the current value when `append`.
///
/// Returns the message to report when the name or the value cannot cross the
/// boundary.
pub(crate) fn write(key: &str, value: Value, append: bool) -> Result<(), String> {
    check_key(key)?;
    let value = if append {
        append_bytes(key, value)?
    } else {
        OsString::from(serialize(key, value)?)
    };
    if value.as_bytes().contains(&0) {
        return Err(format!(
            "$env.{key}: an environment value cannot contain a NUL byte"
        ));
    }
    // SAFETY: the shell runs its execution loop single-threaded, so mutating the
    // environment here races with nothing — the same reasoning `cd` relies on
    // when it updates `$env.PWD`.
    unsafe { env::set_var(key, value) };
    Ok(())
}

/// Remove `$env.KEY` from the process environment, so children stop inheriting it.
///
/// Removing what is not there is an **error**, matching what `unset` already does
/// for a name and for a place inside a binding: nothing missing is forgiven, so a
/// typo says so rather than passing silently. It is the read side's message, so a
/// failed removal and a failed read describe the same absence the same way.
pub(crate) fn remove(key: &str) -> Result<(), String> {
    check_key(key)?;
    if env::var_os(key).is_none() {
        return Err(format!("$env.{key}: not set"));
    }
    // SAFETY: single-threaded execution loop, as `write` above relies on.
    unsafe { env::remove_var(key) };
    Ok(())
}

/// The bytes `key` should hold after `+=`: what is there now, then `addition`,
/// separated by `:` for a path-type name.
///
/// This works on the **raw bytes** rather than reading the current value into a
/// mesh `Value` first. Environment values are arbitrary non-NUL bytes, but a
/// mesh string is UTF-8, so decoding what is already there would replace any
/// invalid sequence with U+FFFD and write the mangled version back — quietly
/// breaking, say, a non-UTF-8 `PATH` component that had been resolving fine.
/// Appending never needs to look at the existing bytes, so it does not.
fn append_bytes(key: &str, addition: Value) -> Result<OsString, String> {
    let addition = serialize(key, addition)?;
    // Unset: `+=` starts the value rather than failing, and takes no separator —
    // an absent name and an empty one mean the same thing to a child.
    let Some(current) = env::var_os(key) else {
        return Ok(OsString::from(addition));
    };
    let mut bytes = current.into_vec();
    if is_path_var(key) {
        bytes.push(PATH_SEPARATOR as u8);
    }
    bytes.extend_from_slice(addition.as_bytes());
    Ok(OsString::from_vec(bytes))
}

/// Render a value as the bytes the environment carries, or explain why it
/// cannot cross.
fn serialize(key: &str, value: Value) -> Result<String, String> {
    Ok(match value {
        Value::String(text) => text,
        // The environment carries bytes, so a styled value crosses as its text.
        // Shipping SGR escapes to every child would be the wrong reading of
        // "behaves exactly as its text".
        Value::Styled(styled) => styled.text,
        Value::Integer(number) => number.to_string(),
        Value::Boolean(flag) => flag.to_string(),
        // The environment carries bytes and a flag has them, so it crosses as
        // the word it was written with -- the argv rule, one boundary over.
        Value::Flag(flag) => flag.text(),
        Value::FlagTerminator => "--".to_owned(),
        Value::List(entries) if is_path_var(key) => join_path(key, entries)?,
        Value::List(_) => {
            return Err(format!(
                "$env.{key}: only strings cross into the environment; join the list first, \
                 e.g. `$env.{key} = $dirs:join(\":\")`"
            ));
        }
        Value::Map(_) => {
            return Err(format!(
                "$env.{key}: only strings cross into the environment, not a map"
            ));
        }
        Value::Function(_) => {
            return Err(format!(
                "$env.{key}: only strings cross into the environment, not a function"
            ));
        }
        Value::Regex(_) | Value::Glob(_) => {
            return Err(format!(
                "$env.{key}: only strings cross into the environment, not a pattern"
            ));
        }
        Value::Stream(_) | Value::Job(_) => {
            return Err(format!(
                "$env.{key}: only strings cross into the environment, not a stream handle"
            ));
        }
    })
}

/// Join a path-type list. Nested lists have no `:`-serialization, so they are an
/// error rather than a silently flattened one.
fn join_path(key: &str, entries: Vec<Value>) -> Result<String, String> {
    let mut parts = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            Value::String(text) => parts.push(text),
            Value::Integer(number) => parts.push(number.to_string()),
            Value::Boolean(flag) => parts.push(flag.to_string()),
            _ => {
                return Err(format!(
                    "$env.{key}: a path entry must be a string, not a nested value"
                ));
            }
        }
    }
    Ok(parts.join(&PATH_SEPARATOR.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[Value]) -> Vec<&str> {
        values
            .iter()
            .map(|value| match value {
                Value::String(text) => text.as_str(),
                _ => panic!("expected strings"),
            })
            .collect()
    }

    #[test]
    fn path_splitting_keeps_every_empty_component() {
        // `PATH=/usr/bin:` means "…and the cwd", so the trailing empty is
        // meaningful and a round trip has to preserve it.
        for raw in ["/a:/b", ":/a", "/a:", "/a::/b", "", ":"] {
            let split = split_path(raw);
            let joined = join_path("PATH", split.clone()).unwrap();
            assert_eq!(joined, raw, "round trip of {raw:?}");
            assert_eq!(split.len(), raw.split(':').count(), "components of {raw:?}");
        }
        assert_eq!(strings(&split_path("/a::/b")), ["/a", "", "/b"]);
    }

    #[test]
    fn only_strings_and_path_lists_cross_the_boundary() {
        assert_eq!(
            serialize("EDITOR", Value::String("vim".into())).unwrap(),
            "vim"
        );
        assert_eq!(serialize("COUNT", Value::Integer(3)).unwrap(), "3");
        assert_eq!(
            serialize("PATH", Value::List(vec![Value::String("/a".into())])).unwrap(),
            "/a"
        );

        let error = serialize("EDITOR", Value::List(vec![Value::String("/a".into())])).unwrap_err();
        assert!(error.contains("join the list first"), "{error}");
        assert!(
            serialize("EDITOR", Value::Map(Vec::new()))
                .unwrap_err()
                .contains("not a map")
        );
        assert!(
            join_path("PATH", vec![Value::List(Vec::new())])
                .unwrap_err()
                .contains("must be a string")
        );
    }

    #[test]
    fn path_vars_are_the_fixed_built_in_set() {
        assert!(is_path_var("PATH"));
        assert!(is_path_var("LD_LIBRARY_PATH"));
        assert!(!is_path_var("EDITOR"));
        // Case matters: the environment is case-sensitive, so `Path` is a
        // different, ordinary variable.
        assert!(!is_path_var("Path"));
    }

    /// The three names `set_var` / `remove_var` abort on. A literal `$env.KEY`
    /// could never carry one — the parser proved it a name first — so this guard
    /// exists for the computed `$env[$n]`, whose key is a run-time value.
    #[test]
    fn a_name_the_process_cannot_hold_is_an_error_not_a_panic() {
        for (key, expected) in [
            ("", "cannot be empty"),
            ("A=B", "cannot contain `=`"),
            ("A\0B", "cannot contain a NUL byte"),
        ] {
            let error = check_key(key).unwrap_err();
            assert!(error.contains(expected), "{key:?} gave {error}");
        }

        // Only those three. A name mesh's own grammar could not spell still
        // passes, because `$env:keys` can answer with one and a round trip over
        // the listing has to be able to write what it read.
        for key in ["PATH", "MY-VAR", "MY VAR", "lower", "1LEADING"] {
            assert!(check_key(key).is_ok(), "{key:?}");
        }
    }

    /// A listing is built from the pairs it was handed, so an entry whose **name**
    /// is not valid UTF-8 still appears — under its lossy spelling, since that is
    /// all a mesh string can hold, but with **its own value**. Re-querying the
    /// lossy name instead would drop it, or find a different variable that really
    /// is spelled that way and show that one's value under this key.
    #[test]
    fn a_listing_keeps_an_entry_whose_name_is_not_utf8() {
        use std::os::unix::ffi::OsStringExt;

        let invalid = OsString::from_vec(b"BAD\xffNAME".to_vec());
        let lossy = invalid.to_string_lossy().into_owned();
        let Value::Map(entries) = entries_of(
            [
                (invalid, OsString::from("its own")),
                // Present under the *lossy* spelling too, which is what makes the
                // re-query path pick the wrong one rather than merely lose one.
                (OsString::from(lossy.clone()), OsString::from("the other")),
                (OsString::from("PATH"), OsString::from("/bin:/usr/bin")),
            ]
            .into_iter(),
        ) else {
            panic!("a listing is a map");
        };

        let found: Vec<&Value> = entries
            .iter()
            .filter(|(key, _)| *key == lossy)
            .map(|(_, value)| value)
            .collect();
        assert_eq!(found.len(), 2, "both entries survive: {entries:?}");
        assert!(found.contains(&&Value::String("its own".into())));
        assert!(found.contains(&&Value::String("the other".into())));

        // A path-type name is still typed as a list when listed, exactly as
        // reading it by name gives.
        let path = entries.iter().find(|(key, _)| key == "PATH").expect("PATH");
        assert_eq!(path.1, Value::List(split_path("/bin:/usr/bin")));
    }
}
