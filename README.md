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

The shell launches external commands and includes prompt configuration alongside
the `cd`, `pwd`, `puts`, and `exit` builtins. Interactive Tab completion covers
builtins, defined functions, commands on `PATH`, filesystem paths, variables,
and map keys. After a command, completion passes the words already entered to
`COMMAND --help` and extracts options and subcommands from both output streams;
builtins and defined functions use their generated help in the same way. A
growing slice of the language is in place: quoting and escapes, `~`
and filename globs, typed scalar/list/map values, arithmetic and comparisons,
regex and glob matching with `~`, collection iteration and destructuring,
functions, `if`/`match` expressions, and postfix value modifiers.
For a hands-on walk through what runs today, see
[`docs/TOUR.md`](docs/TOUR.md); for a terse lookup,
[`docs/REFERENCE.md`](docs/REFERENCE.md). This is the completed M3 language
surface; later design work remains tracked in
[`ROADMAP.md`](ROADMAP.md) and [`TODO.md`](TODO.md).

## Name

**mesh.** No other shell claims the name. Two tradeoffs accepted: the word is
overloaded in infra (service mesh, mesh networking) and sits one letter from
`mosh` (mobile shell). The runner-up was **smash**.

## Status

Language design remains in draft. Implementation has completed the M2 shell
runtime (pipelines, redirection, and job control) and completed **M3** with typed
values, the clean-break parser, explicit `...$list` argument spread, functions,
conditionals, collection loops, destructuring, and matching. See
[`ROADMAP.md`](ROADMAP.md).
