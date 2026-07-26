//! `$sh.options` — the shell's settings, and the first writable corner of `$sh`.
//!
//! `DESIGN.md` §"Variables and assignment" splits `$sh` in two. The **runtime**
//! entries (`$sh.status`, `$sh.jobs`, the stream handles, …) are the shell's
//! authoritative state and stay read-only, so config cannot corrupt an invariant.
//! The **configuration** entries are the user's to write, and this is the first
//! of them to exist.
//!
//! Every setting here governs an **interactive decoration** and is **on by
//! default**: mesh should be pleasant out of the box, and the setting is for the
//! terminal, multiplexer, or personal taste the decoration does not suit.
//! Turning one off never changes what a command *does*, only what the shell draws
//! around it — so nothing a script can observe depends on one.
//!
//! Bracketed paste is deliberately **not** here. `DESIGN.md` decides it "on,
//! always": with the guard off a pasted newline arrives as Enter, so every line
//! but the last runs before it can be read. That is data loss rather than a
//! decoration, and an option to ask for it would be a mistake to offer.
//!
//! The values live behind atomics in an `Arc` rather than in the variable store,
//! because the line editor holds a reader of its own: reedline is handed the
//! highlighter once when the session starts, so anything it consults has to be
//! *shared* with it rather than read back through [`Vars`](crate::vars::Vars) at
//! the moment of the write. That is also what makes a change take effect on the
//! next keystroke instead of the next session.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::vars::Value;

/// One `$sh.options` setting.
///
/// Declared in the order `$sh.options` lists them, which [`Options`] also indexes
/// by — see [`Opt::ALL`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Opt {
    /// `bold-input` — draw what is being typed in bold, so a command stays
    /// distinguishable from its own output once both are in scrollback.
    BoldInput,
    /// `command-notify` — the desktop notification a command raises when it has
    /// run for longer than the threshold. Off is for anyone who would rather not
    /// have a notification daemon told what they are running, or who works
    /// somewhere the notification lands on the wrong machine.
    CommandNotify,
    /// `cwd-report` — the `OSC 7` working-directory report, so a new tab or split
    /// opens where the shell is. Quiet in a terminal that ignores it, but it is
    /// one more sequence on the wire.
    CwdReport,
    /// `osc-title` — the window and tab title: where the shell is at the prompt,
    /// what it is running while a command runs. Off is for anyone whose terminal,
    /// multiplexer, or window manager sets the title itself and does not want it
    /// overwritten every prompt.
    OscTitle,
    /// `shell-integration` — the `OSC 133` semantic prompt marks, which let a
    /// terminal tell prompt from input from output. The half the shell owns (`C`
    /// and `D`); the line editor's `A` and `B` go with it, since half the marks
    /// is worse than none.
    ShellIntegration,
}

impl Opt {
    /// Every setting, in the order `$sh.options` lists them — alphabetical by
    /// key, so `$sh.options:keys` reads predictably.
    ///
    /// [`Options`] indexes its flags by an `Opt`'s position *here*, which is the
    /// enum's declaration order; `all_options_index_themselves` holds the two
    /// together.
    pub const ALL: [Opt; 5] = [
        Opt::BoldInput,
        Opt::CommandNotify,
        Opt::CwdReport,
        Opt::OscTitle,
        Opt::ShellIntegration,
    ];

    /// The key this setting is spelled with under `$sh.options` — kebab-case,
    /// like every other multi-word name in the language.
    pub fn key(self) -> &'static str {
        match self {
            Opt::BoldInput => "bold-input",
            Opt::CommandNotify => "command-notify",
            Opt::CwdReport => "cwd-report",
            Opt::OscTitle => "osc-title",
            Opt::ShellIntegration => "shell-integration",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Opt::ALL.into_iter().find(|option| option.key() == key)
    }
}

/// The live `$sh.options` values.
///
/// Shared: the shell reads and writes them through [`Vars`](crate::vars::Vars),
/// and the line editor holds a clone to consult while drawing. Atomics rather
/// than a lock because every access is one flag and neither side may block the
/// other mid-keystroke.
pub struct Options {
    /// One flag per [`Opt`], indexed by its position in [`Opt::ALL`].
    flags: [AtomicBool; Opt::ALL.len()],
}

impl Default for Options {
    fn default() -> Self {
        Self {
            // On out of the box, every one of them: the decoration is the point,
            // and the setting exists to decline it.
            flags: [const { AtomicBool::new(true) }; Opt::ALL.len()],
        }
    }
}

impl Options {
    /// Is `option` on?
    pub fn get(&self, option: Opt) -> bool {
        self.flags[option as usize].load(Ordering::Relaxed)
    }

