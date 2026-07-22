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

The shell launches external commands and the builtins `cd`, `pwd`, `puts`, and
`exit`, with a growing slice of the language in place: quoting and escapes, `~`
and filename globs, typed scalar/list/map values, arithmetic and comparisons,
regex and glob matching with `~`, collection iteration and destructuring,
functions, `if`/`match` expressions, and postfix value modifiers.
For a hands-on walk through what runs today, see
[`docs/TOUR.md`](docs/TOUR.md); for a terse lookup,
[`docs/REFERENCE.md`](docs/REFERENCE.md). The full language is still landing task
by task — see [`ROADMAP.md`](ROADMAP.md).

## Name

**mesh.** No other shell claims the name. Two tradeoffs accepted: the word is
overloaded in infra (service mesh, mesh networking) and sits one letter from
`mosh` (mobile shell). The runner-up was **smash**.

## Status

Language design remains in draft. Implementation has completed the M2 shell
runtime (pipelines, redirection, and job control) and begun **M3** with typed
values, explicit `...$list` argument spread, functions, conditionals, collection
loops and matching. See
[`ROADMAP.md`](ROADMAP.md).
