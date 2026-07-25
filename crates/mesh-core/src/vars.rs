//! The variable store.
//!
//! A session-global scope of typed scalar and collection variables, plus a stack of
//! **function-local** scopes pushed for the duration of a `func` call. Reads
//! resolve the innermost local scope, then the global scope — a callee never
//! sees its caller's locals (lexical, not dynamic). Writes land in the innermost
//! scope (a function-local when one is active, else the global). The read-only
//! `$sh` namespace holds the invocation entries `$sh.name` and `$sh.args`;
//! `export` and the rest of the `$sh.*` surface are deferred to later tasks —
//! see `DESIGN.md` §"Variables and assignment".

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    String(String),
    Integer(i64),
    Boolean(bool),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
    Regex(RegexValue),
    Glob(String),
    /// An anonymous `func(params) { … }` bound to a name — value-called through
    /// the variable (`$double(5)`). It has no byte form, so unlike every other
    /// value it cannot reach a command argument or an interpolation.
    Function(FuncValue),
}

/// A function value: the signature and body of a `func(params) { … }` lambda.
///
/// Shared behind an `Arc` because binding or passing one copies the `Value` and a
/// body is a whole parsed `Source`; atomic rather than an `Rc` because the
/// interactive completer holds shell values across a thread boundary.
///
/// Identity, not structure, is what equality means here — two separately written
/// lambdas with the same text are different functions, the answer every language
/// with first-class functions gives — which also keeps `Hash` cheap and
/// consistent with `Eq`.
#[derive(Debug, Clone)]
pub struct FuncValue(Arc<Lambda>);

#[derive(Debug)]
struct Lambda {
    params: Vec<crate::parser::Param>,
    body: crate::parser::Source,
}

impl FuncValue {
    pub fn new(params: Vec<crate::parser::Param>, body: crate::parser::Source) -> Self {
        Self(Arc::new(Lambda { params, body }))
    }

    pub fn params(&self) -> &[crate::parser::Param] {
        &self.0.params
    }

    pub fn body(&self) -> &crate::parser::Source {
        &self.0.body
    }
}

impl PartialEq for FuncValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for FuncValue {}

impl std::hash::Hash for FuncValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegexValue {
    pub pattern: String,
    pub case_insensitive: bool,
    pub multi_line: bool,
    pub dot_matches_new_line: bool,
    pub ignore_whitespace: bool,
}

impl RegexValue {
    pub fn new(pattern: String) -> Self {
        Self {
            pattern,
            case_insensitive: false,
            multi_line: false,
            dot_matches_new_line: false,
            ignore_whitespace: false,
        }
    }
}

type Scope = HashMap<String, Value>;

/// Names the shell owns rather than the user: `$env` is the process
/// environment, `$sh` is the shell's own state. Neither can be bound by an
/// assignment, a function parameter, or a pattern.
pub fn is_reserved_namespace(name: &str) -> bool {
    matches!(name, "env" | "sh")
}

/// The shell-or-script name when nothing named it — bash's `$0` for an
/// interactive shell.
const DEFAULT_SHELL_NAME: &str = "mesh";

/// Variable bindings: one session-global scope plus a stack of function-local
/// scopes (one per active `func` call), alongside the read-only `$sh` namespace.
pub struct Vars {
    global: Scope,
    locals: Vec<Scope>,
    shell: Vec<(String, Value)>,
}

impl Default for Vars {
    fn default() -> Self {
        Self {
            global: Scope::new(),
            locals: Vec::new(),
            shell: invocation_entries(DEFAULT_SHELL_NAME.to_owned(), Vec::new()),
        }
    }
}

/// The `$sh` entries that describe how mesh was invoked. Only these exist today;
/// the rest of the `$sh.*` surface in `DESIGN.md` is deferred.
fn invocation_entries(name: String, args: Vec<String>) -> Vec<(String, Value)> {
    vec![
        ("name".to_owned(), Value::String(name)),
        (
            "args".to_owned(),
            Value::List(args.into_iter().map(Value::String).collect()),
        ),
    ]
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record how mesh was invoked: `$sh.name` is the script or shell name and
    /// `$sh.args` the positional arguments, as a real list.
    pub fn set_invocation(&mut self, name: String, args: Vec<String>) {
        self.shell = invocation_entries(name, args);
    }

    /// The read-only `$sh` namespace as an ordered map, so member access,
    /// indexing, and modifiers all work through the usual map and list paths.
    pub(crate) fn shell_namespace(&self) -> Value {
        Value::Map(self.shell.clone())
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

    pub(crate) fn active_snapshot(&self) -> Scope {
        self.locals
            .last()
            .cloned()
            .unwrap_or_else(|| self.global.clone())
    }

    pub(crate) fn restore_active(&mut self, snapshot: Scope) {
        if let Some(scope) = self.locals.last_mut() {
            *scope = snapshot;
        } else {
            self.global = snapshot;
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

    /// Iterate over bindings visible in the current scope, with locals taking
    /// precedence over globals of the same name.
    pub(crate) fn visible(&self) -> impl Iterator<Item = (&str, &Value)> {
        let local = self.locals.last();
        local
            .into_iter()
            .flat_map(|scope| scope.iter().map(|(name, value)| (name.as_str(), value)))
            .chain(self.global.iter().filter_map(move |(name, value)| {
                (!local.is_some_and(|scope| scope.contains_key(name)))
                    .then_some((name.as_str(), value))
            }))
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
            (Value::Integer(left), Value::Integer(right)) => {
                *left = left
                    .checked_add(right)
                    .ok_or_else(|| format!("{name}: numeric overflow"))?;
            }
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
            (Value::String(_), _) => {
                return Err(format!("{name}: can only append a string to a string"));
            }
            (Value::Integer(_), _) => {
                return Err(format!("{name}: can only add an integer to an integer"));
            }
            (Value::Boolean(_), _) => return Err(format!("{name}: cannot append to a boolean")),
            (Value::Map(_), _) => return Err(format!("{name}: can only merge a map into a map")),
            (Value::Regex(_), _) => return Err(format!("{name}: cannot append to a regex")),
            (Value::Glob(_), _) => return Err(format!("{name}: cannot append to a glob")),
            (Value::Function(_), _) => {
                return Err(format!("{name}: cannot append to a function value"));
            }
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
