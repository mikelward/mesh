//! The function store.
//!
//! A `func name(params) { body }` definition binds a named callable. v1 supports
//! **required named positionals** only; optional defaults, `--flags`, and
//! `...rest` are deferred (see `DESIGN.md` §"Functions"). Function bodies are
//! stored as parsed sources and executed recursively on each call.

use std::collections::HashMap;

use crate::parser::Source;

/// A defined function: its positional parameter names and parsed body.
pub struct FuncDef {
    pub params: Vec<String>,
    pub body: Source,
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
