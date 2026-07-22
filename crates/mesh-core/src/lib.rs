//! Core lexer, expansion, and runtime for the mesh shell.
//!
//! The [`run`] entry point owns the read/tokenize/dispatch loop. The binary crate
//! deliberately contains only process startup so other frontends and tests can
//! use the shell implementation without depending on an executable crate.

mod builtins;
mod exec;
mod expand;
mod funcs;
pub mod lexer;
mod repl;
mod vars;

pub use exec::run_background_redirect;
pub use repl::run;
