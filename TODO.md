# TODO

The working front — concrete, checkable tasks for the current and next
milestone. The stable milestone arc is in [`ROADMAP.md`](ROADMAP.md); update this
file as tasks land.

## M0 — It runs `ls` ✅ (done)

- [x] Cargo workspace, edition 2024, MSRV 1.85, `rust-toolchain.toml`
- [x] `crates/mesh` binary: `main` / `repl` / `lexer` / `builtins` / `exec`
- [x] Read/tokenize/dispatch loop over stdin (TTY + piped)
- [x] Launch external commands; exit-status conventions (127 / 126 / 128+sig)
- [x] Builtin: `exit` (8-bit masking); `cd` punted to M1 (tentative)
- [x] Unit tests (lexer) + end-to-end tests (built binary, std-only)
- [x] CI: fmt + clippy (`-D warnings`) + test on Linux and macOS

## M1 — Next up

- [x] `reedline` line editor for interactive (TTY) input; std byte reader kept
      for piped input. Ctrl-D exits on an empty line; Ctrl-C cancels the line.
- [x] History (in-memory, reedline default). Persisted history: later.
- [x] Two-glyph prompt (`mesh$` / `mesh!`) via reedline.
- [ ] Lexer v1: single/double quotes and escapes (raw form = heredoc, see below)
- [ ] Promote internals into `crates/mesh-core` (lib); binary becomes thin `main`
- [ ] `;`, `&&`, `||` sequencing
- [x] `cd` builtin (basic): `$HOME` default, `cd -`, updates `$PWD`/`$OLDPWD`,
      rejects surplus operands. Still deferred: `CDPATH`, `--physical`, autocd,
      logical cwd.
- [x] `pwd` and `puts` builtins
- [ ] Globs + `~` expansion (glob no-match → **empty**, see Decisions made)

## Known limitations

- Ctrl-C during a foreground command kills the shell instead of returning to the
  prompt with status `130`. Deferred to the job-control task (M2); see
  `ROADMAP.md`.

## Decisions made

- **Merge method:** rebase. **Toolchain:** floating `stable`. **Loop autonomy:**
  proceed with best call, documented + overridable; pause only for grammar-level
  design decisions.
- **Working-directory var namespace = `$env.PWD` / `$env.OLDPWD`** (confirms
  `DESIGN.md`; the `$sh.*` alternative was considered and dropped — if a value is
  exported to and inherited by children, it lives under `$env.`).
- **Raw mixed-quote string form = heredoc** (`<< END … END`) — for a literal
  that embeds both `'` and `"` with no escaping. Chosen over a Rust-style
  `r#"…"#` delimiter. Implementation lands with the quoting task (task 5).
- **Repo license = decide later** (leave unlicensed for now; revisit before any
  real release). Recorded in "Decisions needed" below for visibility.
- **Glob no-match → empty** (nullglob-style: the pattern expands to zero words).
  This is *principled*, not a compromise, and fully consistent with "absence is
  loud": specific-element access (`xs[99]`, `$map.key`) errors because you asked
  for one thing that isn't there and there is no null; a glob (`*.txt`) is a
  **collection query** whose result type is a *list*, so zero matches = the empty
  list = a complete, honest answer, not an absence. Rejects bash's literal
  pass-through as a footgun.

## Decisions needed

- [ ] **Choose a repo license** — *decided: later* (revisit before any real
      release). Nothing constrains the choice today: all current/planned deps are
      permissive (`reedline`/`nix`/`crossterm` MIT) except `nucleo` **MPL-2.0**
      (weak, file-level copyleft — compatible with a permissive project). Likely
      landing spot: `MIT OR Apache-2.0` (the Rust-ecosystem norm).

## Icebox / decide later

- [ ] **Empty-glob warning (optional).** Keep behavior "empty always", but
      consider *warning* on an empty glob expansion while still proceeding — mesh
      is the only party that can detect it (the argv boundary carries bytes, not
      lists, so the emptiness is erased at `execve`; a downstream `grep` can't
      tell `grep foo *.log`-matched-nothing from `grep foo`). Interactively it
      could even prompt what to do. Warn on an empty *glob*, not on a genuinely
      empty `$list`.
- [ ] Reading a script file as an argument (`mesh script.msh`) vs. stdin only
- [ ] `-c "…"` one-shot command flag
- [ ] Whether satellite helpers (`vcs`, prompt) are Rust workspace members or
      standalone (per-helper call; see `DEVELOPMENT.md`)
