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
    let raw = env::var_os(key)?.to_string_lossy().into_owned();
    Some(if is_path_var(key) {
        Value::List(split_path(&raw))
    } else {
        Value::String(raw)
    })
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

/// Write `$env.KEY`, appending to the current value when `append`.
///
/// Returns the message to report when the value cannot cross the boundary.
pub(crate) fn write(key: &str, value: Value, append: bool) -> Result<(), String> {
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
        Value::Integer(number) => number.to_string(),
        Value::Boolean(flag) => flag.to_string(),
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
}
