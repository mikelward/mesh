//! The function store.
//!
//! A `func name(params) { body }` definition binds a named callable. A signature
//! carries the four roles from `DESIGN.md` §"Functions": required and optional
//! positionals, boolean/valued `--flags`, and a `...rest` parameter. Function
//! bodies are stored as parsed sources and executed recursively on each call.

use std::collections::HashMap;

use crate::parser::{Param, ParamKind, Source};

/// A defined function: its parameters and parsed body.
pub struct FuncDef {
    pub params: Vec<Param>,
    pub body: Source,
}

impl FuncDef {
    /// Produce help in the same conventional shape used by builtin commands: a
    /// `Usage:` line with positionals (optionals bracketed, a rest `...`), an
    /// `Arguments:` list, and an `Options:` block of the declared flags plus
    /// `--help`.
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
        if !arguments.is_empty() {
            help.push_str("\nArguments:\n");
            help.push_str(&arguments);
        }
        help.push_str("\nOptions:\n");
        help.push_str(&options);
        help.push_str("  --help  Print help\n");
        help
    }
}

/// The session's defined functions (name → definition).
#[derive(Default)]
pub struct Funcs {
    map: HashMap<String, FuncDef>,
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
}
