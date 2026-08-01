//! The hook registry — one store, addressed two ways.
//!
//! `DESIGN.md` §"Hooks and the prompt" specifies hooks as insertion-ordered maps
//! of named callables (`$sh.preprompt.git = …`); the `on` builtin is the terse
//! spelling of the same thing. [`docs/HOOKS.md`](../../../docs/HOOKS.md) D4 is
//! the decision this implements: **one authority, `on` is sugar**. A hook
//! registered by `on` is in the map, one written through the map is removable by
//! `on --remove`, and both are what dispatch runs — there is no second copy to
//! drift.
//!
//! The store lives here rather than on the shell so that the read path can reach
//! it: `$sh` is resolved deep in expansion, which holds only
//! [`Vars`](crate::vars::Vars). That is the same reason `$sh.options` lives in
//! [`crate::options`], and it is a plumbing choice — the map `$sh.<event>` reads
//! is rebuilt from this vector on each access, never written to directly.
//!
//! A `Vec` rather than a map per event because the key is the *pair*
//! `(event, name)` and order is registration order, which a flat vector gives
//! for free; the per-event map a read sees is built by filtering it.

use crate::vars::Value;

/// One point in the shell's lifecycle a handler can be attached to.
///
/// A closed set: an unknown word is a typo rather than a new event, and
/// [`parse`](HookEvent::parse) is what says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    PrePrompt,
    PreExec,
    PostExec,
    PreCd,
    PostCd,
    JobDone,
    Exit,
}

impl HookEvent {
    /// Every event, in the order `$sh` lists their maps — lifecycle order, so
    /// the namespace reads as the sequence a session actually runs through
    /// rather than alphabetically.
    pub const ALL: [HookEvent; 7] = [
        HookEvent::PrePrompt,
        HookEvent::PreExec,
        HookEvent::PostExec,
        HookEvent::PreCd,
        HookEvent::PostCd,
        HookEvent::JobDone,
        HookEvent::Exit,
    ];

    /// The word this event is spelled with — `on <key>` and `$sh.<key>` alike,
    /// which is what makes the two surfaces one namespace.
    pub fn key(self) -> &'static str {
        match self {
            HookEvent::PrePrompt => "preprompt",
            HookEvent::PreExec => "preexec",
            HookEvent::PostExec => "postexec",
            HookEvent::PreCd => "precd",
            HookEvent::PostCd => "postcd",
            HookEvent::JobDone => "jobdone",
            HookEvent::Exit => "exit",
        }
    }

    /// The event a word names, or `None` for one that is not an event.
    pub fn parse(name: &str) -> Option<Self> {
        HookEvent::ALL.into_iter().find(|event| event.key() == name)
    }
}

/// One registered handler. `(event, name)` is the key; `function` is resolved at
/// **dispatch**, by name, so redefining the function changes what the hook runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub event: HookEvent,
    pub name: String,
    pub function: String,
}

/// Every hook the session has, in registration order.
///
/// The single authority behind both `on` and `$sh.<event>`.
#[derive(Default, Debug)]
pub struct Hooks {
    hooks: Vec<Hook>,
}

impl Hooks {
    /// Attach `function` to `event` under `name`, **replacing in place** if that
    /// pair is already registered.
    ///
    /// Replacing rather than appending is what makes re-sourcing an rc file
    /// safe — it is the bash `PROMPT_COMMAND` stacking bug the keying exists to
    /// prevent — and in place rather than remove-then-push so a re-registration
    /// keeps its position in the run order.
    pub fn register(&mut self, event: HookEvent, name: &str, function: &str) {
        if let Some(hook) = self
            .hooks
            .iter_mut()
            .find(|hook| hook.event == event && hook.name == name)
        {
            hook.function = function.to_owned();
        } else {
            self.hooks.push(Hook {
                event,
                name: name.to_owned(),
                function: function.to_owned(),
            });
        }
    }

    /// Drop the handler `name` is keyed by for `event`. Reports whether there
    /// was one, which `unset` needs and `on --remove` does not.
    pub fn remove(&mut self, event: HookEvent, name: &str) -> bool {
        let before = self.hooks.len();
        self.hooks
            .retain(|hook| hook.event != event || hook.name != name);
        self.hooks.len() != before
    }

