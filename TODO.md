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

- [ ] Add `reedline`; replace the stdin reader with a real line editor
- [ ] History (in-memory first, then persisted)
- [ ] Lexer v1: single/double quotes and escapes
- [ ] Promote internals into `crates/mesh-core` (lib); binary becomes thin `main`
- [ ] `;`, `&&`, `||` sequencing
- [ ] `cd` builtin (deferred from M0): `$env.PWD`/`OLDPWD`, `cd -`, `CDPATH`
- [ ] `pwd` and `puts` builtins
- [ ] Globs + `~` expansion (glob no-match → **empty**, see Decisions made)
- [ ] A simple prompt (host/dir), stderr-rendered as today

## Known limitations

- Ctrl-C during a foreground command kills the shell instead of returning to the
  prompt with status `130`. Deferred to the job-control task (M2); see
  `ROADMAP.md`.

## Decisions made

- **Merge method:** rebase. **Toolchain:** floating `stable`. **Loop autonomy:**
  proceed with best call, documented + overridable; pause only for grammar-level
  design decisions.
- **Glob no-match → empty** (nullglob-style: the pattern expands to zero words,
  "as if it weren't there"). Rejects bash's literal pass-through as a footgun.
  Caveat to revisit: a silently-vanishing no-match is a mild version of the
  "absence is loud" concern elsewhere in `DESIGN.md` (e.g. `rm *.bak` with no
  matches becomes a bare `rm`); the alternative was erroring like zsh `nomatch`.

## Decisions needed

- [ ] **Revisit punting `cd`.** Deferred from M0 to M1 to keep M0 minimal, but
      this isn't settled — an interactive shell arguably needs `cd` from day one.
      Decide whether to pull a minimal `cd` back into M0 or keep it in M1 with
      the full logical-cwd/`CDPATH`/`cd -` treatment.
- [ ] **Namespace for the working-directory vars in the mesh language:**
      `$env.PWD` / `$env.OLDPWD` vs `$sh.PWD` / `$sh.OLDPWD`. `DESIGN.md` (~line
      2027) currently writes `$env.PWD` / `$env.OLDPWD` — reconcile with the
      intended `$sh.*` choice and make the design consistent (`$env.PATH` etc.
      use `$env.`). Language-surface only: M0 sets the real OS `PWD`/`OLDPWD`
      environment variables (that's what child processes read), which is
      unaffected by how the shell language exposes them.
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
