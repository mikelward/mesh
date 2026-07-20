# Development

How to build, test, and lay out the mesh implementation. For *what* mesh is and
the language design, see [`DESIGN.md`](DESIGN.md); for the milestone plan, see
[`ROADMAP.md`](ROADMAP.md).

> **Status:** the implementation is at **M0** — a read/tokenize/exec loop that
> launches external commands plus an `exit` builtin. None of the mesh *language* is
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

M0 is intentionally **dependency-free** — the standard library covers a
line/tokenize/exec loop, and zero deps keep the build offline and fast. The
interactive stack named in `DESIGN.md` arrives with the milestones that need it:

| Crate | Purpose | License | Milestone |
| --- | --- | --- | --- |
| `reedline` | line editing, history, hinting | MIT | M1 |
| `nix` | `fork`/`exec`, `setpgid`, `tcsetpgrp`, signals | MIT | M2 (job control) |
| `crossterm` | terminal control | MIT | later |
| `nucleo` | fuzzy completion | MPL-2.0 | later |

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
│       │   ├── lexer.rs    # M0 whitespace tokenizer (PLACEHOLDER)
│       │   ├── builtins.rs # exit (cd deferred to M1)
│       │   └── exec.rs     # launch external commands, map exit status
│       └── tests/
│           └── cli.rs      # end-to-end tests driving the built binary
├── DESIGN.md               # vision + language design (the "why/what")
├── DEVELOPMENT.md          # this file (the "how to build")
├── GRAMMAR.md              # the grammar actually implemented so far (grows per task)
├── ROADMAP.md              # milestones M0 → beyond
├── TODO.md                 # current-milestone checklist
└── docs/                   # INTRO.md, PROMPT.md — design narratives
```

### How the M0 code fits together

`main` calls `repl::run`, which loops: read a line → `lexer::split` into words →
`builtins::dispatch` (handles `exit`, returns `None` otherwise) → else
`exec::run` launches the external command. The loop tracks the last exit status
and returns it as the process exit code at EOF.

**Planned evolution.** When the real lexer/parser replace the M0 placeholder,
the shell internals graduate into a `crates/mesh-core` **library** and the binary
becomes a thin `main` over it — that is the natural moment to make the split
(direct unit-testing of the parser, shared code for satellite binaries), not
before. See [`ROADMAP.md`](ROADMAP.md).
