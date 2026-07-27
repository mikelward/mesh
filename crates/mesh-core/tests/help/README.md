# Captured `--help` output

Verbatim output of real commands, used as fixtures by the completion-spec tests
in `src/completion.rs`. Hand-written help text is fine for a single parsing
rule, but only real output shows what these commands actually print — the
sentence git captions its command table with, the description cargo puts on the
line *below* its option, the star docker hangs off a plugin command.

Each file is the unedited stdout+stderr of the command below, captured with
`COLUMNS=80` and no terminal attached, since some of these wrap to the terminal
width:

| File | Command | Version |
| --- | --- | --- |
| `git.txt` | `git --help` | git 2.43.0 |
| `cargo.txt` | `cargo --help` | cargo 1.97.1 |
| `docker.txt` | `docker --help` | Docker 29.3.1 |
| `ls.txt` | `ls --help` | GNU coreutils 9.4 |
| `rustup.txt` | `COLUMNS=50 rustup --help` | rustup 1.29.0 |

`rustup.txt` is captured at **50** columns on purpose: the narrow width wraps
its command descriptions onto continuation lines, and its worked examples sit
under a "Common commands:" caption — the two shapes a table entry has to be
told apart from.

Re-capture with, for example:

```sh
COLUMNS=80 git --help > crates/mesh-core/tests/help/git.txt 2>&1
```

A re-capture from a different version is expected to move some of the details
the tests assert on (a subcommand added, a flag renamed). Update the assertions
to what the new output says rather than editing a fixture to keep a test green —
the point of these files is that nobody wrote them.

There is deliberately no `man.txt`: `man --help` is the case that motivated the
`PAGE` operand rule, but man-db is not installed in the container these were
captured in, so the man usage line in the tests is transcribed rather than
captured. Replace it with a real capture when one is available.
