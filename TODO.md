# TODO

The working front — concrete, checkable tasks for the current and next
milestone. The stable milestone arc is in [`ROADMAP.md`](ROADMAP.md); update this
file as tasks land.

## M0 — It runs `ls` ✅ (done)

- [x] Cargo workspace, edition 2024, MSRV 1.85, `rust-toolchain.toml`
- [x] `crates/mesh` binary: `main` / `repl` / `lexer` / `builtins` / `exec`
- [x] Read/tokenize/dispatch loop over stdin (TTY + piped)
- [x] Launch external commands; exit-status conventions (127 / 126 / 128+sig)
- [x] Builtins: `cd`, `exit`
- [x] Unit tests (lexer) + end-to-end tests (built binary, std-only)
- [x] CI: fmt + clippy (`-D warnings`) + test on Linux and macOS

## M1 — Next up

- [ ] Add `reedline`; replace the stdin reader with a real line editor
- [ ] History (in-memory first, then persisted)
- [ ] Lexer v1: single/double quotes and escapes
- [ ] Promote internals into `crates/mesh-core` (lib); binary becomes thin `main`
- [ ] `;`, `&&`, `||` sequencing
- [ ] A simple prompt (host/dir), stderr-rendered as today

## Decisions needed

- [ ] **Choose a repo license** (none declared yet). M0 has no dependencies, so
      nothing constrains the choice today. Planned deps and their licenses:
      `reedline` MIT, `nix` MIT, `crossterm` MIT — all permissive; `nucleo`
      **MPL-2.0** (weak, file-level copyleft). MPL-2.0 is compatible with a
      permissive project license (it only obliges sharing changes to nucleo's
      *own* files), but confirm the intended repo license (e.g. MIT, or
      MIT OR Apache-2.0 — the Rust-ecosystem norm) is acceptable alongside it.

## Icebox / decide later

- [ ] Reading a script file as an argument (`mesh script.msh`) vs. stdin only
- [ ] `-c "…"` one-shot command flag
- [ ] Whether satellite helpers (`vcs`, prompt) are Rust workspace members or
      standalone (per-helper call; see `DEVELOPMENT.md`)
