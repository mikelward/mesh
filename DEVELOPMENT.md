# Development

How to build, test, and lay out the mesh implementation. For *what* mesh is and
the language design, see [`DESIGN.md`](DESIGN.md); for the milestone plan, see
[`ROADMAP.md`](ROADMAP.md).

The clean-break M3 parser target and precedence rules live in
[`PARSER.md`](PARSER.md). [`GRAMMAR.md`](GRAMMAR.md) continues to describe only
the subset accepted by the current implementation.

> **Status:** the implementation is a read/tokenize/exec loop that launches
> external commands plus the `cd`, `pwd`, `puts`, `exit`, `jobs`, `fg`, `bg`,
> `prompt`, and `prompt-hook` builtins. Interactive
> input uses `reedline` line editing (history, Ctrl-C/Ctrl-D, and session-aware
> Tab completion) behind a two-glyph
> prompt that can be customized with lifecycle hooks; piped input uses a std-only reader. None of the mesh *language* is
> implemented yet. Treat the current code as a seed, not a foundation to build
> features on before the real lexer/parser land.

Interactive startup follows Unix job-control rules: a mesh launched as a
background job stops before reading its terminal and can be resumed with `fg`.
Foreground commands receive the terminal, and mesh restores both terminal
ownership and its saved terminal modes when they finish or stop.
Bare `&` launches an external command or pipeline in its own background process
group and registers it in the job table; background stdin defaults to
`/dev/null`, so a job cannot consume later shell input.

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
- **Three members today** — `crates/mesh`, the thin shell executable;
  `crates/mesh-core`, the reusable lexer, expansion, and runtime library; and
  `crates/mesh-platform`, a small crate holding the `libc` constants and types
  whose definitions differ across platforms. The workspace leaves room for more
  satellite crates without restructuring.
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
| `reedline` | interactive line editing, history, Ctrl-C/D, completion | MIT | **in use** |
| `glob` | filesystem glob expansion | MIT/Apache-2.0 | **in use** |
| `libc` | process groups and foreground-terminal handoff | MIT/Apache-2.0 | **in use** |
| `crossterm` | terminal control (pulled in by `reedline`) | MIT | transitive |
| `nucleo-matcher` | fuzzy completion | MPL-2.0 | **in use** |

Add a dependency only when a milestone calls for it; prefer a small, focused
crate over a framework. The repo is licensed `MIT OR Apache-2.0`; keep the
license column permissive-compatible. All planned deps are permissive except
`nucleo`, which is MPL-2.0 (weak, file-level copyleft — compatible with a
permissive project).

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
│   ├── mesh/               # thin shell executable
│   │   ├── Cargo.toml
│   │   ├── src/main.rs     # calls mesh_core::run
│   │   └── tests/cli.rs    # end-to-end tests driving the built binary
│   ├── mesh-core/          # reusable shell implementation
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs      # public run entry point and lexer module
│   │       ├── repl.rs     # read / tokenize / dispatch loop
│   │       ├── lexer.rs    # compatibility lexer used by the current evaluator
│   │       ├── parser.rs   # span-carrying M3 tokens and command/value AST
│   │       ├── expand.rs   # interpolation resolve + tilde/glob (respects quoting)
│   │       ├── vars.rs     # variable store: global scope + function-local scopes
│   │       ├── funcs.rs    # user-defined function store (name → params + body)
│   │       ├── builtins.rs # cd, pwd, puts, exit + job-builtin recognition
│   │       └── exec.rs     # launch external commands + pipelines/redirection
│   └── mesh-platform/      # libc constants/types that differ across platforms
│       ├── Cargo.toml
│       └── src/lib.rs      # e.g. TIOCSCTTY, typed for libc::ioctl per platform
├── DESIGN.md               # vision + language design (the "why/what")
├── DEVELOPMENT.md          # this file (the "how to build")
├── GRAMMAR.md              # the grammar actually implemented so far (grows per task)
├── ROADMAP.md              # milestones M0 → beyond
├── TODO.md                 # current-milestone checklist
└── docs/                   # TOUR.md, REFERENCE.md (implemented), INTRO/PROMPT (design)
```

### How the code fits together

`main` calls `mesh_core::run`, which enters the REPL and loops: read a line →
`lexer::split_line` into
command segments joined by `;` / `&&` / `||` / `&`, each a list of words of pieces →
run the segments left to right, each connector deciding from the previous status
whether its command runs → per command, classify as an assignment or a command →
for a command, `expand::expand` (resolve `$` interpolation against `vars`, then
tilde/globs) → job-table dispatch (`jobs`/`fg`/`bg`) or `builtins::dispatch`
(`cd`/`pwd`/`puts`/`exit`) → a user function from the `funcs` store → else
`exec::run` launches the external command. A `func name(params) { … }` definition
is recognized from raw text before `split_line` (its body spans lines the per-line
lexer would flatten); the reader buffers input until the body braces balance. A
function call runs the body in a fresh function-local `vars` scope. A session
`vars` store (global scope plus a stack of function-local scopes) and the `funcs`
store persist across lines; the loop tracks the last exit status and returns it as
the process exit code at EOF.

The shell internals live in the `crates/mesh-core` library; `crates/mesh` is a
thin executable that calls its public `run` entry point. This keeps lexer and
future parser logic directly testable and makes the runtime reusable by future
frontends or satellite binaries. See [`ROADMAP.md`](ROADMAP.md).
