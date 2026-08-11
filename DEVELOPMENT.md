# Development

How to build, test, and lay out the mesh implementation. For *what* mesh is and
the language design, see [`docs/DESIGN.md`](docs/DESIGN.md); for the milestone plan, see
[`ROADMAP.md`](ROADMAP.md).

[`GRAMMAR.md`](GRAMMAR.md) is the grammar the implementation accepts today —
tokens, statements, value expressions, precedence, and the rules that tell a
command from a value.

> **Status:** M0–M3 have landed, so this is a working shell with a working
> language — typed values, functions, `if` / `for` / `match`, pipelines,
> redirection, and POSIX job control. Run it, or read
> [`docs/TOUR.md`](docs/TOUR.md) for the guided version and
> [`docs/REFERENCE.md`](docs/REFERENCE.md) for what is actually implemented.
> Interactive input uses `reedline` line editing (history, Ctrl-C/Ctrl-D, and
> session-aware Tab completion) behind a prompt customizable with lifecycle
> hooks; piped input uses a std-only reader. Work now continues past M3 —
> [`ROADMAP.md`](ROADMAP.md) holds the arc and [`TODO.md`](TODO.md) the working
> front.

Interactive startup follows Unix job-control rules: a mesh launched as a
background job stops before reading its terminal and can be resumed with `fg`.
Foreground commands receive the terminal, and mesh restores both terminal
ownership and its saved terminal modes when they finish or stop.
Bare `&` launches an external command or pipeline in its own background process
group and registers it in the job table; background stdin defaults to
`/dev/null`, so a job cannot consume later shell input.

## Prerequisites

- `rustup`. [`rust-toolchain.toml`](rust-toolchain.toml) pins an exact Rust
  release with `rustfmt` and `clippy`, and rustup installs it automatically on
  the first `cargo` invocation inside the checkout — once per machine, into
  `~/.rustup`, not once per clone. Without rustup the file is ignored and a
  compiler below the MSRV floor fails with cargo's `rustc … is not supported`
  rather than installing anything.
