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
    /// One of the shell's own streams (`$sh.stdin` and friends), carrying the
    /// descriptor it names. A handle has **no canonical byte form**, so it never
    /// renders to argv or into a string — `DESIGN.md` §"Values and the bytes
    /// boundary" lists it beside a regex for exactly that reason. The descriptor
    /// stays private: the value answers questions (`:tty`) rather than being one.
    Stream(i32),
    /// An anonymous `func(params) { … }` bound to a name — value-called through
    /// the variable (`$double(5)`). It has no byte form, so unlike every other
    /// value it cannot reach a command argument or an interpolation.
    Function(FuncValue),
}

/// A function value: something callable with one thing to do.
///
/// Shared behind an `Arc` because binding or passing one copies the `Value` and a
/// lambda body is a whole parsed `Source`; atomic rather than an `Rc` because the
/// interactive completer holds shell values across a thread boundary.
///
/// Identity, not structure, is what equality means here — two separately written
/// lambdas with the same text are different functions, the answer every language
/// with first-class functions gives — which also keeps `Hash` cheap and
/// consistent with `Eq`. That extends to modifier references: `:stem` written
/// twice is two values, since they are two references rather than one shared one.
#[derive(Debug, Clone)]
pub struct FuncValue(Arc<Callable>);

#[derive(Debug)]
enum Callable {
    /// A written `func(params) { body }`.
    Lambda {
        params: Vec<crate::parser::Param>,
        body: crate::parser::Source,
    },
    /// A bare `:name` reference — the function that applies that modifier to its
    /// one argument. Held as the name rather than a synthesized lambda body: there
    /// is nothing to parse, and applying a modifier is a direct call.
    Modifier(String),
}

impl FuncValue {
    pub fn lambda(params: Vec<crate::parser::Param>, body: crate::parser::Source) -> Self {
        Self(Arc::new(Callable::Lambda { params, body }))
    }

    pub fn modifier(name: String) -> Self {
        Self(Arc::new(Callable::Modifier(name)))
    }

    /// The signature and body, when this is a written lambda.
    pub fn as_lambda(&self) -> Option<(&[crate::parser::Param], &crate::parser::Source)> {
        match &*self.0 {
            Callable::Lambda { params, body } => Some((params, body)),
            Callable::Modifier(_) => None,
        }
    }

