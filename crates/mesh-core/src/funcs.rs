//! The function store.
//!
//! A `func name(params) { body }` definition binds a named callable. A signature
//! carries the four roles from `DESIGN.md` §"Functions": required and optional
//! positionals, boolean/valued `--flags`, and a `...rest` parameter. Function
//! bodies are stored as parsed sources and executed recursively on each call.

use std::collections::HashMap;

use crate::parser::{Param, ParamKind, ReturnType, Source};
use crate::vars::Value;

/// A defined function: its parameters and parsed body.
pub struct FuncDef {
    pub params: Vec<Param>,
    pub body: Source,
    /// What the definition's `with (…)` list named, read where the definition ran
    /// and copied in. Bound into the fresh call scope beside the parameters, so a
    /// body sees the value the *definition* saw rather than whatever the name holds
    /// when the call happens — which is what lets a definition inside a loop bake
    /// that pass's value. Empty for a definition with no list, which is every
    /// definition that predates one.
    pub captures: Vec<(String, Value)>,
    /// Defined as `wrapper func` — it parses no flags of its own, so every
    /// argument reaches its positionals and `...rest` verbatim and `--help` is
    /// forwarded rather than answered here.
    pub wrapper: bool,
    /// The **subject** of a modifier declaration — the `_s` of `func _s:foo()`,
    /// the `..._xs` of `func ..._xs:join(_sep)`. `None` for an ordinary `func`.
    ///
    /// Not a positional: it arrives through the call site's `$x:` rather than
    /// through the parens, which is what lets a list-taking modifier still take
    /// arguments without colliding with rest-must-be-last. Its kind is the whole
    /// of the element-wise rule — `Required` receives one element, `Rest` the
    /// collection (`DESIGN.md` §"Modifiers").
    pub subject: Option<Param>,
    /// The declared return type — the `int` of `int func f() { … }`. `None` for a
    /// `func` that declares none, which is the meaningful absence: it says the
    /// function has no value channel rather than that its type is unknown.
    pub return_type: Option<ReturnType>,
}

impl FuncDef {
    /// Whether the signature declares its own `--help` flag (switch or valued). A
    /// function that claims `--help` keeps it: the canned help never overrides the
    /// function's own contract (`DESIGN.md` §"Command resolution and help").
    pub fn declares_help(&self) -> bool {
        self.params.iter().any(|param| {
            param.name == "help" && matches!(param.kind, ParamKind::Switch | ParamKind::Flag(_))
        })
    }

    /// Produce help in the same conventional shape used by builtin commands: a
    /// `Usage:` line with positionals (optionals bracketed, a rest `...`), an
    /// `Arguments:` list, and an `Options:` block of the declared flags plus a
    /// synthesized `--help` (unless the signature declares its own).
    ///
    /// A wrapper gets no `Options:` block at all. It can declare no flags (the
    /// parser refuses that), and the synthesized `--help` would be a lie twice
    /// over: the token is forwarded rather than answered, and this text is what
    /// completion is built from, so `g --<Tab>` would offer a flag that goes
    /// straight to `...rest`.
    pub fn help(&self, name: &str) -> String {
        let mut usage = format!("Usage: {name}");
        let mut arguments = String::new();
        let mut options = String::new();
        for param in &self.params {
            let upper = param.name.to_uppercase();
            match &param.kind {
                ParamKind::Required => {
                    usage.push_str(&format!(" <{upper}>"));
                    arguments.push_str(&format!("  <{upper}>\n"));
                }
                ParamKind::Optional(_) => {
                    usage.push_str(&format!(" [<{upper}>]"));
                    arguments.push_str(&format!("  [<{upper}>]\n"));
                }
                ParamKind::Rest => {
                    usage.push_str(&format!(" [<{upper}>...]"));
                    arguments.push_str(&format!("  [<{upper}>...]\n"));
                }
                ParamKind::Switch => {
                    options.push_str(&format!("  --{}\n", param.name));
                }
                ParamKind::Flag(_) => {
                    options.push_str(&format!("  --{}=<{upper}>\n", param.name));
                }
            }
        }
        let mut help = format!("{usage}\n");
        // The result is part of the signature a caller reads, and the whole point
        // of declaring it is that `help` can answer "what does this return?"
        // without anyone opening the body. A `func` that declares nothing prints
        // no line rather than "status": absence is what says there is no value
        // channel, and spelling it out here would read as a promise of one.
        if let Some(declared) = self.return_type {
            help.push_str(&format!("\nReturns: {}\n", declared.as_str()));
        }
        if !arguments.is_empty() {
            help.push_str("\nArguments:\n");
            help.push_str(&arguments);
        }
        if self.wrapper {
            // No literal flag in this line: completion is built from this text,
            // so naming one here would put it back in the candidate list.
            help.push_str("\nParses no flags; every argument is forwarded verbatim.\n");
            return help;
        }
        help.push_str("\nOptions:\n");
        help.push_str(&options);
        // A declared `--help` is already listed above; don't duplicate it.
        if !self.declares_help() {
            help.push_str("  --help  Print help\n");
        }
        help
    }
}

