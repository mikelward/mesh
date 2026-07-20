# Development

How to build, test, and lay out the mesh implementation. For *what* mesh is and
the language design, see [`DESIGN.md`](DESIGN.md); for the milestone plan, see
[`ROADMAP.md`](ROADMAP.md).

> **Status:** the implementation is a read/tokenize/exec loop that launches
> external commands plus the `cd`, `pwd`, `puts`, and `exit` builtins. Interactive
> input uses `reedline` line editing (history, Ctrl-C/Ctrl-D) behind a two-glyph
> prompt; piped input uses a std-only reader. None of the mesh *language* is
> implemented yet. Treat the current code as a seed, not a foundation to build
> features on before the real lexer/parser land.

## Prerequisites

- A stable Rust toolchain. [`rust-toolchain.toml`](rust-toolchain.toml) pins
  `stable` with `rustfmt` and `clippy`, so `rustup` installs the right thing
  automatically on first `cargo` invocation.
- A Unix host (see [Supported systems](#supported-systems)).

Per [`AGENTS.md`](AGENTS.md), install tools via direct binary downloads or
`cargo install` — **not** `apt`/`apt-get`.

## Build system

Cargo, as a **workspace** rooted at [`Cargo.toml`](Cargo.toml).

- **Edition:** 2024. **MSRV:** 1.85 (recorded as `rust-version`; bumps are
  deliberate, not incidental).
- **One member today** — `crates/mesh`, the shell binary. The workspace exists
  so satellite crates (a VCS/prompt helper, a future `mesh-core` library) drop in
  as new `members` without restructuring.
- **Lints are centralized** in `[workspace.lints]` and inherited by each crate
  (`[lints] workspace = true`). CI denies warnings, so keep the tree clean rather
  than scattering `#[allow]`.

```sh
cargo build            # debug build → target/debug/mesh
cargo run -p mesh      # build and start the shell
cargo build --release  # optimized build
```

`Cargo.lock` **is committed** (mesh is a binary, so builds are reproducible).

### Dependencies

The dependency set is kept minimal. `reedline` powers interactive line editing;
it is used **only** for TTY input, so piped input stays std-only and the
integration tests need no terminal. The rest of the interactive stack named in
`DESIGN.md` arrives with the milestones that need it:

| Crate | Purpose | License | Status |
| --- | --- | --- | --- |
| `reedline` | interactive line editing, history, Ctrl-C/D | MIT | **in use** |
| `glob` | filesystem glob expansion | MIT/Apache-2.0 | **in use** |
| `nix` | `fork`/`exec`, `setpgid`, `tcsetpgrp`, signals | MIT | planned (job control) |
| `crossterm` | terminal control (pulled in by `reedline`) | MIT | transitive |
| `nucleo` | fuzzy completion | MPL-2.0 | planned |

Add a dependency only when a milestone calls for it; prefer a small, focused
crate over a framework. Note the license column when the repo license is chosen
(see `TODO.md`): all planned deps are permissive except `nucleo`, which is
MPL-2.0 (weak, file-level copyleft — compatible with a permissive project).

## Testing

Two layers, both run by `cargo test --workspace`:

- **Unit tests** — inline `#[cfg(test)] mod tests` next to pure logic (e.g.
  `lexer::split`). Fast, no process spawning.
- **Integration tests** — `crates/mesh/tests/*.rs` drive the *built binary*
  end-to-end. Cargo exposes its path as `CARGO_BIN_EXE_mesh`, so these use only
  `std::process` — no test-harness crate needed. They pipe a script on stdin and
  assert on stdout, stderr, and the exit code.

```sh
cargo test --workspace          # everything
cargo test -p mesh --test cli   # just the end-to-end (integration) tests
```

Convention (from `AGENTS.md`): **a change isn't done until it's covered.** When
fixing a bug, add a test that fails before the fix and passes after. Richer
harnesses (`assert_cmd`, snapshot testing via `insta`) are fine to adopt when the
end-to-end surface grows past what plain `std` expresses comfortably.

## Formatting and linting

Default `rustfmt`, and `clippy` with warnings denied — the same checks CI runs:

```sh
cargo fmt --all
cargo fmt --all -- --check                       # CI gate
cargo clippy --all-targets -- -D warnings        # CI gate
```

## Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs fmt, clippy, and the
test suite on `ubuntu-latest` and `macos-latest` for every push to `main` and
every pull request.

## Supported systems

mesh is **Unix-only**. Real POSIX job control (`Ctrl-Z`/`fg`/`bg`, handing the
terminal to a full-screen program) is the headline feature, and it drives the
platform matrix.

| Platform | Support |
| --- | --- |
| Linux (x86_64, aarch64) | **Primary** — develop and test here first. |
| macOS (Apple Silicon, Intel) | **Secondary** — kept green in CI. |
| Windows | **Not supported.** The POSIX process/terminal model is assumed throughout. |

The floor is any modern Unix with POSIX job control and a stable Rust toolchain.

## Directory layout

```
mesh/
├── Cargo.toml              # workspace root (members, shared edition/MSRV, lints)
├── Cargo.lock              # committed — mesh is a binary
├── rust-toolchain.toml     # pins stable + rustfmt + clippy
├── .github/workflows/ci.yml
├── crates/
│   └── mesh/               # the shell binary
│       ├── Cargo.toml
│       ├── src/
│       │   ├── main.rs     # entry point
│       │   ├── repl.rs     # read / tokenize / dispatch loop
│       │   ├── lexer.rs    # quotes + escapes + $interpolation → words of pieces
│       │   ├── expand.rs   # interpolation resolve + tilde/glob (respects quoting)
│       │   ├── vars.rs     # session-global variable store
│       │   ├── builtins.rs # cd, pwd, puts, exit
│       │   └── exec.rs     # launch external commands, map exit status
│       └── tests/
│           └── cli.rs      # end-to-end tests driving the built binary
├── DESIGN.md               # vision + language design (the "why/what")
├── DEVELOPMENT.md          # this file (the "how to build")
├── GRAMMAR.md              # the grammar actually implemented so far (grows per task)
├── ROADMAP.md              # milestones M0 → beyond
├── TODO.md                 # current-milestone checklist
└── docs/                   # TOUR.md, REFERENCE.md (implemented), INTRO/PROMPT (design)
```

### How the code fits together

`main` calls `repl::run`, which loops: read a line → `lexer::split` into words of
pieces → classify as an assignment or a command → for a command,
`expand::expand` (resolve `$` interpolation against `vars`, then tilde/globs) →
`builtins::dispatch` (handles `cd`/`pwd`/`puts`/`exit`, returns `None` otherwise)
→ else `exec::run` launches the external command. A session-global `vars` store
persists across lines; the loop tracks the last exit status and returns it as the
process exit code at EOF.

**Planned evolution.** When the real lexer/parser replace the M0 placeholder,
the shell internals graduate into a `crates/mesh-core` **library** and the binary
becomes a thin `main` over it — that is the natural moment to make the split
(direct unit-testing of the parser, shared code for satellite binaries), not
before. See [`ROADMAP.md`](ROADMAP.md).