    fn set(&self, option: Opt, on: bool) {
        self.flags[option as usize].store(on, Ordering::Relaxed);
    }

    /// `$sh.options` as map entries, for the namespace `$sh` resolves to.
    pub(crate) fn entries(&self) -> Vec<(String, Value)> {
        Opt::ALL
            .into_iter()
            .map(|option| (option.key().to_owned(), Value::Boolean(self.get(option))))
            .collect()
    }

    /// `$sh.options.KEY = value`, where `target` is how the user spelled the
    /// place — the prefix every member-assignment error carries.
    ///
    /// Strict on both halves, because a settings map that accepts a typo is a
    /// setting silently not applied. An unknown key is refused rather than added:
    /// unlike an ordinary map, `$sh.options` has a fixed set of keys, and a new
    /// one would configure nothing. A non-boolean is refused rather than
    /// coerced — `"false"` is a *string*, and a truthiness rule here would make
    /// it turn the setting on.
    pub(crate) fn assign(&self, target: &str, key: &str, value: &Value) -> Result<(), String> {
        let Some(option) = Opt::from_key(key) else {
            // "no `x` in this map" is what a *read* of the same place says; the
            // list follows because the whole set is short and a typo is the
            // likeliest reason to be here.
            return Err(format!(
                "$sh.options: no `{key}` in this map; the settings are {}",
                Opt::ALL.map(|option| option.key()).join(", ")
            ));
        };
        let Value::Boolean(on) = value else {
            return Err(format!(
                "{target}: a setting is `true` or `false`, got {}",
                crate::repl::value_kind(value)
            ));
        };
        self.set(option, *on);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`Options`] indexes its flags by `option as usize`, which is only the
    /// right slot while the enum is declared in [`Opt::ALL`]'s order. Reordering
    /// one without the other would silently cross the settings over.
    #[test]
    fn all_options_index_themselves() {
        for (index, option) in Opt::ALL.into_iter().enumerate() {
            assert_eq!(option as usize, index, "{}", option.key());
        }
    }

    #[test]
    fn every_setting_starts_on() {
        let options = Options::default();
        for option in Opt::ALL {
            assert!(options.get(option), "{} should default on", option.key());
        }
    }

    #[test]
    fn assigning_a_boolean_changes_only_that_setting() {
        let options = Options::default();
        options
            .assign(
                "$sh.options.bold-input",
                "bold-input",
                &Value::Boolean(false),
            )
            .expect("bold-input is a setting");
        assert!(!options.get(Opt::BoldInput));
        assert!(options.get(Opt::CwdReport));
        assert!(options.get(Opt::ShellIntegration));
        options
            .assign(
                "$sh.options.bold-input",
                "bold-input",
                &Value::Boolean(true),
            )
            .expect("and back on again");
        assert!(options.get(Opt::BoldInput));
    }

    #[test]
    fn an_unknown_setting_is_refused_rather_than_added() {
        let options = Options::default();
        let error = options
            .assign(
                "$sh.options.bold-imput",
                "bold-imput",
                &Value::Boolean(false),
            )
            .expect_err("a typo is not a new setting");
        assert!(error.contains("no `bold-imput`"), "{error}");
        // And it names what there is, since the whole set is short.
        assert!(error.contains("bold-input"), "{error}");
        assert!(options.entries().iter().all(|(key, _)| key != "bold-imput"));
    }

    #[test]
    fn a_non_boolean_is_refused_rather_than_coerced() {
        let options = Options::default();
        let error = options
            .assign(
                "$sh.options.bold-input",
                "bold-input",
                &Value::String("false".to_owned()),
            )
            .expect_err("a string is not a boolean");
        assert!(error.contains("`true` or `false`"), "{error}");
        assert!(error.contains("a string"), "{error}");
        // Refused means unchanged — not "off because the string was falsey", and
        // not "on because a non-empty string is truthy".
        assert!(options.get(Opt::BoldInput));
    }

    #[test]
    fn entries_list_every_setting_as_a_boolean() {
        let options = Options::default();
        options
            .assign(
                "$sh.options.cwd-report",
                "cwd-report",
                &Value::Boolean(false),
            )
            .expect("cwd-report is a setting");
        assert_eq!(
            options.entries(),
            vec![
                ("bold-input".to_owned(), Value::Boolean(true)),
                ("command-notify".to_owned(), Value::Boolean(true)),
                ("cwd-report".to_owned(), Value::Boolean(false)),
                ("osc-title".to_owned(), Value::Boolean(true)),
                ("shell-integration".to_owned(), Value::Boolean(true)),
            ]
        );
    }
}
