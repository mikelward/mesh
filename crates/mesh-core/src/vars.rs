//! The variable store.
//!
//! A simple first cut: one flat session-global scope of string-valued
//! variables. Lists/maps as values, function-local scopes, `export`, and the
//! `$sh.*` surface are deferred to later tasks — see `DESIGN.md` §"Variables and
//! assignment".

use std::collections::HashMap;

/// Session-global variable bindings (name → string value).
#[derive(Default)]
pub struct Vars {
    map: HashMap<String, String>,
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `name` to `value`, creating or replacing it.
    pub fn set(&mut self, name: &str, value: String) {
        self.map.insert(name.to_string(), value);
    }

    /// Read `name`. Returns `None` if unbound — the caller turns that into a
    /// loud error, per the no-null / fail-loud rule.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.map.get(name).map(String::as_str)
    }
}