- A Unix host (see [Supported systems](#supported-systems)).

Per [`AGENTS.md`](AGENTS.md), install tools via direct binary downloads or
`cargo install` — **not** `apt`/`apt-get`.

## Quick start

[`Makefile`](Makefile) holds the commands below as one-word targets, so the
everyday ones need no flag string:

```sh
make           # debug build (the default target)
make install   # install the mesh binary into ~/.cargo/bin
make run       # build and start the shell
make test      # every suite, cargo and shell alike
make check     # what CI checks: fmt, clippy, tests — run this before pushing
make help      # list every target
```

It is a table of entry points, not a second build system: each target is one
cargo command, cargo does the work, and `makefile_test.sh` fails if `make check`
drifts from what [`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs.
`CARGO=...` overrides the cargo binary; `CARGO_INSTALL_ROOT=...` changes where
`make install` puts the binary.

The rest of this section spells out the underlying commands, which are still the
thing to reach for when a target doesn't fit.

## Build system

Cargo, as a **workspace** rooted at [`Cargo.toml`](Cargo.toml).

- **Edition:** 2024. **MSRV:** 1.95 (recorded as `rust-version` and verified by
  the `msrv` CI job; bumps are deliberate, not incidental). The floor is set by
  `libsqlite3-sys` — reached through `reedline`'s `sqlite` history backend —
  whose build script uses the `cfg_select!` macro stabilized in Rust 1.95.
- **Three members today** — `crates/mesh`, the thin shell executable;
  `crates/mesh-core`, the reusable parser, expansion, and runtime library; and
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
cargo install --locked --path crates/mesh   # install the mesh binary into ~/.cargo/bin
```

The root `Cargo.toml` is a virtual manifest, so a bare `cargo install` (or
`--path .`) from the root fails with *"found a virtual manifest instead of a
package manifest."* Point `--path` at `crates/mesh` instead. A git install
(`--git <url>`) needs no package name — `mesh` is the workspace's only
installable binary, so Cargo picks it automatically. Pass `--locked` to install
the exact dependency versions from the committed `Cargo.lock`.

`Cargo.lock` **is committed** (mesh is a binary, so builds are reproducible).

### Dependencies

The dependency set is kept minimal. `reedline` powers interactive line editing;
it is used **only** for TTY input, so piped input stays std-only and the
integration tests need no terminal. The rest of the interactive stack named in
`DESIGN.md` arrives with the milestones that need it:

| Crate | Purpose | License | Status |
| --- | --- | --- | --- |
| `reedline` | interactive line editing, history, Ctrl-C/D, completion | MIT | **in use** |
| `rusqlite` | the persisted-history backend `reedline`'s `sqlite` feature uses | MIT | **in use** |
| `glob` | filesystem glob expansion | MIT/Apache-2.0 | **in use** |
| `regex` | the `~` tests and `match`'s regex arms | MIT/Apache-2.0 | **in use** |
| `libc` | process groups and foreground-terminal handoff | MIT/Apache-2.0 | **in use** |
| `chrono` | timestamps for history and the prompt | MIT/Apache-2.0 | **in use** |
| `nu-ansi-term` | color for the prompt and the completion menu | MIT | **in use** |
| `unicode-segmentation` | grapheme-aware string handling | MIT/Apache-2.0 | **in use** |
| `unicode-width` | display width, so a prompt lines up | MIT/Apache-2.0 | **in use** |
| `crossterm` | terminal control and key events | MIT | **in use** |
| `nucleo-matcher` | fuzzy completion | MPL-2.0 | **in use** |

`crossterm` was transitive until the line editor needed to name its key-event
types; it is now a direct dependency pinned to the version `reedline` itself
builds against, so the two see one set of types.

Add a dependency only when a milestone calls for it; prefer a small, focused
crate over a framework. The repo is licensed `MIT OR Apache-2.0`; keep the
license column permissive-compatible. Everything here is permissive except
`nucleo-matcher`, which is MPL-2.0 (weak, file-level copyleft — compatible with a
permissive project).

Note the MSRV floor is set from *inside* this table: `rusqlite` reaches
`libsqlite3-sys`, whose build script needs Rust 1.95. Dropping persisted history
would drop the floor with it.

## Testing

Two layers, both run by `cargo test --workspace`:

- **Unit tests** — inline `#[cfg(test)] mod tests` next to pure logic (e.g.
  `parser::tokenize`). Fast, no process spawning.
- **Integration tests** — `crates/mesh/tests/*.rs` drive the *built binary*
  end-to-end. Cargo exposes its path as `CARGO_BIN_EXE_mesh`, so these use only
  `std::process` — no test-harness crate needed. They pipe a script on stdin and
  assert on stdout, stderr, and the exit code.

```sh
cargo test --workspace                 # everything
cargo test -p mesh --test cli          # just the end-to-end (integration) tests
cargo test -p mesh --test docs         # just the documentation examples
cargo test -p mesh --test transcripts  # run the documentation transcripts
```

The last two are how the docs are kept honest, and they divide the work.
`docs` parses every example with `mesh -n`, so a construct that was renamed
fails here rather than in a reader's terminal. `transcripts` **runs** every
`<pre>` transcript — each block in the session its own file built ahead of it —
and compares the `mesh:` lines the shell produces against the ones the block
shows. Output itself is not compared: a transcript names the author's home
directory, the programs on their `PATH`, and a pid, none of which a test host
has. What is reproducible is whether mesh objects, and an example that quietly
stopped working objects where the doc promises a result.

A block that cannot run — one that waits on a background job, or asks about the
host — says so in the prose above it, as `<!-- no-run: reason -->`. A test
bounds how many may do that.

Three suites sit outside `cargo`, all run by CI alongside the Rust tests and all
by `make test`. `toolchain_test.sh` covers
[`rust-toolchain.toml`](rust-toolchain.toml): that it pins an exact release
rather than a floating channel, that the pin has not fallen below the MSRV
floor, and that the CI jobs which deliberately override the pin still do.
`session_start_hook_test.sh` covers
[`.claude/hooks/session-start.sh`](.claude/hooks/session-start.sh), the backstop
for a toolchain that cannot build the tree. `makefile_test.sh` covers the
[`Makefile`](Makefile): that `make install` names a package that exists, and
that `make check` is still the set of commands CI runs.

```sh
sh toolchain_test.sh            # reads the manifests; installs nothing
sh session_start_hook_test.sh   # stubs rustc/rustup; downloads nothing
sh makefile_test.sh             # `make -n` dry runs; builds nothing
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

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) has three jobs. **`check`**
is fmt, clippy, the test suite, shellcheck, and the three shell suites below; it
also cross-checks FreeBSD and macOS. **`msrv`** builds against the MSRV floor
read out of `Cargo.toml`. Both run for every push to `main` and every pull
request. **`stable`** is an early warning — the same clippy and tests on the
floating stable toolchain, so a lint or behavior change arriving in a later Rust
is seen before the pin moves to it — and it runs on **pushes to `main` only**:
on a pull request an upstream release would turn someone's unrelated PR red,
which teaches everyone to ignore it.

Every job runs on `ubuntu-latest`: the FreeBSD and macOS coverage is `cargo
check` against those targets — cross-compiled, Zig supplying the macOS C
toolchain — rather than a runner of that platform, so it catches type errors but
never executes a test there.

[`.github/workflows/release.yml`](.github/workflows/release.yml) is separate: it
publishes the Linux x86-64 binary for every push to `main`, versioned by commit
count (see the README's *Releases*).

The binary is not told that version — it derives its own, in
[`crates/mesh-core/build.rs`](crates/mesh-core/build.rs), from the checkout it is
built from, which is what lets your own clean build of a released commit report
the released number rather than the `0.0.0` placeholder in `Cargo.toml`. The
release job checks the two derivations against each other before publishing, so
a disagreement fails the release instead of shipping a binary that misreports
which release it is. Setting `MESH_BUILD_VERSION` overrides the derivation
outright, for building from a source archive that knows its version but has no
history to read; it has to be a semver version, and a value that is not one is
reported as a build warning and the derivation runs instead.

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
├── Makefile                # one-line entry points over cargo (make install, make check)
├── makefile_test.sh        # asserts those entry points match the docs and CI
├── rust-toolchain.toml     # pins an exact release + rustfmt + clippy
├── .github/workflows/ci.yml       # fmt, clippy, tests, cross-checks, MSRV, stable
├── .github/workflows/release.yml  # the per-push Linux x86-64 binary
├── crates/
│   ├── mesh/               # thin shell executable
│   │   ├── Cargo.toml
│   │   ├── src/main.rs     # calls mesh_core::run
│   │   ├── tests/cli.rs    # end-to-end tests driving the built binary
│   │   ├── tests/docs.rs   # parses every mesh example in the documentation
│   │   └── tests/transcripts.rs # runs every documented <pre> transcript
│   ├── mesh-core/          # reusable shell implementation
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs      # public run entry point and parser module
│   │   │   ├── repl.rs     # read / tokenize / dispatch loop
│   │   │   ├── parser.rs   # span-carrying M3 tokens and command/value AST
│   │   │   ├── expand.rs   # interpolation resolve + tilde/glob (respects quoting)
│   │   │   ├── vars.rs     # variable store: global scope + function-local scopes
│   │   │   ├── environ.rs  # $env.KEY, and the path-type entries that split on `:`
│   │   │   ├── options.rs  # $sh.options — the settings, shared with the line editor
│   │   │   ├── funcs.rs    # user-defined function store (name → params + body)
│   │   │   ├── hooks.rs    # the lifecycle event registry behind `on` and $sh.<event>
│   │   │   ├── whence.rs   # what a name is — the report `type` prints
│   │   │   ├── completion.rs # the layered spec resolver behind Tab
│   │   │   ├── reaper.rs   # the one place that calls waitpid; SIGCHLD handling
│   │   │   ├── stack.rs    # makes a stack overflow report rather than abort
│   │   │   ├── url.rs      # the file: URL encoder shared by :url and OSC 7
│   │   │   ├── builtins.rs # cd, pwd, puts, type, … + job-builtin recognition
│   │   │   ├── exec.rs     # launch external commands + pipelines/redirection
│   │   │   └── prelude.mesh # mesh's shipped defaults, written in mesh
│   │   └── tests/
│   │       ├── help/       # captured `--help` output, verbatim (see its README)
│   │       └── man/        # captured `man` output, verbatim (see its README)
│   └── mesh-platform/      # libc constants/types that differ across platforms
│       ├── Cargo.toml
│       └── src/lib.rs      # e.g. TIOCSCTTY, typed for libc::ioctl per platform
├── DEVELOPMENT.md          # this file (the "how to build")
├── GRAMMAR.md              # the grammar the parser accepts today (EBNF + precedence)
├── ROADMAP.md              # milestones M0 → beyond
├── TODO.md                 # current-milestone checklist
└── docs/                   # DESIGN.md (vision + language design, the "why/what"),
                            # TOUR.md, REFERENCE.md (implemented), HOOKS.md (the
                            # lifecycle events), INTRO/PROMPT (design),
                            # INTEGRATION.md (external tools), COMPARISON.md and
                            # UPSTREAM.md (mesh against the other shells)
```

### How the code fits together

`main` calls `mesh_core::run`, which enters the REPL and loops: read a line →
`parser::parse` into a syntax tree of statements joined by `;` / `&&` / `||` / `&` →
run the statements left to right, each connector deciding from the previous status
whether its command runs → per statement, an assignment, a value expression, or a
pipeline → for a command, `expand::expand` (resolve `$` interpolation against
`vars`, then tilde/globs) → job-table dispatch (`jobs`/`fg`/`bg`) or
`builtins::dispatch` (`cd`/`pwd`/`puts`/`exit`) → a user function from the `funcs`
store → else `exec::run` launches the external command. Input that does not yet form
a complete unit is buffered rather than dispatched: `parse` reports it as
incomplete, and a `func` definition whose header is malformed is judged separately
(`repl::func_definition_is_open`) so its error is reported instead of swallowing the
lines after it. A function call runs the body in a fresh function-local `vars`
scope. A session `vars` store (global scope plus a stack of function-local scopes)
and the `funcs` store persist across lines; the loop tracks the last exit status and
returns it as the process exit code at EOF.

Two things run around that loop, for a session that reaches it at all. The first
is `prelude.mesh` — mesh's own defaults, written in mesh and embedded in the
binary — evaluated ahead of the startup files, so a default like the window title
is a hook a user can inspect and replace rather than Rust they cannot reach.
`run_startup_files` takes the session's **resolved** interactivity as a parameter
and evaluates the prelude only when that is true — the interactive path passes a
literal `true`, the batch paths pass the `-i` flag through — so **interactivity
is the condition, not the input shape**: an ordinary terminal session gets the
prelude, a plain script, `-c` string, or piped run does not, and each of those
under `-i` does. The guard is there because everything the prelude registers
fires from the interactive loops, so a batch run would pay to parse it and never
use it. (`-n` is upstream of all of this — it checks syntax and runs nothing,
prelude and startup files included.) The `hooks` registry then fires the lifecycle
events at their points in the loop: `preprompt` before a line is read, `preexec`
and `postexec` around running it, `precd` / `postcd` around an actual `cd`,
`jobdone` when a background job finishes, and `exit` on the way out. `run_logout`
is the one function every *exit* path arrives at, which is why the drain, the
title clear, and the `exit` hook all live there — and it is equally why a
successful `exec` and a fatal signal run none of them: neither is an exit path,
because neither leaves a shell to return to it.

The shell internals live in the `crates/mesh-core` library; `crates/mesh` is a
thin executable that calls its public `run` entry point. This keeps parser and
runtime logic directly testable and makes the runtime reusable by future
frontends or satellite binaries. See [`ROADMAP.md`](ROADMAP.md).