    /// The modifier name, when this is a bare reference.
    pub fn modifier_name(&self) -> Option<&str> {
        match &*self.0 {
            Callable::Modifier(name) => Some(name),
            Callable::Lambda { .. } => None,
        }
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
    status: u8,
    stages: Vec<u8>,
    interactive: bool,
    /// Captured once rather than read per access, so they stay the *shell's* —
    /// bash's `$$` / `$PPID`, which do not change inside a subshell. A forked
    /// pipeline stage inherits this copy, so `$sh.pid` there still names the
    /// session rather than the short-lived stage.
    pid: u32,
    ppid: i32,
    /// The live job table as `$sh.jobs` reports it. A snapshot rather than a
    /// live borrow, because expansion is handed only the variable store — the
    /// shell refreshes it from the real table on the same funnel that publishes
    /// `$sh.status`, so a read never sees a stale one.
    jobs: Vec<(String, Value)>,
}

impl Default for Vars {
    fn default() -> Self {
        Self {
            global: Scope::new(),
            locals: Vec::new(),
            shell: invocation_entries(DEFAULT_SHELL_NAME.to_owned(), Vec::new()),
            status: 0,
            stages: vec![0],
            interactive: false,
            pid: std::process::id(),
            // SAFETY: `getppid` takes no arguments and cannot fail.
            ppid: unsafe { libc::getppid() },
            jobs: Vec::new(),
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

    /// Record the status the next command sees as `$sh.status`, and the
    /// per-stage breakdown `$sh.pipestatus` reports. Kept as one call because
    /// the two must agree: `$sh.pipestatus` always describes the run that
    /// produced the current `$sh.status`.
    pub(crate) fn set_status(&mut self, status: u8, stages: Vec<u8>) {
        self.status = status;
        self.stages = stages;
    }

    /// Record that this session is interactive, which `$sh.interactive` reports.
    /// Set by the shell rather than derived from `isatty`: the question is which
    /// loop is running, not what fd 0 happens to be — `mesh -s` on a terminal
    /// reads commands without being an interactive session.
    pub fn set_interactive(&mut self, interactive: bool) {
        self.interactive = interactive;
    }

    /// Publish the live jobs, keyed by job id in registration order.
    pub(crate) fn set_jobs(&mut self, jobs: Vec<(String, Value)>) {
        self.jobs = jobs;
    }

    /// The currently published `$sh.status`.
    pub(crate) fn status(&self) -> u8 {
        self.status
    }

    /// Take a copy of both runtime entries, to put back with
    /// [`Vars::restore_status`] after running something that is the shell's own
    /// bookkeeping rather than the user's command.
    pub(crate) fn status_snapshot(&self) -> (u8, Vec<u8>) {
        (self.status, self.stages.clone())
    }

    pub(crate) fn restore_status(&mut self, (status, stages): (u8, Vec<u8>)) {
        self.status = status;
        self.stages = stages;
    }

    /// The read-only `$sh` namespace as an ordered map, so member access,
    /// indexing, and modifiers all work through the usual map and list paths.
    ///
    /// The runtime entries are built here rather than stored in `shell`, so
    /// recording a status after every command is two field writes instead of a
    /// keyed update of the map.
    pub(crate) fn shell_namespace(&self) -> Value {
        let mut entries = vec![
            ("status".to_owned(), Value::Integer(i64::from(self.status))),
            (
                "pipestatus".to_owned(),
                Value::List(
                    self.stages
                        .iter()
                        .map(|code| Value::Integer(i64::from(*code)))
                        .collect(),
                ),
            ),
            ("pid".to_owned(), Value::Integer(i64::from(self.pid))),
            ("ppid".to_owned(), Value::Integer(i64::from(self.ppid))),
            (
                "version".to_owned(),
                Value::String(env!("CARGO_PKG_VERSION").to_owned()),
            ),
            ("interactive".to_owned(), Value::Boolean(self.interactive)),
            // Handles rather than integers: `DESIGN.md` puts a stream handle in
            // the same row as a regex — no byte form — so `puts $sh.stdin` is a
            // loud error and `:tty` is the way to ask about one.
            ("stdin".to_owned(), Value::Stream(0)),
            ("stdout".to_owned(), Value::Stream(1)),
            ("stderr".to_owned(), Value::Stream(2)),
            ("jobs".to_owned(), Value::Map(self.jobs.clone())),
        ];
        entries.extend(self.shell.iter().cloned());
        Value::Map(entries)
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

    /// Bind in the **session-global** scope, whatever scope is active — what
    /// `global name = value` says explicitly, since a plain assignment inside a
    /// function is local by default (`DESIGN.md` §"Scope — two levels").
    pub fn set_value_global(&mut self, name: &str, value: Value) {
        self.global.insert(name.to_string(), value);
    }

    /// Remove `name` from the active scope, reporting whether it was bound
    /// there. Inside a function this drops the local only: a global it was
    /// shadowing becomes visible again, because plain `unset` never reaches
    /// through to a global — the same rule that makes assignment local.
    pub fn unset(&mut self, name: &str) -> bool {
        self.active_mut().remove(name).is_some()
    }

    /// Remove `name` from the session-global scope — `global unset name`,
    /// symmetric with `global name = value`.
    pub fn unset_global(&mut self, name: &str) -> bool {
        self.global.remove(name).is_some()
    }

    /// Is `name` bound in any visible scope? A read errors only when it is
    /// bound in none, so `unset` uses this to tell "removed" from "never there".
    pub fn is_bound(&self, name: &str) -> bool {
        self.get(name).is_some()
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
    /// `global name += value`: append in the session-global scope. Unlike
    /// [`Vars::append`] there is no seeding step — the global scope *is* the
    /// target, so an unbound name is simply an error rather than something to
    /// copy inward.
    pub fn append_global(&mut self, name: &str, value: Value) -> Result<(), String> {
        if !self.global.contains_key(name) {
            return Err(format!("{name}: unbound variable"));
        }
        let saved = std::mem::take(&mut self.locals);
        let result = self.append(name, value);
        self.locals = saved;
        result
    }

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
            (Value::Stream(_), _) => {
                return Err(format!("{name}: cannot append to a stream handle"));
            }
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
    fn interactive_is_reported_by_the_shell_not_inferred() {
        // The `true` case needs the interactive loop, which needs a terminal, so
        // the flag-to-namespace plumbing is checked here and the CLI tests cover
        // every path that must report `false`.
        let mut vars = Vars::new();
        let read = |vars: &Vars| match vars.shell_namespace() {
            Value::Map(entries) => entries
                .iter()
                .find(|(key, _)| key == "interactive")
                .map(|(_, value)| value.clone())
                .expect("$sh.interactive"),
            other => panic!("$sh should be a map, got {other:?}"),
        };
        assert_eq!(read(&vars), Value::Boolean(false));
        vars.set_interactive(true);
        assert_eq!(read(&vars), Value::Boolean(true));
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
