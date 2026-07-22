//! The variable store.
//!
//! A simple first cut: one flat session-global scope of string and list
//! variables. Maps, function-local scopes, `export`, and the
//! `$sh.*` surface are deferred to later tasks — see `DESIGN.md` §"Variables and
//! assignment".

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(String),
    List(Vec<String>),
}

/// Session-global variable bindings.
#[derive(Default)]
pub struct Vars {
    map: HashMap<String, Value>,
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `name` to `value`, creating or replacing it.
    pub fn set(&mut self, name: &str, value: String) {
        self.map.insert(name.to_string(), Value::String(value));
    }

    /// Bind `name` to a list, preserving its arity (including an empty list).
    pub fn set_list(&mut self, name: &str, value: Vec<String>) {
        self.map.insert(name.to_string(), Value::List(value));
    }

    /// Bind an already typed value without converting lists to strings.
    pub fn set_value(&mut self, name: &str, value: Value) {
        self.map.insert(name.to_string(), value);
    }

    /// Read `name`. Returns `None` if unbound — the caller turns that into a
    /// loud error, per the no-null / fail-loud rule.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.map.get(name)
    }

    /// Append `value` according to the current string/list value rules.
    pub fn append(&mut self, name: &str, value: Value) -> Result<(), String> {
        let current = self
            .map
            .get_mut(name)
            .ok_or_else(|| format!("{name}: unbound variable"))?;
        match (current, value) {
            (Value::String(left), Value::String(right)) => left.push_str(&right),
            (Value::List(left), Value::List(mut right)) => left.append(&mut right),
            (Value::List(left), Value::String(right)) => left.push(right),
            (Value::String(_), Value::List(_)) => {
                return Err(format!("{name}: cannot append a list to a string"));
            }
        }
        Ok(())
    }
}