/// The session's defined functions (name → definition).
///
/// Modifiers live in their own map rather than in `map` under a mangled name:
/// they are a **separate vocabulary**, so `func upper() { tr a-z A-Z }` and a
/// built-in `:upper` do not collide, and `$x:f` never finds an ordinary `func f`
/// (`DESIGN.md` §"Modifiers").
#[derive(Default)]
pub struct Funcs {
    map: HashMap<String, FuncDef>,
    modifiers: HashMap<String, FuncDef>,
}

impl Funcs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Define (or redefine) a function.
    pub fn define(&mut self, name: String, def: FuncDef) {
        self.map.insert(name, def);
    }

    /// Look up a function by name.
    pub fn get(&self, name: &str) -> Option<&FuncDef> {
        self.map.get(name)
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    /// Define (or redefine) a declared modifier, `func _s:name()`.
    pub fn define_modifier(&mut self, name: String, def: FuncDef) {
        self.modifiers.insert(name, def);
    }

    /// Look up a declared modifier by name.
    ///
    /// Resolved **per application**, never cached at the point a chain is written:
    /// a later declaration changes what an already-written `$x:foo` runs, exactly
    /// as redefining a `func` changes what an already-written call runs
    /// (`DESIGN.md` §"Modifiers").
    pub fn modifier(&self, name: &str) -> Option<&FuncDef> {
        self.modifiers.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::CompletionSpec;
    use crate::parser::Source;

    fn typed_def(params: Vec<Param>, declared: Option<ReturnType>) -> FuncDef {
        FuncDef {
            return_type: declared,
            ..def(params, false)
        }
    }

    fn def(params: Vec<Param>, wrapper: bool) -> FuncDef {
        FuncDef {
            params,
            body: Source {
                statements: Vec::new(),
                span: 0..0,
            },
            captures: Vec::new(),
            wrapper,
            subject: None,
            return_type: None,
        }
    }

    fn param(name: &str, kind: ParamKind) -> Param {
        Param {
            name: name.into(),
            kind,
        }
    }

    #[test]
    fn help_names_a_declared_return_and_stays_silent_without_one() {
        // The result is part of the signature a caller reads — the whole point of
        // declaring it is that `help` answers "what does this return?" without
        // anyone opening the body.
        let typed = typed_def(vec![param("a", ParamKind::Required)], Some(ReturnType::Int));
        assert!(
            typed.help("f").contains("Returns: int"),
            "{}",
            typed.help("f")
        );

        let hatch = typed_def(Vec::new(), Some(ReturnType::Value));
        assert!(
            hatch.help("f").contains("Returns: value"),
            "{}",
            hatch.help("f")
        );

        // And **no** line without a declaration: absence is what says there is no
        // value channel, so printing `status` here would advertise one the
        // function has not got.
        let untyped = typed_def(vec![param("a", ParamKind::Required)], None);
        assert!(
            !untyped.help("f").contains("Returns"),
            "{}",
            untyped.help("f")
        );
    }

    #[test]
    fn a_plain_func_advertises_a_generated_help_flag() {
        let help = def(vec![param("xs", ParamKind::Rest)], false).help("g");
        assert!(help.contains("--help"), "{help}");
        assert_eq!(CompletionSpec::from_help(&help).matching("--"), ["--help"]);
    }

    #[test]
    fn a_wrapper_advertises_no_options_at_all() {
        // The help text is what completion is built from, so a synthesized
        // `--help` here would make `g --<Tab>` offer a flag that the call
        // forwards to `...rest` instead of answering.
        let help = def(vec![param("xs", ParamKind::Rest)], true).help("g");
        assert!(!help.contains("--help"), "{help}");
        assert!(!help.contains("Options:"), "{help}");
        assert!(help.contains("Parses no flags"), "{help}");
        assert!(
            CompletionSpec::from_help(&help).matching("--").is_empty(),
            "{help}"
        );
    }

    #[test]
    fn a_wrapper_still_documents_its_positionals() {
        let help = def(
            vec![
                param("first", ParamKind::Required),
                param("xs", ParamKind::Rest),
            ],
            true,
        )
        .help("g");
        assert!(help.contains("Usage: g <FIRST> [<XS>...]"), "{help}");
        assert!(help.contains("Arguments:"), "{help}");
    }
}
