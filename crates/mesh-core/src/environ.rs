//! The `$env` namespace — the boundary between mesh's typed values and the
//! process environment.
//!
//! The environment is a flat `KEY=bytes` table, so only byte-strings cross it.
//! The one exception is the **path-type** variables (`PATH` and friends), which
//! are lists in mesh and are `:`-joined on the way out and split on the way in —
//! a defined serialization for known names, not a general "lists become strings"
//! rule. See `DESIGN.md` §"Variables and assignment".

use std::env;

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
        match combine(key, value)? {
            Some(combined) => combined,
            None => return Ok(()),
        }
    } else {
        value
    };
    let text = serialize(key, value)?;
    if text.contains('\0') {
        return Err(format!(
            "$env.{key}: an environment value cannot contain a NUL byte"
        ));
    }
    // SAFETY: the shell runs its execution loop single-threaded, so mutating the
    // environment here races with nothing — the same reasoning `cd` relies on
    // when it updates `$env.PWD`.
    unsafe { env::set_var(key, text) };
    Ok(())
}

/// The value `key` should end up holding after `+=` — its current contents
/// followed by `addition`. A path-type name grows by elements, anything else by
/// string concatenation. `None` means "nothing to do".
fn combine(key: &str, addition: Value) -> Result<Option<Value>, String> {
    let current = read(key);
    if is_path_var(key) {
        let mut entries = match current {
            Some(Value::List(entries)) => entries,
            // Unset: `+=` starts the list rather than failing, since an absent
            // PATH and an empty one mean the same thing to a child.
            _ => Vec::new(),
        };
        match addition {
            Value::List(added) => entries.extend(added),
            scalar => entries.push(scalar),
        }
        return Ok(Some(Value::List(entries)));
    }
    let Some(Value::String(current)) = current else {
        return Ok(Some(addition));
    };
    let added = serialize(key, addition)?;
    Ok(Some(Value::String(format!("{current}{added}"))))
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
