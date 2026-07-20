# mesh

A personal, **interactive-first** Unix shell: byte-stream pipes with real
arrays, a clean-break syntax (no POSIX-script baggage), and a prompt/session/
completion setup built around how one person actually works at a terminal.

The **language design** is still in progress — see [`DESIGN.md`](DESIGN.md) for
the rationale and the language sketch so far. In parallel, a **build track** has
started: [`ROADMAP.md`](ROADMAP.md) lays out the milestones and
[`DEVELOPMENT.md`](DEVELOPMENT.md) covers how to build, test, and lay out the
code.

## Building

Unix only, stable Rust (pinned via `rust-toolchain.toml`):

```sh
cargo run -p mesh      # start the shell
cargo test --workspace # run the tests
```

The shell is at milestone **M0**: it reads a line and launches the external
command it names (plus an `exit` builtin) — `echo 'ls' | mesh` works. None of the
mesh *language* is implemented yet; see [`ROADMAP.md`](ROADMAP.md).

## Name

**mesh.** No other shell claims the name. Two tradeoffs accepted: the word is
overloaded in infra (service mesh, mesh networking) and sits one letter from
`mosh` (mobile shell). The runner-up was **smash**.

## Status

Language design in draft. Implementation at milestone **M0** (runs external
commands); see [`ROADMAP.md`](ROADMAP.md).
