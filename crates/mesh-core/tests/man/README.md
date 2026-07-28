# Captured manual pages

Verbatim `man` output for real pages, used as fixtures by the man-page-spec
tests in `src/completion.rs`. Same reasoning as
[`../help/README.md`](../help/README.md): hand-written text covers one rule at a
time, but only a real page shows the layout the parser actually meets.

These are the *rendered* pages, not their roff source, because rendering is what
mesh does — it runs `man -l <path>` and parses the text that comes back. What
gets asserted on is therefore what the shell itself will see.

| File | Page | Source | Rendered by |
| --- | --- | --- | --- |
| `createdb.1.txt` | `createdb(1)` | PostgreSQL 16.13, DocBook XSL | man-db 2.12.0 + groff 1.23.0 |
| `jar.1.txt` | `jar(1)` | OpenJDK, pandoc | man-db 2.12.0 + groff 1.23.0 |

`createdb.1.txt` is the one that motivates the declaration-column rule: its
descriptions cite other options constantly — `--locale`'s reads "equivalent to
specifying `--lc-collate`, `--lc-ctype`, and `--icu-locale`" — and at some widths
a citation wraps to the start of its line, where it reads exactly like a
declaration. Only the column tells them apart.

Both are captured at `MANWIDTH=80` and through a **pipe**. The pipe matters: when
`man` is not writing to a terminal it renders plain text, with no SGR escapes and
no backspace overstrike, so the parser needs to strip nothing. Re-capture with:

```sh
MANWIDTH=80 man -l /usr/share/man/man1/createdb.1.gz \
    > crates/mesh-core/tests/man/createdb.1.txt
```

A re-capture from a different page version, `man`, or groff is expected to move
some of the details the tests assert on — `createdb` asserts an exact option
count, which a page that gained a flag would change. Update the assertions to
what the new output says rather than editing a fixture to keep a test green; the
point of these files is that nobody wrote them.

Note the container these were captured in ships the *minimized* `man`, which
prints an advisory and exits 0 without rendering anything. man-db, groff, and
`col` were unpacked from their `.deb`s into a scratch prefix to capture these.
That stub is also why `a_man_that_renders_nothing_useful_yields_no_spec` exists:
a zero exit status from `man` does not mean a page was rendered.
