//! The variable store.
//!
//! A session-global scope of string and list variables, plus a stack of
//! **function-local** scopes pushed for the duration of a `func` call. Reads
//! resolve the innermost local scope, then the global scope — a callee never
//! sees its caller's locals (lexical, not dynamic). Writes land in the innermost
//! scope (a function-local when one is active, else the global). Maps, `export`,
//! and the `$sh.*` surface are deferred to later tasks — see `DESIGN.md`
//! §"Variables and assignment".

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    String(String),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
}

type Scope = HashMap<String, Value>;

/// Variable bindings: one session-global scope plus a stack of function-local
/// scopes (one per active `func` call).
#[derive(Default)]
pub struct Vars {
    global: Scope,
    locals: Vec<Scope>,
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enter a fresh function-local scope; balanced by [`pop_scope`].
    pub fn push_scope(&mut self) {
        self.locals.push(Scope::new());
    }

    /// Leave the innermost function-local scope, discarding its bindings.
    pub fn pop_scope(&mut self) {
        self.locals.pop();
    }

    /// The scope writes land in: the innermost function-local if one is active,
    /// else the global scope.
    fn active_mut(&mut self) -> &mut Scope {
        if self.locals.is_empty() {
            &mut self.global
        } else {
            self.locals.last_mut().unwrap()
        }
    }

    /// Does the active scope already hold `name`? (Only the innermost local when
    /// one is active, else the global — never an outer scope.)
    fn active_has(&self, name: &str) -> bool {
        if let Some(scope) = self.locals.last() {
            scope.contains_key(name)
        } else {
            self.global.contains_key(name)
        }
    }

    /// Bind `name` to `value`, creating or replacing it in the active scope.
    #[cfg(test)]
    pub fn set(&mut self, name: &str, value: String) {
        self.active_mut()
            .insert(name.to_string(), Value::String(value));
    }

    /// Bind an already typed value without converting lists to strings.
    pub fn set_value(&mut self, name: &str, value: Value) {
        self.active_mut().insert(name.to_string(), value);
    }

    /// Read `name`: the innermost function-local binding, else the global one.
    /// Returns `None` if unbound — the caller turns that into a loud error, per
    /// the no-null / fail-loud rule.
    pub fn get(&self, name: &str) -> Option<&Value> {
        if let Some(scope) = self.locals.last()
            && let Some(value) = scope.get(name)
        {
            return Some(value);
        }
        self.global.get(name)
    }

    /// Append `value` according to the current string/list value rules.
    ///
    /// Append is an assignment, and assignment is **local-by-default**: inside a
    /// function it must create or modify a local, never reach out and mutate an
    /// outer (global) binding (`DESIGN.md` §"Scope — two levels"). So if the
    /// active scope does not already hold `name`, the visible value (resolved
    /// outward: local → global) is copied into the active scope first, then
    /// appended there — leaving any shadowed global untouched. At top level the
    /// active scope *is* the global, so this stays an in-place append there.
    pub fn append(&mut self, name: &str, value: Value) -> Result<(), String> {
        if !self.active_has(name) {
            let seed = self
                .get(name)
                .cloned()
                .ok_or_else(|| format!("{name}: unbound variable"))?;
            self.active_mut().insert(name.to_string(), seed);
        }
        let current = self.active_mut().get_mut(name).expect("seeded above");
        match (current, value) {
            (Value::String(left), Value::String(right)) => left.push_str(&right),
            (Value::List(left), Value::List(mut right)) => left.append(&mut right),
            (Value::List(left), right) => left.push(right),
            (Value::Map(left), Value::Map(right)) => {
                for (key, value) in right {
                    if let Some((_, old)) = left.iter_mut().find(|(old, _)| old == &key) {
                        *old = value;
                    } else {
                        left.push((key, value));
                    }
                }
            }
            (Value::String(_), Value::List(_) | Value::Map(_)) => {
                return Err(format!("{name}: cannot append a list to a string"));
            }
            (Value::Map(_), _) => return Err(format!("{name}: can only merge a map into a map")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, Vars};

    #[test]
    fn a_local_shadows_the_global_and_is_dropped_on_pop() {
        let mut vars = Vars::new();
        vars.set("x", "global".into());
        vars.push_scope();
        vars.set("x", "local".into());
        assert_eq!(vars.get("x"), Some(&Value::String("local".into())));
        vars.pop_scope();
        assert_eq!(vars.get("x"), Some(&Value::String("global".into())));
    }

    #[test]
    fn a_read_falls_through_to_the_global() {
        let mut vars = Vars::new();
        vars.set("g", "seen".into());
        vars.push_scope();
        // A name not bound locally resolves against the global scope.
        assert_eq!(vars.get("g"), Some(&Value::String("seen".into())));
    }

    #[test]
    fn a_callee_does_not_see_a_callers_local() {
        // Two nested scopes: only the innermost local plus the global are visible,
        // so a name bound in an outer (caller) scope is invisible to the callee.
        let mut vars = Vars::new();
        vars.push_scope();
        vars.set("caller-only", "x".into());
        vars.push_scope();
        assert_eq!(vars.get("caller-only"), None);
    }

    #[test]
    fn append_mutates_the_binding_in_place() {
        let mut vars = Vars::new();
        vars.set("s", "a".into());
        vars.append("s", Value::String("b".into())).unwrap();
        assert_eq!(vars.get("s"), Some(&Value::String("ab".into())));
    }

    #[test]
    fn append_in_a_function_shadows_rather_than_clobbers_a_global() {
        // `+=` on a global-only name inside a function must create a local from
        // the visible value, not mutate the global (local-by-default assignment).
        let mut vars = Vars::new();
        vars.set("g", "before".into());
        vars.push_scope();
        vars.append("g", Value::String("after".into())).unwrap();
        assert_eq!(vars.get("g"), Some(&Value::String("beforeafter".into())));
        vars.pop_scope();
        assert_eq!(vars.get("g"), Some(&Value::String("before".into())));
    }
}
