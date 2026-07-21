//! The variable store.
//!
//! Two-level lexical scope per `DESIGN.md` §"Variables and assignment": a
//! **session-global** scope plus a fresh **function-local** scope per `func`
//! call. Assignment binds in the current (innermost) scope; a read resolves
//! outward, local → global. Lists/maps as values, `export`, `global`/`unset`,
//! and the `$sh.*` surface are still deferred.

use std::collections::HashMap;

/// A stack of scopes. `scopes[0]` is the session-global scope; each active
/// function call pushes another scope on top.
pub struct Vars {
    scopes: Vec<HashMap<String, String>>,
}

impl Default for Vars {
    fn default() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `name` to `value` in the current (innermost) scope. Inside a function
    /// this creates a **local** by default, shadowing any global of the same
    /// name; at top level it binds the global.
    pub fn set(&mut self, name: &str, value: String) {
        self.scopes
            .last_mut()
            .expect("at least the global scope")
            .insert(name.to_string(), value);
    }

    /// Read `name` with **lexical** two-level scope: the current (innermost)
    /// scope, then the global scope. A function's *caller's* locals are **not**
    /// visible — scoping is lexical, not dynamic (the classic shell footgun).
    /// Returns `None` if unbound in both, which the caller turns into a loud
    /// error per the no-null / fail-loud rule.
    pub fn get(&self, name: &str) -> Option<&str> {
        let current = self.scopes.last().expect("at least the global scope");
        current
            .get(name)
            .or_else(|| self.scopes[0].get(name))
            .map(String::as_str)
    }

    /// Enter a fresh function-local scope.
    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Leave the innermost scope (dropping its locals). The global scope is never
    /// popped.
    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }
}