    /// Is anything attached to `event`? Asked before the work of building a
    /// dispatch's arguments, which for `precd` means a `pwd`.
    pub fn any(&self, event: HookEvent) -> bool {
        self.hooks.iter().any(|hook| hook.event == event)
    }

    /// The functions to run for `event`, in registration order.
    ///
    /// Cloned names rather than borrows because a handler runs with the shell
    /// mutably borrowed, and one handler may register another.
    pub fn functions(&self, event: HookEvent) -> Vec<String> {
        self.hooks
            .iter()
            .filter(|hook| hook.event == event)
            .map(|hook| hook.function.clone())
            .collect()
    }

    /// `$sh.<event>` as map entries — the read view, rebuilt per access.
    ///
    /// Handler values are strings because that is what a handler *is* today; see
    /// `docs/HOOKS.md` D1 for the callable-value question, which widens this
    /// without changing the map's shape.
    pub(crate) fn entries(&self, event: HookEvent) -> Vec<(String, Value)> {
        self.hooks
            .iter()
            .filter(|hook| hook.event == event)
            .map(|hook| (hook.name.clone(), Value::String(hook.function.clone())))
            .collect()
    }

    /// Every hook, for the tests that assert the whole store at once.
    #[cfg(test)]
    pub fn all(&self) -> &[Hook] {
        &self.hooks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_parses_from_the_word_it_is_spelled_with() {
        for event in HookEvent::ALL {
            assert_eq!(
                HookEvent::parse(event.key()),
                Some(event),
                "{}",
                event.key()
            );
        }
        assert_eq!(HookEvent::parse("preprompts"), None);
        assert_eq!(HookEvent::parse(""), None);
    }

    #[test]
    fn re_registering_a_name_replaces_it_in_place() {
        let mut hooks = Hooks::default();
        hooks.register(HookEvent::PrePrompt, "git", "first");
        hooks.register(HookEvent::PrePrompt, "jobs", "second");
        hooks.register(HookEvent::PrePrompt, "git", "third");
        // Replaced, not stacked — and still first, so the run order a config
        // established survives re-sourcing it.
        assert_eq!(
            hooks.functions(HookEvent::PrePrompt),
            vec!["third".to_owned(), "second".to_owned()]
        );
    }

    #[test]
    fn the_key_is_the_event_and_name_together() {
        let mut hooks = Hooks::default();
        hooks.register(HookEvent::Exit, "save", "save-history");
        hooks.register(HookEvent::PrePrompt, "save", "redraw");
        assert_eq!(hooks.functions(HookEvent::Exit), vec!["save-history"]);
        assert_eq!(hooks.functions(HookEvent::PrePrompt), vec!["redraw"]);
        assert!(hooks.remove(HookEvent::Exit, "save"));
        // The same name under another event is untouched.
        assert!(hooks.functions(HookEvent::Exit).is_empty());
        assert_eq!(hooks.functions(HookEvent::PrePrompt), vec!["redraw"]);
    }

    #[test]
    fn removing_reports_whether_there_was_one() {
        let mut hooks = Hooks::default();
        hooks.register(HookEvent::PostCd, "trace", "arrived");
        assert!(hooks.remove(HookEvent::PostCd, "trace"));
        assert!(!hooks.remove(HookEvent::PostCd, "trace"));
    }

    #[test]
    fn entries_read_as_the_map_in_registration_order() {
        let mut hooks = Hooks::default();
        hooks.register(HookEvent::Exit, "b", "second");
        hooks.register(HookEvent::PrePrompt, "elsewhere", "other");
        hooks.register(HookEvent::Exit, "a", "first");
        assert_eq!(
            hooks.entries(HookEvent::Exit),
            vec![
                ("b".to_owned(), Value::String("second".to_owned())),
                ("a".to_owned(), Value::String("first".to_owned())),
            ]
        );
        assert!(hooks.entries(HookEvent::JobDone).is_empty());
    }

    #[test]
    fn any_matches_what_functions_would_find() {
        let mut hooks = Hooks::default();
        assert!(!hooks.any(HookEvent::PreExec));
        hooks.register(HookEvent::PreExec, "log", "record");
        assert!(hooks.any(HookEvent::PreExec));
        assert!(!hooks.any(HookEvent::PostExec));
    }
}
