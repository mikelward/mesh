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

## Installing

This repository is a Cargo workspace, so its root `Cargo.toml` is a *virtual*
manifest with no package of its own. A bare `cargo install` — which installs the
current directory, like `cargo install --path .` — therefore fails from the root
with:

```text
error: found a virtual manifest instead of a package manifest
```

Point `--path` at the `mesh` package instead of the workspace root. Installing
from git needs no such qualifier: `mesh` is the workspace's only installable
binary, so Cargo selects it automatically.

```sh
cargo install --locked --path crates/mesh                  # from a local checkout
cargo install --locked --git https://github.com/mikelward/mesh   # straight from git
```

`--locked` installs the exact dependency versions from the committed `Cargo.lock`
rather than re-resolving to newer ones. Both commands place a `mesh` binary in
`~/.cargo/bin`.

## Releases

Every push to `main` publishes a Linux x86-64 binary. The version is
`0.0.COMMITS`, where `COMMITS` is the number of commits reachable from that
revision, and the release is tagged `v0.0.COMMITS`. The workflow calculates the
version and updates the Cargo metadata used for the build; no manual version
edit or tag is needed.

Release assets contain the binary, README, and license files in
`mesh-VERSION-x86_64-unknown-linux-gnu.tar.gz`, together with a SHA-256
checksum. The `0.0.0` workspace version is a source-tree placeholder. Commit
counts are calculated from a full clone, and rewriting `main` history is avoided
so release versions remain unique and increasing.

The shell launches external commands and includes prompt configuration alongside
builtins for the shell's own state — `cd`, `pwd`, `puts`, `exit`, job control,
and the rest. The `help` builtin prints mesh in one screen: every builtin with
its usage, then every keyword and operator with the shape it is written in.
Interactive Tab completion covers builtins, defined functions, commands on
`PATH`, filesystem paths, variables, and map keys, ranked with fuzzy, smart-case
matching (all-lowercase ignores case; any uppercase makes the query
case-sensitive, and exact-case matches rank first). After a command, mesh works
out its subcommands and flags for itself — there are no completion scripts to
install. It takes the first of four sources that answers: a curated file under
`~/.local/share/mesh/completions/`, else the command's manual page, else a
bounded `--help` probe of the command, else files and directories. Builtins and
defined functions use their generated help the same way. File, directory, and
enumerated option values narrow argument completion to the expected type. A growing slice of the language
is in place: quoting and escapes, `~` and filename globs, captures and heredocs,
typed scalar/list/map values, arithmetic and comparisons, regex and glob matching
with `~`, collection iteration and destructuring, `while`/`loop`, `if`/`match`
expressions, postfix `if`/`unless` guards, postfix value modifiers, functions —
value calls, lambdas, and the higher-order `:map`/`:filter`/`:each` — `fork`
subshells, environment writes, scripts and `source`, and styled, clickable
output. For a hands-on walk through the main features, see
[`docs/TOUR.md`](docs/TOUR.md); for a terse lookup of the whole surface,
[`docs/REFERENCE.md`](docs/REFERENCE.md). That covers the completed M3 language
surface and the work since; later design work remains tracked in
[`ROADMAP.md`](ROADMAP.md) and [`TODO.md`](TODO.md).
[`docs/INTEGRATION.md`](docs/INTEGRATION.md) works through the external tools a
bash or zsh user arrives with — starship, atuin, fzf, carapace, zoxide, direnv —
saying which already work, which are blocked, and on what.

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

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
